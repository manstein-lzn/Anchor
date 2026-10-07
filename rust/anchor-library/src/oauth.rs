use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use rustix::fs::{AtFlags, FlockOperation, Mode, OFlags, flock, openat, renameat, unlinkat};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{InstallError, filesystem};

const OAUTH_FORMAT: u32 = 1;
const RECORD_NAME: &str = "authorization.json";
const LOCK_NAME: &str = "refresh.lock";
const MAX_BINDING_COMPONENT_BYTES: usize = 64;
const MAX_METADATA_VALUE_BYTES: usize = 256;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthBinding {
    provider: String,
    plugin: String,
    server: String,
    owner: String,
}

impl<'de> Deserialize<'de> for OAuthBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            provider: String,
            plugin: String,
            server: String,
            owner: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.provider, wire.plugin, wire.server, wire.owner)
            .map_err(serde::de::Error::custom)
    }
}

impl OAuthBinding {
    pub fn new(
        provider: impl Into<String>,
        plugin: impl Into<String>,
        server: impl Into<String>,
        owner: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        let binding = Self {
            provider: provider.into(),
            plugin: plugin.into(),
            server: server.into(),
            owner: owner.into(),
        };
        for value in [
            &binding.provider,
            &binding.plugin,
            &binding.server,
            &binding.owner,
        ] {
            validate_binding_component(value)?;
        }
        Ok(binding)
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthAuthorizationMetadata {
    token_endpoint: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl OAuthAuthorizationMetadata {
    pub fn new(token_endpoint: impl Into<String>) -> Result<Self, OAuthError> {
        let token_endpoint = token_endpoint.into();
        validate_token_endpoint(&token_endpoint)?;
        Ok(Self {
            token_endpoint,
            client_id: None,
            scope: None,
        })
    }

    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Result<Self, OAuthError> {
        let client_id = client_id.into();
        validate_metadata_value(&client_id)?;
        self.client_id = Some(client_id);
        Ok(self)
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Result<Self, OAuthError> {
        let scope = scope.into();
        validate_metadata_value(&scope)?;
        self.scope = Some(scope);
        Ok(self)
    }

    pub fn token_endpoint(&self) -> &str {
        &self.token_endpoint
    }

    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthSecret(String);

impl OAuthSecret {
    pub fn new(value: impl Into<String>) -> Result<Self, OAuthError> {
        let value = value.into();
        validate_secret(&value)?;
        Ok(Self(value))
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OAuthSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl std::fmt::Display for OAuthSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthTokenResponse {
    access_token: OAuthSecret,
    refresh_token: Option<OAuthSecret>,
    token_type: Option<String>,
    scope: Option<String>,
    expires_in: Option<u64>,
}

impl OAuthTokenResponse {
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    ) -> Result<Self, OAuthError> {
        Ok(Self {
            access_token: OAuthSecret::new(access_token)?,
            refresh_token: refresh_token.map(OAuthSecret::new).transpose()?,
            token_type: None,
            scope: None,
            expires_in,
        })
    }

    pub fn with_token_type(mut self, token_type: impl Into<String>) -> Result<Self, OAuthError> {
        let token_type = token_type.into();
        validate_metadata_value(&token_type)?;
        self.token_type = Some(token_type);
        Ok(self)
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Result<Self, OAuthError> {
        let scope = scope.into();
        validate_metadata_value(&scope)?;
        self.scope = Some(scope);
        Ok(self)
    }

    pub fn access_token(&self) -> &OAuthSecret {
        &self.access_token
    }

    pub fn refresh_token(&self) -> Option<&OAuthSecret> {
        self.refresh_token.as_ref()
    }

    pub fn expires_in(&self) -> Option<u64> {
        self.expires_in
    }
}

impl std::fmt::Debug for OAuthTokenResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthTokenResponse")
            .field("access_token", &self.access_token)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthAuthorization {
    binding: OAuthBinding,
    metadata: OAuthAuthorizationMetadata,
    access_token: OAuthSecret,
    refresh_token: Option<OAuthSecret>,
    token_type: String,
    issued_at: u64,
    expires_at: Option<u64>,
}

impl OAuthAuthorization {
    pub fn binding(&self) -> &OAuthBinding {
        &self.binding
    }

    pub fn metadata(&self) -> &OAuthAuthorizationMetadata {
        &self.metadata
    }

    pub fn access_token(&self) -> &OAuthSecret {
        &self.access_token
    }

    pub fn refresh_token(&self) -> Option<&OAuthSecret> {
        self.refresh_token.as_ref()
    }

    pub fn token_type(&self) -> &str {
        &self.token_type
    }

    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    pub fn is_expired_at(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|expires_at| now >= expires_at)
    }
}

impl std::fmt::Debug for OAuthAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuthorization")
            .field("binding", &self.binding)
            .field("metadata", &self.metadata)
            .field("access_token", &self.access_token)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("token_type", &self.token_type)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub struct OAuthRefreshRequest<'a> {
    binding: &'a OAuthBinding,
    metadata: &'a OAuthAuthorizationMetadata,
    refresh_token: &'a OAuthSecret,
}

impl<'a> OAuthRefreshRequest<'a> {
    pub fn binding(&self) -> &OAuthBinding {
        self.binding
    }

    pub fn metadata(&self) -> &OAuthAuthorizationMetadata {
        self.metadata
    }

    pub fn refresh_token(&self) -> &OAuthSecret {
        self.refresh_token
    }
}

impl std::fmt::Debug for OAuthRefreshRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthRefreshRequest")
            .field("binding", &self.binding)
            .field("metadata", &self.metadata)
            .field("refresh_token", &self.refresh_token)
            .finish()
    }
}

pub trait OAuthRefreshTransport: Send + Sync {
    fn refresh(
        &self,
        request: &OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, OAuthTransportError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthTransportError {
    #[error("OAuth provider refresh request failed")]
    RequestFailed,
    #[error("OAuth provider revoked authorization")]
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    #[error("OAuth authorization is not available")]
    NotAuthorized,
    #[error("OAuth authorization metadata is invalid")]
    InvalidMetadata,
    #[error("OAuth token response is invalid")]
    InvalidResponse,
    #[error("OAuth authorization has no refresh token")]
    MissingRefreshToken,
    #[error("OAuth authorization refresh failed and was revoked locally")]
    RefreshFailed,
    #[error("OAuth authorization was revoked")]
    Revoked,
    #[error("OAuth token store operation failed")]
    Storage,
}

#[derive(Debug, Clone)]
pub struct FileOAuthTokenStore {
    root: PathBuf,
}

impl FileOAuthTokenStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load(&self, binding: &OAuthBinding) -> Result<Option<OAuthAuthorization>, OAuthError> {
        let locked = self.lock(binding)?;
        self.load_locked(&locked, binding)
    }

    pub fn save(&self, authorization: &OAuthAuthorization) -> Result<(), OAuthError> {
        let locked = self.lock(&authorization.binding)?;
        self.save_locked(&locked, authorization)
    }

    pub fn revoke(&self, binding: &OAuthBinding) -> Result<(), OAuthError> {
        let locked = self.lock(binding)?;
        self.revoke_locked(&locked)
    }

    fn lock(&self, binding: &OAuthBinding) -> Result<LockedStore, OAuthError> {
        let (directory_path, directory) = self.binding_directory(binding)?;
        let lock = File::from(
            openat(
                &directory,
                LOCK_NAME,
                OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| OAuthError::Storage)?,
        );
        ensure_private_file(&lock)?;
        flock(&lock, FlockOperation::LockExclusive).map_err(|_| OAuthError::Storage)?;
        Ok(LockedStore {
            directory_path,
            directory,
            _lock: lock,
        })
    }

    fn binding_directory(&self, binding: &OAuthBinding) -> Result<(PathBuf, File), OAuthError> {
        let (root_path, root) = filesystem::directory(&self.root, true).map_err(storage_error)?;
        drop(root);
        let (oauth_path, oauth) =
            filesystem::directory(&root_path.join("oauth"), true).map_err(storage_error)?;
        ensure_private_directory(&oauth)?;
        let binding_path = oauth_path
            .join(hex_component(binding.provider()))
            .join(hex_component(binding.plugin()))
            .join(hex_component(binding.server()))
            .join(hex_component(binding.owner()));
        let (binding_path, binding_directory) =
            filesystem::directory(&binding_path, true).map_err(storage_error)?;
        ensure_private_directory(&binding_directory)?;
        Ok((binding_path, binding_directory))
    }

    fn load_locked(
        &self,
        locked: &LockedStore,
        expected: &OAuthBinding,
    ) -> Result<Option<OAuthAuthorization>, OAuthError> {
        let Some(mut file) = open_record(&locked.directory)? else {
            return Ok(None);
        };
        let size = file.metadata().map_err(|_| OAuthError::Storage)?.len();
        if size > MAX_RECORD_BYTES {
            return Err(OAuthError::Storage);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|_| OAuthError::Storage)?;
        let stored: StoredAuthorization =
            serde_json::from_slice(&bytes).map_err(|_| OAuthError::Storage)?;
        if stored.format != OAUTH_FORMAT || stored.binding != *expected {
            return Err(OAuthError::Storage);
        }
        Ok(Some(stored.into_authorization()?))
    }

    fn save_locked(
        &self,
        locked: &LockedStore,
        authorization: &OAuthAuthorization,
    ) -> Result<(), OAuthError> {
        let bytes = serde_json::to_vec(&StoredAuthorization::from(authorization))
            .map_err(|_| OAuthError::Storage)?;
        let temporary = tempfile::NamedTempFile::new_in(&locked.directory_path)
            .map_err(|_| OAuthError::Storage)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| OAuthError::Storage)?;
        temporary
            .as_file()
            .set_len(0)
            .map_err(|_| OAuthError::Storage)?;
        let mut file = temporary.as_file();
        file.write_all(&bytes).map_err(|_| OAuthError::Storage)?;
        file.sync_all().map_err(|_| OAuthError::Storage)?;
        let name = temporary_file_name(&temporary)?;
        renameat(&locked.directory, name, &locked.directory, RECORD_NAME)
            .map_err(|_| OAuthError::Storage)?;
        locked.directory.sync_all().map_err(|_| OAuthError::Storage)
    }

    fn revoke_locked(&self, locked: &LockedStore) -> Result<(), OAuthError> {
        match unlinkat(&locked.directory, RECORD_NAME, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {
                locked.directory.sync_all().map_err(|_| OAuthError::Storage)
            }
            Err(_) => Err(OAuthError::Storage),
        }
    }
}

struct LockedStore {
    directory_path: PathBuf,
    directory: File,
    _lock: File,
}

fn open_record(directory: &File) -> Result<Option<File>, OAuthError> {
    match openat(
        directory,
        RECORD_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => {
            let file = File::from(file);
            ensure_private_file(&file)?;
            Ok(Some(file))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(_) => Err(OAuthError::Storage),
    }
}

fn temporary_file_name(temporary: &tempfile::NamedTempFile) -> Result<&str, OAuthError> {
    temporary
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(OAuthError::Storage)
}

fn ensure_private_directory(file: &File) -> Result<(), OAuthError> {
    let metadata = file.metadata().map_err(|_| OAuthError::Storage)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(OAuthError::Storage);
    }
    Ok(())
}

fn ensure_private_file(file: &File) -> Result<(), OAuthError> {
    let metadata = file.metadata().map_err(|_| OAuthError::Storage)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(OAuthError::Storage);
    }
    Ok(())
}

fn storage_error(error: InstallError) -> OAuthError {
    let _ = error;
    OAuthError::Storage
}

fn hex_component(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .flat_map(|byte| [hex_digit(byte >> 4), hex_digit(byte & 0x0f)])
        .collect()
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + value - 10) as char,
        _ => unreachable!(),
    }
}

fn validate_binding_component(value: &str) -> Result<(), OAuthError> {
    if value.is_empty()
        || value.len() > MAX_BINDING_COMPONENT_BYTES
        || value.chars().any(char::is_control)
    {
        Err(OAuthError::InvalidMetadata)
    } else {
        Ok(())
    }
}

fn validate_metadata_value(value: &str) -> Result<(), OAuthError> {
    if value.is_empty()
        || value.len() > MAX_METADATA_VALUE_BYTES
        || value.chars().any(char::is_control)
    {
        Err(OAuthError::InvalidMetadata)
    } else {
        Ok(())
    }
}

fn validate_token_endpoint(endpoint: &str) -> Result<(), OAuthError> {
    if endpoint.len() > MAX_METADATA_VALUE_BYTES {
        return Err(OAuthError::InvalidMetadata);
    }
    let parsed = Url::parse(endpoint).map_err(|_| OAuthError::InvalidMetadata)?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_none()
    {
        return Err(OAuthError::InvalidMetadata);
    }
    let secure = parsed.scheme() == "https";
    let loopback = parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !secure && !loopback {
        return Err(OAuthError::InvalidMetadata);
    }
    Ok(())
}

fn validate_metadata(metadata: &OAuthAuthorizationMetadata) -> Result<(), OAuthError> {
    validate_token_endpoint(&metadata.token_endpoint)?;
    if let Some(client_id) = &metadata.client_id {
        validate_metadata_value(client_id)?;
    }
    if let Some(scope) = &metadata.scope {
        validate_metadata_value(scope)?;
    }
    Ok(())
}

fn validate_secret(value: &str) -> Result<(), OAuthError> {
    if value.is_empty() || value.len() > MAX_SECRET_BYTES || value.chars().any(char::is_control) {
        Err(OAuthError::InvalidResponse)
    } else {
        Ok(())
    }
}

fn expires_at(now: u64, expires_in: Option<u64>) -> Result<Option<u64>, OAuthError> {
    expires_in
        .map(|duration| now.checked_add(duration).ok_or(OAuthError::InvalidResponse))
        .transpose()
}

fn current_time() -> Result<u64, OAuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OAuthError::Storage)
        .map(|duration| duration.as_secs())
}

impl OAuthAuthorization {
    fn from_response(
        binding: OAuthBinding,
        metadata: OAuthAuthorizationMetadata,
        response: OAuthTokenResponse,
        now: u64,
    ) -> Result<Self, OAuthError> {
        validate_metadata(&metadata)?;
        let token_type = response.token_type.unwrap_or_else(|| "Bearer".to_owned());
        validate_metadata_value(&token_type)?;
        let mut metadata = metadata;
        if let Some(scope) = response.scope {
            validate_metadata_value(&scope)?;
            metadata.scope = Some(scope);
        }
        Ok(Self {
            binding,
            metadata,
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            token_type,
            issued_at: now,
            expires_at: expires_at(now, response.expires_in)?,
        })
    }

    fn refreshed(&self, response: OAuthTokenResponse, now: u64) -> Result<Self, OAuthError> {
        let token_type = response
            .token_type
            .unwrap_or_else(|| self.token_type.clone());
        validate_metadata_value(&token_type)?;
        let mut metadata = self.metadata.clone();
        if let Some(scope) = response.scope {
            validate_metadata_value(&scope)?;
            metadata.scope = Some(scope);
        }
        Ok(Self {
            binding: self.binding.clone(),
            metadata,
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .or_else(|| self.refresh_token.clone()),
            token_type,
            issued_at: now,
            expires_at: expires_at(now, response.expires_in)?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAuthorization {
    format: u32,
    binding: OAuthBinding,
    metadata: OAuthAuthorizationMetadata,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    token_type: String,
    issued_at: u64,
    expires_at: Option<u64>,
}

impl From<&OAuthAuthorization> for StoredAuthorization {
    fn from(authorization: &OAuthAuthorization) -> Self {
        Self {
            format: OAUTH_FORMAT,
            binding: authorization.binding.clone(),
            metadata: authorization.metadata.clone(),
            access_token: authorization.access_token.expose_secret().to_owned(),
            refresh_token: authorization
                .refresh_token
                .as_ref()
                .map(|token| token.expose_secret().to_owned()),
            token_type: authorization.token_type.clone(),
            issued_at: authorization.issued_at,
            expires_at: authorization.expires_at,
        }
    }
}

impl StoredAuthorization {
    fn into_authorization(self) -> Result<OAuthAuthorization, OAuthError> {
        validate_metadata(&self.metadata)?;
        validate_metadata_value(&self.token_type)?;
        Ok(OAuthAuthorization {
            binding: OAuthBinding::new(
                self.binding.provider,
                self.binding.plugin,
                self.binding.server,
                self.binding.owner,
            )?,
            metadata: self.metadata,
            access_token: OAuthSecret::new(self.access_token)?,
            refresh_token: self.refresh_token.map(OAuthSecret::new).transpose()?,
            token_type: self.token_type,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }
}

pub struct OAuthClient<T> {
    store: FileOAuthTokenStore,
    transport: T,
}

impl<T> OAuthClient<T>
where
    T: OAuthRefreshTransport,
{
    pub fn new(store: FileOAuthTokenStore, transport: T) -> Self {
        Self { store, transport }
    }

    pub fn authorize(
        &self,
        binding: OAuthBinding,
        metadata: OAuthAuthorizationMetadata,
        response: OAuthTokenResponse,
        now: u64,
    ) -> Result<(), OAuthError> {
        let authorization = OAuthAuthorization::from_response(binding, metadata, response, now)?;
        self.store.save(&authorization)
    }

    pub fn access_token(&self, binding: &OAuthBinding) -> Result<OAuthSecret, OAuthError> {
        self.access_token_at(binding, current_time()?)
    }

    pub fn access_token_at(
        &self,
        binding: &OAuthBinding,
        now: u64,
    ) -> Result<OAuthSecret, OAuthError> {
        let locked = self.store.lock(binding)?;
        let Some(authorization) = self.store.load_locked(&locked, binding)? else {
            return Err(OAuthError::NotAuthorized);
        };
        if !authorization.is_expired_at(now) {
            return Ok(authorization.access_token.clone());
        }
        let Some(refresh_token) = authorization.refresh_token.clone() else {
            self.store.revoke_locked(&locked)?;
            return Err(OAuthError::MissingRefreshToken);
        };
        let request = OAuthRefreshRequest {
            binding: &authorization.binding,
            metadata: &authorization.metadata,
            refresh_token: &refresh_token,
        };
        let response = match self.transport.refresh(&request) {
            Ok(response) => response,
            Err(OAuthTransportError::Revoked) => {
                self.store.revoke_locked(&locked)?;
                return Err(OAuthError::Revoked);
            }
            Err(OAuthTransportError::RequestFailed) => {
                self.store.revoke_locked(&locked)?;
                return Err(OAuthError::RefreshFailed);
            }
        };
        let refreshed = match authorization.refreshed(response, now) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                self.store.revoke_locked(&locked)?;
                return Err(error);
            }
        };
        if refreshed.refresh_token.is_none() {
            self.store.revoke_locked(&locked)?;
            return Err(OAuthError::InvalidResponse);
        }
        if let Err(error) = self.store.save_locked(&locked, &refreshed) {
            let _ = self.store.revoke_locked(&locked);
            return Err(error);
        }
        Ok(refreshed.access_token)
    }

    pub fn revoke(&self, binding: &OAuthBinding) -> Result<(), OAuthError> {
        self.store.revoke(binding)
    }

    pub fn store(&self) -> &FileOAuthTokenStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use tempfile::tempdir;

    use super::*;

    #[derive(Clone)]
    struct CountingTransport {
        calls: Arc<AtomicUsize>,
        response: OAuthTokenResponse,
    }

    impl OAuthRefreshTransport for CountingTransport {
        fn refresh(
            &self,
            request: &OAuthRefreshRequest<'_>,
        ) -> Result<OAuthTokenResponse, OAuthTransportError> {
            assert_eq!(request.refresh_token().expose_secret(), "refresh-secret");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    fn binding(owner: &str) -> OAuthBinding {
        OAuthBinding::new("provider", "plugin", "server", owner).unwrap()
    }

    fn metadata() -> OAuthAuthorizationMetadata {
        OAuthAuthorizationMetadata::new("https://provider.example/token")
            .unwrap()
            .with_client_id("client")
            .unwrap()
            .with_scope("read")
            .unwrap()
    }

    fn response(
        access: &str,
        refresh: Option<&str>,
        expires_in: Option<u64>,
    ) -> OAuthTokenResponse {
        OAuthTokenResponse::new(access, refresh.map(str::to_owned), expires_in).unwrap()
    }

    #[test]
    fn stores_isolated_private_records_without_secret_debug_output() {
        let root = tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let client = OAuthClient::new(
            FileOAuthTokenStore::new(root.path()),
            CountingTransport {
                calls,
                response: response("new-access", Some("new-refresh"), Some(100)),
            },
        );
        let first = binding("owner/a");
        let second = binding("owner-b");
        client
            .authorize(
                first.clone(),
                metadata(),
                response("access-secret", Some("refresh-secret"), Some(60)),
                10,
            )
            .unwrap();
        assert_eq!(
            client.access_token_at(&first, 20).unwrap().expose_secret(),
            "access-secret"
        );
        assert!(matches!(
            client.access_token_at(&second, 20),
            Err(OAuthError::NotAuthorized)
        ));
        let authorization = client.store().load(&first).unwrap().unwrap();
        let debug = format!("{authorization:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
        assert_eq!(format!("{}", authorization.access_token()), "[redacted]");
        let record = root
            .path()
            .join("oauth")
            .join(hex_component(first.provider()))
            .join(hex_component(first.plugin()))
            .join(hex_component(first.server()))
            .join(hex_component(first.owner()))
            .join(RECORD_NAME);
        let mode = fs::metadata(record).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn expired_refresh_is_single_winner_across_clients() {
        let root = tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let setup = OAuthClient::new(
            FileOAuthTokenStore::new(root.path()),
            CountingTransport {
                calls: calls.clone(),
                response: response("refreshed-access", Some("refreshed-refresh"), Some(100)),
            },
        );
        let target = binding("owner");
        setup
            .authorize(
                target.clone(),
                metadata(),
                response("expired-access", Some("refresh-secret"), Some(1)),
                10,
            )
            .unwrap();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = FileOAuthTokenStore::new(root.path());
            let transport = CountingTransport {
                calls: calls.clone(),
                response: response("refreshed-access", Some("refreshed-refresh"), Some(100)),
            };
            let target = target.clone();
            workers.push(thread::spawn(move || {
                OAuthClient::new(store, transport)
                    .access_token_at(&target, 20)
                    .unwrap()
                    .expose_secret()
                    .to_owned()
            }));
        }
        let values = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(values, vec!["refreshed-access"; 8]);
    }

    #[test]
    fn revoked_or_failed_refresh_removes_authorization() {
        struct FailingTransport {
            error: OAuthTransportError,
        }

        impl OAuthRefreshTransport for FailingTransport {
            fn refresh(
                &self,
                _request: &OAuthRefreshRequest<'_>,
            ) -> Result<OAuthTokenResponse, OAuthTransportError> {
                Err(self.error)
            }
        }

        for error in [
            OAuthTransportError::Revoked,
            OAuthTransportError::RequestFailed,
        ] {
            let root = tempdir().unwrap();
            let client = OAuthClient::new(
                FileOAuthTokenStore::new(root.path()),
                FailingTransport { error },
            );
            let target = binding("owner");
            client
                .authorize(
                    target.clone(),
                    metadata(),
                    response("expired-access", Some("refresh-secret"), Some(1)),
                    10,
                )
                .unwrap();
            let expected = match error {
                OAuthTransportError::Revoked => OAuthError::Revoked,
                OAuthTransportError::RequestFailed => OAuthError::RefreshFailed,
            };
            assert_eq!(client.access_token_at(&target, 20), Err(expected));
            assert!(client.store().load(&target).unwrap().is_none());
            assert!(matches!(
                client.access_token_at(&target, 20),
                Err(OAuthError::NotAuthorized)
            ));
        }
    }

    #[test]
    fn invalid_endpoints_and_missing_refresh_fail_closed() {
        for endpoint in [
            "http://public.example/token",
            "https://user:secret@provider.example/token",
            "https://provider.example/token?secret=token",
        ] {
            assert!(matches!(
                OAuthAuthorizationMetadata::new(endpoint),
                Err(OAuthError::InvalidMetadata)
            ));
        }
        assert!(OAuthAuthorizationMetadata::new("http://127.0.0.1:4321/token").is_ok());

        let root = tempdir().unwrap();
        let client = OAuthClient::new(
            FileOAuthTokenStore::new(root.path()),
            CountingTransport {
                calls: Arc::new(AtomicUsize::new(0)),
                response: response("unused", Some("unused"), Some(100)),
            },
        );
        let target = binding("owner");
        client
            .authorize(
                target.clone(),
                metadata(),
                response("expired", None, Some(1)),
                10,
            )
            .unwrap();
        assert!(matches!(
            client.access_token_at(&target, 20),
            Err(OAuthError::MissingRefreshToken)
        ));
        assert!(client.store().load(&target).unwrap().is_none());
    }

    #[test]
    fn deserializing_invalid_binding_fails_before_storage_can_use_it() {
        for value in [
            serde_json::json!({
                "provider":"",
                "plugin":"plugin",
                "server":"server",
                "owner":"owner"
            }),
            serde_json::json!({
                "provider":"provider",
                "plugin":"plugin",
                "server":"server",
                "owner":"owner\u{0000}"
            }),
            serde_json::json!({
                "provider":"provider",
                "plugin":"plugin",
                "server":"server",
                "owner":"owner",
                "unexpected":"field"
            }),
        ] {
            assert!(serde_json::from_value::<OAuthBinding>(value).is_err());
        }
    }
}
