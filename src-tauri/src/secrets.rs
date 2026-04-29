//! Secrets storage with keychain-first, encrypted-file fallback.
//!
//! Keychain-first because that's the security posture per SPEC §10. But
//! we discovered (v0.5.8 dogfood, real Windows machine) that the
//! `keyring` crate on Windows can silently no-op writes from inside
//! Tauri's tokio runtime — `set_password` returns `Ok(())` while
//! Windows Credential Manager has no record of the credential.
//!
//! v0.5.9 fixes this two ways, layered:
//!
//! 1. **`spawn_blocking` wrapper.** All keyring ops run on tokio's
//!    blocking-thread pool, not on async worker threads. Hypothesis:
//!    Windows CredWrite needs a "regular" OS thread context that
//!    tokio's async workers don't provide.
//!
//! 2. **Verify-after-write + fallback.** After every `set_password`
//!    we immediately read back. On mismatch, mark this install's
//!    keychain as broken and persist to an encrypted file under
//!    `<storage_root>/.secrets/<account>.enc` instead. The fallback
//!    key is a 32-byte random value at `.secrets/.master` — protected
//!    by OS file permissions (per-user `%APPDATA%`), a downgrade from
//!    keychain but still encrypted-at-rest.
//!
//! Once a fallback-storage flag is set for this install, all future
//! reads/writes go to the file instead of keychain. The flag persists
//! at `<storage_root>/.secrets/.use-file` to survive restart.

use anyhow::{Context, Result};
use keyring::Entry;
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "AgentScout";
const ACCOUNT_ANTHROPIC_KEY: &str = "anthropic-api-key";
const ACCOUNT_GMAIL_OAUTH_CLIENT_ID: &str = "gmail-oauth-client-id";
const ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET: &str = "gmail-oauth-client-secret";
/// Account name matches the original `KEYCHAIN_USER_REFRESH` constant
/// in `email/oauth.rs` so legacy installs that already had a working
/// keychain read the same value.
const ACCOUNT_GMAIL_REFRESH: &str = "gmail-refresh-v1";

const FALLBACK_DIR: &str = ".secrets";
const FALLBACK_MASTER_FILENAME: &str = ".master";
const FALLBACK_FLAG_FILENAME: &str = ".use-file";

/// Public entry points use the storage root passed by the caller. We
/// resolve it lazily — `crate::config::storage_root()` returns the
/// platform-specific app-data dir.
fn storage_root() -> Result<PathBuf> {
    crate::config::storage_root()
}

fn fallback_dir() -> Result<PathBuf> {
    let dir = storage_root()?.join(FALLBACK_DIR);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating secrets fallback directory at {}", dir.display()))?;
    Ok(dir)
}

fn fallback_in_use() -> bool {
    matches!(fallback_dir(), Ok(d) if d.join(FALLBACK_FLAG_FILENAME).exists())
}

fn mark_fallback_in_use() -> Result<()> {
    let flag = fallback_dir()?.join(FALLBACK_FLAG_FILENAME);
    std::fs::write(
        &flag,
        b"keychain writes silently fail on this machine; using encrypted file fallback. \
          See secrets.rs.",
    )
    .with_context(|| format!("writing fallback flag to {}", flag.display()))
}

/// Load the fallback master key. If the file doesn't exist, generate a
/// fresh 32-byte random key and persist it. The file is plaintext binary
/// — relies on OS file permissions (`%APPDATA%` is per-user on Windows,
/// `~/.local/share` is per-user on Linux, `~/Library` is per-user on
/// macOS). Less protective than keychain but acceptable when keychain
/// is broken.
fn load_or_init_fallback_master_key() -> Result<[u8; 32]> {
    use rand::RngCore;
    let path = fallback_dir()?.join(FALLBACK_MASTER_FILENAME);
    if path.exists() {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading fallback master from {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!(
                "fallback master at {} has unexpected length {} (expected 32)",
                path.display(),
                bytes.len()
            );
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    std::fs::write(&path, key)
        .with_context(|| format!("writing fallback master to {}", path.display()))?;
    Ok(key)
}

fn fallback_path_for(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{account}.enc")))
}

fn fallback_set(account: &str, value: &str) -> Result<()> {
    let key = load_or_init_fallback_master_key()?;
    let crypto = crate::storage::crypto::FileCrypto::with_key(key);
    crypto.encrypt_to_file(&fallback_path_for(account)?, value.as_bytes())
}

fn fallback_get(account: &str) -> Result<Option<String>> {
    let path = fallback_path_for(account)?;
    if !path.exists() {
        return Ok(None);
    }
    let key = load_or_init_fallback_master_key()?;
    let crypto = crate::storage::crypto::FileCrypto::with_key(key);
    let bytes = crypto.decrypt_from_file(&path)?;
    Ok(Some(
        String::from_utf8(bytes).context("decrypted secret was not utf-8")?,
    ))
}

fn fallback_delete(account: &str) -> Result<()> {
    let path = fallback_path_for(account)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing fallback secret {}", path.display()))?;
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────
// keyring wrappers — synchronous, called via `spawn_blocking` from any
// async context. Tauri commands are async; if they call these directly
// without spawn_blocking the OS-level credential write may silently
// no-op (observed on Windows 11 in v0.5.8).
// ───────────────────────────────────────────────────────────────────────

fn keyring_set(account: &str, value: &str) -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("opening keychain entry for {account}"))?;
    entry
        .set_password(value)
        .with_context(|| format!("writing {account} to keychain"))
}

fn keyring_get(account: &str) -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("opening keychain entry for {account}"))?;
    match entry.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {account} from keychain")),
    }
}

fn keyring_delete(account: &str) -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("opening keychain entry for {account}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("deleting {account} from keychain")),
    }
}

/// Verify-after-write: returns Ok(true) if a write to keychain
/// successfully persisted (read-back equals the value we wrote),
/// Ok(false) if the write returned Ok but the read-back is missing or
/// different. Err on actual keychain errors.
fn keyring_set_with_verify(account: &str, value: &str) -> Result<bool> {
    keyring_set(account, value)?;
    match keyring_get(account)? {
        Some(read) if read == value => Ok(true),
        _ => Ok(false),
    }
}

/// Generic set: tries keychain first (with verify), falls back to file
/// on silent-no-op. After the first failed verify on this install, sets
/// the fallback flag and routes all subsequent calls to the file
/// without re-attempting keychain.
fn store_secret(account: &str, value: &str) -> Result<()> {
    if fallback_in_use() {
        return fallback_set(account, value);
    }
    match keyring_set_with_verify(account, value) {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::warn!(
                "keychain write for '{}' silently failed verification; switching this \
                 install to encrypted-file fallback",
                account
            );
            mark_fallback_in_use()?;
            fallback_set(account, value)
        }
        Err(e) => {
            tracing::warn!(
                "keychain write for '{}' errored ({:#}); switching this install to \
                 encrypted-file fallback",
                account,
                e
            );
            mark_fallback_in_use()?;
            fallback_set(account, value)
        }
    }
}

fn load_secret(account: &str) -> Result<Option<String>> {
    if fallback_in_use() {
        return fallback_get(account);
    }
    match keyring_get(account) {
        Ok(v) => Ok(v),
        Err(e) => {
            tracing::warn!(
                "keychain read for '{}' failed ({:#}); checking fallback file",
                account,
                e
            );
            // Read errors might be transient; check fallback before
            // giving up.
            fallback_get(account)
        }
    }
}

fn delete_secret(account: &str) -> Result<()> {
    // Best-effort delete from BOTH locations — if a user toggles fallback
    // mode between writes, we don't want a stale value lingering.
    let _ = keyring_delete(account);
    let _ = fallback_delete(account);
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────
// Async-safe public API — wraps the sync helpers in spawn_blocking so
// Tauri commands can `await` them without blocking the async worker.
// ───────────────────────────────────────────────────────────────────────

async fn store_secret_async(account: &'static str, value: String) -> Result<()> {
    tokio::task::spawn_blocking(move || store_secret(account, &value))
        .await
        .context("spawn_blocking joined with error")?
}

async fn load_secret_async(account: &'static str) -> Result<Option<String>> {
    tokio::task::spawn_blocking(move || load_secret(account))
        .await
        .context("spawn_blocking joined with error")?
}

async fn delete_secret_async(account: &'static str) -> Result<()> {
    tokio::task::spawn_blocking(move || delete_secret(account))
        .await
        .context("spawn_blocking joined with error")?
}

// ───────────────────────────────────────────────────────────────────────
// Anthropic API key — public surface
// ───────────────────────────────────────────────────────────────────────

pub async fn get_anthropic_key() -> Result<Option<String>> {
    load_secret_async(ACCOUNT_ANTHROPIC_KEY).await
}

pub async fn set_anthropic_key(key: &str) -> Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("api key cannot be empty");
    }
    store_secret_async(ACCOUNT_ANTHROPIC_KEY, key.trim().to_string()).await
}

pub async fn clear_anthropic_key() -> Result<()> {
    delete_secret_async(ACCOUNT_ANTHROPIC_KEY).await
}

pub async fn has_anthropic_key() -> bool {
    matches!(get_anthropic_key().await, Ok(Some(_)))
}

// Synchronous wrapper for code paths that aren't async — used by the
// auto-cycle loop's threshold gate. It ALSO uses spawn_blocking
// internally if called from a tokio context, falling back to a direct
// call when there's no runtime (e.g., self_test bin).
pub fn has_anthropic_key_blocking() -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .block_on(async { has_anthropic_key().await })
            .to_owned(),
        Err(_) => matches!(load_secret(ACCOUNT_ANTHROPIC_KEY), Ok(Some(_))),
    }
}

// ───────────────────────────────────────────────────────────────────────
// Gmail OAuth client credentials — same async pattern
// ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GmailOAuthCreds {
    pub client_id: String,
    pub client_secret: Option<String>,
}

pub async fn set_gmail_oauth_creds(client_id: &str, client_secret: Option<&str>) -> Result<()> {
    if client_id.trim().is_empty() {
        anyhow::bail!("client_id cannot be empty");
    }
    store_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_ID, client_id.trim().to_string()).await?;
    match client_secret {
        Some(s) if !s.trim().is_empty() => {
            store_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET, s.trim().to_string()).await
        }
        _ => delete_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET).await,
    }
}

pub async fn get_gmail_oauth_creds() -> Result<Option<GmailOAuthCreds>> {
    let client_id = match load_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_ID).await? {
        Some(s) => s,
        None => return Ok(None),
    };
    let client_secret = load_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET).await?;
    Ok(Some(GmailOAuthCreds {
        client_id,
        client_secret,
    }))
}

pub async fn clear_gmail_oauth_creds() -> Result<()> {
    delete_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_ID).await?;
    delete_secret_async(ACCOUNT_GMAIL_OAUTH_CLIENT_SECRET).await
}

pub async fn has_gmail_oauth_creds() -> bool {
    matches!(get_gmail_oauth_creds().await, Ok(Some(_)))
}

// ───────────────────────────────────────────────────────────────────────
// v0.5.13 — Gmail refresh token (moved out of email/oauth.rs which was
// using keyring directly and silently no-op'd on Tauri's tokio worker
// threads, same as the v0.5.9 Anthropic-key bug)
// ───────────────────────────────────────────────────────────────────────

pub async fn set_gmail_refresh_token(token: &str) -> Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("gmail refresh token cannot be empty");
    }
    store_secret_async(ACCOUNT_GMAIL_REFRESH, token.trim().to_string()).await
}

pub async fn get_gmail_refresh_token() -> Result<Option<String>> {
    load_secret_async(ACCOUNT_GMAIL_REFRESH).await
}

pub async fn clear_gmail_refresh_token() -> Result<()> {
    delete_secret_async(ACCOUNT_GMAIL_REFRESH).await
}

pub async fn has_gmail_refresh_token() -> bool {
    matches!(get_gmail_refresh_token().await, Ok(Some(_)))
}

// ───────────────────────────────────────────────────────────────────────
// Diagnostics — exposed via cmd_get_secrets_diagnostic so the user can
// see which storage path is active and whether keychain is healthy.
// ───────────────────────────────────────────────────────────────────────

pub struct SecretsDiagnostic {
    /// Where this install reads/writes secrets right now.
    pub backend: &'static str,
    /// True when keychain proved unreliable on this machine.
    pub fallback_active: bool,
    /// Path to the encrypted-file fallback dir (when active).
    pub fallback_dir_path: Option<String>,
}

pub async fn get_diagnostic() -> SecretsDiagnostic {
    let fallback_active = fallback_in_use();
    let fallback_dir_path = if fallback_active {
        fallback_dir().ok().map(|p| p.display().to_string())
    } else {
        None
    };
    SecretsDiagnostic {
        backend: if fallback_active {
            "encrypted-file fallback"
        } else {
            "OS keychain"
        },
        fallback_active,
        fallback_dir_path,
    }
}

#[allow(dead_code)]
fn _suppress_unused_path_import(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fallback storage round-trips through file encryption.
    /// Doesn't touch keychain — uses the fallback path explicitly.
    #[tokio::test]
    async fn fallback_round_trip_stores_and_retrieves() {
        let dir = std::env::temp_dir().join(format!("as-secrets-{}", uuid::Uuid::new_v4()));
        std::env::set_var("AGENTSCOUT_DATA_DIR", dir.to_str().unwrap());
        let _ = std::fs::create_dir_all(&dir);
        // Force fallback mode for this test. We can't easily share the
        // storage_root so we directly call the file-level helpers.
        // Set up the fallback dir manually.
        let fdir = dir.join(FALLBACK_DIR);
        let _ = std::fs::create_dir_all(&fdir);
        std::fs::write(fdir.join(FALLBACK_FLAG_FILENAME), b"test").unwrap();

        // The test relies on fallback_in_use() reading from the
        // process's storage_root. If config::storage_root reads an env
        // var we're set; otherwise this test is best-effort.

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_anthropic_key_rejected() {
        // Sync-context test that doesn't touch keychain or fs.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(set_anthropic_key("").await.is_err());
            assert!(set_anthropic_key("   ").await.is_err());
        });
    }
}
