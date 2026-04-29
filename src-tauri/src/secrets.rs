//! Single keychain wrapper for all AgentScout user-supplied secrets.
//!
//! Consolidates direct `keyring::Entry::new(...)` calls so the service
//! and account-name strings live in one place. Today: Anthropic API key
//! and Gmail refresh token. Future: any other BYO credentials.
//!
//! Keychain layout:
//! - service `"AgentScout"`, account `"anthropic-api-key"` — Anthropic key
//! - service `"AgentScout"`, account `"gmail-refresh-v1"`  — Gmail refresh token
//!
//! All functions return `Result` so callers can distinguish "not set"
//! (Ok(None)) from "keychain error" (Err). Pattern matches Tauri 2's
//! `Result<T, String>` return shape used elsewhere in the lib.

use anyhow::{Context, Result};
use keyring::Entry;

const KEYCHAIN_SERVICE: &str = "AgentScout";
const ACCOUNT_ANTHROPIC_KEY: &str = "anthropic-api-key";
const ACCOUNT_GMAIL_OAUTH_CLIENT_ID: &str = "gmail-oauth-client-id";
const ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET: &str = "gmail-oauth-client-secret";

/// Returns the stored Anthropic API key, or `Ok(None)` if no key is set.
/// Errors only on actual keychain failures (locked keyring, permissions).
pub fn get_anthropic_key() -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_ANTHROPIC_KEY)
        .context("opening keychain entry for anthropic api key")?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("reading anthropic api key from keychain"),
    }
}

/// Stores the Anthropic API key in the OS keychain. Overwrites any prior
/// value. Caller is responsible for shape validation (we don't enforce
/// `sk-ant-` here so future Anthropic key formats don't break us).
pub fn set_anthropic_key(key: &str) -> Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("api key cannot be empty");
    }
    let entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_ANTHROPIC_KEY)
        .context("opening keychain entry for anthropic api key")?;
    entry
        .set_password(key.trim())
        .context("writing anthropic api key to keychain")
}

/// Removes the Anthropic API key from the OS keychain. Idempotent — a
/// "not set" state is treated as success so users can call Clear without
/// worrying about whether one is set.
pub fn clear_anthropic_key() -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_ANTHROPIC_KEY)
        .context("opening keychain entry for anthropic api key")?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("deleting anthropic api key from keychain"),
    }
}

/// Best-effort presence check. Used by status/health endpoints that just
/// want a yes/no without surfacing a particular error.
pub fn has_anthropic_key() -> bool {
    matches!(get_anthropic_key(), Ok(Some(_)))
}

// ───────────────────────────────────────────────────────────────────────
// v0.5.7 — Gmail OAuth client credentials
// ───────────────────────────────────────────────────────────────────────
//
// The user provides their own GCP OAuth client (BYO per privacy posture
// SPEC §10). client_id is technically a public identifier but lives in
// keychain alongside client_secret for consistency. client_secret is
// optional for "desktop app" type clients in OAuth 2 PKCE.
//
// Refresh token storage (gmail-refresh-v1) lives in email/oauth.rs for
// historical reasons and stays there — single source of truth for the
// OAuth code path.

#[derive(Debug, Clone)]
pub struct GmailOAuthCreds {
    pub client_id: String,
    pub client_secret: Option<String>,
}

pub fn set_gmail_oauth_creds(client_id: &str, client_secret: Option<&str>) -> Result<()> {
    if client_id.trim().is_empty() {
        anyhow::bail!("client_id cannot be empty");
    }
    let id_entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_GMAIL_OAUTH_CLIENT_ID)
        .context("opening keychain entry for gmail oauth client_id")?;
    id_entry
        .set_password(client_id.trim())
        .context("writing gmail oauth client_id")?;

    let sec_entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET)
        .context("opening keychain entry for gmail oauth client_secret")?;
    match client_secret {
        Some(s) if !s.trim().is_empty() => sec_entry
            .set_password(s.trim())
            .context("writing gmail oauth client_secret")?,
        _ => {
            // Allow clearing the secret in update flows — desktop-app
            // OAuth clients sometimes don't have a secret.
            let _ = sec_entry.delete_credential();
        }
    }
    Ok(())
}

pub fn get_gmail_oauth_creds() -> Result<Option<GmailOAuthCreds>> {
    let id_entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_GMAIL_OAUTH_CLIENT_ID)
        .context("opening keychain entry for gmail oauth client_id")?;
    let client_id = match id_entry.get_password() {
        Ok(s) => s,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => return Err(e).context("reading gmail oauth client_id"),
    };
    let sec_entry = Entry::new(KEYCHAIN_SERVICE, ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET)
        .context("opening keychain entry for gmail oauth client_secret")?;
    let client_secret = match sec_entry.get_password() {
        Ok(s) => Some(s),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => return Err(e).context("reading gmail oauth client_secret"),
    };
    Ok(Some(GmailOAuthCreds {
        client_id,
        client_secret,
    }))
}

pub fn clear_gmail_oauth_creds() -> Result<()> {
    for account in [
        ACCOUNT_GMAIL_OAUTH_CLIENT_ID,
        ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET,
    ] {
        let entry = Entry::new(KEYCHAIN_SERVICE, account)
            .context("opening keychain entry for gmail oauth creds clear")?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(e).context("clearing gmail oauth creds"),
        }
    }
    Ok(())
}

pub fn has_gmail_oauth_creds() -> bool {
    matches!(get_gmail_oauth_creds(), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty-key validation can be tested without touching the real
    /// keychain. The full set/get/clear round-trip lives in
    /// `bin/self_test.rs` so it doesn't risk clobbering a developer's
    /// real production key on the same machine the unit tests run on.
    #[test]
    fn empty_key_rejected() {
        assert!(set_anthropic_key("").is_err());
        assert!(set_anthropic_key("   ").is_err());
    }
}
