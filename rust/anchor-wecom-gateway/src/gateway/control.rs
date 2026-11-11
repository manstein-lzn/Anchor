use std::collections::BTreeSet;

use anchor_channel_transport::control::{authorize, read_request, write_response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};

use super::{ConnectionStatus, ProactiveDelivery, SendContent, SendRequest, stopping};
use crate::{
    GatewayError, MAX_CONTROL_REQUEST_BYTES, MAX_REQUEST_BYTES, MAX_TEXT_BYTES, config::allowed,
    media::outbound_file, protocol::identity,
};

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
    SendFile {
        token: String,
        request_id: String,
        userid: String,
        name: String,
        data_base64: String,
        md5: String,
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
    write_response(&mut stream, &value).await;
}

async fn dispatch(
    stream: &mut UnixStream,
    token: &str,
    users: &BTreeSet<String>,
    send: mpsc::Sender<SendRequest>,
    status: watch::Receiver<ConnectionStatus>,
) -> Result<Value, GatewayError> {
    let (request, length): (Request, _) = read_request(stream, MAX_CONTROL_REQUEST_BYTES).await?;
    let request_token = match &request {
        Request::Status { token }
        | Request::Send { token, .. }
        | Request::SendFile { token, .. } => token,
    };
    authorize(request_token, token)?;
    if !matches!(request, Request::SendFile { .. }) && length > MAX_REQUEST_BYTES {
        return Err(GatewayError::Invalid(
            "channel request exceeds text request limits",
        ));
    }
    let (request_id, userid, content) = match request {
        Request::Status { .. } => return Ok(json!({"status": *status.borrow()})),
        Request::Send {
            request_id,
            userid,
            content,
            ..
        } => {
            if content.trim().is_empty() || content.len() > MAX_TEXT_BYTES {
                return Err(GatewayError::Invalid(
                    "message content is empty or exceeds platform limits",
                ));
            }
            (request_id, userid, SendContent::Markdown(content))
        }
        Request::SendFile {
            request_id,
            userid,
            name,
            data_base64,
            md5,
            ..
        } => (
            request_id,
            userid,
            SendContent::File(outbound_file(name, &data_base64, &md5)?),
        ),
    };
    identity(Some(&json!(request_id)), 500)?;
    identity(Some(&json!(userid)), 200)?;
    if !allowed(users, &userid) {
        return Err(GatewayError::Invalid("recipient is not allowed"));
    }
    if *status.borrow() != ConnectionStatus::Authenticated {
        return Err(GatewayError::Disconnected);
    }
    let (response, result) = oneshot::channel();
    send.try_send(SendRequest {
        delivery: ProactiveDelivery {
            request_id,
            userid,
            response,
        },
        content,
    })
    .map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => GatewayError::Invalid("channel send queue is full"),
        mpsc::error::TrySendError::Closed(_) => GatewayError::Stopped,
    })?;
    result.await.map_err(|_| GatewayError::Stopped)?
}
