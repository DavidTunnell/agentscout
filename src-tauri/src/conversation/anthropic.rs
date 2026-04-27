use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct CompletionRequest<'a> {
    pub messages: &'a [Message],
    pub system: Option<&'a str>,
    pub model: &'a str,
    pub max_tokens: u32,
}

#[async_trait]
pub trait AnthropicClient: Send + Sync {
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<String>;
    fn name(&self) -> &str;
}

/// Live HTTP client. Wired in week 3 with prompt caching breakpoints,
/// streaming, and recorded-fixture support. For now it issues a basic
/// non-streaming request — enough for the conversation-shell scaffolding.
pub struct LiveAnthropicClient {
    api_key: String,
    http: reqwest::Client,
    base_url: String,
}

impl LiveAnthropicClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
            base_url,
        }
    }
}

#[async_trait]
impl AnthropicClient for LiveAnthropicClient {
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<String> {
        let body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": req.messages,
        });

        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("posting to anthropic /v1/messages")?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "anthropic returned status {}: {}",
                status,
                body_text
            ));
        }

        let parsed: AnthropicResponse =
            serde_json::from_str(&body_text).context("parsing anthropic response")?;
        let combined = parsed
            .content
            .into_iter()
            .filter_map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(combined)
    }

    fn name(&self) -> &str {
        "live"
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: Option<String>,
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    kind: Option<String>,
    text: Option<String>,
}

/// Returns deterministic canned responses. Used for tests and as the
/// scaffold backend before week 3's fixture harness lands.
pub struct MockAnthropicClient {
    responses: Vec<String>,
    counter: std::sync::Mutex<usize>,
}

impl MockAnthropicClient {
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses,
            counter: std::sync::Mutex::new(0),
        }
    }
}

#[async_trait]
impl AnthropicClient for MockAnthropicClient {
    async fn complete(&self, _req: CompletionRequest<'_>) -> Result<String> {
        let mut idx = self.counter.lock().expect("mock counter mutex poisoned");
        let response = self
            .responses
            .get(*idx)
            .cloned()
            .ok_or_else(|| anyhow!("MockAnthropicClient ran out of canned responses"))?;
        *idx += 1;
        Ok(response)
    }

    fn name(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_returns_canned_responses_in_order() {
        let mock = MockAnthropicClient::new(vec!["one".into(), "two".into()]);
        let req = CompletionRequest {
            messages: &[],
            system: None,
            model: "claude-sonnet-4-6",
            max_tokens: 100,
        };
        assert_eq!(mock.complete(req.clone()).await.unwrap(), "one");
        assert_eq!(mock.complete(req.clone()).await.unwrap(), "two");
        assert!(mock.complete(req).await.is_err());
    }

    #[test]
    fn role_serializes_lowercase() {
        let r = Role::Assistant;
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"assistant\"");
    }
}
