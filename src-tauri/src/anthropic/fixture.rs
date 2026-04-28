use super::{AnthropicClient, CompletionRequest, CompletionResponse};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Replays recorded API responses keyed by a stable hash of the request
/// content. Used by integration tests and the `replay-cycle` binary so
/// the analysis pipeline can be exercised end-to-end at zero API cost
/// with byte-stable outputs.
///
/// Fixture format: a directory containing one JSON file per recorded
/// response, named `<request-hash>.json`. Each file matches
/// [`FixtureRecord`].
pub struct FixtureClient {
    dir: PathBuf,
    /// Strict mode (default true) panics if a request doesn't have a
    /// recorded fixture. Set false when augmenting an existing fixture
    /// set — missing requests fall through to a deterministic canned
    /// "(unrecorded)" response.
    strict: bool,
    /// Allow tests to introspect what was looked up.
    lookups: Mutex<Vec<FixtureLookup>>,
}

#[derive(Debug, Clone)]
pub struct FixtureLookup {
    pub key: String,
    pub hit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureRecord {
    /// Optional human-readable label so a developer reading the file
    /// knows what call this captured.
    #[serde(default)]
    pub label: Option<String>,
    /// Hash of the request used as the file name. Stored redundantly so
    /// loading a fixture and re-hashing the request can sanity-check
    /// the match.
    pub request_hash: String,
    /// Model the fixture was recorded against.
    pub model: String,
    pub response: CompletionResponse,
}

impl FixtureClient {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            strict: true,
            lookups: Mutex::new(Vec::new()),
        }
    }

    pub fn lenient(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            strict: false,
            lookups: Mutex::new(Vec::new()),
        }
    }

    pub fn lookups(&self) -> Vec<FixtureLookup> {
        self.lookups
            .lock()
            .expect("fixture lookups mutex poisoned")
            .clone()
    }

    fn record_lookup(&self, key: &str, hit: bool) {
        self.lookups
            .lock()
            .expect("fixture lookups mutex poisoned")
            .push(FixtureLookup {
                key: key.to_string(),
                hit,
            });
    }
}

/// Compute the canonical fixture key for a request. Stable across runs
/// for byte-identical inputs.
pub fn request_key(req: &CompletionRequest<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(req.model.as_bytes());
    hasher.update([0]);
    if let Some(s) = req.system {
        hasher.update(s.as_bytes());
    }
    hasher.update([0]);
    for m in req.messages {
        let role_str = match m.role {
            super::Role::User => "user",
            super::Role::Assistant => "assistant",
        };
        hasher.update(role_str.as_bytes());
        hasher.update([0]);
        hasher.update(m.content.as_bytes());
        hasher.update([0]);
    }
    hasher.update([req.max_tokens as u8]);
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

#[async_trait]
impl AnthropicClient for FixtureClient {
    async fn complete(&self, req: CompletionRequest<'_>) -> Result<CompletionResponse> {
        let key = request_key(&req);
        let path = self.dir.join(format!("{key}.json"));
        if !path.exists() {
            self.record_lookup(&key, false);
            if self.strict {
                return Err(anyhow!(
                    "no fixture for request key {} at {} (strict mode); record this fixture or run in lenient mode",
                    key,
                    path.display()
                ));
            }
            return Ok(CompletionResponse {
                text: format!("(unrecorded fixture for key {key})"),
                usage: super::CompletionUsage::default(),
                model: req.model.to_string(),
            });
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading fixture at {}", path.display()))?;
        let record: FixtureRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing fixture at {}", path.display()))?;
        self.record_lookup(&key, true);
        Ok(record.response)
    }

    fn name(&self) -> &str {
        "fixture"
    }
}

/// Helper for tests that need to write a fixture file in the canonical
/// location.
pub fn write_fixture(
    dir: &Path,
    req: &CompletionRequest<'_>,
    response: CompletionResponse,
    label: Option<String>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let key = request_key(req);
    let path = dir.join(format!("{key}.json"));
    let record = FixtureRecord {
        label,
        request_hash: key,
        model: req.model.to_string(),
        response,
    };
    let json = serde_json::to_vec_pretty(&record)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::super::{CompletionUsage, Message, Role};
    use super::*;

    fn sample_request<'a>(messages: &'a [Message]) -> CompletionRequest<'a> {
        CompletionRequest {
            messages,
            system: Some("test system"),
            model: "claude-sonnet-4-6",
            max_tokens: 100,
            cache_breakpoint: None,
        }
    }

    #[test]
    fn request_key_is_stable_for_identical_inputs() {
        let messages = vec![Message {
            role: Role::User,
            content: "hello".into(),
        }];
        let k1 = request_key(&sample_request(&messages));
        let k2 = request_key(&sample_request(&messages));
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32); // 16 bytes hex
    }

    #[test]
    fn request_key_differs_for_different_content() {
        let m1 = vec![Message {
            role: Role::User,
            content: "hello".into(),
        }];
        let m2 = vec![Message {
            role: Role::User,
            content: "hello!".into(),
        }];
        assert_ne!(
            request_key(&sample_request(&m1)),
            request_key(&sample_request(&m2))
        );
    }

    #[tokio::test]
    async fn replays_recorded_fixture() {
        let dir = std::env::temp_dir().join(format!("as-fixture-{}", uuid::Uuid::new_v4()));
        let messages = vec![Message {
            role: Role::User,
            content: "what's 2+2?".into(),
        }];
        let req = sample_request(&messages);

        let response = CompletionResponse {
            text: "four".into(),
            usage: CompletionUsage {
                input_tokens: 8,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
            model: "claude-sonnet-4-6".into(),
        };
        write_fixture(&dir, &req, response.clone(), Some("two-plus-two".into())).unwrap();

        let client = FixtureClient::new(&dir);
        let got = client.complete(req).await.unwrap();
        assert_eq!(got.text, "four");
        assert_eq!(got.usage.input_tokens, 8);

        let lookups = client.lookups();
        assert_eq!(lookups.len(), 1);
        assert!(lookups[0].hit);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn strict_mode_errors_on_missing_fixture() {
        let dir = std::env::temp_dir().join(format!("as-fixture-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let messages = vec![Message {
            role: Role::User,
            content: "uncovered".into(),
        }];
        let client = FixtureClient::new(&dir);
        let result = client.complete(sample_request(&messages)).await;
        assert!(result.is_err());
        assert!(!client.lookups()[0].hit);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn lenient_mode_returns_canned_response_on_miss() {
        let dir = std::env::temp_dir().join(format!("as-fixture-lenient-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let messages = vec![Message {
            role: Role::User,
            content: "uncovered".into(),
        }];
        let client = FixtureClient::lenient(&dir);
        let resp = client.complete(sample_request(&messages)).await.unwrap();
        assert!(resp.text.starts_with("(unrecorded fixture"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
