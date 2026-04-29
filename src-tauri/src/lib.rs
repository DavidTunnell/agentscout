pub mod analysis;
pub mod anthropic;
pub mod capture;
pub mod config;
pub mod conversation;
pub mod email;
pub mod ocr;
pub mod secrets;
pub mod storage;

use crate::analysis::{run_cycle, OrchestratorDeps};
use crate::anthropic::{
    AnthropicClient, CompletionRequest, LiveAnthropicClient, Message as AnthropicMessage, Role,
};
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
    /// In-progress setup conversation. v0.5.6 — held in memory; if the
    /// app restarts mid-conversation the user re-opens the wizard and
    /// starts over (conversations are 3-5 turns max so this is cheap).
    pub setup_conv: Arc<Mutex<Option<crate::conversation::SetupConversation>>>,
    pub tier_calib_conv: Arc<Mutex<Option<crate::conversation::TierCalibrationConversation>>>,
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
            cmd_get_cost_projection,
            cmd_get_capability_info,
            cmd_get_settings,
            cmd_update_settings,
            // v0.5.5: credentials + on-demand analysis
            cmd_set_anthropic_key,
            cmd_test_anthropic_key,
            cmd_clear_anthropic_key,
            cmd_get_credentials_status,
            cmd_run_cycle_now,
            // v0.5.6: setup + tier-calibration conversations
            cmd_start_setup_conversation,
            cmd_continue_setup_conversation,
            cmd_finalize_setup_conversation,
            cmd_start_tier_calibration,
            cmd_continue_tier_calibration,
            cmd_finalize_tier_calibration,
            cmd_get_personalization_status,
            // v0.5.7: Gmail OAuth + email send
            cmd_set_gmail_oauth_creds,
            cmd_clear_gmail_oauth_creds,
            cmd_begin_gmail_oauth,
            cmd_poll_gmail_oauth_status,
            cmd_disconnect_gmail,
            cmd_set_recipient_email,
            cmd_send_test_email,
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
        setup_conv: Arc::new(Mutex::new(None)),
        tier_calib_conv: Arc::new(Mutex::new(None)),
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
) -> Result<Vec<crate::storage::RecommendationRow>, String> {
    state
        .storage
        .list_recommendations(include_suppressed.unwrap_or(false), 100)
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_set_disposition(
    state: State<'_, AppState>,
    rec_id: String,
    action: String,
    note: Option<String>,
) -> Result<(), String> {
    state
        .storage
        .set_disposition(&rec_id, &action, note.as_deref())
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
async fn cmd_get_cost_projection(
    state: State<'_, AppState>,
) -> Result<crate::analysis::CycleProjection, String> {
    let cfg = state.config.lock().await;
    let input = crate::analysis::ProjectionInput {
        model_cluster_summary: cfg.analysis.model_cluster_summary.clone(),
        model_synthesis: cfg.analysis.model_synthesis.clone(),
        ..Default::default()
    };
    let table = crate::analysis::default_pricing_table();
    crate::analysis::project_cycle_cost(&table, &input).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_get_capability_info() -> Result<CapabilityInfo, String> {
    Ok(detect_capabilities())
}

#[derive(serde::Serialize)]
struct CapabilityInfo {
    os: String,
    /// "x11" | "wayland" | "headless" | "windows" | "macos"
    session_type: String,
    /// True when foreground-window detection is reliable on this session.
    /// Wayland without XDG portal extensions degrades to app-name-only.
    foreground_detection_reliable: bool,
    /// Tesseract availability check — true when the binary is on PATH or
    /// in a known install location.
    tesseract_available: bool,
    /// Banner copy to render on first launch when reliability is degraded.
    /// None when the platform is fine.
    degraded_notice: Option<String>,
}

fn detect_capabilities() -> CapabilityInfo {
    let os = std::env::consts::OS.to_string();
    let (session_type, reliable, notice) = if cfg!(target_os = "linux") {
        let display = std::env::var_os("DISPLAY");
        let wayland = std::env::var_os("WAYLAND_DISPLAY");
        let xdg = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        if wayland.is_some() || xdg.eq_ignore_ascii_case("wayland") {
            (
                "wayland".to_string(),
                false,
                Some(
                    "AgentScout is running on Wayland. Foreground-window titles aren't \
                     reliably available; clusters will use app names only. For finer \
                     clustering, log into an X11 session."
                        .to_string(),
                ),
            )
        } else if display.is_some() || xdg.eq_ignore_ascii_case("x11") {
            ("x11".to_string(), true, None)
        } else {
            (
                "headless".to_string(),
                false,
                Some(
                    "AgentScout can't see a graphical session. Foreground detection \
                     is disabled. Captures will still record but won't be tagged with \
                     window titles."
                        .to_string(),
                ),
            )
        }
    } else {
        let s = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "other"
        };
        (s.to_string(), true, None)
    };

    let tesseract_available =
        crate::ocr::TesseractCliEngine::new(std::env::temp_dir().join("agentscout-cap-probe"))
            .is_ok();

    CapabilityInfo {
        os,
        session_type,
        foreground_detection_reliable: reliable,
        tesseract_available,
        degraded_notice: notice,
    }
}

#[tauri::command]
async fn cmd_get_settings(state: State<'_, AppState>) -> Result<crate::config::Config, String> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

#[tauri::command]
async fn cmd_update_settings(
    state: State<'_, AppState>,
    new_config: crate::config::Config,
) -> Result<(), String> {
    let mut cfg = state.config.lock().await;
    *cfg = new_config;
    cfg.save().map_err(|e| format!("{:#}", e))
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

// ───────────────────────────────────────────────────────────────────────
// v0.5.5 — credentials + on-demand cycle
// ───────────────────────────────────────────────────────────────────────

#[tauri::command]
async fn cmd_set_anthropic_key(key: String) -> Result<(), String> {
    crate::secrets::set_anthropic_key(&key).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_clear_anthropic_key() -> Result<(), String> {
    crate::secrets::clear_anthropic_key().map_err(|e| format!("{:#}", e))
}

#[derive(serde::Serialize)]
struct TestKeyResult {
    ok: bool,
    /// Friendly message: "Connected — Claude responded with X tokens" or
    /// the trimmed Anthropic error message.
    message: String,
}

/// Hits Anthropic with a tiny prompt to validate the stored key. Used by
/// the Settings UI's "Test connection" button. Returns a structured
/// result instead of `Result<_, String>` so the UI can render success
/// and failure with the same render path.
///
/// Uses the user's configured cluster-summary model (default
/// claude-sonnet-4-6) so a successful test confirms the same model the
/// real analysis cycle will use is callable. If the user's configured
/// model doesn't exist on Anthropic, the error message surfaces here
/// rather than failing later inside `run_cycle`.
#[tauri::command]
async fn cmd_test_anthropic_key(state: State<'_, AppState>) -> Result<TestKeyResult, String> {
    let key = match crate::secrets::get_anthropic_key().map_err(|e| format!("{:#}", e))? {
        Some(k) => k,
        None => {
            return Ok(TestKeyResult {
                ok: false,
                message: "No API key set. Paste your key above and click Save.".into(),
            })
        }
    };

    let model = {
        let cfg = state.config.lock().await;
        cfg.analysis.model_cluster_summary.clone()
    };

    let client = LiveAnthropicClient::new(key);
    let messages = vec![AnthropicMessage {
        role: Role::User,
        content: "Reply with exactly the word: OK".into(),
    }];
    let req = CompletionRequest {
        messages: &messages,
        system: None,
        model: &model,
        max_tokens: 16,
        cache_breakpoint: None,
    };

    match client.complete(req).await {
        Ok(resp) => Ok(TestKeyResult {
            ok: true,
            message: format!(
                "Connected — {} replied ({} input + {} output tokens).",
                model, resp.usage.input_tokens, resp.usage.output_tokens
            ),
        }),
        Err(e) => {
            let raw = format!("{:#}", e);
            // Trim long body dumps; users only need the gist.
            let trimmed: String = raw.chars().take(400).collect();
            Ok(TestKeyResult {
                ok: false,
                message: trimmed,
            })
        }
    }
}

#[derive(serde::Serialize)]
struct CredsStatus {
    has_anthropic_key: bool,
    has_gmail_oauth: bool,
    recipient_email: Option<String>,
    /// Convenience flag for the "Setup status" badge — green when this is
    /// true. v0.5.5 considers Gmail optional, so this is true when the
    /// Anthropic key is set; v0.5.7 will tighten this once OAuth is wired.
    minimum_setup_complete: bool,
}

#[tauri::command]
async fn cmd_get_credentials_status(state: State<'_, AppState>) -> Result<CredsStatus, String> {
    let cfg = state.config.lock().await;
    let has_anthropic_key = crate::secrets::has_anthropic_key();
    // Gmail "fully connected" requires both OAuth client creds AND a
    // refresh token. v0.5.7 wires both. The Settings UI uses these
    // separately to give specific guidance ("you need creds" vs "you
    // need to click Connect").
    let has_gmail_creds = crate::secrets::has_gmail_oauth_creds();
    let has_gmail_refresh = crate::email::has_stored_refresh_token().unwrap_or(false);
    let has_gmail_oauth = has_gmail_creds && has_gmail_refresh;
    Ok(CredsStatus {
        has_anthropic_key,
        has_gmail_oauth,
        recipient_email: cfg
            .email
            .recipient
            .clone()
            .or_else(|| cfg.email.gmail_account.clone()),
        minimum_setup_complete: has_anthropic_key,
    })
}

#[derive(serde::Serialize)]
struct RunCycleResultView {
    cycle_id: String,
    n_captures: usize,
    n_clusters: usize,
    n_recommendations: usize,
    n_visible: usize,
    n_suppressed: usize,
    estimated_cost_usd: f64,
    email_sent: bool,
}

/// Runs a single analysis cycle right now against captures from the
/// last `hours_back` hours. Skips email (analysis-only mode) when Gmail
/// OAuth isn't connected; recommendations land in the Recommendations
/// tab regardless. v0.5.5 entry point — the user clicks "Run analysis
/// now" and this fires.
#[tauri::command]
async fn cmd_run_cycle_now(
    state: State<'_, AppState>,
    hours_back: u32,
) -> Result<RunCycleResultView, String> {
    // Read API key first so we fail fast with a clean message rather
    // than panicking inside run_cycle when the client gets a 401.
    let api_key = crate::secrets::get_anthropic_key()
        .map_err(|e| format!("reading api key: {:#}", e))?
        .ok_or_else(|| {
            "No Anthropic API key set. Open Settings → paste your key → Save.".to_string()
        })?;

    // Re-tag recent captures into the current cycle so they actually
    // get analyzed. Without this, captures from before the current
    // active-hours session would be skipped by the orchestrator's
    // "WHERE cycle_id = ?" load.
    let cycle_id = state
        .storage
        .load_active_hours()
        .map_err(|e| format!("{:#}", e))?
        .current_cycle_id;
    let cutoff = chrono::Utc::now().timestamp() - i64::from(hours_back) * 3600;
    let n_retagged = state
        .storage
        .retag_captures_into_cycle(&cycle_id, cutoff)
        .map_err(|e| format!("retagging captures: {:#}", e))?;
    info!(
        "cmd_run_cycle_now: re-tagged {} captures from last {}h into cycle {}",
        n_retagged, hours_back, cycle_id
    );

    // Build the orchestrator deps. v0.5.7: when Gmail OAuth refresh
    // token is present and creds are stored, we refresh an access token
    // and pass Some(GmailSender). Otherwise email = None (analysis-only,
    // v0.5.5 behavior).
    let cfg_owned: Config = {
        let cfg = state.config.lock().await;
        cfg.clone()
    };

    let live = LiveAnthropicClient::new(api_key);
    let gmail_sender = crate::email::GmailSender::new();
    let access_token = try_fresh_gmail_access_token(&state.disposition_server_origin)
        .await
        .ok();

    let (email_arg, token_arg): (Option<&dyn crate::email::EmailSender>, Option<String>) =
        match access_token {
            Some(tok) => (Some(&gmail_sender), Some(tok.token)),
            None => (None, None),
        };

    // Load user-profile.md and tier-definitions.json from storage if
    // they exist; otherwise use baseline defaults so the cycle runs
    // even before the user has done the setup conversation. The
    // recommendations will be less personalized but won't be empty.
    let storage_root = state.storage.root().to_path_buf();
    let user_profile_md = std::fs::read_to_string(storage_root.join("user-profile.md"))
        .unwrap_or_else(|_| DEFAULT_USER_PROFILE_MD.to_string());
    let tier_definitions_json = std::fs::read_to_string(storage_root.join("tier-definitions.json"))
        .unwrap_or_else(|_| DEFAULT_TIER_DEFINITIONS_JSON.to_string());

    let deps = OrchestratorDeps {
        config: &cfg_owned,
        storage: state.storage.clone(),
        anthropic: &live,
        email: email_arg,
        link_signer: state.link_signer.clone(),
        gmail_access_token: token_arg,
        server_origin: state.disposition_server_origin.clone(),
        user_profile_md,
        tier_definitions_json,
    };

    let result = run_cycle(deps).await.map_err(|e| format!("{:#}", e))?;
    Ok(RunCycleResultView {
        cycle_id: result.cycle_id,
        n_captures: result.n_captures,
        n_clusters: result.n_clusters,
        n_recommendations: result.n_recommendations,
        n_visible: result.n_visible,
        n_suppressed: result.n_suppressed,
        estimated_cost_usd: result.estimated_cost_usd,
        email_sent: result.email_message_id.is_some(),
    })
}

// ───────────────────────────────────────────────────────────────────────
// v0.5.7 — Gmail OAuth + email send
// ───────────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct SetGmailOAuthCredsArgs {
    client_id: String,
    client_secret: Option<String>,
}

#[tauri::command]
async fn cmd_set_gmail_oauth_creds(args: SetGmailOAuthCredsArgs) -> Result<(), String> {
    crate::secrets::set_gmail_oauth_creds(&args.client_id, args.client_secret.as_deref())
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_clear_gmail_oauth_creds() -> Result<(), String> {
    // Also revoke the stored refresh token — credentials and refresh
    // are paired; clearing one without the other leaves a dead token.
    let _ = crate::email::oauth::revoke_stored_refresh_token();
    crate::secrets::clear_gmail_oauth_creds().map_err(|e| format!("{:#}", e))
}

#[derive(serde::Serialize)]
struct BeginGmailOAuthResult {
    auth_url: String,
    csrf_state: String,
}

/// Starts a fresh Gmail OAuth flow. Returns the Google consent URL +
/// the csrf state token (the frontend uses the state token to poll
/// status).
#[tauri::command]
async fn cmd_begin_gmail_oauth(
    state: State<'_, AppState>,
) -> Result<BeginGmailOAuthResult, String> {
    let creds = crate::secrets::get_gmail_oauth_creds()
        .map_err(|e| format!("{:#}", e))?
        .ok_or_else(|| {
            "No Gmail OAuth client credentials set. Open Settings → Gmail and paste your \
             GCP OAuth client_id (and optionally client_secret) first."
                .to_string()
        })?;

    let redirect_uri = format!("{}/oauth/callback", state.disposition_server_origin);
    let oauth_config = crate::email::OAuthConfig {
        client_id: creds.client_id,
        client_secret: creds.client_secret,
        redirect_uri,
    };
    let init = crate::email::begin_auth(&oauth_config).map_err(|e| format!("{:#}", e))?;
    crate::email::oauth_flow::put_flow(init.csrf_state.clone(), init.pkce_verifier);
    Ok(BeginGmailOAuthResult {
        auth_url: init.auth_url,
        csrf_state: init.csrf_state,
    })
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum GmailOAuthStatus {
    Unknown,
    InProgress,
    Completed { account_label: String },
    Failed { error: String },
}

#[tauri::command]
async fn cmd_poll_gmail_oauth_status(csrf_state: String) -> Result<GmailOAuthStatus, String> {
    use crate::email::oauth_flow::FlowStatus;
    Ok(match crate::email::oauth_flow::poll_status(&csrf_state) {
        FlowStatus::Unknown => GmailOAuthStatus::Unknown,
        FlowStatus::InProgress => GmailOAuthStatus::InProgress,
        FlowStatus::Completed { account_label } => {
            // Reading harvest forgets the flow so memory doesn't grow
            // across reconnect attempts.
            crate::email::oauth_flow::forget(&csrf_state);
            GmailOAuthStatus::Completed { account_label }
        }
        FlowStatus::Failed { error } => {
            crate::email::oauth_flow::forget(&csrf_state);
            GmailOAuthStatus::Failed { error }
        }
    })
}

#[tauri::command]
async fn cmd_disconnect_gmail() -> Result<(), String> {
    crate::email::oauth::revoke_stored_refresh_token().map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_set_recipient_email(state: State<'_, AppState>, email: String) -> Result<(), String> {
    let trimmed = email.trim();
    if trimmed.is_empty() {
        // Allow empty to clear.
        let mut cfg = state.config.lock().await;
        cfg.email.recipient = None;
        return cfg.save().map_err(|e| format!("{:#}", e));
    }
    if !trimmed.contains('@') {
        return Err("That doesn't look like an email address.".into());
    }
    let mut cfg = state.config.lock().await;
    cfg.email.recipient = Some(trimmed.to_string());
    // Default Gmail account = recipient unless the user explicitly set
    // a separate from. Avoids "no Gmail account configured" errors.
    if cfg.email.gmail_account.is_none() {
        cfg.email.gmail_account = Some(trimmed.to_string());
    }
    cfg.save().map_err(|e| format!("{:#}", e))
}

#[tauri::command]
async fn cmd_send_test_email(state: State<'_, AppState>) -> Result<String, String> {
    let recipient = {
        let cfg = state.config.lock().await;
        cfg.email
            .recipient
            .clone()
            .or_else(|| cfg.email.gmail_account.clone())
            .ok_or_else(|| {
                "No recipient email set. Save one in Settings → Gmail first.".to_string()
            })?
    };
    let access = try_fresh_gmail_access_token(&state.disposition_server_origin)
        .await
        .map_err(|e| format!("{:#}", e))?;

    let from = {
        let cfg = state.config.lock().await;
        cfg.email
            .gmail_account
            .clone()
            .unwrap_or_else(|| recipient.clone())
    };

    let rendered = crate::email::RenderedEmail {
        subject: "AgentScout test email".into(),
        html_body: "<p>If you're reading this, AgentScout's Gmail OAuth setup is \
                    working.</p><p>This was sent from the \"Send test email\" button in \
                    Settings.</p>"
            .into(),
        plain_body:
            "If you're reading this, AgentScout's Gmail OAuth setup is working.\n\nSent from the \"Send test email\" button in Settings.".into(),
    };
    let sender = crate::email::GmailSender::new();
    use crate::email::EmailSender as _;
    sender
        .send(&access.token, &from, &recipient, &rendered)
        .await
        .map(|id| format!("Sent (Gmail message id: {id}). Check your inbox."))
        .map_err(|e| format!("{:#}", e))
}

/// Helper used by both `cmd_run_cycle_now`'s email step and
/// `cmd_send_test_email` to convert a stored refresh token + creds
/// into a fresh access token. Returns Err if any of the inputs are
/// missing OR the refresh exchange fails.
async fn try_fresh_gmail_access_token(
    disposition_server_origin: &str,
) -> anyhow::Result<crate::email::AccessToken> {
    let creds = crate::secrets::get_gmail_oauth_creds()?
        .ok_or_else(|| anyhow::anyhow!("Gmail OAuth client_id/secret not set"))?;
    if !crate::email::has_stored_refresh_token()? {
        anyhow::bail!("No Gmail refresh token — user has not authorized Gmail yet");
    }
    let oauth_config = crate::email::OAuthConfig {
        client_id: creds.client_id,
        client_secret: creds.client_secret,
        redirect_uri: format!("{disposition_server_origin}/oauth/callback"),
    };
    crate::email::refresh_access_token(&oauth_config).await
}

// ───────────────────────────────────────────────────────────────────────
// v0.5.6 — setup + tier-calibration conversations
// ───────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct ConversationStep {
    /// Latest assistant message (the question or wrap-up text shown to
    /// the user).
    bot_message: String,
    /// Total turns in conversation history (assistant + user combined).
    turn_count: usize,
}

#[derive(serde::Serialize)]
struct PersonalizationStatus {
    has_user_profile: bool,
    has_tier_definitions: bool,
    user_profile_excerpt: Option<String>,
}

/// Build a `LiveAnthropicClient` from the keychain. Shared helper for
/// every conversation cmd (and `cmd_run_cycle_now`). Returns the
/// "no key set" error string the UI shows verbatim.
fn build_live_anthropic() -> Result<LiveAnthropicClient, String> {
    let key = crate::secrets::get_anthropic_key()
        .map_err(|e| format!("reading api key: {:#}", e))?
        .ok_or_else(|| {
            "No Anthropic API key set. Open Settings → paste your key → Save first.".to_string()
        })?;
    Ok(LiveAnthropicClient::new(key))
}

/// Initial bot message for the setup wizard's "what's your role"
/// conversation. Resets any in-progress conversation; the wizard
/// always starts fresh.
#[tauri::command]
async fn cmd_start_setup_conversation(
    state: State<'_, AppState>,
    template_id: String,
) -> Result<String, String> {
    let conv = crate::conversation::SetupConversation::new(&template_id)
        .map_err(|e| format!("{:#}", e))?;
    let opener = conv
        .conversation
        .messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let mut slot = state.setup_conv.lock().await;
    *slot = Some(conv);
    Ok(opener)
}

#[tauri::command]
async fn cmd_continue_setup_conversation(
    state: State<'_, AppState>,
    reply: String,
) -> Result<ConversationStep, String> {
    let client = build_live_anthropic()?;
    let model = {
        let cfg = state.config.lock().await;
        cfg.analysis.model_cluster_summary.clone()
    };

    let mut slot = state.setup_conv.lock().await;
    let conv = slot
        .as_mut()
        .ok_or_else(|| "No active setup conversation. Click Start first.".to_string())?;
    let msg = conv
        .step(&reply, &client, &model)
        .await
        .map_err(|e| format!("{:#}", e))?
        .to_string();
    let turn_count = conv.conversation.messages.len();
    Ok(ConversationStep {
        bot_message: msg,
        turn_count,
    })
}

#[tauri::command]
async fn cmd_finalize_setup_conversation(state: State<'_, AppState>) -> Result<String, String> {
    let client = build_live_anthropic()?;
    let model = {
        let cfg = state.config.lock().await;
        cfg.analysis.model_synthesis.clone()
    };
    let storage_root = state.storage.root().to_path_buf();

    let mut slot = state.setup_conv.lock().await;
    let conv = slot
        .as_mut()
        .ok_or_else(|| "No active setup conversation to finalize.".to_string())?;

    let path = conv
        .finalize(&client, &model, &storage_root)
        .await
        .map_err(|e| format!("{:#}", e))?;

    // Clear the slot — finalize() consumed the conversation.
    *slot = None;

    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn cmd_start_tier_calibration(
    state: State<'_, AppState>,
    template_id: String,
) -> Result<String, String> {
    // Tier calibration needs the user-profile.md. Fail loudly with a
    // clear message if the user skipped setup conversation.
    let storage_root = state.storage.root().to_path_buf();
    let user_profile_md =
        std::fs::read_to_string(storage_root.join("user-profile.md")).map_err(|_| {
            "Tier calibration needs a user-profile.md first. Run setup conversation \
             above, then come back."
                .to_string()
        })?;
    let conv =
        crate::conversation::TierCalibrationConversation::new(&template_id, &user_profile_md)
            .map_err(|e| format!("{:#}", e))?;
    let opener = conv
        .conversation
        .messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let mut slot = state.tier_calib_conv.lock().await;
    *slot = Some(conv);
    Ok(opener)
}

#[tauri::command]
async fn cmd_continue_tier_calibration(
    state: State<'_, AppState>,
    reply: String,
) -> Result<ConversationStep, String> {
    let client = build_live_anthropic()?;
    let model = {
        let cfg = state.config.lock().await;
        cfg.analysis.model_cluster_summary.clone()
    };

    let mut slot = state.tier_calib_conv.lock().await;
    let conv = slot
        .as_mut()
        .ok_or_else(|| "No active tier-calibration conversation. Click Start first.".to_string())?;
    let msg = conv
        .step(&reply, &client, &model)
        .await
        .map_err(|e| format!("{:#}", e))?
        .to_string();
    let turn_count = conv.conversation.messages.len();
    Ok(ConversationStep {
        bot_message: msg,
        turn_count,
    })
}

#[tauri::command]
async fn cmd_finalize_tier_calibration(state: State<'_, AppState>) -> Result<String, String> {
    let client = build_live_anthropic()?;
    let model = {
        let cfg = state.config.lock().await;
        cfg.analysis.model_synthesis.clone()
    };
    let storage_root = state.storage.root().to_path_buf();

    let mut slot = state.tier_calib_conv.lock().await;
    let conv = slot
        .as_mut()
        .ok_or_else(|| "No active tier-calibration conversation to finalize.".to_string())?;

    let path = conv
        .finalize(&client, &model, &storage_root)
        .await
        .map_err(|e| format!("{:#}", e))?;

    *slot = None;

    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn cmd_get_personalization_status(
    state: State<'_, AppState>,
) -> Result<PersonalizationStatus, String> {
    let storage_root = state.storage.root().to_path_buf();
    let profile_path = storage_root.join("user-profile.md");
    let tiers_path = storage_root.join("tier-definitions.json");

    let has_user_profile = profile_path.exists();
    let has_tier_definitions = tiers_path.exists();

    // First 280 chars of profile as a preview for the UI.
    let user_profile_excerpt = if has_user_profile {
        std::fs::read_to_string(&profile_path)
            .ok()
            .map(|s| s.chars().take(280).collect::<String>())
    } else {
        None
    };

    Ok(PersonalizationStatus {
        has_user_profile,
        has_tier_definitions,
        user_profile_excerpt,
    })
}

const DEFAULT_USER_PROFILE_MD: &str = r#"# AgentScout User Profile (default)

You haven't completed the setup conversation yet, so AgentScout is
running with a generic profile. Recommendations will be less
personalized than they could be. Open Settings → "Run setup
conversation" to personalize."#;

/// Minimal valid tier-definitions JSON used until the user completes
/// the tier-calibration conversation. Two tiers covering the common
/// "tactical/quantitative" and "strategic/qualitative" splits, both
/// enabled with neutral weights.
const DEFAULT_TIER_DEFINITIONS_JSON: &str = r#"{
  "schema_version": 1,
  "tiers": [
    {
      "id": "time-reclaimers",
      "name": "Time Reclaimers",
      "description": "Tactical agents that save time on a recurring task you do today.",
      "weight": 1.0,
      "scoring": "quantitative",
      "qualitative_multiplier": 50.0,
      "enabled": true,
      "example_shapes": []
    },
    {
      "id": "strategic",
      "name": "Strategic Helpers",
      "description": "Agents that improve quality of decisions or shift how you work.",
      "weight": 1.0,
      "scoring": "qualitative",
      "qualitative_multiplier": 100.0,
      "enabled": true,
      "example_shapes": []
    }
  ]
}"#;

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

    let stderr_layer = fmt::layer().with_target(false).compact();

    // Daily-rotated log file under <storage>/logs/agentscout.log.<YYYY-MM-DD>.
    // Best-effort: if we can't resolve the storage dir at boot, skip the
    // file layer rather than crash. Hold the guard in a OnceLock so the
    // appender flushes on process exit.
    let file_layer = match crate::config::storage_root() {
        Ok(root) => {
            let log_dir = root.join("logs");
            if let Err(e) = std::fs::create_dir_all(&log_dir) {
                eprintln!(
                    "warning: could not create log dir {}: {e}",
                    log_dir.display()
                );
                None
            } else {
                let appender = tracing_appender::rolling::daily(&log_dir, "agentscout.log");
                let (writer, guard) = tracing_appender::non_blocking(appender);
                store_log_guard(guard);
                Some(
                    fmt::layer()
                        .with_target(true)
                        .with_writer(writer)
                        .with_ansi(false),
                )
            }
        }
        Err(e) => {
            eprintln!("warning: could not resolve storage root for logs: {e:#}");
            None
        }
    };

    if let Some(file_layer) = file_layer {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .init();
    }

    install_panic_hook();
}

fn store_log_guard(guard: tracing_appender::non_blocking::WorkerGuard) {
    use std::sync::OnceLock;
    static HOLDER: OnceLock<std::sync::Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
        OnceLock::new();
    let cell = HOLDER.get_or_init(|| std::sync::Mutex::new(None));
    *cell.lock().expect("log guard mutex poisoned") = Some(guard);
}

/// Crash reporter — writes a redacted panic record to
/// `<storage>/logs/crash-<timestamp>.log` and re-raises the panic so the
/// process still aborts. Never sends anything off-box per SPEC §10.1.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("(non-string panic payload)");
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "(unknown location)".to_string());
        let when = chrono::Utc::now().to_rfc3339();
        let record = format!(
            "AgentScout crash report\n\
             timestamp: {when}\n\
             location:  {location}\n\
             payload:   {payload}\n\
             \n\
             This file lives only on your machine. AgentScout never \
             uploads crash data. Open an issue and attach if you'd like \
             help triaging: https://github.com/DavidTunnell/agentscout/issues\n"
        );

        if let Ok(root) = crate::config::storage_root() {
            let log_dir = root.join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let safe_when = when.replace(':', "-");
            let path = log_dir.join(format!("crash-{safe_when}.log"));
            if let Err(e) = std::fs::write(&path, record.as_bytes()) {
                eprintln!("(could not write crash log to {}: {e})", path.display());
            } else {
                eprintln!("crash log written to {}", path.display());
            }
        }
        eprintln!("{record}");

        prev(info);
    }));
}
