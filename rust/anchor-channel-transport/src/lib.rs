#![forbid(unsafe_code)]

pub mod control;
mod error;
mod event;
pub mod ledger;
pub mod media;
mod private_state;
pub mod progress;
mod settlement;
mod webhook;

use std::{collections::BTreeSet, path::Path, time::Duration};

pub use control::ConnectionStatus;
pub use error::GatewayError;
pub use event::{ChannelEvent, ConversationScope, digest, event_digest};
pub use private_state::PrivateState;
pub use settlement::{settlement_loop, stopping};
pub use webhook::{WebhookClient, WebhookConfig, parse_receipt};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_TEXT_BYTES: usize = 20_480;

pub fn allowed(users: &BTreeSet<String>, userid: &str) -> bool {
    userid != "@all" && (users.contains(userid) || users.contains("*"))
}

pub fn validate_state_dir(path: &Path) -> Result<(), GatewayError> {
    if !path.is_absolute()
        || path.components().any(|part| {
            !matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err(GatewayError::Invalid(
            "state directory must be an absolute normal path",
        ));
    }
    Ok(())
}

pub fn validate_url(value: &str, websocket: bool) -> Result<(), GatewayError> {
    validate_transport_url(value, websocket, false)
}

/// Only the authenticated platform bootstrap may supply query-bearing URLs.
pub fn validate_endpoint_url(value: &str) -> Result<(), GatewayError> {
    validate_transport_url(value, true, true)
}

fn validate_transport_url(
    value: &str,
    websocket: bool,
    allow_query: bool,
) -> Result<(), GatewayError> {
    let url =
        reqwest::Url::parse(value).map_err(|_| GatewayError::Invalid("invalid transport URL"))?;
    let secure = if websocket { "wss" } else { "https" };
    let local = if websocket { "ws" } else { "http" };
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!allow_query && url.query().is_some())
        || !(url.scheme() == secure || (url.scheme() == local && loopback))
    {
        return Err(GatewayError::Invalid(
            "transport requires TLS or explicit loopback without URL credentials",
        ));
    }
    Ok(())
}

pub fn http_client(timeout: Option<Duration>) -> Result<reqwest::Client, GatewayError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().map_err(|_| GatewayError::Webhook)
}

pub async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, GatewayError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(GatewayError::Webhook);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| GatewayError::Webhook)? {
        if bytes.len() + chunk.len() > limit {
            return Err(GatewayError::Webhook);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
