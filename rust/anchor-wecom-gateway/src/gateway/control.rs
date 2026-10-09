use std::{collections::BTreeSet, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};

use super::{ConnectionStatus, SendRequest, stopping};
use crate::{GatewayError, MAX_REQUEST_BYTES, MAX_TEXT_BYTES, config::allowed, protocol::identity};

pub(super) struct Server {
    pub listener: UnixListener,
    pub token: String,
    pub users: BTreeSet<String>,
    pub send: mpsc::Sender<SendRequest>,
    pub shutdown: watch::Receiver<bool>,
    pub status: watch::Receiver<ConnectionStatus>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status {
        token: String,
    },
    Send {
        token: String,
        request_id: String,
        userid: String,
        content: String,
    },
}

impl Server {
    pub async fn run(mut self) -> Result<(), GatewayError> {
        let mut clients = JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = stopping(&mut self.shutdown) => break Ok(()),
                accepted = self.listener.accept(), if clients.len() < 32 => {
                    match accepted {
                        Ok((stream, _)) => {
                            let token = self.token.clone();
                            let users = self.users.clone();
                            let send = self.send.clone();
                            let status = self.status.clone();
                            clients.spawn(async move { handle(stream, &token, &users, send, status).await; });
                        },
                        Err(_) => break Err(GatewayError::PrivateState),
                    }
                },
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        };
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        result
    }
}

async fn handle(
    mut stream: UnixStream,
    token: &str,
    users: &BTreeSet<String>,
    send: mpsc::Sender<SendRequest>,
    status: watch::Receiver<ConnectionStatus>,
) {
    let result = dispatch(&mut stream, token, users, send, status).await;
    let value = result.unwrap_or_else(|error| json!({"error": error.to_string()}));
    let Ok(mut bytes) = serde_json::to_vec(&value) else {
        return;
    };
    bytes.push(b'\n');
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.write_all(&bytes)).await;
    let _ = stream.shutdown().await;
}

async fn dispatch(
    stream: &mut UnixStream,
    token: &str,
    users: &BTreeSet<String>,
    send: mpsc::Sender<SendRequest>,
    status: watch::Receiver<ConnectionStatus>,
) -> Result<Value, GatewayError> {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(stream.take((MAX_REQUEST_BYTES + 1) as u64)).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| GatewayError::Invalid("channel request timed out"))?
    .map_err(|_| GatewayError::Invalid("invalid channel request"))?;
    if bytes.len() > MAX_REQUEST_BYTES || bytes.last() != Some(&b'\n') {
        return Err(GatewayError::Invalid(
            "channel request is oversized or missing NDJSON newline",
        ));
    }
    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::Invalid("invalid channel request"))?;
    let request_token = match &request {
        Request::Status { token } | Request::Send { token, .. } => token,
    };
    if !bool::from(request_token.as_bytes().ct_eq(token.as_bytes())) {
        return Err(GatewayError::Invalid("unauthorized channel request"));
    }
    let Request::Send {
        request_id,
        userid,
        content,
        ..
    } = request
    else {
        return Ok(json!({"status": *status.borrow()}));
    };
    identity(Some(&json!(request_id)), 500)?;
    identity(Some(&json!(userid)), 200)?;
    if content.trim().is_empty() || content.len() > MAX_TEXT_BYTES {
        return Err(GatewayError::Invalid(
            "message content is empty or exceeds platform limits",
        ));
    }
    if !allowed(users, &userid) {
        return Err(GatewayError::Invalid("recipient is not allowed"));
    }
    if *status.borrow() != ConnectionStatus::Authenticated {
        return Err(GatewayError::Disconnected);
    }
    let (response, result) = oneshot::channel();
    send.try_send(SendRequest {
        request_id,
        userid,
        content,
        response,
    })
    .map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => GatewayError::Invalid("channel send queue is full"),
        mpsc::error::TrySendError::Closed(_) => GatewayError::Stopped,
    })?;
    result.await.map_err(|_| GatewayError::Stopped)?
}
