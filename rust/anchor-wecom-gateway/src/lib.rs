#![forbid(unsafe_code)]

mod config;
mod gateway;
mod ledger;
mod private_state;
mod protocol;
mod webhook;

#[cfg(test)]
mod tests;

pub use config::{GatewayConfig, Timing, WebhookConfig};
pub use gateway::{ConnectionStatus, Gateway, RunningGateway};
pub use ledger::{DeliveryFact, DeliveryStatus};
pub use protocol::{ChannelEvent, normalize_message, stream_id};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_TEXT_BYTES: usize = 20_480;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("unsupported media; this gateway accepts text and text-only mixed messages")]
    UnsupportedMedia,
    #[error("private gateway state is unavailable or unsafe")]
    PrivateState,
    #[error("gateway state directory is already leased")]
    AlreadyRunning,
    #[error("gateway ledger is unavailable")]
    Ledger,
    #[error("gateway is not authenticated; no message was dispatched")]
    Disconnected,
    #[error("gateway has stopped")]
    Stopped,
    #[error("delivery is unconfirmed; do not resend automatically")]
    Unconfirmed,
    #[error("previous delivery is unconfirmed; do not resend automatically")]
    PreviousUnconfirmed,
    #[error("Anchor channel webhook failed; check the accepted event before continuing")]
    Webhook,
}

impl From<rusqlite::Error> for GatewayError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Ledger
    }
}
