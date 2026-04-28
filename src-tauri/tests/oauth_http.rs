//! OAuth token-exchange wire test. Stands up a wiremock that pretends
//! to be Google's `oauth2.googleapis.com/token` endpoint and verifies
//! the client posts the right form parameters and parses the response.
//!
//! The auth-URL construction is unit-tested in `email::oauth::tests`
//! already; this exercises the network round-trip we couldn't reach
//! from the unit tests.

use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, PkceCodeChallenge, RedirectUrl,
    RefreshToken, TokenResponse, TokenUrl,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn authorization_code_exchange_posts_pkce_and_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=test-code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("client_id=test-client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.test-access",
            "refresh_token": "1//0g-test-refresh",
            "expires_in": 3599,
            "scope": "https://www.googleapis.com/auth/gmail.send",
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let auth_url =
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string()).unwrap();
    let token_url = TokenUrl::new(format!("{}/token", server.uri())).unwrap();
    let redirect = RedirectUrl::new("http://127.0.0.1:51234/oauth/callback".to_string()).unwrap();

    let client = BasicClient::new(ClientId::new("test-client".into()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect);
    let (_pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let http = reqwest::Client::new();
    let resp = client
        .exchange_code(AuthorizationCode::new("test-code".into()))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await
        .unwrap();

    assert_eq!(resp.access_token().secret(), "ya29.test-access");
    assert!(resp.refresh_token().is_some());
    assert_eq!(resp.refresh_token().unwrap().secret(), "1//0g-test-refresh");
    let expires = resp.expires_in().unwrap();
    assert_eq!(expires.as_secs(), 3599);
}

#[tokio::test]
async fn refresh_token_exchange_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=stored-refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.refreshed",
            "expires_in": 3600,
            "scope": "https://www.googleapis.com/auth/gmail.send",
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let auth_url =
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string()).unwrap();
    let token_url = TokenUrl::new(format!("{}/token", server.uri())).unwrap();
    let redirect = RedirectUrl::new("http://127.0.0.1:51234/oauth/callback".to_string()).unwrap();
    let client = BasicClient::new(ClientId::new("test-client".into()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect);

    let http = reqwest::Client::new();
    let resp = client
        .exchange_refresh_token(&RefreshToken::new("stored-refresh-token".into()))
        .request_async(&http)
        .await
        .unwrap();

    assert_eq!(resp.access_token().secret(), "ya29.refreshed");
}

#[tokio::test]
async fn token_endpoint_4xx_surfaces_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "code expired"
        })))
        .mount(&server)
        .await;

    let auth_url =
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string()).unwrap();
    let token_url = TokenUrl::new(format!("{}/token", server.uri())).unwrap();
    let redirect = RedirectUrl::new("http://127.0.0.1:51234/oauth/callback".to_string()).unwrap();
    let client = BasicClient::new(ClientId::new("test-client".into()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect);

    let (_, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let http = reqwest::Client::new();
    let result = client
        .exchange_code(AuthorizationCode::new("expired".into()))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await;
    assert!(result.is_err());
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(err_str.contains("invalid_grant") || err_str.contains("400"));
}
