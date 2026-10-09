//! Validated reply media for the WeCom bot long connection.
//!
//! The platform silently ignores `stream.msg_item` on long connections, so a
//! reply image is uploaded over the same connection
//! (`aibot_upload_media_init`/`_chunk`/`_finish`) and then sent as its own
//! `aibot_respond_msg`. The Host already validated the file it read from disk;
//! this module re-checks shape, bounds and digest so the transport never
//! uploads an unverified blob.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut as _, KeyIvInit as _, block_padding::NoPadding};
use md5::Md5;
use reqwest::{Client, redirect::Policy};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use std::time::Duration;

use crate::{
    GatewayError, INBOUND_FETCH_TIMEOUT_SECS, MAX_IMAGE_BYTES, MAX_IMAGE_COUNT,
    MAX_IMAGE_TOTAL_BASE64, MAX_INBOUND_FILE_BYTES, MAX_INBOUND_FILES, MAX_INBOUND_TOTAL_BYTES,
    MEDIA_CHUNK_BYTES,
};

pub(crate) struct ImageItem {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) md5: String,
}

impl ImageItem {
    pub(crate) fn chunk_count(&self) -> usize {
        self.bytes.len().div_ceil(MEDIA_CHUNK_BYTES).max(1)
    }

    pub(crate) fn chunk(&self, index: usize) -> &[u8] {
        let start = index * MEDIA_CHUNK_BYTES;
        let end = (start + MEDIA_CHUNK_BYTES).min(self.bytes.len());
        &self.bytes[start..end]
    }
}

/// Parse the `msg_item` images a callback response may carry.
///
/// Exactly `[{"msgtype":"image","image":{"base64":…,"md5":…}}]` is accepted;
/// every other shape is rejected instead of being forwarded to the platform.
pub(crate) fn reply_images(value: Option<&Value>) -> Result<Vec<ImageItem>, GatewayError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= MAX_IMAGE_COUNT)
        .ok_or(GatewayError::Invalid(
            "reply image count is outside the supported range",
        ))?;
    let mut total = 0usize;
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let item = item
            .as_object()
            .filter(|item| {
                item.len() == 2 && item.get("msgtype").and_then(Value::as_str) == Some("image")
            })
            .ok_or(GatewayError::UnsupportedMedia)?;
        let image = item
            .get("image")
            .and_then(Value::as_object)
            .filter(|image| image.len() == 2)
            .ok_or(GatewayError::UnsupportedMedia)?;
        let encoded = image
            .get("base64")
            .and_then(Value::as_str)
            .ok_or(GatewayError::UnsupportedMedia)?;
        let declared = image
            .get("md5")
            .and_then(Value::as_str)
            .filter(|digest| {
                digest.len() == 32
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(GatewayError::UnsupportedMedia)?;
        total = total.saturating_add(encoded.len());
        if total > MAX_IMAGE_TOTAL_BASE64 {
            return Err(GatewayError::Invalid(
                "reply images exceed the aggregate size limit",
            ));
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| GatewayError::Invalid("reply image is not valid base64"))?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(GatewayError::Invalid(
                "reply image size is outside the supported range",
            ));
        }
        let name = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "reply.png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "reply.jpg"
        } else {
            return Err(GatewayError::Invalid("reply image is not PNG or JPEG"));
        };
        if format!("{:x}", <Md5 as md5::Digest>::digest(&bytes)) != declared {
            return Err(GatewayError::Invalid(
                "reply image digest does not match its content",
            ));
        }
        parsed.push(ImageItem {
            name: name.into(),
            bytes,
            md5: declared.into(),
        });
    }
    Ok(parsed)
}

/// Persist validated items in the same shape [`reply_images`] accepts, so a
/// restarted gateway can resume an unsent reply image.
pub(crate) fn encode_images(items: &[ImageItem]) -> Result<Option<String>, GatewayError> {
    if items.is_empty() {
        return Ok(None);
    }
    let encoded: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({"msgtype":"image","image":{
                "base64": STANDARD.encode(&item.bytes),
                "md5": item.md5,
            }})
        })
        .collect();
    serde_json::to_string(&encoded)
        .map(Some)
        .map_err(|_| GatewayError::Ledger)
}

/// Stable identity of one reply-image send: same recipient, same conversation,
/// same bytes. A send that was never acknowledged therefore stays unconfirmed
/// across restarts instead of being repeated.
pub(crate) fn media_send_digest(
    sender_id: &str,
    conversation_id: &str,
    item: &ImageItem,
) -> Result<String, GatewayError> {
    let bound = serde_json::to_vec(&serde_json::json!([
        sender_id,
        conversation_id,
        format!("{:x}", <Sha256 as sha2::Digest>::digest(&item.bytes)),
    ]))
    .map_err(|_| GatewayError::Ledger)?;
    Ok(crate::protocol::digest(&bound))
}

/// Whether a response object carries a field this transport cannot deliver.
pub(crate) fn rejected_response_field(object: &Map<String, Value>) -> Option<&'static str> {
    [
        "attachments",
        "image",
        "file",
        "voice",
        "video",
        "audio",
        "stream",
    ]
    .into_iter()
    .find(|key| {
        object.get(*key).is_some_and(|value| {
            !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty())
        })
    })
}

// ---------------------------------------------------------------------------
// Inbound platform media
// ---------------------------------------------------------------------------

/// The platform's inbound message kinds that carry a downloadable payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InboundKind {
    Image,
    File,
    Voice,
}

impl InboundKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
            Self::Voice => "voice",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "image" => Some(Self::Image),
            "file" => Some(Self::File),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }

    fn fallback_extension(self) -> &'static str {
        match self {
            Self::Image => "img",
            Self::File | Self::Voice => "bin",
        }
    }
}

/// One inbound media item exactly as the callback frame described it.
///
/// The temporary URL and AES key stay inside the transport: the Host receives
/// only decrypted bytes through its existing attachment contract. Descriptors
/// are what the ledger stores, so recovery re-downloads from the platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboundMedia {
    pub(crate) kind: InboundKind,
    pub(crate) url: String,
    pub(crate) aes_key: String,
    pub(crate) name: Option<String>,
}

impl InboundMedia {
    pub(crate) fn descriptor(&self) -> Value {
        let mut value = json!({
            "kind": self.kind.as_str(),
            "url": self.url,
            "aeskey": self.aes_key,
        });
        if let Some(name) = &self.name {
            value["name"] = json!(name);
        }
        value
    }

    /// Read the platform's own media payload: `url` plus `aeskey` (the platform
    /// also spells it `aes_key`), with an optional filename hint.
    pub(crate) fn from_payload(
        kind: &str,
        payload: &Map<String, Value>,
    ) -> Result<Self, GatewayError> {
        let kind = InboundKind::parse(kind).ok_or(GatewayError::UnsupportedMedia)?;
        let url = payload
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| permitted_media_url(url))
            .ok_or(GatewayError::Invalid(
                "media callback URL is missing or unsupported",
            ))?;
        let aes_key = payload
            .get("aeskey")
            .or_else(|| payload.get("aes_key"))
            .and_then(Value::as_str)
            .filter(|key| valid_aes_key(key))
            .ok_or(GatewayError::Invalid("media callback key is missing"))?;
        let name = payload
            .get("filename")
            .or_else(|| payload.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 512)
            .map(str::to_owned);
        Ok(Self {
            kind,
            url: url.to_owned(),
            aes_key: aes_key.to_owned(),
            name,
        })
    }

    /// Read back one descriptor. Recovery replays these from the ledger, so the
    /// shape is re-validated rather than trusted.
    pub(crate) fn from_value(value: &Value) -> Result<Self, GatewayError> {
        let object = value
            .as_object()
            .filter(|object| {
                object
                    .keys()
                    .all(|key| matches!(key.as_str(), "kind" | "url" | "aeskey" | "name"))
            })
            .ok_or(GatewayError::Invalid("invalid inbound media descriptor"))?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .and_then(InboundKind::parse)
            .ok_or(GatewayError::UnsupportedMedia)?;
        let url = object
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| permitted_media_url(url))
            .ok_or(GatewayError::Invalid(
                "inbound media URL must be HTTPS or a loopback address",
            ))?;
        let aes_key = object
            .get("aeskey")
            .and_then(Value::as_str)
            .filter(|key| valid_aes_key(key))
            .ok_or(GatewayError::Invalid("inbound media key is invalid"))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 512)
            .map(str::to_owned);
        Ok(Self {
            kind,
            url: url.to_owned(),
            aes_key: aes_key.to_owned(),
            name,
        })
    }
}

/// The platform encrypts media with a base64 AES-256 key. Anything else cannot
/// be decrypted, so it is refused before a download is attempted.
fn valid_aes_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
        })
}

/// Media may come from the platform over TLS. Plain HTTP is accepted only for a
/// literal loopback address, so tests and local fixtures stay possible.
pub(crate) fn permitted_media_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return false;
    }
    match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => parsed.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        }),
        _ => false,
    }
}

fn redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= 3 || !permitted_media_url(attempt.url().as_str()) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

struct Fetched {
    bytes: Vec<u8>,
    name: Option<String>,
}

/// Downloads and decrypts inbound platform media.
pub(crate) struct MediaFetcher {
    client: Client,
}

impl MediaFetcher {
    pub(crate) fn new() -> Result<Self, GatewayError> {
        let client = Client::builder()
            .no_proxy()
            .redirect(redirect_policy())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(INBOUND_FETCH_TIMEOUT_SECS))
            .build()
            .map_err(|_| GatewayError::Media)?;
        Ok(Self { client })
    }

    /// Fetch every descriptor and render the Host's attachment shape. Count and
    /// aggregate bounds are enforced here, before the callback is admitted.
    pub(crate) async fn fetch_all(
        &self,
        descriptors: &[Value],
    ) -> Result<Vec<Value>, GatewayError> {
        if descriptors.len() > MAX_INBOUND_FILES {
            return Err(GatewayError::Invalid("too many inbound attachments"));
        }
        let mut total = 0usize;
        let mut uploaded = Vec::with_capacity(descriptors.len());
        for (index, value) in descriptors.iter().enumerate() {
            let media = InboundMedia::from_value(value)?;
            let fetched = self.fetch(&media).await?;
            total = total.saturating_add(fetched.bytes.len());
            if total > MAX_INBOUND_TOTAL_BYTES {
                return Err(GatewayError::Invalid(
                    "inbound attachments exceed the event size limit",
                ));
            }
            let name = attachment_name(
                fetched.name.as_deref().or(media.name.as_deref()),
                media.kind,
                index + 1,
                &fetched.bytes,
            );
            let media_type = attachment_media_type(media.kind, &fetched.bytes)?;
            uploaded.push(json!({
                "name": name,
                "data_base64": STANDARD.encode(&fetched.bytes),
                "media_type": media_type,
            }));
        }
        Ok(uploaded)
    }

    async fn fetch(&self, media: &InboundMedia) -> Result<Fetched, GatewayError> {
        let mut response = self
            .client
            .get(&media.url)
            .send()
            .await
            .map_err(|_| GatewayError::Media)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(GatewayError::Media);
        }
        // The decrypted payload can exceed the ciphertext by at most one block.
        let limit = MAX_INBOUND_FILE_BYTES + 16;
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(GatewayError::Invalid(
                "inbound media exceeds the per-file limit",
            ));
        }
        let name = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(disposition_filename);
        let mut encrypted = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| GatewayError::Media)? {
            if encrypted.len() + chunk.len() > limit {
                return Err(GatewayError::Invalid(
                    "inbound media exceeds the per-file limit",
                ));
            }
            encrypted.extend_from_slice(&chunk);
        }
        if encrypted.is_empty() {
            return Err(GatewayError::Media);
        }
        let bytes = decrypt_media(&encrypted, &media.aes_key)?;
        if bytes.is_empty() || bytes.len() > MAX_INBOUND_FILE_BYTES {
            return Err(GatewayError::Invalid(
                "inbound media size is outside the supported range",
            ));
        }
        Ok(Fetched { bytes, name })
    }
}

/// AES-256-CBC with the platform's exact conventions: the base64 key is padded
/// before decoding, the IV is its first 16 bytes, the ciphertext is zero-padded
/// to a block multiple, and PKCS#7 is removed manually.
pub(crate) fn decrypt_media(encrypted: &[u8], aes_key: &str) -> Result<Vec<u8>, GatewayError> {
    let mut padded = aes_key.to_owned();
    if !padded.len().is_multiple_of(4) {
        padded.push_str(&"=".repeat(4 - padded.len() % 4));
    }
    let key = STANDARD
        .decode(padded.as_bytes())
        .map_err(|_| GatewayError::Media)?;
    if key.len() != 32 {
        return Err(GatewayError::Media);
    }
    let mut buffer = encrypted.to_vec();
    let remainder = buffer.len() % 16;
    if remainder != 0 {
        buffer.resize(buffer.len() + (16 - remainder), 0);
    }
    let cipher = cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, &key[..16])
        .map_err(|_| GatewayError::Media)?;
    let plain = cipher
        .decrypt_padded_mut::<NoPadding>(&mut buffer)
        .map_err(|_| GatewayError::Media)?;
    let pad = *plain.last().ok_or(GatewayError::Media)? as usize;
    if !(1..=32).contains(&pad) || pad > plain.len() {
        return Err(GatewayError::Media);
    }
    if plain[plain.len() - pad..]
        .iter()
        .any(|byte| *byte as usize != pad)
    {
        return Err(GatewayError::Media);
    }
    Ok(plain[..plain.len() - pad].to_vec())
}

/// The declared media type is what the Host re-checks against the bytes, so an
/// unsupported image format is refused here instead of arriving as an image.
fn attachment_media_type(kind: InboundKind, bytes: &[u8]) -> Result<Option<String>, GatewayError> {
    let sniffed = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    };
    match (kind, sniffed) {
        (InboundKind::Image, Some(mime)) => Ok(Some(mime.to_owned())),
        (InboundKind::Image, None) => Err(GatewayError::UnsupportedMedia),
        _ => Ok(None),
    }
}

/// A filename the Host's attachment contract accepts: one path component, no
/// control characters, bounded, and never empty.
pub(crate) fn attachment_name(
    hint: Option<&str>,
    kind: InboundKind,
    index: usize,
    bytes: &[u8],
) -> String {
    let candidate = hint
        .and_then(|value| value.rsplit(['/', '\\']).next())
        .unwrap_or_default();
    let mut name: String = candidate
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
                '_'
            } else {
                character
            }
        })
        .collect();
    name = name.trim_matches([' ', '.']).to_owned();
    if name.chars().count() > 180 {
        name = name.chars().take(180).collect();
    }
    if name.is_empty() {
        let extension = match attachment_media_type(kind, bytes).ok().flatten().as_deref() {
            Some("image/png") => "png",
            Some("image/jpeg") => "jpg",
            Some("image/webp") => "webp",
            _ => kind.fallback_extension(),
        };
        name = format!("attachment-{index}.{extension}");
    }
    name
}

/// `filename*=UTF-8''…` first (RFC 5987), then a plain `filename=` parameter.
pub(crate) fn disposition_filename(value: &str) -> Option<String> {
    let lowered = value.to_ascii_lowercase();
    if let Some(position) = lowered.find("filename*=utf-8''") {
        let rest = &value[position + "filename*=utf-8''".len()..];
        let raw = rest.split([';', ',']).next().unwrap_or_default().trim();
        let decoded = percent_decode(raw);
        if !decoded.is_empty() {
            return Some(decoded);
        }
    }
    let position = lowered.find("filename=")?;
    let rest = &value[position + "filename=".len()..];
    let raw = rest.split(';').next().unwrap_or_default().trim();
    let raw = raw.trim_matches('"');
    let decoded = percent_decode(raw);
    (!decoded.is_empty()).then_some(decoded)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
