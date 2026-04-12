use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ccrust_core::{
    AgentId, BoundaryKind, BoundaryMarker, ContentBlock, Message, MessageMetadata, MessageRole,
    SessionId, TokenUsage, ToolCall, ToolResult,
};
use std::collections::BTreeMap;
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

#[derive(Clone, Debug, Default)]
struct MessageMetadataPatch {
    tags: Vec<String>,
    attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
enum DecodedTranscriptLine {
    Message(Message),
    MetadataPatch {
        target_id: Uuid,
        patch: MessageMetadataPatch,
    },
}

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
        let mut pending_patches = BTreeMap::<Uuid, MessageMetadataPatch>::new();
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let value = serde_json::from_str::<serde_json::Value>(line).with_context(|| {
                format!("failed to decode transcript line in {}", path.display())
            })?;
            match decode_transcript_line(&value) {
                Some(DecodedTranscriptLine::Message(mut message)) => {
                    if let Some(patch) = pending_patches.remove(&message.id) {
                        apply_metadata_patch(&mut message, patch);
                    }
                    messages.push(message);
                }
                Some(DecodedTranscriptLine::MetadataPatch { target_id, patch }) => {
                    if let Some(message) =
                        messages.iter_mut().find(|message| message.id == target_id)
                    {
                        apply_metadata_patch(message, patch);
                    } else {
                        pending_patches
                            .entry(target_id)
                            .and_modify(|existing| merge_metadata_patch(existing, &patch))
                            .or_insert(patch);
                    }
                }
                None => {}
            }
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

fn merge_metadata_patch(target: &mut MessageMetadataPatch, patch: &MessageMetadataPatch) {
    for tag in &patch.tags {
        if !target.tags.contains(tag) {
            target.tags.push(tag.clone());
        }
    }
    for (key, value) in &patch.attributes {
        target.attributes.insert(key.clone(), value.clone());
    }
}

fn apply_metadata_patch(message: &mut Message, patch: MessageMetadataPatch) {
    for tag in patch.tags {
        if !message.metadata.tags.contains(&tag) {
            message.metadata.tags.push(tag);
        }
    }
    for (key, value) in patch.attributes {
        message.metadata.attributes.insert(key, value);
    }
}

fn decode_transcript_line(value: &serde_json::Value) -> Option<DecodedTranscriptLine> {
    serde_json::from_value::<Message>(value.clone())
        .ok()
        .map(DecodedTranscriptLine::Message)
        .or_else(|| decode_legacy_transcript_line(value))
}

fn decode_legacy_transcript_line(value: &serde_json::Value) -> Option<DecodedTranscriptLine> {
    let entry_type = value.get("type")?.as_str()?;
    let legacy_message = value.get("message")?;
    let base_role = parse_legacy_message_role(
        legacy_message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(entry_type),
    )?;

    if value
        .get("isMeta")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && base_role == MessageRole::User
    {
        let expanded_prompt = legacy_text_content(legacy_message.get("content")?)
            .trim()
            .to_owned();
        if expanded_prompt.is_empty() {
            return None;
        }
        let target_id = parse_legacy_uuid(value.get("parentUuid")?)?;
        let mut patch = MessageMetadataPatch::default();
        patch.tags.push(ccrust_core::PROMPT_COMMAND_TAG.to_owned());
        patch.attributes.insert(
            ccrust_core::EXPANDED_PROMPT_ATTRIBUTE.to_owned(),
            expanded_prompt,
        );
        return Some(DecodedTranscriptLine::MetadataPatch { target_id, patch });
    }

    let blocks = legacy_content_blocks(legacy_message.get("content")?)?;
    if blocks.is_empty() {
        return None;
    }

    let mut message = Message::new(legacy_role_for_blocks(base_role.clone(), &blocks), blocks);
    message.id = parse_legacy_uuid(value.get("uuid")?)?;
    message.parent_id = value.get("parentUuid").and_then(parse_legacy_uuid);
    message.session_id = value.get("sessionId").and_then(parse_legacy_session_id);
    message.created_at_unix_ms = 0;
    message.metadata = legacy_message_metadata(value, legacy_message, &message.blocks);
    Some(DecodedTranscriptLine::Message(message))
}

fn parse_legacy_message_role(value: &str) -> Option<MessageRole> {
    match value.trim().to_ascii_lowercase().as_str() {
        "system" => Some(MessageRole::System),
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        "tool" => Some(MessageRole::Tool),
        "attachment" => Some(MessageRole::Attachment),
        _ => None,
    }
}

fn parse_legacy_uuid(value: &serde_json::Value) -> Option<Uuid> {
    value.as_str().and_then(|value| Uuid::parse_str(value).ok())
}

fn parse_legacy_session_id(value: &serde_json::Value) -> Option<SessionId> {
    parse_legacy_uuid(value)
}

fn legacy_content_blocks(content: &serde_json::Value) -> Option<Vec<ContentBlock>> {
    match content {
        serde_json::Value::String(text) => Some(vec![ContentBlock::Text { text: text.clone() }]),
        serde_json::Value::Array(blocks) => {
            let blocks = blocks
                .iter()
                .filter_map(legacy_content_block)
                .collect::<Vec<_>>();
            (!blocks.is_empty()).then_some(blocks)
        }
        _ => None,
    }
}

fn legacy_content_block(block: &serde_json::Value) -> Option<ContentBlock> {
    let block_type = block.get("type")?.as_str()?;
    match block_type {
        "text" => block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(|text| ContentBlock::Text {
                text: text.to_owned(),
            }),
        "tool_use" => {
            let id = block.get("id")?.as_str()?;
            let name = block.get("name")?.as_str()?;
            let input_json = serde_json::to_string(
                block
                    .get("input")
                    .unwrap_or(&serde_json::Value::Object(Default::default())),
            )
            .ok()?;
            Some(ContentBlock::ToolCall {
                call: ToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    input_json,
                    thought_signature: block
                        .get("thought_signature")
                        .or_else(|| block.get("thoughtSignature"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                },
            })
        }
        "tool_result" => {
            let tool_call_id = block
                .get("tool_use_id")
                .or_else(|| block.get("toolUseId"))
                .and_then(serde_json::Value::as_str)?;
            Some(ContentBlock::ToolResult {
                result: ToolResult {
                    tool_call_id: tool_call_id.to_owned(),
                    output_text: legacy_text_content(block.get("content")?),
                    is_error: block
                        .get("is_error")
                        .or_else(|| block.get("isError"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
            })
        }
        _ => block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(|text| ContentBlock::Text {
                text: text.to_owned(),
            }),
    }
}

fn legacy_text_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(serde_json::Value::as_str))
                    .flatten()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn legacy_role_for_blocks(role: MessageRole, blocks: &[ContentBlock]) -> MessageRole {
    if matches!(role, MessageRole::User)
        && blocks
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
    {
        MessageRole::Tool
    } else {
        role
    }
}

fn legacy_message_metadata(
    entry: &serde_json::Value,
    legacy_message: &serde_json::Value,
    blocks: &[ContentBlock],
) -> MessageMetadata {
    let mut metadata = MessageMetadata {
        provider: entry
            .get("apiProvider")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        model: legacy_message
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        usage: legacy_token_usage(legacy_message.get("usage")),
        tags: Vec::new(),
        attributes: BTreeMap::new(),
    };

    if let Some(raw_input) = legacy_prompt_command_raw_input(blocks) {
        metadata
            .tags
            .push(ccrust_core::PROMPT_COMMAND_TAG.to_owned());
        metadata.attributes.insert(
            ccrust_core::PROMPT_COMMAND_RAW_INPUT_ATTRIBUTE.to_owned(),
            raw_input,
        );
    }

    metadata
}

fn legacy_token_usage(value: Option<&serde_json::Value>) -> Option<TokenUsage> {
    let value = value?;
    Some(TokenUsage {
        input_tokens: value
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: value
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    })
}

fn legacy_prompt_command_raw_input(blocks: &[ContentBlock]) -> Option<String> {
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    prompt_command_raw_input_from_text(&text)
}

fn decoded_message_from_json_line(line: &str) -> Option<Message> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    match decode_transcript_line(&value)? {
        DecodedTranscriptLine::Message(message) => Some(message),
        DecodedTranscriptLine::MetadataPatch { .. } => None,
    }
}

fn decoded_message_count(content: &str) -> usize {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(decoded_message_from_json_line)
        .count()
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

fn estimated_message_text(message: &Message) -> String {
    if message.role == MessageRole::User {
        if let Some(expanded_prompt) = message
            .metadata
            .attributes
            .get(ccrust_core::EXPANDED_PROMPT_ATTRIBUTE)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return expanded_prompt.to_owned();
        }
    }

    message_text(message)
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

fn prompt_command_raw_input_from_text(text: &str) -> Option<String> {
    let command_name = extract_tag_content(text, "command-name")?;
    let args = extract_tag_content(text, "command-args").unwrap_or_default();

    if args.is_empty() {
        Some(command_name)
    } else {
        Some(format!("{command_name} {args}"))
    }
}

fn should_skip_first_prompt(value: &str) -> bool {
    if value.starts_with("[Request interrupted by user") {
        return true;
    }

    let mut chars = value.trim_start().chars();
    matches!((chars.next(), chars.next()), (Some('<'), Some(ch)) if ch.is_ascii_lowercase())
}

fn truncate_prompt(value: &str) -> String {
    let mut truncated = String::new();

    for (count, ch) in value.chars().enumerate() {
        if count == 200 {
            truncated.push_str("...");
            return truncated.trim().to_owned();
        }
        truncated.push(ch);
    }

    truncated.trim().to_owned()
}

fn message_text(message: &Message) -> String {
    message
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::ToolCall { call } => {
                format!("tool call {} {}", call.name, call.input_json)
            }
            ContentBlock::ToolResult { result } => result.output_text.clone(),
            ContentBlock::Attachment { attachment } => {
                format!("attachment {}", attachment.name)
            }
            ContentBlock::Boundary { boundary } => match boundary.kind {
                BoundaryKind::Compact => "[compact boundary]".to_owned(),
                BoundaryKind::MicroCompact => "[micro-compact boundary]".to_owned(),
                BoundaryKind::SessionMemory => "[session-memory boundary]".to_owned(),
                BoundaryKind::Resume => "[resume boundary]".to_owned(),
            },
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
    if message.role == MessageRole::Attachment {
        return None;
    }

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
        .filter(|message| message.role != MessageRole::Attachment)
        .map(|message| {
            let role_tokens = match message.role {
                MessageRole::System => 8,
                MessageRole::User => 6,
                MessageRole::Assistant => 6,
                MessageRole::Tool => 12,
                MessageRole::Attachment => 0,
            };
            let content_tokens = estimated_message_text(message).chars().count().div_ceil(4) as u64;
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
    for (preserved, (index, message)) in runtime_messages.iter().enumerate().rev().enumerate() {
        let message_tokens = estimate_message_tokens(std::slice::from_ref(message));
        let must_keep = preserved < config.min_preserved_messages;
        if !must_keep && preserved_tokens + message_tokens > tail_budget {
            split_index = index + 1;
            break;
        }
        preserved_tokens += message_tokens;
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

        if let Some(raw_input) = message
            .get("metadata")
            .and_then(|metadata| metadata.get("attributes"))
            .and_then(|attributes| attributes.get(ccrust_core::PROMPT_COMMAND_RAW_INPUT_ATTRIBUTE))
            .and_then(serde_json::Value::as_str)
        {
            let normalized = raw_input.trim();
            if !normalized.is_empty() {
                return truncate_prompt(normalized);
            }
        }

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

            if let Some(command_name) = prompt_command_raw_input_from_text(&normalized) {
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
                .find_map(decoded_message_from_json_line)
                .and_then(|message| {
                    (message.role == ccrust_core::MessageRole::User).then_some(message)
                })
                .and_then(|message| {
                    message.blocks.into_iter().find_map(|block| match block {
                        ccrust_core::ContentBlock::Text { text } => Some(text),
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
        message_count: decoded_message_count(&content),
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
mod tests;
