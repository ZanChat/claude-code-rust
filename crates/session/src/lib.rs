use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use code_agent_core::{
    AgentId, BoundaryKind, BoundaryMarker, ContentBlock, Message, MessageRole, SessionId,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub const TRANSCRIPT_EXTENSION: &str = "jsonl";
pub const MAX_SANITIZED_LENGTH: usize = 200;
pub const LITE_READ_BUF_SIZE: usize = 65_536;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub transcript_path: PathBuf,
    pub modified_at_unix_ms: i64,
    pub message_count: usize,
    pub first_prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionConfig {
    pub kind: BoundaryKind,
    pub trigger: String,
    pub max_tokens_before: Option<u64>,
    pub target_tokens_after: u64,
    pub min_preserved_messages: usize,
    pub summary_line_limit: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            kind: BoundaryKind::Compact,
            trigger: "manual".to_owned(),
            max_tokens_before: None,
            target_tokens_after: 12_000,
            min_preserved_messages: 6,
            summary_line_limit: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompactionOutcome {
    pub summary_message: Message,
    pub boundary_message: Message,
    pub runtime_messages: Vec<Message>,
    pub estimated_tokens_before: u64,
    pub estimated_tokens_after: u64,
    pub summarized_message_count: usize,
    pub preserved_message_count: usize,
}

#[async_trait]
pub trait TranscriptCodec: Send + Sync {
    async fn read_messages(&self, path: &Path) -> Result<Vec<Message>>;
    async fn append_message(&self, path: &Path, message: &Message) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct JsonlTranscriptCodec;

#[async_trait]
impl TranscriptCodec for JsonlTranscriptCodec {
    async fn read_messages(&self, path: &Path) -> Result<Vec<Message>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read transcript {}", path.display()))?;

        let mut messages = Vec::new();
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let message = serde_json::from_str::<Message>(line).with_context(|| {
                format!("failed to decode transcript line in {}", path.display())
            })?;
            messages.push(message);
        }

        Ok(messages)
    }

    async fn append_message(&self, path: &Path, message: &Message) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create transcript dir {}", parent.display()))?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open transcript {}", path.display()))?;

        let serialized = serde_json::to_string(message)?;
        file.write_all(serialized.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }
}

pub fn claude_config_home_dir() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }

    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".claude"),
        None => PathBuf::from(".claude"),
    }
}

fn djb2_hash(input: &str) -> i32 {
    let mut hash = 0i32;
    for ch in input.chars() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(ch as i32);
    }
    hash
}

fn to_base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    if value == 0 {
        return "0".to_owned();
    }

    let mut reversed = Vec::new();
    while value > 0 {
        reversed.push(DIGITS[(value % 36) as usize] as char);
        value /= 36;
    }
    reversed.iter().rev().collect()
}

fn simple_hash(input: &str) -> String {
    let hash = i64::from(djb2_hash(input)).unsigned_abs();
    to_base36(hash)
}

pub fn sanitize_path(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();

    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return sanitized;
    }

    format!(
        "{}-{}",
        &sanitized[..MAX_SANITIZED_LENGTH],
        simple_hash(name)
    )
}

pub fn get_projects_dir() -> PathBuf {
    claude_config_home_dir().join("projects")
}

pub fn get_project_dir(project_dir: &Path) -> PathBuf {
    get_projects_dir().join(sanitize_path(&project_dir.to_string_lossy()))
}

pub fn transcript_path_for(project_dir: &Path, session_id: SessionId) -> PathBuf {
    get_project_dir(project_dir).join(format!("{session_id}.{TRANSCRIPT_EXTENSION}"))
}

pub fn session_id_from_transcript_path(path: &Path) -> Option<SessionId> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| Uuid::parse_str(value).ok())
}

pub fn agent_transcript_path_for(
    project_dir: &Path,
    session_id: SessionId,
    agent_id: AgentId,
    subdir: Option<&str>,
) -> PathBuf {
    let mut path = get_project_dir(project_dir).join(session_id.to_string());
    path.push("subagents");
    if let Some(subdir) = subdir.filter(|value| !value.trim().is_empty()) {
        path.push(subdir);
    }
    path.join(format!("agent-{agent_id}.{TRANSCRIPT_EXTENSION}"))
}

fn unescape_json_string(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_owned();
    }

    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_owned())
}

pub fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];

    for pattern in patterns {
        let Some(index) = text.find(&pattern) else {
            continue;
        };

        let value_start = index + pattern.len();
        let bytes = text.as_bytes();
        let mut cursor = value_start;
        while cursor < bytes.len() {
            if bytes[cursor] == b'\\' {
                cursor += 2;
                continue;
            }
            if bytes[cursor] == b'"' {
                return Some(unescape_json_string(&text[value_start..cursor]));
            }
            cursor += 1;
        }
    }

    None
}

pub fn extract_last_json_string_field(text: &str, key: &str) -> Option<String> {
    let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
    let mut last_value = None;

    for pattern in patterns {
        let mut search_from = 0usize;
        while let Some(index) = text[search_from..].find(&pattern) {
            let start = search_from + index + pattern.len();
            let bytes = text.as_bytes();
            let mut cursor = start;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == b'"' {
                    last_value = Some(unescape_json_string(&text[start..cursor]));
                    break;
                }
                cursor += 1;
            }
            search_from = cursor.saturating_add(1);
        }
    }

    last_value
}

fn extract_tag_content(input: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = input.find(&start_tag)? + start_tag.len();
    let end = input[start..].find(&end_tag)? + start;
    Some(input[start..end].trim().to_owned())
}

fn should_skip_first_prompt(value: &str) -> bool {
    if value.starts_with("[Request interrupted by user") {
        return true;
    }

    let mut chars = value.trim_start().chars();
    matches!((chars.next(), chars.next()), (Some('<'), Some(ch)) if ch.is_ascii_lowercase())
}

fn truncate_prompt(value: &str) -> String {
    let mut count = 0usize;
    let mut truncated = String::new();

    for ch in value.chars() {
        if count == 200 {
            truncated.push_str("...");
            return truncated.trim().to_owned();
        }
        truncated.push(ch);
        count += 1;
    }

    truncated.trim().to_owned()
}

fn message_text(message: &Message) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ToolCall { call } => {
                Some(format!("tool call {} {}", call.name, call.input_json))
            }
            ContentBlock::ToolResult { result } => Some(result.output_text.clone()),
            ContentBlock::Attachment { attachment } => {
                Some(format!("attachment {}", attachment.name))
            }
            ContentBlock::Boundary { boundary } => Some(match boundary.kind {
                BoundaryKind::Compact => "[compact boundary]".to_owned(),
                BoundaryKind::MicroCompact => "[micro-compact boundary]".to_owned(),
                BoundaryKind::SessionMemory => "[session-memory boundary]".to_owned(),
                BoundaryKind::Resume => "[resume boundary]".to_owned(),
            }),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_boundary_message(message: &Message) -> bool {
    message
        .blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Boundary { .. }))
}

fn is_compact_summary_message(message: &Message) -> bool {
    message
        .metadata
        .tags
        .iter()
        .any(|tag| tag == "compact_summary")
}

fn summarize_line(message: &Message) -> Option<String> {
    let prefix = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        MessageRole::Attachment => "attachment",
    };
    let text = message_text(message).replace('\n', " ");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut line = format!("{prefix}: {trimmed}");
    if line.chars().count() > 180 {
        line = line.chars().take(177).collect::<String>();
        line.push_str("...");
    }
    Some(line)
}

pub fn estimate_message_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|message| {
            let role_tokens = match message.role {
                MessageRole::System => 8,
                MessageRole::User => 6,
                MessageRole::Assistant => 6,
                MessageRole::Tool => 12,
                MessageRole::Attachment => 10,
            };
            let content_tokens = message_text(message).chars().count().div_ceil(4) as u64;
            role_tokens + content_tokens + (message.blocks.len() as u64 * 6)
        })
        .sum()
}

pub fn materialize_runtime_messages(messages: &[Message]) -> Vec<Message> {
    let mut latest_boundary = None;

    for message in messages.iter().rev() {
        if let Some(boundary) = message.blocks.iter().find_map(|block| match block {
            ContentBlock::Boundary { boundary } => Some(boundary.clone()),
            _ => None,
        }) {
            latest_boundary = Some(boundary);
            break;
        }
    }

    let Some(boundary) = latest_boundary else {
        return messages
            .iter()
            .filter(|message| !is_boundary_message(message))
            .cloned()
            .collect();
    };

    let Some(summary_message_id) = boundary.summary_message_id else {
        return messages
            .iter()
            .filter(|message| !is_boundary_message(message))
            .cloned()
            .collect();
    };
    let Some(summary_message) = messages
        .iter()
        .find(|message| message.id == summary_message_id)
    else {
        return messages
            .iter()
            .filter(|message| !is_boundary_message(message))
            .cloned()
            .collect();
    };

    let tail_start_index = boundary
        .preserved_tail_id
        .and_then(|tail_id| messages.iter().position(|message| message.id == tail_id))
        .unwrap_or(messages.len());

    let mut runtime_messages = vec![summary_message.clone()];
    runtime_messages.extend(
        messages
            .iter()
            .skip(tail_start_index)
            .filter(|message| message.id != summary_message_id)
            .filter(|message| !is_boundary_message(message))
            .filter(|message| !is_compact_summary_message(message))
            .cloned(),
    );
    runtime_messages
}

fn build_summary_message(
    session_id: SessionId,
    kind: BoundaryKind,
    trigger: &str,
    summarized: &[Message],
) -> Message {
    let lines = summarized
        .iter()
        .filter_map(summarize_line)
        .collect::<Vec<_>>();
    let header = match kind {
        BoundaryKind::Compact => "Conversation summary",
        BoundaryKind::MicroCompact => "Micro-compact summary",
        BoundaryKind::SessionMemory => "Session memory summary",
        BoundaryKind::Resume => "Resume summary",
    };
    let mut text = format!(
        "{header}\ntrigger: {trigger}\nsummarized_messages: {}\n",
        summarized.len()
    );
    if !lines.is_empty() {
        text.push('\n');
        for line in lines {
            text.push_str("- ");
            text.push_str(&line);
            text.push('\n');
        }
    }

    let mut message = Message::new(MessageRole::Assistant, vec![ContentBlock::Text { text }]);
    message.session_id = Some(session_id);
    message.metadata.tags.push("compact_summary".to_owned());
    message
        .metadata
        .attributes
        .insert("compaction_kind".to_owned(), format!("{kind:?}"));
    message
        .metadata
        .attributes
        .insert("compaction_trigger".to_owned(), trigger.to_owned());
    message
}

fn build_boundary_message(
    session_id: SessionId,
    kind: BoundaryKind,
    summary_message_id: Uuid,
    preserved_tail_id: Option<Uuid>,
) -> Message {
    let mut message = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Boundary {
            boundary: BoundaryMarker {
                kind,
                summary_message_id: Some(summary_message_id),
                preserved_tail_id,
            },
        }],
    );
    message.session_id = Some(session_id);
    message.metadata.tags.push("compact_boundary".to_owned());
    message
}

pub fn compact_messages(
    messages: &[Message],
    config: &CompactionConfig,
) -> Option<CompactionOutcome> {
    let runtime_messages = materialize_runtime_messages(messages);
    if runtime_messages.len() <= config.min_preserved_messages {
        return None;
    }

    let estimated_tokens_before = estimate_message_tokens(&runtime_messages);
    if estimated_tokens_before <= config.target_tokens_after {
        return None;
    }

    let summary_budget = config.target_tokens_after / 3;
    let tail_budget = config
        .target_tokens_after
        .saturating_sub(summary_budget)
        .max(1);
    let mut preserved_tokens = 0u64;
    let mut split_index = runtime_messages.len();
    let mut preserved = 0usize;

    for (index, message) in runtime_messages.iter().enumerate().rev() {
        let message_tokens = estimate_message_tokens(std::slice::from_ref(message));
        let must_keep = preserved < config.min_preserved_messages;
        if !must_keep && preserved_tokens + message_tokens > tail_budget {
            split_index = index + 1;
            break;
        }
        preserved_tokens += message_tokens;
        preserved += 1;
        split_index = index;
    }

    if split_index == 0 || split_index >= runtime_messages.len() {
        return None;
    }

    let summarized = runtime_messages[..split_index]
        .iter()
        .filter(|message| !is_boundary_message(message))
        .cloned()
        .collect::<Vec<_>>();
    let preserved_tail = runtime_messages[split_index..]
        .iter()
        .filter(|message| !is_boundary_message(message))
        .cloned()
        .collect::<Vec<_>>();
    if summarized.is_empty() || preserved_tail.is_empty() {
        return None;
    }

    let session_id = preserved_tail
        .first()
        .and_then(|message| message.session_id)
        .or_else(|| summarized.first().and_then(|message| message.session_id))
        .unwrap_or_else(Uuid::new_v4);

    let summary_source = summarized
        .into_iter()
        .take(config.summary_line_limit)
        .collect::<Vec<_>>();
    let summary_message = build_summary_message(
        session_id,
        config.kind.clone(),
        &config.trigger,
        &summary_source,
    );
    let boundary_message = build_boundary_message(
        session_id,
        config.kind.clone(),
        summary_message.id,
        preserved_tail.first().map(|message| message.id),
    );
    let mut materialized = vec![summary_message.clone()];
    materialized.extend(preserved_tail.clone());
    let estimated_tokens_after = estimate_message_tokens(&materialized);
    if estimated_tokens_after >= estimated_tokens_before {
        return None;
    }

    Some(CompactionOutcome {
        summary_message,
        boundary_message,
        estimated_tokens_before,
        estimated_tokens_after,
        summarized_message_count: split_index,
        preserved_message_count: preserved_tail.len(),
        runtime_messages: materialized,
    })
}

pub fn extract_first_prompt_from_head(head: &str) -> String {
    let mut command_fallback = String::new();

    for line in head.lines() {
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if line.contains("\"tool_result\"") {
            continue;
        }
        if line.contains("\"isMeta\":true") || line.contains("\"isMeta\": true") {
            continue;
        }
        if line.contains("\"isCompactSummary\":true") || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(|value| value.as_str()) != Some("user") {
            continue;
        }

        let Some(message) = entry.get("message") else {
            continue;
        };

        let mut texts = Vec::new();
        match message.get("content") {
            Some(serde_json::Value::String(text)) => texts.push(text.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                        if let Some(text) = block.get("text").and_then(|value| value.as_str()) {
                            texts.push(text.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }

        for raw in texts {
            let normalized = raw.replace('\n', " ").trim().to_owned();
            if normalized.is_empty() {
                continue;
            }

            if let Some(command_name) = extract_tag_content(&normalized, "command-name") {
                if command_fallback.is_empty() {
                    command_fallback = command_name;
                }
                continue;
            }

            if let Some(bash_input) = extract_tag_content(&normalized, "bash-input") {
                return format!("! {bash_input}");
            }

            if should_skip_first_prompt(&normalized) {
                continue;
            }

            return truncate_prompt(&normalized);
        }
    }

    command_fallback
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedTranscript {
    pub session_id: SessionId,
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub message_count: usize,
}

pub async fn import_transcript_to_session_root(
    codec: &impl TranscriptCodec,
    source_path: &Path,
    destination_root: &Path,
) -> Result<ImportedTranscript> {
    let session_id = session_id_from_transcript_path(source_path).ok_or_else(|| {
        anyhow!(
            "transcript path does not contain a valid session id: {}",
            source_path.display()
        )
    })?;
    let destination_path = destination_root.join(format!("{session_id}.{TRANSCRIPT_EXTENSION}"));
    if let Some(parent) = destination_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create transcript dir {}", parent.display()))?;
    }
    let messages = codec.read_messages(source_path).await?;
    fs::copy(source_path, &destination_path).with_context(|| {
        format!(
            "failed to copy transcript {} to {}",
            source_path.display(),
            destination_path.display()
        )
    })?;
    Ok(ImportedTranscript {
        session_id,
        source_path: source_path.to_path_buf(),
        destination_path,
        message_count: messages.len(),
    })
}

pub fn summarize_transcript_path(path: &Path) -> Result<Option<SessionSummary>> {
    if !path.exists() {
        return Ok(None);
    }

    let session_id = session_id_from_transcript_path(path).ok_or_else(|| {
        anyhow!(
            "transcript path does not contain a valid session id: {}",
            path.display()
        )
    })?;
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read transcript {}", path.display()))?;
    let head = content.chars().take(LITE_READ_BUF_SIZE).collect::<String>();
    let modified_at_unix_ms = fs::metadata(path)
        .with_context(|| format!("failed to stat transcript {}", path.display()))?
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();

    let first_prompt = {
        let extracted = extract_first_prompt_from_head(&head);
        if !extracted.is_empty() {
            extracted
        } else {
            content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .find_map(|line| serde_json::from_str::<Message>(line).ok())
                .and_then(|message| {
                    (message.role == code_agent_core::MessageRole::User).then_some(message)
                })
                .and_then(|message| {
                    message.blocks.into_iter().find_map(|block| match block {
                        code_agent_core::ContentBlock::Text { text } => Some(text),
                        _ => None,
                    })
                })
                .unwrap_or_default()
        }
    };

    Ok(Some(SessionSummary {
        session_id,
        transcript_path: path.to_path_buf(),
        modified_at_unix_ms,
        message_count: content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        first_prompt,
    }))
}

pub fn list_sessions_in_dir(dir: &Path) -> Result<Vec<SessionSummary>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some(TRANSCRIPT_EXTENSION)
        {
            if let Some(summary) = summarize_transcript_path(&path)? {
                sessions.push(summary);
            }
        }
    }
    sessions.sort_by(|left, right| {
        right
            .modified_at_unix_ms
            .cmp(&left.modified_at_unix_ms)
            .then(left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    fn root_dir(&self) -> &Path;
    async fn transcript_path(&self, session_id: SessionId) -> Result<PathBuf>;
    async fn load_session(&self, session_id: SessionId) -> Result<Vec<Message>>;
    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct LocalSessionStore {
    root_dir: PathBuf,
    codec: JsonlTranscriptCodec,
}

impl LocalSessionStore {
    pub fn new(root_dir: PathBuf) -> Self {
        Self {
            root_dir,
            codec: JsonlTranscriptCodec,
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn transcript_path_for_session(&self, session_id: SessionId) -> PathBuf {
        self.root_dir
            .join(format!("{session_id}.{TRANSCRIPT_EXTENSION}"))
    }

    pub fn resolve_resume_target(&self, value: &str) -> Result<PathBuf> {
        if value.ends_with(".jsonl") {
            return Ok(PathBuf::from(value));
        }

        let session_id = Uuid::parse_str(value)
            .map_err(|error| anyhow!("invalid session id '{value}': {error}"))?;
        Ok(self.transcript_path_for_session(session_id))
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        list_sessions_in_dir(&self.root_dir)
    }

    pub async fn load_resume_target(
        &self,
        value: &str,
    ) -> Result<(SessionId, PathBuf, Vec<Message>)> {
        let path = self.resolve_resume_target(value)?;
        let session_id = session_id_from_transcript_path(&path).ok_or_else(|| {
            anyhow!(
                "resume target does not resolve to a session transcript path: {}",
                path.display()
            )
        })?;
        let messages = self.codec.read_messages(&path).await?;
        Ok((session_id, path, messages))
    }
}

#[async_trait]
impl SessionStore for LocalSessionStore {
    fn root_dir(&self) -> &Path {
        self.root_dir()
    }

    async fn transcript_path(&self, session_id: SessionId) -> Result<PathBuf> {
        Ok(self.transcript_path_for_session(session_id))
    }

    async fn load_session(&self, session_id: SessionId) -> Result<Vec<Message>> {
        let path = self.transcript_path_for_session(session_id);
        self.codec.read_messages(&path).await
    }

    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()> {
        let path = self.transcript_path_for_session(session_id);
        self.codec.append_message(&path, message).await
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProjectSessionStore {
    project_root: PathBuf,
    storage_dir: PathBuf,
    codec: JsonlTranscriptCodec,
}

impl ProjectSessionStore {
    pub fn new(project_root: PathBuf) -> Self {
        let storage_dir = get_project_dir(&project_root);
        Self {
            project_root,
            storage_dir,
            codec: JsonlTranscriptCodec,
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    pub fn transcript_path_for_session(&self, session_id: SessionId) -> PathBuf {
        self.storage_dir
            .join(format!("{session_id}.{TRANSCRIPT_EXTENSION}"))
    }

    pub fn resolve_resume_target(&self, value: &str) -> Result<PathBuf> {
        if value.ends_with(".jsonl") {
            return Ok(PathBuf::from(value));
        }

        let session_id = Uuid::parse_str(value)
            .map_err(|error| anyhow!("invalid session id '{value}': {error}"))?;
        Ok(self.transcript_path_for_session(session_id))
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        list_sessions_in_dir(&self.storage_dir)
    }

    pub async fn load_resume_target(
        &self,
        value: &str,
    ) -> Result<(SessionId, PathBuf, Vec<Message>)> {
        let path = self.resolve_resume_target(value)?;
        let session_id = session_id_from_transcript_path(&path).ok_or_else(|| {
            anyhow!(
                "resume target does not resolve to a session transcript path: {}",
                path.display()
            )
        })?;
        let messages = self.codec.read_messages(&path).await?;
        Ok((session_id, path, messages))
    }
}

#[async_trait]
impl SessionStore for ProjectSessionStore {
    fn root_dir(&self) -> &Path {
        self.storage_dir()
    }

    async fn transcript_path(&self, session_id: SessionId) -> Result<PathBuf> {
        Ok(self.transcript_path_for_session(session_id))
    }

    async fn load_session(&self, session_id: SessionId) -> Result<Vec<Message>> {
        let path = self.transcript_path_for_session(session_id);
        self.codec.read_messages(&path).await
    }

    async fn append_message(&self, session_id: SessionId, message: &Message) -> Result<()> {
        let path = self.transcript_path_for_session(session_id);
        self.codec.append_message(&path, message).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        agent_transcript_path_for, compact_messages, estimate_message_tokens,
        extract_first_prompt_from_head, extract_json_string_field, extract_last_json_string_field,
        get_project_dir, list_sessions_in_dir, materialize_runtime_messages, sanitize_path,
        summarize_transcript_path, transcript_path_for, CompactionConfig, JsonlTranscriptCodec,
        LocalSessionStore, TranscriptCodec,
    };
    use code_agent_core::{BoundaryKind, ContentBlock, Message, MessageRole};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

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

    #[test]
    fn sanitizes_and_hashes_long_paths() {
        let input = format!("/tmp/{}", "very-long-segment/".repeat(30));
        let sanitized = sanitize_path(&input);

        assert!(sanitized.len() > 200);
        assert!(sanitized.starts_with("-tmp-very-long-segment-"));
        assert!(sanitized
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
    }

    #[test]
    fn resolves_project_and_agent_transcript_paths() {
        with_env_var("CLAUDE_CONFIG_DIR", Some("/tmp/claude-home"), || {
            let project = Path::new("/Users/example/worktree/project");
            let session_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
            let agent_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

            assert_eq!(
                get_project_dir(project),
                PathBuf::from("/tmp/claude-home/projects/-Users-example-worktree-project")
            );
            assert_eq!(
                transcript_path_for(project, session_id),
                PathBuf::from(
                    "/tmp/claude-home/projects/-Users-example-worktree-project/11111111-1111-4111-8111-111111111111.jsonl"
                )
            );
            assert_eq!(
                agent_transcript_path_for(project, session_id, agent_id, Some("workflows/run-1")),
                PathBuf::from(
                    "/tmp/claude-home/projects/-Users-example-worktree-project/11111111-1111-4111-8111-111111111111/subagents/workflows/run-1/agent-22222222-2222-4222-8222-222222222222.jsonl"
                )
            );
        });
    }

    #[test]
    fn extracts_json_string_fields_without_full_parse() {
        let text = r#"{"title":"first","title":"second","escaped":"say \"hello\""}"#;

        assert_eq!(
            extract_json_string_field(text, "escaped"),
            Some("say \"hello\"".to_owned())
        );
        assert_eq!(
            extract_last_json_string_field(text, "title"),
            Some("second".to_owned())
        );
    }

    #[test]
    fn extracts_first_prompt_and_skips_metadata() {
        let head = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>compact</command-name>\"}}\n",
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"ignored\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<bash-input>ls -la</bash-input>\"}}\n"
        );

        assert_eq!(extract_first_prompt_from_head(head), "! ls -la");
    }

    #[test]
    fn falls_back_to_command_name_when_no_prompt_survives() {
        let head = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>resume</command-name>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<tool-output>ignored</tool-output>\"}}\n"
        );

        assert_eq!(extract_first_prompt_from_head(head), "resume");
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("code-agent-session-{label}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn summarizes_and_lists_transcripts() {
        let dir = make_temp_dir("summary");
        let session_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut message = Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "Summarize this session".to_owned(),
            }],
        );
        message.session_id = Some(session_id);

        JsonlTranscriptCodec
            .append_message(&path, &message)
            .await
            .unwrap();

        let summary = summarize_transcript_path(&path).unwrap().unwrap();
        assert_eq!(summary.session_id, session_id);
        assert_eq!(summary.message_count, 1);

        let sessions = list_sessions_in_dir(&dir).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_prompt, "Summarize this session");
    }

    #[test]
    fn estimates_tokens_and_materializes_latest_compaction() {
        let session_id = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();
        let mut messages = Vec::new();
        for index in 0..8 {
            let mut message = Message::new(
                if index % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                vec![ContentBlock::Text {
                    text: format!(
                        "Message {index} contains enough repeated detail to make compaction worthwhile."
                    ),
                }],
            );
            message.session_id = Some(session_id);
            messages.push(message);
        }
        let outcome = compact_messages(
            &messages,
            &CompactionConfig {
                target_tokens_after: estimate_message_tokens(&messages) / 2,
                min_preserved_messages: 1,
                ..CompactionConfig::default()
            },
        )
        .unwrap();

        let mut transcript = messages.clone();
        transcript.push(outcome.summary_message.clone());
        transcript.push(outcome.boundary_message.clone());
        let runtime_messages = materialize_runtime_messages(&transcript);

        assert!(estimate_message_tokens(&runtime_messages) > 0);
        assert_eq!(
            runtime_messages.first().unwrap().id,
            outcome.summary_message.id
        );
        assert_eq!(
            runtime_messages.last().unwrap().id,
            messages.last().unwrap().id
        );
    }

    #[test]
    fn skips_compaction_when_summary_would_not_help() {
        let session_id = Uuid::parse_str("66666666-6666-4666-8666-666666666666").unwrap();
        let mut first = Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "short prompt".to_owned(),
            }],
        );
        first.session_id = Some(session_id);
        let mut second = Message::new(
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: "short reply".to_owned(),
            }],
        );
        second.session_id = Some(session_id);

        let outcome = compact_messages(
            &[first, second],
            &CompactionConfig {
                target_tokens_after: 1,
                min_preserved_messages: 1,
                ..CompactionConfig::default()
            },
        );

        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn imports_fixture_transcript_into_session_root() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let fixture = workspace
            .join("fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl");
        let root = make_temp_dir("fixture-import");
        let imported = super::import_transcript_to_session_root(&JsonlTranscriptCodec, &fixture, &root)
            .await
            .unwrap();

        assert_eq!(imported.session_id.to_string(), "77777777-7777-4777-8777-777777777777");
        assert!(imported.destination_path.exists());
        assert_eq!(imported.message_count, 6);
    }

    #[tokio::test]
    async fn loads_fixture_transcript_and_resumes_by_jsonl_path() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let fixture = workspace
            .join("fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl");
        let messages = JsonlTranscriptCodec.read_messages(&fixture).await.unwrap();
        let runtime = materialize_runtime_messages(&messages);
        let root = make_temp_dir("fixture-resume");
        let store = LocalSessionStore::new(root);
        let (session_id, path, resumed) = store
            .load_resume_target(fixture.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(path, fixture);
        assert_eq!(session_id.to_string(), "77777777-7777-4777-8777-777777777777");
        assert_eq!(runtime.len(), 3);
        assert!(runtime[0]
            .metadata
            .tags
            .contains(&"compact_summary".to_owned()));
        assert_eq!(resumed.len(), messages.len());
    }

    #[test]
    fn compaction_reduces_runtime_size() {
        let session_id = Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap();
        let mut messages = Vec::new();
        for index in 0..10 {
            let mut user = Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: format!(
                        "User message {index} with enough text to count toward the estimate."
                    ),
                }],
            );
            user.session_id = Some(session_id);
            messages.push(user);

            let mut assistant = Message::new(
                MessageRole::Assistant,
                vec![ContentBlock::Text {
                    text: format!("Assistant reply {index} with a matching amount of detail."),
                }],
            );
            assistant.session_id = Some(session_id);
            messages.push(assistant);
        }

        let before = estimate_message_tokens(&messages);
        let outcome = compact_messages(
            &messages,
            &CompactionConfig {
                kind: BoundaryKind::SessionMemory,
                trigger: "auto".to_owned(),
                target_tokens_after: before / 3,
                min_preserved_messages: 4,
                summary_line_limit: 6,
                max_tokens_before: Some(before),
            },
        )
        .unwrap();

        assert_eq!(outcome.boundary_message.blocks.len(), 1);
        assert!(outcome
            .summary_message
            .metadata
            .tags
            .contains(&"compact_summary".to_owned()));
        assert!(outcome.estimated_tokens_after < before);
        assert_eq!(
            outcome.runtime_messages.first().unwrap().id,
            outcome.summary_message.id
        );
    }
}
