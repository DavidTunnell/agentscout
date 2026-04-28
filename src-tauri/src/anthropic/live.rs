use super::{AnthropicClient, CompletionRequest, CompletionResponse, CompletionUsage};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta header required to opt into prompt caching responses.
const PROMPT_CACHING_BETA: &str = "prompt-caching-2024-07-31";

/// Live HTTP client against the Anthropic API. Sets the cache_control
/// breakpoint when [`CompletionRequest::cache_breakpoint`] is supplied so
/// repeated synthesis calls pay reduced input-token rates on the static
/// prefix (user-profile + tier-definitions per SPEC.md §7.2).
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
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<CompletionResponse> {
        let messages = build_messages(req.messages, req.cache_breakpoint);

        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if let Some(system) = req.system {
            body["system"] = serde_json::Value::String(system.to_string());
        }

        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", PROMPT_CACHING_BETA)
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
        let text = parsed
            .content
            .into_iter()
            .filter_map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");
        let usage = parsed.usage.unwrap_or_default();
        Ok(CompletionResponse {
            text,
            usage: CompletionUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
                cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            },
            model: req.model.to_string(),
        })
    }

    fn name(&self) -> &str {
        "live"
    }
}

/// Build the messages array, attaching `cache_control: ephemeral` to the
/// content of message at index `cache_breakpoint` so everything up to and
/// including that message is cached.
fn build_messages(
    messages: &[super::Message],
    cache_breakpoint: Option<usize>,
) -> Vec<serde_json::Value> {
    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            if Some(idx) == cache_breakpoint {
                serde_json::json!({
                    "role": m.role,
                    "content": [{
                        "type": "text",
                        "text": m.content,
                        "cache_control": { "type": "ephemeral" }
                    }]
                })
            } else {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            }
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: Option<String>,
    content: Vec<AnthropicContent>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    cache_read_input_tokens: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::super::{Message, Role};
    use super::*;

    #[test]
    fn build_messages_without_cache_breakpoint_uses_string_content() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
        }];
        let out = build_messages(&msgs, None);
        assert_eq!(out[0]["content"], serde_json::json!("hi"));
    }

    #[test]
    fn build_messages_with_cache_breakpoint_attaches_cache_control() {
        let msgs = vec![
            Message {
                role: Role::User,
                content: "static prefix".into(),
            },
            Message {
                role: Role::User,
                content: "dynamic suffix".into(),
            },
        ];
        let out = build_messages(&msgs, Some(0));
        let first_content = &out[0]["content"];
        assert_eq!(first_content[0]["text"], "static prefix");
        assert_eq!(first_content[0]["cache_control"]["type"], "ephemeral");
        // Second message stays as plain string
        assert_eq!(out[1]["content"], serde_json::json!("dynamic suffix"));
    }
}
