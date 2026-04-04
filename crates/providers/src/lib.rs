pub mod auth;
pub use auth::*;
pub mod http;
pub use http::*;
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
                            thought_signature: None,
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








pub fn build_provider(provider: ApiProvider, auth: AuthMaterial) -> Box<dyn Provider> {
    Box::new(HttpProvider::new(provider, auth))
}




fn env_value<const N: usize>(names: [&str; N]) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}




fn read_chatgpt_codex_models_cache() -> Option<ChatGPTCodexModelsCache> {
    let path = codex_home_dir().join("models_cache.json");
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}




fn openai_chat_tool_call(call: &ToolCall) -> Value {
    let mut tool_call = json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.input_json,
        }
    });

    if let Some(thought_signature) = call
        .thought_signature
        .as_deref()
        .filter(|thought_signature| !thought_signature.trim().is_empty())
    {
        tool_call["extra_content"] = json!({
            "google": {
                "thought_signature": thought_signature,
            }
        });
    }

    tool_call
}

fn openai_chat_thought_signature(tool_call: &Value) -> Option<String> {
    tool_call
        .get("extra_content")
        .and_then(|extra_content| extra_content.get("google"))
        .and_then(|google| google.get("thought_signature"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|thought_signature| !thought_signature.trim().is_empty())
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
                        thought_signature: None,
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
                thought_signature: openai_chat_thought_signature(tool_call),
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
                        thought_signature: None,
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




fn codex_auth_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
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
mod tests;
