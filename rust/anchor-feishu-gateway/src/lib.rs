#![forbid(unsafe_code)]

mod api;
mod config;
mod control;
mod gateway;
mod media;
mod progress;
pub mod protocol;
mod rich;
mod worker;

pub use anchor_channel_transport::WebhookConfig;
pub use anchor_channel_transport::ledger::{DeliveryFact, DeliveryStatus};
pub use anchor_channel_transport::{ChannelEvent, GatewayError, MAX_REQUEST_BYTES, MAX_TEXT_BYTES};
pub use config::{GatewayConfig, Timing};
pub use gateway::{ConnectionStatus, Gateway, RunningGateway};

/// The Host attachment envelope has its own bound, separate from event frames.
pub const MAX_MEDIA_REQUEST_BYTES: usize = 72 * 1024 * 1024;
