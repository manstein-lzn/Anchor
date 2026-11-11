use std::sync::Arc;

use anchor_channel_transport::{
    allowed,
    control::{authorize, read_request, write_response},
    digest,
    ledger::Ledger,
    media::{MAX_OUTBOUND_FILE_BYTES, OutboundFile, outbound_file},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
};

use crate::{
    ConnectionStatus, GatewayConfig, GatewayError, MAX_REQUEST_BYTES, MAX_TEXT_BYTES, api::Api,
    protocol::valid_id, worker::while_running,
};

const MAX_FILE_REQUEST_BYTES: usize = MAX_OUTBOUND_FILE_BYTES.div_ceil(3) * 4 + MAX_REQUEST_BYTES;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status {
        token: String,
    },
    Send {
        token: String,
        request_id: String,
        open_id: String,
        chat_id: String,
        content: String,
    },
    SendFile {
        token: String,
        request_id: String,
        open_id: String,
        chat_id: String,
        name: String,
        data_base64: String,
        md5: String,
    },
}

enum Payload {
    Markdown(String),
    File(OutboundFile),
}

enum WireContent {
    Markdown(String),
    File(String),
}

pub(crate) async fn run(
    listener: UnixListener,
    config: Arc<GatewayConfig>,
    api: Arc<Api>,
    ledger: Arc<Ledger>,
    status: watch::Receiver<ConnectionStatus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    let mut clients = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = crate::gateway::stopping(&mut shutdown) => break Ok(()),
            accepted = listener.accept(), if clients.len() < 32 => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => break Err(GatewayError::PrivateState),
                };
                clients.spawn(handle(stream, config.clone(), api.clone(), ledger.clone(), status.clone(), shutdown.clone()));
            },
            _ = clients.join_next(), if !clients.is_empty() => {},
        }
    };
    // Network waits observe shutdown, but blocking claims must finish before
    // the Gateway releases its exclusive state lease.
    while clients.join_next().await.is_some() {}
    result
}

async fn handle(
    mut stream: UnixStream,
    config: Arc<GatewayConfig>,
    api: Arc<Api>,
    ledger: Arc<Ledger>,
    status: watch::Receiver<ConnectionStatus>,
    mut shutdown: watch::Receiver<bool>,
) {
    let result = async {
        let (request, size): (Request, _) = while_running(
            &mut shutdown,
            read_request(&mut stream, MAX_FILE_REQUEST_BYTES),
        )
        .await?;
        let supplied = match &request {
            Request::Status { token }
            | Request::Send { token, .. }
            | Request::SendFile { token, .. } => token,
        };
        authorize(supplied, &config.control_token)?;
        if !matches!(&request, Request::SendFile { .. }) && size > MAX_REQUEST_BYTES {
            return Err(GatewayError::Invalid(
                "text control request exceeds size limit",
            ));
        }
        let (request_id, open_id, chat_id, payload) = match request {
            Request::Status { .. } => return Ok(json!({"status":*status.borrow()})),
            Request::Send {
                request_id,
                open_id,
                chat_id,
                content,
                ..
            } => (request_id, open_id, chat_id, Payload::Markdown(content)),
            Request::SendFile {
                request_id,
                open_id,
                chat_id,
                name,
                data_base64,
                md5,
                ..
            } => (
                request_id,
                open_id,
                chat_id,
                Payload::File(outbound_file(name, &data_base64, &md5)?),
            ),
        };
        if !valid_id(&request_id) || !valid_id(&open_id) || !valid_id(&chat_id) {
            return Err(GatewayError::Invalid("invalid Feishu send identity"));
        }
        if !allowed(&config.send_users, &open_id) {
            return Err(GatewayError::Invalid("recipient is not allowed"));
        }
        let bound = match &payload {
            Payload::Markdown(content) => {
                if content.trim().is_empty() || content.len() > MAX_TEXT_BYTES {
                    return Err(GatewayError::Invalid(
                        "message content is empty or exceeds platform limits",
                    ));
                }
                // Retain old text receipt identities: Markdown is only a projection.
                json!([open_id, chat_id, content])
            }
            Payload::File(file) => {
                json!([open_id, chat_id, "file", file.name, digest(&file.bytes)])
            }
        };
        let content_digest = digest(&serde_json::to_vec(&bound).map_err(|_| GatewayError::Ledger)?);
        let wire_id = format!("feishu-send-{}", digest(request_id.as_bytes()));
        let ack = json!({"accepted":true,"request_id":request_id});
        let previous_id = request_id.clone();
        let previous_digest = content_digest.clone();
        if ledger
            .blocking(move |ledger| ledger.previous("send", &previous_id, &previous_digest))
            .await?
        {
            return Ok(ack);
        }
        let card = match &payload {
            Payload::Markdown(text) => Some(crate::rich::markdown_card(text)?),
            Payload::File(_) => None,
        };
        let token = while_running(&mut shutdown, api.token()).await?;
        let wire_content = match payload {
            Payload::Markdown(_) => WireContent::Markdown(card.expect("Markdown card validated")),
            Payload::File(file) => WireContent::File(
                while_running(
                    &mut shutdown,
                    api.upload_file(&file.name, file.bytes, &token),
                )
                .await?,
            ),
        };
        let claim_wire = wire_id.clone();
        if !ledger
            .blocking(move |ledger| ledger.claim("send", &request_id, &content_digest, &claim_wire))
            .await?
        {
            return Ok(ack);
        }
        while_running(&mut shutdown, async {
            match wire_content {
                WireContent::Markdown(card) => api.send(&open_id, &card, &token).await,
                WireContent::File(key) => api.send_file(&open_id, &key, &token).await,
            }
        })
        .await
        .map_err(|_| GatewayError::Unconfirmed)?;
        ledger
            .blocking(move |ledger| ledger.confirm(&wire_id))
            .await?;
        Ok::<Value, GatewayError>(ack)
    }
    .await;
    let response = result.unwrap_or_else(|error| json!({"error":error.to_string()}));
    write_response(&mut stream, &response).await;
}
