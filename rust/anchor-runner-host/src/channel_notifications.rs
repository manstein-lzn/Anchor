//! Projection of confirmed proactive deliveries into the next native input.
//! Files stay in the existing retained Artifact store; this adds no file archive.
use anchor_platform_session::ChannelNotification;
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) fn file_path(id: &str, name: &str) -> PathBuf {
    PathBuf::from("/in/notifications").join(id).join(name)
}

pub(crate) fn projection(notification: ChannelNotification) -> Value {
    json!({
        "id": notification.id,
        "source_run": notification.source_run,
        "content": notification.content,
        "status": "confirmed",
        "files": notification.files.iter().map(|file| json!({
            "name":file.name,
            "path":file_path(&notification.id, &file.name),
            "artifact_id":file.artifact_id,
        })).collect::<Vec<_>>(),
    })
}
