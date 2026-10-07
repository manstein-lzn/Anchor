use std::{error::Error as StdError, fmt};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    kind: &'static str,
}

impl Error {
    pub fn source(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            kind: "ResearchToolError",
        }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_request",
            message: message.into(),
            retryable: false,
            kind: "ValueError",
        }
    }

    pub fn malformed(message: impl Into<String>) -> Self {
        Self::source("invalid_source_response", message, false)
    }

    pub fn timeout() -> Self {
        Self::source("source_timeout", "source request timed out", true)
    }

    pub fn too_large() -> Self {
        Self::source(
            "source_too_large",
            "source exceeds the per-document download size limit",
            false,
        )
    }

    pub fn summary(&self, limit: usize) -> String {
        format!(
            "{}: {}",
            self.kind,
            self.message.chars().take(limit).collect::<String>()
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Error {}
