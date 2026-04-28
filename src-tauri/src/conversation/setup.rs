use super::{
    templates::{find_template, STARTER_TEMPLATES},
    Conversation, ConversationKind,
};
use crate::anthropic::{AnthropicClient, CompletionRequest, Message, Role};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

const SYSTEM_PROMPT: &str = r#"You are AgentScout's setup assistant. Your job is to help the user
finalize their `user-profile.md` so AgentScout can give them grounded
agent-opportunity recommendations later.

You will be given a starter profile skeleton. Ask 3–5 targeted questions
to refine it: role specifics, company stage, team shape, what they
spend most time on, what bugs them about their workflow, what they're
trying to grow strategically, and what constraints exist (regulated
content, confidentiality concerns).

Keep questions short and concrete. After enough back-and-forth, produce
the final user-profile.md as your output. Never ask more than 5 questions
total — if the answers are sparse, fill in reasonable defaults the user
can edit later.
"#;

pub struct SetupConversation {
    pub conversation: Conversation,
    pub template: SetupTemplate,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetupTemplate {
    pub id: String,
    pub name: String,
}

impl SetupConversation {
    pub fn new(template_id: &str) -> Result<Self> {
        let template = find_template(template_id)
            .ok_or_else(|| anyhow!("unknown starter template: {}", template_id))?;
        let mut conversation = Conversation::new(ConversationKind::Setup);

        // Seed the assistant turn with the skeleton + opening question
        let opener = format!(
            "Starting from the **{}** template. Here's the rough profile:\n\n```markdown\n{}\n```\n\nA few quick questions to refine this — feel free to be brief or skip.\n\n1. What's your role and seniority?",
            template.name, template.profile_skeleton
        );
        conversation.append_assistant(opener);

        Ok(Self {
            conversation,
            template: SetupTemplate {
                id: template.id.to_string(),
                name: template.name.to_string(),
            },
        })
    }

    pub fn from_existing(conversation: Conversation, template_id: &str) -> Result<Self> {
        let template = find_template(template_id)
            .ok_or_else(|| anyhow!("unknown starter template: {}", template_id))?;
        Ok(Self {
            conversation,
            template: SetupTemplate {
                id: template.id.to_string(),
                name: template.name.to_string(),
            },
        })
    }

    /// Send the user's reply through Claude and append the assistant's
    /// follow-up turn to the conversation.
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
            cache_breakpoint: None,
        };
        let response = client.complete(req).await?;
        self.conversation.append_assistant(&response.text);
        Ok(self
            .conversation
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or(""))
    }

    /// Synthesize the final user-profile.md from the transcript and write
    /// it to `<storage>/user-profile.md`. Marks the conversation complete.
    pub async fn finalize(
        &mut self,
        client: &dyn AnthropicClient,
        model: &str,
        storage_root: &Path,
    ) -> Result<PathBuf> {
        let mut messages = self.conversation.messages.clone();
        messages.push(Message {
            role: Role::User,
            content: "Based on what I've shared, write the final user-profile.md. \
                 Output ONLY the markdown — no preamble, no closing remarks."
                .to_string(),
        });
        let req = CompletionRequest {
            messages: &messages,
            system: Some(SYSTEM_PROMPT),
            model,
            max_tokens: 2048,
            cache_breakpoint: None,
        };
        let response = client.complete(req).await?;

        let path = storage_root.join("user-profile.md");
        std::fs::create_dir_all(storage_root)?;
        std::fs::write(&path, &response.text)?;
        self.conversation
            .complete(path.to_string_lossy().to_string());
        Ok(path)
    }
}

pub fn list_templates() -> &'static [super::templates::StarterTemplate] {
    STARTER_TEMPLATES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::MockAnthropicClient;

    #[tokio::test]
    async fn setup_seeds_with_template_and_advances_on_step() {
        let mut s = SetupConversation::new("solo-engineer").unwrap();
        assert_eq!(s.conversation.messages.len(), 1);

        let mock = MockAnthropicClient::new(vec!["Got it — and what's your tech stack?".into()]);
        let reply = s
            .step(
                "I'm a senior engineer at a small startup.",
                &mock,
                "claude-sonnet-4-6",
            )
            .await
            .unwrap();
        assert!(reply.contains("tech stack"));
        assert_eq!(s.conversation.messages.len(), 3); // assistant, user, assistant
    }

    #[tokio::test]
    async fn unknown_template_id_errors() {
        assert!(SetupConversation::new("does-not-exist").is_err());
    }

    #[tokio::test]
    async fn finalize_writes_profile_and_marks_complete() {
        let mut s = SetupConversation::new("custom").unwrap();
        let mock = MockAnthropicClient::new(vec!["# User Profile\n\n**Role:** Test".into()]);
        let tmp = std::env::temp_dir().join(format!("as-setup-test-{}", uuid::Uuid::new_v4()));
        let path = s.finalize(&mock, "claude-sonnet-4-6", &tmp).await.unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("User Profile"));
        assert!(s.conversation.is_complete());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
