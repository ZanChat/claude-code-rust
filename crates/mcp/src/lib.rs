use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
    WebSocket { url: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpAuthConfig {
    None,
    EnvToken {
        env_var: String,
    },
    BearerToken {
        env_var: String,
    },
    OAuthDevice {
        client_id: String,
        audience: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: Option<McpTransportConfig>,
    pub auth: Option<McpAuthConfig>,
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpResourceDescriptor {
    pub uri: String,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerState {
    pub connected: bool,
    pub last_error: Option<String>,
    pub tool_count: usize,
    pub resource_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpServerManifest {
    pub config: McpServerConfig,
    pub tools: Vec<McpToolDescriptor>,
    pub resources: Vec<McpResourceDescriptor>,
    pub state: McpServerState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedMcpAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpAuthorizationMetadata {
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub device_authorization_endpoint: Option<String>,
    pub registration_endpoint: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMcpDeviceFlow {
    pub device_code: String,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub expires_in_seconds: Option<u64>,
    pub interval_seconds: Option<u64>,
    pub token_endpoint: Option<String>,
    pub requested_at_unix_ms: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpClientCapabilities {
    pub supports_tools: bool,
    pub supports_resources: bool,
    pub supports_prompts: bool,
}

#[derive(Clone, Debug, Default)]
pub struct McpRegistry {
    configs: BTreeMap<String, McpServerConfig>,
    manifests: BTreeMap<String, McpServerManifest>,
}

impl McpRegistry {
    pub fn register(&mut self, key: String, config: McpServerConfig) {
        self.configs.insert(key, config);
    }

    pub fn register_manifest(&mut self, key: String, manifest: McpServerManifest) {
        self.configs.insert(key.clone(), manifest.config.clone());
        self.manifests.insert(key, manifest);
    }

    pub fn get(&self, key: &str) -> Option<&McpServerConfig> {
        self.configs.get(key)
    }

    pub fn get_manifest(&self, key: &str) -> Option<&McpServerManifest> {
        self.manifests.get(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<McpServerConfig> {
        self.manifests.remove(key);
        self.configs.remove(key)
    }

    pub fn list(&self) -> Vec<&McpServerConfig> {
        self.configs.values().collect()
    }

    pub fn list_manifests(&self) -> Vec<&McpServerManifest> {
        self.manifests.values().collect()
    }

    pub fn merge_plugin_servers(
        &mut self,
        prefix: &str,
        servers: BTreeMap<String, McpServerConfig>,
    ) {
        for (name, config) in servers {
            self.register(format!("{prefix}:{name}"), config);
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct McpToolCallResult {
    pub content_text: String,
    pub is_error: bool,
    pub raw: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct McpResourceReadResult {
    pub content_text: String,
    pub raw: Value,
}

async fn write_content_length_message<W>(writer: &mut W, value: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_content_length_message<R>(reader: &mut R) -> Result<Value>
where
    R: AsyncRead + Unpin,
{
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            bail!("mcp stream closed while reading headers");
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 16_384 {
            bail!("mcp header exceeded maximum length");
        }
    }

    let header_text = String::from_utf8(header)?;
    let mut content_length = None;
    for line in header_text.split("\r\n") {
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let content_length =
        content_length.ok_or_else(|| anyhow!("mcp header missing Content-Length"))?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

fn env_headers(config: &McpServerConfig) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    for (key, value) in &config.headers {
        headers.insert(
            HeaderName::from_bytes(key.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }

    if let Some(header) = authorization_header_value(config)? {
        headers.insert(AUTHORIZATION, header);
    }

    Ok(headers)
}

fn merged_env(config: &McpServerConfig) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for (key, value) in &config.env {
        values.insert(key.clone(), value.clone());
    }
    values
}

fn parse_auth_config(value: Option<&Value>) -> Option<McpAuthConfig> {
    let Value::Object(map) = value? else {
        return None;
    };
    match map.get("type").and_then(Value::as_str)? {
        "none" => Some(McpAuthConfig::None),
        "env_token" => map
            .get("envVar")
            .or_else(|| map.get("env_var"))
            .and_then(Value::as_str)
            .map(|env_var| McpAuthConfig::EnvToken {
                env_var: env_var.to_owned(),
            }),
        "bearer_token" => map
            .get("envVar")
            .or_else(|| map.get("env_var"))
            .and_then(Value::as_str)
            .map(|env_var| McpAuthConfig::BearerToken {
                env_var: env_var.to_owned(),
            }),
        "oauth_device" => map
            .get("clientId")
            .or_else(|| map.get("client_id"))
            .and_then(Value::as_str)
            .map(|client_id| McpAuthConfig::OAuthDevice {
                client_id: client_id.to_owned(),
                audience: map
                    .get("audience")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
        _ => None,
    }
}

fn sanitize_cache_key(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        output.push(if ch.is_ascii_alphanumeric() { ch } else { '-' });
    }
    output
}

fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn authorization_header_value(config: &McpServerConfig) -> Result<Option<HeaderValue>> {
    match config.auth.as_ref() {
        Some(McpAuthConfig::EnvToken { env_var } | McpAuthConfig::BearerToken { env_var }) => {
            let token = env::var(env_var)
                .with_context(|| format!("required MCP auth env var is missing: {env_var}"))?;
            Ok(Some(HeaderValue::from_str(&format!("Bearer {token}"))?))
        }
        Some(McpAuthConfig::OAuthDevice { .. }) => {
            let cached = load_cached_auth_token(config)?
                .ok_or_else(|| anyhow!("mcp OAuth device auth is required; run mcp_auth first"))?;
            Ok(Some(HeaderValue::from_str(&format!(
                "{} {}",
                cached
                    .token_type
                    .clone()
                    .unwrap_or_else(|| "Bearer".to_owned()),
                cached.access_token
            ))?))
        }
        Some(McpAuthConfig::None) | None => Ok(None),
    }
}

pub fn mcp_auth_cache_dir() -> PathBuf {
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        return PathBuf::from(codex_home).join("mcp-auth");
    }
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".code-agent").join("mcp-auth"),
        None => PathBuf::from(".code-agent").join("mcp-auth"),
    }
}

pub fn mcp_auth_cache_key(config: &McpServerConfig) -> String {
    let identity = match config.transport.as_ref() {
        Some(McpTransportConfig::Http { url } | McpTransportConfig::WebSocket { url }) => {
            format!("{}-{url}", config.name)
        }
        Some(McpTransportConfig::Stdio { command, args }) => {
            format!("{}-{command}-{}", config.name, args.join("-"))
        }
        None => config.name.clone(),
    };
    sanitize_cache_key(&identity)
}

pub fn mcp_auth_cache_path(config: &McpServerConfig) -> PathBuf {
    mcp_auth_cache_dir().join(format!("{}.json", mcp_auth_cache_key(config)))
}

pub fn mcp_pending_device_flow_path(config: &McpServerConfig) -> PathBuf {
    mcp_auth_cache_dir().join(format!("{}.pending.json", mcp_auth_cache_key(config)))
}

pub fn load_cached_auth_token(config: &McpServerConfig) -> Result<Option<CachedMcpAuthToken>> {
    let path = mcp_auth_cache_path(config);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read MCP auth cache {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw).with_context(|| {
        format!("failed to decode MCP auth cache {}", path.display())
    })?))
}

pub fn store_cached_auth_token(
    config: &McpServerConfig,
    token: &CachedMcpAuthToken,
) -> Result<PathBuf> {
    let path = mcp_auth_cache_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create MCP auth cache dir {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(token)?)
        .with_context(|| format!("failed to write MCP auth cache {}", path.display()))?;
    Ok(path)
}

pub fn clear_cached_auth_token(config: &McpServerConfig) -> Result<bool> {
    let path = mcp_auth_cache_path(config);
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path)
        .with_context(|| format!("failed to remove MCP auth cache {}", path.display()))?;
    Ok(true)
}

pub fn load_pending_device_flow(config: &McpServerConfig) -> Result<Option<PendingMcpDeviceFlow>> {
    let path = mcp_pending_device_flow_path(config);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read MCP device flow cache {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw).with_context(|| {
        format!("failed to decode MCP device flow cache {}", path.display())
    })?))
}

pub fn store_pending_device_flow(
    config: &McpServerConfig,
    flow: &PendingMcpDeviceFlow,
) -> Result<PathBuf> {
    let path = mcp_pending_device_flow_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create MCP auth cache dir {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(flow)?)
        .with_context(|| format!("failed to write MCP device flow cache {}", path.display()))?;
    Ok(path)
}

pub fn clear_pending_device_flow(config: &McpServerConfig) -> Result<bool> {
    let path = mcp_pending_device_flow_path(config);
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path)
        .with_context(|| format!("failed to remove MCP device flow cache {}", path.display()))?;
    Ok(true)
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| value.as_str().map(|raw| (key.clone(), raw.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn auth_object(config: &McpServerConfig) -> Option<&serde_json::Map<String, Value>> {
    config.metadata.get("auth")?.as_object()
}

fn auth_string(config: &McpServerConfig, camel: &str, snake: &str) -> Option<String> {
    auth_object(config)?
        .get(camel)
        .or_else(|| auth_object(config)?.get(snake))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn auth_url(config: &McpServerConfig, camel: &str, snake: &str) -> Option<String> {
    auth_string(config, camel, snake)
}

fn auth_form_value(config: &McpServerConfig, camel: &str, snake: &str) -> Option<String> {
    auth_string(config, camel, snake)
}

fn transport_base_url(config: &McpServerConfig) -> Result<Url> {
    let raw = match config.transport.as_ref() {
        Some(McpTransportConfig::Http { url } | McpTransportConfig::WebSocket { url }) => {
            url.clone()
        }
        Some(McpTransportConfig::Stdio { .. }) => {
            bail!("mcp OAuth device auth requires an HTTP or websocket transport")
        }
        None => bail!(
            "mcp server '{}' has no transport configuration",
            config.name
        ),
    };
    let normalized = raw
        .strip_prefix("ws://")
        .map(|rest| format!("http://{rest}"))
        .or_else(|| {
            raw.strip_prefix("wss://")
                .map(|rest| format!("https://{rest}"))
        })
        .unwrap_or(raw);
    let mut url = Url::parse(&normalized)?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn resolve_endpoint(base: &Url, endpoint: &str) -> Result<String> {
    let candidate = Url::parse(endpoint).or_else(|_| base.join(endpoint))?;
    Ok(candidate.to_string())
}

fn fallback_authorization_metadata(
    base: &Url,
    config: &McpServerConfig,
) -> Result<McpAuthorizationMetadata> {
    Ok(McpAuthorizationMetadata {
        authorization_endpoint: Some(resolve_endpoint(
            base,
            &auth_url(config, "authorizationEndpoint", "authorization_endpoint")
                .unwrap_or_else(|| "/authorize".to_owned()),
        )?),
        token_endpoint: Some(resolve_endpoint(
            base,
            &auth_url(config, "tokenEndpoint", "token_endpoint")
                .unwrap_or_else(|| "/token".to_owned()),
        )?),
        device_authorization_endpoint: Some(resolve_endpoint(
            base,
            &auth_url(
                config,
                "deviceAuthorizationEndpoint",
                "device_authorization_endpoint",
            )
            .unwrap_or_else(|| "/device_authorization".to_owned()),
        )?),
        registration_endpoint: auth_url(config, "registrationEndpoint", "registration_endpoint")
            .map(|value| resolve_endpoint(base, &value))
            .transpose()?,
    })
}

pub async fn discover_authorization_metadata(
    config: &McpServerConfig,
) -> Result<McpAuthorizationMetadata> {
    let base = transport_base_url(config)?;
    let metadata_url = base.join(".well-known/oauth-authorization-server")?;
    let client = reqwest::Client::new();
    let response = client
        .get(metadata_url)
        .header("MCP-Protocol-Version", "2024-11-05")
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {
            let value = response.json::<Value>().await?;
            let mut metadata = McpAuthorizationMetadata {
                authorization_endpoint: value
                    .get("authorization_endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                token_endpoint: value
                    .get("token_endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                device_authorization_endpoint: value
                    .get("device_authorization_endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                registration_endpoint: value
                    .get("registration_endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            let fallback = fallback_authorization_metadata(&base, config)?;
            if metadata.authorization_endpoint.is_none() {
                metadata.authorization_endpoint = fallback.authorization_endpoint;
            }
            if metadata.token_endpoint.is_none() {
                metadata.token_endpoint = fallback.token_endpoint;
            }
            if metadata.device_authorization_endpoint.is_none() {
                metadata.device_authorization_endpoint = fallback.device_authorization_endpoint;
            }
            if metadata.registration_endpoint.is_none() {
                metadata.registration_endpoint = fallback.registration_endpoint;
            }
            Ok(metadata)
        }
        _ => fallback_authorization_metadata(&base, config),
    }
}

pub async fn start_oauth_device_flow(config: &McpServerConfig) -> Result<PendingMcpDeviceFlow> {
    let McpAuthConfig::OAuthDevice {
        client_id,
        audience,
    } = config.auth.clone().ok_or_else(|| {
        anyhow!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        )
    })?
    else {
        bail!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        );
    };
    let metadata = discover_authorization_metadata(config).await?;
    let endpoint = metadata
        .device_authorization_endpoint
        .clone()
        .ok_or_else(|| {
            anyhow!("authorization metadata is missing a device authorization endpoint")
        })?;
    let mut form = vec![("client_id", client_id)];
    if let Some(audience) = audience {
        form.push(("audience", audience));
    }
    if let Some(scope) = auth_form_value(config, "scope", "scope") {
        form.push(("scope", scope));
    }
    if let Some(resource) = auth_form_value(config, "resource", "resource") {
        form.push(("resource", resource));
    }
    let response = reqwest::Client::new()
        .post(&endpoint)
        .form(&form)
        .send()
        .await?;
    let status = response.status();
    let value = response.json::<Value>().await?;
    if !status.is_success() {
        bail!(
            "mcp device authorization failed with status {}: {}",
            status,
            value
        );
    }
    let flow = PendingMcpDeviceFlow {
        device_code: value
            .get("device_code")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("device authorization response missing device_code"))?
            .to_owned(),
        user_code: value
            .get("user_code")
            .and_then(Value::as_str)
            .map(str::to_owned),
        verification_uri: value
            .get("verification_uri")
            .and_then(Value::as_str)
            .map(str::to_owned),
        verification_uri_complete: value
            .get("verification_uri_complete")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expires_in_seconds: value.get("expires_in").and_then(Value::as_u64),
        interval_seconds: value.get("interval").and_then(Value::as_u64),
        token_endpoint: metadata.token_endpoint,
        requested_at_unix_ms: unix_time_ms(),
    };
    store_pending_device_flow(config, &flow)?;
    Ok(flow)
}

async fn exchange_token_form(
    endpoint: &str,
    form: &[(&str, String)],
) -> Result<CachedMcpAuthToken> {
    let response = reqwest::Client::new()
        .post(endpoint)
        .form(form)
        .send()
        .await?;
    let status = response.status();
    let value = response.json::<Value>().await?;
    if !status.is_success() {
        bail!(
            "mcp token exchange failed with status {}: {}",
            status,
            value
        );
    }
    Ok(CachedMcpAuthToken {
        access_token: value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("token response missing access_token"))?
            .to_owned(),
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        token_type: value
            .get("token_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expires_at_unix_ms: value
            .get("expires_in")
            .and_then(Value::as_i64)
            .map(|seconds| unix_time_ms() + (seconds * 1000)),
    })
}

pub async fn poll_oauth_device_flow(
    config: &McpServerConfig,
    device_code: Option<&str>,
) -> Result<CachedMcpAuthToken> {
    let McpAuthConfig::OAuthDevice { client_id, .. } = config.auth.clone().ok_or_else(|| {
        anyhow!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        )
    })?
    else {
        bail!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        );
    };
    let pending = load_pending_device_flow(config)?;
    let pending = pending.as_ref();
    let metadata = discover_authorization_metadata(config).await?;
    let token_endpoint = pending
        .and_then(|flow| flow.token_endpoint.clone())
        .or(metadata.token_endpoint)
        .ok_or_else(|| anyhow!("authorization metadata is missing a token endpoint"))?;
    let device_code = device_code
        .map(str::to_owned)
        .or_else(|| pending.map(|flow| flow.device_code.clone()))
        .ok_or_else(|| anyhow!("device poll requires a device code or a pending device flow"))?;
    let form = vec![
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
        ),
        ("client_id", client_id),
        ("device_code", device_code),
    ];
    let token = exchange_token_form(&token_endpoint, &form).await?;
    store_cached_auth_token(config, &token)?;
    let _ = clear_pending_device_flow(config);
    Ok(token)
}

pub async fn refresh_oauth_device_token(config: &McpServerConfig) -> Result<CachedMcpAuthToken> {
    let McpAuthConfig::OAuthDevice { client_id, .. } = config.auth.clone().ok_or_else(|| {
        anyhow!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        )
    })?
    else {
        bail!(
            "mcp server '{}' is not configured for OAuth device auth",
            config.name
        );
    };
    let cached = load_cached_auth_token(config)?
        .ok_or_else(|| anyhow!("mcp refresh requires a cached auth token"))?;
    let refresh_token = cached
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("mcp refresh requires a cached refresh token"))?;
    let metadata = discover_authorization_metadata(config).await?;
    let token_endpoint = metadata
        .token_endpoint
        .ok_or_else(|| anyhow!("authorization metadata is missing a token endpoint"))?;
    let form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    let token = exchange_token_form(&token_endpoint, &form).await?;
    store_cached_auth_token(config, &token)?;
    Ok(token)
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {},
                "resources": {}
            },
            "clientInfo": {
                "name": "code-agent-rust",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
}

fn content_to_text(value: &Value) -> String {
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        item.get("content")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .or_else(|| item.as_str().map(str::to_owned))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn parse_tools(result: &Value) -> Vec<McpToolDescriptor> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|entry| serde_json::from_value::<McpToolDescriptor>(entry.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_resources(result: &Value) -> Vec<McpResourceDescriptor> {
    result
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .filter_map(|entry| {
                    serde_json::from_value::<McpResourceDescriptor>(entry.clone()).ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_tool_call_result(result: Value) -> McpToolCallResult {
    let content = result.get("content").cloned().unwrap_or_else(|| json!([]));
    McpToolCallResult {
        content_text: content_to_text(&content),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw: result,
    }
}

fn parse_resource_read_result(result: Value) -> McpResourceReadResult {
    let contents = result
        .get("contents")
        .cloned()
        .or_else(|| result.get("content").cloned())
        .unwrap_or_else(|| json!([]));
    McpResourceReadResult {
        content_text: content_to_text(&contents),
        raw: result,
    }
}

async fn stdio_request(config: &McpServerConfig, request: Value) -> Result<Value> {
    let Some(McpTransportConfig::Stdio { command, args }) = config.transport.as_ref() else {
        bail!("mcp stdio request requires stdio transport");
    };

    let mut child = Command::new(command);
    child.args(args);
    child.envs(merged_env(config));
    child.stdin(std::process::Stdio::piped());
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::null());

    let mut child = child
        .spawn()
        .with_context(|| format!("failed to launch MCP server '{}'", config.name))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("mcp child stdin unavailable"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("mcp child stdout unavailable"))?;

    write_content_length_message(&mut stdin, &initialize_request()).await?;
    let initialize_response = read_content_length_message(&mut stdout).await?;
    if initialize_response.get("error").is_some() {
        bail!("mcp initialize failed: {}", initialize_response);
    }
    write_content_length_message(&mut stdin, &initialized_notification()).await?;
    write_content_length_message(&mut stdin, &request).await?;

    loop {
        let message = read_content_length_message(&mut stdout).await?;
        if message.get("id") == request.get("id") {
            child.start_kill().ok();
            if let Some(error) = message.get("error") {
                bail!("mcp request failed: {}", error);
            }
            return Ok(message.get("result").cloned().unwrap_or_else(|| json!({})));
        }
    }
}

async fn http_request(url: &str, config: &McpServerConfig, request: Value) -> Result<Value> {
    let response = reqwest::Client::new()
        .post(url)
        .headers(env_headers(config)?)
        .json(&request)
        .send()
        .await?;
    let status = response.status();
    let value = response.json::<Value>().await?;
    if !status.is_success() {
        bail!("mcp http request failed with status {}: {}", status, value);
    }
    if let Some(error) = value.get("error") {
        bail!("mcp http request failed: {}", error);
    }
    Ok(value.get("result").cloned().unwrap_or_else(|| json!({})))
}

async fn websocket_request(url: &str, config: &McpServerConfig, request: Value) -> Result<Value> {
    let mut request_ws = url.into_client_request()?;
    for (key, value) in &config.headers {
        request_ws.headers_mut().insert(
            HeaderName::from_bytes(key.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    if let Some(header) = authorization_header_value(config)? {
        request_ws.headers_mut().insert(AUTHORIZATION, header);
    }
    let (mut socket, _) = connect_async(request_ws).await?;
    socket
        .send(WsMessage::Text(initialize_request().to_string().into()))
        .await?;
    while let Some(message) = socket.next().await {
        let message = message?;
        if message.is_text() {
            let value = serde_json::from_str::<Value>(message.to_text()?)?;
            if value.get("id") == Some(&json!(1)) {
                break;
            }
        }
    }

    socket
        .send(WsMessage::Text(
            initialized_notification().to_string().into(),
        ))
        .await?;
    socket
        .send(WsMessage::Text(request.to_string().into()))
        .await?;

    while let Some(message) = socket.next().await {
        let message = message?;
        if !message.is_text() {
            continue;
        }
        let value = serde_json::from_str::<Value>(message.to_text()?)?;
        if value.get("id") == request.get("id") {
            if let Some(error) = value.get("error") {
                bail!("mcp websocket request failed: {}", error);
            }
            return Ok(value.get("result").cloned().unwrap_or_else(|| json!({})));
        }
    }

    bail!("mcp websocket server closed before replying")
}

async fn send_request(config: &McpServerConfig, request: Value) -> Result<Value> {
    match config.transport.as_ref() {
        Some(McpTransportConfig::Stdio { .. }) => stdio_request(config, request).await,
        Some(McpTransportConfig::Http { url }) => http_request(url, config, request).await,
        Some(McpTransportConfig::WebSocket { url }) => {
            websocket_request(url, config, request).await
        }
        None => bail!(
            "mcp server '{}' has no transport configuration",
            config.name
        ),
    }
}

pub async fn list_tools_from_config(config: &McpServerConfig) -> Result<Vec<McpToolDescriptor>> {
    let result = send_request(
        config,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await?;
    Ok(parse_tools(&result))
}

pub async fn call_tool_from_config(
    config: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolCallResult> {
    let result = send_request(
        config,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        }),
    )
    .await?;
    Ok(parse_tool_call_result(result))
}

pub async fn list_resources_from_config(
    config: &McpServerConfig,
) -> Result<Vec<McpResourceDescriptor>> {
    let result = send_request(
        config,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/list",
            "params": {}
        }),
    )
    .await?;
    Ok(parse_resources(&result))
}

pub async fn read_resource_from_config(
    config: &McpServerConfig,
    uri: &str,
) -> Result<McpResourceReadResult> {
    let result = send_request(
        config,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/read",
            "params": {
                "uri": uri,
            }
        }),
    )
    .await?;
    Ok(parse_resource_read_result(result))
}

pub async fn load_manifest_from_config(config: &McpServerConfig) -> Result<McpServerManifest> {
    let tools = list_tools_from_config(config).await.unwrap_or_default();
    let resources = list_resources_from_config(config).await.unwrap_or_default();
    Ok(McpServerManifest {
        config: config.clone(),
        state: McpServerState {
            connected: true,
            last_error: None,
            tool_count: tools.len(),
            resource_count: resources.len(),
        },
        tools,
        resources,
    })
}

pub fn parse_mcp_server_config(name: &str, value: &Value) -> Option<McpServerConfig> {
    match value {
        Value::Object(map) => {
            let transport = if let Some(command) = map.get("command").and_then(Value::as_str) {
                Some(McpTransportConfig::Stdio {
                    command: command.to_owned(),
                    args: map
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            } else if let Some(url) = map.get("url").and_then(Value::as_str) {
                if url.starts_with("ws://") || url.starts_with("wss://") {
                    Some(McpTransportConfig::WebSocket {
                        url: url.to_owned(),
                    })
                } else {
                    Some(McpTransportConfig::Http {
                        url: url.to_owned(),
                    })
                }
            } else {
                None
            };

            Some(McpServerConfig {
                name: name.to_owned(),
                transport,
                auth: parse_auth_config(map.get("auth")),
                env: string_map(map.get("env")),
                headers: string_map(map.get("headers")),
                metadata: map
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            })
        }
        Value::String(url) => Some(McpServerConfig {
            name: name.to_owned(),
            transport: Some(if url.starts_with("ws://") || url.starts_with("wss://") {
                McpTransportConfig::WebSocket {
                    url: url.to_owned(),
                }
            } else {
                McpTransportConfig::Http {
                    url: url.to_owned(),
                }
            }),
            ..McpServerConfig::default()
        }),
        _ => None,
    }
}

pub fn parse_mcp_server_configs(
    raw: &BTreeMap<String, Value>,
) -> BTreeMap<String, McpServerConfig> {
    raw.iter()
        .filter_map(|(name, value)| {
            parse_mcp_server_config(name, value).map(|config| (name.clone(), config))
        })
        .collect()
}

#[cfg(test)]
mod tests;
