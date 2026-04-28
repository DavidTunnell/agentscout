//! Storage CRUD integration tests — exercise the SQLite path directly
//! against a temp directory to verify schema, indices, and update paths.

use agentscout::storage::{CaptureRecord, Storage};

fn temp_storage() -> (Storage, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("as-storage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = Storage::open_at(dir.clone()).unwrap();
    (storage, dir)
}

#[test]
fn record_and_list_recent_captures() {
    let (storage, _) = temp_storage();
    for i in 0..5 {
        storage
            .record_capture(&CaptureRecord {
                timestamp: 1_000 + i as i64,
                cycle_id: "cycle-A".into(),
                monitor_ids: vec![0, 1],
                foreground_app: Some("Editor".into()),
                foreground_window_title: Some(format!("file-{}.rs", i)),
                image_path: format!("/tmp/img-{}.enc", i),
                ocr_text: None,
                thumbnail_path: None,
            })
            .unwrap();
    }
    let rows = storage.list_recent_captures(3).unwrap();
    assert_eq!(rows.len(), 3);
    // Ordered DESC by timestamp
    assert_eq!(rows[0].timestamp, 1_004);
    assert_eq!(rows[1].timestamp, 1_003);
    assert_eq!(rows[2].timestamp, 1_002);
}

#[test]
fn update_capture_ocr_writes_text_and_engine() {
    let (storage, _) = temp_storage();
    let id = storage
        .record_capture(&CaptureRecord {
            timestamp: 100,
            cycle_id: "cycle-B".into(),
            monitor_ids: vec![0],
            foreground_app: None,
            foreground_window_title: None,
            image_path: "/tmp/orig.enc".into(),
            ocr_text: None,
            thumbnail_path: None,
        })
        .unwrap();

    storage
        .update_capture_ocr(
            id,
            "extracted text",
            "tesseract-cli",
            Some("/tmp/thumb.enc"),
            false,
        )
        .unwrap();

    let rows = storage.list_recent_captures(10).unwrap();
    let row = rows.iter().find(|r| r.id == id).expect("row exists");
    assert_eq!(row.ocr_text.as_deref(), Some("extracted text"));
    assert_eq!(row.ocr_engine.as_deref(), Some("tesseract-cli"));
    assert_eq!(row.thumbnail_path.as_deref(), Some("/tmp/thumb.enc"));
    // image_path unchanged because delete_original=false
    assert_eq!(row.image_path, "/tmp/orig.enc");
}

#[test]
fn update_capture_ocr_with_delete_original_replaces_image_path() {
    let (storage, _) = temp_storage();
    let id = storage
        .record_capture(&CaptureRecord {
            timestamp: 100,
            cycle_id: "cycle-C".into(),
            monitor_ids: vec![0],
            foreground_app: None,
            foreground_window_title: None,
            image_path: "/tmp/full.enc".into(),
            ocr_text: None,
            thumbnail_path: None,
        })
        .unwrap();

    storage
        .update_capture_ocr(id, "txt", "mock", Some("/tmp/thumb.enc"), true)
        .unwrap();

    let rows = storage.list_recent_captures(10).unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.image_path, "/tmp/thumb.enc");
}

#[test]
fn skip_log_dedupes_by_timestamp() {
    let (storage, _) = temp_storage();
    storage.record_skip(123, "idle").unwrap();
    storage.record_skip(123, "paused").unwrap(); // INSERT OR REPLACE — second wins
    let count: u32 = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM skip_log WHERE timestamp = 123",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 1);

    let reason: String = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT reason FROM skip_log WHERE timestamp = 123",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(reason, "paused");
}

#[test]
fn directories_created_on_open() {
    let (storage, dir) = temp_storage();
    assert!(dir.join("screenshots").is_dir());
    assert!(dir.join("archive").is_dir());
    assert!(dir.join("recommendations").is_dir());
    assert!(dir.join("thumbnails").is_dir());
    assert!(dir.join("database.sqlite").is_file());
    drop(storage);
}

/// Helper for the recommendation tests below. Inserts a row directly so
/// we don't depend on the analysis pipeline running.
fn insert_rec(
    storage: &Storage,
    cycle_id: &str,
    name: &str,
    score: f32,
    suppressed: bool,
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    storage
        .with_conn(|c| {
            c.execute(
                "INSERT INTO recommendations
                 (id, cycle_id, generated_at, tier_id, name, score, suppressed,
                  description, observed_pattern, build_complexity, confidence)
                 VALUES (?1, ?2, 0, 'time-reclaimers', ?3, ?4, ?5, 'desc', 'pat', 'low', 0.9)",
                rusqlite::params![id, cycle_id, name, score, suppressed as i64],
            )?;
            Ok(())
        })
        .unwrap();
    id
}

#[test]
fn list_recommendations_orders_by_score_desc_and_excludes_suppressed_by_default() {
    let (storage, _) = temp_storage();
    let id_low = insert_rec(&storage, "c1", "Low score", 10.0, false);
    let id_high = insert_rec(&storage, "c1", "High score", 100.0, false);
    let id_supp = insert_rec(&storage, "c1", "Suppressed", 200.0, true);

    let visible = storage.list_recommendations(false, 50).unwrap();
    let names: Vec<&str> = visible.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["High score", "Low score"]);

    // Sanity: suppressed appears when included
    let with_supp = storage.list_recommendations(true, 50).unwrap();
    assert_eq!(with_supp.len(), 3);
    // suppressed go to the bottom
    assert_eq!(with_supp.last().unwrap().name, "Suppressed");

    // and the IDs round-tripped
    assert!(visible.iter().any(|r| r.id == id_high));
    assert!(visible.iter().any(|r| r.id == id_low));
    assert!(!visible.iter().any(|r| r.id == id_supp));
}

#[test]
fn list_recommendations_respects_limit() {
    let (storage, _) = temp_storage();
    for i in 0..5 {
        insert_rec(&storage, "c1", &format!("Rec{i}"), i as f32, false);
    }
    let rows = storage.list_recommendations(false, 3).unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn set_disposition_updates_row_and_timestamp() {
    let (storage, _) = temp_storage();
    let id = insert_rec(&storage, "c1", "Pick me", 42.0, false);
    storage
        .set_disposition(&id, "implemented", Some("worked great"))
        .unwrap();

    let row: (String, Option<String>, Option<i64>) = storage
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT disposition, disposition_note, disposition_at
                 FROM recommendations WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(row.0, "implemented");
    assert_eq!(row.1.as_deref(), Some("worked great"));
    assert!(row.2.unwrap() > 0);
}

#[test]
fn set_disposition_rejects_unknown_action() {
    let (storage, _) = temp_storage();
    let id = insert_rec(&storage, "c1", "x", 1.0, false);
    let result = storage.set_disposition(&id, "delete_forever", None);
    assert!(result.is_err());
}

#[test]
fn set_disposition_errors_on_unknown_rec_id() {
    let (storage, _) = temp_storage();
    let result = storage.set_disposition("does-not-exist", "implemented", None);
    assert!(result.is_err());
}

#[test]
fn list_prior_dispositions_returns_only_dispositioned() {
    let (storage, _) = temp_storage();
    let id_no = insert_rec(&storage, "c1", "Pending", 10.0, false);
    let id_yes = insert_rec(&storage, "c1", "Decided", 20.0, false);
    storage
        .set_disposition(&id_yes, "not_interested", None)
        .unwrap();

    let priors = storage.list_prior_dispositions().unwrap();
    assert_eq!(priors.len(), 1);
    assert_eq!(priors[0].name, "Decided");
    let _ = id_no; // referenced for clarity
}

#[test]
fn opening_storage_twice_at_same_root_is_safe() {
    let dir = std::env::temp_dir().join(format!("as-reopen-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let s1 = Storage::open_at(dir.clone()).unwrap();
    let id = s1
        .record_capture(&CaptureRecord {
            timestamp: 1,
            cycle_id: "c".into(),
            monitor_ids: vec![0],
            foreground_app: None,
            foreground_window_title: None,
            image_path: "/tmp/x".into(),
            ocr_text: None,
            thumbnail_path: None,
        })
        .unwrap();
    drop(s1);

    let s2 = Storage::open_at(dir.clone()).unwrap();
    let rows = s2.list_recent_captures(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
}
