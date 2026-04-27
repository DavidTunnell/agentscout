//! End-to-end pipeline integration tests.
//!
//! Drives `Scheduler::tick_once` against synthetic screenshots, mock OCR,
//! and a temporary storage root — proves the capture → encrypt → OCR →
//! thumbnail → DB pipeline works as a unit, without requiring a real
//! display, real Tesseract, or real Anthropic API access.

use agentscout::capture::{
    ActivityMonitor, FakeScreenshotter, Scheduler, Screenshotter, TickOutcome,
};
use agentscout::config::Config;
use agentscout::ocr::{MockEngine, OcrEngine};
use agentscout::storage::{crypto::FileCrypto, Storage};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

struct Fixture {
    _tempdir: PathBuf,
    storage: Arc<Storage>,
    config: Arc<Mutex<Config>>,
    scheduler: Arc<Scheduler>,
}

async fn build_fixture(
    budget_mode: bool,
    ocr: Arc<dyn OcrEngine>,
    screenshotter: Arc<dyn Screenshotter>,
) -> Fixture {
    let tempdir = std::env::temp_dir().join(format!("as-pipeline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tempdir).unwrap();

    let storage = Arc::new(Storage::open_at(tempdir.clone()).expect("open storage"));
    let crypto = Arc::new(FileCrypto::with_key([0x42; 32]));

    let mut cfg = Config::default();
    cfg.capture.budget_mode = budget_mode;
    cfg.capture.idle_threshold_minutes = 60; // generous so fake activity passes
    cfg.capture.work_hours.enabled = false;
    cfg.capture.monitors = vec![agentscout::config::MonitorConfig {
        id: 0,
        enabled: true,
        label: "fake".into(),
    }];
    let config = Arc::new(Mutex::new(cfg));

    let activity = ActivityMonitor::fake_always_active();
    let scheduler = Arc::new(Scheduler::new(
        config.clone(),
        storage.clone(),
        crypto,
        activity,
        ocr,
        screenshotter,
    ));

    Fixture {
        _tempdir: tempdir,
        storage,
        config,
        scheduler,
    }
}

#[tokio::test]
async fn full_resolution_capture_persists_encrypted_image_and_db_row() {
    let fx = build_fixture(
        false, // budget_mode = off, keep full image
        Arc::new(MockEngine::new("")),
        Arc::new(FakeScreenshotter::single(800, 600, [200, 100, 50, 255])),
    )
    .await;

    let outcome = fx.scheduler.tick_once().await.expect("tick succeeds");
    let TickOutcome::Captured { capture_id } = outcome else {
        panic!("expected Captured, got {:?}", outcome);
    };

    let rows = fx.storage.list_recent_captures(10).unwrap();
    assert_eq!(rows.len(), 1, "exactly one capture should exist");
    let row = &rows[0];
    assert_eq!(row.id, capture_id);
    assert!(row.thumbnail_path.is_none(), "no thumbnail in non-budget mode");
    assert!(row.ocr_text.is_none(), "no OCR text in non-budget mode");
    assert!(
        std::path::Path::new(&row.image_path).exists(),
        "encrypted image file must exist on disk"
    );

    let raw = std::fs::read(&row.image_path).unwrap();
    assert!(raw.len() > 12, "encrypted blob must include nonce + cipher");
}

#[tokio::test]
async fn budget_mode_runs_full_pipeline_and_replaces_image_with_thumbnail() {
    let ocr_text = "Recognized text from the synthetic image.";
    let fx = build_fixture(
        true, // budget_mode = on
        Arc::new(MockEngine::new(ocr_text)),
        Arc::new(FakeScreenshotter::single(1920, 1080, [128, 128, 128, 255])),
    )
    .await;

    let outcome = fx.scheduler.tick_once().await.expect("tick succeeds");
    let TickOutcome::Captured { .. } = outcome else {
        panic!("expected Captured, got {:?}", outcome);
    };

    let rows = fx.storage.list_recent_captures(10).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.ocr_text.as_deref(), Some(ocr_text));
    assert_eq!(row.ocr_engine.as_deref(), Some("mock"));

    let thumb_path = row.thumbnail_path.as_deref().expect("thumbnail recorded");
    assert!(
        std::path::Path::new(thumb_path).exists(),
        "thumbnail file must exist on disk"
    );

    // image_path should now point at the thumbnail (original deleted).
    assert_eq!(row.image_path, thumb_path);
}

#[tokio::test]
async fn paused_scheduler_records_skip_and_no_capture() {
    let fx = build_fixture(
        false,
        Arc::new(MockEngine::new("")),
        Arc::new(FakeScreenshotter::single(800, 600, [0, 0, 0, 255])),
    )
    .await;
    fx.scheduler.set_paused(true);

    let outcome = fx.scheduler.tick_once().await.expect("tick succeeds");
    match outcome {
        TickOutcome::Skipped { reason } => assert_eq!(reason, "paused"),
        other => panic!("expected Skipped(paused), got {:?}", other),
    }
    let rows = fx.storage.list_recent_captures(10).unwrap();
    assert!(rows.is_empty(), "no capture rows when paused");
}

#[tokio::test]
async fn no_enabled_monitors_skips_with_reason() {
    let fx = build_fixture(
        false,
        Arc::new(MockEngine::new("")),
        Arc::new(FakeScreenshotter::single(800, 600, [0, 0, 0, 255])),
    )
    .await;
    {
        let mut cfg = fx.config.lock().await;
        for m in &mut cfg.capture.monitors {
            m.enabled = false;
        }
    }
    let outcome = fx.scheduler.tick_once().await.expect("tick succeeds");
    match outcome {
        TickOutcome::Skipped { reason } => assert_eq!(reason, "no_monitors_enabled"),
        other => panic!("expected Skipped(no_monitors_enabled), got {:?}", other),
    }
}

#[tokio::test]
async fn budget_mode_survives_ocr_failure() {
    // OCR engine that always fails — simulates Tesseract crash mid-capture.
    struct FailingOcr;
    use async_trait::async_trait;
    #[async_trait]
    impl OcrEngine for FailingOcr {
        async fn extract(&self, _: &[u8]) -> anyhow::Result<agentscout::ocr::OcrResult> {
            Err(anyhow::anyhow!("simulated OCR failure"))
        }
        fn name(&self) -> &str {
            "failing"
        }
    }

    let fx = build_fixture(
        true,
        Arc::new(FailingOcr),
        Arc::new(FakeScreenshotter::single(1024, 768, [50, 200, 50, 255])),
    )
    .await;

    let outcome = fx.scheduler.tick_once().await.expect("tick must succeed");
    let TickOutcome::Captured { .. } = outcome else {
        panic!("expected Captured even on OCR failure");
    };
    let rows = fx.storage.list_recent_captures(10).unwrap();
    assert_eq!(rows.len(), 1);
    // OCR failed, so the row keeps its original (non-thumbnail) image.
    // ocr_text remains None.
    assert!(rows[0].ocr_text.is_none());
}
