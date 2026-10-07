use std::{collections::BTreeSet, fmt, path::PathBuf, time::Duration};

use crate::GatewayError;

pub struct GatewayConfig {
    pub state_dir: PathBuf,
    pub ws_url: String,
    pub bot_id: String,
    pub secret: String,
    pub control_token: String,
    pub inbound_users: BTreeSet<String>,
    pub send_users: BTreeSet<String>,
    pub webhook: Option<WebhookConfig>,
    pub timing: Timing,
}

pub struct WebhookConfig {
    pub url: String,
    pub api_key: String,
    pub timeout: Duration,
}

impl fmt::Debug for GatewayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayConfig")
            .field("credentials", &"[REDACTED]")
            .field("timing", &self.timing)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for WebhookConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookConfig")
            .field("credentials", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub connect_timeout: Duration,
    pub ack_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub reconnect_base: Duration,
    pub reconnect_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            ack_timeout: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(30),
            reconnect_base: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

impl GatewayConfig {
    pub fn new(
        state_dir: impl Into<PathBuf>,
        bot_id: String,
        secret: String,
        control_token: String,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            ws_url: "wss://openws.work.weixin.qq.com".into(),
            bot_id,
            secret,
            control_token,
            inbound_users: BTreeSet::new(),
            send_users: BTreeSet::new(),
            webhook: None,
            timing: Timing::default(),
        }
    }

    pub fn from_env() -> Result<Self, GatewayError> {
        let mut config = Self::new(
            required("WECOM_CHANNEL_STATE")?,
            required("WECOM_BOT_ID")?,
            required("WECOM_BOT_SECRET")?,
            required("ANCHOR_CHANNEL_CONTROL_TOKEN")?,
        );
        config.inbound_users = users("ANCHOR_WECOM_USERS");
        config.send_users = users("ANCHOR_WECOM_SEND_USERS");
        if config.send_users.is_empty() {
            config.send_users = config.inbound_users.clone();
        }
        if let Ok(url) = std::env::var("ANCHOR_WECOM_WS_URL") {
            config.ws_url = url;
        }
        if let Ok(url) = std::env::var("ANCHOR_CHANNEL_WEBHOOK_URL") {
            config.webhook = Some(WebhookConfig {
                url,
                api_key: required("ANCHOR_API_KEY")?,
                timeout: Duration::from_secs(130),
            });
        }
        for (name, duration) in [
            (
                "ANCHOR_WECOM_CONNECT_MS",
                &mut config.timing.connect_timeout,
            ),
            ("ANCHOR_WECOM_ACK_MS", &mut config.timing.ack_timeout),
            (
                "ANCHOR_WECOM_HEARTBEAT_MS",
                &mut config.timing.heartbeat_interval,
            ),
            (
                "ANCHOR_WECOM_RECONNECT_MS",
                &mut config.timing.reconnect_base,
            ),
            (
                "ANCHOR_WECOM_RECONNECT_MAX_MS",
                &mut config.timing.reconnect_max,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                *duration =
                    Duration::from_millis(value.parse().map_err(|_| {
                        GatewayError::Invalid("invalid gateway timing environment")
                    })?);
            }
        }
        config.validate()?;
        for (name, expected) in [
            ("ANCHOR_CHANNEL_CONTROL_SOCKET", config.socket_path()),
            (
                "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR",
                config.descriptor_path(),
            ),
        ] {
            if let Some(value) = std::env::var_os(name)
                && value != expected
            {
                return Err(GatewayError::Invalid(
                    "control paths must be inside WECOM_CHANNEL_STATE with canonical names",
                ));
            }
        }
        Ok(config)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.state_dir.join("control.sock")
    }

    pub fn descriptor_path(&self) -> PathBuf {
        self.state_dir.join("control.json")
    }

    pub(crate) fn validate(&self) -> Result<(), GatewayError> {
        if !self.state_dir.is_absolute()
            || self.state_dir.components().any(|part| {
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
        for value in [&self.bot_id, &self.secret, &self.control_token] {
            if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
            {
                return Err(GatewayError::Invalid(
                    "missing or invalid gateway credential",
                ));
            }
        }
        if self.control_token.len() < 32 {
            return Err(GatewayError::Invalid(
                "control token must contain at least 32 bytes",
            ));
        }
        validate_url(&self.ws_url, true)?;
        if let Some(webhook) = &self.webhook {
            if webhook.url.len() > 4096 {
                return Err(GatewayError::Invalid("webhook URL exceeds size limit"));
            }
            validate_url(&webhook.url, false)?;
            if webhook.api_key.is_empty()
                || webhook.api_key.len() > 4096
                || webhook.api_key.chars().any(char::is_control)
            {
                return Err(GatewayError::Invalid(
                    "missing or invalid webhook credential",
                ));
            }
            if webhook.timeout.is_zero() || webhook.timeout > Duration::from_secs(180) {
                return Err(GatewayError::Invalid("invalid webhook timeout"));
            }
        }
        for users in [&self.inbound_users, &self.send_users] {
            if users.len() > 1024
                || users.iter().any(|user| {
                    user.trim().is_empty()
                        || user.chars().count() > 200
                        || user.chars().any(char::is_control)
                        || user == "@all"
                })
            {
                return Err(GatewayError::Invalid("invalid gateway userid allowlist"));
            }
        }
        let durations = [
            self.timing.connect_timeout,
            self.timing.ack_timeout,
            self.timing.heartbeat_interval,
            self.timing.reconnect_base,
            self.timing.reconnect_max,
        ];
        if durations
            .iter()
            .any(|duration| duration.is_zero() || *duration > Duration::from_secs(180))
            || self.timing.reconnect_base > self.timing.reconnect_max
        {
            return Err(GatewayError::Invalid("invalid gateway timing"));
        }
        Ok(())
    }
}

fn required(name: &str) -> Result<String, GatewayError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(GatewayError::Invalid(
            "required gateway environment is missing",
        ))
}

fn users(name: &str) -> BTreeSet<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn allowed(users: &BTreeSet<String>, userid: &str) -> bool {
    userid != "@all" && (users.contains(userid) || users.contains("*"))
}

fn validate_url(value: &str, websocket: bool) -> Result<(), GatewayError> {
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
        || url.query().is_some()
        || !(url.scheme() == secure || (url.scheme() == local && loopback))
    {
        return Err(GatewayError::Invalid(
            "transport requires TLS or explicit loopback without URL credentials",
        ));
    }
    Ok(())
}
