use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustix::{
    fs::{AtFlags, FlockOperation, Mode, OFlags, flock, openat, renameat, unlinkat},
    rand::{GetRandomFlags, getrandom},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use crate::{InstallError, filesystem};

const OAUTH_FORMAT: u32 = 1;
const RECORD_NAME: &str = "authorization.json";
const LOCK_NAME: &str = "refresh.lock";
const PENDING_DIRECTORY: &str = "pending";
const PENDING_LOCK_NAME: &str = "pending.lock";
const MAX_BINDING_COMPONENT_BYTES: usize = 64;
const MAX_METADATA_VALUE_BYTES: usize = 256;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const OAUTH_TRANSACTION_RANDOM_BYTES: usize = 32;
const MAX_OAUTH_ENDPOINT_BYTES: usize = 2048;
const MAX_REDIRECT_URI_BYTES: usize = 2048;
const OAUTH_AUTHORIZATION_PARAMETERS: [&str; 8] = [
    "response_type",
    "client_id",
    "redirect_uri",
    "state",
    "code_challenge",
    "code_challenge_method",
    "scope",
    "resource",
];

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

pub struct OAuthAuthorizationTransaction {
    binding: OAuthBinding,
    authorization_endpoint: Url,
    redirect_uri: String,
    resource: Option<String>,
    client_id: String,
    scope: Option<String>,
    state: OAuthSecret,
    code_verifier: OAuthSecret,
    expires_at: u64,
}

impl OAuthAuthorizationTransaction {
    pub fn new(
        binding: OAuthBinding,
        authorization_endpoint: impl AsRef<str>,
        redirect_uri: impl Into<String>,
        client_id: impl Into<String>,
        scope: Option<String>,
        now: u64,
        expires_at: u64,
    ) -> Result<Self, OAuthError> {
        let authorization_endpoint =
            validate_oauth_web_url(authorization_endpoint.as_ref(), MAX_OAUTH_ENDPOINT_BYTES)?;
        if authorization_endpoint
            .query_pairs()
            .any(|(name, _)| OAUTH_AUTHORIZATION_PARAMETERS.contains(&name.as_ref()))
        {
            return Err(OAuthError::InvalidMetadata);
        }

        let redirect_uri = redirect_uri.into();
        validate_oauth_web_url(&redirect_uri, MAX_REDIRECT_URI_BYTES)?;
        let client_id = client_id.into();
        validate_metadata_value(&client_id)?;
        if let Some(scope) = &scope {
            validate_metadata_value(scope)?;
        }
        if expires_at <= now {
            return Err(OAuthError::InvalidMetadata);
        }

        Ok(Self {
            binding,
            authorization_endpoint,
            redirect_uri,
            resource: None,
            client_id,
            scope,
            state: random_oauth_secret()?,
            code_verifier: random_oauth_secret()?,
            expires_at,
        })
    }

    pub fn binding(&self) -> &OAuthBinding {
        &self.binding
    }

    pub fn with_resource(mut self, resource: impl Into<String>) -> Result<Self, OAuthError> {
        let resource = resource.into();
        validate_oauth_web_url(&resource, MAX_REDIRECT_URI_BYTES)?;
        self.resource = Some(resource);
        Ok(self)
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub fn authorization_url(&self) -> Url {
        let challenge = pkce_s256_challenge(self.code_verifier.expose_secret());
        let mut url = self.authorization_endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", &self.redirect_uri)
                .append_pair("state", self.state.expose_secret())
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            if let Some(scope) = &self.scope {
                query.append_pair("scope", scope);
            }
            if let Some(resource) = &self.resource {
                query.append_pair("resource", resource);
            }
        }
        url
    }

    pub fn complete(
        self,
        response: OAuthAuthorizationResponse<'_>,
        now: u64,
    ) -> Result<OAuthAuthorizationCode, OAuthError> {
        let returned_state = match &response {
            OAuthAuthorizationResponse::Code { state, .. }
            | OAuthAuthorizationResponse::Error { state } => *state,
        };
        if now >= self.expires_at
            || !constant_time_state_matches(self.state.expose_secret(), returned_state)
        {
            return Err(OAuthError::InvalidResponse);
        }

        let OAuthAuthorizationResponse::Code { code, .. } = response else {
            return Err(OAuthError::InvalidResponse);
        };
        let code = OAuthSecret::new(code).map_err(|_| OAuthError::InvalidResponse)?;
        Ok(OAuthAuthorizationCode {
            binding: self.binding,
            redirect_uri: self.redirect_uri,
            resource: self.resource,
            client_id: self.client_id,
            scope: self.scope,
            code,
            code_verifier: self.code_verifier,
        })
    }
}

impl std::fmt::Debug for OAuthAuthorizationTransaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationTransaction")
            .field("binding", &self.binding)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("redirect_uri", &self.redirect_uri)
            .field("resource", &self.resource)
            .field("client_id", &self.client_id)
            .field("scope", &self.scope)
            .field("state", &self.state)
            .field("code_verifier", &self.code_verifier)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub enum OAuthAuthorizationResponse<'a> {
    Code { state: &'a str, code: &'a str },
    Error { state: &'a str },
}

pub struct OAuthAuthorizationCode {
    binding: OAuthBinding,
    redirect_uri: String,
    resource: Option<String>,
    client_id: String,
    scope: Option<String>,
    code: OAuthSecret,
    code_verifier: OAuthSecret,
}

impl OAuthAuthorizationCode {
    pub fn binding(&self) -> &OAuthBinding {
        &self.binding
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub fn code(&self) -> &OAuthSecret {
        &self.code
    }

    pub fn code_verifier(&self) -> &OAuthSecret {
        &self.code_verifier
    }
}

impl std::fmt::Debug for OAuthAuthorizationCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuthorizationCode")
            .field("binding", &self.binding)
            .field("redirect_uri", &self.redirect_uri)
            .field("resource", &self.resource)
            .field("client_id", &self.client_id)
            .field("scope", &self.scope)
            .field("code", &self.code)
            .field("code_verifier", &self.code_verifier)
            .finish()
    }
}

fn random_oauth_secret() -> Result<OAuthSecret, OAuthError> {
    let mut bytes = [0_u8; OAUTH_TRANSACTION_RANDOM_BYTES];
    getrandom(&mut bytes[..], GetRandomFlags::empty()).map_err(|_| OAuthError::Storage)?;
    OAuthSecret::new(URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn constant_time_state_matches(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected.as_bytes().ct_eq(actual.as_bytes()).unwrap_u8() == 1
}

fn validate_oauth_web_url(value: &str, maximum_bytes: usize) -> Result<Url, OAuthError> {
    if value.len() > maximum_bytes {
        return Err(OAuthError::InvalidMetadata);
    }
    let parsed = Url::parse(value).map_err(|_| OAuthError::InvalidMetadata)?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
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
    Ok(parsed)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthAuthorizationMetadata {
    token_endpoint: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

impl OAuthAuthorizationMetadata {
    pub fn new(token_endpoint: impl Into<String>) -> Result<Self, OAuthError> {
        let token_endpoint = token_endpoint.into();
        validate_token_endpoint(&token_endpoint)?;
        Ok(Self {
            token_endpoint,
            client_id: None,
            scope: None,
            resource: None,
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

    pub fn with_resource(mut self, resource: impl Into<String>) -> Result<Self, OAuthError> {
        let resource = resource.into();
        validate_oauth_web_url(&resource, MAX_REDIRECT_URI_BYTES)?;
        self.resource = Some(resource);
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

    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
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
    #[error("OAuth authorization refresh request failed")]
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

    pub fn save_pending(
        &self,
        transaction: OAuthAuthorizationTransaction,
        metadata: OAuthAuthorizationMetadata,
    ) -> Result<(), OAuthError> {
        validate_metadata(&metadata)?;
        if metadata.client_id() != Some(transaction.client_id.as_str())
            || metadata.scope() != transaction.scope.as_deref()
            || metadata.resource() != transaction.resource.as_deref()
        {
            return Err(OAuthError::InvalidMetadata);
        }
        let state = transaction.state.expose_secret().to_owned();
        let record = StoredPendingAuthorization::from_transaction(transaction, metadata);
        let bytes = serde_json::to_vec(&record).map_err(|_| OAuthError::Storage)?;
        let (directory_path, directory, lock) = self.pending_lock()?;
        let temporary =
            tempfile::NamedTempFile::new_in(&directory_path).map_err(|_| OAuthError::Storage)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| OAuthError::Storage)?;
        let mut file = temporary.as_file();
        file.write_all(&bytes).map_err(|_| OAuthError::Storage)?;
        file.sync_all().map_err(|_| OAuthError::Storage)?;
        renameat(
            &directory,
            temporary_file_name(&temporary)?,
            &directory,
            pending_record_name(&state),
        )
        .map_err(|_| OAuthError::Storage)?;
        directory.sync_all().map_err(|_| OAuthError::Storage)?;
        drop(lock);
        Ok(())
    }

    pub fn consume_pending(
        &self,
        state: &str,
        response: OAuthAuthorizationResponse<'_>,
        now: u64,
    ) -> Result<(OAuthAuthorizationCode, OAuthAuthorizationMetadata), OAuthError> {
        if state.is_empty() || state.len() > MAX_SECRET_BYTES || state.chars().any(char::is_control)
        {
            return Err(OAuthError::NotAuthorized);
        }
        let (_directory_path, directory, lock) = self.pending_lock()?;
        let name = pending_record_name(state);
        let mut file = match openat(
            &directory,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Err(OAuthError::NotAuthorized),
            Err(_) => return Err(OAuthError::Storage),
        };
        ensure_private_file(&file)?;
        let size = file.metadata().map_err(|_| OAuthError::Storage)?.len();
        if size > MAX_RECORD_BYTES {
            return Err(OAuthError::Storage);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|_| OAuthError::Storage)?;
        let stored: StoredPendingAuthorization =
            serde_json::from_slice(&bytes).map_err(|_| OAuthError::Storage)?;
        if stored.format != OAUTH_FORMAT
            || pending_record_name(&stored.state) != name
            || !constant_time_state_matches(&stored.state, state)
        {
            return Err(OAuthError::Storage);
        }
        unlinkat(&directory, &name, AtFlags::empty()).map_err(|_| OAuthError::Storage)?;
        directory.sync_all().map_err(|_| OAuthError::Storage)?;
        drop(file);
        drop(lock);

        let (transaction, metadata) = stored.into_transaction()?;
        let code = transaction.complete(response, now)?;
        Ok((code, metadata))
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

    fn pending_lock(&self) -> Result<(PathBuf, File, File), OAuthError> {
        let (root_path, root) = filesystem::directory(&self.root, true).map_err(storage_error)?;
        drop(root);
        let (oauth_path, oauth) =
            filesystem::directory(&root_path.join("oauth"), true).map_err(storage_error)?;
        ensure_private_directory(&oauth)?;
        let (directory_path, directory) =
            filesystem::directory(&oauth_path.join(PENDING_DIRECTORY), true)
                .map_err(storage_error)?;
        ensure_private_directory(&directory)?;
        let lock = File::from(
            openat(
                &directory,
                PENDING_LOCK_NAME,
                OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| OAuthError::Storage)?,
        );
        ensure_private_file(&lock)?;
        flock(&lock, FlockOperation::LockExclusive).map_err(|_| OAuthError::Storage)?;
        Ok((directory_path, directory, lock))
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPendingAuthorization {
    format: u32,
    binding: OAuthBinding,
    metadata: OAuthAuthorizationMetadata,
    authorization_endpoint: String,
    redirect_uri: String,
    #[serde(default)]
    resource: Option<String>,
    client_id: String,
    scope: Option<String>,
    state: String,
    code_verifier: String,
    expires_at: u64,
}

impl StoredPendingAuthorization {
    fn from_transaction(
        transaction: OAuthAuthorizationTransaction,
        metadata: OAuthAuthorizationMetadata,
    ) -> Self {
        Self {
            format: OAUTH_FORMAT,
            binding: transaction.binding,
            metadata,
            authorization_endpoint: transaction.authorization_endpoint.to_string(),
            redirect_uri: transaction.redirect_uri,
            resource: transaction.resource,
            client_id: transaction.client_id,
            scope: transaction.scope,
            state: transaction.state.expose_secret().to_owned(),
            code_verifier: transaction.code_verifier.expose_secret().to_owned(),
            expires_at: transaction.expires_at,
        }
    }

    fn into_transaction(
        self,
    ) -> Result<(OAuthAuthorizationTransaction, OAuthAuthorizationMetadata), OAuthError> {
        validate_metadata(&self.metadata)?;
        let authorization_endpoint =
            validate_oauth_web_url(&self.authorization_endpoint, MAX_OAUTH_ENDPOINT_BYTES)?;
        let redirect_uri = validate_oauth_web_url(&self.redirect_uri, MAX_REDIRECT_URI_BYTES)?;
        if let Some(resource) = &self.resource {
            validate_oauth_web_url(resource, MAX_REDIRECT_URI_BYTES)?;
        }
        validate_metadata_value(&self.client_id)?;
        if self.metadata.client_id() != Some(self.client_id.as_str())
            || self.metadata.scope() != self.scope.as_deref()
            || self.metadata.resource() != self.resource.as_deref()
        {
            return Err(OAuthError::Storage);
        }
        let transaction = OAuthAuthorizationTransaction {
            binding: OAuthBinding::new(
                self.binding.provider,
                self.binding.plugin,
                self.binding.server,
                self.binding.owner,
            )?,
            authorization_endpoint,
            redirect_uri: redirect_uri.to_string(),
            resource: self.resource,
            client_id: self.client_id,
            scope: self.scope,
            state: OAuthSecret::new(self.state)?,
            code_verifier: OAuthSecret::new(self.code_verifier)?,
            expires_at: self.expires_at,
        };
        Ok((transaction, self.metadata))
    }
}

fn pending_record_name(state: &str) -> String {
    format!("{:x}.json", Sha256::digest(state.as_bytes()))
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
    if let Some(resource) = &metadata.resource {
        validate_oauth_web_url(resource, MAX_REDIRECT_URI_BYTES)?;
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

    fn new_test_transaction() -> OAuthAuthorizationTransaction {
        test_transaction_for_owner("owner-a")
    }

    fn test_transaction_for_owner(owner: &str) -> OAuthAuthorizationTransaction {
        OAuthAuthorizationTransaction::new(
            binding(owner),
            "https://provider.example/authorize?audience=api",
            "https://anchor.example/oauth/callback",
            "client-id",
            Some("read write".into()),
            100,
            300,
        )
        .unwrap()
        .with_resource("https://mcp.example/resource")
        .unwrap()
    }

    fn query_value(url: &Url, key: &str) -> String {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn pkce_s256_uses_the_rfc_7636_test_vector() {
        assert_eq!(
            pkce_s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorization_transaction_builds_owner_bound_pkce_url_and_consumes_callback() {
        let transaction = new_test_transaction();
        let url = transaction.authorization_url();
        let state = query_value(&url, "state");
        let challenge = query_value(&url, "code_challenge");
        let verifier = transaction.code_verifier.expose_secret().to_owned();
        let transaction_debug = format!("{transaction:?}");

        assert_eq!(transaction.binding().owner(), "owner-a");
        assert!(!transaction_debug.contains(&state));
        assert!(!transaction_debug.contains(&verifier));
        assert_eq!(
            url.query_pairs()
                .filter(|(name, _)| name == "state")
                .count(),
            1
        );
        assert_eq!(query_value(&url, "response_type"), "code");
        assert_eq!(query_value(&url, "client_id"), "client-id");
        assert_eq!(
            query_value(&url, "redirect_uri"),
            "https://anchor.example/oauth/callback"
        );
        assert_eq!(query_value(&url, "scope"), "read write");
        assert_eq!(
            query_value(&url, "resource"),
            "https://mcp.example/resource"
        );
        assert_eq!(query_value(&url, "audience"), "api");
        assert_eq!(query_value(&url, "code_challenge_method"), "S256");
        assert_eq!(challenge, pkce_s256_challenge(&verifier));
        assert_eq!(state.len(), 43);
        assert_eq!(verifier.len(), 43);
        assert!(
            state
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );

        let completed = transaction
            .complete(
                OAuthAuthorizationResponse::Code {
                    state: &state,
                    code: "authorization-code",
                },
                200,
            )
            .unwrap();
        assert_eq!(completed.binding().owner(), "owner-a");
        assert_eq!(completed.code().expose_secret(), "authorization-code");
        assert_eq!(completed.code_verifier().expose_secret(), verifier);
        assert_eq!(
            completed.redirect_uri(),
            "https://anchor.example/oauth/callback"
        );
        assert_eq!(completed.client_id(), "client-id");
        assert_eq!(completed.scope(), Some("read write"));
        assert_eq!(completed.resource(), Some("https://mcp.example/resource"));
        let debug = format!("{completed:?}");
        assert!(!debug.contains("authorization-code"));
        assert!(!debug.contains(&verifier));
    }

    #[test]
    fn authorization_transaction_rejects_mismatched_expired_and_provider_error_callbacks() {
        let transaction = new_test_transaction();
        assert!(matches!(
            transaction.complete(
                OAuthAuthorizationResponse::Code {
                    state: "wrong-state",
                    code: "authorization-code",
                },
                200,
            ),
            Err(OAuthError::InvalidResponse)
        ));

        let transaction = new_test_transaction();
        let state = query_value(&transaction.authorization_url(), "state");
        assert!(matches!(
            transaction.complete(
                OAuthAuthorizationResponse::Code {
                    state: &state,
                    code: "authorization-code",
                },
                300,
            ),
            Err(OAuthError::InvalidResponse)
        ));

        let transaction = new_test_transaction();
        let state = query_value(&transaction.authorization_url(), "state");
        assert!(matches!(
            transaction.complete(OAuthAuthorizationResponse::Error { state: &state }, 200),
            Err(OAuthError::InvalidResponse)
        ));

        let first_owner_transaction = test_transaction_for_owner("owner-a");
        let first_owner_state = query_value(&first_owner_transaction.authorization_url(), "state");
        let second_owner_transaction = test_transaction_for_owner("owner-b");
        assert!(matches!(
            second_owner_transaction.complete(
                OAuthAuthorizationResponse::Code {
                    state: &first_owner_state,
                    code: "authorization-code",
                },
                200,
            ),
            Err(OAuthError::InvalidResponse)
        ));
    }

    #[test]
    fn authorization_transaction_rejects_unsafe_and_ambiguous_configuration() {
        for (endpoint, redirect_uri, expires_at) in [
            (
                "http://provider.example/authorize",
                "https://anchor.example/callback",
                300,
            ),
            (
                "https://provider.example/authorize#fragment",
                "https://anchor.example/callback",
                300,
            ),
            (
                "https://provider.example/authorize?state=attacker",
                "https://anchor.example/callback",
                300,
            ),
            (
                "https://provider.example/authorize",
                "http://public.example/callback",
                300,
            ),
            (
                "https://provider.example/authorize",
                "https://anchor.example/callback",
                100,
            ),
        ] {
            assert!(matches!(
                OAuthAuthorizationTransaction::new(
                    binding("owner"),
                    endpoint,
                    redirect_uri,
                    "client-id",
                    None,
                    100,
                    expires_at,
                ),
                Err(OAuthError::InvalidMetadata)
            ));
        }

        assert!(
            OAuthAuthorizationTransaction::new(
                binding("owner"),
                "http://127.0.0.1:4321/authorize",
                "http://localhost:4321/callback",
                "client-id",
                None,
                100,
                300,
            )
            .is_ok()
        );
    }

    #[test]
    fn pending_authorization_survives_restart_and_is_consumed_once() {
        let root = tempdir().unwrap();
        let store = FileOAuthTokenStore::new(root.path());
        let target = binding("owner");
        let metadata = OAuthAuthorizationMetadata::new("https://provider.example/token")
            .unwrap()
            .with_client_id("fixture-client")
            .unwrap()
            .with_scope("read")
            .unwrap()
            .with_resource("https://mcp.example/resource")
            .unwrap();
        let transaction = OAuthAuthorizationTransaction::new(
            target.clone(),
            "https://provider.example/authorize",
            "https://anchor.example/oauth/callback",
            "fixture-client",
            Some("read".into()),
            100,
            200,
        )
        .unwrap()
        .with_resource("https://mcp.example/resource")
        .unwrap();
        let url = transaction.authorization_url();
        let state = query_value(&url, "state");
        store.save_pending(transaction, metadata.clone()).unwrap();

        let restarted_store = FileOAuthTokenStore::new(root.path());
        let (code, restored_metadata) = restarted_store
            .consume_pending(
                &state,
                OAuthAuthorizationResponse::Code {
                    state: &state,
                    code: "authorization-code",
                },
                150,
            )
            .unwrap();
        assert_eq!(code.binding(), &target);
        assert_eq!(code.code().expose_secret(), "authorization-code");
        assert_eq!(code.redirect_uri(), "https://anchor.example/oauth/callback");
        assert_eq!(restored_metadata, metadata);
        assert_eq!(code.resource(), Some("https://mcp.example/resource"));
        assert!(matches!(
            restarted_store.consume_pending(
                &state,
                OAuthAuthorizationResponse::Code {
                    state: &state,
                    code: "authorization-code",
                },
                150,
            ),
            Err(OAuthError::NotAuthorized)
        ));
        let pending = root.path().join("oauth/pending");
        let record = std::fs::read_dir(&pending)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|entry| entry.path());
        assert!(record.is_none());
    }

    #[test]
    fn invalid_pending_callback_consumes_only_its_own_transaction() {
        let root = tempdir().unwrap();
        let store = FileOAuthTokenStore::new(root.path());
        let metadata = OAuthAuthorizationMetadata::new("https://provider.example/token")
            .unwrap()
            .with_client_id("fixture-client")
            .unwrap();
        let transaction = OAuthAuthorizationTransaction::new(
            binding("owner"),
            "https://provider.example/authorize",
            "https://anchor.example/oauth/callback",
            "fixture-client",
            None,
            100,
            200,
        )
        .unwrap();
        let state = query_value(&transaction.authorization_url(), "state");
        store.save_pending(transaction, metadata).unwrap();
        assert!(matches!(
            store.consume_pending(
                &state,
                OAuthAuthorizationResponse::Error { state: &state },
                150,
            ),
            Err(OAuthError::InvalidResponse)
        ));
        assert!(matches!(
            store.consume_pending(
                &state,
                OAuthAuthorizationResponse::Error { state: &state },
                150,
            ),
            Err(OAuthError::NotAuthorized)
        ));
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
    fn revoked_refresh_removes_authorization_but_transient_failure_preserves_it() {
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
            if error == OAuthTransportError::Revoked {
                assert!(client.store().load(&target).unwrap().is_none());
                assert!(matches!(
                    client.access_token_at(&target, 20),
                    Err(OAuthError::NotAuthorized)
                ));
            } else {
                assert!(client.store().load(&target).unwrap().is_some());
                assert_eq!(
                    client.access_token_at(&target, 20),
                    Err(OAuthError::RefreshFailed)
                );
            }
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
