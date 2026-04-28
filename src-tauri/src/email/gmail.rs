//! Gmail send via the v1 API. Uses only the `gmail.send` scope per the
//! pinned plan decision (SPEC.md §12.1 #4).

use crate::email::template::RenderedEmail;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;

const GMAIL_SEND_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(
        &self,
        access_token: &str,
        from: &str,
        to: &str,
        rendered: &RenderedEmail,
    ) -> Result<String>;
    fn name(&self) -> &str;
}

pub struct GmailSender {
    http: reqwest::Client,
    base_url: String,
}

impl GmailSender {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: GMAIL_SEND_URL.to_string(),
        }
    }

    pub fn with_base_url(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
        }
    }
}

impl Default for GmailSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmailSender for GmailSender {
    async fn send(
        &self,
        access_token: &str,
        from: &str,
        to: &str,
        rendered: &RenderedEmail,
    ) -> Result<String> {
        let raw_message = build_raw_rfc2822(from, to, rendered)?;
        let raw_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_message);

        let body = serde_json::json!({ "raw": raw_b64 });
        let resp = self
            .http
            .post(&self.base_url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await
            .context("posting to gmail send endpoint")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("gmail returned {}: {}", status, text));
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&text).context("parsing gmail response")?;
        let id = parsed
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "(no id returned)".to_string());
        Ok(id)
    }

    fn name(&self) -> &str {
        "gmail"
    }
}

/// Construct the multipart/alternative RFC 2822 message that Gmail's
/// `users.messages.send` expects. Gmail accepts the message base64url
/// encoded inside `{ "raw": "..." }`.
pub fn build_raw_rfc2822(from: &str, to: &str, rendered: &RenderedEmail) -> Result<String> {
    if from.contains('\n') || to.contains('\n') || rendered.subject.contains('\n') {
        return Err(anyhow!(
            "header smuggling attempt: from/to/subject contain newlines"
        ));
    }
    let boundary = format!("----=_AS_{}", uuid::Uuid::new_v4().simple());

    let mut s = String::new();
    s.push_str(&format!("From: {}\r\n", from));
    s.push_str(&format!("To: {}\r\n", to));
    s.push_str(&format!(
        "Subject: {}\r\n",
        encode_subject(&rendered.subject)
    ));
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{}\"\r\n",
        boundary
    ));
    s.push_str("\r\n");

    s.push_str(&format!("--{}\r\n", boundary));
    s.push_str("Content-Type: text/plain; charset=\"UTF-8\"\r\n");
    s.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    s.push_str(&rendered.plain_body);
    s.push_str("\r\n");

    s.push_str(&format!("--{}\r\n", boundary));
    s.push_str("Content-Type: text/html; charset=\"UTF-8\"\r\n");
    s.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    s.push_str(&rendered.html_body);
    s.push_str("\r\n");

    s.push_str(&format!("--{}--\r\n", boundary));
    Ok(s)
}

/// Encode subject lines with non-ASCII chars per RFC 2047. Gmail accepts
/// raw UTF-8 on input but the encoded-word form is more universally
/// portable. Falls through to the raw subject for ASCII-only content.
fn encode_subject(subject: &str) -> String {
    if subject.is_ascii() {
        return subject.to_string();
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(subject);
    format!("=?UTF-8?B?{}?=", b64)
}

/// In-memory mock used by tests and the smoke binary. Captures the last
/// message rather than hitting the network.
pub struct MockEmailSender {
    pub last: std::sync::Mutex<Option<MockSent>>,
}

#[derive(Debug, Clone)]
pub struct MockSent {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub html_body: String,
    pub plain_body: String,
}

impl MockEmailSender {
    pub fn new() -> Self {
        Self {
            last: std::sync::Mutex::new(None),
        }
    }
}

impl Default for MockEmailSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmailSender for MockEmailSender {
    async fn send(
        &self,
        _access_token: &str,
        from: &str,
        to: &str,
        rendered: &RenderedEmail,
    ) -> Result<String> {
        *self.last.lock().expect("mock sender mutex poisoned") = Some(MockSent {
            from: from.to_string(),
            to: to.to_string(),
            subject: rendered.subject.clone(),
            html_body: rendered.html_body.clone(),
            plain_body: rendered.plain_body.clone(),
        });
        Ok(format!("mock-{}", uuid::Uuid::new_v4()))
    }

    fn name(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered() -> RenderedEmail {
        RenderedEmail {
            subject: "Test subject".to_string(),
            html_body: "<p>HTML body</p>".to_string(),
            plain_body: "Plain body".to_string(),
        }
    }

    #[test]
    fn build_raw_includes_required_headers() {
        let raw = build_raw_rfc2822("a@x.com", "b@y.com", &rendered()).unwrap();
        assert!(raw.contains("From: a@x.com"));
        assert!(raw.contains("To: b@y.com"));
        assert!(raw.contains("Subject: Test subject"));
        assert!(raw.contains("MIME-Version: 1.0"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/plain"));
        assert!(raw.contains("text/html"));
        assert!(raw.contains("Plain body"));
        assert!(raw.contains("<p>HTML body</p>"));
    }

    #[test]
    fn build_raw_rejects_header_smuggling() {
        let bad = RenderedEmail {
            subject: "Subject\r\nBcc: evil@example.com".to_string(),
            html_body: "<p>x</p>".to_string(),
            plain_body: "x".to_string(),
        };
        assert!(build_raw_rfc2822("a@x.com", "b@y.com", &bad).is_err());

        let result = build_raw_rfc2822("a@x.com\nBcc: evil@x.com", "b@y.com", &rendered());
        assert!(result.is_err());
    }

    #[test]
    fn encode_subject_passes_ascii_through() {
        assert_eq!(encode_subject("Hello"), "Hello");
    }

    #[test]
    fn encode_subject_uses_rfc2047_for_unicode() {
        let encoded = encode_subject("café — résumé");
        assert!(encoded.starts_with("=?UTF-8?B?"));
        assert!(encoded.ends_with("?="));
    }

    #[tokio::test]
    async fn mock_sender_captures_last_message() {
        let sender = MockEmailSender::new();
        let id = sender
            .send("token", "a@x.com", "b@y.com", &rendered())
            .await
            .unwrap();
        assert!(id.starts_with("mock-"));
        let last = sender.last.lock().unwrap().clone().unwrap();
        assert_eq!(last.subject, "Test subject");
        assert_eq!(last.to, "b@y.com");
    }
}
