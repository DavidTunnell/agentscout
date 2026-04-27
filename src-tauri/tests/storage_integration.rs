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
