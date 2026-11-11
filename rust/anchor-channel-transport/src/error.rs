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
    #[error("platform media upload failed; no message was dispatched")]
    MediaUpload,
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
    #[error("the channel assistant is stopped and requires explicit resume")]
    AssistantStopped,
}

impl From<rusqlite::Error> for GatewayError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Ledger
    }
}
