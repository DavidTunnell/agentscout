//! Process-wide OAuth flow state. Bridges the disposition server (which
//! receives the Google redirect) and the Tauri commands (which start
//! the flow and poll for completion).
//!
//! Why a global: the disposition server task is spawned at app boot
//! before any Tauri command can wire state into it. Threading a state
//! parameter through `start_disposition_server` would also work, but a
//! `OnceLock<Mutex<HashMap>>` is far simpler and the OAuth flow is a
//! short-lived (seconds) handshake with at most one or two flows in
//! flight.
//!
//! v0.5.7 introduces this; v0.5.8 onward leaves it untouched.

use chrono::Utc;
use oauth2::PkceCodeVerifier;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// One in-progress OAuth flow keyed by Google's CSRF state token.
pub struct OAuthFlow {
    pub pkce_verifier: PkceCodeVerifier,
    pub started_at: i64,
    /// Set when the callback completes successfully — the email address
    /// the access token authorizes and the recipient field can default
    /// to.
    pub completed: Option<OAuthCompletion>,
    /// Set when the callback fails with a Google error (consent denied,
    /// invalid_grant, etc.). UI surfaces this verbatim.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthCompletion {
    /// Authorization succeeded; refresh token was stored in the
    /// keychain by `oauth::complete_auth`.
    pub completed_at: i64,
    /// User-facing label — the Gmail address. May be empty if Google's
    /// userinfo endpoint isn't reachable; UI handles that.
    pub account_label: String,
}

fn store() -> &'static Mutex<HashMap<String, OAuthFlow>> {
    static STORE: OnceLock<Mutex<HashMap<String, OAuthFlow>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn put_flow(state: String, verifier: PkceCodeVerifier) {
    let mut s = store().lock().expect("oauth flow store poisoned");
    // Sweep flows older than 10 min — Google's auth_url's typically
    // valid for 10 min and a stale flow tying up a state token serves
    // no purpose.
    let cutoff = Utc::now().timestamp() - 600;
    s.retain(|_, f| f.started_at >= cutoff);
    s.insert(
        state,
        OAuthFlow {
            pkce_verifier: verifier,
            started_at: Utc::now().timestamp(),
            completed: None,
            error: None,
        },
    );
}

/// Take ownership of the verifier for a given state — used when the
/// callback receives the code and needs to do the token exchange.
pub fn take_verifier(state: &str) -> Option<PkceCodeVerifier> {
    let mut s = store().lock().expect("oauth flow store poisoned");
    let flow = s.get_mut(state)?;
    // Replace with a placeholder so the flow stays in the map for
    // status polling.
    let original = std::mem::replace(
        &mut flow.pkce_verifier,
        PkceCodeVerifier::new("consumed".into()),
    );
    Some(original)
}

pub fn mark_complete(state: &str, account_label: String) {
    let mut s = store().lock().expect("oauth flow store poisoned");
    if let Some(flow) = s.get_mut(state) {
        flow.completed = Some(OAuthCompletion {
            completed_at: Utc::now().timestamp(),
            account_label,
        });
    }
}

pub fn mark_error(state: &str, error: String) {
    let mut s = store().lock().expect("oauth flow store poisoned");
    if let Some(flow) = s.get_mut(state) {
        flow.error = Some(error);
    }
}

#[derive(Debug, Clone)]
pub enum FlowStatus {
    /// Flow not started, expired, or already harvested.
    Unknown,
    InProgress,
    Completed {
        account_label: String,
    },
    Failed {
        error: String,
    },
}

pub fn poll_status(state: &str) -> FlowStatus {
    let s = store().lock().expect("oauth flow store poisoned");
    match s.get(state) {
        None => FlowStatus::Unknown,
        Some(flow) => {
            if let Some(err) = &flow.error {
                FlowStatus::Failed { error: err.clone() }
            } else if let Some(c) = &flow.completed {
                FlowStatus::Completed {
                    account_label: c.account_label.clone(),
                }
            } else {
                FlowStatus::InProgress
            }
        }
    }
}

/// Remove a flow from the store after the UI has read its terminal
/// state. Avoids unbounded growth across many consecutive Connect
/// attempts.
pub fn forget(state: &str) {
    let mut s = store().lock().expect("oauth flow store poisoned");
    s.remove(state);
}
