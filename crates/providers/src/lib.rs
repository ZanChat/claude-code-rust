use anyhow::{anyhow, Result};
use async_trait::async_trait;
use code_agent_core::{ContentBlock, Message, MessageRole, TokenUsage, ToolCall};
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use time::macros::format_description;
use time::OffsetDateTime;
use tokio::process::Command;

type HmacSha256 = Hmac<Sha256>;

const URI_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiProvider {
    #[serde(rename = "firstParty")]
    FirstParty,
    #[serde(rename = "bedrock")]
    Bedrock,
    #[serde(rename = "vertex")]
    Vertex,
    #[serde(rename = "foundry")]
    Foundry,
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "chatgpt-codex")]
    ChatGPTCodex,
    #[serde(rename = "openai-compatible")]
    OpenAICompatible,
}

impl Default for ApiProvider {
    fn default() -> Self {
        Self::FirstParty
    }
}

impl ApiProvider {
    pub const ALL: [Self; 7] = [
        Self::FirstParty,
        Self::Bedrock,
        Self::Vertex,
        Self::Foundry,
        Self::OpenAI,
        Self::ChatGPTCodex,
        Self::OpenAICompatible,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstParty => "firstParty",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Foundry => "foundry",
            Self::OpenAI => "openai",
            Self::ChatGPTCodex => "chatgpt-codex",
            Self::OpenAICompatible => "openai-compatible",
        }
    }
}

impl Display for ApiProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ApiProvider {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "firstParty" => Ok(Self::FirstParty),
            "bedrock" => Ok(Self::Bedrock),
            "vertex" => Ok(Self::Vertex),
            "foundry" => Ok(Self::Foundry),
            "openai" => Ok(Self::OpenAI),
            "chatgpt-codex" => Ok(Self::ChatGPTCodex),
            "openai-compatible" => Ok(Self::OpenAICompatible),
            other => Err(anyhow!("unsupported provider: {other}")),
        }
    }
}

pub fn resolve_api_provider(explicit: Option<&str>) -> Result<ApiProvider> {
    if let Some(raw) = explicit.filter(|value| !value.trim().is_empty()) {
        return raw.parse();
    }

    if let Ok(raw) = env::var("CLAUDE_CODE_API_PROVIDER") {
        if !raw.trim().is_empty() {
            return raw.parse();
        }
    }

    if env_flag("CLAUDE_CODE_USE_BEDROCK") {
        return Ok(ApiProvider::Bedrock);
    }
    if env_flag("CLAUDE_CODE_USE_VERTEX") {
        return Ok(ApiProvider::Vertex);
    }
    if env_flag("CLAUDE_CODE_USE_FOUNDRY") {
        return Ok(ApiProvider::Foundry);
    }

    Ok(ApiProvider::FirstParty)
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

pub fn codex_home_dir() -> PathBuf {
    if let Some(home) = env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }

    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".codex"),
        None => PathBuf::from(".codex"),
    }
}

pub fn codex_auth_file_path() -> PathBuf {
    codex_home_dir().join("auth.json")
}

pub fn code_agent_auth_snapshot_path() -> PathBuf {
    codex_home_dir().join("code-agent-auth.json")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthRequest {
    pub provider: ApiProvider,
    pub profile: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthMaterial {
    pub api_key: Option<String>,
    pub bearer_token: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AuthSnapshotFile {
    providers: BTreeMap<String, AuthMaterial>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: String,
    pub provider: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supports_tool_use: bool,
    pub supports_reasoning: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ProviderToolDefinition>,
    pub extra_headers: BTreeMap<String, String>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    MessageDelta { text: String },
    ToolCall { call: ToolCall },
    Usage { usage: TokenUsage },
    ToolCallBoundary { id: String },
    Stop { reason: String },
    Error { message: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CollectedProviderResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
    pub stop_reason: Option<String>,
}

#[async_trait]
pub trait AuthResolver: Send + Sync {
    async fn resolve_auth(&self, request: AuthRequest) -> Result<AuthMaterial>;
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn api_provider(&self) -> ApiProvider;
    async fn start_stream(&self, request: ProviderRequest) -> Result<Box<dyn ProviderStream>>;
}

#[async_trait]
pub trait ProviderStream: Send {
    async fn next_event(&mut self) -> Result<Option<ProviderEvent>>;
}

pub trait ModelCatalog: Send + Sync {
    fn list_models(&self) -> Vec<ModelMetadata>;
    fn get_model(&self, id: &str) -> Option<ModelMetadata>;
}

pub trait UsageAccounting: Send + Sync {
    fn total_tokens(&self, usage: &TokenUsage) -> u64;
}

pub trait ContextWindowResolver: Send + Sync {
    fn effective_context_window(&self, model: &str) -> Option<u64>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnvironmentAuthResolver;

#[derive(Clone, Debug, Default)]
pub struct StaticProviderStream {
    events: Vec<ProviderEvent>,
    cursor: usize,
}

impl StaticProviderStream {
    pub fn new(events: Vec<ProviderEvent>) -> Self {
        Self { events, cursor: 0 }
    }
}

#[async_trait]
impl ProviderStream for StaticProviderStream {
    async fn next_event(&mut self) -> Result<Option<ProviderEvent>> {
        if self.cursor >= self.events.len() {
            return Ok(None);
        }

        let event = self.events[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(event))
    }
}

#[derive(Clone, Debug)]
pub struct EchoProvider {
    provider: ApiProvider,
}

impl EchoProvider {
    pub fn new(provider: ApiProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn api_provider(&self) -> ApiProvider {
        self.provider
    }

    async fn start_stream(&self, request: ProviderRequest) -> Result<Box<dyn ProviderStream>> {
        if let Some(result) = request.messages.iter().rev().find_map(|message| {
            if message.role != MessageRole::Tool {
                return None;
            }
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::ToolResult { result } => Some(result.output_text.clone()),
                _ => None,
            })
        }) {
            let reply = format!("{} echo tool result: {}", self.provider, result);
            let usage = TokenUsage {
                input_tokens: result.split_whitespace().count() as u64,
                output_tokens: reply.split_whitespace().count() as u64,
                ..TokenUsage::default()
            };
            return Ok(Box::new(StaticProviderStream::new(vec![
                ProviderEvent::MessageDelta { text: reply },
                ProviderEvent::Usage { usage },
                ProviderEvent::Stop {
                    reason: "end_turn".to_owned(),
                },
            ])));
        }

        let prompt = request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, code_agent_core::MessageRole::User))
            .and_then(|message| {
                message.blocks.iter().find_map(|block| match block {
                    code_agent_core::ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "No user prompt provided.".to_owned());

        if let Some((name, input_json)) = parse_echo_tool_directive(&prompt) {
            if request.tools.iter().any(|tool| tool.name == name) {
                let usage = TokenUsage {
                    input_tokens: prompt.split_whitespace().count() as u64,
                    output_tokens: 0,
                    ..TokenUsage::default()
                };
                return Ok(Box::new(StaticProviderStream::new(vec![
                    ProviderEvent::MessageDelta {
                        text: format!("{} requesting tool {}", self.provider, name),
                    },
                    ProviderEvent::ToolCall {
                        call: ToolCall {
                            id: "echo_tool_call".to_owned(),
                            name,
                            input_json,
                        },
                    },
                    ProviderEvent::ToolCallBoundary {
                        id: "echo_tool_call".to_owned(),
                    },
                    ProviderEvent::Usage { usage },
                    ProviderEvent::Stop {
                        reason: "tool_use".to_owned(),
                    },
                ])));
            }
        }

        let reply = format!("{} echo: {}", self.provider, prompt);
        let usage = TokenUsage {
            input_tokens: prompt.split_whitespace().count() as u64,
            output_tokens: reply.split_whitespace().count() as u64,
            ..TokenUsage::default()
        };

        Ok(Box::new(StaticProviderStream::new(vec![
            ProviderEvent::MessageDelta { text: reply },
            ProviderEvent::Usage { usage },
            ProviderEvent::Stop {
                reason: "end_turn".to_owned(),
            },
        ])))
    }
}

fn parse_echo_tool_directive(prompt: &str) -> Option<(String, String)> {
    let body = prompt.trim().strip_prefix("tool:")?;
    let split_at = body
        .char_indices()
        .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx))?;
    let (name, rest) = body.split_at(split_at);
    let input_json = rest.trim();
    if name.is_empty() || input_json.is_empty() {
        return None;
    }
    Some((name.to_owned(), input_json.to_owned()))
}

#[derive(Clone, Debug)]
pub struct HttpProvider {
    provider: ApiProvider,
    auth: AuthMaterial,
    client: reqwest::Client,
    base_url: String,
}

impl HttpProvider {
    pub fn new(provider: ApiProvider, auth: AuthMaterial) -> Self {
        let base_url = provider_base_url(provider);
        Self {
            provider,
            auth,
            client: reqwest::Client::new(),
            base_url,
        }
    }

    pub fn with_base_url(provider: ApiProvider, auth: AuthMaterial, base_url: String) -> Self {
        Self {
            provider,
            auth,
            client: reqwest::Client::new(),
            base_url,
        }
    }

    async fn start_anthropic_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let url = join_api_path(&self.base_url, "messages", "v1");
        let mut payload = json!({
            "model": request.model,
            "stream": false,
            "max_tokens": request.max_output_tokens.unwrap_or(4_096),
            "messages": anthropic_messages(&request.messages),
        });

        if let Some(system) = anthropic_system_prompt(&request.messages) {
            payload["system"] = Value::String(system);
        }
        if !request.tools.is_empty() {
            payload["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.input_schema,
                        })
                    })
                    .collect(),
            );
        }

        let response = self
            .client
            .post(url)
            .headers(self.anthropic_headers(&request.extra_headers)?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "anthropic request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_openai_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let url = join_api_path(&self.base_url, "chat/completions", "v1");
        let mut payload = json!({
            "model": request.model,
            "stream": false,
            "messages": openai_messages(&request.messages),
        });

        if let Some(max_output_tokens) = request.max_output_tokens {
            payload["max_completion_tokens"] = Value::Number(max_output_tokens.into());
        }
        if !request.tools.is_empty() {
            payload["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.input_schema,
                            },
                        })
                    })
                    .collect(),
            );
            payload["tool_choice"] = Value::String("auto".to_owned());
        }

        let response = self
            .client
            .post(url)
            .headers(self.openai_headers(&request.extra_headers)?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "openai request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_openai_response(&value)?,
        )))
    }

    async fn start_bedrock_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Bedrock, &request.model);
        let mut payload = build_anthropic_payload(
            &request,
            Some((
                "anthropic_version",
                Value::String("bedrock-2023-05-31".to_owned()),
            )),
        );
        payload.as_object_mut().map(|object| object.remove("model"));
        let body = serde_json::to_vec(&payload)?;
        let url = bedrock_invoke_url(&self.base_url, &model);
        let headers = self
            .bedrock_headers(&url, &body, &request.extra_headers)
            .await?;

        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "bedrock request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_vertex_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Vertex, &request.model);
        let url = vertex_predict_url(&self.base_url, &model)?;
        let mut payload = build_anthropic_payload(
            &request,
            Some((
                "anthropic_version",
                Value::String("vertex-2023-10-16".to_owned()),
            )),
        );
        payload.as_object_mut().map(|object| object.remove("model"));

        let response = self
            .client
            .post(url)
            .headers(self.vertex_headers(&request.extra_headers).await?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "vertex request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_foundry_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Foundry, &request.model);
        let url = foundry_messages_url(&self.base_url)?;
        let mut request = request;
        request.model = model;
        let payload = build_anthropic_payload(
            &request,
            Some(("anthropic_version", Value::String("2023-06-01".to_owned()))),
        );

        let response = self
            .client
            .post(url)
            .headers(self.foundry_headers(&request.extra_headers).await?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "foundry request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    fn anthropic_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        if let Some(api_key) = self.auth.api_key.as_deref() {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(api_key)?,
            );
        } else if let Some(token) = self.auth.bearer_token.as_deref() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else {
            return Err(anyhow!("missing Anthropic auth material"));
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn bedrock_headers(
        &self,
        url: &str,
        payload: &[u8],
        extra_headers: &BTreeMap<String, String>,
    ) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if let Some(token) = self
            .auth
            .bearer_token
            .clone()
            .or_else(|| env_value(["AWS_BEARER_TOKEN_BEDROCK"]))
        {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            insert_extra_headers(&mut headers, extra_headers)?;
            return Ok(headers);
        }

        if env_flag("CLAUDE_CODE_SKIP_BEDROCK_AUTH") {
            insert_extra_headers(&mut headers, extra_headers)?;
            return Ok(headers);
        }

        let access_key = env_value(["AWS_ACCESS_KEY_ID"])
            .ok_or_else(|| anyhow!("missing AWS_ACCESS_KEY_ID for Bedrock runtime"))?;
        let secret_key = env_value(["AWS_SECRET_ACCESS_KEY"])
            .ok_or_else(|| anyhow!("missing AWS_SECRET_ACCESS_KEY for Bedrock runtime"))?;
        let session_token = env_value(["AWS_SESSION_TOKEN"]);
        let region = bedrock_region();
        let url = reqwest::Url::parse(url)?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("bedrock base URL is missing a host"))?;
        let path = url.path();
        let signed = sign_bedrock_request(
            "POST",
            host,
            path,
            payload,
            &region,
            &access_key,
            &secret_key,
            session_token.as_deref(),
            OffsetDateTime::now_utc(),
        )?;

        headers.insert(
            HeaderName::from_static("x-amz-date"),
            HeaderValue::from_str(&signed.amz_date)?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-content-sha256"),
            HeaderValue::from_str(&signed.payload_sha256)?,
        );
        if let Some(token) = signed.session_token {
            headers.insert(
                HeaderName::from_static("x-amz-security-token"),
                HeaderValue::from_str(&token)?,
            );
        }
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&signed.authorization)?);
        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn vertex_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if !env_flag("CLAUDE_CODE_SKIP_VERTEX_AUTH") {
            let token = self
                .auth
                .bearer_token
                .clone()
                .or_else(|| env_value(["VERTEX_ACCESS_TOKEN", "GOOGLE_OAUTH_ACCESS_TOKEN"]))
                .or(get_command_stdout(&[
                    ("gcloud", &["auth", "application-default", "print-access-token"][..]),
                    ("gcloud", &["auth", "print-access-token"][..]),
                ])
                .await)
                .ok_or_else(|| anyhow!("missing Vertex bearer token; set VERTEX_ACCESS_TOKEN or sign in with gcloud"))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn foundry_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        if let Some(api_key) = self
            .auth
            .api_key
            .clone()
            .or_else(|| env_value(["ANTHROPIC_FOUNDRY_API_KEY", "AZURE_API_KEY"]))
        {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(&api_key)?,
            );
        } else if !env_flag("CLAUDE_CODE_SKIP_FOUNDRY_AUTH") {
            let token = self
                .auth
                .bearer_token
                .clone()
                .or_else(|| env_value(["AZURE_AUTH_TOKEN", "FOUNDRY_AUTH_TOKEN"]))
                .or(get_command_stdout(&[
                    (
                        "az",
                        &[
                            "account",
                            "get-access-token",
                            "--resource",
                            "https://ai.azure.com",
                            "--query",
                            "accessToken",
                            "-o",
                            "tsv",
                        ][..],
                    ),
                ])
                .await)
                .ok_or_else(|| {
                    anyhow!(
                        "missing Foundry auth; set ANTHROPIC_FOUNDRY_API_KEY/AZURE_API_KEY or sign in with az"
                    )
                })?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    fn openai_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let bearer = self
            .auth
            .bearer_token
            .as_deref()
            .or(self.auth.api_key.as_deref())
            .ok_or_else(|| anyhow!("missing OpenAI auth material"))?;
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer}"))?,
        );

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }
}

#[async_trait]
impl Provider for HttpProvider {
    fn api_provider(&self) -> ApiProvider {
        self.provider
    }

    async fn start_stream(&self, request: ProviderRequest) -> Result<Box<dyn ProviderStream>> {
        match self.provider {
            ApiProvider::FirstParty => self.start_anthropic_stream(request).await,
            ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
                self.start_openai_stream(request).await
            }
            ApiProvider::Bedrock => self.start_bedrock_stream(request).await,
            ApiProvider::Vertex => self.start_vertex_stream(request).await,
            ApiProvider::Foundry => self.start_foundry_stream(request).await,
        }
    }
}

pub fn provider_base_url(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::FirstParty => env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_owned()),
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => env::var(
            "OPENAI_BASE_URL",
        )
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
        ApiProvider::Bedrock => env_value(["ANTHROPIC_BEDROCK_BASE_URL", "BEDROCK_BASE_URL"])
            .unwrap_or_else(|| format!("https://bedrock-runtime.{}.amazonaws.com", bedrock_region())),
        ApiProvider::Vertex => env_value(["ANTHROPIC_VERTEX_BASE_URL", "VERTEX_BASE_URL"]).unwrap_or_else(
            || {
                let location = vertex_region_for_model(None);
                let project = vertex_project_id().unwrap_or_else(|_| "unset-project".to_owned());
                format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}"
                )
            },
        ),
        ApiProvider::Foundry => env_value(["ANTHROPIC_FOUNDRY_BASE_URL", "FOUNDRY_BASE_URL"])
            .or_else(|| {
                env_value(["ANTHROPIC_FOUNDRY_RESOURCE"])
                    .map(|resource| format!("https://{resource}.services.ai.azure.com/anthropic"))
            })
            .unwrap_or_else(|| "https://foundry.unconfigured.local/anthropic".to_owned()),
    }
}

pub fn build_provider(provider: ApiProvider, auth: AuthMaterial) -> Box<dyn Provider> {
    Box::new(HttpProvider::new(provider, auth))
}

fn join_api_path(base_url: &str, suffix: &str, version_segment: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(&format!("/{version_segment}")) {
        format!("{trimmed}/{suffix}")
    } else {
        format!("{trimmed}/{version_segment}/{suffix}")
    }
}

fn join_if_missing(base_url: &str, suffix: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/{suffix}")
    }
}

fn env_value<const N: usize>(names: [&str; N]) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

fn resolve_provider_model(provider: ApiProvider, model: &str) -> String {
    let normalized = model.trim();
    if normalized.is_empty() {
        return model.to_owned();
    }

    let mapped = match provider {
        ApiProvider::Bedrock if normalized.starts_with("arn:aws:bedrock:") => normalized,
        ApiProvider::Bedrock if normalized.contains(".anthropic.") || normalized.contains(":") => {
            normalized
        }
        ApiProvider::Vertex if normalized.contains('@') => normalized,
        ApiProvider::Foundry if !normalized.starts_with("claude-") => normalized,
        ApiProvider::FirstParty
        | ApiProvider::OpenAI
        | ApiProvider::ChatGPTCodex
        | ApiProvider::OpenAICompatible => normalized,
        ApiProvider::Bedrock => match normalized {
            "claude-3-7-sonnet-20250219" => "us.anthropic.claude-3-7-sonnet-20250219-v1:0",
            "claude-3-5-sonnet-20241022" => "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "claude-3-5-haiku-20241022" => "us.anthropic.claude-3-5-haiku-20241022-v1:0",
            "claude-haiku-4-5" => "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-sonnet-4" => "us.anthropic.claude-sonnet-4-20250514-v1:0",
            "claude-sonnet-4-5" => "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "claude-sonnet-4-6" => "us.anthropic.claude-sonnet-4-6",
            "claude-opus-4" => "us.anthropic.claude-opus-4-20250514-v1:0",
            "claude-opus-4-1" => "us.anthropic.claude-opus-4-1-20250805-v1:0",
            "claude-opus-4-5" => "us.anthropic.claude-opus-4-5-20251101-v1:0",
            "claude-opus-4-6" => "us.anthropic.claude-opus-4-6-v1",
            _ => normalized,
        },
        ApiProvider::Vertex => match normalized {
            "claude-3-7-sonnet-20250219" => "claude-3-7-sonnet@20250219",
            "claude-3-5-sonnet-20241022" => "claude-3-5-sonnet-v2@20241022",
            "claude-3-5-haiku-20241022" => "claude-3-5-haiku@20241022",
            "claude-haiku-4-5" => "claude-haiku-4-5@20251001",
            "claude-sonnet-4" => "claude-sonnet-4@20250514",
            "claude-sonnet-4-5" => "claude-sonnet-4-5@20250929",
            "claude-sonnet-4-6" => "claude-sonnet-4-6",
            "claude-opus-4" => "claude-opus-4@20250514",
            "claude-opus-4-1" => "claude-opus-4-1@20250805",
            "claude-opus-4-5" => "claude-opus-4-5@20251101",
            "claude-opus-4-6" => "claude-opus-4-6",
            _ => normalized,
        },
        ApiProvider::Foundry => normalized,
    };

    mapped.to_owned()
}

fn build_anthropic_payload(
    request: &ProviderRequest,
    extra_body_field: Option<(&str, Value)>,
) -> Value {
    let mut payload = json!({
        "model": request.model,
        "stream": false,
        "max_tokens": request.max_output_tokens.unwrap_or(4_096),
        "messages": anthropic_messages(&request.messages),
    });

    if let Some(system) = anthropic_system_prompt(&request.messages) {
        payload["system"] = Value::String(system);
    }
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    if let Some((key, value)) = extra_body_field {
        payload[key] = value;
    }

    payload
}

fn bedrock_region() -> String {
    env_value(["AWS_REGION", "AWS_DEFAULT_REGION"]).unwrap_or_else(|| "us-east-1".to_owned())
}

fn vertex_region_for_model(model: Option<&str>) -> String {
    if let Some(model) = model {
        let normalized = model.trim();
        if !normalized.is_empty() {
            for (prefix, env_name) in [
                ("claude-haiku-4-5", "VERTEX_REGION_CLAUDE_HAIKU_4_5"),
                ("claude-3-5-haiku", "VERTEX_REGION_CLAUDE_3_5_HAIKU"),
                ("claude-3-5-sonnet", "VERTEX_REGION_CLAUDE_3_5_SONNET"),
                ("claude-3-7-sonnet", "VERTEX_REGION_CLAUDE_3_7_SONNET"),
                ("claude-opus-4-1", "VERTEX_REGION_CLAUDE_4_1_OPUS"),
                ("claude-opus-4", "VERTEX_REGION_CLAUDE_4_0_OPUS"),
                ("claude-sonnet-4-6", "VERTEX_REGION_CLAUDE_4_6_SONNET"),
                ("claude-sonnet-4-5", "VERTEX_REGION_CLAUDE_4_5_SONNET"),
                ("claude-sonnet-4", "VERTEX_REGION_CLAUDE_4_0_SONNET"),
            ] {
                if normalized.starts_with(prefix) {
                    if let Some(region) = env_value([env_name]) {
                        return region;
                    }
                }
            }
        }
    }

    env_value(["CLOUD_ML_REGION", "VERTEX_REGION"]).unwrap_or_else(|| "us-east5".to_owned())
}

fn vertex_project_id() -> Result<String> {
    env_value([
        "GCLOUD_PROJECT",
        "GOOGLE_CLOUD_PROJECT",
        "gcloud_project",
        "google_cloud_project",
        "ANTHROPIC_VERTEX_PROJECT_ID",
    ])
    .ok_or_else(|| {
        anyhow!(
            "missing Vertex project id; set GOOGLE_CLOUD_PROJECT or ANTHROPIC_VERTEX_PROJECT_ID"
        )
    })
}

fn bedrock_invoke_url(base_url: &str, model: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.contains("/model/") && trimmed.ends_with("/invoke") {
        return trimmed.to_owned();
    }
    let encoded_model = utf8_percent_encode(model, URI_COMPONENT_ENCODE_SET).to_string();
    format!("{trimmed}/model/{encoded_model}/invoke")
}

fn vertex_predict_url(base_url: &str, model: &str) -> Result<String> {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(":rawPredict") || trimmed.ends_with(":streamRawPredict") {
        return Ok(trimmed.replace(":streamRawPredict", ":rawPredict"));
    }
    if trimmed.contains("/publishers/anthropic/models/") {
        return Ok(format!("{trimmed}:rawPredict"));
    }
    if trimmed.ends_with("/publishers/anthropic/models") {
        return Ok(format!("{trimmed}/{model}:rawPredict"));
    }
    if trimmed.contains("/projects/") && trimmed.contains("/locations/") {
        return Ok(format!(
            "{trimmed}/publishers/anthropic/models/{model}:rawPredict"
        ));
    }

    let location = vertex_region_for_model(Some(model));
    let project = vertex_project_id()?;
    Ok(format!(
        "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:rawPredict"
    ))
}

fn foundry_messages_url(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1/messages") {
        return Ok(trimmed.to_owned());
    }
    if trimmed.ends_with("/anthropic") {
        return Ok(join_api_path(trimmed, "messages", "v1"));
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Ok(join_if_missing(trimmed, "anthropic/v1/messages"));
    }
    Err(anyhow!("invalid Foundry base URL: {trimmed}"))
}

#[derive(Clone, Debug)]
struct SignedBedrockRequest {
    authorization: String,
    amz_date: String,
    payload_sha256: String,
    session_token: Option<String>,
}

fn hash_hex(payload: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(payload);
    hex::encode(digest.finalize())
}

fn hmac_sha256(key: &[u8], message: &str) -> Result<Vec<u8>> {
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| anyhow!("invalid HMAC signing key"))?;
    mac.update(message.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn sign_bedrock_request(
    method: &str,
    host: &str,
    path: &str,
    payload: &[u8],
    region: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
    now: OffsetDateTime,
) -> Result<SignedBedrockRequest> {
    let amz_date = now.format(&format_description!(
        "[year][month][day]T[hour][minute][second]Z"
    ))?;
    let date_stamp = now.format(&format_description!("[year][month][day]"))?;
    let payload_sha256 = hash_hex(payload);

    let mut canonical_headers = vec![
        format!("accept:application/json\n"),
        format!("content-type:application/json\n"),
        format!("host:{host}\n"),
        format!("x-amz-content-sha256:{payload_sha256}\n"),
        format!("x-amz-date:{amz_date}\n"),
    ];
    let mut signed_headers = vec![
        "accept",
        "content-type",
        "host",
        "x-amz-content-sha256",
        "x-amz-date",
    ];
    let session_token = session_token.map(str::to_owned);
    if let Some(token) = session_token.as_deref() {
        canonical_headers.push(format!("x-amz-security-token:{token}\n"));
        signed_headers.push("x-amz-security-token");
    }

    let canonical_request = format!(
        "{method}\n{path}\n\n{}{}\n{}",
        canonical_headers.join(""),
        signed_headers.join(";"),
        payload_sha256
    );
    let canonical_request_hash = hash_hex(canonical_request.as_bytes());
    let service = env::var("BEDROCK_SIGNING_NAME").unwrap_or_else(|_| "bedrock".to_owned());
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

    let k_date = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), &date_stamp)?;
    let k_region = hmac_sha256(&k_date, region)?;
    let k_service = hmac_sha256(&k_region, &service)?;
    let k_signing = hmac_sha256(&k_service, "aws4_request")?;
    let signature = hex::encode(hmac_sha256(&k_signing, &string_to_sign)?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={}, Signature={signature}",
        signed_headers.join(";")
    );

    Ok(SignedBedrockRequest {
        authorization,
        amz_date,
        payload_sha256,
        session_token,
    })
}

async fn get_command_stdout(candidates: &[(&str, &[&str])]) -> Option<String> {
    for (program, args) in candidates {
        let output = match Command::new(program).args(args.iter()).output().await {
            Ok(output) => output,
            Err(_) => continue,
        };
        if !output.status.success() {
            continue;
        }
        let stdout = match String::from_utf8(output.stdout) {
            Ok(stdout) => stdout,
            Err(_) => continue,
        };
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn compact_error_body(body: &str) -> String {
    let trimmed = body.trim();
    let mut compact = trimmed.replace('\n', " ");
    if compact.len() > 240 {
        compact.truncate(240);
        compact.push_str("...");
    }
    compact
}

fn insert_extra_headers(
    headers: &mut HeaderMap,
    extra_headers: &BTreeMap<String, String>,
) -> Result<()> {
    for (key, value) in extra_headers {
        headers.insert(
            HeaderName::from_bytes(key.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(())
}

fn message_text(message: &Message) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_tool_input(input_json: &str) -> Value {
    serde_json::from_str(input_json).unwrap_or_else(|_| json!({}))
}

fn anthropic_system_prompt(messages: &[Message]) -> Option<String> {
    let parts = messages
        .iter()
        .filter(|message| matches!(message.role, MessageRole::System))
        .map(message_text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();

    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        let role = match message.role {
            MessageRole::System | MessageRole::Attachment => continue,
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "user",
        };

        let content = anthropic_content_blocks(message);
        if content.is_empty() {
            continue;
        }

        encoded.push(json!({
            "role": role,
            "content": content,
        }));
    }

    encoded
}

fn anthropic_content_blocks(message: &Message) -> Vec<Value> {
    let mut content = Vec::new();

    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }
            }
            ContentBlock::ToolCall { call } => {
                content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": parse_tool_input(&call.input_json),
                }));
            }
            ContentBlock::ToolResult { result } => {
                content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": result.tool_call_id,
                    "content": result.output_text,
                    "is_error": result.is_error,
                }));
            }
            ContentBlock::Attachment { .. } | ContentBlock::Boundary { .. } => {}
        }
    }

    content
}

fn openai_messages(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        match message.role {
            MessageRole::System => {
                let text = message_text(message);
                if !text.is_empty() {
                    encoded.push(json!({ "role": "system", "content": text }));
                }
            }
            MessageRole::User => {
                let text = message_text(message);
                if !text.is_empty() {
                    encoded.push(json!({ "role": "user", "content": text }));
                }
            }
            MessageRole::Assistant => {
                let text = message_text(message);
                let tool_calls = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolCall { call } => Some(json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.input_json,
                            },
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !text.is_empty() || !tool_calls.is_empty() {
                    let mut entry = json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    });
                    if !tool_calls.is_empty() {
                        entry["tool_calls"] = Value::Array(tool_calls);
                    }
                    encoded.push(entry);
                }
            }
            MessageRole::Tool => {
                for block in &message.blocks {
                    if let ContentBlock::ToolResult { result } = block {
                        encoded.push(json!({
                            "role": "tool",
                            "tool_call_id": result.tool_call_id,
                            "content": result.output_text,
                        }));
                    }
                }
            }
            MessageRole::Attachment => {}
        }
    }

    encoded
}

fn events_from_anthropic_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    let mut events = Vec::new();

    for block in value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("anthropic response missing content array"))?
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !text.is_empty() {
                    events.push(ProviderEvent::MessageDelta { text });
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("anthropic tool_use block missing id"))?
                    .to_owned();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("anthropic tool_use block missing name"))?
                    .to_owned();
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                events.push(ProviderEvent::ToolCall {
                    call: ToolCall {
                        id: id.clone(),
                        name,
                        input_json: serde_json::to_string(&input)?,
                    },
                });
                events.push(ProviderEvent::ToolCallBoundary { id });
            }
            Some(other) => {
                events.push(ProviderEvent::Error {
                    message: format!("unsupported Anthropic content block type: {other}"),
                });
            }
            None => {}
        }
    }

    if let Some(usage) = anthropic_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    events.push(ProviderEvent::Stop {
        reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned(),
    });

    Ok(events)
}

fn anthropic_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_read_input_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

fn events_from_openai_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow!("openai response missing choices"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| anyhow!("openai response missing assistant message"))?;

    let mut events = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            events.push(ProviderEvent::MessageDelta {
                text: text.to_owned(),
            });
        }
    }
    for tool_call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("openai tool call missing id"))?
            .to_owned();
        let function = tool_call
            .get("function")
            .ok_or_else(|| anyhow!("openai tool call missing function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("openai tool call missing function name"))?
            .to_owned();
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_owned();
        events.push(ProviderEvent::ToolCall {
            call: ToolCall {
                id: id.clone(),
                name,
                input_json: arguments,
            },
        });
        events.push(ProviderEvent::ToolCallBoundary { id });
    }

    if let Some(usage) = openai_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    events.push(ProviderEvent::Stop {
        reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop")
            .to_owned(),
    });

    Ok(events)
}

fn openai_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub provider: ApiProvider,
    pub display_name: String,
    pub supports_streaming: bool,
    pub supports_tool_use: bool,
    pub supports_reasoning: bool,
    pub requires_cloud_auth: bool,
}

#[derive(Clone, Debug, Default)]
pub struct StaticModelCatalog {
    models: Vec<ModelMetadata>,
}

impl StaticModelCatalog {
    pub fn new(models: Vec<ModelMetadata>) -> Self {
        Self { models }
    }
}

impl ModelCatalog for StaticModelCatalog {
    fn list_models(&self) -> Vec<ModelMetadata> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<ModelMetadata> {
        self.models.iter().find(|model| model.id == id).cloned()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DefaultUsageAccounting;

impl UsageAccounting for DefaultUsageAccounting {
    fn total_tokens(&self, usage: &TokenUsage) -> u64 {
        usage.input_tokens
            + usage.output_tokens
            + usage.cache_creation_input_tokens
            + usage.cache_read_input_tokens
    }
}

#[derive(Clone, Debug, Default)]
pub struct StaticContextWindowResolver {
    by_model: BTreeMap<String, u64>,
}

impl StaticContextWindowResolver {
    pub fn new(models: &[ModelMetadata]) -> Self {
        let by_model = models
            .iter()
            .filter_map(|model| {
                model
                    .context_window
                    .map(|window| (model.id.clone(), window))
            })
            .collect();
        Self { by_model }
    }
}

impl ContextWindowResolver for StaticContextWindowResolver {
    fn effective_context_window(&self, model: &str) -> Option<u64> {
        self.by_model.get(model).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAIAuthSource {
    #[serde(rename = "OPENAI_API_KEY")]
    OpenAiApiKey,
    #[serde(rename = "codex_auth_api_key")]
    CodexAuthApiKey,
    #[serde(rename = "codex_auth_token")]
    CodexAuthToken,
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAITokenFreshness {
    Fresh,
    Stale,
    Expired,
    Missing,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexAuthTokens {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexAuthFile {
    pub auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<CodexAuthTokens>,
    pub last_refresh: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenClaims {
    pub exp: Option<u64>,
    pub email: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAIAuthStatus {
    pub has_credentials: bool,
    pub source: OpenAIAuthSource,
    pub api_key: Option<String>,
    pub bearer_token: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub auth_mode: Option<String>,
    pub token_freshness: OpenAITokenFreshness,
}

impl Default for OpenAIAuthStatus {
    fn default() -> Self {
        Self {
            has_credentials: false,
            source: OpenAIAuthSource::None,
            api_key: None,
            bearer_token: None,
            email: None,
            account_id: None,
            auth_mode: None,
            token_freshness: OpenAITokenFreshness::Missing,
        }
    }
}

pub fn is_openai_provider(provider: ApiProvider) -> bool {
    matches!(
        provider,
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
    )
}

pub fn read_codex_auth_file(path: Option<&std::path::Path>) -> Option<CodexAuthFile> {
    let path = path.map(PathBuf::from).unwrap_or_else(codex_auth_file_path);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<CodexAuthFile>(&raw).ok()
}

pub fn read_auth_snapshot() -> Option<BTreeMap<String, AuthMaterial>> {
    let raw = fs::read_to_string(code_agent_auth_snapshot_path()).ok()?;
    serde_json::from_str::<AuthSnapshotFile>(&raw)
        .ok()
        .map(|snapshot| snapshot.providers)
}

pub fn write_auth_snapshot(provider: ApiProvider, auth: &AuthMaterial) -> Result<PathBuf> {
    let path = code_agent_auth_snapshot_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut providers = read_auth_snapshot().unwrap_or_default();
    providers.insert(provider.as_str().to_owned(), auth.clone());
    fs::write(
        &path,
        serde_json::to_vec_pretty(&AuthSnapshotFile { providers })?,
    )?;
    Ok(path)
}

pub fn clear_auth_snapshot(provider: ApiProvider) -> Result<bool> {
    let path = code_agent_auth_snapshot_path();
    if !path.exists() {
        return Ok(false);
    }
    let mut providers = read_auth_snapshot().unwrap_or_default();
    let removed = providers.remove(provider.as_str()).is_some();
    fs::write(
        &path,
        serde_json::to_vec_pretty(&AuthSnapshotFile { providers })?,
    )?;
    Ok(removed)
}

pub fn read_provider_auth_snapshot(provider: ApiProvider) -> Option<AuthMaterial> {
    read_auth_snapshot()?.remove(provider.as_str())
}

fn decode_base64_url(input: &str) -> Option<Vec<u8>> {
    let mut sanitized = input.replace('-', "+").replace('_', "/");
    while sanitized.len() % 4 != 0 {
        sanitized.push('=');
    }

    let mut output = Vec::with_capacity((sanitized.len() / 4) * 3);
    let mut chunk = [0u8; 4];
    let mut filled = 0usize;

    for byte in sanitized.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => return None,
        };

        chunk[filled] = value;
        filled += 1;

        if filled == 4 {
            output.push((chunk[0] << 2) | (chunk[1] >> 4));
            if chunk[2] != 64 {
                output.push((chunk[1] << 4) | (chunk[2] >> 2));
            }
            if chunk[3] != 64 {
                output.push((chunk[2] << 6) | chunk[3]);
            }
            filled = 0;
        }
    }

    Some(output)
}

pub fn decode_jwt_claims(token: Option<&str>) -> Option<TokenClaims> {
    let token = token?;
    let payload = token.split('.').nth(1)?;
    let decoded = decode_base64_url(payload)?;
    serde_json::from_slice::<TokenClaims>(&decoded).ok()
}

pub fn get_token_freshness(token: Option<&str>) -> OpenAITokenFreshness {
    let Some(token) = token else {
        return OpenAITokenFreshness::Missing;
    };
    let Some(claims) = decode_jwt_claims(Some(token)) else {
        return OpenAITokenFreshness::Missing;
    };
    let Some(exp) = claims.exp else {
        return OpenAITokenFreshness::Fresh;
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();

    if exp <= now {
        OpenAITokenFreshness::Expired
    } else if exp <= now + 60 {
        OpenAITokenFreshness::Stale
    } else {
        OpenAITokenFreshness::Fresh
    }
}

pub fn get_openai_auth_status(provider: ApiProvider) -> OpenAIAuthStatus {
    let auth = read_codex_auth_file(None);
    let auth_mode = auth.as_ref().and_then(|entry| entry.auth_mode.clone());

    if provider == ApiProvider::ChatGPTCodex {
        if auth.as_ref().and_then(|entry| entry.auth_mode.as_deref()) != Some("chatgpt") {
            return OpenAIAuthStatus {
                auth_mode,
                ..OpenAIAuthStatus::default()
            };
        }

        let access_token = auth
            .as_ref()
            .and_then(|entry| entry.tokens.as_ref())
            .and_then(|tokens| tokens.access_token.clone());
        let refresh_token = auth
            .as_ref()
            .and_then(|entry| entry.tokens.as_ref())
            .and_then(|tokens| tokens.refresh_token.clone());

        if access_token.is_none() || refresh_token.is_none() {
            return OpenAIAuthStatus {
                auth_mode,
                token_freshness: get_token_freshness(access_token.as_deref()),
                ..OpenAIAuthStatus::default()
            };
        }

        let claims = decode_jwt_claims(access_token.as_deref());
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthToken,
            api_key: None,
            bearer_token: access_token.clone(),
            email: claims.and_then(|value| value.email),
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: get_token_freshness(access_token.as_deref()),
        };
    }

    if let Ok(api_key) = env::var("OPENAI_API_KEY") {
        if !api_key.trim().is_empty() {
            return OpenAIAuthStatus {
                has_credentials: true,
                source: OpenAIAuthSource::OpenAiApiKey,
                api_key: Some(api_key.clone()),
                bearer_token: Some(api_key),
                email: None,
                account_id: None,
                auth_mode,
                token_freshness: OpenAITokenFreshness::Fresh,
            };
        }
    }

    if let Some(api_key) = auth.as_ref().and_then(|entry| entry.openai_api_key.clone()) {
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthApiKey,
            api_key: Some(api_key.clone()),
            bearer_token: Some(api_key),
            email: None,
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: OpenAITokenFreshness::Fresh,
        };
    }

    let access_token = auth
        .as_ref()
        .and_then(|entry| entry.tokens.as_ref())
        .and_then(|tokens| tokens.access_token.clone());
    if access_token.is_some() && provider != ApiProvider::OpenAICompatible {
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthToken,
            api_key: None,
            bearer_token: access_token.clone(),
            email: decode_jwt_claims(access_token.as_deref()).and_then(|claims| claims.email),
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: get_token_freshness(access_token.as_deref()),
        };
    }

    OpenAIAuthStatus {
        auth_mode,
        ..OpenAIAuthStatus::default()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigMigrationReport {
    pub provider: ApiProvider,
    pub env: BTreeMap<String, String>,
    pub auth_snapshot_path: Option<PathBuf>,
    pub codex_auth_path: Option<PathBuf>,
}

pub fn config_migration_report(provider: ApiProvider) -> ConfigMigrationReport {
    let mut env = BTreeMap::new();
    if let Ok(value) = env::var("CLAUDE_CODE_API_PROVIDER") {
        if !value.trim().is_empty() {
            env.insert("CLAUDE_CODE_API_PROVIDER".to_owned(), value);
        }
    }
    for key in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_ORG_ID",
        "AZURE_OPENAI_API_KEY",
        "AWS_REGION",
        "AWS_ACCESS_KEY_ID",
        "VERTEXAI_PROJECT",
        "VERTEXAI_LOCATION",
    ] {
        if let Ok(value) = env::var(key) {
            if !value.trim().is_empty() {
                env.insert(key.to_owned(), value);
            }
        }
    }

    let auth_snapshot_path = code_agent_auth_snapshot_path();
    let codex_auth_path = codex_auth_file_path();
    ConfigMigrationReport {
        provider,
        env,
        auth_snapshot_path: auth_snapshot_path.exists().then_some(auth_snapshot_path),
        codex_auth_path: codex_auth_path.exists().then_some(codex_auth_path),
    }
}

pub fn get_openai_credential_hint(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::ChatGPTCodex => format!(
            "Run official 'codex login' so {} contains auth_mode=chatgpt and refreshable tokens.",
            codex_auth_file_path().display()
        ),
        ApiProvider::OpenAICompatible => "Set OPENAI_API_KEY and OPENAI_BASE_URL.".to_owned(),
        ApiProvider::OpenAI => format!(
            "Set OPENAI_API_KEY or sign in with Codex so {} exists.",
            codex_auth_file_path().display()
        ),
        _ => "Provider does not use OpenAI-family credentials.".to_owned(),
    }
}

pub fn get_anthropic_auth_material(provider: ApiProvider) -> Option<AuthMaterial> {
    if provider != ApiProvider::FirstParty {
        return None;
    }

    if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
        if !api_key.trim().is_empty() {
            return Some(AuthMaterial {
                api_key: Some(api_key),
                source: Some("ANTHROPIC_API_KEY".to_owned()),
                ..AuthMaterial::default()
            });
        }
    }

    for token_var in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
        if let Ok(token) = env::var(token_var) {
            if !token.trim().is_empty() {
                return Some(AuthMaterial {
                    bearer_token: Some(token),
                    source: Some(token_var.to_owned()),
                    ..AuthMaterial::default()
                });
            }
        }
    }

    None
}

pub fn get_anthropic_credential_hint(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::FirstParty => {
            "Set ANTHROPIC_API_KEY or provide CLAUDE_CODE_OAUTH_TOKEN.".to_owned()
        }
        ApiProvider::Bedrock => "Provide AWS credentials and region configuration.".to_owned(),
        ApiProvider::Vertex => {
            "Provide Google Cloud credentials and Vertex region configuration.".to_owned()
        }
        ApiProvider::Foundry => {
            "Provide Azure Foundry credentials and deployment configuration.".to_owned()
        }
        _ => "Provider does not use Anthropic-family credentials.".to_owned(),
    }
}

#[async_trait]
impl AuthResolver for EnvironmentAuthResolver {
    async fn resolve_auth(&self, request: AuthRequest) -> Result<AuthMaterial> {
        match request.provider {
            ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
                let status = get_openai_auth_status(request.provider);
                if status.has_credentials {
                    return Ok(AuthMaterial {
                        api_key: status.api_key,
                        bearer_token: status.bearer_token,
                        source: Some(
                            match status.source {
                                OpenAIAuthSource::OpenAiApiKey => "OPENAI_API_KEY",
                                OpenAIAuthSource::CodexAuthApiKey => "codex_auth_api_key",
                                OpenAIAuthSource::CodexAuthToken => "codex_auth_token",
                                OpenAIAuthSource::None => "none",
                            }
                            .to_owned(),
                        ),
                        ..AuthMaterial::default()
                    });
                }
                read_provider_auth_snapshot(request.provider)
                    .ok_or_else(|| anyhow!(get_openai_credential_hint(request.provider)))
            }
            ApiProvider::FirstParty => get_anthropic_auth_material(request.provider)
                .or_else(|| read_provider_auth_snapshot(request.provider))
                .ok_or_else(|| anyhow!(get_anthropic_credential_hint(request.provider))),
            ApiProvider::Bedrock | ApiProvider::Vertex | ApiProvider::Foundry => Ok(AuthMaterial {
                source: Some("ambient_cloud_auth".to_owned()),
                ..AuthMaterial::default()
            }),
        }
    }
}

pub fn provider_descriptor(provider: ApiProvider) -> ProviderDescriptor {
    match provider {
        ApiProvider::FirstParty => ProviderDescriptor {
            provider,
            display_name: "Anthropic".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Bedrock => ProviderDescriptor {
            provider,
            display_name: "Amazon Bedrock".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Vertex => ProviderDescriptor {
            provider,
            display_name: "Google Vertex AI".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Foundry => ProviderDescriptor {
            provider,
            display_name: "Microsoft Foundry".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::OpenAI => ProviderDescriptor {
            provider,
            display_name: "OpenAI".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: false,
        },
        ApiProvider::ChatGPTCodex => ProviderDescriptor {
            provider,
            display_name: "ChatGPT Codex".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::OpenAICompatible => ProviderDescriptor {
            provider,
            display_name: "OpenAI-Compatible".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: false,
        },
    }
}

pub fn compatibility_models_for(provider: ApiProvider) -> Vec<ModelMetadata> {
    match provider {
        ApiProvider::FirstParty
        | ApiProvider::Bedrock
        | ApiProvider::Vertex
        | ApiProvider::Foundry => vec![
            ModelMetadata {
                id: "claude-sonnet-4-6".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "claude-opus-4-1".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "claude-haiku-4-5".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(16_000),
                supports_tool_use: true,
                supports_reasoning: false,
            },
        ],
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => vec![
            ModelMetadata {
                id: "gpt-5".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "gpt-5-mini".to_owned(),
                provider: provider.to_string(),
                context_window: Some(128_000),
                max_output_tokens: Some(16_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "codex-mini-latest".to_owned(),
                provider: provider.to_string(),
                context_window: Some(128_000),
                max_output_tokens: Some(16_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
        ],
    }
}

pub fn compatibility_model_catalog(provider: ApiProvider) -> StaticModelCatalog {
    StaticModelCatalog::new(compatibility_models_for(provider))
}

pub async fn collect_provider_response(
    provider: &dyn Provider,
    request: ProviderRequest,
) -> Result<CollectedProviderResponse> {
    let mut stream = provider.start_stream(request).await?;
    let mut collected = CollectedProviderResponse::default();

    while let Some(event) = stream.next_event().await? {
        match event {
            ProviderEvent::MessageDelta { text } => collected.text.push_str(&text),
            ProviderEvent::ToolCall { call } => collected.tool_calls.push(call),
            ProviderEvent::Usage { usage } => collected.usage = Some(usage),
            ProviderEvent::Stop { reason } => {
                collected.stop_reason = Some(reason);
                break;
            }
            ProviderEvent::ToolCallBoundary { .. } | ProviderEvent::Error { .. } => {}
        }
    }

    Ok(collected)
}

pub async fn collect_provider_text(
    provider: &dyn Provider,
    request: ProviderRequest,
) -> Result<(String, Option<TokenUsage>)> {
    let collected = collect_provider_response(provider, request).await?;
    Ok((collected.text, collected.usage))
}

#[cfg(test)]
mod tests {
    use super::{
        codex_auth_file_path, collect_provider_response, collect_provider_text,
        compatibility_model_catalog, decode_jwt_claims, events_from_anthropic_response,
        events_from_openai_response, get_anthropic_auth_material, get_openai_auth_status,
        get_openai_credential_hint, is_openai_provider, provider_descriptor, resolve_api_provider,
        resolve_provider_model, sign_bedrock_request, ApiProvider, AuthMaterial, AuthRequest,
        AuthResolver, EchoProvider, EnvironmentAuthResolver, HttpProvider, ModelCatalog,
        OpenAIAuthSource, ProviderRequest, ProviderToolDefinition,
    };
    use code_agent_core::{ContentBlock, Message, MessageRole};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::env;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use time::{Date, Month, PrimitiveDateTime, Time};

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn fixture_json(relative: &str) -> serde_json::Value {
        let path = workspace_root().join(relative);
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug)]
    struct CapturedHttpRequest {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    async fn spawn_json_server(
        response_body: serde_json::Value,
    ) -> (String, std::thread::JoinHandle<CapturedHttpRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body_string = serde_json::to_string(&response_body).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];

            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header_end = position + 4;
                    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
                    let content_length = header_text
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while buffer.len() < header_end + content_length {
                        let read = stream.read(&mut chunk).unwrap();
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    if buffer.len() < header_end + content_length {
                        break;
                    }
                    let request_text = String::from_utf8_lossy(&buffer);
                    let mut lines = request_text.lines();
                    let request_line = lines.next().unwrap();
                    let mut request_line_parts = request_line.split_whitespace();
                    let method = request_line_parts.next().unwrap().to_owned();
                    let path = request_line_parts.next().unwrap().to_owned();
                    let mut headers = BTreeMap::new();
                    for line in lines.by_ref() {
                        if line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                        }
                    }
                    let body =
                        String::from_utf8_lossy(&buffer[header_end..header_end + content_length])
                            .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body_string.len(),
                        body_string
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    return CapturedHttpRequest {
                        method,
                        path,
                        headers,
                        body,
                    };
                }
            }

            panic!("server did not receive a complete HTTP request")
        });

        (format!("http://{address}"), handle)
    }

    fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let previous = env::var(key).ok();
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
        f();
        match previous {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        f()
    }

    #[test]
    fn resolves_provider_from_env_flags() {
        with_env_lock(|| {
            with_env_var("CLAUDE_CODE_API_PROVIDER", None, || {
                with_env_var("CLAUDE_CODE_USE_BEDROCK", Some("1"), || {
                    let provider =
                        resolve_api_provider(None).expect("provider resolution should work");
                    assert_eq!(provider, ApiProvider::Bedrock);
                });
            });
        });
    }

    #[test]
    fn uses_codex_home_for_auth_path() {
        with_env_lock(|| {
            with_env_var("CODEX_HOME", Some("/tmp/codex-home"), || {
                assert_eq!(
                    codex_auth_file_path(),
                    PathBuf::from("/tmp/codex-home/auth.json")
                );
            });
        });
    }

    #[test]
    fn detects_openai_api_key_auth() {
        with_env_lock(|| {
            with_env_var("OPENAI_API_KEY", Some("test-key"), || {
                let status = get_openai_auth_status(ApiProvider::OpenAI);
                assert_eq!(status.source, OpenAIAuthSource::OpenAiApiKey);
                assert!(status.has_credentials);
            });
        });
    }

    #[test]
    fn reads_chatgpt_codex_auth_snapshot() {
        with_env_lock(|| {
            let root = env::temp_dir().join(format!("codex-auth-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("temp dir should exist");
            let auth_path = root.join("auth.json");
            fs::write(
                &auth_path,
                r#"{
  "auth_mode":"chatgpt",
  "tokens":{
    "access_token":"header.eyJleHAiOjQ3MDAwMDAwMDAsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "refresh_token":"refresh-token",
    "account_id":"acct_123"
  }
}"#,
            )
            .expect("auth file should be written");

            with_env_var(
                "CODEX_HOME",
                Some(root.to_str().expect("utf8 path")),
                || {
                    let status = get_openai_auth_status(ApiProvider::ChatGPTCodex);
                    assert!(status.has_credentials);
                    assert_eq!(status.source, OpenAIAuthSource::CodexAuthToken);
                    assert_eq!(status.account_id.as_deref(), Some("acct_123"));
                },
            );
        });
    }

    #[test]
    fn decodes_minimal_jwt_claims() {
        let claims = decode_jwt_claims(Some(
            "header.eyJleHAiOjQ3MDAwMDAwMDAsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
        ))
        .expect("claims should decode");
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn reports_config_migration_inputs() {
        with_env_lock(|| {
            with_env_var("CLAUDE_CODE_API_PROVIDER", Some("openai"), || {
                with_env_var("OPENAI_API_KEY", Some("test-key"), || {
                    let report = super::config_migration_report(ApiProvider::OpenAI);
                    assert_eq!(report.provider, ApiProvider::OpenAI);
                    assert_eq!(report.env.get("CLAUDE_CODE_API_PROVIDER"), Some(&"openai".to_owned()));
                    assert_eq!(report.env.get("OPENAI_API_KEY"), Some(&"test-key".to_owned()));
                });
            });
        });
    }

    #[test]
    fn openai_provider_hint_mentions_expected_setup() {
        assert!(is_openai_provider(ApiProvider::OpenAICompatible));
        assert!(get_openai_credential_hint(ApiProvider::ChatGPTCodex).contains("codex login"));
    }

    #[test]
    fn exposes_provider_descriptors_and_model_catalogs() {
        let descriptor = provider_descriptor(ApiProvider::Foundry);
        let catalog = compatibility_model_catalog(ApiProvider::OpenAI);

        assert_eq!(descriptor.display_name, "Microsoft Foundry");
        assert!(descriptor.supports_tool_use);
        assert!(catalog.get_model("gpt-5").is_some());
    }

    #[test]
    fn reads_anthropic_env_auth_material() {
        with_env_lock(|| {
            with_env_var("ANTHROPIC_API_KEY", Some("anthropic-key"), || {
                let material = get_anthropic_auth_material(ApiProvider::FirstParty).unwrap();
                assert_eq!(material.api_key.as_deref(), Some("anthropic-key"));
                assert_eq!(material.source.as_deref(), Some("ANTHROPIC_API_KEY"));
            });
        });
    }

    #[tokio::test]
    async fn resolves_auth_from_environment() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = env::var("OPENAI_API_KEY").ok();
        env::set_var("OPENAI_API_KEY", "openai-key");

        let resolver = EnvironmentAuthResolver;
        let auth = resolver
            .resolve_auth(AuthRequest {
                provider: ApiProvider::OpenAI,
                profile: None,
            })
            .await
            .unwrap();

        match previous {
            Some(value) => env::set_var("OPENAI_API_KEY", value),
            None => env::remove_var("OPENAI_API_KEY"),
        }

        assert_eq!(auth.api_key.as_deref(), Some("openai-key"));
    }

    #[tokio::test]
    async fn streams_echo_provider_text() {
        let provider = EchoProvider::new(ApiProvider::OpenAI);
        let request = ProviderRequest {
            model: "gpt-5".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "Summarize the diff".to_owned(),
                }],
            )],
            ..ProviderRequest::default()
        };

        let (text, usage) = collect_provider_text(&provider, request).await.unwrap();

        assert!(text.contains("openai echo"));
        assert_eq!(usage.unwrap().input_tokens, 3);
    }

    #[tokio::test]
    async fn collects_tool_calls_from_provider_stream() {
        let provider = EchoProvider::new(ApiProvider::OpenAI);
        let request = ProviderRequest {
            model: "gpt-5".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "tool:file_read {\"path\":\"src/main.rs\"}".to_owned(),
                }],
            )],
            tools: vec![ProviderToolDefinition {
                name: "file_read".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: json!({"type":"object"}),
            }],
            ..ProviderRequest::default()
        };

        let collected = collect_provider_response(&provider, request).await.unwrap();

        assert!(collected.text.contains("requesting tool file_read"));
        assert_eq!(collected.tool_calls.len(), 1);
        assert_eq!(collected.tool_calls[0].name, "file_read");
    }

    #[test]
    fn parses_anthropic_tool_use_response() {
        let events = events_from_anthropic_response(&fixture_json(
            "fixtures/provider-streams/anthropic_tool_use.json",
        ))
        .unwrap();

        assert!(
            matches!(&events[0], super::ProviderEvent::MessageDelta { text } if text.contains("inspect"))
        );
        assert!(
            matches!(&events[1], super::ProviderEvent::ToolCall { call } if call.name == "file_read")
        );
        assert!(
            matches!(&events[3], super::ProviderEvent::Usage { usage } if usage.output_tokens == 7)
        );
    }

    #[test]
    fn parses_openai_tool_call_response() {
        let events = events_from_openai_response(&fixture_json(
            "fixtures/provider-streams/openai_tool_call.json",
        ))
        .unwrap();

        assert!(
            matches!(&events[0], super::ProviderEvent::MessageDelta { text } if text == "Need to search.")
        );
        assert!(
            matches!(&events[1], super::ProviderEvent::ToolCall { call } if call.id == "call_123")
        );
        assert!(
            matches!(&events[3], super::ProviderEvent::Usage { usage } if usage.input_tokens == 19)
        );
    }

    #[test]
    fn maps_provider_specific_model_ids() {
        assert_eq!(
            resolve_provider_model(ApiProvider::Bedrock, "claude-opus-4-1"),
            "us.anthropic.claude-opus-4-1-20250805-v1:0"
        );
        assert_eq!(
            resolve_provider_model(ApiProvider::Vertex, "claude-opus-4-1"),
            "claude-opus-4-1@20250805"
        );
        assert_eq!(
            resolve_provider_model(ApiProvider::Foundry, "claude-opus-4-1"),
            "claude-opus-4-1"
        );
    }

    #[test]
    fn signs_bedrock_requests_with_sigv4() {
        let timestamp = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::January, 2).unwrap(),
            Time::from_hms(3, 4, 5).unwrap(),
        )
        .assume_utc();
        let signed = sign_bedrock_request(
            "POST",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "/model/us.anthropic.claude-sonnet-4-6/invoke",
            br#"{"anthropic_version":"bedrock-2023-05-31"}"#,
            "us-east-1",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            Some("session-token"),
            timestamp,
        )
        .unwrap();

        assert!(signed
            .authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/"));
        assert_eq!(signed.amz_date, "20260102T030405Z");
        assert_eq!(signed.session_token.as_deref(), Some("session-token"));
        assert_eq!(signed.payload_sha256.len(), 64);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sends_foundry_messages_requests() {
        let (base_url, server) = spawn_json_server(json!({
            "content": [{ "type": "text", "text": "foundry ok" }],
            "usage": { "input_tokens": 8, "output_tokens": 2 },
            "stop_reason": "end_turn"
        }))
        .await;
        let provider = HttpProvider::with_base_url(
            ApiProvider::Foundry,
            AuthMaterial {
                api_key: Some("foundry-key".to_owned()),
                ..AuthMaterial::default()
            },
            format!("{base_url}/anthropic"),
        );
        let request = ProviderRequest {
            model: "claude-sonnet-4-6".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello foundry".to_owned(),
                }],
            )],
            ..ProviderRequest::default()
        };

        let collected = collect_provider_response(&provider, request).await.unwrap();
        let captured = server.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

        assert_eq!(collected.text, "foundry ok");
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/anthropic/v1/messages");
        assert_eq!(
            captured.headers.get("x-api-key").map(String::as_str),
            Some("foundry-key")
        );
        assert_eq!(
            captured
                .headers
                .get("anthropic-version")
                .map(String::as_str),
            Some("2023-06-01")
        );
        assert_eq!(body["model"], "claude-sonnet-4-6");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sends_vertex_raw_predict_requests() {
        let (base_url, server) = spawn_json_server(json!({
            "content": [{ "type": "text", "text": "vertex ok" }],
            "usage": { "input_tokens": 10, "output_tokens": 2 },
            "stop_reason": "end_turn"
        }))
        .await;
        let provider = HttpProvider::with_base_url(
            ApiProvider::Vertex,
            AuthMaterial {
                bearer_token: Some("vertex-token".to_owned()),
                ..AuthMaterial::default()
            },
            format!("{base_url}/v1/projects/demo/locations/us-east5"),
        );
        let request = ProviderRequest {
            model: "claude-opus-4-1".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello vertex".to_owned(),
                }],
            )],
            ..ProviderRequest::default()
        };

        let collected = collect_provider_response(&provider, request).await.unwrap();
        let captured = server.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

        assert_eq!(collected.text, "vertex ok");
        assert_eq!(captured.method, "POST");
        assert!(captured
            .path
            .ends_with("/publishers/anthropic/models/claude-opus-4-1@20250805:rawPredict"));
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer vertex-token")
        );
        assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
        assert!(body.get("model").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sends_bedrock_invoke_requests() {
        let (base_url, server) = spawn_json_server(json!({
            "content": [{ "type": "text", "text": "bedrock ok" }],
            "usage": { "input_tokens": 9, "output_tokens": 2 },
            "stop_reason": "end_turn"
        }))
        .await;
        let provider = HttpProvider::with_base_url(
            ApiProvider::Bedrock,
            AuthMaterial {
                bearer_token: Some("bedrock-token".to_owned()),
                ..AuthMaterial::default()
            },
            base_url,
        );
        let request = ProviderRequest {
            model: "claude-haiku-4-5".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello bedrock".to_owned(),
                }],
            )],
            ..ProviderRequest::default()
        };

        let collected = collect_provider_response(&provider, request).await.unwrap();
        let captured = server.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

        assert_eq!(collected.text, "bedrock ok");
        assert_eq!(captured.method, "POST");
        assert!(captured
            .path
            .ends_with("/model/us.anthropic.claude-haiku-4-5-20251001-v1%3A0/invoke"));
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer bedrock-token")
        );
        assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
        assert!(body.get("model").is_none());
    }
}
