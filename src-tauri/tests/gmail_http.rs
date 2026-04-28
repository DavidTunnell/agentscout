//! Wire-level HTTP tests for `GmailSender` against a wiremock server.
//! Validates bearer-auth header, base64url-encoded raw payload shape,
//! and error handling.

use agentscout::email::gmail::{build_raw_rfc2822, GmailSender};
use agentscout::email::template::RenderedEmail;
use agentscout::email::EmailSender;
use base64::Engine;
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn rendered() -> RenderedEmail {
    RenderedEmail {
        subject: "Test".into(),
        html_body: "<p>html</p>".into(),
        plain_body: "plain".into(),
    }
}

#[tokio::test]
async fn happy_path_posts_raw_field_and_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("Authorization", "Bearer test-access-token"))
        .and(header_exists("Content-Type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "msg-id-123" })))
        .mount(&server)
        .await;

    let sender = GmailSender::with_base_url(server.uri());
    let id = sender
        .send("test-access-token", "from@x.com", "to@y.com", &rendered())
        .await
        .unwrap();
    assert_eq!(id, "msg-id-123");

    // Inspect the body — `raw` must be base64url-encoded RFC 2822.
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let raw_b64 = body["raw"].as_str().expect("raw field is string");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw_b64)
        .expect("raw is base64url");
    let raw_str = String::from_utf8(raw).unwrap();
    assert!(raw_str.contains("From: from@x.com"));
    assert!(raw_str.contains("To: to@y.com"));
    assert!(raw_str.contains("Subject: Test"));
    assert!(raw_str.contains("multipart/alternative"));
    assert!(raw_str.contains("plain"));
    assert!(raw_str.contains("<p>html</p>"));
}

#[tokio::test]
async fn http_4xx_surfaces_status_and_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": 403, "message": "insufficient scope" }
        })))
        .mount(&server)
        .await;

    let sender = GmailSender::with_base_url(server.uri());
    let err = sender
        .send("token", "from@x.com", "to@y.com", &rendered())
        .await
        .unwrap_err();
    let s = format!("{:#}", err);
    assert!(s.contains("403"));
    assert!(s.contains("insufficient scope"));
}

#[tokio::test]
async fn missing_id_field_falls_back_to_placeholder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let sender = GmailSender::with_base_url(server.uri());
    let id = sender.send("t", "f@x", "t@y", &rendered()).await.unwrap();
    assert_eq!(id, "(no id returned)");
}

#[tokio::test]
async fn header_smuggling_attempt_is_blocked_before_send() {
    // Newline injection in the subject would let an attacker add headers
    // (e.g. Bcc:) below the legitimate Subject line. build_raw_rfc2822
    // rejects this before we ever post.
    let bad = RenderedEmail {
        subject: "OK\r\nBcc: evil@example.com".into(),
        html_body: "x".into(),
        plain_body: "x".into(),
    };
    assert!(build_raw_rfc2822("from@x", "to@y", &bad).is_err());

    // And via the actual sender path:
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "x"})))
        .mount(&server)
        .await;
    let sender = GmailSender::with_base_url(server.uri());
    let result = sender.send("t", "from@x", "to@y", &bad).await;
    assert!(result.is_err());
    // Server should not have been hit
    assert!(server.received_requests().await.unwrap().is_empty());
}
