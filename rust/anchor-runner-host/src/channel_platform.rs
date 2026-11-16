//! Shared channel identity; independent of application use cases and adapters.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelPlatform {
    #[default]
    Wecom,
    Feishu,
}

impl ChannelPlatform {
    pub(crate) const ALL: [Self; 2] = [Self::Wecom, Self::Feishu];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.as_str() == value)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Wecom => "wecom",
            Self::Feishu => "feishu",
        }
    }

    pub(crate) fn account_field(self) -> &'static str {
        match self {
            Self::Wecom => "WECOM_BOT_ID",
            Self::Feishu => "FEISHU_APP_ID",
        }
    }

    pub(crate) fn account_identity(self, identity: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{}:{:x}",
            self.as_str(),
            Sha256::digest(identity.trim().as_bytes())
        )
    }
}
