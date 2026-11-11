use std::{collections::BTreeSet, fmt, path::PathBuf, time::Duration};

use anchor_channel_transport::{validate_state_dir, validate_url};

use crate::{GatewayError, WebhookConfig};

pub struct GatewayConfig {
    pub state_dir: PathBuf,
    pub app_id: String,
    pub app_secret: String,
    pub control_token: String,
    pub api_base_url: String,
    pub inbound_users: BTreeSet<String>,
    pub send_users: BTreeSet<String>,
    pub webhook: Option<WebhookConfig>,
    pub progress_url: Option<String>,
    pub timing: Timing,
    pub recovery_window: Duration,
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

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub connect_timeout: Duration,
    pub http_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub reconnect_base: Duration,
    pub reconnect_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            http_timeout: Duration::from_secs(15),
            heartbeat_interval: Duration::from_secs(120),
            reconnect_base: Duration::from_secs(2),
            reconnect_max: Duration::from_secs(120),
        }
    }
}

impl GatewayConfig {
    pub fn new(
        state_dir: impl Into<PathBuf>,
        app_id: String,
        app_secret: String,
        control_token: String,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            app_id,
            app_secret,
            control_token,
            api_base_url: "https://open.feishu.cn".into(),
            inbound_users: BTreeSet::new(),
            send_users: BTreeSet::new(),
            webhook: None,
            progress_url: None,
            timing: Timing::default(),
            recovery_window: Duration::from_secs(900),
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.state_dir.join("control.sock")
    }

    pub fn descriptor_path(&self) -> PathBuf {
        self.state_dir.join("control.json")
    }

    pub fn from_env() -> Result<Self, GatewayError> {
        let mut config = Self::new(
            required("FEISHU_CHANNEL_STATE")?,
            required("FEISHU_APP_ID")?,
            required("FEISHU_APP_SECRET")?,
            required("ANCHOR_CHANNEL_CONTROL_TOKEN")?,
        );
        config.inbound_users = users("ANCHOR_FEISHU_USERS")?;
        config.send_users = users("ANCHOR_FEISHU_SEND_USERS")?;
        if let Ok(value) = std::env::var("ANCHOR_FEISHU_API_BASE_URL") {
            config.api_base_url = value;
        }
        config.webhook = Some(WebhookConfig {
            url: required("ANCHOR_CHANNEL_WEBHOOK_URL")?,
            api_key: required("ANCHOR_API_KEY")?,
            timeout: match std::env::var("ANCHOR_CHANNEL_WEBHOOK_TIMEOUT_MS") {
                Ok(value) => Some(parse_millis(&value)?),
                Err(std::env::VarError::NotPresent) => None,
                Err(_) => return Err(GatewayError::Invalid("invalid webhook timeout")),
            },
        });
        config.progress_url = match std::env::var("ANCHOR_CHANNEL_PROGRESS_URL") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err(GatewayError::Invalid("invalid progress URL environment")),
        };
        for (name, duration) in [
            (
                "ANCHOR_FEISHU_CONNECT_MS",
                &mut config.timing.connect_timeout,
            ),
            ("ANCHOR_FEISHU_HTTP_MS", &mut config.timing.http_timeout),
            (
                "ANCHOR_FEISHU_HEARTBEAT_MS",
                &mut config.timing.heartbeat_interval,
            ),
            (
                "ANCHOR_FEISHU_RECONNECT_MS",
                &mut config.timing.reconnect_base,
            ),
            (
                "ANCHOR_FEISHU_RECONNECT_MAX_MS",
                &mut config.timing.reconnect_max,
            ),
            (
                "ANCHOR_FEISHU_RECOVERY_WINDOW_MS",
                &mut config.recovery_window,
            ),
        ] {
            match std::env::var(name) {
                Ok(value) => *duration = parse_millis(&value)?,
                Err(std::env::VarError::NotPresent) => {}
                Err(_) => return Err(GatewayError::Invalid("invalid gateway timing environment")),
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
                    "control paths must be inside FEISHU_CHANNEL_STATE with canonical names",
                ));
            }
        }
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), GatewayError> {
        validate_state_dir(&self.state_dir)?;
        for value in [&self.app_id, &self.app_secret, &self.control_token] {
            credential(value)?;
        }
        if self.control_token.len() < 32 {
            return Err(GatewayError::Invalid(
                "control token must contain at least 32 bytes",
            ));
        }
        validate_url(&self.api_base_url, false)?;
        let base = reqwest::Url::parse(&self.api_base_url)
            .map_err(|_| GatewayError::Invalid("invalid API base URL"))?;
        if base.path() != "/" {
            return Err(GatewayError::Invalid(
                "API base URL must not contain a path",
            ));
        }
        let webhook = self
            .webhook
            .as_ref()
            .ok_or(GatewayError::Invalid("Host channel webhook is required"))?;
        validate_url(&webhook.url, false)?;
        if let Some(url) = &self.progress_url {
            validate_url(url, false)?;
        }
        credential(&webhook.api_key)?;
        if webhook.timeout.is_some_and(|value| value.is_zero()) {
            return Err(GatewayError::Invalid("invalid webhook timeout"));
        }
        for list in [&self.inbound_users, &self.send_users] {
            if list.len() > 1024
                || list
                    .iter()
                    .any(|id| id != "*" && !crate::protocol::valid_id(id))
            {
                return Err(GatewayError::Invalid("invalid Feishu open_id allowlist"));
            }
        }
        for duration in [
            self.timing.connect_timeout,
            self.timing.http_timeout,
            self.timing.heartbeat_interval,
            self.timing.reconnect_base,
            self.timing.reconnect_max,
        ] {
            if duration.is_zero() || duration > Duration::from_secs(180) {
                return Err(GatewayError::Invalid("invalid gateway timing"));
            }
        }
        if self.timing.reconnect_base > self.timing.reconnect_max
            || self.recovery_window.is_zero()
            || self.recovery_window > Duration::from_secs(86_400)
        {
            return Err(GatewayError::Invalid(
                "invalid gateway recovery or reconnect window",
            ));
        }
        Ok(())
    }
}

fn credential(value: &str) -> Result<(), GatewayError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(GatewayError::Invalid(
            "missing or invalid gateway credential",
        ));
    }
    Ok(())
}

fn required(name: &str) -> Result<String, GatewayError> {
    std::env::var(name)
        .map_err(|_| GatewayError::Invalid("required gateway environment is missing"))
}

fn users(name: &str) -> Result<BTreeSet<String>, GatewayError> {
    let value = match std::env::var(name) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(_) => {
            return Err(GatewayError::Invalid(
                "invalid gateway allowlist environment",
            ));
        }
    };
    Ok(value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect())
}

fn parse_millis(value: &str) -> Result<Duration, GatewayError> {
    value
        .parse()
        .map(Duration::from_millis)
        .map_err(|_| GatewayError::Invalid("invalid gateway timing environment"))
}
