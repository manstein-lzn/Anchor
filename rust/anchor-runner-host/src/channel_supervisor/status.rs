use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const MAX_STATUS_BYTES: u64 = 4096;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionStatus {
    Connecting,
    Authenticating,
    Authenticated,
    Reconnecting,
    Stopped,
    Unavailable,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    socket: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    status: ConnectionStatus,
}

pub(crate) async fn read(descriptor: &Path) -> ConnectionStatus {
    tokio::time::timeout(Duration::from_millis(300), async {
        let mut bytes = Vec::new();
        tokio::fs::File::open(descriptor)
            .await
            .ok()?
            .take(MAX_STATUS_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() as u64 > MAX_STATUS_BYTES {
            return None;
        }
        let descriptor: Descriptor = serde_json::from_slice(&bytes).ok()?;
        if descriptor.socket.is_empty() || descriptor.token.is_empty() {
            return None;
        }
        let mut stream = UnixStream::connect(&descriptor.socket).await.ok()?;
        let request = json!({"operation":"status","token":descriptor.token});
        let mut request = serde_json::to_vec(&request).ok()?;
        request.push(b'\n');
        stream.write_all(&request).await.ok()?;
        let mut response = Vec::new();
        BufReader::new(stream.take(MAX_STATUS_BYTES + 1))
            .read_until(b'\n', &mut response)
            .await
            .ok()?;
        if response.len() as u64 > MAX_STATUS_BYTES || response.last() != Some(&b'\n') {
            return None;
        }
        let response: StatusResponse = serde_json::from_slice(&response).ok()?;
        Some(response.status)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(ConnectionStatus::Unavailable)
}
