use std::{collections::BTreeSet, sync::Arc};

use anchor_channel_transport::{
    control::{authorize, read_request, write_response},
    ledger::Ledger,
};
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
    pub ledger: Arc<Ledger>,
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
    DeliveryStatus {
        token: String,
        request_ids: Vec<String>,
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
                            let ledger = self.ledger.clone();
                            let send = self.send.clone();
                            let status = self.status.clone();
                            clients.spawn(async move { handle(stream, &token, &users, ledger, send, status).await; });
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
    ledger: Arc<Ledger>,
    send: mpsc::Sender<SendRequest>,
    status: watch::Receiver<ConnectionStatus>,
) {
    let result = dispatch(&mut stream, token, users, ledger, send, status).await;
    let value = result.unwrap_or_else(|error| json!({"error": error.to_string()}));
    write_response(&mut stream, &value).await;
}

async fn dispatch(
    stream: &mut UnixStream,
    token: &str,
    users: &BTreeSet<String>,
    ledger: Arc<Ledger>,
    send: mpsc::Sender<SendRequest>,
    status: watch::Receiver<ConnectionStatus>,
) -> Result<Value, GatewayError> {
    let (request, length): (Request, _) = read_request(stream, MAX_CONTROL_REQUEST_BYTES).await?;
    let request_token = match &request {
        Request::Status { token }
        | Request::DeliveryStatus { token, .. }
        | Request::Send { token, .. }
        | Request::SendFile { token, .. } => token,
    };
    authorize(request_token, token)?;
    if !matches!(
        request,
        Request::SendFile { .. } | Request::DeliveryStatus { .. }
    ) && length > MAX_REQUEST_BYTES
    {
        return Err(GatewayError::Invalid(
            "channel request exceeds text request limits",
        ));
    }
    let (request_id, userid, content) = match request {
        Request::Status { .. } => return Ok(json!({"status": *status.borrow()})),
        Request::DeliveryStatus { request_ids, .. } => {
            for id in &request_ids {
                identity(Some(&json!(id)), 500)?;
            }
            let deliveries = ledger
                .blocking(move |ledger| ledger.delivery_status(&request_ids))
                .await?;
            return Ok(json!({"deliveries":deliveries}));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tokio::io::AsyncWriteExt;

    async fn query(socket: &Path, request: Value) -> Value {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        read_request(&mut stream, MAX_REQUEST_BYTES)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn delivery_status_is_authenticated_bounded_and_never_enters_the_send_queue() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let ledger = Arc::new(Ledger::open(Path::new(":memory:"), "fixture").unwrap());
        ledger
            .claim("send", "confirmed", "digest", "wire-1")
            .unwrap();
        ledger.confirm("wire-1").unwrap();
        ledger.claim("send", "unknown", "digest", "wire-2").unwrap();
        let before = ledger.facts().unwrap();
        let (send, mut incoming) = mpsc::channel(1);
        let (stop, shutdown) = watch::channel(false);
        let (_status, status) = watch::channel(ConnectionStatus::Authenticating);
        let server = tokio::spawn(
            Server {
                listener: UnixListener::bind(&socket).unwrap(),
                token: "fixture-token".into(),
                users: BTreeSet::new(),
                ledger: ledger.clone(),
                send,
                shutdown,
                status,
            }
            .run(),
        );
        let request = json!({"operation":"delivery_status","token":"fixture-token",
            "request_ids":["confirmed","unknown","missing"]});
        let mut unauthorized = request.clone();
        unauthorized["token"] = json!("wrong-token");
        assert_eq!(
            query(&socket, unauthorized).await,
            json!({"error":"unauthorized channel request"})
        );
        assert_eq!(
            query(&socket, request.clone()).await,
            json!({"deliveries":{"confirmed":"confirmed","unknown":"unknown"}})
        );
        for ids in [
            json!([""]),
            json!([" "]),
            json!(["bad\nidentity"]),
            json!(["confirmed", "confirmed"]),
            json!(["a".repeat(501)]),
            json!((0..1025).map(|id| id.to_string()).collect::<Vec<_>>()),
        ] {
            let mut invalid = request.clone();
            invalid["request_ids"] = ids;
            assert!(query(&socket, invalid).await.get("error").is_some());
        }
        let mut ids = (0..1021)
            .map(|id| format!("channel-task-{id}-{}", "a".repeat(64)))
            .collect::<Vec<_>>();
        ids.extend(["a".repeat(500), "confirmed".into(), "unknown".into()]);
        let mut maximum = request.clone();
        maximum["request_ids"] = json!(ids);
        assert!(maximum.to_string().len() > MAX_REQUEST_BYTES);
        assert_eq!(
            query(&socket, maximum).await,
            json!({"deliveries":{"confirmed":"confirmed","unknown":"unknown"}})
        );
        let mut empty = request;
        empty["request_ids"] = json!([]);
        assert_eq!(query(&socket, empty).await, json!({"deliveries":{}}));
        assert_eq!(ledger.facts().unwrap(), before);
        assert!(matches!(
            incoming.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        stop.send_replace(true);
        server.await.unwrap().unwrap();
    }
}
