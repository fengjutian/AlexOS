//! OAuth 2.1 primitives for remote MCP servers.
//!
//! This module deliberately separates browser interaction from protocol and
//! token handling. The Shell opens `AuthorizationRequest::authorization_url`,
//! then returns the code, state and issuer to the Daemon for verification and
//! exchange. Tokens are persisted only through the platform SecretStore.

use std::{
    io::Read,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::McpError;
use crate::platform::secret::SecretStore;

const MAX_METADATA_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationRequest {
    pub authorization_url: String,
    pub state: String,
    pub code_verifier: String,
    pub issuer: String,
    pub token_endpoint: String,
    pub resource: String,
    pub client_id: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub obtained_at_ms: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

pub struct OAuthClient {
    agent: ureq::Agent,
}

impl Default for OAuthClient {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .into(),
        }
    }
}

impl OAuthClient {
    pub fn discover_resource(&self, resource: &str) -> Result<ProtectedResourceMetadata, McpError> {
        let resource = secure_url(resource, "MCP resource")?;
        let candidates = protected_resource_metadata_urls(&resource)?;
        let mut last_error = None;
        for candidate in candidates {
            match self.get_json::<ProtectedResourceMetadata>(&candidate) {
                Ok(metadata) => {
                    validate_resource_metadata(&resource, &metadata)?;
                    return Ok(metadata);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            McpError::Protocol("protected resource metadata was not found".into())
        }))
    }

    pub fn discover_authorization_server(
        &self,
        issuer: &str,
    ) -> Result<AuthorizationServerMetadata, McpError> {
        let issuer = secure_url(issuer, "authorization issuer")?;
        let metadata_url = issuer
            .join(".well-known/oauth-authorization-server")
            .map_err(|error| McpError::InvalidConfig(error.to_string()))?;
        let metadata = self.get_json(&metadata_url)?;
        validate_authorization_server(&issuer, &metadata)?;
        Ok(metadata)
    }

    pub fn begin(
        &self,
        resource: &str,
        metadata: &AuthorizationServerMetadata,
        client_id: &str,
        redirect_uri: &str,
        scopes: &[String],
    ) -> Result<AuthorizationRequest, McpError> {
        if client_id.is_empty() {
            return Err(McpError::InvalidConfig("OAuth client id is empty".into()));
        }
        let resource = secure_url(resource, "MCP resource")?;
        let issuer = secure_url(&metadata.issuer, "authorization issuer")?;
        validate_authorization_server(&issuer, metadata)?;
        let redirect =
            Url::parse(redirect_uri).map_err(|error| McpError::InvalidConfig(error.to_string()))?;
        let loopback = redirect
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback())
            || redirect.host_str() == Some("localhost");
        if redirect.scheme() != "https" && !(redirect.scheme() == "http" && loopback) {
            return Err(McpError::InvalidConfig(
                "OAuth redirect must use HTTPS or loopback HTTP".into(),
            ));
        }
        let code_verifier = random_urlsafe(32)?;
        let state = random_urlsafe(32)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut authorization_url =
            secure_url(&metadata.authorization_endpoint, "authorization endpoint")?;
        {
            let mut query = authorization_url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("resource", resource.as_str())
                .append_pair("state", &state);
            if !scopes.is_empty() {
                query.append_pair("scope", &scopes.join(" "));
            }
        }
        Ok(AuthorizationRequest {
            authorization_url: authorization_url.into(),
            state,
            code_verifier,
            issuer: issuer.into(),
            token_endpoint: metadata.token_endpoint.clone(),
            resource: resource.into(),
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
        })
    }

    pub fn exchange_code(
        &self,
        pending: &AuthorizationRequest,
        code: &str,
        returned_state: &str,
        returned_issuer: &str,
    ) -> Result<TokenSet, McpError> {
        if code.is_empty() || returned_state != pending.state {
            return Err(McpError::Protocol("OAuth state or code is invalid".into()));
        }
        if secure_url(returned_issuer, "returned issuer")?.as_str() != pending.issuer {
            return Err(McpError::Protocol("OAuth issuer mismatch".into()));
        }
        let mut tokens = self.post_token(
            &pending.token_endpoint,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", &pending.client_id),
                ("redirect_uri", &pending.redirect_uri),
                ("code_verifier", &pending.code_verifier),
                ("resource", &pending.resource),
            ],
        )?;
        tokens.token_endpoint = Some(pending.token_endpoint.clone());
        tokens.client_id = Some(pending.client_id.clone());
        tokens.resource = Some(pending.resource.clone());
        Ok(tokens)
    }

    pub fn refresh(
        &self,
        token_endpoint: &str,
        client_id: &str,
        resource: &str,
        refresh_token: &str,
    ) -> Result<TokenSet, McpError> {
        self.post_token(
            token_endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", client_id),
                ("resource", resource),
            ],
        )
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &Url) -> Result<T, McpError> {
        let request = ureq::http::Request::builder()
            .uri(url.as_str())
            .header("accept", "application/json")
            .body(())
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let response = self
            .agent
            .run(request)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        read_json(response)
    }

    fn post_token(&self, endpoint: &str, fields: &[(&str, &str)]) -> Result<TokenSet, McpError> {
        let endpoint = secure_url(endpoint, "token endpoint")?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter().copied())
            .finish();
        let request = ureq::http::Request::builder()
            .method(ureq::http::Method::POST)
            .uri(endpoint.as_str())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body.into_bytes())
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let response = self
            .agent
            .run(request)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let mut tokens: TokenSet = read_json(response)?;
        if tokens.access_token.is_empty()
            || tokens
                .token_type
                .as_deref()
                .is_some_and(|value| !value.eq_ignore_ascii_case("bearer"))
        {
            return Err(McpError::Protocol("invalid OAuth token response".into()));
        }
        tokens.obtained_at_ms = Some(now_ms());
        Ok(tokens)
    }
}

#[derive(Clone)]
pub struct TokenVault {
    store: Arc<dyn SecretStore>,
}

pub trait AccessTokenProvider: Send + Sync {
    fn access_token(&self) -> Result<Option<String>, McpError>;
    fn refresh_access_token(
        &self,
        challenge: &AuthChallenge,
        rejected_token: Option<&str>,
    ) -> Result<bool, McpError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthChallenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
}

pub fn parse_www_authenticate(value: &str) -> Result<AuthChallenge, McpError> {
    let (scheme, parameters) = value.split_once(' ').unwrap_or((value, ""));
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(McpError::Authorization(
            "MCP server did not return a Bearer challenge".into(),
        ));
    }
    let mut challenge = AuthChallenge {
        resource_metadata: None,
        scope: None,
    };
    for part in parameters.split(',') {
        let Some((name, raw)) = part.trim().split_once('=') else {
            continue;
        };
        let value = raw.trim().trim_matches('"');
        match name.trim().to_ascii_lowercase().as_str() {
            "resource_metadata" => {
                secure_url(value, "resource metadata")?;
                challenge.resource_metadata = Some(value.into());
            }
            "scope" => challenge.scope = Some(value.into()),
            _ => {}
        }
    }
    Ok(challenge)
}

#[derive(Clone)]
pub struct VaultAccessTokenProvider {
    vault: TokenVault,
    account: String,
    refresh_gate: Arc<std::sync::Mutex<()>>,
}

impl VaultAccessTokenProvider {
    pub fn new(vault: TokenVault, account: impl Into<String>) -> Result<Self, McpError> {
        let account = account.into();
        if account.is_empty() || account.len() > 255 || account.contains(['\r', '\n', '\0']) {
            return Err(McpError::InvalidConfig(
                "invalid OAuth token account".into(),
            ));
        }
        Ok(Self {
            vault,
            account,
            refresh_gate: Arc::new(std::sync::Mutex::new(())),
        })
    }
}

impl AccessTokenProvider for VaultAccessTokenProvider {
    fn access_token(&self) -> Result<Option<String>, McpError> {
        let mut stored = self.vault.load(&self.account)?;
        if stored.as_ref().is_some_and(|tokens| {
            tokens
                .expires_in
                .zip(tokens.obtained_at_ms)
                .is_some_and(|(seconds, obtained)| {
                    now_ms().saturating_add(30_000)
                        >= obtained.saturating_add(seconds.saturating_mul(1_000))
                })
        }) {
            let rejected = stored.as_ref().map(|tokens| tokens.access_token.clone());
            if !self.refresh_access_token(
                &AuthChallenge {
                    resource_metadata: None,
                    scope: None,
                },
                rejected.as_deref(),
            )? {
                return Err(McpError::Authorization(
                    "stored access token expired and cannot be refreshed".into(),
                ));
            }
            stored = self.vault.load(&self.account)?;
        }
        let token = stored.map(|tokens| tokens.access_token);
        if token
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.contains(['\r', '\n', '\0']))
        {
            return Err(McpError::Protocol(
                "stored OAuth access token is invalid".into(),
            ));
        }
        Ok(token)
    }

    fn refresh_access_token(
        &self,
        _: &AuthChallenge,
        rejected_token: Option<&str>,
    ) -> Result<bool, McpError> {
        let _gate = self
            .refresh_gate
            .lock()
            .map_err(|_| McpError::Transport("OAuth refresh lock poisoned".into()))?;
        let Some(previous) = self.vault.load(&self.account)? else {
            return Ok(false);
        };
        // Another request may have refreshed while this request waited for the
        // gate. Reuse that token instead of rotating the refresh token again.
        if rejected_token.is_some_and(|token| token != previous.access_token) {
            return Ok(true);
        }
        let (Some(refresh_token), Some(endpoint), Some(client_id), Some(resource)) = (
            previous.refresh_token.as_deref(),
            previous.token_endpoint.as_deref(),
            previous.client_id.as_deref(),
            previous.resource.as_deref(),
        ) else {
            return Ok(false);
        };
        let mut tokens =
            OAuthClient::default().refresh(endpoint, client_id, resource, refresh_token)?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = previous.refresh_token;
        }
        tokens.token_endpoint = Some(endpoint.into());
        tokens.client_id = Some(client_id.into());
        tokens.resource = Some(resource.into());
        self.vault.save(&self.account, &tokens)?;
        Ok(true)
    }
}

impl TokenVault {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
    pub fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), McpError> {
        self.store
            .set(
                "com.alex.runtime.mcp.oauth",
                account,
                &serde_json::to_vec(tokens)
                    .map_err(|error| McpError::Protocol(error.to_string()))?,
            )
            .map_err(|error| McpError::Transport(error.to_string()))
    }
    pub fn load(&self, account: &str) -> Result<Option<TokenSet>, McpError> {
        self.store
            .get("com.alex.runtime.mcp.oauth", account)
            .map_err(|error| McpError::Transport(error.to_string()))?
            .map(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|error| McpError::Protocol(error.to_string()))
            })
            .transpose()
    }
    pub fn delete(&self, account: &str) -> Result<bool, McpError> {
        self.store
            .delete("com.alex.runtime.mcp.oauth", account)
            .map_err(|error| McpError::Transport(error.to_string()))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn secure_url(value: &str, label: &str) -> Result<Url, McpError> {
    let url = Url::parse(value).map_err(|error| McpError::InvalidConfig(error.to_string()))?;
    let loopback = url
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
        || url.host_str() == Some("localhost");
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(McpError::InvalidConfig(format!(
            "{label} must use HTTPS (HTTP is loopback-only)"
        )));
    }
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(McpError::InvalidConfig(format!("invalid {label} URL")));
    }
    Ok(url)
}

fn protected_resource_metadata_urls(resource: &Url) -> Result<Vec<Url>, McpError> {
    let mut root = resource.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    let path = resource.path().trim_start_matches('/');
    if path.is_empty() {
        return Ok(vec![root]);
    }
    let mut scoped = root.clone();
    scoped.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
    Ok(vec![scoped, root])
}

fn validate_resource_metadata(
    resource: &Url,
    metadata: &ProtectedResourceMetadata,
) -> Result<(), McpError> {
    if secure_url(&metadata.resource, "metadata resource")?.as_str() != resource.as_str() {
        return Err(McpError::Protocol(
            "OAuth resource metadata mismatch".into(),
        ));
    }
    if metadata.authorization_servers.is_empty() {
        return Err(McpError::Protocol(
            "resource metadata omitted authorization_servers".into(),
        ));
    }
    for issuer in &metadata.authorization_servers {
        secure_url(issuer, "authorization issuer")?;
    }
    Ok(())
}

fn validate_authorization_server(
    issuer: &Url,
    metadata: &AuthorizationServerMetadata,
) -> Result<(), McpError> {
    if secure_url(&metadata.issuer, "metadata issuer")?.as_str() != issuer.as_str() {
        return Err(McpError::Protocol(
            "authorization metadata issuer mismatch".into(),
        ));
    }
    secure_url(&metadata.authorization_endpoint, "authorization endpoint")?;
    secure_url(&metadata.token_endpoint, "token endpoint")?;
    if !metadata
        .code_challenge_methods_supported
        .iter()
        .any(|method| method == "S256")
    {
        return Err(McpError::Protocol(
            "authorization server does not advertise PKCE S256".into(),
        ));
    }
    Ok(())
}

fn random_urlsafe(bytes: usize) -> Result<String, McpError> {
    let mut value = vec![0; bytes];
    getrandom::fill(&mut value).map_err(|error| McpError::Transport(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn read_json<T: for<'de> Deserialize<'de>>(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<T, McpError> {
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return Err(McpError::Protocol(
            "OAuth endpoint did not return application/json".into(),
        ));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| McpError::Transport(error.to_string()))?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(McpError::Protocol("OAuth response is too large".into()));
    }
    serde_json::from_slice(&bytes).map_err(|error| McpError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};

    #[derive(Default)]
    struct MemorySecrets(Mutex<BTreeMap<(String, String), Vec<u8>>>);
    impl SecretStore for MemorySecrets {
        fn set(
            &self,
            service: &str,
            account: &str,
            secret: &[u8],
        ) -> Result<(), crate::platform::secret::SecretStoreError> {
            self.0
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), secret.to_vec());
            Ok(())
        }
        fn get(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Vec<u8>>, crate::platform::secret::SecretStoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn delete(
            &self,
            service: &str,
            account: &str,
        ) -> Result<bool, crate::platform::secret::SecretStoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()))
                .is_some())
        }
    }

    fn metadata() -> AuthorizationServerMetadata {
        AuthorizationServerMetadata {
            issuer: "https://auth.example.test/".into(),
            authorization_endpoint: "https://auth.example.test/authorize".into(),
            token_endpoint: "https://auth.example.test/token".into(),
            code_challenge_methods_supported: vec!["S256".into()],
            registration_endpoint: None,
        }
    }

    #[test]
    fn begin_builds_pkce_and_resource_bound_authorization_url() {
        let request = OAuthClient::default()
            .begin(
                "https://mcp.example.test/v1",
                &metadata(),
                "alex-desktop",
                "http://127.0.0.1:34991/callback",
                &["tools:read".into()],
            )
            .unwrap();
        let url = Url::parse(&request.authorization_url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(
            query.get("resource").unwrap(),
            "https://mcp.example.test/v1"
        );
        assert_eq!(query.get("state").unwrap(), &request.state);
        assert!(request.code_verifier.len() >= 43);
    }

    #[test]
    fn metadata_requires_exact_issuer_and_pkce_s256() {
        let issuer = Url::parse("https://auth.example.test/").unwrap();
        assert!(validate_authorization_server(&issuer, &metadata()).is_ok());
        let mut invalid = metadata();
        invalid.issuer = "https://attacker.example/".into();
        assert!(validate_authorization_server(&issuer, &invalid).is_err());
        invalid = metadata();
        invalid.code_challenge_methods_supported.clear();
        assert!(validate_authorization_server(&issuer, &invalid).is_err());
    }

    #[test]
    fn protected_metadata_paths_include_scoped_then_root_fallback() {
        let resource = Url::parse("https://mcp.example.test/public/mcp").unwrap();
        let paths = protected_resource_metadata_urls(&resource).unwrap();
        assert_eq!(
            paths[0].path(),
            "/.well-known/oauth-protected-resource/public/mcp"
        );
        assert_eq!(paths[1].path(), "/.well-known/oauth-protected-resource");
    }

    #[test]
    fn token_vault_round_trips_without_exposing_storage_details() {
        let vault = TokenVault::new(Arc::new(MemorySecrets::default()));
        let tokens = TokenSet {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            token_type: Some("Bearer".into()),
            expires_in: Some(3600),
            obtained_at_ms: Some(now_ms()),
            scope: Some("tools:read".into()),
            token_endpoint: Some("https://auth.example.test/token".into()),
            client_id: Some("alex-desktop".into()),
            resource: Some("https://mcp.example.test/v1".into()),
        };
        vault.save("com.example.app:search", &tokens).unwrap();
        assert_eq!(vault.load("com.example.app:search").unwrap(), Some(tokens));
        let provider =
            VaultAccessTokenProvider::new(vault.clone(), "com.example.app:search").unwrap();
        assert_eq!(provider.access_token().unwrap().as_deref(), Some("access"));
        assert!(vault.delete("com.example.app:search").unwrap());
        assert!(provider.access_token().unwrap().is_none());
    }
}
