use anyhow::{anyhow, Result};
use async_trait::async_trait;
use code_agent_core::{ContentBlock, Message, MessageRole, TokenUsage, ToolCall};
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use time::macros::format_description;
use time::OffsetDateTime;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

type HmacSha256 = Hmac<Sha256>;

const URI_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');
const OPENAI_AUTH_URL: &str = "https://auth.openai.com/oauth/token";
const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const OPENAI_RESPONSES_DEFAULT_MAX_RETRIES: usize = 10;
const OPENAI_RESPONSES_BASE_DELAY_MS: u64 = 500;
const OPENAI_RESPONSES_MAX_DELAY_MS: u64 = 32_000;
const OPENAI_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: u64 = 60;
const CODEX_AUTH_LOCK_RETRIES: usize = 5;
const CODEX_AUTH_LOCK_MIN_DELAY_MS: u64 = 50;
const CODEX_AUTH_LOCK_MAX_DELAY_MS: u64 = 250;

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

#[derive(Clone, Debug, Default, Deserialize)]
struct ChatGPTCodexModelsCache {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    models: Vec<ChatGPTCodexModelCacheEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ChatGPTCodexModelCacheEntry {
    slug: String,
    #[serde(default)]
    supported_in_api: bool,
    #[serde(default)]
    supports_reasoning_summaries: bool,
    #[serde(default)]
    support_verbosity: bool,
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

/// Thinking configuration for a provider request.
///
/// Claude and OpenAI use fundamentally different thinking parameter shapes:
/// - Claude: `adaptive` or `enabled` with a token budget
/// - OpenAI: `reasoning_effort` string level (low/medium/high/xhigh)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThinkingConfig {
    /// Model decides how much thinking to use (Claude 4.6+ models).
    Adaptive,
    /// Explicit thinking token budget (older Claude models).
    Enabled { budget_tokens: u64 },
    /// Thinking is disabled.
    Disabled,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ProviderToolDefinition>,
    pub extra_headers: BTreeMap<String, String>,
    pub max_output_tokens: Option<u64>,
    pub thinking: ThinkingConfig,
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

pub struct OpenAIResponsesSseStream {
    response: reqwest::Response,
    buffer: String,
    pending: VecDeque<ProviderEvent>,
    completed: bool,
    saw_text_delta: bool,
}

impl OpenAIResponsesSseStream {
    fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            buffer: String::new(),
            pending: VecDeque::new(),
            completed: false,
            saw_text_delta: false,
        }
    }

    fn drain_buffer(&mut self, finalize: bool) -> Result<()> {
        while let Some(separator_index) = self.buffer.find("\n\n") {
            let raw_event = self.buffer[..separator_index].to_owned();
            self.buffer = self.buffer[separator_index + 2..].to_owned();
            if let Some(value) = parse_openai_sse_event(&raw_event)? {
                let events = provider_events_from_openai_sse_event(
                    &value,
                    &mut self.saw_text_delta,
                    &mut self.completed,
                )?;
                self.pending.extend(events);
            }
            if self.completed {
                self.buffer.clear();
                return Ok(());
            }
        }

        if finalize {
            let trailing = self.buffer.trim().to_owned();
            self.buffer.clear();
            if !trailing.is_empty() {
                if !self.completed
                    && self.pending.is_empty()
                    && !self.saw_text_delta
                    && !trailing.contains("data:")
                    && (trailing.starts_with('{') || trailing.starts_with('['))
                {
                    let value: Value = serde_json::from_str(&trailing)?;
                    self.pending.extend(events_from_openai_response(&value)?);
                    self.completed = true;
                    return Ok(());
                }

                if let Some(value) = parse_openai_sse_event(&trailing)? {
                    let events = provider_events_from_openai_sse_event(
                        &value,
                        &mut self.saw_text_delta,
                        &mut self.completed,
                    )?;
                    self.pending.extend(events);
                }
            }
            if !self.completed {
                return Err(anyhow!(
                    "responses stream completed without a response.completed event"
                ));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ProviderStream for OpenAIResponsesSseStream {
    async fn next_event(&mut self) -> Result<Option<ProviderEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if self.completed {
                return Ok(None);
            }

            match self.response.chunk().await? {
                Some(chunk) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    if self.buffer.contains("\r\n") {
                        self.buffer = self.buffer.replace("\r\n", "\n");
                    }
                    self.drain_buffer(false)?;
                }
                None => {
                    self.drain_buffer(true)?;
                }
            }
        }
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
        // Wire Claude thinking config into the API payload.
        match &request.thinking {
            ThinkingConfig::Adaptive => {
                payload["thinking"] = json!({ "type": "adaptive" });
            }
            ThinkingConfig::Enabled { budget_tokens } => {
                payload["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": budget_tokens,
                });
            }
            ThinkingConfig::Disabled => {}
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

    async fn start_openai_responses_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        if matches!(self.provider, ApiProvider::OpenAICompatible)
            && openai_compatible_uses_chat_completions(&self.base_url)
        {
            let url = join_if_missing(&self.base_url, "chat/completions");
            return self
                .start_openai_chat_completions_stream(url, request)
                .await;
        }
        let url = join_if_missing(&self.base_url, "responses");
        self.send_openai_responses_request(url, request, true, true)
            .await
    }

    async fn start_openai_chat_completions_stream(
        &self,
        url: String,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let payload = build_openai_chat_completions_payload(&request);
        let response = self
            .client
            .post(&url)
            .headers(self.openai_headers(&request.extra_headers)?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "{} request failed with status {}: {}",
                openai_request_failure_label(self.provider),
                status,
                compact_error_body(&body)
            ));
        }

        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_openai_chat_response(&value)?,
        )))
    }

    async fn start_chatgpt_codex_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let url = join_if_missing(&self.base_url, "codex/responses");
        let mut request = request;
        let mut supports_reasoning_summaries = false;
        let mut supports_verbosity = false;

        if let Some(cache) = read_chatgpt_codex_models_cache() {
            if let Some(etag) = cache.etag {
                request
                    .extra_headers
                    .entry("x-models-etag".to_owned())
                    .or_insert(etag);
            }

            if let Some(model) = cache
                .models
                .iter()
                .find(|entry| entry.slug == request.model)
            {
                supports_reasoning_summaries = model.supports_reasoning_summaries;
                supports_verbosity = model.support_verbosity;
                if !model.supported_in_api {
                    return Err(anyhow!(
                        "ChatGPT Codex model '{}' is not available through the Codex API",
                        request.model
                    ));
                }
            }
        }

        self.send_openai_responses_request(
            url,
            request,
            supports_reasoning_summaries,
            supports_verbosity,
        )
        .await
    }

    async fn send_openai_responses_request(
        &self,
        url: String,
        request: ProviderRequest,
        supports_reasoning_summaries: bool,
        supports_verbosity: bool,
    ) -> Result<Box<dyn ProviderStream>> {
        let payload = build_openai_responses_payload(
            &request,
            supports_reasoning_summaries,
            supports_verbosity,
        );

        let max_retries = openai_responses_max_retries();
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let response = self
                .client
                .post(&url)
                .headers(self.openai_headers(&request.extra_headers)?)
                .json(&payload)
                .send()
                .await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            if !status.is_success() {
                let body = response.text().await?;
                if should_retry_openai_responses_status(status) && attempt <= max_retries {
                    sleep(openai_responses_retry_delay(
                        attempt,
                        retry_after.as_deref(),
                    ))
                    .await;
                    continue;
                }
                return Err(anyhow!(
                    "{} request failed with status {}: {}",
                    openai_request_failure_label(self.provider),
                    status,
                    compact_error_body(&body)
                ));
            }

            return Ok(Box::new(OpenAIResponsesSseStream::new(response)));
        }
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
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("code-agent-rust/", env!("CARGO_PKG_VERSION"))),
        );

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
            ApiProvider::OpenAI | ApiProvider::OpenAICompatible => {
                self.start_openai_responses_stream(request).await
            }
            ApiProvider::ChatGPTCodex => self.start_chatgpt_codex_stream(request).await,
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
        ApiProvider::OpenAI => "https://api.openai.com/v1".to_owned(),
        ApiProvider::ChatGPTCodex => CHATGPT_CODEX_BASE_URL.to_owned(),
        ApiProvider::OpenAICompatible => env::var("OPENAI_BASE_URL")
            .map(|value| value.trim_end_matches('/').to_owned())
            .unwrap_or_else(|_| "https://openai-compatible.invalid".to_owned()),
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

fn openai_compatible_uses_chat_completions(base_url: &str) -> bool {
    if env_flag_truthy("OPENAI_COMPAT_CHAT_COMPLETIONS") {
        return true;
    }

    base_url
        .to_ascii_lowercase()
        .contains("generativelanguage.googleapis.com")
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
    if let Some(message) = extract_error_message(body) {
        return truncate_error_text(&message, 600);
    }

    let trimmed = body.trim();
    let compact = trimmed.replace('\n', " ");
    truncate_error_text(&compact, 600)
}

fn truncate_error_text(text: &str, max_len: usize) -> String {
    let mut compact = text.trim().to_owned();
    if compact.len() > max_len {
        compact.truncate(max_len);
        compact.push_str("...");
    }
    compact
}

fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let mut fragments = Vec::new();

    for pointer in [
        "/detail",
        "/message",
        "/error/message",
        "/response/error/message",
        "/error/errors",
        "/error",
        "/response/error",
    ] {
        if let Some(target) = value.pointer(pointer) {
            collect_error_fragments(target, &mut fragments);
        }
    }

    if fragments.is_empty() {
        collect_error_fragments(&value, &mut fragments);
    }

    (!fragments.is_empty()).then(|| fragments.join(" "))
}

fn collect_error_fragments(value: &Value, fragments: &mut Vec<String>) {
    match value {
        Value::String(message) => push_error_fragment(fragments, message),
        Value::Array(items) => {
            for item in items {
                collect_error_fragments(item, fragments);
            }
        }
        Value::Object(object) => {
            let location = object.get("loc").and_then(format_error_location);
            if let Some(message) = object
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| object.get("msg").and_then(Value::as_str))
                .or_else(|| object.get("reason").and_then(Value::as_str))
                .or_else(|| object.get("error_description").and_then(Value::as_str))
            {
                if let Some(location) = location {
                    push_error_fragment(fragments, format!("{location}: {message}"));
                } else {
                    push_error_fragment(fragments, message);
                }
            }

            for key in ["detail", "error", "errors"] {
                if let Some(child) = object.get(key) {
                    collect_error_fragments(child, fragments);
                }
            }
        }
        _ => {}
    }
}

fn format_error_location(value: &Value) -> Option<String> {
    let location = value
        .as_array()?
        .iter()
        .filter_map(|part| match part {
            Value::String(text) => Some(text.trim().to_owned()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!location.is_empty()).then(|| location.join("."))
}

fn push_error_fragment(fragments: &mut Vec<String>, fragment: impl AsRef<str>) {
    let fragment = fragment.as_ref().trim();
    if fragment.is_empty() {
        return;
    }
    if !fragments.iter().any(|existing| existing == fragment) {
        fragments.push(fragment.to_owned());
    }
}

fn openai_request_failure_label(provider: ApiProvider) -> &'static str {
    match provider {
        ApiProvider::ChatGPTCodex => "ChatGPT Codex",
        ApiProvider::OpenAICompatible => "OpenAI-compatible",
        _ => "OpenAI",
    }
}

fn read_chatgpt_codex_models_cache() -> Option<ChatGPTCodexModelsCache> {
    let path = codex_home_dir().join("models_cache.json");
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn openai_responses_max_retries() -> usize {
    env::var("CLAUDE_CODE_MAX_RETRIES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(OPENAI_RESPONSES_DEFAULT_MAX_RETRIES)
}

fn should_retry_openai_responses_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::CONFLICT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn openai_responses_retry_delay(attempt: usize, retry_after_header: Option<&str>) -> Duration {
    if let Some(retry_after_header) = retry_after_header {
        if let Ok(seconds) = retry_after_header.trim().parse::<u64>() {
            return Duration::from_secs(seconds);
        }
    }

    let shift = attempt.saturating_sub(1).min(16) as u32;
    let delay_ms = OPENAI_RESPONSES_BASE_DELAY_MS
        .saturating_mul(1u64 << shift)
        .min(OPENAI_RESPONSES_MAX_DELAY_MS);
    Duration::from_millis(delay_ms)
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

fn build_openai_responses_payload(
    request: &ProviderRequest,
    supports_reasoning_summaries: bool,
    supports_verbosity: bool,
) -> Value {
    let mut payload = json!({
        "model": request.model,
        "instructions": anthropic_system_prompt(&request.messages)
            .unwrap_or_else(|| "You are Codex.".to_owned()),
        "input": openai_responses_input(&request.messages),
        "stream": true,
        "store": false,
    });

    if let Some(max_output_tokens) = request.max_output_tokens {
        payload["max_output_tokens"] = Value::Number(max_output_tokens.into());
    }
    if request.thinking != ThinkingConfig::Disabled {
        let mut reasoning = json!({
            "effort": resolve_reasoning_effort(&request.model),
        });
        if supports_reasoning_summaries {
            reasoning["summary"] = Value::String("auto".to_owned());
        }
        payload["reasoning"] = reasoning;
    }
    if supports_verbosity {
        payload["text"] = json!({
            "verbosity": "medium",
        });
    }
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = Value::String("auto".to_owned());
    }

    payload
}

fn build_openai_chat_completions_payload(request: &ProviderRequest) -> Value {
    let mut payload = json!({
        "model": request.model,
        "messages": openai_chat_messages(&request.messages),
        "stream": false,
    });

    if request.thinking != ThinkingConfig::Disabled {
        payload["reasoning_effort"] = Value::String(resolve_reasoning_effort(&request.model));
    }

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
                        }
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = Value::String("auto".to_owned());
    }

    payload
}

fn openai_chat_messages(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        match message.role {
            MessageRole::System => {
                let text = message_text(message);
                if !text.trim().is_empty() {
                    encoded.push(json!({
                        "role": "system",
                        "content": text,
                    }));
                }
            }
            MessageRole::User => {
                let text = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        ContentBlock::Attachment { attachment } => {
                            Some(format!("[Attachment omitted: {}]", attachment.name))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    encoded.push(json!({
                        "role": "user",
                        "content": text,
                    }));
                }
            }
            MessageRole::Assistant => {
                let mut item = json!({
                    "role": "assistant",
                });
                let text = message_text(message);
                if !text.trim().is_empty() {
                    item["content"] = Value::String(text);
                }
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
                            }
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !tool_calls.is_empty() {
                    item["tool_calls"] = Value::Array(tool_calls);
                }
                if item.get("content").is_some() || item.get("tool_calls").is_some() {
                    encoded.push(item);
                }
            }
            MessageRole::Tool => {
                for result in message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolResult { result } => Some(result),
                    _ => None,
                }) {
                    encoded.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_call_id,
                        "content": result.output_text,
                    }));
                }
            }
            MessageRole::Attachment => {}
        }
    }

    encoded
}

fn openai_responses_input(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        match message.role {
            MessageRole::System => {}
            MessageRole::User => {
                let content = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            Some(json!({ "type": "input_text", "text": text }))
                        }
                        ContentBlock::Attachment { attachment } => Some(json!({
                            "type": "input_text",
                            "text": format!("[Attachment omitted: {}]", attachment.name),
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    encoded.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            MessageRole::Assistant => {
                let text = message_text(message);
                if !text.is_empty() {
                    encoded.push(json!({
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": text,
                        }],
                    }));
                }
                encoded.extend(message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolCall { call } => Some(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.input_json,
                    })),
                    _ => None,
                }));
            }
            MessageRole::Tool => {
                encoded.extend(message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolResult { result } => Some(json!({
                        "type": "function_call_output",
                        "call_id": result.tool_call_id,
                        "output": result.output_text,
                    })),
                    _ => None,
                }));
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
    if value.get("choices").is_some() {
        return events_from_openai_chat_response(value);
    }
    events_from_openai_responses_response(value)
}

fn parse_openai_sse_event(raw_event: &str) -> Result<Option<Value>> {
    let data = raw_event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }

    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| anyhow!("failed to parse OpenAI Responses SSE event: {error}"))?;
    Ok(Some(value))
}

fn provider_events_from_openai_sse_event(
    value: &Value,
    saw_text_delta: &mut bool,
    completed: &mut bool,
) -> Result<Vec<ProviderEvent>> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    *saw_text_delta = true;
                    return Ok(vec![ProviderEvent::MessageDelta {
                        text: delta.to_owned(),
                    }]);
                }
            }
            Ok(Vec::new())
        }
        Some("response.completed") => {
            *completed = true;
            let response = value
                .get("response")
                .ok_or_else(|| anyhow!("response.completed event missing response payload"))?;
            events_from_openai_responses_response_parts(response, !*saw_text_delta)
        }
        Some("error") | Some("response.failed") => Err(anyhow!(openai_sse_error_message(value))),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
fn events_from_openai_sse_body(body: &str) -> Result<Vec<ProviderEvent>> {
    let normalized = body.replace("\r\n", "\n");
    let mut events = Vec::new();
    let mut completed = false;
    let mut saw_text_delta = false;

    for raw_event in normalized.split("\n\n") {
        if let Some(value) = parse_openai_sse_event(raw_event)? {
            events.extend(provider_events_from_openai_sse_event(
                &value,
                &mut saw_text_delta,
                &mut completed,
            )?);
        }
    }

    if !completed {
        return Err(anyhow!(
            "responses stream completed without a response.completed event"
        ));
    }
    Ok(events)
}

fn openai_sse_error_message(value: &Value) -> String {
    for pointer in [
        "/error/message",
        "/response/error/message",
        "/detail",
        "/message",
    ] {
        if let Some(message) = value.pointer(pointer).and_then(Value::as_str) {
            if !message.trim().is_empty() {
                return message.to_owned();
            }
        }
    }

    compact_error_body(&value.to_string())
}

fn events_from_openai_chat_response(value: &Value) -> Result<Vec<ProviderEvent>> {
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

fn events_from_openai_responses_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    events_from_openai_responses_response_parts(value, true)
}

fn events_from_openai_responses_response_parts(
    value: &Value,
    include_text: bool,
) -> Result<Vec<ProviderEvent>> {
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        if !message.trim().is_empty() {
            return Err(anyhow!(message.to_owned()));
        }
    }

    let mut events = Vec::new();

    for item in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") if include_text => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                events.push(ProviderEvent::MessageDelta {
                                    text: text.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("responses function_call missing id"))?
                    .to_owned();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("responses function_call missing name"))?
                    .to_owned();
                let arguments = item
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
            _ => {}
        }
    }

    if let Some(usage) = openai_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    let stop_reason = if events
        .iter()
        .any(|event| matches!(event, ProviderEvent::ToolCall { .. }))
    {
        "tool_use".to_owned()
    } else if value
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        == Some("max_output_tokens")
    {
        "max_tokens".to_owned()
    } else {
        "end_turn".to_owned()
    };
    events.push(ProviderEvent::Stop {
        reason: stop_reason,
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
    pub aud: Option<Value>,
    pub client_id: Option<String>,
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

fn openai_auth_url() -> String {
    env::var("OPENAI_AUTH_URL").unwrap_or_else(|_| OPENAI_AUTH_URL.to_owned())
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

fn is_jwt_expired(token: Option<&str>, skew_seconds: u64) -> bool {
    let Some(exp) = decode_jwt_claims(token).and_then(|claims| claims.exp) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    exp <= now + skew_seconds
}

pub fn get_token_freshness(token: Option<&str>) -> OpenAITokenFreshness {
    let Some(token) = token else {
        return OpenAITokenFreshness::Missing;
    };
    if is_jwt_expired(Some(token), 0) {
        OpenAITokenFreshness::Expired
    } else if is_jwt_expired(Some(token), OPENAI_ACCESS_TOKEN_REFRESH_SKEW_SECONDS) {
        OpenAITokenFreshness::Stale
    } else {
        OpenAITokenFreshness::Fresh
    }
}

#[derive(Debug)]
struct CodexAuthFileLock {
    path: PathBuf,
}

impl Drop for CodexAuthFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn codex_auth_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

async fn acquire_codex_auth_file_lock(path: &Path) -> Result<CodexAuthFileLock> {
    let lock_path = codex_auth_lock_path(path);
    for attempt in 0..=CODEX_AUTH_LOCK_RETRIES {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(CodexAuthFileLock { path: lock_path }),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if attempt == CODEX_AUTH_LOCK_RETRIES {
                    break;
                }
                let delay_ms =
                    (CODEX_AUTH_LOCK_MIN_DELAY_MS << attempt).min(CODEX_AUTH_LOCK_MAX_DELAY_MS);
                sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(error) => {
                return Err(anyhow!(
                    "failed to acquire Codex auth lock {}: {}",
                    lock_path.display(),
                    error
                ));
            }
        }
    }

    Err(anyhow!(
        "timed out acquiring Codex auth lock {}",
        lock_path.display()
    ))
}

fn refresh_client_id(auth: &CodexAuthFile) -> Option<String> {
    if let Some(client_id) = decode_jwt_claims(auth.tokens.as_ref()?.access_token.as_deref())
        .and_then(|claims| claims.client_id)
    {
        return Some(client_id);
    }

    match decode_jwt_claims(auth.tokens.as_ref()?.id_token.as_deref()).and_then(|claims| claims.aud)
    {
        Some(Value::String(aud)) if !aud.trim().is_empty() => Some(aud),
        Some(Value::Array(audiences)) => audiences.into_iter().find_map(|aud| match aud {
            Value::String(value) if !value.trim().is_empty() => Some(value),
            _ => None,
        }),
        _ => None,
    }
}

fn write_codex_auth_file(path: &Path, auth: &CodexAuthFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let mut payload = serde_json::to_vec_pretty(auth)?;
    payload.push(b'\n');
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn format_oauth_error_detail(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
        None => None,
    }
}

async fn refresh_codex_access_token(path: Option<&Path>) -> Result<Option<String>> {
    let path = path.map(PathBuf::from).unwrap_or_else(codex_auth_file_path);
    let _lock = acquire_codex_auth_file_lock(path.as_path()).await?;
    let Some(auth) = read_codex_auth_file(Some(path.as_path())) else {
        return Ok(None);
    };
    let Some(tokens) = auth.tokens.as_ref() else {
        return Ok(None);
    };
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Ok(None);
    };

    if tokens.access_token.is_some() && !is_jwt_expired(tokens.access_token.as_deref(), 10) {
        return Ok(tokens.access_token.clone());
    }

    let client_id = refresh_client_id(&auth)
        .ok_or_else(|| anyhow!("unable to determine the OpenAI OAuth client_id"))?;
    let client = reqwest::Client::new();
    let response = client
        .post(openai_auth_url())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    let value: Option<Value> = serde_json::from_str(&body).ok();
    let access_token = value
        .as_ref()
        .and_then(|json| json.get("access_token"))
        .and_then(Value::as_str);
    if !status.is_success() || access_token.is_none() {
        let detail = format_oauth_error_detail(
            value
                .as_ref()
                .and_then(|json| json.get("error_description")),
        )
        .or_else(|| format_oauth_error_detail(value.as_ref().and_then(|json| json.get("error"))))
        .unwrap_or_else(|| compact_error_body(&body));
        return Err(anyhow!("failed to refresh Codex auth token: {detail}"));
    }

    let mut next_auth = auth;
    if let Some(tokens) = next_auth.tokens.as_mut() {
        tokens.access_token = access_token.map(str::to_owned);
        if let Some(refresh_token) = value
            .as_ref()
            .and_then(|json| json.get("refresh_token"))
            .and_then(Value::as_str)
        {
            tokens.refresh_token = Some(refresh_token.to_owned());
        }
        if let Some(id_token) = value
            .as_ref()
            .and_then(|json| json.get("id_token"))
            .and_then(Value::as_str)
        {
            tokens.id_token = Some(id_token.to_owned());
        }
    }
    next_auth.last_refresh =
        Some(OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?);
    write_codex_auth_file(path.as_path(), &next_auth)?;
    Ok(access_token.map(str::to_owned))
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
                    let mut api_key = status.api_key.clone();
                    let mut bearer_token = status.bearer_token.clone();
                    let source = status.source.clone();

                    if source == OpenAIAuthSource::CodexAuthToken {
                        let needs_refresh = matches!(
                            status.token_freshness,
                            OpenAITokenFreshness::Stale | OpenAITokenFreshness::Expired
                        );
                        if needs_refresh {
                            bearer_token = refresh_codex_access_token(None).await?;
                        }
                    }

                    if request.provider == ApiProvider::ChatGPTCodex
                        && source != OpenAIAuthSource::CodexAuthToken
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    if request.provider == ApiProvider::OpenAICompatible
                        && !matches!(
                            source,
                            OpenAIAuthSource::OpenAiApiKey | OpenAIAuthSource::CodexAuthApiKey
                        )
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }
                    if request.provider == ApiProvider::OpenAICompatible
                        && env::var("OPENAI_BASE_URL")
                            .ok()
                            .map(|value| value.trim().is_empty())
                            .unwrap_or(true)
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    if source == OpenAIAuthSource::CodexAuthToken && bearer_token.is_none() {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    return Ok(AuthMaterial {
                        api_key: api_key.take(),
                        bearer_token,
                        source: Some(
                            match source {
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
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
            openai_family_compatibility_models(provider)
        }
    }
}

fn openai_family_compatibility_models(provider: ApiProvider) -> Vec<ModelMetadata> {
    let provider_name = provider.to_string();
    let mut models = Vec::new();
    let mut push_unique = |model: ModelMetadata| {
        if models
            .iter()
            .any(|existing: &ModelMetadata| existing.id == model.id)
        {
            return;
        }
        models.push(model);
    };

    push_unique(ModelMetadata {
        id: get_openai_reasoning_model(),
        provider: provider_name.clone(),
        context_window: Some(200_000),
        max_output_tokens: Some(32_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });
    push_unique(ModelMetadata {
        id: get_openai_completion_model(),
        provider: provider_name.clone(),
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: false,
    });
    push_unique(ModelMetadata {
        id: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        provider: provider_name.clone(),
        context_window: Some(200_000),
        max_output_tokens: Some(32_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });
    push_unique(ModelMetadata {
        id: DEFAULT_OPENAI_COMPLETION_MODEL.to_owned(),
        provider: provider_name.clone(),
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: false,
    });
    push_unique(ModelMetadata {
        id: "codex-mini-latest".to_owned(),
        provider: provider_name,
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });

    models
}

pub fn compatibility_model_catalog(provider: ApiProvider) -> StaticModelCatalog {
    StaticModelCatalog::new(compatibility_models_for(provider))
}

// ---------------------------------------------------------------------------
// Model overrides and thinking configuration (parity with TS)
// ---------------------------------------------------------------------------

/// Default OpenAI reasoning model (used for thinking-enabled turns).
pub const DEFAULT_OPENAI_REASONING_MODEL: &str = "gpt-5.4";

/// Default OpenAI completion model (used for standard/utility turns).
pub const DEFAULT_OPENAI_COMPLETION_MODEL: &str = "gpt-5.3-codex";

/// Default max thinking token budget for Claude models.
pub const DEFAULT_MAX_THINKING_TOKENS: u64 = 10_000;

/// Valid think-level values for OpenAI reasoning effort.
pub const OPENAI_THINK_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// Returns the OpenAI reasoning model, honouring the `REASONING_MODEL` env var.
pub fn get_openai_reasoning_model() -> String {
    env::var("REASONING_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_REASONING_MODEL.to_owned())
}

/// Returns the OpenAI completion model, honouring the `COMPLETION_MODEL` env var.
pub fn get_openai_completion_model() -> String {
    env::var("COMPLETION_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_COMPLETION_MODEL.to_owned())
}

/// Parse a think-level env var value into a validated level string.
fn parse_think_level(value: Option<String>, fallback: &str) -> String {
    match value
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
    {
        Some(v) if OPENAI_THINK_LEVELS.contains(&v.as_str()) => v,
        Some(_) => fallback.to_owned(),
        None => fallback.to_owned(),
    }
}

/// Returns the reasoning think level from `REASONING_MODEL_THINK` (default: `xhigh`).
pub fn get_openai_reasoning_think_level() -> String {
    parse_think_level(env::var("REASONING_MODEL_THINK").ok(), "xhigh")
}

/// Returns the completion think level from `COMPLETION_MODEL_THINK` (default: `xhigh`).
pub fn get_openai_completion_think_level() -> String {
    parse_think_level(env::var("COMPLETION_MODEL_THINK").ok(), "xhigh")
}

fn env_flag_truthy(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Claude-specific thinking helpers (parity with TS thinking.ts)
// ---------------------------------------------------------------------------

/// Check if a Claude model supports thinking at all.
/// Claude 4+ models support thinking. Claude 3.x models do not.
pub fn model_supports_thinking(model: &str, provider: ApiProvider) -> bool {
    let lower = model.to_lowercase();
    match provider {
        // OpenAI family always supports "thinking" via reasoning_effort
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => true,
        // 1P and Foundry: all Claude 4+ models
        ApiProvider::FirstParty | ApiProvider::Foundry => !lower.contains("claude-3-"),
        // 3P (Bedrock / Vertex): only Opus 4+ and Sonnet 4+
        ApiProvider::Bedrock | ApiProvider::Vertex => {
            lower.contains("sonnet-4") || lower.contains("opus-4")
        }
    }
}

/// Check if a Claude model supports adaptive thinking (newer 4.6+ models).
pub fn model_supports_adaptive_thinking(model: &str, provider: ApiProvider) -> bool {
    match provider {
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => true,
        _ => {
            let lower = model.to_lowercase();
            if lower.contains("opus-4-6") || lower.contains("sonnet-4-6") {
                return true;
            }
            // Known legacy models do not support adaptive
            if lower.contains("opus") || lower.contains("sonnet") || lower.contains("haiku") {
                return false;
            }
            // Default: true for 1P/Foundry (newer models), false for 3P
            matches!(provider, ApiProvider::FirstParty | ApiProvider::Foundry)
        }
    }
}

/// Resolve the `ThinkingConfig` for a Claude request.
///
/// Reads `CLAUDE_CODE_DISABLE_THINKING`, `CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING`,
/// and `MAX_THINKING_TOKENS` to determine the correct thinking config.
pub fn resolve_claude_thinking_config(
    model: &str,
    provider: ApiProvider,
    max_output_tokens: u64,
) -> ThinkingConfig {
    if env_flag_truthy("CLAUDE_CODE_DISABLE_THINKING") {
        return ThinkingConfig::Disabled;
    }
    if !model_supports_thinking(model, provider) {
        return ThinkingConfig::Disabled;
    }

    if !env_flag_truthy("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING")
        && model_supports_adaptive_thinking(model, provider)
    {
        return ThinkingConfig::Adaptive;
    }

    // Budget-based thinking for older models
    let budget = env::var("MAX_THINKING_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_THINKING_TOKENS);
    let clamped = budget.min(max_output_tokens.saturating_sub(1));
    ThinkingConfig::Enabled {
        budget_tokens: clamped,
    }
}

/// Resolve the model to use for a given request.
///
/// If the user has explicitly set `--model`, that value is always used.
/// For OpenAI-family providers (when no explicit model is set), the model is
/// split based on whether thinking is enabled:
/// - thinking enabled → `REASONING_MODEL` (default `gpt-5.4`)
/// - thinking disabled → `COMPLETION_MODEL` (default `gpt-5.3-codex`)
///
/// For Claude providers, `REASONING_MODEL`/`COMPLETION_MODEL` are also respected
/// if set, but no split is applied by default (Claude uses a single model).
pub fn resolve_active_model(
    provider: ApiProvider,
    model: &str,
    user_specified_model: bool,
    thinking_enabled: bool,
) -> String {
    if user_specified_model {
        return model.to_owned();
    }

    match provider {
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
            // OpenAI providers split between reasoning and completion models
            if thinking_enabled {
                get_openai_reasoning_model()
            } else {
                get_openai_completion_model()
            }
        }
        _ => {
            // Claude providers: honour REASONING_MODEL/COMPLETION_MODEL if
            // explicitly set, otherwise use the catalog default.
            if thinking_enabled {
                if let Ok(v) = env::var("REASONING_MODEL") {
                    if !v.trim().is_empty() {
                        return v;
                    }
                }
            } else {
                if let Ok(v) = env::var("COMPLETION_MODEL") {
                    if !v.trim().is_empty() {
                        return v;
                    }
                }
            }
            model.to_owned()
        }
    }
}

/// Returns the OpenAI reasoning effort level for a resolved model.
pub fn resolve_reasoning_effort(model: &str) -> String {
    if model == get_openai_completion_model() {
        get_openai_completion_think_level()
    } else {
        get_openai_reasoning_think_level()
    }
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
        events_from_openai_response, events_from_openai_sse_body, get_anthropic_auth_material,
        get_openai_auth_status, get_openai_credential_hint, get_token_freshness,
        is_openai_provider, provider_base_url, provider_descriptor, refresh_codex_access_token,
        resolve_api_provider, resolve_provider_model, sign_bedrock_request, ApiProvider,
        AuthMaterial, AuthRequest, AuthResolver, EchoProvider, EnvironmentAuthResolver,
        HttpProvider, ModelCatalog, OpenAIAuthSource, OpenAITokenFreshness, ProviderRequest,
        ProviderToolDefinition, DEFAULT_OPENAI_COMPLETION_MODEL, DEFAULT_OPENAI_REASONING_MODEL,
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
    fn treats_undecodable_tokens_as_fresh_for_openai_compatibility() {
        assert_eq!(
            get_token_freshness(Some("not-a-jwt")),
            OpenAITokenFreshness::Fresh
        );
    }

    #[tokio::test]
    async fn refreshes_codex_auth_atomically() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "codex-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let auth_path = root.join("auth.json");
        fs::write(
            &auth_path,
            r#"{
  "auth_mode":"chatgpt",
  "tokens":{
    "access_token":"header.eyJleHAiOjEsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "refresh_token":"old-refresh-token",
    "id_token":"header.eyJhdWQiOlsiY2xpZW50MTIzIl0sImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "account_id":"acct_123"
  }
}"#,
        )
        .unwrap();

        let (base_url, server) = spawn_json_server(json!({
            "access_token": "header.eyJleHAiOjQ3MDAwMDAwMDAsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
            "refresh_token": "new-refresh-token",
            "id_token": "header.eyJhdWQiOlsiY2xpZW50MTIzIl0sImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature"
        }))
        .await;

        let previous_auth_url = env::var("OPENAI_AUTH_URL").ok();
        env::set_var("OPENAI_AUTH_URL", &base_url);
        let refreshed_result = refresh_codex_access_token(Some(auth_path.as_path())).await;
        match previous_auth_url {
            Some(value) => env::set_var("OPENAI_AUTH_URL", value),
            None => env::remove_var("OPENAI_AUTH_URL"),
        }
        let refreshed = refreshed_result.unwrap();

        let persisted = fs::read_to_string(&auth_path).unwrap();
        let persisted_json: serde_json::Value = serde_json::from_str(&persisted).unwrap();
        let captured = server.join().unwrap();

        assert_eq!(
            refreshed.as_deref(),
            Some(
                "header.eyJleHAiOjQ3MDAwMDAwMDAsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature"
            )
        );
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/");
        assert!(captured.body.contains("grant_type=refresh_token"));
        assert!(captured.body.contains("refresh_token=old-refresh-token"));
        assert!(captured.body.contains("client_id=client123"));
        assert_eq!(
            persisted_json["tokens"]["refresh_token"].as_str(),
            Some("new-refresh-token")
        );
        assert_eq!(
            persisted_json["tokens"]["access_token"].as_str(),
            refreshed.as_deref()
        );
        assert_eq!(persisted_json["auth_mode"].as_str(), Some("chatgpt"));
        assert!(persisted.ends_with('\n'));
        assert!(!super::codex_auth_lock_path(&auth_path).exists());
    }

    #[test]
    fn reports_config_migration_inputs() {
        with_env_lock(|| {
            with_env_var("CLAUDE_CODE_API_PROVIDER", Some("openai"), || {
                with_env_var("OPENAI_API_KEY", Some("test-key"), || {
                    let report = super::config_migration_report(ApiProvider::OpenAI);
                    assert_eq!(report.provider, ApiProvider::OpenAI);
                    assert_eq!(
                        report.env.get("CLAUDE_CODE_API_PROVIDER"),
                        Some(&"openai".to_owned())
                    );
                    assert_eq!(
                        report.env.get("OPENAI_API_KEY"),
                        Some(&"test-key".to_owned())
                    );
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
        assert!(catalog.get_model(DEFAULT_OPENAI_REASONING_MODEL).is_some());
        assert!(catalog.get_model(DEFAULT_OPENAI_COMPLETION_MODEL).is_some());
    }

    #[test]
    fn openai_compatibility_catalog_honours_env_models() {
        with_env_lock(|| {
            with_env_var("REASONING_MODEL", Some("gemini-3.1-pro-preview"), || {
                with_env_var("COMPLETION_MODEL", Some("gemini-3.1-flash"), || {
                    let catalog = compatibility_model_catalog(ApiProvider::OpenAICompatible);
                    let listed = catalog
                        .list_models()
                        .into_iter()
                        .map(|model| model.id)
                        .collect::<Vec<_>>();
                    assert_eq!(
                        listed.first().map(String::as_str),
                        Some("gemini-3.1-pro-preview")
                    );
                    assert!(listed.iter().any(|model| model == "gemini-3.1-flash"));
                });
            });
        });
    }

    #[test]
    fn detects_gemini_openai_compatible_chat_completions_mode() {
        assert!(super::openai_compatible_uses_chat_completions(
            "https://generativelanguage.googleapis.com/v1beta/openai"
        ));
        assert!(!super::openai_compatible_uses_chat_completions(
            "https://compat.example/v1"
        ));
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
            model: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
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
            model: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
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
    fn parses_openai_responses_tool_call_response() {
        let events = events_from_openai_response(&json!({
            "id": "resp_123",
            "output": [
                {
                    "type": "message",
                    "content": [
                        { "type": "output_text", "text": "Need to search." }
                    ]
                },
                {
                    "type": "function_call",
                    "id": "fc_123",
                    "call_id": "call_123",
                    "name": "file_read",
                    "arguments": "{\"path\":\"src/main.rs\"}"
                }
            ],
            "usage": {
                "input_tokens": 19,
                "output_tokens": 4
            }
        }))
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
    fn parses_openai_responses_sse_stream() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Need to \"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"search.\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Need to search.\"}]},{\"type\":\"function_call\",\"id\":\"fc_123\",\"call_id\":\"call_123\",\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}],\"usage\":{\"input_tokens\":19,\"output_tokens\":4}}}\n\n"
        );
        let events = events_from_openai_sse_body(body).unwrap();

        assert!(matches!(
            &events[0],
            super::ProviderEvent::MessageDelta { text } if text == "Need to "
        ));
        assert!(matches!(
            &events[1],
            super::ProviderEvent::MessageDelta { text } if text == "search."
        ));
        assert!(
            matches!(&events[2], super::ProviderEvent::ToolCall { call } if call.id == "call_123")
        );
        assert!(
            matches!(&events[4], super::ProviderEvent::Usage { usage } if usage.input_tokens == 19)
        );
        assert!(
            matches!(&events[5], super::ProviderEvent::Stop { reason } if reason == "tool_use")
        );
    }

    #[test]
    fn extracts_error_message_from_detail_array() {
        let body = r#"{
            "detail": [
                {
                    "loc": ["body", "input", 0, "call_id"],
                    "msg": "Field required"
                }
            ]
        }"#;

        assert_eq!(
            super::compact_error_body(body),
            "body.input.0.call_id: Field required"
        );
    }

    #[test]
    fn uses_provider_specific_openai_base_urls() {
        with_env_lock(|| {
            with_env_var(
                "OPENAI_BASE_URL",
                Some("https://compat.example/v1/"),
                || {
                    assert_eq!(
                        provider_base_url(ApiProvider::OpenAI),
                        "https://api.openai.com/v1"
                    );
                    assert_eq!(
                        provider_base_url(ApiProvider::ChatGPTCodex),
                        "https://chatgpt.com/backend-api"
                    );
                    assert_eq!(
                        provider_base_url(ApiProvider::OpenAICompatible),
                        "https://compat.example/v1"
                    );
                },
            );
        });
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
    async fn sends_openai_responses_requests() {
        let (base_url, server) = spawn_json_server(json!({
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "openai ok" }]
                }
            ],
            "usage": { "input_tokens": 8, "output_tokens": 2 }
        }))
        .await;
        let provider = HttpProvider::with_base_url(
            ApiProvider::OpenAI,
            AuthMaterial {
                api_key: Some("openai-key".to_owned()),
                bearer_token: Some("openai-key".to_owned()),
                ..AuthMaterial::default()
            },
            format!("{base_url}/v1"),
        );
        let request = ProviderRequest {
            model: "gpt-5.4".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello openai".to_owned(),
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
        let captured = server.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

        assert_eq!(collected.text, "openai ok");
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/v1/responses");
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer openai-key")
        );
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sends_chatgpt_codex_responses_requests() {
        let (base_url, server) = spawn_json_server(json!({
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "codex ok" }]
                }
            ],
            "usage": { "input_tokens": 7, "output_tokens": 2 }
        }))
        .await;
        let provider = HttpProvider::with_base_url(
            ApiProvider::ChatGPTCodex,
            AuthMaterial {
                bearer_token: Some("codex-token".to_owned()),
                ..AuthMaterial::default()
            },
            base_url,
        );
        let request = ProviderRequest {
            model: "gpt-5.3-codex".to_owned(),
            messages: vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello codex".to_owned(),
                }],
            )],
            ..ProviderRequest::default()
        };

        let collected = collect_provider_response(&provider, request).await.unwrap();
        let captured = server.join().unwrap();
        let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

        assert_eq!(collected.text, "codex ok");
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/codex/responses");
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer codex-token")
        );
        assert_eq!(body["model"], "gpt-5.3-codex");
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"][0]["role"], "user");
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
