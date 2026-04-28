//! Anthropic API client surface.
//!
//! Used by both the conversation flows (`crate::conversation`) and the
//! analysis pipeline (`crate::analysis`). Three implementations:
//!
//! - [`LiveAnthropicClient`] — real HTTP calls. Honors prompt caching
//!   breakpoints and surfaces token usage for the cost estimator.
//! - [`MockAnthropicClient`] — canned responses, deterministic, used by
//!   unit tests that need to drive a state machine without I/O.
//! - [`FixtureClient`] — replays JSON fixtures captured from prior live
//!   runs. Used by integration tests and the `replay-cycle` binary.

pub mod fixture;
pub mod live;
pub mod mock;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use fixture::FixtureClient;
pub use live::LiveAnthropicClient;
pub use mock::MockAnthropicClient;

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
    /// Optional cache breakpoint position. The static prefix (system + first
    /// N messages) is cached on the Anthropic side; subsequent calls with
    /// the same prefix hit the cache and are billed at a discount.
    /// Index is 0-based and refers to messages array. None = no caching.
    pub cache_breakpoint: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompletionUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    pub usage: CompletionUsage,
    pub model: String,
}

#[async_trait]
pub trait AnthropicClient: Send + Sync {
    /// Issue a completion request and return the assembled text plus
    /// token-usage breakdown.
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<CompletionResponse>;

    /// Identifier used in logs and fixture-recording metadata.
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        let r = Role::Assistant;
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"assistant\"");
    }

    #[test]
    fn message_roundtrips_through_json() {
        let m = Message {
            role: Role::User,
            content: "hi".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m.content, back.content);
    }
}
