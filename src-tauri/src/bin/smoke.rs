//! Smoke test binary — runs the capture pipeline end-to-end against
//! synthetic inputs and prints a pass/fail summary.
//!
//! Usage:
//!     cargo run --bin smoke
//!     cargo run --bin smoke -- --live   # use real xcap + tesseract if present
//!
//! Exit code 0 on success, non-zero on any failure. Designed to be safe
//! to run in CI: never touches the real platform data dir, never makes
//! network calls (unless --live is passed and tesseract triggers a
//! traineddata download).

use agentscout::capture::{
    ActivityMonitor, FakeScreenshotter, Scheduler, Screenshotter, TickOutcome, XcapScreenshotter,
};
use agentscout::config::Config;
use agentscout::ocr::{MockEngine, OcrEngine, TesseractCliEngine};
use agentscout::storage::{crypto::FileCrypto, Storage};
use anyhow::{bail, Result};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let live = args.iter().any(|a| a == "--live");

    println!(
        "AgentScout smoke test ({})",
        if live { "live" } else { "mock" }
    );

    let workdir = std::env::temp_dir().join(format!("as-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workdir)?;
    println!("  workdir:    {}", workdir.display());

    let started = Instant::now();
    let result = run_pipeline(&workdir, live).await;

    // Best-effort cleanup — keep the dir on failure for inspection.
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(&workdir);
    } else {
        println!("  workdir kept for inspection: {}", workdir.display());
    }
    println!("  elapsed:    {:.2}s", started.elapsed().as_secs_f64());

    match result {
        Ok(stats) => {
            println!("PASS");
            println!("  captures:   {}", stats.captures_recorded);
            println!("  ocr text:   {}", stats.ocr_text_chars);
            println!("  ocr engine: {}", stats.ocr_engine);
            println!("  thumbnail:  {} bytes", stats.thumbnail_bytes);
            Ok(())
        }
        Err(e) => {
            println!("FAIL: {:#}", e);
            std::process::exit(1);
        }
    }
}

struct PipelineStats {
    captures_recorded: usize,
    ocr_text_chars: usize,
    ocr_engine: String,
    thumbnail_bytes: usize,
}

async fn run_pipeline(workdir: &std::path::Path, live: bool) -> Result<PipelineStats> {
    // Storage + crypto
    let storage = Arc::new(Storage::open_at(workdir.to_path_buf())?);
    let crypto = Arc::new(FileCrypto::with_key([0xAB; 32]));

    // Config — budget mode on so the OCR + thumbnail pipeline runs
    let mut cfg = Config::default();
    cfg.capture.budget_mode = true;
    cfg.capture.idle_threshold_minutes = 60;
    cfg.capture.work_hours.enabled = false;
    cfg.capture.monitors = vec![agentscout::config::MonitorConfig {
        id: 0,
        enabled: true,
        label: "smoke-primary".into(),
    }];
    let config = Arc::new(Mutex::new(cfg));

    // OCR engine
    let ocr_engine: Arc<dyn OcrEngine> = if live {
        match TesseractCliEngine::new(workdir.join("tessdata")) {
            Ok(e) => {
                println!("  ocr:        tesseract CLI (live)");
                Arc::new(e)
            }
            Err(e) => {
                println!("  ocr:        mock (tesseract unavailable: {})", e);
                Arc::new(MockEngine::new("smoke-mock-ocr-text"))
            }
        }
    } else {
        println!("  ocr:        mock");
        Arc::new(MockEngine::new("smoke-mock-ocr-text"))
    };

    // Screenshotter
    let screenshotter: Arc<dyn Screenshotter> = if live {
        println!("  capture:    xcap (live display required)");
        Arc::new(XcapScreenshotter::new())
    } else {
        println!("  capture:    fake (synthetic 1024x768)");
        Arc::new(FakeScreenshotter::single(1024, 768, [120, 180, 240, 255]))
    };

    let activity = ActivityMonitor::fake_always_active();
    let scheduler = Arc::new(Scheduler::new(
        config.clone(),
        storage.clone(),
        crypto.clone(),
        activity,
        ocr_engine,
        screenshotter,
    ));

    // Tick once
    let outcome = scheduler.tick_once().await?;
    let capture_id = match outcome {
        TickOutcome::Captured { capture_id } => capture_id,
        TickOutcome::Skipped { reason } => bail!("scheduler skipped: {}", reason),
    };
    println!("  tick:       captured id={}", capture_id);

    // Read back
    let rows = storage.list_recent_captures(10)?;
    let row = rows
        .iter()
        .find(|r| r.id == capture_id)
        .ok_or_else(|| anyhow::anyhow!("capture {} not found in DB", capture_id))?;

    let thumbnail_path = row
        .thumbnail_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("budget mode failed to record thumbnail_path"))?;

    let thumb_bytes = crypto.decrypt_from_file(std::path::Path::new(thumbnail_path))?;
    if thumb_bytes.is_empty() {
        bail!("decrypted thumbnail is empty");
    }
    // Verify it actually decodes as an image
    image::load_from_memory(&thumb_bytes)?;

    let ocr_text = row.ocr_text.clone().unwrap_or_default();

    Ok(PipelineStats {
        captures_recorded: rows.len(),
        ocr_text_chars: ocr_text.len(),
        ocr_engine: row.ocr_engine.clone().unwrap_or_else(|| "<none>".into()),
        thumbnail_bytes: thumb_bytes.len(),
    })
}
