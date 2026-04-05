#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[derive(Default)]
pub enum ApiProvider {
    #[serde(rename = "firstParty")]
    #[default]
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
#[derive(Default)]
pub enum ThinkingConfig {
    /// Model decides how much thinking to use (Claude 4.6+ models).
    Adaptive,
    /// Explicit thinking token budget (older Claude models).
    Enabled { budget_tokens: u64 },
    /// Thinking is disabled.
    #[default]
    Disabled,
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
