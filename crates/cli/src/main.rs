use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use code_agent_bridge::{
    base64_decode, base64_encode, connect_and_exchange, serve_bridge_session, serve_direct_session,
    AssistantDirective, BridgeServerConfig, BridgeSessionHandler, RemoteEndpoint, RemoteEnvelope,
    RemoteMode, RemotePermissionRequest, RemoteSessionState, ResumeSessionRequest, VoiceFrame,
};
use code_agent_core::{
    compatibility_command_registry, coordinator_tasks, create_coordinator_synthesis_task,
    create_coordinator_task, create_coordinator_worker_task, resume_tasks_for_question,
    update_task_record, AppEvent, BoundaryKind, CommandInvocation, CommandRegistry,
    CommandSource, CommandSpec, ContentBlock, LocalTaskStore as CoreLocalTaskStore, Message,
    MessageRole, QuestionRequest, QuestionResponse, SessionId, TaskRecord, TaskStatus,
    TaskStore,
};
use code_agent_mcp::parse_mcp_server_configs;
use code_agent_plugins::{
    BridgeLaunchRequest, CommandDefinitions, OutOfProcessPluginRuntime, PluginRuntime,
};
use code_agent_providers::{
    build_provider, clear_auth_snapshot, code_agent_auth_snapshot_path, collect_provider_response,
    compatibility_model_catalog, config_migration_report, get_anthropic_credential_hint,
    get_openai_credential_hint, resolve_api_provider, write_auth_snapshot, ApiProvider,
    AuthRequest, AuthResolver, EnvironmentAuthResolver, ModelCatalog, ProviderRequest,
    ProviderToolDefinition,
};
use code_agent_session::{
    agent_transcript_path_for, compact_messages, estimate_message_tokens,
    extract_last_json_string_field, get_project_dir, import_transcript_to_session_root,
    materialize_runtime_messages, CompactionConfig, CompactionOutcome, JsonlTranscriptCodec,
    LocalSessionStore, ProjectSessionStore, SessionStore, SessionSummary, TranscriptCodec,
};
use code_agent_tools::{compatibility_tool_registry, ToolCallRequest, ToolContext, ToolRegistry};
use code_agent_ui::{
    render_to_string as render_tui_to_string, Notification, PaneKind, PanePreview,
    PermissionPromptState, RatatuiApp, StatusLevel,
};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size as terminal_size, Clear, ClearType,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{stdout, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

#[cfg(test)]
use code_agent_providers::EchoProvider;

#[derive(Debug, Default)]
struct Cli {
    provider: Option<String>,
    model: Option<String>,
    session_root: Option<PathBuf>,
    print_workspace: bool,
    list_commands: bool,
    list_sessions: bool,
    tui: bool,
    repl: bool,
    plugin_root: Option<PathBuf>,
    show_plugin: bool,
    list_skills: bool,
    list_mcp: bool,
    bridge_server: Option<String>,
    bridge_connect: Option<String>,
    bridge_receive_count: Option<usize>,
    assistant_directive: Option<String>,
    assistant_agent: Option<String>,
    voice_text: Option<String>,
    voice_file: Option<PathBuf>,
    voice_format: Option<String>,
    resume: Option<String>,
    clear_session: Option<String>,
    tool: Option<String>,
    input: Option<String>,
    prompt: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StartupReport {
    provider: String,
    model: Option<String>,
    cwd: PathBuf,
    project_dir: PathBuf,
    session_root: PathBuf,
    command_count: usize,
    prompt: Option<String>,
    parsed_command: Option<String>,
    active_session_id: Option<SessionId>,
    transcript_path: Option<PathBuf>,
    auth_source: Option<String>,
    turn_count: usize,
    stop_reason: Option<String>,
    applied_compaction: Option<String>,
    estimated_tokens_before: Option<u64>,
    estimated_tokens_after: Option<u64>,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct ResumeReport {
    session_id: SessionId,
    transcript_path: PathBuf,
    message_count: usize,
    preview: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ToolRunReport {
    tool: String,
    ok: bool,
    output: String,
    metadata: Value,
}

#[derive(Debug, Serialize)]
struct PluginReport {
    root: PathBuf,
    name: String,
    version: Option<String>,
    description: Option<String>,
    skill_names: Vec<String>,
    command_names: Vec<String>,
    mcp_server_names: Vec<String>,
    lsp_server_names: Vec<String>,
    command_count: usize,
    has_agents: bool,
    has_output_styles: bool,
}

#[derive(Debug, Serialize)]
struct CommandReport {
    name: String,
    description: String,
    source: String,
    category: String,
    kind: String,
    aliases: Vec<String>,
    remote_safe: bool,
    bridge_safe: bool,
    requires_provider: bool,
    origin: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionCommandReport {
    session_id: SessionId,
    session_root: PathBuf,
    transcript_path: PathBuf,
    message_count: usize,
    runtime_message_count: usize,
    first_prompt: Option<String>,
    last_message_preview: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuthCommandReport {
    provider: String,
    status: String,
    auth_source: Option<String>,
    hint: Option<String>,
    snapshot_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct TaskCommandReport {
    count: usize,
    tasks: Vec<TaskRecord>,
}

#[derive(Debug, Serialize)]
struct QuestionCommandReport {
    count: usize,
    questions: Vec<QuestionRequest>,
}

#[derive(Debug, Serialize)]
struct ResponseCommandReport {
    count: usize,
    responses: Vec<QuestionResponse>,
}

enum ActiveSessionStore {
    Local(LocalSessionStore),
    Project(ProjectSessionStore),
}

impl ActiveSessionStore {
    fn new(cwd: PathBuf, session_root: Option<PathBuf>) -> Self {
        match session_root {
            Some(root) => Self::Local(LocalSessionStore::new(root)),
            None => Self::Project(ProjectSessionStore::new(cwd)),
        }
    }

    fn root_dir(&self) -> &Path {
        match self {
            Self::Local(store) => store.root_dir(),
            Self::Project(store) => store.storage_dir(),
        }
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        match self {
            Self::Local(store) => store.list_sessions().await,
            Self::Project(store) => store.list_sessions().await,
        }
    }

    async fn transcript_path(&self, session_id: SessionId) -> Result<PathBuf> {
        Ok(match self {
            Self::Local(store) => store.transcript_path_for_session(session_id),
            Self::Project(store) => store.transcript_path_for_session(session_id),
        })
    }

    async fn load_resume_target(&self, value: &str) -> Result<(SessionId, PathBuf, Vec<Message>)> {
        match self {
            Self::Local(store) => store.load_resume_target(value).await,
            Self::Project(store) => store.load_resume_target(value).await,
        }
    }

    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()> {
        match self {
            Self::Local(store) => store.append_message(session_id, message).await,
            Self::Project(store) => store.append_message(session_id, message).await,
        }
    }

    async fn load_session(&self, session_id: SessionId) -> Result<Vec<Message>> {
        match self {
            Self::Local(store) => store.load_session(session_id).await,
            Self::Project(store) => store.load_session(session_id).await,
        }
    }
}

fn task_store_for(cwd: &Path) -> CoreLocalTaskStore {
    CoreLocalTaskStore::new(cwd.join(".code-agent"))
}

fn command_report(spec: &CommandSpec) -> CommandReport {
    CommandReport {
        name: spec.name.clone(),
        description: spec.description.clone(),
        source: match spec.source {
            CommandSource::BuiltIn => "builtin",
            CommandSource::Plugin => "plugin",
            CommandSource::Skill => "skill",
            CommandSource::Workflow => "workflow",
        }
        .to_owned(),
        category: format!("{:?}", spec.category),
        kind: format!("{:?}", spec.kind),
        aliases: spec.aliases.clone(),
        remote_safe: spec.remote_safe,
        bridge_safe: spec.bridge_safe,
        requires_provider: spec.requires_provider,
        origin: spec.origin.clone(),
    }
}

async fn resolved_command_registry(
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
) -> CommandRegistry {
    let mut registry = compatibility_command_registry();
    let runtime = OutOfProcessPluginRuntime;
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    if let Ok(dynamic_commands) = runtime.discover_commands(&root).await {
        registry.extend(dynamic_commands);
    }
    registry
}

fn session_preview(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find_map(|message| {
            let text = message_text(message);
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| preview_lines_from_text(trimmed.to_owned(), 1, 72).join(" "))
        })
}

fn auth_hint_for_provider(provider: ApiProvider) -> String {
    if matches!(
        provider,
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
    ) {
        get_openai_credential_hint(provider)
    } else {
        get_anthropic_credential_hint(provider)
    }
}

fn parse_task_id(value: &str) -> Result<uuid::Uuid> {
    Ok(uuid::Uuid::parse_str(value)?)
}

fn parse_task_status(value: &str) -> Result<TaskStatus> {
    match value.trim() {
        "pending" => Ok(TaskStatus::Pending),
        "running" => Ok(TaskStatus::Running),
        "waiting_for_input" => Ok(TaskStatus::WaitingForInput),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status: {other}"),
    }
}

fn parse_assignment_args(args: &[String]) -> BTreeMap<String, String> {
    args.iter()
        .filter_map(|arg| arg.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn parse_cli() -> Cli {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => cli.provider = args.next(),
            "--model" => cli.model = args.next(),
            "--session-root" => cli.session_root = args.next().map(PathBuf::from),
            "--print-workspace" => cli.print_workspace = true,
            "--list-commands" => cli.list_commands = true,
            "--list-sessions" => cli.list_sessions = true,
            "--tui" => cli.tui = true,
            "--repl" => cli.repl = true,
            "--plugin-root" => cli.plugin_root = args.next().map(PathBuf::from),
            "--show-plugin" => cli.show_plugin = true,
            "--list-skills" => cli.list_skills = true,
            "--list-mcp" => cli.list_mcp = true,
            "--bridge-server" => cli.bridge_server = args.next(),
            "--bridge-connect" => cli.bridge_connect = args.next(),
            "--bridge-receive-count" => {
                cli.bridge_receive_count = args.next().and_then(|value| value.parse().ok())
            }
            "--assistant-directive" => cli.assistant_directive = args.next(),
            "--assistant-agent" => cli.assistant_agent = args.next(),
            "--voice-text" => cli.voice_text = args.next(),
            "--voice-file" => cli.voice_file = args.next().map(PathBuf::from),
            "--voice-format" => cli.voice_format = args.next(),
            "--resume" => cli.resume = args.next(),
            "--clear-session" => cli.clear_session = args.next(),
            "--tool" => cli.tool = args.next(),
            "--input" => cli.input = args.next(),
            "--help" | "-h" => {
                println!(
                    "Usage: code-agent-rust [--provider NAME] [--model NAME] [--resume TARGET] [--list-sessions] [--tool NAME --input JSON] [--tui|--repl] [--voice-text TEXT|--voice-file PATH] [--bridge-server ADDR|tcp://ADDR --bridge-connect URL|tcp://ADDR] [prompt]"
                );
                println!("Slash commands such as '/help', '/resume <session>', '/clear', '/compact', '/model', and '/config' are supported.");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            value => cli.prompt.push(value.to_owned()),
        }
    }

    cli
}

fn prompt_preview(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .take(12)
        .map(|message| {
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    ContentBlock::Boundary { boundary } => Some(match boundary.kind {
                        BoundaryKind::Compact => "[compact boundary]",
                        BoundaryKind::MicroCompact => "[micro-compact boundary]",
                        BoundaryKind::SessionMemory => "[session-memory boundary]",
                        BoundaryKind::Resume => "[resume boundary]",
                    }),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{:?}: {}", message.role, text)
        })
        .collect()
}

fn choose_active_session(
    cli: &Cli,
    listed_sessions: &[SessionSummary],
    explicit_resume: Option<(SessionId, PathBuf, Vec<Message>)>,
) -> Result<(SessionId, Option<PathBuf>, Vec<Message>)> {
    if let Some((session_id, path, messages)) = explicit_resume {
        return Ok((session_id, Some(path), messages));
    }

    if cli.prompt.is_empty() {
        if let Some(summary) = listed_sessions.first() {
            return Ok((
                summary.session_id,
                Some(summary.transcript_path.clone()),
                Vec::new(),
            ));
        }
    }

    Ok((Uuid::new_v4(), None, Vec::new()))
}

fn build_text_message(
    session_id: SessionId,
    role: MessageRole,
    text: String,
    parent_id: Option<Uuid>,
) -> Message {
    let mut message = Message::new(role, vec![ContentBlock::Text { text }]);
    message.session_id = Some(session_id);
    message.parent_id = parent_id;
    message
}

async fn run_tool(
    tool_name: &str,
    input: Value,
    cwd: PathBuf,
    provider: ApiProvider,
    model: Option<String>,
) -> Result<ToolRunReport> {
    let registry = compatibility_tool_registry();
    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: tool_name.to_owned(),
                input,
            },
            &ToolContext {
                cwd,
                provider: Some(provider.to_string()),
                model,
                ..ToolContext::default()
            },
        )
        .await?;

    Ok(ToolRunReport {
        tool: tool_name.to_owned(),
        ok: !output.is_error,
        output: output.content,
        metadata: output.metadata,
    })
}

fn tool_definitions(registry: &ToolRegistry) -> Vec<ProviderToolDefinition> {
    registry
        .specs()
        .into_iter()
        .map(|spec| ProviderToolDefinition {
            name: spec.name,
            description: spec.description,
            input_schema: serde_json::to_value(spec.input_schema).unwrap_or_else(|_| json!({})),
        })
        .collect()
}

fn build_tool_result_message(
    session_id: SessionId,
    tool_call_id: String,
    output_text: String,
    is_error: bool,
    parent_id: Option<uuid::Uuid>,
) -> Message {
    let mut message = Message::new(
        MessageRole::Tool,
        vec![ContentBlock::ToolResult {
            result: code_agent_core::ToolResult {
                tool_call_id,
                output_text,
                is_error,
            },
        }],
    );
    message.session_id = Some(session_id);
    message.parent_id = parent_id;
    message
}

fn build_assistant_message(
    session_id: SessionId,
    parent_id: Option<uuid::Uuid>,
    text: String,
    tool_calls: Vec<code_agent_core::ToolCall>,
) -> Message {
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(ContentBlock::Text { text });
    }
    for call in tool_calls {
        blocks.push(ContentBlock::ToolCall { call });
    }
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: String::new(),
        });
    }

    let mut message = Message::new(MessageRole::Assistant, blocks);
    message.session_id = Some(session_id);
    message.parent_id = parent_id;
    message
}

fn provider_supports_live_runtime(provider: ApiProvider) -> bool {
    matches!(
        provider,
        ApiProvider::FirstParty
            | ApiProvider::Bedrock
            | ApiProvider::Vertex
            | ApiProvider::Foundry
            | ApiProvider::OpenAI
            | ApiProvider::ChatGPTCodex
            | ApiProvider::OpenAICompatible
    )
}

async fn resolve_provider_client(
    provider: ApiProvider,
    auth_configured: bool,
) -> Result<Box<dyn code_agent_providers::Provider>> {
    if !auth_configured || !provider_supports_live_runtime(provider) {
        #[cfg(test)]
        {
            return Ok(Box::new(EchoProvider::new(provider)));
        }
        #[cfg(not(test))]
        {
            return Err(anyhow!(auth_hint_for_provider(provider)));
        }
    }

    let auth = EnvironmentAuthResolver
        .resolve_auth(AuthRequest {
            provider,
            profile: None,
        })
        .await?;
    Ok(build_provider(provider, auth))
}

fn compaction_kind_name(outcome: &CompactionOutcome) -> Option<String> {
    outcome
        .summary_message
        .metadata
        .attributes
        .get("compaction_kind")
        .cloned()
}

fn guess_voice_format(path: &Path) -> String {
    match path.extension().and_then(|value| value.to_str()) {
        Some("wav") => "audio/wav".to_owned(),
        Some("pcm") => "audio/pcm".to_owned(),
        Some("mp3") => "audio/mpeg".to_owned(),
        Some("flac") => "audio/flac".to_owned(),
        _ => "application/octet-stream".to_owned(),
    }
}

fn voice_extension(format: &str) -> &'static str {
    let normalized = format.trim().to_ascii_lowercase();
    if normalized.contains("wav") {
        "wav"
    } else if normalized.contains("pcm") {
        "pcm"
    } else if normalized.contains("mpeg") || normalized.contains("mp3") {
        "mp3"
    } else if normalized.contains("flac") {
        "flac"
    } else if normalized.starts_with("text/") {
        "txt"
    } else {
        "bin"
    }
}

fn streamed_voice_frames(
    format: String,
    stream_id: String,
    payload: &[u8],
    chunk_size: usize,
) -> Vec<RemoteEnvelope> {
    let chunk_size = chunk_size.max(1);
    if payload.is_empty() {
        return vec![RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format,
                payload_base64: String::new(),
                sequence: 1,
                stream_id: Some(stream_id),
                is_final: true,
            },
        }];
    }

    let total_chunks = payload.chunks(chunk_size).len();
    payload
        .chunks(chunk_size)
        .enumerate()
        .map(|(index, chunk)| RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: format.clone(),
                payload_base64: base64_encode(chunk),
                sequence: index as u64 + 1,
                stream_id: Some(stream_id.clone()),
                is_final: index + 1 == total_chunks,
            },
        })
        .collect()
}

fn build_remote_outbound(
    cli: &Cli,
    session_id: SessionId,
    prompt: Option<String>,
    resume_target: Option<&str>,
) -> Result<Vec<RemoteEnvelope>> {
    let mut outbound = Vec::new();
    if let Some(target) = resume_target.filter(|target| !target.trim().is_empty()) {
        outbound.push(RemoteEnvelope::ResumeSession {
            request: ResumeSessionRequest {
                target: target.to_owned(),
            },
        });
    }
    if let Some(instruction) = cli.assistant_directive.clone() {
        outbound.push(RemoteEnvelope::AssistantDirective {
            directive: AssistantDirective {
                agent_id: cli.assistant_agent.clone(),
                instruction,
                ..AssistantDirective::default()
            },
        });
    }
    if let Some(voice_text) = cli.voice_text.clone() {
        outbound.extend(streamed_voice_frames(
            cli.voice_format
                .clone()
                .unwrap_or_else(|| "text/plain".to_owned()),
            "cli-text".to_owned(),
            voice_text.as_bytes(),
            24_576,
        ));
    }
    if let Some(path) = cli.voice_file.as_ref() {
        let bytes = fs::read(path)
            .map_err(|error| anyhow!("failed to read voice file {}: {error}", path.display()))?;
        outbound.extend(streamed_voice_frames(
            cli.voice_format
                .clone()
                .unwrap_or_else(|| guess_voice_format(path)),
            format!("file-{}", Uuid::new_v4()),
            &bytes,
            24_576,
        ));
    }
    if let Some(prompt_text) = prompt {
        outbound.push(RemoteEnvelope::Message {
            message: build_text_message(session_id, MessageRole::User, prompt_text, None),
        });
    }
    Ok(outbound)
}

fn remote_mode_for_address(address: &str) -> RemoteMode {
    if address.starts_with("tcp://") || address.starts_with("direct://") {
        RemoteMode::DirectConnect
    } else if address.starts_with("ide://") {
        RemoteMode::IdeBridge
    } else {
        RemoteMode::WebSocket
    }
}

fn remote_mode_enabled(cli: &Cli) -> bool {
    cli.bridge_connect.is_some() || cli.bridge_server.is_some()
}

fn command_allowed_in_repl(registry: &CommandRegistry, remote_mode: bool, name: &str) -> bool {
    if !remote_mode {
        return registry.resolve(name).is_some();
    }
    registry.is_remote_safe(name)
}

fn command_allowed_for_bridge(registry: &CommandRegistry, name: &str) -> bool {
    registry.is_bridge_safe(name)
}

fn remote_endpoint(address: &str, session_id: SessionId) -> RemoteEndpoint {
    let mode = remote_mode_for_address(address);
    RemoteEndpoint {
        mode: Some(mode.clone()),
        scheme: match mode {
            RemoteMode::DirectConnect => "tcp".to_owned(),
            RemoteMode::IdeBridge => "ide".to_owned(),
            _ => "ws".to_owned(),
        },
        address: address.to_owned(),
        session_id: Some(session_id),
        ..RemoteEndpoint::default()
    }
}

async fn exchange_remote_envelopes(
    address: &str,
    session_id: SessionId,
    mut outbound: Vec<RemoteEnvelope>,
    receive_count: usize,
) -> Result<Vec<RemoteEnvelope>> {
    outbound.push(RemoteEnvelope::Interrupt);
    let mut inbound = connect_and_exchange(
        remote_endpoint(address, session_id),
        outbound,
        receive_count.max(16),
    )
    .await?;
    if matches!(
        inbound.last(),
        Some(RemoteEnvelope::Ack { note }) if note == "interrupt"
    ) {
        inbound.pop();
    }
    Ok(inbound)
}

fn message_text_blocks(message: &Message) -> Vec<String> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn message_text(message: &Message) -> String {
    message_text_blocks(message).join("\n")
}

fn preview_lines_from_text(text: impl Into<String>, max_lines: usize, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.into().lines() {
        let trimmed = line.trim_end();
        if trimmed.chars().count() <= max_width {
            lines.push(trimmed.to_owned());
        } else {
            let mut clipped = trimmed
                .chars()
                .take(max_width.saturating_sub(3))
                .collect::<String>();
            clipped.push_str("...");
            lines.push(clipped);
        }
        if lines.len() == max_lines {
            break;
        }
    }
    if lines.is_empty() {
        lines.push("No details available.".to_owned());
    }
    lines
}

fn rotate_pane(current: PaneKind, forward: bool) -> PaneKind {
    let panes = PaneKind::ALL;
    let index = panes.iter().position(|pane| *pane == current).unwrap_or(0);
    let next = if forward {
        (index + 1) % panes.len()
    } else if index == 0 {
        panes.len() - 1
    } else {
        index - 1
    };
    panes[next]
}

fn pane_from_digit(ch: char) -> Option<PaneKind> {
    let index = ch.to_digit(10)? as usize;
    if index == 0 || index > PaneKind::ALL.len() {
        return None;
    }
    Some(PaneKind::ALL[index - 1])
}

fn recent_task_preview(cwd: &Path) -> PanePreview {
    let store = task_store_for(cwd);
    match store.list_tasks() {
        Ok(tasks) if tasks.is_empty() => PanePreview {
            title: "Tasks".to_owned(),
            lines: vec!["No task activity yet.".to_owned()],
        },
        Ok(tasks) => {
            let lines = tasks
                .into_iter()
                .rev()
                .take(6)
                .map(|task| {
                    format!(
                        "{:?} {} [{}]",
                        task.status,
                        task.title,
                        task.kind
                    )
                })
                .collect::<Vec<_>>();
            PanePreview {
                title: "Tasks".to_owned(),
                lines,
            }
        }
        Err(error) => PanePreview {
            title: "Tasks".to_owned(),
            lines: vec![format!("task store error: {error}")],
        },
    }
}

fn recent_question_preview(cwd: &Path) -> Option<PanePreview> {
    let store = task_store_for(cwd);
    match store.list_questions() {
        Ok(questions) => questions.last().map(|question| PanePreview {
            title: "Pending question".to_owned(),
            lines: {
                let mut lines = vec![question.prompt.clone()];
                if !question.choices.is_empty() {
                    lines.push(format!("choices: {}", question.choices.join(", ")));
                }
                lines
            },
        }),
        Err(_) => None,
    }
}

fn preview_for_last_file_message(messages: &[Message], cwd: &Path) -> Option<PanePreview> {
    for message in messages.iter().rev() {
        let MessageRole::Tool = message.role else {
            continue;
        };
        let Some(result) = message.blocks.iter().find_map(|block| match block {
            ContentBlock::ToolResult { result } => Some(result),
            _ => None,
        }) else {
            continue;
        };
        let path = extract_last_json_string_field(&result.output_text, "path")
            .map(PathBuf::from)
            .or_else(|| {
                let output = result.output_text.trim();
                output
                    .strip_prefix("edited ")
                    .or_else(|| output.strip_prefix("wrote "))
                    .map(PathBuf::from)
            })?;
        let resolved = if path.is_absolute() { path } else { cwd.join(path) };
        let title = resolved
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "File preview".to_owned());
        return Some(PanePreview {
            title,
            lines: preview_lines_from_text(result.output_text.clone(), 10, 60),
        });
    }
    None
}

fn preview_for_last_diff_message(messages: &[Message]) -> Option<PanePreview> {
    for message in messages.iter().rev() {
        let MessageRole::Tool = message.role else {
            continue;
        };
        let Some(result) = message.blocks.iter().find_map(|block| match block {
            ContentBlock::ToolResult { result } => Some(result),
            _ => None,
        }) else {
            continue;
        };
        if result.output_text.starts_with("edited ") || result.output_text.starts_with("wrote ") {
            return Some(PanePreview {
                title: "Diff preview".to_owned(),
                lines: preview_lines_from_text(result.output_text.clone(), 10, 60),
            });
        }
    }
    None
}

fn recent_log_preview(messages: &[Message]) -> PanePreview {
    let mut lines = messages
        .iter()
        .rev()
        .take(8)
        .map(|message| format!("{:?}: {}", message.role, message_text(message)))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push("No runtime logs yet.".to_owned());
    }
    PanePreview {
        title: "Logs".to_owned(),
        lines,
    }
}

fn pending_permission_from_tasks(cwd: &Path) -> Option<PermissionPromptState> {
    let store = task_store_for(cwd);
    let task = store
        .list_tasks()
        .ok()?
        .into_iter()
        .rev()
        .find(|task| task.status == TaskStatus::WaitingForInput)?;
    Some(PermissionPromptState {
        tool_name: task.kind,
        summary: task
            .input
            .unwrap_or_else(|| "Additional approval or input is required.".to_owned()),
        allow_once_label: "Approve once".to_owned(),
        deny_label: "Deny".to_owned(),
    })
}

fn build_repl_ui_state(
    app: &RatatuiApp,
    registry: &code_agent_core::CommandRegistry,
    raw_messages: &[Message],
    cwd: &Path,
    input_buffer: &code_agent_ui::InputBuffer,
    status_line: &str,
    active_pane: PaneKind,
    compact_banner: Option<String>,
) -> code_agent_ui::UiState {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let mut state = app.state_from_messages(runtime_messages.clone(), &registry.all());
    state.input_buffer = input_buffer.clone();
    state.status_line = status_line.to_owned();
    state.active_pane = Some(active_pane);
    state.compact_banner = compact_banner;
    state.task_preview = recent_task_preview(cwd);
    state.file_preview = preview_for_last_file_message(&runtime_messages, cwd).unwrap_or(PanePreview {
        title: "File preview".to_owned(),
        lines: vec!["No file preview available yet.".to_owned()],
    });
    state.diff_preview = preview_for_last_diff_message(&runtime_messages).unwrap_or(PanePreview {
        title: "Diff preview".to_owned(),
        lines: vec!["No diff preview available yet.".to_owned()],
    });
    state.log_preview = recent_log_preview(&runtime_messages);
    state.permission_prompt = pending_permission_from_tasks(cwd);
    if let Some(question_preview) = recent_question_preview(cwd) {
        state.task_preview.lines.extend(question_preview.lines);
    }
    if let Some(notification) = state.permission_prompt.as_ref() {
        state.push_notification(Notification {
            title: "permission".to_owned(),
            body: notification.summary.clone(),
            level: Some(StatusLevel::Warning),
        });
    }
    state
}

fn persist_voice_capture(
    cwd: &Path,
    stream_id: &str,
    format: &str,
    payload: &[u8],
) -> Result<PathBuf> {
    let path = cwd
        .join(".code-agent")
        .join("voice")
        .join(format!("{stream_id}.{}", voice_extension(format)));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, payload)?;
    Ok(path)
}

fn compaction_event(outcome: &CompactionOutcome) -> Option<RemoteEnvelope> {
    outcome
        .boundary_message
        .blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Boundary { boundary } => Some(RemoteEnvelope::Event {
                event: AppEvent::CompactApplied {
                    kind: boundary.kind.clone(),
                },
            }),
            _ => None,
        })
}

fn remote_envelopes_from_new_messages(
    messages: &[Message],
    start_index: usize,
) -> Vec<RemoteEnvelope> {
    let mut envelopes = Vec::new();
    for message in messages.iter().skip(start_index) {
        match message.role {
            MessageRole::Assistant => {
                envelopes.push(RemoteEnvelope::Message {
                    message: message.clone(),
                });
                for block in &message.blocks {
                    if let ContentBlock::ToolCall { call } = block {
                        envelopes.push(RemoteEnvelope::ToolCall { call: call.clone() });
                    }
                }
            }
            MessageRole::Tool => {
                for block in &message.blocks {
                    if let ContentBlock::ToolResult { result } = block {
                        envelopes.push(RemoteEnvelope::ToolResult {
                            result: result.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    envelopes
}

struct LocalBridgeHandler<'a> {
    store: &'a ActiveSessionStore,
    tool_registry: &'a ToolRegistry,
    cwd: PathBuf,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: Vec<Message>,
    live_runtime: bool,
    allow_remote_tools: bool,
    pending_permission: Option<PendingRemoteTool>,
    voice_streams: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
struct PendingRemoteTool {
    request: RemotePermissionRequest,
    call: code_agent_core::ToolCall,
}

impl<'a> LocalBridgeHandler<'a> {
    fn task_store(&self) -> CoreLocalTaskStore {
        task_store_for(&self.cwd)
    }

    fn tool_runtime_envelopes(&self, tool_name: &str, metadata: &Value) -> Vec<RemoteEnvelope> {
        let mut envelopes = Vec::new();
        if let Ok(task) = serde_json::from_value::<TaskRecord>(metadata.clone()) {
            envelopes.push(RemoteEnvelope::TaskState { task });
        }
        if let Some(task) = metadata
            .get("workflow")
            .cloned()
            .and_then(|value| serde_json::from_value::<TaskRecord>(value).ok())
        {
            envelopes.push(RemoteEnvelope::TaskState { task });
        }
        if let Ok(question) = serde_json::from_value::<QuestionRequest>(metadata.clone()) {
            envelopes.push(RemoteEnvelope::Question { question });
        }
        if tool_name == "ask_user_question" {
            if let Some(question) = metadata
                .get("id")
                .and_then(|_| serde_json::from_value::<QuestionRequest>(metadata.clone()).ok())
            {
                envelopes.push(RemoteEnvelope::Question { question });
            }
        }
        envelopes
    }

    fn session_state(&self) -> RemoteSessionState {
        RemoteSessionState {
            endpoint: "local".to_owned(),
            connected: true,
            session_id: Some(self.session_id),
            provider: Some(self.provider.to_string()),
            model: Some(self.active_model.clone()),
            message_count: materialize_runtime_messages(&self.raw_messages).len(),
            pending_permission_id: self
                .pending_permission
                .as_ref()
                .map(|pending| pending.request.id.clone()),
            last_error: None,
        }
    }

    fn session_state_envelope(&self) -> RemoteEnvelope {
        RemoteEnvelope::SessionState {
            state: self.session_state(),
        }
    }

    fn with_session_state(&self, mut outbound: Vec<RemoteEnvelope>) -> Vec<RemoteEnvelope> {
        outbound.push(self.session_state_envelope());
        outbound
    }

    async fn resume_session(&mut self, target: &str) -> Result<Vec<RemoteEnvelope>> {
        let (session_id, _, messages) = self.store.load_resume_target(target).await?;
        self.session_id = session_id;
        self.raw_messages = messages;
        self.pending_permission = None;
        let runtime_messages = materialize_runtime_messages(&self.raw_messages);
        let start = runtime_messages.len().saturating_sub(8);
        let mut outbound = runtime_messages
            .into_iter()
            .skip(start)
            .map(|message| RemoteEnvelope::Message { message })
            .collect::<Vec<_>>();
        outbound.push(self.session_state_envelope());
        Ok(outbound)
    }

    async fn run_remote_tool_call(
        &mut self,
        call: code_agent_core::ToolCall,
    ) -> Result<Vec<RemoteEnvelope>> {
        let parent_id = self.raw_messages.last().map(|message| message.id);
        let tool_call_message = build_assistant_message(
            self.session_id,
            parent_id,
            String::new(),
            vec![call.clone()],
        );
        self.store
            .append_message(self.session_id, &tool_call_message)
            .await?;
        self.raw_messages.push(tool_call_message.clone());

        let (result, supplemental) = match serde_json::from_str::<Value>(&call.input_json) {
            Ok(input) => match self
                .tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: call.name.clone(),
                        input,
                    },
                    &ToolContext {
                        session_id: Some(self.session_id),
                        cwd: self.cwd.clone(),
                        provider: Some(self.provider.to_string()),
                        model: Some(self.active_model.clone()),
                        ..ToolContext::default()
                    },
                )
                .await
            {
                Ok(output) => (
                    code_agent_core::ToolResult {
                        tool_call_id: call.id.clone(),
                        output_text: output.content,
                        is_error: output.is_error,
                    },
                    self.tool_runtime_envelopes(&call.name, &output.metadata),
                ),
                Err(error) => (
                    code_agent_core::ToolResult {
                        tool_call_id: call.id.clone(),
                        output_text: error.to_string(),
                        is_error: true,
                    },
                    Vec::new(),
                ),
            },
            Err(error) => (
                code_agent_core::ToolResult {
                    tool_call_id: call.id.clone(),
                    output_text: format!("invalid tool input JSON: {error}"),
                    is_error: true,
                },
                Vec::new(),
            ),
        };

        let tool_message = build_tool_result_message(
            self.session_id,
            result.tool_call_id.clone(),
            result.output_text.clone(),
            result.is_error,
            Some(tool_call_message.id),
        );
        self.store
            .append_message(self.session_id, &tool_message)
            .await?;
        self.raw_messages.push(tool_message);

        let mut outbound = vec![
            RemoteEnvelope::ToolCall { call },
            RemoteEnvelope::ToolResult { result },
        ];
        outbound.extend(supplemental);
        Ok(self.with_session_state(outbound))
    }

    async fn execute_remote_tool_call(
        &mut self,
        call: code_agent_core::ToolCall,
    ) -> Result<Vec<RemoteEnvelope>> {
        let Some(spec) = self.tool_registry.get(&call.name).map(|tool| tool.spec()) else {
            return Ok(self.with_session_state(vec![RemoteEnvelope::Error {
                message: format!("unknown tool: {}", call.name),
            }]));
        };

        if spec.needs_permission && !self.allow_remote_tools {
            let request = RemotePermissionRequest {
                id: Uuid::new_v4().to_string(),
                tool_name: call.name.clone(),
                input_json: call.input_json.clone(),
                read_only: spec.read_only,
                reason: Some("remote tool execution requires approval".to_owned()),
            };
            self.pending_permission = Some(PendingRemoteTool {
                request: request.clone(),
                call,
            });
            return Ok(self.with_session_state(vec![RemoteEnvelope::PermissionRequest { request }]));
        }

        self.pending_permission = None;
        self.run_remote_tool_call(call).await
    }

    async fn execute_prompt(&mut self, prompt_text: String) -> Result<Vec<RemoteEnvelope>> {
        let start_index = self.raw_messages.len();
        let (applied_compaction, _, _, _, _) = execute_local_turn(
            self.store,
            self.tool_registry,
            self.cwd.clone(),
            self.provider,
            self.active_model.clone(),
            self.session_id,
            &mut self.raw_messages,
            prompt_text,
            self.live_runtime,
        )
        .await?;

        let mut outbound = Vec::new();
        if let Some(outcome) = applied_compaction.as_ref().and_then(compaction_event) {
            outbound.push(outcome);
        }
        outbound.extend(remote_envelopes_from_new_messages(
            &self.raw_messages,
            start_index + 1,
        ));
        if outbound.is_empty() {
            outbound.push(RemoteEnvelope::Ack {
                note: "no_output".to_owned(),
            });
        }
        Ok(self.with_session_state(outbound))
    }

    async fn execute_coordinator(
        &mut self,
        directive: &AssistantDirective,
    ) -> Result<Vec<RemoteEnvelope>> {
        let tasks = coordinator_tasks(&directive.instruction);
        if tasks.is_empty() {
            return Ok(vec![RemoteEnvelope::Ack {
                note: "empty_coordinator_directive".to_owned(),
            }]);
        }

        let start_index = self.raw_messages.len();
        let mut outbound = Vec::new();
        let mut worker_summaries = Vec::new();
        let task_store = self.task_store();
        let coordinator_task =
            create_coordinator_task(&task_store, self.session_id, directive.instruction.clone())?;
        outbound.push(RemoteEnvelope::TaskState {
            task: coordinator_task.clone(),
        });
        let codec = JsonlTranscriptCodec;

        for (index, task) in tasks.iter().enumerate() {
            let worker_start = self.raw_messages.len();
            let agent_id = uuid::Uuid::new_v4();
            let transcript_path = agent_transcript_path_for(
                &self.cwd,
                self.session_id,
                agent_id,
                Some("coordinator"),
            );
            let worker_task = create_coordinator_worker_task(
                &task_store,
                self.session_id,
                coordinator_task.id,
                agent_id,
                format!("worker {}", index + 1),
                task.clone(),
                Some(transcript_path.clone()),
            )?;
            outbound.push(RemoteEnvelope::TaskState {
                task: worker_task.clone(),
            });
            let worker_prompt = format!(
                "[worker {}/{}]\nTask: {}\nReturn concise findings only.",
                index + 1,
                tasks.len(),
                task
            );
            let (applied_compaction, _, _, _, _) = execute_local_turn(
                self.store,
                self.tool_registry,
                self.cwd.clone(),
                self.provider,
                self.active_model.clone(),
                self.session_id,
                &mut self.raw_messages,
                worker_prompt,
                self.live_runtime,
            )
            .await?;
            if let Some(event) = applied_compaction.as_ref().and_then(compaction_event) {
                outbound.push(event);
            }
            let worker_findings = self
                .raw_messages
                .iter()
                .skip(worker_start)
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
                .map(message_text)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| "no findings".to_owned());
            for message in self.raw_messages.iter().skip(worker_start) {
                codec.append_message(&transcript_path, message).await?;
            }
            let worker_task = update_task_record(
                &task_store,
                worker_task,
                TaskStatus::Completed,
                Some(worker_findings.clone()),
            )?;
            outbound.push(RemoteEnvelope::TaskState { task: worker_task });
            worker_summaries.push(format!("worker {}: {}", index + 1, worker_findings));
        }

        let synthesis_task = create_coordinator_synthesis_task(
            &task_store,
            self.session_id,
            coordinator_task.id,
            directive.instruction.clone(),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: synthesis_task.clone(),
        });
        let synthesis_prompt = format!(
            "[coordinator synthesis]\nOriginal directive: {}\n{}\nRespond with a combined answer.",
            directive.instruction.trim(),
            worker_summaries.join("\n")
        );
        let (applied_compaction, _, _, _, _) = execute_local_turn(
            self.store,
            self.tool_registry,
            self.cwd.clone(),
            self.provider,
            self.active_model.clone(),
            self.session_id,
            &mut self.raw_messages,
            synthesis_prompt,
            self.live_runtime,
        )
        .await?;
        if let Some(event) = applied_compaction.as_ref().and_then(compaction_event) {
            outbound.push(event);
        }
        let synthesis_output = self
            .raw_messages
            .iter()
            .skip(start_index)
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(message_text)
            .unwrap_or_default();
        let synthesis_task = update_task_record(
            &task_store,
            synthesis_task,
            TaskStatus::Completed,
            Some(synthesis_output.clone()),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: synthesis_task,
        });
        let coordinator_task = update_task_record(
            &task_store,
            coordinator_task,
            TaskStatus::Completed,
            Some(synthesis_output),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: coordinator_task,
        });
        outbound.extend(remote_envelopes_from_new_messages(
            &self.raw_messages,
            start_index,
        ));
        if outbound.is_empty() {
            outbound.push(RemoteEnvelope::Ack {
                note: "no_output".to_owned(),
            });
        }
        Ok(self.with_session_state(outbound))
    }
}

#[async_trait]
impl BridgeSessionHandler for LocalBridgeHandler<'_> {
    async fn on_connect(
        &mut self,
        _record: &code_agent_bridge::BridgeSessionRecord,
    ) -> Result<Vec<RemoteEnvelope>> {
        Ok(vec![
            RemoteEnvelope::Event {
                event: AppEvent::RemoteConnected,
            },
            self.session_state_envelope(),
        ])
    }

    async fn on_envelope(&mut self, envelope: &RemoteEnvelope) -> Result<Vec<RemoteEnvelope>> {
        match envelope {
            RemoteEnvelope::Message { message } => {
                let prompt_text = message_text(message);
                if prompt_text.trim().is_empty() {
                    return Ok(vec![RemoteEnvelope::Ack {
                        note: "empty_message".to_owned(),
                    }]);
                }
                self.execute_prompt(prompt_text).await
            }
            RemoteEnvelope::AssistantDirective { directive } => {
                let prompt = directive.instruction.trim();
                if prompt.is_empty() {
                    return Ok(vec![RemoteEnvelope::Ack {
                        note: "empty_assistant_directive".to_owned(),
                    }]);
                }
                if directive.agent_id.as_deref() == Some("coordinator") {
                    return self.execute_coordinator(directive).await;
                }
                let decorated = directive
                    .agent_id
                    .as_ref()
                    .map(|agent_id| format!("[assistant:{agent_id}] {prompt}"))
                    .unwrap_or_else(|| prompt.to_owned());
                self.execute_prompt(decorated).await
            }
            RemoteEnvelope::VoiceFrame { frame } => {
                let payload = base64_decode(&frame.payload_base64)?;
                let stream_id = frame
                    .stream_id
                    .clone()
                    .unwrap_or_else(|| "default".to_owned());
                let buffered = self.voice_streams.entry(stream_id.clone()).or_default();
                buffered.extend_from_slice(&payload);
                if !frame.is_final {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: format!("voice_frame_buffered:{stream_id}"),
                    }]));
                }

                let payload = self.voice_streams.remove(&stream_id).unwrap_or_default();
                let prompt = match String::from_utf8(payload.clone()) {
                    Ok(prompt) => prompt,
                    Err(_) => {
                        let path =
                            persist_voice_capture(&self.cwd, &stream_id, &frame.format, &payload)?;
                        return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                            note: format!("voice_frame_saved:{}", path.display()),
                        }]));
                    }
                };
                if prompt.trim().is_empty() {
                    let path = persist_voice_capture(
                        &self.cwd,
                        &stream_id,
                        &frame.format,
                        prompt.as_bytes(),
                    )?;
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: format!("voice_frame_saved:{}", path.display()),
                    }]));
                }
                self.execute_prompt(prompt).await
            }
            RemoteEnvelope::ResumeSession { request } => {
                if request.target.trim().is_empty() {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: "empty_resume_target".to_owned(),
                    }]));
                }
                self.resume_session(&request.target).await
            }
            RemoteEnvelope::TaskState { .. } => Ok(Vec::new()),
            RemoteEnvelope::Question { question } => {
                let stored = self.task_store().record_question(question.clone())?;
                Ok(self.with_session_state(vec![RemoteEnvelope::Question { question: stored }]))
            }
            RemoteEnvelope::QuestionResponse { response } => {
                let store = self.task_store();
                let stored = store.answer_question(response.clone())?;
                let resumed = resume_tasks_for_question(&store, stored.question_id)?;
                let mut outbound = vec![RemoteEnvelope::QuestionResponse { response: stored }];
                outbound.extend(
                    resumed
                        .into_iter()
                        .map(|task| RemoteEnvelope::TaskState { task }),
                );
                Ok(self.with_session_state(outbound))
            }
            RemoteEnvelope::ToolCall { call } => self.execute_remote_tool_call(call.clone()).await,
            RemoteEnvelope::PermissionResponse { response } => {
                let Some(pending) = self.pending_permission.clone() else {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: "no_pending_permission".to_owned(),
                    }]));
                };
                if pending.request.id != response.id {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Error {
                        message: format!("unknown permission request: {}", response.id),
                    }]));
                }
                self.pending_permission = None;
                if !response.approved {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::ToolResult {
                        result: code_agent_core::ToolResult {
                            tool_call_id: pending.call.id,
                            output_text: response
                                .note
                                .clone()
                                .unwrap_or_else(|| "remote tool permission denied".to_owned()),
                            is_error: true,
                        },
                    }]));
                }
                self.run_remote_tool_call(pending.call).await
            }
            RemoteEnvelope::Interrupt => Ok(vec![RemoteEnvelope::Ack {
                note: "interrupt".to_owned(),
            }]),
            RemoteEnvelope::ToolResult { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "tool_result_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Event { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "event_received".to_owned(),
                }]))
            }
            RemoteEnvelope::SessionState { .. } => Ok(Vec::new()),
            RemoteEnvelope::PermissionRequest { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "permission_request_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Error { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "error_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Ack { .. } => Ok(Vec::new()),
        }
    }
}

fn env_u64(names: &[&str], default: u64) -> u64 {
    for name in names {
        if let Ok(raw) = env::var(name) {
            if let Ok(value) = raw.trim().parse::<u64>() {
                return value;
            }
        }
    }
    default
}

fn auto_compact_threshold_tokens() -> u64 {
    env_u64(
        &[
            "CODE_AGENT_AUTO_COMPACT_THRESHOLD_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_THRESHOLD_TOKENS",
        ],
        24_000,
    )
}

fn compact_target_tokens() -> u64 {
    env_u64(
        &[
            "CODE_AGENT_COMPACT_TARGET_TOKENS",
            "CLAUDE_CODE_COMPACT_TARGET_TOKENS",
        ],
        12_000,
    )
}

async fn apply_compaction_outcome(
    store: &ActiveSessionStore,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    outcome: &CompactionOutcome,
) -> Result<()> {
    store
        .append_message(session_id, &outcome.summary_message)
        .await?;
    store
        .append_message(session_id, &outcome.boundary_message)
        .await?;
    raw_messages.push(outcome.summary_message.clone());
    raw_messages.push(outcome.boundary_message.clone());
    Ok(())
}

async fn maybe_auto_compact(
    store: &ActiveSessionStore,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
) -> Result<Option<CompactionOutcome>> {
    let estimated_tokens_before =
        estimate_message_tokens(&materialize_runtime_messages(raw_messages));
    if estimated_tokens_before <= auto_compact_threshold_tokens() {
        return Ok(None);
    }

    let outcome = compact_messages(
        raw_messages,
        &CompactionConfig {
            kind: BoundaryKind::Compact,
            trigger: "auto".to_owned(),
            max_tokens_before: Some(estimated_tokens_before),
            target_tokens_after: compact_target_tokens(),
            ..CompactionConfig::default()
        },
    );
    if let Some(outcome) = &outcome {
        apply_compaction_outcome(store, session_id, raw_messages, outcome).await?;
    }
    Ok(outcome)
}

async fn run_agent_turns(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    provider: ApiProvider,
    model: String,
    session_id: SessionId,
    messages: &mut Vec<Message>,
    auth_configured: bool,
) -> Result<(Option<code_agent_core::TokenUsage>, usize, Option<String>)> {
    const MAX_AGENT_STEPS: usize = 8;

    let provider_tools = tool_definitions(tool_registry);
    let tool_context = ToolContext {
        session_id: Some(session_id),
        cwd: cwd.clone(),
        provider: Some(provider.to_string()),
        model: Some(model.clone()),
        ..ToolContext::default()
    };

    for step in 1..=MAX_AGENT_STEPS {
        let provider_client = resolve_provider_client(provider, auth_configured).await?;
        let parent_id = messages.last().map(|message| message.id);
        let response = collect_provider_response(
            provider_client.as_ref(),
            ProviderRequest {
                model: model.clone(),
                messages: messages.clone(),
                tools: provider_tools.clone(),
                ..ProviderRequest::default()
            },
        )
        .await?;

        let latest_usage = response.usage.clone();
        let stop_reason = response.stop_reason.clone();
        let mut assistant_message = build_assistant_message(
            session_id,
            parent_id,
            response.text,
            response.tool_calls.clone(),
        );
        assistant_message.metadata.provider = Some(provider.to_string());
        assistant_message.metadata.model = Some(model.clone());
        assistant_message.metadata.usage = response.usage.clone();
        store.append_message(session_id, &assistant_message).await?;
        messages.push(assistant_message.clone());

        if response.tool_calls.is_empty() {
            return Ok((latest_usage, step, stop_reason));
        }

        for call in response.tool_calls {
            let input = serde_json::from_str(&call.input_json).unwrap_or_else(|_| json!({}));
            let output = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: call.name.clone(),
                        input,
                    },
                    &tool_context,
                )
                .await?;
            let tool_message = build_tool_result_message(
                session_id,
                call.id,
                output.content,
                output.is_error,
                messages.last().map(|message| message.id),
            );
            store.append_message(session_id, &tool_message).await?;
            messages.push(tool_message);
        }
    }

    Err(anyhow!("agent loop exceeded tool iteration limit"))
}

async fn execute_local_turn(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    prompt_text: String,
    live_runtime: bool,
) -> Result<(Option<CompactionOutcome>, usize, Option<String>, u64, u64)> {
    let parent_id = raw_messages.last().map(|message| message.id);
    let user_message = build_text_message(session_id, MessageRole::User, prompt_text, parent_id);
    store.append_message(session_id, &user_message).await?;
    raw_messages.push(user_message);

    let estimated_tokens_before =
        estimate_message_tokens(&materialize_runtime_messages(raw_messages));
    let applied_compaction = maybe_auto_compact(store, session_id, raw_messages).await?;
    let estimated_tokens_after_compaction = applied_compaction
        .as_ref()
        .map(|outcome| outcome.estimated_tokens_after)
        .unwrap_or_else(|| estimate_message_tokens(&materialize_runtime_messages(raw_messages)));
    let mut runtime_messages = materialize_runtime_messages(raw_messages);
    let (_, turn_count, stop_reason) = run_agent_turns(
        store,
        tool_registry,
        cwd,
        provider,
        active_model,
        session_id,
        &mut runtime_messages,
        live_runtime,
    )
    .await?;
    *raw_messages = store.load_session(session_id).await.unwrap_or_default();

    Ok((
        applied_compaction,
        turn_count,
        stop_reason,
        estimated_tokens_before,
        estimated_tokens_after_compaction,
    ))
}

async fn render_auth_command(provider: ApiProvider, action: &str) -> Result<String> {
    match action {
        "login" => {
            let resolver = EnvironmentAuthResolver;
            let auth = resolver
                .resolve_auth(AuthRequest {
                    provider,
                    profile: None,
                })
                .await?;
            let snapshot_path = if matches!(
                provider,
                ApiProvider::OpenAI
                    | ApiProvider::ChatGPTCodex
                    | ApiProvider::OpenAICompatible
                    | ApiProvider::FirstParty
            ) {
                Some(write_auth_snapshot(provider, &auth)?)
            } else {
                None
            };
            Ok(serde_json::to_string_pretty(&AuthCommandReport {
                provider: provider.to_string(),
                status: "ready".to_owned(),
                auth_source: auth.source,
                hint: Some(auth_hint_for_provider(provider)),
                snapshot_path,
            })?)
        }
        "logout" => Ok(serde_json::to_string_pretty(&AuthCommandReport {
            provider: provider.to_string(),
            status: if clear_auth_snapshot(provider)? {
                "cleared".to_owned()
            } else {
                "no_snapshot".to_owned()
            },
            auth_source: None,
            hint: Some(auth_hint_for_provider(provider)),
            snapshot_path: Some(code_agent_auth_snapshot_path()),
        })?),
        other => Err(anyhow!("unsupported auth action: {other}")),
    }
}

async fn render_memory_command(
    invocation: &CommandInvocation,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
) -> Result<String> {
    let action = invocation
        .args
        .first()
        .map(String::as_str)
        .unwrap_or("read");
    let input = match action {
        "read" => json!({ "action": "read" }),
        "write" => json!({
            "action": "write",
            "value": invocation.args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ")
        }),
        other => bail!("unsupported memory action: {other}"),
    };
    let report = tool_registry
        .invoke(
            ToolCallRequest {
                tool_name: "memory".to_owned(),
                input,
            },
            &ToolContext {
                cwd: cwd.to_path_buf(),
                provider: Some(provider.to_string()),
                model,
                ..ToolContext::default()
            },
        )
        .await?;
    Ok(report.content)
}

async fn render_skills_command(cwd: &Path, plugin_root: Option<&PathBuf>) -> Result<String> {
    let runtime = OutOfProcessPluginRuntime;
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    let skills = runtime.discover_skills(&root).await?;
    let commands = runtime.discover_commands(&root).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "root": root,
        "skills": skills,
        "commands": commands.into_iter().map(|spec| command_report(&spec)).collect::<Vec<_>>(),
    }))?)
}

fn render_command_help(registry: &CommandRegistry, remote_only: bool) -> String {
    let commands = if remote_only {
        registry.remote_safe()
    } else {
        registry.all()
    };
    let mut lines = vec!["REPL commands:".to_owned()];
    lines.extend(commands.into_iter().map(|spec| format!("/{:<16} {}", spec.name, spec.description)));
    lines.join("\n")
}

async fn render_permissions_command(cwd: &Path) -> Result<String> {
    let task_store = task_store_for(cwd);
    let pending = task_store
        .list_tasks()?
        .into_iter()
        .filter(|task| task.status == TaskStatus::WaitingForInput)
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&json!({
        "mode": "ask",
        "pending_requests": pending,
    }))?)
}

async fn render_session_command(store: &ActiveSessionStore, session_id: SessionId) -> Result<String> {
    let transcript_path = store.transcript_path(session_id).await?;
    let messages = store.load_session(session_id).await.unwrap_or_default();
    let runtime_messages = materialize_runtime_messages(&messages);
    let first_prompt = runtime_messages.iter().find_map(|message| {
        (message.role == MessageRole::User).then(|| message_text(message))
    });
    let report = SessionCommandReport {
        session_id,
        session_root: store.root_dir().to_path_buf(),
        transcript_path,
        message_count: messages.len(),
        runtime_message_count: runtime_messages.len(),
        first_prompt,
        last_message_preview: session_preview(&runtime_messages),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}

fn render_status_command(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    live_runtime: bool,
    cwd: &Path,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "provider": provider,
        "model": active_model,
        "session_id": session_id,
        "runtime": if live_runtime { "live" } else { "offline" },
        "task_count": task_store_for(cwd).list_tasks()?.len(),
        "question_count": task_store_for(cwd).list_questions()?.len(),
    }))?)
}

fn render_statusline_command(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "statusline": format!("{provider}  {active_model}  session {session_id}"),
    }))?)
}

fn render_theme_command() -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "status": "compatible",
        "message": "Theme selection is currently terminal-native in the Rust UI.",
    }))?)
}

fn render_vim_command(enabled: bool) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "enabled": enabled,
        "status": if enabled { "experimental" } else { "disabled" },
        "message": "Full vim state-machine parity is still in progress.",
    }))?)
}

fn render_plan_command() -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "status": "compatibility_surface_only",
        "message": "Plan-mode workflow is tracked outside the Rust runtime core.",
    }))?)
}

fn render_simple_compat_command(name: &str, message: &str) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "command": name,
        "status": "compatibility_surface_only",
        "message": message,
    }))?)
}

fn render_files_command(raw_messages: &[Message], cwd: &Path) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_file_message(&runtime_messages, cwd).unwrap_or(PanePreview {
        title: "File preview".to_owned(),
        lines: vec!["No file preview available yet.".to_owned()],
    });
    Ok(serde_json::to_string_pretty(&preview)?)
}

fn render_diff_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_diff_message(&runtime_messages).unwrap_or(PanePreview {
        title: "Diff preview".to_owned(),
        lines: vec!["No diff preview available yet.".to_owned()],
    });
    Ok(serde_json::to_string_pretty(&preview)?)
}

fn render_usage_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let total = runtime_messages
        .iter()
        .filter_map(|message| message.metadata.usage.as_ref())
        .fold((0u64, 0u64), |(input, output), usage| {
            (input + usage.input_tokens, output + usage.output_tokens)
        });
    Ok(serde_json::to_string_pretty(&json!({
        "input_tokens": total.0,
        "output_tokens": total.1,
        "message_count": runtime_messages.len(),
    }))?)
}

fn render_export_command(store: &ActiveSessionStore, session_id: SessionId) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "session_id": session_id,
        "transcript_path": store.root_dir().join(format!("{session_id}.jsonl")),
        "status": "ready",
    }))?)
}

fn render_tasks_command(invocation: &CommandInvocation, cwd: &Path) -> Result<String> {
    let store = task_store_for(cwd);
    match invocation.args.first().map(String::as_str) {
        Some("create") => {
            let assignments = parse_assignment_args(&invocation.args[1..]);
            let mut task = TaskRecord::new(
                assignments
                    .get("kind")
                    .cloned()
                    .unwrap_or_else(|| "task".to_owned()),
                assignments.get("title").cloned().unwrap_or_else(|| {
                    invocation
                        .args
                        .iter()
                        .skip(1)
                        .filter(|arg| !arg.contains('='))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                }),
            );
            if task.title.trim().is_empty() {
                task.title = "task".to_owned();
            }
            task.input = assignments.get("input").cloned();
            if let Some(status) = assignments.get("status") {
                task.status = parse_task_status(status)?;
            }
            if let Some(session_id) = assignments
                .get("session_id")
                .map(|value| parse_task_id(value))
                .transpose()?
            {
                task.session_id = Some(session_id);
            }
            let created = store.create_task(task)?;
            Ok(serde_json::to_string_pretty(&created)?)
        }
        Some("get") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks get requires a task id"))?,
            )?;
            let task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            Ok(serde_json::to_string_pretty(&task)?)
        }
        Some("update") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks update requires a task id"))?,
            )?;
            let mut task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            let assignments = parse_assignment_args(&invocation.args[2..]);
            if let Some(title) = assignments.get("title") {
                task.title = title.clone();
            }
            if let Some(kind) = assignments.get("kind") {
                task.kind = kind.clone();
            }
            if let Some(output) = assignments.get("output") {
                task.output = Some(output.clone());
            }
            if let Some(status) = assignments.get("status") {
                task.status = parse_task_status(status)?;
            }
            let saved = store.save_task(task)?;
            Ok(serde_json::to_string_pretty(&saved)?)
        }
        Some("stop") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks stop requires a task id"))?,
            )?;
            let mut task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            task.status = TaskStatus::Cancelled;
            task.output = Some("stopped from slash command".to_owned());
            Ok(serde_json::to_string_pretty(&store.save_task(task)?)?)
        }
        Some("questions") => {
            let questions = store.list_questions()?;
            Ok(serde_json::to_string_pretty(&QuestionCommandReport {
                count: questions.len(),
                questions,
            })?)
        }
        Some("responses") => {
            let responses = store.list_responses()?;
            Ok(serde_json::to_string_pretty(&ResponseCommandReport {
                count: responses.len(),
                responses,
            })?)
        }
        Some("answer") => {
            let question_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks answer requires a question id"))?,
            )?;
            let answer = invocation
                .args
                .iter()
                .skip(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let response = store.answer_question(QuestionResponse::new(question_id, answer))?;
            for mut task in store
                .list_tasks()?
                .into_iter()
                .filter(|task| task.question_id == Some(question_id))
            {
                task.status = TaskStatus::Running;
                let _ = store.save_task(task)?;
            }
            Ok(serde_json::to_string_pretty(&response)?)
        }
        _ => {
            let tasks = store.list_tasks()?;
            Ok(serde_json::to_string_pretty(&TaskCommandReport {
                count: tasks.len(),
                tasks,
            })?)
        }
    }
}

async fn render_agents_command(
    invocation: &CommandInvocation,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
    session_id: SessionId,
) -> Result<String> {
    match invocation.args.first().map(String::as_str) {
        Some("create" | "spawn") => {
            let title = invocation
                .args
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "agent".to_owned(),
                        input: json!({
                            "action": "spawn",
                            "title": if title.trim().is_empty() { "agent task" } else { title.as_str() },
                        }),
                    },
                    &ToolContext {
                        session_id: Some(session_id),
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report.metadata)?)
        }
        Some("get" | "resume") => {
            let task_id = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("agents get requires a task id"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "agent".to_owned(),
                        input: json!({
                            "action": "resume",
                            "task_id": task_id,
                        }),
                    },
                    &ToolContext {
                        session_id: Some(session_id),
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        _ => {
            let tasks = task_store_for(cwd)
                .list_tasks()?
                .into_iter()
                .filter(|task| {
                    matches!(
                        task.kind.as_str(),
                        "agent"
                            | "workflow"
                            | "workflow_step"
                            | "coordinator"
                            | "assistant_worker"
                            | "assistant_synthesis"
                    )
                })
                .collect::<Vec<_>>();
            Ok(serde_json::to_string_pretty(&TaskCommandReport {
                count: tasks.len(),
                tasks,
            })?)
        }
    }
}

async fn render_plugin_command(
    invocation: &CommandInvocation,
    plugin_root: Option<&PathBuf>,
    cwd: &Path,
) -> Result<String> {
    let root_arg = match invocation.args.first().map(String::as_str) {
        Some("bridge-start" | "bridge-stop" | "bridge-status") => {
            invocation.args.get(1).map(String::as_str)
        }
        other => other,
    };
    let root = resolve_plugin_root_with_override(plugin_root, root_arg, cwd);
    let runtime = OutOfProcessPluginRuntime;
    match invocation.args.first().map(String::as_str) {
        Some("bridge-start") => {
            let executable = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("plugin bridge-start requires an executable path"))?;
            let args = invocation
                .args
                .iter()
                .skip(if root_arg.is_some() { 3 } else { 2 })
                .cloned()
                .collect::<Vec<_>>();
            Ok(serde_json::to_string_pretty(
                &runtime
                    .start_bridge(BridgeLaunchRequest {
                        plugin_root: root,
                        executable: Some(executable),
                        args,
                        component: Some("runtime".to_owned()),
                        ..BridgeLaunchRequest::default()
                    })
                    .await?,
            )?)
        }
        Some("bridge-stop") => Ok(serde_json::to_string_pretty(
            &runtime.stop_bridge(&root, Some("runtime")).await?,
        )?),
        Some("bridge-status") => Ok(serde_json::to_string_pretty(
            &runtime.bridge_status(&root, Some("runtime")).await?,
        )?),
        _ => Ok(serde_json::to_string_pretty(
            &load_plugin_report(root).await?,
        )?),
    }
}

async fn render_mcp_command(
    invocation: &CommandInvocation,
    plugin_root: Option<&PathBuf>,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
) -> Result<String> {
    let root_arg = match invocation.args.first().map(String::as_str) {
        Some(
            "auth-status" | "auth-set" | "auth-login" | "auth-poll" | "auth-refresh" | "auth-clear",
        ) => invocation.args.get(1).map(String::as_str),
        other => other,
    };
    let root = resolve_plugin_root_with_override(plugin_root, root_arg, cwd);
    let runtime = OutOfProcessPluginRuntime;
    let plugin = runtime.load_manifest(&root).await?;
    let parsed = parse_mcp_server_configs(&plugin.manifest.mcp_servers);
    match invocation.args.first().map(String::as_str) {
        Some("auth-status") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-status requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "status"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        Some("auth-login") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-login requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "login"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        Some("auth-set") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-set requires a server name"))?;
            let token = invocation
                .args
                .get(if root_arg.is_some() { 3 } else { 2 })
                .ok_or_else(|| anyhow!("mcp auth-set requires an access token"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "set_token",
                            "access_token": token
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-poll") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-poll requires a server name"))?;
            let device_code = invocation
                .args
                .get(if root_arg.is_some() { 3 } else { 2 })
                .cloned();
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "poll",
                            "device_code": device_code,
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-refresh") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-refresh requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "refresh"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-clear") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-clear requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "clear"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        _ => Ok(serde_json::to_string_pretty(&parsed)?),
    }
}

async fn render_remote_control_command(
    registry: &CommandRegistry,
    invocation: &CommandInvocation,
    cli: &Cli,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &[Message],
    live_runtime: bool,
) -> Result<String> {
    match invocation.args.first().map(String::as_str) {
        Some("connect") => {
            if !command_allowed_for_bridge(registry, "remote-control") {
                return Ok(serde_json::to_string_pretty(&json!({
                    "status": "blocked",
                    "reason": "remote-control is not bridge-safe in the current registry",
                }))?);
            }
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control connect requires an address"))?;
            let receive_count = invocation
                .args
                .get(2)
                .and_then(|value| value.parse::<usize>().ok())
                .or(cli.bridge_receive_count)
                .unwrap_or(4);
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                build_remote_outbound(cli, session_id, None, cli.resume.as_deref())?,
                receive_count,
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("send") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control send requires an address"))?;
            let prompt_text = invocation
                .args
                .iter()
                .skip(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if prompt_text.trim().is_empty() {
                bail!("remote-control send requires a message");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                build_remote_outbound(cli, session_id, Some(prompt_text), cli.resume.as_deref())?,
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("resume") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control resume requires an address"))?;
            let target = invocation
                .args
                .get(2)
                .ok_or_else(|| anyhow!("remote-control resume requires a session target"))?;
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::ResumeSession {
                    request: ResumeSessionRequest {
                        target: target.clone(),
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("directive") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control directive requires an address"))?;
            let mut agent_id = None;
            let mut instruction_parts = Vec::new();
            for arg in invocation.args.iter().skip(2) {
                if agent_id.is_none() {
                    if let Some(value) = arg.strip_prefix("agent=") {
                        agent_id = Some(value.to_owned());
                        continue;
                    }
                }
                instruction_parts.push(arg.clone());
            }
            let instruction = instruction_parts.join(" ");
            if instruction.trim().is_empty() {
                bail!("remote-control directive requires an instruction");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::AssistantDirective {
                    directive: AssistantDirective {
                        agent_id,
                        instruction,
                        ..AssistantDirective::default()
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("answer") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control answer requires an address"))?;
            let question_id = parse_task_id(
                invocation
                    .args
                    .get(2)
                    .ok_or_else(|| anyhow!("remote-control answer requires a question id"))?,
            )?;
            let answer = invocation
                .args
                .iter()
                .skip(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if answer.trim().is_empty() {
                bail!("remote-control answer requires a response");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::QuestionResponse {
                    response: QuestionResponse::new(question_id, answer),
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("approve" | "deny") => {
            let approved = matches!(invocation.args.first().map(String::as_str), Some("approve"));
            let address = invocation.args.get(1).ok_or_else(|| {
                anyhow!(
                    "remote-control {} requires an address",
                    if approved { "approve" } else { "deny" }
                )
            })?;
            let permission_id = invocation.args.get(2).ok_or_else(|| {
                anyhow!(
                    "remote-control {} requires a permission id",
                    if approved { "approve" } else { "deny" }
                )
            })?;
            let note = invocation
                .args
                .iter()
                .skip(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::PermissionResponse {
                    response: code_agent_bridge::RemotePermissionResponse {
                        id: permission_id.clone(),
                        approved,
                        note: (!note.trim().is_empty()).then_some(note),
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("serve") => {
            let bind_address = invocation
                .args
                .get(1)
                .cloned()
                .or_else(|| cli.bridge_server.clone())
                .ok_or_else(|| anyhow!("remote-control serve requires a bind address"))?;
            let mode = remote_mode_for_address(&bind_address);
            let handler = LocalBridgeHandler {
                store,
                tool_registry,
                cwd: cwd.to_path_buf(),
                provider,
                active_model: active_model.to_owned(),
                session_id,
                raw_messages: raw_messages.to_vec(),
                live_runtime,
                allow_remote_tools: true,
                pending_permission: None,
                voice_streams: BTreeMap::new(),
            };
            let config = BridgeServerConfig {
                bind_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            };
            let record = match mode {
                RemoteMode::DirectConnect | RemoteMode::IdeBridge => {
                    serve_direct_session(config, handler).await?
                }
                _ => serve_bridge_session(config, handler).await?,
            };
            Ok(serde_json::to_string_pretty(&record)?)
        }
        _ => Ok(serde_json::to_string_pretty(&json!({
            "provider": provider,
            "model": active_model,
            "session_id": session_id,
            "session_root": store.root_dir(),
            "task_count": task_store_for(cwd).list_tasks()?.len(),
            "question_count": task_store_for(cwd).list_questions()?.len(),
            "bridge_server": cli.bridge_server,
            "bridge_connect": cli.bridge_connect,
            "receive_count": cli.bridge_receive_count,
        }))?),
    }
}

async fn handle_repl_slash_command(
    registry: &CommandRegistry,
    invocation: CommandInvocation,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: &mut String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    live_runtime: bool,
    vim_state: &mut code_agent_ui::vim::VimState,
    remote_mode: bool,
) -> Result<String> {
    if !command_allowed_in_repl(registry, remote_mode, &invocation.name) {
        return Ok(format!("command '/{}' is unavailable in remote mode", invocation.name));
    }
    match invocation.name.as_str() {
        "help" => Ok(render_command_help(registry, remote_mode)),
        "version" => Ok(env!("CARGO_PKG_VERSION").to_owned()),
        "config" => {
            if matches!(invocation.args.first().map(String::as_str), Some("migrate")) {
                Ok(serde_json::to_string_pretty(&config_migration_report(provider))?)
            } else {
                Ok(format!(
                    "provider={} model={} session={} runtime={}",
                    provider,
                    active_model,
                    session_id,
                    if live_runtime { "live" } else { "offline" }
                ))
            }
        }
        "model" => {
            let Some(model) = invocation.args.first() else {
                return Ok(format!("current model={active_model}"));
            };
            let catalog = compatibility_model_catalog(provider);
            if catalog.get_model(model).is_none() {
                return Ok(format!("unknown compatibility model: {model}"));
            }
            *active_model = model.clone();
            Ok(format!("model switched to {active_model}"))
        }
        "compact" => {
            let estimated_tokens_before =
                estimate_message_tokens(&materialize_runtime_messages(raw_messages));
            let outcome = compact_messages(
                raw_messages,
                &CompactionConfig {
                    trigger: "manual".to_owned(),
                    max_tokens_before: Some(estimated_tokens_before),
                    target_tokens_after: compact_target_tokens(),
                    ..CompactionConfig::default()
                },
            );
            if let Some(outcome) = outcome {
                apply_compaction_outcome(store, session_id, raw_messages, &outcome).await?;
                return Ok(format!(
                    "compacted {} messages to ~{} tokens",
                    outcome.summarized_message_count, outcome.estimated_tokens_after
                ));
            }
            Ok("nothing to compact".to_owned())
        }
        "clear" => {
            let transcript_path = store.transcript_path(session_id).await?;
            if transcript_path.exists() {
                fs::remove_file(&transcript_path)?;
            }
            raw_messages.clear();
            Ok(format!("cleared session {}", session_id))
        }
        "resume" => Ok("restart with --resume to switch transcripts".to_owned()),
        "session" => render_session_command(store, session_id).await,
        "login" => render_auth_command(provider, "login").await,
        "logout" => render_auth_command(provider, "logout").await,
        "permissions" => render_permissions_command(cwd).await,
        "plugin" => render_plugin_command(&invocation, plugin_root, cwd).await,
        "skills" => render_skills_command(cwd, plugin_root).await,
        "reload-plugins" => render_skills_command(cwd, plugin_root).await,
        "hooks" => render_simple_compat_command(
            "hooks",
            "Hook discovery is exposed through plugin manifests in the Rust runtime.",
        ),
        "output-style" => render_simple_compat_command(
            "output-style",
            "Output styles are discovered from plugin manifests but alternate renderers remain limited.",
        ),
        "mcp" => {
            render_mcp_command(
                &invocation,
                plugin_root,
                tool_registry,
                cwd,
                provider,
                Some(active_model.clone()),
            )
            .await
        }
        "memory" => {
            render_memory_command(&invocation, tool_registry, cwd, provider, Some(active_model.clone())).await
        }
        "files" => render_files_command(raw_messages, cwd),
        "diff" => render_diff_command(raw_messages),
        "usage" | "cost" | "stats" => render_usage_command(raw_messages),
        "status" => render_status_command(provider, active_model, session_id, live_runtime, cwd),
        "statusline" => render_statusline_command(provider, active_model, session_id),
        "theme" => render_theme_command(),
        "vim" => {
            vim_state.enabled = !vim_state.enabled;
            if vim_state.enabled {
                vim_state.enter_normal();
            } else {
                vim_state.mode = code_agent_ui::vim::VimMode::Insert;
            }
            render_vim_command(vim_state.enabled)
        }
        "plan" => render_plan_command(),
        "fast" => render_simple_compat_command(
            "fast",
            "Fast mode uses the same model family with lower latency-focused behavior.",
        ),
        "passes" => render_simple_compat_command(
            "passes",
            "Pass-count tuning is not yet modeled separately in the Rust runtime.",
        ),
        "effort" => render_simple_compat_command(
            "effort",
            "Reasoning effort tuning remains compatibility-surface only in the current build.",
        ),
        "remote-env" => render_simple_compat_command(
            "remote-env",
            "Remote environment reporting currently flows through bridge and session status surfaces.",
        ),
        "export" => render_export_command(store, session_id),
        "tasks" => render_tasks_command(&invocation, cwd),
        "agents" => {
            render_agents_command(
                &invocation,
                tool_registry,
                cwd,
                provider,
                Some(active_model.clone()),
                session_id,
            )
            .await
        }
        "remote-control" => {
            render_remote_control_command(
                registry,
                &invocation,
                &Cli::default(),
                store,
                tool_registry,
                cwd,
                provider,
                active_model,
                session_id,
                raw_messages,
                live_runtime,
            )
            .await
        }
        "voice" => Ok("voice features are intentionally deferred in this build".to_owned()),
        "exit" | "quit" => Ok("exit".to_owned()),
        other => Err(anyhow!("unknown registered REPL command: {other}")),
    }
}

async fn run_interactive_repl(
    store: &ActiveSessionStore,
    registry: &code_agent_core::CommandRegistry,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    live_runtime: bool,
    remote_mode: bool,
) -> Result<()> {
    let mut active_model = active_model;
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut out = stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, Hide)?;

    let loop_result = async {
        let mut input_buffer = code_agent_ui::InputBuffer::new();
        let mut status_line = format!("{provider}  {active_model}  session {session_id}");
        let mut active_pane = PaneKind::Transcript;
        let mut compact_banner = None;
        loop {
            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
            let mut state = build_repl_ui_state(
                &app,
                registry,
                raw_messages,
                &cwd,
                &input_buffer,
                &status_line,
                active_pane,
                compact_banner.clone(),
            );
            state.vim_state = vim_state.clone();

            let (width, height) = terminal_size().unwrap_or((110, 28));
            let rendered = render_tui_to_string(&state, width.max(80), height.max(24))?;
            queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
            write!(out, "{rendered}")?;
            out.flush()?;

            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Esc => {
                    if vim_state.enabled {
                        if matches!(vim_state.mode, code_agent_ui::vim::VimMode::Insert) {
                            vim_state.enter_normal();
                        } else {
                            vim_state.mode = code_agent_ui::vim::VimMode::Normal(code_agent_ui::vim::CommandState::Idle);
                        }
                    } else {
                        break;
                    }
                }
                KeyCode::Tab => {
                    active_pane = rotate_pane(active_pane, true);
                }
                KeyCode::BackTab => {
                    active_pane = rotate_pane(active_pane, false);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char(ch) if key.modifiers.is_empty() => {
                    if let Some(pane) = pane_from_digit(ch) {
                        active_pane = pane;
                    } else if vim_state.is_insert() {
                        input_buffer.push(ch);
                    } else {
                        if let code_agent_ui::vim::VimMode::Normal(ref mut cmd_state) = vim_state.mode {
                            let transition = code_agent_ui::vim::handle_normal_key(cmd_state, ch);
                            match transition {
                                code_agent_ui::vim::VimTransition::EnterInsert => {
                                    vim_state.enter_insert();
                                }
                                code_agent_ui::vim::VimTransition::MoveCursor(delta) => {
                                    let mut new_pos = input_buffer.cursor as isize + delta;
                                    if new_pos < 0 { new_pos = 0; }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    if new_pos > max_pos as isize && !input_buffer.is_empty() {
                                        new_pos = max_pos as isize;
                                    }
                                    input_buffer.cursor = new_pos as usize;
                                }
                                code_agent_ui::vim::VimTransition::SetCursor(pos) => {
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = pos.min(max_pos);
                                }
                                code_agent_ui::vim::VimTransition::DeleteChars(mut amount) => {
                                    while amount > 0 && input_buffer.cursor < input_buffer.chars.len() {
                                        input_buffer.chars.remove(input_buffer.cursor);
                                        amount -= 1;
                                    }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = input_buffer.cursor.min(max_pos);
                                }
                                code_agent_ui::vim::VimTransition::ReplaceChar(r) => {
                                    if input_buffer.cursor < input_buffer.chars.len() {
                                        input_buffer.chars[input_buffer.cursor] = r;
                                    }
                                }
                                code_agent_ui::vim::VimTransition::None => {}
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    let prompt_text = input_buffer.as_str().trim().to_owned();
                    if prompt_text.is_empty() {
                        continue;
                    }
                    if prompt_text == "/quit" || prompt_text == "/exit" {
                        break;
                    }
                    input_buffer.clear();
                    compact_banner = None;
                    if let Some(invocation) = registry.parse_slash_command(&prompt_text) {
                        match handle_repl_slash_command(
                            registry,
                            invocation,
                            store,
                            tool_registry,
                            &cwd,
                            plugin_root,
                            provider,
                            &mut active_model,
                            session_id,
                            raw_messages,
                            live_runtime,
                            &mut vim_state,
                            remote_mode,
                        )
                        .await
                        {
                            Ok(next_status) if next_status == "exit" => break,
                            Ok(next_status) => {
                                status_line = format!("{provider}  {active_model}  {next_status}");
                                if next_status.starts_with("compacted ") {
                                    compact_banner = Some(next_status.clone());
                                }
                            }
                            Err(error) => {
                                status_line = format!("{provider}  {active_model}  error: {error}");
                            }
                        }
                        continue;
                    }

                    match execute_local_turn(
                        store,
                        tool_registry,
                        cwd.clone(),
                        provider,
                        active_model.clone(),
                        session_id,
                        raw_messages,
                        prompt_text,
                        live_runtime,
                    )
                    .await
                    {
                        Ok((applied_compaction, turn_count, stop_reason, _, _)) => {
                            compact_banner = applied_compaction.as_ref().and_then(|outcome| {
                                compaction_kind_name(outcome).map(|kind| format!("compacted {kind}"))
                            });
                            status_line = if let Some(kind) =
                                applied_compaction.as_ref().and_then(compaction_kind_name)
                            {
                                format!(
                                    "{provider}  {active_model}  last turn: {turn_count} steps, {:?}, compacted {}",
                                    stop_reason, kind
                                )
                            } else {
                                format!(
                                    "{provider}  {active_model}  last turn: {turn_count} steps, {:?}",
                                    stop_reason
                                )
                            };
                        }
                        Err(error) => {
                            status_line = format!("{provider}  {active_model}  error: {error}");
                        }
                    }
                }
                KeyCode::Backspace => {
                    if vim_state.is_insert() {
                        input_buffer.pop();
                    } else {
                        if input_buffer.cursor > 0 {
                            input_buffer.cursor -= 1;
                        }
                    }
                }
                _ => {}
            }
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    disable_raw_mode().ok();
    execute!(out, Show, LeaveAlternateScreen).ok();
    loop_result
}

async fn handle_slash_command(
    registry: &CommandRegistry,
    invocation: CommandInvocation,
    cli: &Cli,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    provider: ApiProvider,
    model: Option<String>,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &[Message],
    live_runtime: bool,
    cwd: &Path,
    auth_source: Option<String>,
) -> Result<()> {
    match invocation.name.as_str() {
        "help" => println!("{}", render_command_help(registry, false)),
        "version" => println!("{}", env!("CARGO_PKG_VERSION")),
        "session" => println!("{}", render_session_command(store, session_id).await?),
        "permissions" => println!("{}", render_permissions_command(cwd).await?),
        "status" => println!("{}", render_status_command(provider, active_model, session_id, live_runtime, cwd)?),
        "statusline" => println!("{}", render_statusline_command(provider, active_model, session_id)?),
        "theme" => println!("{}", render_theme_command()?),
        "vim" => println!("{}", render_vim_command(false)?),
        "plan" => println!("{}", render_plan_command()?),
        "fast" => println!("{}", render_simple_compat_command("fast", "Fast mode uses the same model family with lower latency-focused behavior.")?),
        "passes" => println!("{}", render_simple_compat_command("passes", "Pass-count tuning is not yet modeled separately in the Rust runtime.")?),
        "effort" => println!("{}", render_simple_compat_command("effort", "Reasoning effort tuning remains compatibility-surface only in the current build.")?),
        "skills" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "reload-plugins" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "hooks" => println!("{}", render_simple_compat_command("hooks", "Hook discovery is exposed through plugin manifests in the Rust runtime.")?),
        "output-style" => println!("{}", render_simple_compat_command("output-style", "Output styles are discovered from plugin manifests but alternate renderers remain limited.")?),
        "files" => println!("{}", render_files_command(raw_messages, cwd)?),
        "diff" => println!("{}", render_diff_command(raw_messages)?),
        "usage" | "cost" | "stats" => println!("{}", render_usage_command(raw_messages)?),
        "remote-env" => println!("{}", render_simple_compat_command("remote-env", "Remote environment reporting currently flows through bridge and session status surfaces.")?),
        "export" => println!("{}", render_export_command(store, session_id)?),
        "resume" => {
            if matches!(invocation.args.first().map(String::as_str), Some("import")) {
                let source = invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("resume import requires a .jsonl path"))?;
                let imported = import_transcript_to_session_root(
                    &JsonlTranscriptCodec,
                    Path::new(source),
                    store.root_dir(),
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&imported)?);
            } else if let Some(target) = invocation.args.first() {
                let (session_id, transcript_path, messages) =
                    store.load_resume_target(target).await?;
                let runtime_messages = materialize_runtime_messages(&messages);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ResumeReport {
                        session_id,
                        transcript_path,
                        message_count: messages.len(),
                        preview: prompt_preview(&runtime_messages),
                    })?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.list_sessions().await?)?
                );
            }
        }
        "clear" => {
            let target = if let Some(target) = invocation.args.first().cloned() {
                target
            } else if let Some(target) = cli.clear_session.clone() {
                target
            } else if let Some(target) = cli.resume.clone() {
                target
            } else {
                store
                    .list_sessions()
                    .await?
                    .first()
                    .map(|entry| entry.transcript_path.display().to_string())
                    .ok_or_else(|| {
                        anyhow!("clear requires --resume, --clear-session, or an existing session")
                    })?
            };
            let (_, path, _) = store.load_resume_target(&target).await?;
            if path.exists() {
                fs::remove_file(&path)?;
            }
            println!("{}", json!({ "cleared": path }));
        }
        "compact" => {
            let target = if let Some(target) = invocation.args.first().cloned() {
                target
            } else if let Some(target) = cli.resume.clone() {
                target
            } else {
                store
                    .list_sessions()
                    .await?
                    .first()
                    .map(|entry| entry.transcript_path.display().to_string())
                    .ok_or_else(|| anyhow!("compact requires --resume or an existing session"))?
            };
            let (session_id, path, mut messages) = store.load_resume_target(&target).await?;
            let estimated_tokens_before =
                estimate_message_tokens(&materialize_runtime_messages(&messages));
            let outcome = compact_messages(
                &messages,
                &CompactionConfig {
                    kind: BoundaryKind::Compact,
                    trigger: "manual".to_owned(),
                    max_tokens_before: Some(estimated_tokens_before),
                    target_tokens_after: compact_target_tokens(),
                    ..CompactionConfig::default()
                },
            );
            if let Some(outcome) = outcome {
                apply_compaction_outcome(store, session_id, &mut messages, &outcome).await?;
                println!(
                    "{}",
                    json!({
                        "compacted": path,
                        "session_id": session_id,
                        "summarized_message_count": outcome.summarized_message_count,
                        "preserved_message_count": outcome.preserved_message_count,
                        "estimated_tokens_before": outcome.estimated_tokens_before,
                        "estimated_tokens_after": outcome.estimated_tokens_after,
                    })
                );
            } else {
                println!(
                    "{}",
                    json!({
                        "compacted": false,
                        "session_id": session_id,
                        "reason": "already_under_target",
                        "estimated_tokens_before": estimated_tokens_before,
                    })
                );
            }
        }
        "model" => {
            let catalog = compatibility_model_catalog(provider);
            if let Some(selected) = model {
                println!("{}", json!({ "provider": provider, "model": selected }));
            } else {
                println!("{}", serde_json::to_string_pretty(&catalog.list_models())?);
            }
        }
        "config" => {
            if matches!(invocation.args.first().map(String::as_str), Some("migrate")) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config_migration_report(provider))?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "provider": provider,
                        "model": model,
                        "cwd": cwd,
                        "project_dir": get_project_dir(cwd),
                        "session_root": store.root_dir(),
                        "auth_source": auth_source,
                        "auto_compact_threshold_tokens": auto_compact_threshold_tokens(),
                        "compact_target_tokens": compact_target_tokens(),
                    }))?
                );
            }
        }
        "login" => {
            println!("{}", render_auth_command(provider, "login").await?);
        }
        "logout" => {
            println!("{}", render_auth_command(provider, "logout").await?);
        }
        "plugin" => {
            println!(
                "{}",
                render_plugin_command(&invocation, cli.plugin_root.as_ref(), cwd).await?
            );
        }
        "mcp" => {
            println!(
                "{}",
                render_mcp_command(
                    &invocation,
                    cli.plugin_root.as_ref(),
                    tool_registry,
                    cwd,
                    provider,
                    model.clone(),
                )
                .await?
            );
        }
        "memory" => println!(
            "{}",
            render_memory_command(&invocation, tool_registry, cwd, provider, model.clone()).await?
        ),
        "tasks" => println!("{}", render_tasks_command(&invocation, cwd)?),
        "agents" => {
            println!(
                "{}",
                render_agents_command(
                    &invocation,
                    tool_registry,
                    cwd,
                    provider,
                    model.clone(),
                    session_id,
                )
                .await?
            );
        }
        "remote-control" => {
            println!(
                "{}",
                render_remote_control_command(
                    registry,
                    &invocation,
                    cli,
                    store,
                    tool_registry,
                    cwd,
                    provider,
                    active_model,
                    session_id,
                    raw_messages,
                    live_runtime,
                )
                .await?
            );
        }
        "voice" => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": "deferred",
                "message": "voice features are intentionally excluded from the current finish target",
            }))?
        ),
        other => bail!("unknown registered command: {other}"),
    }
    Ok(())
}

fn parse_input(input: Option<&str>) -> Result<Value> {
    match input {
        Some(raw) if !raw.trim().is_empty() => Ok(serde_json::from_str(raw)?),
        _ => Ok(json!({})),
    }
}

fn resolve_plugin_root_with_override(
    plugin_root: Option<&PathBuf>,
    candidate: Option<&str>,
    cwd: &Path,
) -> PathBuf {
    match candidate {
        Some(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        _ => plugin_root.cloned().unwrap_or_else(|| cwd.to_path_buf()),
    }
}

fn resolve_plugin_root(cli: &Cli, candidate: Option<&str>, cwd: &Path) -> PathBuf {
    resolve_plugin_root_with_override(cli.plugin_root.as_ref(), candidate, cwd)
}

fn command_count(commands: Option<&CommandDefinitions>) -> usize {
    match commands {
        Some(CommandDefinitions::Single(_)) => 1,
        Some(CommandDefinitions::List(items)) => items.len(),
        Some(CommandDefinitions::Mapping(items)) => items.len(),
        None => 0,
    }
}

async fn load_plugin_report(root: PathBuf) -> Result<PluginReport> {
    let runtime = OutOfProcessPluginRuntime;
    let loaded = runtime.load_manifest(&root).await?;
    let skills = runtime.discover_skills(&root).await?;
    let commands = runtime.discover_commands(&root).await?;
    let mut skill_names = skills
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    skill_names.sort();
    let mut command_names = commands
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    command_names.sort();

    let mut mcp_server_names = parse_mcp_server_configs(&loaded.manifest.mcp_servers)
        .into_keys()
        .collect::<Vec<_>>();
    mcp_server_names.sort();

    let mut lsp_server_names = loaded
        .manifest
        .lsp_servers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    lsp_server_names.sort();

    Ok(PluginReport {
        root,
        name: loaded.manifest.name,
        version: loaded.manifest.version,
        description: loaded.manifest.description,
        skill_names,
        command_names,
        mcp_server_names,
        lsp_server_names,
        command_count: command_count(loaded.manifest.commands.as_ref()),
        has_agents: loaded.manifest.agents.is_some(),
        has_output_styles: loaded.manifest.output_styles.is_some(),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = parse_cli();
    let provider = resolve_api_provider(cli.provider.as_deref())?;
    let cwd = env::current_dir()?;
    let project_dir = get_project_dir(&cwd);
    let prompt = (!cli.prompt.is_empty()).then(|| cli.prompt.join(" "));
    let tool_registry = compatibility_tool_registry();
    let store = ActiveSessionStore::new(
        cwd.clone(),
        cli.session_root
            .clone()
            .or_else(|| env::var_os("CLAUDE_CODE_SESSION_DIR").map(PathBuf::from)),
    );

    let registry = resolved_command_registry(&cwd, cli.plugin_root.as_ref()).await;

    if cli.list_commands {
        println!("{}", render_command_help(&registry, false));
        return Ok(());
    }

    if cli.list_sessions {
        println!(
            "{}",
            serde_json::to_string_pretty(&store.list_sessions().await?)?
        );
        return Ok(());
    }

    if cli.show_plugin {
        let root = resolve_plugin_root(&cli, None, &cwd);
        println!(
            "{}",
            serde_json::to_string_pretty(&load_plugin_report(root).await?)?
        );
        return Ok(());
    }

    if cli.list_skills {
        let runtime = OutOfProcessPluginRuntime;
        let root = resolve_plugin_root(&cli, None, &cwd);
        let skills = runtime.discover_skills(&root).await?;
        println!("{}", serde_json::to_string_pretty(&skills)?);
        return Ok(());
    }

    if cli.list_mcp {
        let runtime = OutOfProcessPluginRuntime;
        let root = resolve_plugin_root(&cli, None, &cwd);
        let plugin = runtime.load_manifest(&root).await?;
        let parsed = parse_mcp_server_configs(&plugin.manifest.mcp_servers);
        println!("{}", serde_json::to_string_pretty(&parsed)?);
        return Ok(());
    }

    if let Some(address) = cli.bridge_connect.clone() {
        let session_id = cli
            .resume
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            .unwrap_or_else(Uuid::new_v4);
        let mode = remote_mode_for_address(&address);
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(mode.clone()),
                scheme: match mode {
                    RemoteMode::DirectConnect => "tcp".to_owned(),
                    RemoteMode::IdeBridge => "ide".to_owned(),
                    _ => "ws".to_owned(),
                },
                address,
                session_id: Some(session_id),
                ..RemoteEndpoint::default()
            },
            build_remote_outbound(&cli, session_id, prompt.clone(), cli.resume.as_deref())?,
            cli.bridge_receive_count.unwrap_or(1),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&inbound)?);
        return Ok(());
    }

    if let Some(target) = cli.clear_session.as_deref() {
        let (_, path, _) = store.load_resume_target(target).await?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        println!("{}", json!({ "cleared": path }));
        return Ok(());
    }

    if let Some(tool_name) = cli.tool.as_deref() {
        let report = run_tool(
            tool_name,
            parse_input(cli.input.as_deref())?,
            cwd.clone(),
            provider,
            cli.model.clone(),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let auth_resolver = EnvironmentAuthResolver;
    let auth = auth_resolver
        .resolve_auth(AuthRequest {
            provider,
            profile: None,
        })
        .await
        .ok();
    let auth_source = auth.as_ref().and_then(|value| value.source.clone());
    let parsed_command = prompt
        .as_deref()
        .and_then(|input| registry.parse_slash_command(input))
        .map(|command| {
            if command.args.is_empty() {
                command.name
            } else {
                format!("{} {}", command.name, command.args.join(" "))
            }
        });

    let explicit_resume = match cli.resume.as_deref() {
        Some(target) => Some(store.load_resume_target(target).await?),
        None => None,
    };
    let listed_sessions = if cli.resume.is_some() {
        Vec::new()
    } else {
        store.list_sessions().await.unwrap_or_default()
    };
    let (session_id, transcript_path, mut existing_messages) =
        choose_active_session(&cli, &listed_sessions, explicit_resume)?;
    let active_model = cli
        .model
        .clone()
        .or_else(|| {
            compatibility_model_catalog(provider)
                .list_models()
                .first()
                .map(|model| model.id.clone())
        })
        .ok_or_else(|| anyhow!("no compatibility model catalog entries for {provider}"))?;
    let live_runtime = auth.is_some() && provider_supports_live_runtime(provider);

    if let Some(bind_address) = cli.bridge_server.clone() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let mode = remote_mode_for_address(&bind_address);
        let allow_remote_tools = true;
        let handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: cwd.clone(),
            provider,
            active_model: active_model.clone(),
            session_id,
            raw_messages: existing_messages,
            live_runtime,
            allow_remote_tools,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        let config = BridgeServerConfig {
            bind_address,
            session_id: Some(session_id),
            allow_remote_tools,
        };
        let record = match mode {
            RemoteMode::DirectConnect | RemoteMode::IdeBridge => {
                serve_direct_session(config, handler).await?
            }
            _ => serve_bridge_session(config, handler).await?,
        };
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    if cli.tui && prompt.is_none() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let runtime_messages = materialize_runtime_messages(&existing_messages);
        let title = format!("{provider}  {active_model}");
        let app = RatatuiApp::new(title);
        let mut state = app.state_from_messages(runtime_messages, &registry.all());
        if let Some(path) = transcript_path.as_ref() {
            state.status_line = format!("{}  {}", state.status_line, path.display());
        }
        println!("{}", render_tui_to_string(&state, 110, 28)?);
        return Ok(());
    }

    if cli.repl {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        run_interactive_repl(
            &store,
            &registry,
            &tool_registry,
            cwd.clone(),
            cli.plugin_root.as_ref(),
            provider,
            active_model.clone(),
            session_id,
            &mut existing_messages,
            live_runtime,
            remote_mode_enabled(&cli),
        )
        .await?;
        return Ok(());
    }

    if let Some(prompt_text) = prompt.clone() {
        if let Some(invocation) = registry.parse_slash_command(&prompt_text) {
            handle_slash_command(
                &registry,
                invocation,
                &cli,
                &store,
                &tool_registry,
                provider,
                cli.model.clone(),
                &active_model,
                session_id,
                &existing_messages,
                live_runtime,
                &cwd,
                auth_source,
            )
            .await?;
            return Ok(());
        }

        let transcript_path = match transcript_path {
            Some(path) => path,
            None => store.transcript_path(session_id).await?,
        };
        let parent_id = existing_messages.last().map(|message| message.id);
        let user_message = build_text_message(
            session_id,
            MessageRole::User,
            prompt_text.clone(),
            parent_id,
        );
        store.append_message(session_id, &user_message).await?;
        existing_messages.push(user_message.clone());
        let estimated_tokens_before =
            estimate_message_tokens(&materialize_runtime_messages(&existing_messages));
        let applied_compaction =
            maybe_auto_compact(&store, session_id, &mut existing_messages).await?;
        let estimated_tokens_after = applied_compaction
            .as_ref()
            .map(|outcome| outcome.estimated_tokens_after)
            .or(Some(estimate_message_tokens(
                &materialize_runtime_messages(&existing_messages),
            )));
        let mut runtime_messages = materialize_runtime_messages(&existing_messages);

        let (_assistant_usage, turn_count, stop_reason) = run_agent_turns(
            &store,
            &tool_registry,
            cwd.clone(),
            provider,
            active_model.clone(),
            session_id,
            &mut runtime_messages,
            live_runtime,
        )
        .await?;

        let report = StartupReport {
            provider: provider.to_string(),
            model: Some(active_model),
            cwd,
            project_dir,
            session_root: store.root_dir().to_path_buf(),
            command_count: registry.all().len(),
            prompt: Some(prompt_text),
            parsed_command: None,
            active_session_id: Some(session_id),
            transcript_path: Some(transcript_path),
            auth_source: auth_source.clone(),
            turn_count,
            stop_reason,
            applied_compaction: applied_compaction.as_ref().and_then(compaction_kind_name),
            estimated_tokens_before: Some(estimated_tokens_before),
            estimated_tokens_after,
            note: "Provider-backed runtime is active. Sessions, compaction, slash commands, tool execution, TUI REPL, MCP transport execution, bridge server/client flows, and multi-step agent turns now persist locally.",
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let report = StartupReport {
        provider: provider.to_string(),
        model: cli.model.clone(),
        cwd,
        project_dir,
        session_root: store.root_dir().to_path_buf(),
        command_count: registry.all().len(),
        prompt,
        parsed_command,
        active_session_id: Some(session_id),
        transcript_path,
        auth_source,
        turn_count: 0,
        stop_reason: None,
        applied_compaction: None,
        estimated_tokens_before: None,
        estimated_tokens_after: None,
        note: "Local runtime shell is active. Use --list-sessions, --resume, --tool, --repl, or a slash command prompt to exercise persisted sessions, tools, plugins, MCP, and remote-control flows.",
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_text_message, handle_repl_slash_command, message_text, render_remote_control_command,
        resolved_command_registry, ActiveSessionStore, Cli, LocalBridgeHandler, Message,
        MessageRole,
    };
    use code_agent_bridge::{
        base64_encode, serve_direct_session, AssistantDirective, BridgeServerConfig,
        BridgeSessionHandler, RemoteEnvelope, RemotePermissionResponse, ResumeSessionRequest,
        VoiceFrame,
    };
    use code_agent_core::{compatibility_command_registry, CommandInvocation, ContentBlock, SessionId};
    use code_agent_providers::ApiProvider;
    use code_agent_session::LocalSessionStore;
    use code_agent_tools::compatibility_tool_registry;
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    #[derive(Deserialize)]
    struct SlashCommandFixture {
        cases: Vec<SlashCommandCase>,
    }

    #[derive(Deserialize)]
    struct SlashCommandCase {
        input: String,
        name: String,
        args: Vec<String>,
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn temp_session_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("code-agent-rust-{label}-{}", Uuid::new_v4()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn temp_tcp_address() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        format!("tcp://{address}")
    }

    fn write_test_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn parses_fixture_backed_slash_commands() {
        let fixture_path = workspace_root().join("fixtures/command-golden/slash-commands.json");
        let fixture = serde_json::from_str::<SlashCommandFixture>(&fs::read_to_string(fixture_path).unwrap()).unwrap();
        let registry = compatibility_command_registry();

        for case in fixture.cases {
            let parsed = registry.parse_slash_command(&case.input).unwrap();
            assert_eq!(parsed.name, case.name);
            assert_eq!(parsed.args, case.args);
        }
    }

    #[tokio::test]
    async fn config_migrate_reports_compatibility_inputs() {
        let store =
            ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("config-migrate")));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let root = env::temp_dir();
        let registry = resolved_command_registry(&root, None).await;
        let mut vim_state = code_agent_ui::vim::VimState::default();
        let invocation = CommandInvocation {
            name: "config".to_owned(),
            args: vec!["migrate".to_owned()],
            raw_input: "/config migrate".to_owned(),
        };
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();

        let status = handle_repl_slash_command(
            &registry,
            invocation,
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("\"provider\": \"openai\""));
    }

    #[tokio::test]
    async fn repl_config_command_reports_runtime_state() {
        let store =
            ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-config")));
        let tool_registry = compatibility_tool_registry();
        let root = env::temp_dir();
        let registry = resolved_command_registry(&root, None).await;
        let mut active_model = "claude-sonnet-4-6".to_owned();
        let session_id = SessionId::new_v4();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "config".to_owned(),
                raw_input: "/config".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::FirstParty,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("provider=firstParty"));
        assert!(status.contains("runtime=offline"));
    }

    #[tokio::test]
    async fn repl_model_command_switches_active_model() {
        let store =
            ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-model")));
        let tool_registry = compatibility_tool_registry();
        let root = env::temp_dir();
        let registry = resolved_command_registry(&root, None).await;
        let mut active_model = "gpt-5".to_owned();
        let session_id = SessionId::new_v4();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "model".to_owned(),
                args: vec!["gpt-5-mini".to_owned()],
                raw_input: "/model gpt-5-mini".to_owned(),
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            true,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert_eq!(active_model, "gpt-5-mini");
        assert!(status.contains("model switched"));
    }

    #[tokio::test]
    async fn repl_clear_command_resets_transcript_state() {
        let store =
            ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-clear")));
        let tool_registry = compatibility_tool_registry();
        let root = env::temp_dir();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let transcript_path = store.transcript_path(session_id).await.unwrap();
        let mut raw_messages = vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
        )];
        let persisted =
            build_text_message(session_id, MessageRole::User, "persist".to_owned(), None);
        store.append_message(session_id, &persisted).await.unwrap();
        let mut active_model = "claude-sonnet-4-6".to_owned();
        let mut vim_state = code_agent_ui::vim::VimState::default();

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "clear".to_owned(),
                raw_input: "/clear".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::FirstParty,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("cleared session"));
        assert!(raw_messages.is_empty());
        assert!(!transcript_path.exists());
    }

    #[tokio::test]
    async fn repl_tasks_command_creates_and_lists_tasks() {
        let root = temp_session_root("repl-tasks");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();

        let created = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "tasks".to_owned(),
                args: vec![
                    "create".to_owned(),
                    "title=review".to_owned(),
                    "status=running".to_owned(),
                ],
                raw_input: "/tasks create title=review status=running".to_owned(),
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();
        let listed = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "tasks".to_owned(),
                raw_input: "/tasks".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(created.contains("\"title\": \"review\""));
        assert!(listed.contains("\"count\": 1"));
        assert!(listed.contains("\"status\": \"Running\""));
    }

    #[tokio::test]
    async fn repl_plugin_command_reports_manifest_details() {
        let root = temp_session_root("repl-plugin");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();
        write_test_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name": "review-tools",
              "version": "1.0.0",
              "description": "Review helpers",
              "skills": "./skills/review",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp"
                }
              }
            }"#,
        );
        write_test_file(&root.join("skills/review/SKILL.md"), "# Review\n");

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "plugin".to_owned(),
                raw_input: "/plugin".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("\"name\": \"review-tools\""));
        assert!(status.contains("\"mcp_server_names\""));
        assert!(status.contains("\"skill_names\""));
    }

    #[tokio::test]
    async fn repl_mcp_command_lists_parsed_servers_and_auth() {
        let root = temp_session_root("repl-mcp");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();
        write_test_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name": "mcp-tools",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp",
                  "auth": {
                    "type": "oauth_device",
                    "clientId": "client-123",
                    "audience": "example"
                  }
                }
              }
            }"#,
        );

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "mcp".to_owned(),
                raw_input: "/mcp".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("\"example\""));
        assert!(status.contains("\"oauth_device\""));
        assert!(status.contains("\"client_id\": \"client-123\""));
    }

    #[tokio::test]
    async fn repl_mcp_auth_login_starts_device_flow() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap();
                let header_text = String::from_utf8(request[..header_end].to_vec()).unwrap();
                let path = header_text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let response = match path.as_str() {
                    "/.well-known/oauth-authorization-server" => json!({
                        "device_authorization_endpoint": format!("http://{address}/device_authorization"),
                        "token_endpoint": format!("http://{address}/token")
                    }),
                    "/device_authorization" => json!({
                        "device_code": "device-123",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://verify.example.com",
                        "verification_uri_complete": "https://verify.example.com/complete",
                        "expires_in": 900,
                        "interval": 5
                    }),
                    other => panic!("unexpected auth path: {other}"),
                };
                let body = response.to_string();
                let response_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                std::io::Write::write_all(&mut stream, response_text.as_bytes()).unwrap();
            }
        });

        let root = temp_session_root("repl-mcp-auth");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();
        write_test_file(
            &root.join(".claude-plugin/plugin.json"),
            &format!(
                r#"{{
                  "name": "mcp-tools",
                  "mcpServers": {{
                    "example": {{
                      "url": "http://{address}/mcp",
                      "auth": {{
                        "type": "oauth_device",
                        "clientId": "client-123",
                        "audience": "example"
                      }}
                    }}
                  }}
                }}"#
            ),
        );

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "mcp".to_owned(),
                args: vec![
                    "auth-login".to_owned(),
                    root.display().to_string(),
                    "example".to_owned(),
                ],
                raw_input: format!("/mcp auth-login {} example", root.display()),
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("\"device_code\": \"device-123\""));
        assert!(status.contains("\"verification_uri\""));
    }

    #[tokio::test]
    async fn repl_remote_control_reports_local_state() {
        let root = temp_session_root("repl-remote");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let mut active_model = "gpt-5".to_owned();
        let mut raw_messages = Vec::new();
        let mut vim_state = code_agent_ui::vim::VimState::default();

        let status = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: "remote-control".to_owned(),
                raw_input: "/remote-control".to_owned(),
                ..CommandInvocation::default()
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::OpenAI,
            &mut active_model,
            session_id,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
        )
        .await
        .unwrap();

        assert!(status.contains("\"session_id\""));
        assert!(status.contains("\"task_count\""));
        assert!(status.contains("\"question_count\""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_control_directive_command_reaches_bridge() {
        let root = temp_session_root("remote-directive");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let address = temp_tcp_address();
        let server_root = root.clone();
        let server_address = address.clone();
        let server = tokio::spawn(async move {
            let server_store =
                ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
            let server_tool_registry = compatibility_tool_registry();
            let handler = LocalBridgeHandler {
                store: &server_store,
                tool_registry: &server_tool_registry,
                cwd: server_root,
                provider: ApiProvider::FirstParty,
                active_model: "claude-sonnet-4-6".to_owned(),
                session_id,
                raw_messages: Vec::new(),
                live_runtime: false,
                allow_remote_tools: true,
                pending_permission: None,
                voice_streams: BTreeMap::new(),
            };
            serve_direct_session(
                BridgeServerConfig {
                    bind_address: server_address,
                    session_id: Some(session_id),
                    allow_remote_tools: true,
                },
                handler,
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let output = render_remote_control_command(
            &registry,
            &CommandInvocation {
                name: "remote-control".to_owned(),
                args: vec![
                    "directive".to_owned(),
                    address.clone(),
                    "agent=coordinator".to_owned(),
                    "delegate the review".to_owned(),
                ],
                raw_input: format!(
                    "/remote-control directive {address} agent=coordinator delegate the review"
                ),
            },
            &Cli {
                bridge_receive_count: Some(24),
                ..Cli::default()
            },
            &store,
            &tool_registry,
            &root,
            ApiProvider::FirstParty,
            "claude-sonnet-4-6",
            session_id,
            &[],
            false,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert!(output.contains("assistant_synthesis") || output.contains("delegate the review"));
        assert!(record
            .envelopes
            .iter()
            .any(|envelope| matches!(envelope, RemoteEnvelope::AssistantDirective { .. })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_control_answer_command_round_trips_question_response() {
        let root = temp_session_root("remote-answer");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let registry = resolved_command_registry(&root, None).await;
        let session_id = SessionId::new_v4();
        let question_id = Uuid::new_v4();
        let address = temp_tcp_address();
        let server_root = root.clone();
        let server_address = address.clone();
        let server = tokio::spawn(async move {
            let server_store =
                ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
            let server_tool_registry = compatibility_tool_registry();
            let handler = LocalBridgeHandler {
                store: &server_store,
                tool_registry: &server_tool_registry,
                cwd: server_root,
                provider: ApiProvider::FirstParty,
                active_model: "claude-sonnet-4-6".to_owned(),
                session_id,
                raw_messages: Vec::new(),
                live_runtime: false,
                allow_remote_tools: true,
                pending_permission: None,
                voice_streams: BTreeMap::new(),
            };
            serve_direct_session(
                BridgeServerConfig {
                    bind_address: server_address,
                    session_id: Some(session_id),
                    allow_remote_tools: true,
                },
                handler,
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let output = render_remote_control_command(
            &registry,
            &CommandInvocation {
                name: "remote-control".to_owned(),
                args: vec![
                    "answer".to_owned(),
                    address.clone(),
                    question_id.to_string(),
                    "approved".to_owned(),
                ],
                raw_input: format!("/remote-control answer {address} {question_id} approved"),
            },
            &Cli {
                bridge_receive_count: Some(8),
                ..Cli::default()
            },
            &store,
            &tool_registry,
            &root,
            ApiProvider::FirstParty,
            "claude-sonnet-4-6",
            session_id,
            &[],
            false,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert!(output.contains(&question_id.to_string()));
        assert!(record.envelopes.iter().any(|envelope| {
            matches!(
                envelope,
                RemoteEnvelope::QuestionResponse { response }
                    if response.question_id == question_id
            )
        }));
    }

    #[tokio::test]
    async fn local_bridge_handler_runs_prompt_turns() {
        let store =
            ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("bridge-prompt")));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: env::temp_dir(),
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let envelopes = handler
            .on_envelope(&RemoteEnvelope::Message {
                message: build_text_message(
                    session_id,
                    MessageRole::User,
                    "bridge hello".to_owned(),
                    None,
                ),
            })
            .await
            .unwrap();

        assert!(envelopes.iter().any(|envelope| match envelope {
            RemoteEnvelope::Message { message } => message
                .blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("bridge hello"))),
            _ => false,
        }));
    }

    #[tokio::test]
    async fn local_bridge_handler_supports_assistant_and_voice_inputs() {
        let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
            "bridge-assistant",
        )));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: env::temp_dir(),
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let directive = handler
            .on_envelope(&RemoteEnvelope::AssistantDirective {
                directive: AssistantDirective {
                    agent_id: Some("coordinator".to_owned()),
                    instruction: "delegate the review".to_owned(),
                    ..AssistantDirective::default()
                },
            })
            .await
            .unwrap();
        let voice = handler
            .on_envelope(&RemoteEnvelope::VoiceFrame {
                frame: VoiceFrame {
                    format: "text/plain".to_owned(),
                    payload_base64: base64_encode(b"voice hello"),
                    sequence: 1,
                    stream_id: Some("voice".to_owned()),
                    is_final: true,
                },
            })
            .await
            .unwrap();

        let directive_messages = directive
            .iter()
            .filter(|envelope| matches!(envelope, RemoteEnvelope::Message { .. }))
            .count();
        assert!(directive_messages >= 2);
        assert!(voice
            .iter()
            .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
    }

    #[tokio::test]
    async fn local_bridge_handler_emits_tool_call_and_result_envelopes() {
        let root = temp_session_root("bridge-tool");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let envelopes = handler
            .on_envelope(&RemoteEnvelope::Message {
                message: build_text_message(
                    session_id,
                    MessageRole::User,
                    "tool:memory {\"action\":\"write\",\"value\":{\"note\":\"ok\"}}".to_owned(),
                    None,
                ),
            })
            .await
            .unwrap();

        assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::ToolCall { call } if call.name == "memory")));
        assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if result.tool_call_id == "echo_tool_call")));
    }

    #[tokio::test]
    async fn local_bridge_handler_buffers_streamed_voice_frames() {
        let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
            "bridge-voice-stream",
        )));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: env::temp_dir(),
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let partial = handler
            .on_envelope(&RemoteEnvelope::VoiceFrame {
                frame: VoiceFrame {
                    format: "text/plain".to_owned(),
                    payload_base64: base64_encode(b"voice "),
                    sequence: 1,
                    stream_id: Some("stream-a".to_owned()),
                    is_final: false,
                },
            })
            .await
            .unwrap();
        let final_chunk = handler
            .on_envelope(&RemoteEnvelope::VoiceFrame {
                frame: VoiceFrame {
                    format: "text/plain".to_owned(),
                    payload_base64: base64_encode(b"hello"),
                    sequence: 2,
                    stream_id: Some("stream-a".to_owned()),
                    is_final: true,
                },
            })
            .await
            .unwrap();

        assert!(partial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
        assert!(final_chunk
            .iter()
            .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
    }

    #[tokio::test]
    async fn local_bridge_handler_persists_binary_voice_frames() {
        let root = temp_session_root("bridge-voice-binary");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: root.clone(),
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let first = handler
            .on_envelope(&RemoteEnvelope::VoiceFrame {
                frame: VoiceFrame {
                    format: "audio/wav".to_owned(),
                    payload_base64: base64_encode(&[0, 255]),
                    sequence: 1,
                    stream_id: Some("binary-stream".to_owned()),
                    is_final: false,
                },
            })
            .await
            .unwrap();
        let second = handler
            .on_envelope(&RemoteEnvelope::VoiceFrame {
                frame: VoiceFrame {
                    format: "audio/wav".to_owned(),
                    payload_base64: base64_encode(&[12, 13]),
                    sequence: 2,
                    stream_id: Some("binary-stream".to_owned()),
                    is_final: true,
                },
            })
            .await
            .unwrap();

        let saved_path = second
            .iter()
            .find_map(|envelope| match envelope {
                RemoteEnvelope::Ack { note } => {
                    note.strip_prefix("voice_frame_saved:").map(PathBuf::from)
                }
                _ => None,
            })
            .expect("missing voice_frame_saved ack");

        assert!(first.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
        assert!(saved_path.exists());
        assert_eq!(fs::read(saved_path).unwrap(), vec![0, 255, 12, 13]);
    }

    #[tokio::test]
    async fn local_bridge_handler_requires_permission_for_remote_tool_calls() {
        let root = temp_session_root("bridge-remote-tool");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
        let tool_registry = compatibility_tool_registry();
        let session_id = SessionId::new_v4();
        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: false,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let initial = handler
            .on_envelope(&RemoteEnvelope::ToolCall {
                call: code_agent_core::ToolCall {
                    id: "remote-write".to_owned(),
                    name: "file_write".to_owned(),
                    input_json: json!({
                        "path": "remote.txt",
                        "content": "ok"
                    })
                    .to_string(),
                },
            })
            .await
            .unwrap();

        let request_id = initial
            .iter()
            .find_map(|envelope| match envelope {
                RemoteEnvelope::PermissionRequest { request } => Some(request.id.clone()),
                _ => None,
            })
            .expect("missing permission request");

        let approved = handler
            .on_envelope(&RemoteEnvelope::PermissionResponse {
                response: RemotePermissionResponse {
                    id: request_id,
                    approved: true,
                    note: None,
                },
            })
            .await
            .unwrap();

        assert!(initial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::PermissionRequest { request } if request.tool_name == "file_write")));
        assert!(approved.iter().any(
            |envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if !result.is_error)
        ));
    }

    #[tokio::test]
    async fn local_bridge_handler_resumes_existing_sessions() {
        let root = temp_session_root("bridge-resume");
        let store = ActiveSessionStore::Local(LocalSessionStore::new(root));
        let tool_registry = compatibility_tool_registry();
        let resumed_session = SessionId::new_v4();
        let persisted = build_text_message(
            resumed_session,
            MessageRole::Assistant,
            "resumed output".to_owned(),
            None,
        );
        store
            .append_message(resumed_session, &persisted)
            .await
            .unwrap();

        let mut handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: env::temp_dir(),
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id: SessionId::new_v4(),
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };

        let envelopes = handler
            .on_envelope(&RemoteEnvelope::ResumeSession {
                request: ResumeSessionRequest {
                    target: resumed_session.to_string(),
                },
            })
            .await
            .unwrap();

        assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Message { message } if message_text(message).contains("resumed output"))));
        assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::SessionState { state } if state.session_id == Some(resumed_session))));
    }
}
