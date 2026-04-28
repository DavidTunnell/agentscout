pub mod analysis;
pub mod anthropic;
pub mod capture;
pub mod config;
pub mod conversation;
pub mod ocr;
pub mod storage;

use crate::capture::{ActivityMonitor, Scheduler, TickOutcome, XcapScreenshotter};
use crate::config::Config;
use crate::ocr::{MockEngine, OcrEngine, TesseractCliEngine};
use crate::storage::{crypto::FileCrypto, CaptureRow, Storage};
use anyhow::Result;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager, State,
};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub storage: Arc<Storage>,
    pub crypto: Arc<FileCrypto>,
    pub scheduler: Arc<Scheduler>,
    pub paused: Arc<AtomicBool>,
}

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            cmd_get_status,
            cmd_toggle_pause,
            cmd_run_tick_now,
            cmd_list_recent_captures,
            cmd_get_capture_image,
            cmd_list_starter_templates,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = bootstrap(handle).await {
                    error!("bootstrap failed: {:#}", e);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run tauri application");
}

async fn bootstrap(app: AppHandle) -> Result<()> {
    let config = Arc::new(Mutex::new(Config::load_or_init()?));
    let storage = Arc::new(Storage::open()?);
    let crypto = Arc::new(FileCrypto::load_or_init()?);

    let poll_interval = Duration::from_secs(10);
    let (activity, _handle) = ActivityMonitor::start(poll_interval);

    let ocr_engine: Arc<dyn OcrEngine> = build_ocr_engine(storage.root());
    let screenshotter = Arc::new(XcapScreenshotter::new());

    let scheduler = Arc::new(Scheduler::new(
        config.clone(),
        storage.clone(),
        crypto.clone(),
        activity.clone(),
        ocr_engine,
        screenshotter,
    ));
    let paused = scheduler.pause_handle();

    let state = AppState {
        config: config.clone(),
        storage: storage.clone(),
        crypto: crypto.clone(),
        scheduler: scheduler.clone(),
        paused,
    };

    build_tray(&app, scheduler.clone())?;

    app.manage(state);

    let sched_for_run = scheduler.clone();
    tauri::async_runtime::spawn(async move {
        sched_for_run.run().await;
    });

    info!("agentscout bootstrap complete");
    Ok(())
}

fn build_tray(app: &AppHandle, scheduler: Arc<Scheduler>) -> Result<TrayIcon> {
    let pause_item = MenuItem::with_id(app, "toggle_pause", "Pause capture", true, None::<&str>)?;
    let tick_now = MenuItem::with_id(app, "tick_now", "Capture now", true, None::<&str>)?;
    let open_main = MenuItem::with_id(app, "open_main", "Open window", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&pause_item, &tick_now, &open_main, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("AgentScout")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event: MenuEvent| {
            handle_menu_event(app, event.id.as_ref(), &scheduler);
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    Ok(builder.build(app)?)
}

fn handle_menu_event(app: &AppHandle, id: &str, scheduler: &Arc<Scheduler>) {
    match id {
        "toggle_pause" => {
            let paused = scheduler.pause_handle();
            let was = paused.load(std::sync::atomic::Ordering::SeqCst);
            paused.store(!was, std::sync::atomic::Ordering::SeqCst);
            info!("paused: {} -> {}", was, !was);
        }
        "tick_now" => {
            let sched = scheduler.clone();
            tauri::async_runtime::spawn(async move {
                match sched.tick_once().await {
                    Ok(TickOutcome::Captured { capture_id }) => {
                        info!("manual tick captured id={}", capture_id);
                    }
                    Ok(TickOutcome::Skipped { reason }) => {
                        info!("manual tick skipped: {}", reason);
                    }
                    Err(e) => error!("manual tick failed: {:#}", e),
                }
            });
        }
        "open_main" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        "quit" => {
            app.exit(0);
        }
        _ => {}
    }
}

#[tauri::command]
async fn cmd_get_status(state: State<'_, AppState>) -> Result<StatusPayload, String> {
    let cfg = state.config.lock().await;
    Ok(StatusPayload {
        paused: state.paused.load(std::sync::atomic::Ordering::SeqCst),
        cadence_minutes: cfg.capture.cadence_minutes,
        budget_mode: cfg.capture.budget_mode,
        monitors_enabled: cfg.capture.monitors.iter().filter(|m| m.enabled).count() as u32,
        schema_version: cfg.schema_version,
    })
}

#[tauri::command]
async fn cmd_toggle_pause(state: State<'_, AppState>) -> Result<bool, String> {
    let was = state.paused.load(std::sync::atomic::Ordering::SeqCst);
    state
        .paused
        .store(!was, std::sync::atomic::Ordering::SeqCst);
    Ok(!was)
}

#[tauri::command]
async fn cmd_run_tick_now(state: State<'_, AppState>) -> Result<String, String> {
    match state.scheduler.tick_once().await {
        Ok(TickOutcome::Captured { capture_id }) => Ok(format!("captured id={}", capture_id)),
        Ok(TickOutcome::Skipped { reason }) => Ok(format!("skipped: {}", reason)),
        Err(e) => Err(format!("{:#}", e)),
    }
}

#[derive(serde::Serialize)]
struct StatusPayload {
    paused: bool,
    cadence_minutes: u32,
    budget_mode: bool,
    monitors_enabled: u32,
    schema_version: u32,
}

#[tauri::command]
async fn cmd_list_recent_captures(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<CaptureRow>, String> {
    state
        .storage
        .list_recent_captures(limit.unwrap_or(50))
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_get_capture_image(
    state: State<'_, AppState>,
    capture_id: i64,
) -> Result<CaptureImagePayload, String> {
    let captures = state
        .storage
        .list_recent_captures(500)
        .map_err(|e| format!("{:#}", e))?;
    let row = captures
        .into_iter()
        .find(|r| r.id == capture_id)
        .ok_or_else(|| format!("capture {} not found", capture_id))?;

    // Prefer thumbnail (smaller payload); fall back to original.
    let (path, mime) = match row.thumbnail_path.as_deref() {
        Some(p) => (p.to_string(), "image/webp"),
        None => (row.image_path.clone(), "image/png"),
    };

    let bytes = state
        .crypto
        .decrypt_from_file(std::path::Path::new(&path))
        .map_err(|e| format!("decrypt {}: {:#}", path, e))?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(CaptureImagePayload {
        capture_id,
        mime: mime.to_string(),
        data_base64: b64,
        from_thumbnail: row.thumbnail_path.is_some(),
        ocr_text: row.ocr_text,
    })
}

#[derive(serde::Serialize)]
struct CaptureImagePayload {
    capture_id: i64,
    mime: String,
    data_base64: String,
    from_thumbnail: bool,
    ocr_text: Option<String>,
}

#[tauri::command]
async fn cmd_list_starter_templates() -> Result<Vec<StarterTemplateView>, String> {
    Ok(crate::conversation::STARTER_TEMPLATES
        .iter()
        .map(|t| StarterTemplateView {
            id: t.id.to_string(),
            name: t.name.to_string(),
            description: t.description.to_string(),
        })
        .collect())
}

#[derive(serde::Serialize)]
struct StarterTemplateView {
    id: String,
    name: String,
    description: String,
}

fn build_ocr_engine(storage_root: &std::path::Path) -> Arc<dyn OcrEngine> {
    let tessdata_dir = storage_root.join("tessdata");
    match TesseractCliEngine::new(tessdata_dir) {
        Ok(engine) => {
            info!("OCR engine: tesseract CLI");
            Arc::new(engine)
        }
        Err(e) => {
            warn!(
                "tesseract CLI unavailable ({:#}); budget mode will run with mock OCR \
                 returning empty text. Install Tesseract to enable real OCR.",
                e
            );
            Arc::new(MockEngine::new(""))
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,agentscout=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).compact())
        .init();
}
