//! Localhost HTTP server that handles disposition link clicks from
//! email. Bound to `127.0.0.1` only — never reachable off-box.
//!
//! Per the pinned plan decision, links are HMAC-signed with the
//! per-install secret and have a default 60-day expiry, so a user can
//! restart the app without invalidating yesterday's email.

use crate::email::link_signer::{
    parse_token_from_query, DispositionAction, LinkSigner, SignedToken,
};
use crate::storage::Storage;
use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct DispositionServerConfig {
    /// Listen on a specific port. Use 0 (the default) to let the OS pick
    /// an ephemeral port.
    pub requested_port: u16,
}

#[derive(Clone)]
struct AppState {
    storage: Arc<Storage>,
    signer: Arc<LinkSigner>,
}

#[derive(Debug, Deserialize)]
struct DispositionQuery {
    rec: Option<String>,
    action: Option<String>,
    issued: Option<i64>,
    exp: Option<i64>,
    sig: Option<String>,
}

pub struct RunningServer {
    pub addr: SocketAddr,
    pub origin: String,
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub task: tokio::task::JoinHandle<()>,
}

impl RunningServer {
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.task.await;
    }
}

/// Bind + start the disposition server. Returns once the server is
/// accepting connections, with the URL origin to use in email links.
pub async fn start(
    storage: Arc<Storage>,
    signer: Arc<LinkSigner>,
    config: DispositionServerConfig,
) -> Result<RunningServer> {
    let state = AppState { storage, signer };
    let app = Router::new()
        .route("/disposition", get(handle_disposition))
        .route("/oauth/callback", get(handle_oauth_callback))
        .route("/health", get(handle_health))
        .with_state(state);

    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.requested_port))
            .await
            .context("binding disposition server to 127.0.0.1")?;
    let addr = listener.local_addr()?;
    let origin = format!("http://{}:{}", addr.ip(), addr.port());
    // Record for the OAuth callback handler to read when it builds the
    // redirect_uri at exchange time (v0.5.7).
    record_server_origin(&origin);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = server.await {
            tracing::warn!("disposition server stopped with error: {:#}", e);
        }
    });

    Ok(RunningServer {
        addr,
        origin,
        shutdown_tx,
        task,
    })
}

async fn handle_health() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(rename = "error_description")]
    error_description: Option<String>,
}

/// Receives the Google OAuth redirect after the user consents. Looks
/// up the in-flight flow by `state`, performs the code-for-token
/// exchange, persists the refresh token, and signals completion via
/// the shared `oauth_flow` store. Renders a "you can close this tab"
/// page so the user gets visual confirmation.
async fn handle_oauth_callback(Query(q): Query<OAuthCallbackQuery>) -> impl IntoResponse {
    use crate::email::oauth;
    use crate::email::oauth_flow;

    if let Some(err) = q.error.as_deref() {
        let detail = q.error_description.as_deref().unwrap_or("");
        if let Some(state) = q.state.as_deref() {
            oauth_flow::mark_error(state, format!("Google returned: {err} {detail}"));
        }
        return error_page(
            StatusCode::BAD_REQUEST,
            "Gmail consent declined",
            &format!("{err}: {detail}. Close this tab and try again."),
        );
    }

    let code = match q.code {
        Some(c) => c,
        None => {
            return error_page(
                StatusCode::BAD_REQUEST,
                "Missing code",
                "Google didn't include an authorization code in the redirect.",
            )
        }
    };
    let state_token = match q.state {
        Some(s) => s,
        None => {
            return error_page(
                StatusCode::BAD_REQUEST,
                "Missing state",
                "Google didn't include the state token. CSRF protection failed.",
            )
        }
    };

    let verifier = match oauth_flow::take_verifier(&state_token) {
        Some(v) => v,
        None => {
            return error_page(
                StatusCode::BAD_REQUEST,
                "Unknown state",
                "AgentScout has no record of starting an OAuth flow with this state token. \
                 You may have clicked a stale link, or the app restarted while the flow was \
                 in progress. Try clicking Connect Gmail again.",
            )
        }
    };

    // Reconstitute the OAuth client config from the keychain — same
    // call shape `cmd_begin_gmail_oauth` used.
    let creds = match crate::secrets::get_gmail_oauth_creds() {
        Ok(Some(c)) => c,
        _ => {
            oauth_flow::mark_error(&state_token, "OAuth client creds vanished mid-flow".into());
            return error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Lost OAuth client creds",
                "AgentScout's Gmail OAuth client_id is no longer in the keychain. Open \
                 Settings → Gmail and re-enter your client_id.",
            );
        }
    };
    // Derive the redirect_uri from this very server's origin. Building
    // it explicitly avoids the request struct giving us an externally-
    // controlled host header.
    let redirect_uri = match request_origin() {
        Some(o) => format!("{o}/oauth/callback"),
        None => {
            oauth_flow::mark_error(&state_token, "could not determine server origin".into());
            return error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server origin unknown",
                "Could not determine our own origin to complete OAuth.",
            );
        }
    };
    let oauth_config = oauth::OAuthConfig {
        client_id: creds.client_id,
        client_secret: creds.client_secret,
        redirect_uri,
    };

    match oauth::complete_auth(&oauth_config, code, verifier).await {
        Ok(_access_token) => {
            // The user-visible label could be derived from a Gmail
            // userinfo call but adds another scope (userinfo.email).
            // For v0.5.7 we just say "connected" — recipient address is
            // configured separately in Settings.
            oauth_flow::mark_complete(&state_token, "connected".to_string());
            oauth_success_page()
        }
        Err(e) => {
            oauth_flow::mark_error(&state_token, format!("{:#}", e));
            error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Token exchange failed",
                &format!(
                    "Google rejected the authorization code: {:#}. Open Settings → Gmail and \
                     try again.",
                    e
                ),
            )
        }
    }
}

/// Origin of THIS disposition server. Set during `start()` via a
/// OnceLock so the OAuth callback handler can build its own
/// redirect_uri without threading state through axum extractors.
fn request_origin() -> Option<String> {
    server_origin_holder().get().cloned()
}

fn server_origin_holder() -> &'static std::sync::OnceLock<String> {
    static HOLDER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    &HOLDER
}

fn record_server_origin(origin: &str) {
    let _ = server_origin_holder().set(origin.to_string());
}

fn oauth_success_page() -> axum::response::Response {
    let html = "<!doctype html><html><head><title>AgentScout · Gmail connected</title></head>
<body style='font-family:-apple-system,Segoe UI,sans-serif;background:#f3f4f6;margin:0;padding:48px 24px;color:#111827;'>
<div style='max-width:480px;margin:0 auto;background:#fff;border:1px solid #e5e7eb;border-radius:8px;padding:32px;'>
<div style='font-size:13px;color:#16a34a;text-transform:uppercase;letter-spacing:0.04em;'>AgentScout</div>
<h1 style='margin:6px 0 12px;font-size:22px;font-weight:600;'>Gmail connected.</h1>
<p style='font-size:14px;line-height:1.6;color:#374151;margin:0;'>You can close this tab and return to AgentScout. Set a recipient email and run a cycle to receive your first recommendation email.</p>
</div></body></html>";
    Html(html).into_response()
}

async fn handle_disposition(
    State(state): State<AppState>,
    Query(q): Query<DispositionQuery>,
) -> impl IntoResponse {
    let token = match validate_query(&q) {
        Ok(t) => t,
        Err(reason) => return error_page(StatusCode::BAD_REQUEST, "Bad request", &reason),
    };

    if let Err(e) = state.signer.verify(
        &token.rec_id,
        token.action,
        token.issued_at,
        token.expires_at,
        &token.signature,
    ) {
        return error_page(
            StatusCode::UNAUTHORIZED,
            "Link rejected",
            &format!(
                "{e}. Links expire 60 days after they're sent — if this email is older than that, open the AgentScout app and dispose of the recommendation directly."
            ),
        );
    }

    let now = chrono::Utc::now().timestamp();
    let action_label = token.action.as_str();
    let result = state
        .storage
        .with_conn(|c| {
            let updated = c.execute(
                "UPDATE recommendations
                 SET disposition = ?1, disposition_at = ?2
                 WHERE id = ?3",
                rusqlite::params![action_label, now, token.rec_id],
            )?;
            Ok(updated)
        })
        .map_err(|e| format!("{:#}", e));

    match result {
        Ok(0) => error_page(
            StatusCode::NOT_FOUND,
            "Not found",
            "AgentScout doesn't have a recommendation with that ID — perhaps it was archived already.",
        ),
        Ok(_) => success_page(token.action, &token.rec_id),
        Err(e) => error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal error",
            &format!("Failed to record disposition: {e}"),
        ),
    }
}

fn validate_query(q: &DispositionQuery) -> std::result::Result<SignedToken, String> {
    let serialized = format!(
        "rec={}&action={}&issued={}&exp={}&sig={}",
        urlencoding::encode(q.rec.as_deref().unwrap_or("")),
        q.action.as_deref().unwrap_or(""),
        q.issued.unwrap_or(0),
        q.exp.unwrap_or(0),
        urlencoding::encode(q.sig.as_deref().unwrap_or(""))
    );
    parse_token_from_query(&serialized).map_err(|e| format!("malformed link: {e:#}"))
}

fn success_page(action: DispositionAction, rec_id: &str) -> axum::response::Response {
    let (header, blurb) = match action {
        DispositionAction::Implemented => (
            "Implemented — nice.",
            "AgentScout will skip similar recommendations next cycle.",
        ),
        DispositionAction::NotInterested => (
            "Got it — not interested.",
            "AgentScout will skip semantically similar suggestions next cycle.",
        ),
        DispositionAction::MaybeLater => (
            "Saved for later.",
            "AgentScout will keep this in mind but may resurface it if new evidence appears.",
        ),
    };
    let html = format!(
        "<!doctype html><html><head><title>AgentScout</title></head>
<body style='font-family:-apple-system,Segoe UI,sans-serif;background:#f3f4f6;margin:0;padding:48px 24px;color:#111827;'>
<div style='max-width:480px;margin:0 auto;background:#fff;border:1px solid #e5e7eb;border-radius:8px;padding:32px;'>
<div style='font-size:13px;color:#6b7280;text-transform:uppercase;letter-spacing:0.04em;'>AgentScout</div>
<h1 style='margin:6px 0 12px;font-size:22px;font-weight:600;'>{header}</h1>
<p style='font-size:14px;line-height:1.6;color:#374151;margin:0;'>{blurb}</p>
<p style='font-size:11px;color:#9ca3af;margin-top:32px;'>Recommendation {rec_id} · You can close this tab.</p>
</div></body></html>"
    );
    Html(html).into_response()
}

fn error_page(status: StatusCode, header: &str, blurb: &str) -> axum::response::Response {
    let html = format!(
        "<!doctype html><html><head><title>AgentScout</title></head>
<body style='font-family:-apple-system,Segoe UI,sans-serif;background:#f3f4f6;margin:0;padding:48px 24px;color:#111827;'>
<div style='max-width:480px;margin:0 auto;background:#fff;border:1px solid #fee2e2;border-radius:8px;padding:32px;'>
<div style='font-size:13px;color:#dc2626;text-transform:uppercase;letter-spacing:0.04em;'>AgentScout · {status}</div>
<h1 style='margin:6px 0 12px;font-size:20px;font-weight:600;'>{header}</h1>
<p style='font-size:14px;line-height:1.6;color:#374151;margin:0;'>{blurb}</p>
</div></body></html>",
        status = status.as_u16()
    );
    (status, Html(html)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::CaptureRecord;

    async fn fixture() -> (Arc<Storage>, Arc<LinkSigner>, RunningServer, String) {
        let dir = std::env::temp_dir().join(format!("as-disp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Arc::new(Storage::open_at(dir.clone()).unwrap());

        // Seed a recommendation row to dispose of.
        let rec_id = uuid::Uuid::new_v4().to_string();
        storage
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO recommendations (id, cycle_id, generated_at, tier_id, name, suppressed)
                     VALUES (?1, 'cycle-1', 0, 'time-reclaimers', 'test rec', 0)",
                    rusqlite::params![rec_id],
                )?;
                Ok(())
            })
            .unwrap();

        let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
        let server = start(
            storage.clone(),
            signer.clone(),
            DispositionServerConfig::default(),
        )
        .await
        .unwrap();
        (storage, signer, server, rec_id)
    }

    #[tokio::test]
    async fn valid_link_records_disposition() {
        let (storage, signer, server, rec_id) = fixture().await;
        let url = format!(
            "{}/disposition{}",
            server.origin,
            signer.build_query(&rec_id, DispositionAction::Implemented)
        );
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let text = resp.text().await.unwrap();
        assert!(text.contains("Implemented"));

        let stored: String = storage
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT disposition FROM recommendations WHERE id = ?1",
                    rusqlite::params![rec_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(stored, "implemented");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn tampered_signature_rejected() {
        let (storage, signer, server, rec_id) = fixture().await;
        let mut q = signer.build_query(&rec_id, DispositionAction::NotInterested);
        // Flip the last char of the signature
        q.pop();
        q.push('x');
        let url = format!("{}/disposition{}", server.origin, q);
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
        let stored: Option<String> = storage
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT disposition FROM recommendations WHERE id = ?1",
                    rusqlite::params![rec_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(
            stored.is_none(),
            "tampered link must not record disposition"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn missing_query_params_returns_400() {
        let (_storage, _signer, server, _rec_id) = fixture().await;
        let url = format!("{}/disposition?rec=abc", server.origin);
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_recommendation_returns_404() {
        let (_storage, signer, server, _rec_id) = fixture().await;
        let bogus_id = uuid::Uuid::new_v4().to_string();
        let url = format!(
            "{}/disposition{}",
            server.origin,
            signer.build_query(&bogus_id, DispositionAction::MaybeLater)
        );
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let dir = std::env::temp_dir().join(format!("as-disp-h-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Arc::new(Storage::open_at(dir.clone()).unwrap());
        let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
        let server = start(storage, signer, DispositionServerConfig::default())
            .await
            .unwrap();
        let resp = reqwest::get(format!("{}/health", server.origin))
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn server_binds_only_to_loopback() {
        let dir = std::env::temp_dir().join(format!("as-disp-l-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Arc::new(Storage::open_at(dir.clone()).unwrap());
        let signer = Arc::new(LinkSigner::new(vec![0xAA; 32]));
        let server = start(storage, signer, DispositionServerConfig::default())
            .await
            .unwrap();
        assert!(server.addr.ip().is_loopback());
        server.shutdown().await;
    }

    // Suppress unused-variable warning on tuple destructure in some tests
    #[allow(dead_code)]
    fn _capture_referenced(_: CaptureRecord) {}
}
