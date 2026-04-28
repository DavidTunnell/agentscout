pub mod analysis;
pub mod anthropic;
pub mod capture;
pub mod config;
pub mod conversation;
pub mod email;
pub mod ocr;
pub mod storage;

use crate::capture::{ActivityMonitor, Scheduler, TickOutcome, XcapScreenshotter};
use crate::config::Config;
use crate::email::{start_disposition_server, DispositionServerConfig, LinkSigner, RunningServer};
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
    pub link_signer: Arc<LinkSigner>,
    pub disposition_server_origin: String,
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
            cmd_list_recommendations,
            cmd_set_disposition,
            cmd_get_cycle_status,
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

    // Disposition server + HMAC link signer using the per-install secret.
    let install_secret = crate::storage::crypto::load_or_init_install_secret()?;
    let link_signer = Arc::new(LinkSigner::new(install_secret));
    let disposition = start_disposition_server(
        storage.clone(),
        link_signer.clone(),
        DispositionServerConfig::default(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("starting disposition server: {:#}", e))?;
    let disposition_origin = disposition.origin.clone();
    info!("disposition server listening at {}", disposition_origin);
    keep_disposition_server_alive(disposition);

    let state = AppState {
        config: config.clone(),
        storage: storage.clone(),
        crypto: crypto.clone(),
        scheduler: scheduler.clone(),
        paused,
        link_signer,
        disposition_server_origin: disposition_origin,
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

/// Move the running disposition server into a static slot so its tokio
/// task stays alive for the app's lifetime. We never need to read it
/// back; the server runs until process exit.
fn keep_disposition_server_alive(server: RunningServer) {
    use std::sync::OnceLock;
    static HOLDER: OnceLock<std::sync::Mutex<Option<RunningServer>>> = OnceLock::new();
    let cell = HOLDER.get_or_init(|| std::sync::Mutex::new(None));
    *cell.lock().expect("disposition server holder poisoned") = Some(server);
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
async fn cmd_list_recommendations(
    state: State<'_, AppState>,
    include_suppressed: Option<bool>,
) -> Result<Vec<RecommendationView>, String> {
    let include_suppressed = include_suppressed.unwrap_or(false);
    state
        .storage
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, cycle_id, generated_at, tier_id, name, description,
                        observed_pattern, frequency_per_week, est_time_saved_minutes,
                        strategic_value, build_complexity, confidence, score,
                        suppressed, disposition, disposition_at
                 FROM recommendations
                 WHERE (?1 OR suppressed = 0)
                 ORDER BY suppressed ASC, score DESC
                 LIMIT 100",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![include_suppressed], |row| {
                    Ok(RecommendationView {
                        id: row.get(0)?,
                        cycle_id: row.get(1)?,
                        generated_at: row.get(2)?,
                        tier_id: row.get(3)?,
                        name: row.get(4)?,
                        description: row.get(5)?,
                        observed_pattern: row.get(6)?,
                        frequency_per_week: row.get(7)?,
                        est_time_saved_minutes: row.get(8)?,
                        strategic_value: row.get(9)?,
                        build_complexity: row.get(10)?,
                        confidence: row.get(11)?,
                        score: row.get(12)?,
                        suppressed: row.get::<_, i64>(13)? != 0,
                        disposition: row.get(14)?,
                        disposition_at: row.get(15)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .map_err(|e| format!("{:#}", e))
}

#[derive(serde::Serialize)]
struct RecommendationView {
    id: String,
    cycle_id: String,
    generated_at: i64,
    tier_id: String,
    name: String,
    description: Option<String>,
    observed_pattern: Option<String>,
    frequency_per_week: Option<f32>,
    est_time_saved_minutes: Option<f32>,
    strategic_value: Option<String>,
    build_complexity: Option<String>,
    confidence: Option<f32>,
    score: Option<f32>,
    suppressed: bool,
    disposition: Option<String>,
    disposition_at: Option<i64>,
}

#[tauri::command]
async fn cmd_set_disposition(
    state: State<'_, AppState>,
    rec_id: String,
    action: String,
    note: Option<String>,
) -> Result<(), String> {
    if !matches!(
        action.as_str(),
        "implemented" | "not_interested" | "maybe_later"
    ) {
        return Err(format!("unknown disposition action: {action}"));
    }
    let now = chrono::Utc::now().timestamp();
    state
        .storage
        .with_conn(|c| {
            let updated = c.execute(
                "UPDATE recommendations
                 SET disposition = ?1, disposition_at = ?2, disposition_note = ?3
                 WHERE id = ?4",
                rusqlite::params![action, now, note, rec_id],
            )?;
            if updated == 0 {
                anyhow::bail!("no recommendation with id {}", rec_id);
            }
            Ok(())
        })
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_get_cycle_status(state: State<'_, AppState>) -> Result<CycleStatusView, String> {
    let s = state
        .storage
        .load_active_hours()
        .map_err(|e| format!("{:#}", e))?;
    let cfg = state.config.lock().await;
    let threshold_seconds = i64::from(cfg.analysis.active_hours_threshold) * 3600;
    Ok(CycleStatusView {
        cycle_id: s.current_cycle_id,
        active_hours: s.active_seconds as f32 / 3600.0,
        threshold_hours: cfg.analysis.active_hours_threshold,
        progress_pct: ((s.active_seconds as f64 / threshold_seconds.max(1) as f64) * 100.0)
            .min(100.0) as f32,
        cycle_started_at: s.cycle_started_at,
        disposition_server_origin: state.disposition_server_origin.clone(),
    })
}

#[derive(serde::Serialize)]
struct CycleStatusView {
    cycle_id: String,
    active_hours: f32,
    threshold_hours: u32,
    progress_pct: f32,
    cycle_started_at: i64,
    disposition_server_origin: String,
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
