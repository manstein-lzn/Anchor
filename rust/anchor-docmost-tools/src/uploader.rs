use std::{path::PathBuf, time::Duration};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderValue},
    multipart::{Form, Part},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Config, assets};

pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const QUOTE_FILENAME: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UploadError {
    #[error("pageId and attachmentId must be UUIDs")]
    Uuid,
    #[error("path must refer to a regular file under /in/publish/assets without symlinks")]
    Path,
    #[error("only SVG, PNG, JPEG, and WebP report images can be uploaded")]
    Mime,
    #[error("image must be non-empty and at most 20 MiB")]
    Size,
    #[error("DOCMOST_API_KEY is not configured")]
    MissingKey,
    #[error("DOCMOST_API_KEY is not a valid authorization value")]
    InvalidKey,
    #[error("endpoint must be an absolute HTTP(S) URL without userinfo or fragment")]
    Endpoint,
    #[error("could not initialize Docmost HTTP transport")]
    Client,
    #[error("could not read report image")]
    Read,
    #[error("Docmost image upload failed: HTTP {0}; upload was not retried")]
    Http(u16),
    #[error("Docmost image upload failed: transport error; upload was not retried")]
    Transport,
    #[error("Docmost image upload failed: invalid JSON response")]
    Json,
    #[error("Docmost returned attachment metadata for a different page or file type")]
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub attachment_id: String,
    pub file_name: String,
    pub url: String,
    pub mime_type: String,
    pub page_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadResponse {
    id: String,
    file_name: String,
    mime_type: String,
    page_id: String,
}

#[derive(Clone)]
pub struct Uploader {
    config: Config,
    client: Client,
    authorization: Option<HeaderValue>,
}

impl Uploader {
    pub fn new(config: Config) -> Result<Self, UploadError> {
        let authorization = if config.api_key.is_empty() {
            None
        } else {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", config.api_key))
                .map_err(|_| UploadError::InvalidKey)?;
            value.set_sensitive(true);
            Some(value)
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| UploadError::Client)?;
        Ok(Self {
            config,
            client,
            authorization,
        })
    }

    pub async fn upload(
        &self,
        path: &str,
        page_id: &str,
        attachment_id: Option<&str>,
    ) -> Result<Attachment, UploadError> {
        uuid::Uuid::parse_str(page_id).map_err(|_| UploadError::Uuid)?;
        if let Some(existing) = attachment_id {
            uuid::Uuid::parse_str(existing).map_err(|_| UploadError::Uuid)?;
        }
        let root = self.config.upload_root.clone();
        let path = PathBuf::from(path);
        let image = tokio::task::spawn_blocking(move || assets::read_image(&root, &path))
            .await
            .map_err(|_| UploadError::Read)??;
        let authorization = self.authorization.as_ref().ok_or(UploadError::MissingKey)?;
        let mut form = Form::new().percent_encode_noop();
        if let Some(existing) = attachment_id {
            form = form.text("attachmentId", existing.to_owned());
        }
        form = form.text("pageId", page_id.to_owned()).part(
            "file",
            Part::bytes(image.bytes)
                .file_name(image.name)
                .mime_str(image.mime)
                .map_err(|_| UploadError::Mime)?,
        );
        let mut response = self
            .client
            .post(self.config.endpoint.clone())
            .header(AUTHORIZATION, authorization.clone())
            .multipart(form)
            .send()
            .await
            .map_err(|_| UploadError::Transport)?;
        if !response.status().is_success() {
            return Err(UploadError::Http(response.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UploadError::Transport)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(UploadError::Json);
            }
            body.extend_from_slice(&chunk);
        }
        let metadata: UploadResponse =
            serde_json::from_slice(&body).map_err(|_| UploadError::Json)?;
        if metadata.page_id != page_id || !assets::supported_mime(&metadata.mime_type) {
            return Err(UploadError::Metadata);
        }
        let url = format!(
            "/api/files/{}/{}",
            metadata.id,
            utf8_percent_encode(&metadata.file_name, QUOTE_FILENAME)
        );
        Ok(Attachment {
            attachment_id: metadata.id,
            file_name: metadata.file_name,
            url,
            mime_type: metadata.mime_type,
            page_id: metadata.page_id,
        })
    }
}
