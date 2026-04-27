pub mod anthropic;
pub mod setup;
pub mod templates;
pub mod tier_calibration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use anthropic::{AnthropicClient, LiveAnthropicClient, MockAnthropicClient, Message, Role};
pub use setup::{SetupConversation, SetupTemplate};
pub use templates::STARTER_TEMPLATES;
pub use tier_calibration::TierCalibrationConversation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Setup,
    TierCalibration,
}

impl ConversationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConversationKind::Setup => "setup",
            ConversationKind::TierCalibration => "tier_calibration",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub messages: Vec<Message>,
    pub output_path: Option<String>,
}

impl Conversation {
    pub fn new(kind: ConversationKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            started_at: chrono::Utc::now().timestamp(),
            completed_at: None,
            messages: Vec::new(),
            output_path: None,
        }
    }

    pub fn complete(&mut self, output_path: impl Into<String>) {
        self.completed_at = Some(chrono::Utc::now().timestamp());
        self.output_path = Some(output_path.into());
    }

    pub fn is_complete(&self) -> bool {
        self.completed_at.is_some()
    }

    pub fn append_user(&mut self, content: impl Into<String>) {
        self.messages.push(Message {
            role: Role::User,
            content: content.into(),
        });
    }

    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.into(),
        });
    }
}

pub fn persist(storage: &crate::storage::Storage, c: &Conversation) -> Result<()> {
    let json = serde_json::to_string(&c.messages)?;
    storage.with_conn(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO conversations
             (id, kind, started_at, completed_at, transcript_json, output_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                c.id.to_string(),
                c.kind.as_str(),
                c.started_at,
                c.completed_at,
                json,
                c.output_path,
            ],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_lifecycle() {
        let mut c = Conversation::new(ConversationKind::Setup);
        assert!(!c.is_complete());
        c.append_assistant("Hi! Let's set up your profile.");
        c.append_user("I'm a solo engineer.");
        assert_eq!(c.messages.len(), 2);
        c.complete("/tmp/user-profile.md");
        assert!(c.is_complete());
        assert_eq!(c.output_path.as_deref(), Some("/tmp/user-profile.md"));
    }

    #[test]
    fn kind_serializes_snake_case() {
        let k = ConversationKind::TierCalibration;
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(s, "\"tier_calibration\"");
    }
}
