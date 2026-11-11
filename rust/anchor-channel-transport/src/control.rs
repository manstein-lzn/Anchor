use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::GatewayError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connecting,
    Authenticating,
    Authenticated,
    Reconnecting,
    Stopped,
}

pub async fn read_request<T: DeserializeOwned>(
    stream: &mut UnixStream,
    limit: usize,
) -> Result<(T, usize), GatewayError> {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(stream.take((limit + 1) as u64)).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| GatewayError::Invalid("channel request timed out"))?
    .map_err(|_| GatewayError::Invalid("invalid channel request"))?;
    if bytes.len() > limit || bytes.last() != Some(&b'\n') {
        return Err(GatewayError::Invalid(
            "channel request is oversized or missing NDJSON newline",
        ));
    }
    let request = serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::Invalid("invalid channel request"))?;
    Ok((request, bytes.len()))
}

pub fn authorize(supplied: &str, token: &str) -> Result<(), GatewayError> {
    if !bool::from(supplied.as_bytes().ct_eq(token.as_bytes())) {
        return Err(GatewayError::Invalid("unauthorized channel request"));
    }
    Ok(())
}

pub async fn write_response(stream: &mut UnixStream, value: &Value) {
    if let Ok(mut bytes) = serde_json::to_vec(value) {
        bytes.push(b'\n');
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.write_all(&bytes)).await;
    }
    let _ = stream.shutdown().await;
}
