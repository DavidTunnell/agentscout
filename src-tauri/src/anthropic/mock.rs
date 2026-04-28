use super::{AnthropicClient, CompletionRequest, CompletionResponse, CompletionUsage};
use anyhow::{anyhow, Result};
use async_trait::async_trait;

/// Returns deterministic canned responses in order. Used by unit tests
/// that drive a state machine and need to assert call counts and content.
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

    /// How many `complete` calls have been made so far.
    pub fn calls(&self) -> usize {
        *self.counter.lock().expect("mock counter mutex poisoned")
    }
}

#[async_trait]
impl AnthropicClient for MockAnthropicClient {
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<CompletionResponse> {
        let mut idx = self.counter.lock().expect("mock counter mutex poisoned");
        let response = self
            .responses
            .get(*idx)
            .cloned()
            .ok_or_else(|| anyhow!("MockAnthropicClient ran out of canned responses"))?;
        *idx += 1;
        // Token counts are best-effort heuristics — tests that care about
        // exact counts should use FixtureClient with recorded usage.
        let input_tokens = (req.messages.iter().map(|m| m.content.len()).sum::<usize>() as u32) / 4;
        let output_tokens = (response.len() as u32) / 4;
        Ok(CompletionResponse {
            text: response,
            usage: CompletionUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
            model: req.model.to_string(),
        })
    }

    fn name(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Message, Role};
    use super::*;

    fn req<'a>(messages: &'a [Message]) -> CompletionRequest<'a> {
        CompletionRequest {
            messages,
            system: None,
            model: "claude-sonnet-4-6",
            max_tokens: 100,
            cache_breakpoint: None,
        }
    }

    #[tokio::test]
    async fn returns_canned_responses_in_order() {
        let mock = MockAnthropicClient::new(vec!["one".into(), "two".into()]);
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
        }];
        assert_eq!(mock.complete(req(&messages)).await.unwrap().text, "one");
        assert_eq!(mock.complete(req(&messages)).await.unwrap().text, "two");
        assert!(mock.complete(req(&messages)).await.is_err());
    }

    #[tokio::test]
    async fn tracks_call_count() {
        let mock = MockAnthropicClient::new(vec!["a".into(), "b".into()]);
        let messages = vec![Message {
            role: Role::User,
            content: "x".into(),
        }];
        assert_eq!(mock.calls(), 0);
        let _ = mock.complete(req(&messages)).await;
        assert_eq!(mock.calls(), 1);
        let _ = mock.complete(req(&messages)).await;
        assert_eq!(mock.calls(), 2);
    }
}
