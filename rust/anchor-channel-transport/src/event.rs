use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub source: String,
    pub event_id: String,
    pub sender_id: String,
    pub conversation_id: String,
    pub text: String,
    pub reply_target: String,
    pub message_type: String,
    pub metadata: BTreeMap<String, Value>,
    pub attachments: Vec<Value>,
}

/// A shared group has one live turn even when a different member speaks next.
/// The enum keeps a private conversation separate from a group with the same ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConversationScope {
    Single {
        sender_id: String,
        conversation_id: String,
    },
    Group {
        conversation_id: String,
    },
}

impl ChannelEvent {
    pub fn conversation_scope(&self) -> ConversationScope {
        if self.metadata.get("chat_type").and_then(Value::as_str) == Some("group") {
            ConversationScope::Group {
                conversation_id: self.conversation_id.clone(),
            }
        } else {
            // Stored private events predate explicit chat types.
            ConversationScope::Single {
                sender_id: self.sender_id.clone(),
                conversation_id: self.conversation_id.clone(),
            }
        }
    }
}

pub fn event_digest(event: &ChannelEvent) -> String {
    let mut bound = event.clone();
    bound.metadata.remove("request_id");
    digest(&serde_json::to_vec(&bound).expect("serializable channel event"))
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
