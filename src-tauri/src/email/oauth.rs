//! Gmail OAuth 2.0 — PKCE flow with a localhost loopback redirect, then
//! refresh-token persistence in the OS keychain. Scope is exactly
//! `gmail.send` per pinned plan decision (SPEC.md §12.1 #4).

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use keyring::Entry;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

const KEYCHAIN_SERVICE: &str = "AgentScout";
const KEYCHAIN_USER_REFRESH: &str = "gmail-refresh-v1";
const GMAIL_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GMAIL_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_SEND_SCOPE: &str = "https://www.googleapis.com/auth/gmail.send";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// e.g. "http://127.0.0.1:51234/oauth/callback". Must match the
    /// authorized redirect URI in the GCP project.
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl AccessToken {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at - Duration::seconds(60)
    }
}

/// Authorization-URL pair — the user opens `auth_url` in their browser
/// and consents; the URL the browser is redirected to lands at our
/// loopback server, carrying `code` + `state` query params.
pub struct AuthInit {
    pub auth_url: String,
    pub csrf_state: String,
    pub pkce_verifier: PkceCodeVerifier,
}

pub fn begin_auth(config: &OAuthConfig) -> Result<AuthInit> {
    let client = build_client(config)?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_state) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(GMAIL_SEND_SCOPE.to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();
    Ok(AuthInit {
        auth_url: auth_url.to_string(),
        csrf_state: csrf_state.into_secret(),
        pkce_verifier,
    })
}

/// Exchange a callback `code` (with the verifier the client kept) for
/// access + refresh tokens. Persists the refresh token to the keychain.
pub async fn complete_auth(
    config: &OAuthConfig,
    code: String,
    pkce_verifier: PkceCodeVerifier,
) -> Result<AccessToken> {
    let client = build_client(config)?;
    let http = http_client();
    let token_resp = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await
        .context("exchanging authorization code for tokens")?;

    if let Some(refresh) = token_resp.refresh_token() {
        store_refresh_token(refresh.secret())?;
    } else {
        bail!(
            "Gmail did not return a refresh token; ensure access_type=offline and prompt=consent on the consent URL"
        );
    }

    let expires_at = compute_expiry(token_resp.expires_in());
    Ok(AccessToken {
        token: token_resp.access_token().secret().clone(),
        expires_at,
    })
}

/// Refresh the access token using the keychain-stored refresh token.
/// Returns the fresh access token; updates the keychain refresh token
/// if Google rotated it.
pub async fn refresh_access_token(config: &OAuthConfig) -> Result<AccessToken> {
    let refresh = load_refresh_token()?
        .ok_or_else(|| anyhow!("no refresh token stored; user has not authorized Gmail yet"))?;
    let client = build_client(config)?;
    let http = http_client();
    let token_resp = client
        .exchange_refresh_token(&RefreshToken::new(refresh))
        .request_async(&http)
        .await
        .context("refreshing access token")?;

    if let Some(new_refresh) = token_resp.refresh_token() {
        store_refresh_token(new_refresh.secret())?;
    }

    let expires_at = compute_expiry(token_resp.expires_in());
    Ok(AccessToken {
        token: token_resp.access_token().secret().clone(),
        expires_at,
    })
}

pub fn has_stored_refresh_token() -> Result<bool> {
    Ok(load_refresh_token()?.is_some())
}

pub fn revoke_stored_refresh_token() -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_REFRESH)
        .context("creating keychain entry for refresh token")?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!(e)).context("deleting stored refresh token"),
    }
}

fn store_refresh_token(value: &str) -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_REFRESH)
        .context("creating keychain entry for refresh token")?;
    entry
        .set_password(value)
        .context("writing refresh token to keychain")
}

fn load_refresh_token() -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER_REFRESH)
        .context("creating keychain entry for refresh token")?;
    match entry.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!(e)).context("reading refresh token from keychain"),
    }
}

fn build_client(
    config: &OAuthConfig,
) -> Result<
    BasicClient<
        oauth2::EndpointSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointSet,
    >,
> {
    let auth_url = AuthUrl::new(GMAIL_AUTH_URL.to_string()).context("auth url")?;
    let token_url = TokenUrl::new(GMAIL_TOKEN_URL.to_string()).context("token url")?;
    let redirect = RedirectUrl::new(config.redirect_uri.clone()).context("redirect url")?;

    let mut client = BasicClient::new(ClientId::new(config.client_id.clone()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect);
    if let Some(secret) = &config.client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.clone()));
    }
    Ok(client)
}

fn http_client() -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("building reqwest client should not fail")
}

fn compute_expiry(maybe_seconds: Option<std::time::Duration>) -> DateTime<Utc> {
    let secs = maybe_seconds.map(|d| d.as_secs() as i64).unwrap_or(3600);
    Utc::now() + Duration::seconds(secs)
}

#[allow(dead_code)]
fn _ensure_systemtime_referenced(_: SystemTime) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuthConfig {
        OAuthConfig {
            client_id: "test-client".into(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:51234/oauth/callback".into(),
        }
    }

    #[test]
    fn begin_auth_emits_url_with_required_query_params() {
        let init = begin_auth(&config()).unwrap();
        assert!(init.auth_url.starts_with("https://accounts.google.com/"));
        assert!(init.auth_url.contains("client_id=test-client"));
        assert!(init.auth_url.contains("code_challenge="));
        assert!(init.auth_url.contains("code_challenge_method=S256"));
        assert!(init
            .auth_url
            .contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.send"));
        assert!(!init.csrf_state.is_empty());
    }

    #[test]
    fn pkce_verifiers_are_unique_per_call() {
        let a = begin_auth(&config()).unwrap();
        let b = begin_auth(&config()).unwrap();
        assert_ne!(a.csrf_state, b.csrf_state);
        assert_ne!(a.pkce_verifier.secret(), b.pkce_verifier.secret());
    }

    #[test]
    fn access_token_is_expired_within_grace_window() {
        let about_to_expire = AccessToken {
            token: "x".into(),
            expires_at: Utc::now() + Duration::seconds(30),
        };
        assert!(about_to_expire.is_expired());
        let healthy = AccessToken {
            token: "x".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        assert!(!healthy.is_expired());
    }

    #[test]
    fn compute_expiry_falls_back_to_one_hour() {
        let when = compute_expiry(None);
        let delta = (when - Utc::now()).num_seconds();
        assert!((3590..=3610).contains(&delta));
    }
}
