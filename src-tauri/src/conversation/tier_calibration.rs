use super::{
    anthropic::{AnthropicClient, CompletionRequest, Message, Role},
    templates::find_template,
    Conversation, ConversationKind,
};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

const SYSTEM_PROMPT: &str = r#"You are AgentScout's tier calibration assistant. Your job is to help
the user finalize `tier-definitions.json` — the rubric AgentScout uses
to score and rank agent opportunities.

Default tiers are:
1. Time Reclaimers — tactical automation
2. Expertise Amplifiers — knowledge capture, team leverage
3. Capability Unlocks — strategic opportunities

You'll be given the user's profile + a starter tier-definitions.json.
For each tier ask: does this apply, what weight should it carry, any
sub-categories specific to their work? Then produce the final JSON.

Keep it tight — at most 4 questions total. The output must be VALID JSON
matching the existing schema (schema_version + tiers array).
"#;

pub struct TierCalibrationConversation {
    pub conversation: Conversation,
    pub template_id: String,
}

impl TierCalibrationConversation {
    pub fn new(template_id: &str, user_profile_md: &str) -> Result<Self> {
        let template = find_template(template_id)
            .ok_or_else(|| anyhow!("unknown starter template: {}", template_id))?;
        let mut conversation = Conversation::new(ConversationKind::TierCalibration);
        let opener = format!(
            "Calibrating tier weights for your profile.\n\n**Your profile:**\n```markdown\n{}\n```\n\n**Starter tiers:**\n```json\n{}\n```\n\n1. Tier 1 (Time Reclaimers) is about tactical automation — does this resonate, and how heavily should it weight versus the other two? (Default 1.0)",
            user_profile_md.trim(),
            template.tier_skeleton_json.trim()
        );
        conversation.append_assistant(opener);
        Ok(Self {
            conversation,
            template_id: template.id.to_string(),
        })
    }

    pub async fn step(
        &mut self,
        user_reply: &str,
        client: &dyn AnthropicClient,
        model: &str,
    ) -> Result<&str> {
        self.conversation.append_user(user_reply);
        let messages = self.conversation.messages.clone();
        let req = CompletionRequest {
            messages: &messages,
            system: Some(SYSTEM_PROMPT),
            model,
            max_tokens: 1024,
        };
        let reply = client.complete(req).await?;
        self.conversation.append_assistant(&reply);
        Ok(self
            .conversation
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or(""))
    }

    pub async fn finalize(
        &mut self,
        client: &dyn AnthropicClient,
        model: &str,
        storage_root: &Path,
    ) -> Result<PathBuf> {
        let mut messages = self.conversation.messages.clone();
        messages.push(Message {
            role: Role::User,
            content:
                "Output ONLY the final tier-definitions.json — no preamble, no markdown fence, just raw JSON."
                    .to_string(),
        });
        let req = CompletionRequest {
            messages: &messages,
            system: Some(SYSTEM_PROMPT),
            model,
            max_tokens: 2048,
        };
        let raw = client.complete(req).await?;
        let json = strip_markdown_fence(&raw);

        // Validate before writing — if Claude returned invalid JSON, fail
        // here rather than corrupting the user's tier definitions.
        let _: serde_json::Value =
            serde_json::from_str(&json).context("tier definitions JSON failed to parse")?;

        let path = storage_root.join("tier-definitions.json");
        std::fs::create_dir_all(storage_root)?;
        std::fs::write(&path, &json)?;
        self.conversation.complete(path.to_string_lossy().to_string());
        Ok(path)
    }
}

fn strip_markdown_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::super::anthropic::MockAnthropicClient;
    use super::*;

    #[tokio::test]
    async fn finalize_strips_markdown_fences_and_validates_json() {
        let mut t = TierCalibrationConversation::new(
            "solo-engineer",
            "# User Profile\n**Role:** Senior engineer",
        )
        .unwrap();
        let mock = MockAnthropicClient::new(vec![
            "```json\n{\"schema_version\":1,\"tiers\":[]}\n```".into(),
        ]);
        let tmp = std::env::temp_dir().join(format!("as-tier-test-{}", uuid::Uuid::new_v4()));
        let path = t.finalize(&mock, "claude-sonnet-4-6", &tmp).await.unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn finalize_rejects_invalid_json() {
        let mut t = TierCalibrationConversation::new(
            "custom",
            "# User Profile\n**Role:** Test",
        )
        .unwrap();
        let mock = MockAnthropicClient::new(vec!["this is not json".into()]);
        let tmp = std::env::temp_dir().join(format!("as-tier-bad-{}", uuid::Uuid::new_v4()));
        let result = t.finalize(&mock, "claude-sonnet-4-6", &tmp).await;
        assert!(result.is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn strip_fence_handles_plain_json() {
        let s = strip_markdown_fence("{\"a\":1}");
        assert_eq!(s, "{\"a\":1}");
    }

    #[test]
    fn strip_fence_handles_json_fence() {
        let s = strip_markdown_fence("```json\n{\"a\":1}\n```");
        assert_eq!(s, "{\"a\":1}");
    }
}
