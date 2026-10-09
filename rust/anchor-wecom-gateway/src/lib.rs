#![forbid(unsafe_code)]

mod config;
mod gateway;
mod ledger;
mod media;
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

/// Long connections ignore `stream.msg_item`, so reply images travel as a
/// separate media upload plus an image message. These bounds mirror the Host's
/// reply contract so neither side accepts what the other cannot validate.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_IMAGE_COUNT: usize = 10;
pub const MAX_IMAGE_TOTAL_BASE64: usize = 14 * 1024 * 1024;
/// One upload chunk plus its base64 encoding and envelope. The 512 KiB payload
/// matches the chunk size the previous transport used against the platform.
pub const MEDIA_CHUNK_BYTES: usize = 512 * 1024;
pub const MAX_MEDIA_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// A callback response that carries validated reply images.
pub const MAX_MEDIA_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Inbound platform media. The gateway downloads and decrypts it, then hands
/// the bytes to the Host through the Host's existing attachment contract, so
/// these bounds mirror `channel_inputs`.
pub const MAX_INBOUND_FILES: usize = 16;
pub const MAX_INBOUND_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_INBOUND_TOTAL_BYTES: usize = 50 * 1024 * 1024;
/// A callback POST that carries base64 media, bounded by the Host body limit.
pub const MAX_MEDIA_REQUEST_BYTES: usize = 72 * 1024 * 1024;
/// One media download. The platform URL is short-lived, so a hung fetch must
/// not hold a callback open indefinitely.
pub const INBOUND_FETCH_TIMEOUT_SECS: u64 = 60;
/// How long an undelivered inbound stays eligible for crash recovery. The
/// platform's reply context is short-lived, so a restart rescues the message it
/// just accepted, not an event that failed hours ago.
pub const DEFAULT_RECOVERY_WINDOW_SECS: u64 = 900;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("unsupported media for this transport")]
    UnsupportedMedia,
    #[error("platform media could not be retrieved and decrypted")]
    Media,
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
    #[error("the Anchor channel webhook permanently refused this event")]
    Rejected,
}

impl From<rusqlite::Error> for GatewayError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Ledger
    }
}
