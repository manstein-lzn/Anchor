use std::path::{Path, PathBuf};

use reqwest::Url;

use crate::{UploadError, assets::open_directory};

pub const DEFAULT_ENDPOINT: &str = "https://docmost.cwise.dev/api/files/upload";
pub const UPLOAD_ROOT: &str = "/in/publish/assets";

#[derive(Clone)]
pub struct Config {
    pub(crate) endpoint: Url,
    pub(crate) upload_root: PathBuf,
    pub(crate) api_key: String,
}

impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            endpoint: Url::parse(DEFAULT_ENDPOINT).expect("fixed Docmost endpoint is valid"),
            upload_root: PathBuf::from(UPLOAD_ROOT),
            api_key: api_key.into(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(std::env::var("DOCMOST_API_KEY").unwrap_or_default())
    }

    pub fn with_endpoint(mut self, endpoint: &str) -> Result<Self, UploadError> {
        let url = Url::parse(endpoint).map_err(|_| UploadError::Endpoint)?;
        let authority = endpoint
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
            .ok_or(UploadError::Endpoint)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || authority.contains('@')
            || url.fragment().is_some()
            || endpoint.chars().any(char::is_whitespace)
        {
            return Err(UploadError::Endpoint);
        }
        self.endpoint = url;
        Ok(self)
    }

    pub fn with_upload_root(mut self, root: impl AsRef<Path>) -> Result<Self, UploadError> {
        open_directory(root.as_ref())?;
        self.upload_root = root.as_ref().to_owned();
        Ok(self)
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn upload_root(&self) -> &Path {
        &self.upload_root
    }
}
