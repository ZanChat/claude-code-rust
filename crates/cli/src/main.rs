mod helpers;
use helpers::*;
mod commands;
use commands::*;
mod spinner_verbs;
use spinner_verbs::sample_spinner_verb;
mod startup;
use startup::*;
mod session;
use session::*;
mod cli_args;
use cli_args::*;
mod reports;
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
    update_task_record, AppEvent, BoundaryKind, CommandInvocation, CommandRegistry, CommandSource,
    CommandSpec, ContentBlock, LocalTaskStore as CoreLocalTaskStore, Message, MessageRole,
    QuestionRequest, QuestionResponse, SessionId, TaskRecord, TaskStatus, TaskStore,
};
use code_agent_mcp::parse_mcp_server_configs;
use code_agent_plugins::{
    BridgeLaunchRequest, CommandDefinitions, OutOfProcessPluginRuntime, PluginRuntime,
};
use code_agent_providers::{
    build_provider, clear_auth_snapshot, code_agent_auth_snapshot_path,
    compatibility_model_catalog, config_migration_report, get_anthropic_credential_hint,
    get_openai_credential_hint, resolve_api_provider, write_auth_snapshot, ApiProvider,
    AuthRequest, AuthResolver, EnvironmentAuthResolver, ModelCatalog, ProviderEvent,
    ProviderRequest, ProviderToolDefinition,
};
use code_agent_session::{
    agent_transcript_path_for, compact_messages, estimate_message_tokens,
    extract_last_json_string_field, get_project_dir, import_transcript_to_session_root,
    materialize_runtime_messages, CompactionConfig, CompactionOutcome, JsonlTranscriptCodec,
    SessionSummary, TranscriptCodec,
};
use code_agent_tools::{compatibility_tool_registry, ToolCallRequest, ToolContext, ToolRegistry};
use code_agent_ui::{
    draw_terminal as draw_tui, mouse_action_for_position, render_to_string as render_tui_to_string,
    transcript_line_from_message, transcript_search_match_items, transcript_search_scroll_for_view,
    transcript_selectable_lines_for_view, transcript_selection_text_for_view,
    transcript_visual_scroll_for_view, ChoiceListItem, ChoiceListState, CommandPaletteEntry,
    Notification, PaneKind, PanePreview, PermissionPromptState, PromptHistorySearchState,
    PromptSelectionState, QuestionUiEntry, RatatuiApp, StatusLevel, TaskUiEntry, TranscriptGroup,
    TranscriptMessageActionsState, TranscriptSearchState, TranscriptSelectableLine,
    TranscriptSelectionPoint, TranscriptSelectionState, UiMouseAction, UiState,
};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size as terminal_size, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use reports::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::future::Future;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

#[cfg(test)]
use code_agent_providers::EchoProvider;

const UI_EVENT_TAG: &str = "ui_event";
const UI_ROLE_ATTRIBUTE: &str = "ui_role";
const UI_AUTHOR_ATTRIBUTE: &str = "ui_author";
const REQUEST_INTERRUPTED_MESSAGE: &str = "[Request interrupted by user]";
const RECENT_COMPLETED_TTL_MS: i64 = 30_000;

fn should_exit_repl(prompt_text: &str) -> bool {
    matches!(prompt_text.trim(), "quit" | "exit" | "/quit" | "/exit")
}

fn status_line_needs_marquee(status_line: &str) -> bool {
    status_line.chars().count() > 96
}

async fn resolve_continue_target(cli: &mut Cli, store: &ActiveSessionStore) -> Result<()> {
    if cli.resume.is_some() || !cli.continue_latest {
        return Ok(());
    }

    let summary = store
        .list_sessions()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No conversation found to continue"))?;
    cli.resume = Some(summary.session_id.to_string());
    Ok(())
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

fn build_user_interruption_message(session_id: SessionId, parent_id: Option<Uuid>) -> Message {
    build_text_message(
        session_id,
        MessageRole::User,
        REQUEST_INTERRUPTED_MESSAGE.to_owned(),
        parent_id,
    )
}

fn build_ui_event_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    text: String,
    ui_role: &str,
    ui_author: Option<String>,
) -> Message {
    let mut message = build_text_message(session_id, MessageRole::Attachment, text, parent_id);
    message.metadata.tags.push(UI_EVENT_TAG.to_owned());
    message
        .metadata
        .attributes
        .insert(UI_ROLE_ATTRIBUTE.to_owned(), ui_role.to_owned());
    if let Some(author) = ui_author.filter(|value| !value.trim().is_empty()) {
        message
            .metadata
            .attributes
            .insert(UI_AUTHOR_ATTRIBUTE.to_owned(), author);
    }
    message
}

fn build_repl_command_input_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    raw_input: impl Into<String>,
) -> Message {
    build_ui_event_message(session_id, parent_id, raw_input.into(), "command", None)
}

fn build_repl_command_output_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    command_name: &str,
    output: impl Into<String>,
) -> Message {
    build_ui_event_message(
        session_id,
        parent_id,
        output.into(),
        "command_output",
        Some(format!("/{command_name}")),
    )
}

fn should_record_repl_command(name: &str) -> bool {
    !matches!(name, "clear" | "resume")
}

fn should_echo_command_result_in_footer(
    command_name: &str,
    command_recorded: bool,
    is_error: bool,
) -> bool {
    if command_recorded {
        return false;
    }
    if is_error {
        return true;
    }
    command_name != "resume"
}

async fn append_session_message(
    store: &ActiveSessionStore,
    raw_messages: &mut Vec<Message>,
    message: Message,
) -> Result<()> {
    let session_id = message
        .session_id
        .ok_or_else(|| anyhow!("session message missing session id"))?;
    store.append_message(session_id, &message).await?;
    raw_messages.push(message);
    Ok(())
}

async fn append_session_messages(
    store: &ActiveSessionStore,
    raw_messages: &mut Vec<Message>,
    messages: Vec<Message>,
) -> Result<()> {
    for message in messages {
        append_session_message(store, raw_messages, message).await?;
    }
    Ok(())
}

fn optimistic_messages_for_command(
    raw_messages: &[Message],
    session_id: SessionId,
    raw_input: &str,
) -> Vec<Message> {
    let mut preview_messages = raw_messages.to_vec();
    preview_messages.push(build_repl_command_input_message(
        session_id,
        raw_messages.last().map(|message| message.id),
        raw_input.to_owned(),
    ));
    preview_messages
}

pub(crate) fn resume_picker_item(summary: &SessionSummary) -> ChoiceListItem {
    let prompt = preview_lines_from_text(summary.first_prompt.clone(), 1, 56).join(" ");
    ChoiceListItem {
        label: format!("s:{}  {prompt}", short_session_id(summary.session_id)),
        detail: Some(format!(
            "{} messages · {}",
            summary.message_count,
            shorten_path(&summary.transcript_path, 64)
        )),
        secondary: None,
    }
}

async fn resume_repl_session(
    store: &ActiveSessionStore,
    repl_session: &mut ReplSessionState,
    raw_messages: &mut Vec<Message>,
    target: &str,
) -> Result<PathBuf> {
    let (session_id, transcript_path, messages) = store.load_resume_target(target).await?;
    repl_session.session_id = session_id;
    repl_session.transcript_path = Some(transcript_path.clone());
    *raw_messages = messages;
    Ok(transcript_path)
}

fn slash_command_footer_status(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    command_name: &str,
    command_recorded: bool,
    is_error: bool,
    detail: &str,
) -> String {
    let base = repl_status(provider, active_model, session_id);
    if should_echo_command_result_in_footer(command_name, command_recorded, is_error) {
        status_with_detail(base, detail)
    } else {
        base
    }
}

fn task_status_summary(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::WaitingForInput => "waiting",
        TaskStatus::Completed => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "stopped",
    }
}

fn preview_detail(text: &str, max_lines: usize, max_width: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(preview_lines_from_text(trimmed.to_owned(), max_lines, max_width).join("\n"))
}

fn task_prefers_input(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::WaitingForInput
    )
}

fn summarize_task_detail(
    status: &TaskStatus,
    input: Option<&str>,
    output: Option<&str>,
    max_lines: usize,
    max_width: usize,
) -> Option<String> {
    let detail = if task_prefers_input(status) {
        input.or(output)
    } else {
        output.or(input)
    };
    detail.and_then(|text| preview_detail(text, max_lines, max_width))
}

fn tool_display_name(tool_name: &str) -> String {
    let normalized = tool_name.replace('_', " ");
    let mut chars = normalized.chars();
    match chars.next() {
        Some(first) => format!(
            "{}{}",
            first.to_ascii_uppercase(),
            chars.collect::<String>()
        ),
        None => "Tool".to_owned(),
    }
}

fn first_non_empty_string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    })
}

fn pending_detail_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => preview_detail(text, 1, 96),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.trim().to_owned()),
                    Value::Number(number) => Some(number.to_string()),
                    Value::Bool(flag) => Some(flag.to_string()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            preview_detail(&joined, 1, 96)
        }
        _ => None,
    }
}

fn pending_tool_detail_from_json(tool_name: &str, payload: &Value) -> Option<String> {
    let preferred_keys: &[&str] = match tool_name {
        "bash" | "powershell" | "terminal_capture" => &["command"],
        "file_read" | "file_write" | "file_edit" | "apply_patch" => &["path", "filePath"],
        "grep" => &["pattern", "query"],
        "glob" => &["pattern"],
        "web_fetch" | "fetch_webpage" => &["url", "query"],
        "memory" => &["action", "value"],
        "run_in_terminal" => &["command", "goal"],
        _ => &[
            "command",
            "path",
            "filePath",
            "query",
            "pattern",
            "url",
            "tool_name",
            "toolName",
            "action",
            "title",
            "prompt",
        ],
    };

    if let Some(text) = first_non_empty_string_field(payload, preferred_keys) {
        return preview_detail(text, 1, 96);
    }

    payload
        .as_object()
        .and_then(|object| object.values().find_map(pending_detail_from_value))
        .or_else(|| preview_detail(&payload.to_string(), 1, 96))
}

fn pending_tool_detail_from_call(call: &code_agent_core::ToolCall) -> Option<String> {
    serde_json::from_str::<Value>(&call.input_json)
        .ok()
        .as_ref()
        .and_then(|payload| pending_tool_detail_from_json(&call.name, payload))
}

fn pending_tool_detail_from_metadata(tool_name: &str, metadata: &Value) -> Option<String> {
    if let Ok(task) = serde_json::from_value::<TaskRecord>(metadata.clone()) {
        return summarize_task_detail(
            &task.status,
            task.input.as_deref(),
            task.output.as_deref(),
            1,
            96,
        );
    }

    if let Some(workflow_task) = metadata
        .get("workflow")
        .cloned()
        .and_then(|value| serde_json::from_value::<TaskRecord>(value).ok())
    {
        return summarize_task_detail(
            &workflow_task.status,
            workflow_task.input.as_deref(),
            workflow_task.output.as_deref(),
            1,
            96,
        );
    }

    pending_tool_detail_from_json(tool_name, metadata)
}

fn compose_pending_progress_label(status_label: &str, status_detail: Option<&str>) -> String {
    match status_detail
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => format!("{status_label} · {detail}"),
        None => status_label.to_owned(),
    }
}

fn title_case_progress_label(label: &str) -> String {
    let trimmed = label.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => "Working".to_owned(),
    }
}

fn pending_spinner_verb(progress_label: &str) -> String {
    let trimmed = progress_label.trim();
    if trimmed.eq_ignore_ascii_case("waiting for response") {
        sample_spinner_verb().to_owned()
    } else {
        title_case_progress_label(trimmed)
    }
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn task_is_recent_completion(task: &TaskRecord, now_ms: i64) -> bool {
    task.status == TaskStatus::Completed
        && now_ms.saturating_sub(task.updated_at_unix_ms) <= RECENT_COMPLETED_TTL_MS
}

fn task_ui_sort_rank(task: &TaskRecord, now_ms: i64) -> usize {
    if task_is_recent_completion(task, now_ms) {
        return 0;
    }

    match task.status {
        TaskStatus::Running => 1,
        TaskStatus::WaitingForInput => 2,
        TaskStatus::Pending => 3,
        TaskStatus::Completed => 4,
        TaskStatus::Failed => 5,
        TaskStatus::Cancelled => 6,
    }
}

fn sort_task_records_for_ui(tasks: &mut [TaskRecord], now_ms: i64) {
    tasks.sort_by(|left, right| {
        task_ui_sort_rank(left, now_ms)
            .cmp(&task_ui_sort_rank(right, now_ms))
            .then_with(|| right.updated_at_unix_ms.cmp(&left.updated_at_unix_ms))
            .then_with(|| left.created_at_unix_ms.cmp(&right.created_at_unix_ms))
            .then_with(|| left.title.cmp(&right.title))
    });
}

fn task_tree_prefixes(
    depth: usize,
    ancestor_has_next: &[bool],
    has_next: bool,
) -> (String, String) {
    if depth == 0 {
        return (String::new(), "  ".to_owned());
    }

    let mut base = String::new();
    for ancestor_has_more in ancestor_has_next {
        base.push_str(if *ancestor_has_more { "│  " } else { "   " });
    }

    let branch = if has_next { "├─ " } else { "└─ " };
    let detail = if has_next { "│    " } else { "     " };
    (format!("{base}{branch}"), format!("{base}{detail}"))
}

fn flatten_task_ui_entries(
    siblings: Vec<TaskRecord>,
    children_by_parent: &mut BTreeMap<Uuid, Vec<TaskRecord>>,
    depth: usize,
    ancestor_has_next: &[bool],
    now_ms: i64,
    entries: &mut Vec<TaskUiEntry>,
) {
    let sibling_count = siblings.len();

    for (index, task) in siblings.into_iter().enumerate() {
        let has_next = index + 1 < sibling_count;
        let (tree_prefix, detail_prefix) = task_tree_prefixes(depth, ancestor_has_next, has_next);
        let task_id = task.id;
        let is_recent_completion = task_is_recent_completion(&task, now_ms);
        let owner_label = task_owner_label(&task);
        let blocker_labels = task_blocker_labels(&task);

        entries.push(TaskUiEntry {
            id: task_id.to_string(),
            parent_id: task.parent_task_id.map(|id| id.to_string()),
            title: task.title,
            kind: task.kind,
            status: task.status,
            owner_label,
            blocker_labels,
            input: task.input,
            output: task.output,
            tree_prefix,
            detail_prefix,
            is_recent_completion,
        });

        if let Some(mut children) = children_by_parent.remove(&task_id) {
            sort_task_records_for_ui(&mut children, now_ms);

            let next_ancestor_has_next = if depth == 0 {
                ancestor_has_next.to_vec()
            } else {
                let mut values = ancestor_has_next.to_vec();
                values.push(has_next);
                values
            };

            flatten_task_ui_entries(
                children,
                children_by_parent,
                depth + 1,
                &next_ancestor_has_next,
                now_ms,
                entries,
            );
        }
    }
}

fn short_task_reference(value: &str) -> String {
    let trimmed = value
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\''))
        .trim_start_matches('@')
        .trim_start_matches('#');
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(uuid) = Uuid::parse_str(trimmed) {
        return uuid
            .to_string()
            .split('-')
            .next()
            .unwrap_or(trimmed)
            .to_owned();
    }

    if trimmed.chars().count() > 18 {
        return shorten_middle(trimmed, 18);
    }

    trimmed.to_owned()
}

fn parse_task_metadata_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut labels = Vec::new();
    let mut push_label = |raw: &str| {
        let label = short_task_reference(raw);
        if !label.is_empty() && !labels.contains(&label) {
            labels.push(label);
        }
    };

    if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
        for value in values {
            push_label(&value);
        }
        return labels;
    }

    for value in trimmed.split(|ch| matches!(ch, ',' | ';' | '\n')) {
        push_label(value);
    }

    labels
}

fn task_owner_label(task: &TaskRecord) -> Option<String> {
    task.metadata
        .get("owner")
        .or_else(|| task.metadata.get("agent"))
        .map(|value| short_task_reference(value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            task.agent_id
                .map(|agent_id| short_task_reference(&agent_id.to_string()))
                .filter(|value| !value.is_empty())
        })
}

fn task_blocker_labels(task: &TaskRecord) -> Vec<String> {
    [
        "blocked_by",
        "blockedBy",
        "blockers",
        "waiting_for",
        "waitingFor",
    ]
    .into_iter()
    .find_map(|key| task.metadata.get(key))
    .map(|value| parse_task_metadata_list(value))
    .unwrap_or_default()
}

fn task_entries_for_ui(tasks: Vec<TaskRecord>) -> Vec<TaskUiEntry> {
    if tasks.is_empty() {
        return Vec::new();
    }

    let now_ms = current_time_ms();
    let known_ids = tasks.iter().map(|task| task.id).collect::<BTreeSet<_>>();
    let mut roots = Vec::new();
    let mut children_by_parent = BTreeMap::<Uuid, Vec<TaskRecord>>::new();

    for task in tasks {
        if let Some(parent_id) = task
            .parent_task_id
            .filter(|parent_id| known_ids.contains(parent_id))
        {
            children_by_parent.entry(parent_id).or_default().push(task);
        } else {
            roots.push(task);
        }
    }

    sort_task_records_for_ui(&mut roots, now_ms);
    for children in children_by_parent.values_mut() {
        sort_task_records_for_ui(children, now_ms);
    }

    let mut entries = Vec::new();
    flatten_task_ui_entries(roots, &mut children_by_parent, 0, &[], now_ms, &mut entries);

    if !children_by_parent.is_empty() {
        let mut remaining = children_by_parent
            .into_values()
            .flatten()
            .collect::<Vec<_>>();
        sort_task_records_for_ui(&mut remaining, now_ms);
        flatten_task_ui_entries(
            remaining,
            &mut BTreeMap::new(),
            0,
            &[],
            now_ms,
            &mut entries,
        );
    }

    entries
}

fn preview_task_kind(kind: &str) -> Option<&str> {
    match kind {
        "" | "task" | "workflow" | "workflow_step" => None,
        "assistant_worker" => Some("worker"),
        "assistant_synthesis" => Some("synthesis"),
        other => Some(other),
    }
}

fn preview_task_icon(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "○",
        TaskStatus::Running => "●",
        TaskStatus::WaitingForInput => "◆",
        TaskStatus::Completed => "✓",
        TaskStatus::Failed => "✕",
        TaskStatus::Cancelled => "◌",
    }
}

fn task_preview_line(task: &TaskUiEntry) -> String {
    let kind_suffix = preview_task_kind(&task.kind)
        .map(|kind| format!("  [{kind}]"))
        .unwrap_or_default();
    let owner_suffix = task
        .owner_label
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|owner| format!(" (@{owner})"))
        .unwrap_or_default();
    let blocker_suffix = if task.blocker_labels.is_empty() {
        String::new()
    } else {
        let blockers = task
            .blocker_labels
            .iter()
            .map(|label| format!("#{label}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("  -> blocked by {blockers}")
    };
    format!(
        "{}{} {}{}{}{}",
        task.tree_prefix,
        preview_task_icon(&task.status),
        task.title,
        owner_suffix,
        blocker_suffix,
        kind_suffix
    )
}

fn pending_step_title(step: &PendingReplStep) -> String {
    let status_label = step.status_label.trim();
    let step_suffix = format!("· step {}", step.step);
    let normalized_label = status_label
        .strip_suffix(&step_suffix)
        .map(str::trim)
        .unwrap_or(status_label);
    let completed_label = format!("Completed step {}", step.step);
    let normalized_label = if normalized_label.eq_ignore_ascii_case(&completed_label) {
        "Completed"
    } else {
        normalized_label
    };
    let summary = compose_pending_progress_label(normalized_label, step.status_detail.as_deref());

    if summary.trim().is_empty() {
        format!("Step {}", step.step)
    } else {
        format!("Step {} · {summary}", step.step)
    }
}

fn pending_step_task_entries_for_ui(pending_view: &PendingReplView) -> Vec<TaskUiEntry> {
    pending_view
        .steps
        .iter()
        .map(|step| TaskUiEntry {
            id: step.id(),
            parent_id: None,
            title: pending_step_title(step),
            kind: "workflow_step".to_owned(),
            status: step.task_status.clone(),
            owner_label: None,
            blocker_labels: Vec::new(),
            input: None,
            output: None,
            tree_prefix: String::new(),
            detail_prefix: "  ".to_owned(),
            is_recent_completion: matches!(step.task_status, TaskStatus::Completed),
        })
        .collect()
}

fn pending_task_preview(pending_view: Option<&PendingReplView>) -> Option<PanePreview> {
    let pending_view = pending_view.filter(|view| !view.steps.is_empty())?;
    Some(PanePreview {
        title: "Tasks".to_owned(),
        lines: pending_step_task_entries_for_ui(pending_view)
            .into_iter()
            .take(6)
            .map(|task| task_preview_line(&task))
            .collect(),
    })
}

fn summarize_task_ui_event(task: &TaskRecord) -> String {
    let mut lines = vec![format!(
        "{} {} [{}]",
        task_status_summary(&task.status),
        task.title,
        task.kind
    )];
    if let Some(detail) = summarize_task_detail(
        &task.status,
        task.input.as_deref(),
        task.output.as_deref(),
        2,
        96,
    ) {
        lines.push(detail);
    }
    lines.join("\n")
}

fn summarize_question_ui_event(question: &QuestionRequest) -> String {
    let mut lines = vec![question.prompt.clone()];
    if !question.choices.is_empty() {
        lines.push(format!("choices: {}", question.choices.join(", ")));
    }
    lines.join("\n")
}

fn tool_ui_event_messages(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    metadata: &Value,
) -> Vec<Message> {
    let mut events = Vec::new();
    let mut next_parent_id = parent_id;

    if let Ok(task) = serde_json::from_value::<TaskRecord>(metadata.clone()) {
        let message = build_ui_event_message(
            session_id,
            next_parent_id,
            summarize_task_ui_event(&task),
            "task",
            Some("Task".to_owned()),
        );
        next_parent_id = Some(message.id);
        events.push(message);
    }

    if let Some(workflow_task) = metadata
        .get("workflow")
        .cloned()
        .and_then(|value| serde_json::from_value::<TaskRecord>(value).ok())
    {
        let message = build_ui_event_message(
            session_id,
            next_parent_id,
            summarize_task_ui_event(&workflow_task),
            "task",
            Some("Task".to_owned()),
        );
        next_parent_id = Some(message.id);
        events.push(message);
    }

    if let Ok(question) = serde_json::from_value::<QuestionRequest>(metadata.clone()) {
        events.push(build_ui_event_message(
            session_id,
            next_parent_id,
            summarize_question_ui_event(&question),
            "task",
            Some("Question".to_owned()),
        ));
    }

    events
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

fn ide_bridge_address(cli: &Cli) -> Option<&str> {
    cli.bridge_connect
        .as_deref()
        .filter(|address| address.starts_with("ide://"))
        .or_else(|| {
            cli.bridge_server
                .as_deref()
                .filter(|address| address.starts_with("ide://"))
        })
}

fn ide_bridge_enabled(cli: &Cli) -> bool {
    ide_bridge_address(cli).is_some()
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

fn preview_lines_from_text(
    text: impl Into<String>,
    max_lines: usize,
    max_width: usize,
) -> Vec<String> {
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

fn apple_terminal_option_digit(ch: char) -> Option<PaneKind> {
    match ch {
        '¡' => pane_from_digit('1'),
        '™' => pane_from_digit('2'),
        '£' => pane_from_digit('3'),
        '¢' => pane_from_digit('4'),
        '∞' => pane_from_digit('5'),
        '§' => pane_from_digit('6'),
        _ => None,
    }
}

fn key_routing_modifiers(modifiers: KeyModifiers) -> KeyModifiers {
    modifiers
        & (KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

fn key_matches_char_with_modifiers(key: &KeyEvent, ch: char, modifiers: KeyModifiers) -> bool {
    matches!(key.code, KeyCode::Char(code) if code.to_ascii_lowercase() == ch)
        && key_routing_modifiers(key.modifiers) == modifiers
}

fn is_plain_ctrl_char(key: &KeyEvent, ch: char) -> bool {
    key_matches_char_with_modifiers(key, ch, KeyModifiers::CONTROL)
}

fn pane_from_shortcut_for_terminal(
    key: &crossterm::event::KeyEvent,
    term_program: Option<&str>,
) -> Option<PaneKind> {
    let modifiers = key_routing_modifiers(key.modifiers);
    if modifiers == KeyModifiers::CONTROL
        || modifiers == KeyModifiers::ALT
        || modifiers == KeyModifiers::SUPER
    {
        return match key.code {
            KeyCode::Char(ch) => pane_from_digit(ch),
            _ => None,
        };
    }

    if term_program == Some("Apple_Terminal") {
        return match key.code {
            KeyCode::Char(ch) => apple_terminal_option_digit(ch),
            _ => None,
        };
    }

    None
}

fn pane_from_shortcut(key: &crossterm::event::KeyEvent) -> Option<PaneKind> {
    pane_from_shortcut_for_terminal(key, std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn recent_task_preview(cwd: &Path) -> PanePreview {
    let store = task_store_for(cwd);
    match store.list_tasks() {
        Ok(tasks) if tasks.is_empty() => PanePreview {
            title: "Tasks".to_owned(),
            lines: vec!["No task activity yet.".to_owned()],
        },
        Ok(tasks) => {
            let lines = task_entries_for_ui(tasks)
                .into_iter()
                .take(6)
                .map(|task| task_preview_line(&task))
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

fn load_task_ui_data(cwd: &Path) -> (Vec<TaskUiEntry>, Vec<QuestionUiEntry>) {
    let store = task_store_for(cwd);
    let tasks = task_entries_for_ui(store.list_tasks().unwrap_or_default());

    let questions = store
        .list_questions()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(3)
        .map(|question| QuestionUiEntry {
            prompt: question.prompt,
            choices: question.choices,
            task_title: None,
        })
        .collect::<Vec<_>>();

    (tasks, questions)
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
        let resolved = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplTranscriptSearchState {
    input_buffer: code_agent_ui::InputBuffer,
    open: bool,
    active_item: Option<usize>,
    saved_input_buffer: code_agent_ui::InputBuffer,
    saved_active_item: Option<usize>,
    anchor_scroll: u16,
}

impl ReplTranscriptSearchState {
    fn ui_state(&self) -> TranscriptSearchState {
        TranscriptSearchState {
            input_buffer: self.input_buffer.clone(),
            open: self.open,
            active_item: self.active_item,
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplPromptHistorySearchState {
    input_buffer: code_agent_ui::InputBuffer,
    original_input_buffer: code_agent_ui::InputBuffer,
    active_history_index: Option<usize>,
    active_match_position: Option<usize>,
    match_count: usize,
    failed_match: bool,
    last_query: String,
}

impl ReplPromptHistorySearchState {
    fn ui_state(&self) -> PromptHistorySearchState {
        PromptHistorySearchState {
            input_buffer: self.input_buffer.clone(),
            active_match: self.active_match_position.map(|position| position + 1),
            match_count: self.match_count,
            failed_match: self.failed_match,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplMessageActionState {
    selected_item: usize,
}

#[derive(Clone, Debug)]
struct ReplMessageActionItem {
    item_index: usize,
    message: Message,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplMessageActionNavigation {
    Prev,
    Next,
    PrevUser,
    NextUser,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolPrimaryInput {
    label: &'static str,
    value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplInteractionState {
    transcript_mode: bool,
    transcript_search: ReplTranscriptSearchState,
    prompt_history_search: Option<ReplPromptHistorySearchState>,
    message_actions: Option<ReplMessageActionState>,
    prompt_selection: Option<PromptSelectionState>,
    prompt_mouse_anchor: Option<usize>,
    transcript_selection: Option<TranscriptSelectionState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptSelectionMove {
    Left,
    Right,
    LineStart,
    LineEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplShortcutAction {
    CopySelection,
    ContextCtrlC,
    ToggleTranscriptMode,
    PromptHistorySearch,
    EnterMessageActions,
    SelectPane(PaneKind),
    RotatePaneForward,
    RotatePaneBackward,
}

fn is_selection_copy_shortcut(key: &KeyEvent) -> bool {
    key_matches_char_with_modifiers(key, 'c', KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        || key_matches_char_with_modifiers(key, 'c', KeyModifiers::SUPER)
}

fn repl_shortcut_action_for_key(
    key: &KeyEvent,
    interaction_state: &ReplInteractionState,
) -> Option<ReplShortcutAction> {
    if (interaction_state.transcript_selection.is_some()
        || interaction_state.prompt_selection.is_some())
        && is_selection_copy_shortcut(key)
    {
        return Some(ReplShortcutAction::CopySelection);
    }

    if is_plain_ctrl_char(key, 'c') {
        return Some(ReplShortcutAction::ContextCtrlC);
    }

    if is_plain_ctrl_char(key, 'o') {
        return Some(ReplShortcutAction::ToggleTranscriptMode);
    }

    if !interaction_state.transcript_mode
        && interaction_state.message_actions.is_none()
        && interaction_state.transcript_selection.is_none()
        && is_plain_ctrl_char(key, 'r')
    {
        return Some(ReplShortcutAction::PromptHistorySearch);
    }

    if matches!(key.code, KeyCode::Up)
        && key.modifiers == KeyModifiers::SHIFT
        && interaction_state.message_actions.is_none()
        && interaction_state.prompt_history_search.is_none()
        && interaction_state.transcript_selection.is_none()
        && !interaction_state.transcript_search.open
    {
        return Some(ReplShortcutAction::EnterMessageActions);
    }

    if interaction_state.transcript_mode {
        return None;
    }

    if let Some(pane) = pane_from_shortcut(key) {
        return Some(ReplShortcutAction::SelectPane(pane));
    }

    match key.code {
        KeyCode::Tab => Some(ReplShortcutAction::RotatePaneForward),
        KeyCode::BackTab => Some(ReplShortcutAction::RotatePaneBackward),
        _ => None,
    }
}

fn enter_transcript_mode(interaction_state: &mut ReplInteractionState, active_pane: &mut PaneKind) {
    interaction_state.transcript_mode = true;
    interaction_state.prompt_history_search = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    *active_pane = PaneKind::Transcript;
}

fn exit_transcript_mode(interaction_state: &mut ReplInteractionState) {
    interaction_state.transcript_mode = false;
    interaction_state.transcript_search.reset();
    interaction_state.prompt_history_search = None;
    interaction_state.message_actions = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
}

fn open_prompt_history_search(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &code_agent_ui::InputBuffer,
) {
    let search_state = interaction_state
        .prompt_history_search
        .get_or_insert_with(|| ReplPromptHistorySearchState {
            original_input_buffer: input_buffer.clone(),
            ..ReplPromptHistorySearchState::default()
        });
    search_state.input_buffer.cursor = search_state.input_buffer.chars.len();
    interaction_state.message_actions = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
}

fn cancel_prompt_history_search(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let Some(search_state) = interaction_state.prompt_history_search.take() else {
        return false;
    };
    *input_buffer = search_state.original_input_buffer;
    true
}

fn accept_prompt_history_search(interaction_state: &mut ReplInteractionState) -> bool {
    interaction_state.prompt_history_search.take().is_some()
}

fn prompt_history_match_cursor(entry: &str, query: &str) -> Option<usize> {
    let byte_index = entry.rfind(query)?;
    Some(entry[..byte_index].chars().count())
}

fn preview_prompt_history_match(
    history: &[String],
    history_index: usize,
    query: &str,
    input_buffer: &mut code_agent_ui::InputBuffer,
) {
    let Some(entry) = history.get(history_index) else {
        return;
    };

    input_buffer.replace(entry.clone());
    if let Some(cursor) = prompt_history_match_cursor(entry, query) {
        input_buffer.cursor = cursor.min(input_buffer.chars.len());
    }
}

fn sync_prompt_history_search_preview(
    history: &[String],
    search_state: &mut ReplPromptHistorySearchState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) {
    let query = search_state.input_buffer.as_str();
    if query.is_empty() {
        *input_buffer = search_state.original_input_buffer.clone();
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.match_count = 0;
        search_state.failed_match = false;
        search_state.last_query.clear();
        return;
    }

    if search_state.last_query != query {
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.failed_match = false;
    }

    let matches = prompt_history_search_matches(history, &query);
    search_state.match_count = matches.len();
    search_state.last_query = query.clone();

    let Some(&history_index) = matches.first() else {
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.failed_match = true;
        return;
    };

    search_state.active_history_index = Some(history_index);
    search_state.active_match_position = Some(0);
    search_state.failed_match = false;
    preview_prompt_history_match(history, history_index, &query, input_buffer);
}

fn step_prompt_history_search_match(
    history: &[String],
    search_state: &mut ReplPromptHistorySearchState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let query = search_state.input_buffer.as_str();
    if query.is_empty() {
        *input_buffer = search_state.original_input_buffer.clone();
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.match_count = 0;
        search_state.failed_match = false;
        search_state.last_query.clear();
        return false;
    }

    let matches = prompt_history_search_matches(history, &query);
    search_state.match_count = matches.len();
    search_state.last_query = query.clone();

    let Some(current_position) = search_state.active_history_index.and_then(|current_index| {
        matches
            .iter()
            .position(|candidate| *candidate == current_index)
    }) else {
        return if let Some(&history_index) = matches.first() {
            search_state.active_history_index = Some(history_index);
            search_state.active_match_position = Some(0);
            search_state.failed_match = false;
            preview_prompt_history_match(history, history_index, &query, input_buffer);
            true
        } else {
            search_state.active_history_index = None;
            search_state.active_match_position = None;
            search_state.failed_match = true;
            false
        };
    };

    let Some(&history_index) = matches.get(current_position + 1) else {
        search_state.active_match_position = Some(current_position);
        search_state.failed_match = true;
        return false;
    };

    search_state.active_history_index = Some(history_index);
    search_state.active_match_position = Some(current_position + 1);
    search_state.failed_match = false;
    preview_prompt_history_match(history, history_index, &query, input_buffer);
    true
}

fn open_transcript_search(search_state: &mut ReplTranscriptSearchState, transcript_scroll: u16) {
    if !search_state.open {
        search_state.saved_input_buffer = search_state.input_buffer.clone();
        search_state.saved_active_item = search_state.active_item;
        search_state.anchor_scroll = transcript_scroll;
    }
    search_state.open = true;
    search_state.input_buffer.cursor = search_state.input_buffer.chars.len();
}

fn cancel_transcript_search(
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
) {
    search_state.input_buffer = search_state.saved_input_buffer.clone();
    search_state.active_item = search_state.saved_active_item;
    search_state.open = false;
    *transcript_scroll = search_state.anchor_scroll;
}

fn sync_transcript_search_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
) {
    let query = search_state.input_buffer.as_str();
    if query.trim().is_empty() {
        search_state.active_item = None;
        *transcript_scroll = search_state.anchor_scroll;
        return;
    }

    let matches = transcript_search_match_items(ui_state, &query);
    if matches.is_empty() {
        search_state.active_item = None;
        *transcript_scroll = search_state.anchor_scroll;
        return;
    }

    let active_item = search_state
        .active_item
        .filter(|item| matches.contains(item))
        .unwrap_or(matches[0]);
    search_state.active_item = Some(active_item);
    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, active_item)
    {
        *transcript_scroll = scroll;
    }
}

fn step_transcript_search_match(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
    reverse: bool,
) -> bool {
    let query = search_state.input_buffer.as_str();
    let matches = transcript_search_match_items(ui_state, &query);
    if matches.is_empty() {
        search_state.active_item = None;
        return false;
    }

    let next_index = match search_state
        .active_item
        .and_then(|item| matches.iter().position(|candidate| *candidate == item))
    {
        Some(index) if reverse => index.checked_sub(1).unwrap_or(matches.len() - 1),
        Some(index) => {
            if index + 1 < matches.len() {
                index + 1
            } else {
                0
            }
        }
        None if reverse => matches.len() - 1,
        None => 0,
    };

    let active_item = matches[next_index];
    search_state.active_item = Some(active_item);
    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, active_item)
    {
        *transcript_scroll = scroll;
    }
    true
}

fn push_message_action_items(
    items: &mut Vec<ReplMessageActionItem>,
    messages: &[Message],
    item_index: &mut usize,
) {
    for message in messages {
        let transcript_line = transcript_line_from_message(message);
        if !transcript_line.text.trim().is_empty() {
            items.push(ReplMessageActionItem {
                item_index: *item_index,
                message: message.clone(),
            });
        }
        *item_index += 1;
    }
}

fn message_action_items_from_runtime(
    runtime_messages: &[Message],
    pending_view: Option<&PendingReplView>,
) -> Vec<ReplMessageActionItem> {
    let mut items = Vec::new();
    let mut item_index = 0usize;

    if let Some(pending_view) = pending_view.filter(|view| !view.steps.is_empty()) {
        let first_step_start = pending_view
            .steps
            .first()
            .map(|step| step.start_index.min(runtime_messages.len()))
            .unwrap_or(runtime_messages.len());
        push_message_action_items(
            &mut items,
            &runtime_messages[..first_step_start],
            &mut item_index,
        );

        if !pending_view.show_transcript_details {
            return items;
        }

        for (index, step) in pending_view.steps.iter().enumerate() {
            item_index += 1;

            if !step.expanded {
                continue;
            }

            let end_index = pending_view
                .steps
                .get(index + 1)
                .map(|next| next.start_index)
                .unwrap_or(runtime_messages.len())
                .min(runtime_messages.len());
            let start_index = step.start_index.min(end_index);
            push_message_action_items(
                &mut items,
                &runtime_messages[start_index..end_index],
                &mut item_index,
            );
        }

        return items;
    }

    push_message_action_items(&mut items, runtime_messages, &mut item_index);
    items
}

fn enter_message_actions(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> bool {
    let Some(item) = items.last() else {
        return false;
    };

    interaction_state.transcript_search.reset();
    interaction_state.prompt_history_search = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
    interaction_state.message_actions = Some(ReplMessageActionState {
        selected_item: item.item_index,
    });
    true
}

fn normalize_message_actions(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> Option<usize> {
    let actions = interaction_state.message_actions.as_mut()?;
    if items.is_empty() {
        interaction_state.message_actions = None;
        return None;
    }

    if !items
        .iter()
        .any(|item| item.item_index == actions.selected_item)
    {
        actions.selected_item = items.last()?.item_index;
    }

    Some(actions.selected_item)
}

fn selected_message_action_item<'a>(
    interaction_state: &mut ReplInteractionState,
    items: &'a [ReplMessageActionItem],
) -> Option<&'a ReplMessageActionItem> {
    let selected_item = normalize_message_actions(interaction_state, items)?;
    items.iter().find(|item| item.item_index == selected_item)
}

fn move_message_action_selection(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
    navigation: ReplMessageActionNavigation,
) -> bool {
    let Some(selected_item) = normalize_message_actions(interaction_state, items) else {
        return false;
    };

    let Some(current_index) = items
        .iter()
        .position(|item| item.item_index == selected_item)
    else {
        return false;
    };

    let target_index = match navigation {
        ReplMessageActionNavigation::Top => 0,
        ReplMessageActionNavigation::Bottom => items.len().saturating_sub(1),
        ReplMessageActionNavigation::Prev => {
            current_index.checked_sub(1).unwrap_or(items.len() - 1)
        }
        ReplMessageActionNavigation::Next => {
            if current_index + 1 < items.len() {
                current_index + 1
            } else {
                0
            }
        }
        ReplMessageActionNavigation::PrevUser => (1..=items.len())
            .find_map(|offset| {
                let index = (current_index + items.len() - offset) % items.len();
                (items[index].message.role == MessageRole::User).then_some(index)
            })
            .unwrap_or(current_index),
        ReplMessageActionNavigation::NextUser => (1..=items.len())
            .find_map(|offset| {
                let index = (current_index + offset) % items.len();
                (items[index].message.role == MessageRole::User).then_some(index)
            })
            .unwrap_or(current_index),
    };

    let next_item = &items[target_index];
    if next_item.item_index == selected_item {
        return false;
    }

    if let Some(actions) = interaction_state.message_actions.as_mut() {
        actions.selected_item = next_item.item_index;
        return true;
    }

    false
}

fn sync_message_action_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    interaction_state: &ReplInteractionState,
    transcript_scroll: &mut u16,
) {
    let Some(selected_item) = interaction_state
        .message_actions
        .as_ref()
        .map(|actions| actions.selected_item)
    else {
        return;
    };

    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, selected_item)
    {
        *transcript_scroll = scroll;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptSelectionMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

fn should_clear_transcript_selection_on_key(key: &KeyEvent) -> bool {
    let is_nav = matches!(
        key.code,
        KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    );
    if is_nav
        && (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::SUPER))
    {
        return false;
    }
    true
}

fn transcript_selection_move_for_key(
    key: &KeyEvent,
    selection_exists: bool,
) -> Option<TranscriptSelectionMove> {
    if !key.modifiers.contains(KeyModifiers::SHIFT)
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }

    match key.code {
        KeyCode::Left => Some(TranscriptSelectionMove::Left),
        KeyCode::Right => Some(TranscriptSelectionMove::Right),
        KeyCode::Home => Some(TranscriptSelectionMove::LineStart),
        KeyCode::End => Some(TranscriptSelectionMove::LineEnd),
        KeyCode::Up if selection_exists => Some(TranscriptSelectionMove::Up),
        KeyCode::Down if selection_exists => Some(TranscriptSelectionMove::Down),
        _ => None,
    }
}

fn transcript_selectable_line_position(
    selectable_lines: &[TranscriptSelectableLine],
    line_index: usize,
) -> Option<usize> {
    selectable_lines
        .iter()
        .position(|line| line.line_index == line_index)
}

fn transcript_selection_default_focus(
    selectable_lines: &[TranscriptSelectableLine],
) -> Option<TranscriptSelectionPoint> {
    let line = selectable_lines.last()?;
    Some(TranscriptSelectionPoint {
        line_index: line.line_index,
        column: line.text.chars().count(),
    })
}

fn move_transcript_selection_focus(
    selectable_lines: &[TranscriptSelectableLine],
    focus: &TranscriptSelectionPoint,
    selection_move: TranscriptSelectionMove,
) -> Option<TranscriptSelectionPoint> {
    let current_position = transcript_selectable_line_position(selectable_lines, focus.line_index)?;
    let current_line = &selectable_lines[current_position];
    let current_len = current_line.text.chars().count();

    let next_focus = match selection_move {
        TranscriptSelectionMove::Left => {
            if focus.column > 0 {
                TranscriptSelectionPoint {
                    line_index: focus.line_index,
                    column: focus.column - 1,
                }
            } else {
                let previous_line = selectable_lines.get(current_position.checked_sub(1)?)?;
                TranscriptSelectionPoint {
                    line_index: previous_line.line_index,
                    column: previous_line.text.chars().count(),
                }
            }
        }
        TranscriptSelectionMove::Right => {
            if focus.column < current_len {
                TranscriptSelectionPoint {
                    line_index: focus.line_index,
                    column: focus.column + 1,
                }
            } else {
                let next_line = selectable_lines.get(current_position + 1)?;
                TranscriptSelectionPoint {
                    line_index: next_line.line_index,
                    column: 0,
                }
            }
        }
        TranscriptSelectionMove::LineStart => TranscriptSelectionPoint {
            line_index: focus.line_index,
            column: 0,
        },
        TranscriptSelectionMove::LineEnd => TranscriptSelectionPoint {
            line_index: focus.line_index,
            column: current_len,
        },
        TranscriptSelectionMove::Up => {
            let previous_line = selectable_lines.get(current_position.checked_sub(1)?)?;
            TranscriptSelectionPoint {
                line_index: previous_line.line_index,
                column: focus.column.min(previous_line.text.chars().count()),
            }
        }
        TranscriptSelectionMove::Down => {
            let next_line = selectable_lines.get(current_position + 1)?;
            TranscriptSelectionPoint {
                line_index: next_line.line_index,
                column: focus.column.min(next_line.text.chars().count()),
            }
        }
    };

    (next_focus != *focus).then_some(next_focus)
}

fn move_transcript_selection(
    interaction_state: &mut ReplInteractionState,
    selectable_lines: &[TranscriptSelectableLine],
    selection_move: TranscriptSelectionMove,
) -> Option<TranscriptSelectionPoint> {
    let anchor = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.anchor.clone())
        .or_else(|| transcript_selection_default_focus(selectable_lines))?;
    let focus = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.focus.clone())
        .unwrap_or_else(|| anchor.clone());
    let next_focus = move_transcript_selection_focus(selectable_lines, &focus, selection_move)?;

    if next_focus == anchor {
        interaction_state.transcript_selection = None;
        return Some(anchor);
    }

    interaction_state.transcript_selection = Some(TranscriptSelectionState {
        anchor,
        focus: next_focus.clone(),
    });
    Some(next_focus)
}

fn sync_transcript_selection_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    interaction_state: &ReplInteractionState,
    transcript_scroll: &mut u16,
) {
    let Some(focus_line) = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.focus.line_index)
    else {
        return;
    };

    if let Some(scroll) =
        transcript_visual_scroll_for_view(ui_state, terminal_width, terminal_height, focus_line)
    {
        *transcript_scroll = scroll;
    }
}

fn transcript_selection_copy_text(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    interaction_state
        .transcript_selection
        .as_ref()
        .and_then(|selection| {
            transcript_selection_text_for_view(ui_state, terminal_width, selection)
        })
}

fn normalize_prompt_selection(
    selection: &PromptSelectionState,
    input_len: usize,
) -> Option<(usize, usize)> {
    let start = selection.anchor.min(selection.focus).min(input_len);
    let end = selection.anchor.max(selection.focus).min(input_len);
    (start < end).then_some((start, end))
}

fn clear_prompt_selection(interaction_state: &mut ReplInteractionState) {
    interaction_state.prompt_selection = None;
}

fn clear_prompt_mouse_anchor(interaction_state: &mut ReplInteractionState) {
    interaction_state.prompt_mouse_anchor = None;
}

fn is_paste_shortcut(key: &KeyEvent) -> bool {
    key_matches_char_with_modifiers(key, 'v', KeyModifiers::SUPER)
        || key_matches_char_with_modifiers(key, 'v', KeyModifiers::CONTROL)
        || (matches!(key.code, KeyCode::Insert)
            && key_routing_modifiers(key.modifiers) == KeyModifiers::SHIFT)
}

fn insert_buffer_text(input_buffer: &mut code_agent_ui::InputBuffer, text: &str) -> bool {
    let mut inserted = false;
    for ch in text.chars() {
        input_buffer.push(ch);
        inserted = true;
    }
    inserted
}

fn prompt_selection_move_for_key(key: &KeyEvent) -> Option<PromptSelectionMove> {
    if key_routing_modifiers(key.modifiers) != KeyModifiers::SHIFT {
        return None;
    }

    match key.code {
        KeyCode::Left => Some(PromptSelectionMove::Left),
        KeyCode::Right => Some(PromptSelectionMove::Right),
        KeyCode::Home => Some(PromptSelectionMove::LineStart),
        KeyCode::End => Some(PromptSelectionMove::LineEnd),
        _ => None,
    }
}

fn prompt_selection_text(
    input_buffer: &code_agent_ui::InputBuffer,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    let (start, end) = interaction_state
        .prompt_selection
        .as_ref()
        .and_then(|selection| normalize_prompt_selection(selection, input_buffer.chars.len()))?;
    Some(input_buffer.chars[start..end].iter().collect())
}

fn repl_selection_copy_text(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    input_buffer: &code_agent_ui::InputBuffer,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    if interaction_state.transcript_selection.is_some() {
        transcript_selection_copy_text(ui_state, terminal_width, interaction_state)
    } else {
        prompt_selection_text(input_buffer, interaction_state)
    }
}

fn move_prompt_selection_focus(
    input_buffer: &code_agent_ui::InputBuffer,
    focus: usize,
    selection_move: PromptSelectionMove,
) -> Option<usize> {
    match selection_move {
        PromptSelectionMove::Left => focus.checked_sub(1),
        PromptSelectionMove::Right => (focus < input_buffer.chars.len()).then_some(focus + 1),
        PromptSelectionMove::LineStart => (focus > 0).then_some(0),
        PromptSelectionMove::LineEnd => {
            (focus < input_buffer.chars.len()).then_some(input_buffer.chars.len())
        }
    }
}

fn move_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    selection_move: PromptSelectionMove,
) -> bool {
    let anchor = interaction_state
        .prompt_selection
        .as_ref()
        .map(|selection| selection.anchor)
        .unwrap_or_else(|| input_buffer.cursor.min(input_buffer.chars.len()));
    let focus = interaction_state
        .prompt_selection
        .as_ref()
        .map(|selection| selection.focus)
        .unwrap_or(anchor);
    let Some(next_focus) = move_prompt_selection_focus(input_buffer, focus, selection_move) else {
        return false;
    };

    input_buffer.cursor = next_focus;
    if next_focus == anchor {
        interaction_state.prompt_selection = None;
    } else {
        interaction_state.prompt_selection = Some(PromptSelectionState {
            anchor,
            focus: next_focus,
        });
    }
    true
}

fn set_prompt_cursor(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    cursor: usize,
) -> bool {
    let next_cursor = cursor.min(input_buffer.chars.len());
    let changed =
        input_buffer.cursor != next_cursor || interaction_state.prompt_selection.is_some();
    input_buffer.cursor = next_cursor;
    interaction_state.prompt_selection = None;
    changed
}

fn insert_prompt_text(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    text: &str,
) -> bool {
    let deleted = delete_prompt_selection(interaction_state, input_buffer);
    let inserted = insert_buffer_text(input_buffer, text);
    deleted || inserted
}

fn set_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    anchor: usize,
    focus: usize,
) -> bool {
    let anchor = anchor.min(input_buffer.chars.len());
    let focus = focus.min(input_buffer.chars.len());
    let previous_cursor = input_buffer.cursor;
    let previous_selection = interaction_state.prompt_selection.clone();
    let had_message_actions = interaction_state.message_actions.take().is_some();
    let had_transcript_selection = interaction_state.transcript_selection.take().is_some();

    input_buffer.cursor = focus;
    interaction_state.prompt_selection =
        (anchor != focus).then_some(PromptSelectionState { anchor, focus });

    previous_cursor != input_buffer.cursor
        || previous_selection != interaction_state.prompt_selection
        || had_message_actions
        || had_transcript_selection
}

fn handle_prompt_mouse_action(
    mouse_kind: &MouseEventKind,
    cursor: usize,
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let cursor = cursor.min(input_buffer.chars.len());
    match mouse_kind {
        MouseEventKind::Down(MouseButton::Left) => {
            interaction_state.prompt_mouse_anchor = Some(cursor);
            let had_message_actions = interaction_state.message_actions.take().is_some();
            let had_transcript_selection = interaction_state.transcript_selection.take().is_some();
            had_message_actions
                || had_transcript_selection
                || set_prompt_cursor(interaction_state, input_buffer, cursor)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let anchor = interaction_state
                .prompt_mouse_anchor
                .unwrap_or_else(|| input_buffer.cursor.min(input_buffer.chars.len()));
            interaction_state.prompt_mouse_anchor = Some(anchor);
            set_prompt_selection(interaction_state, input_buffer, anchor, cursor)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(anchor) = interaction_state.prompt_mouse_anchor.take() else {
                return false;
            };
            set_prompt_selection(interaction_state, input_buffer, anchor, cursor)
        }
        _ => false,
    }
}

fn delete_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let Some((start, end)) = interaction_state
        .prompt_selection
        .as_ref()
        .and_then(|selection| normalize_prompt_selection(selection, input_buffer.chars.len()))
    else {
        return false;
    };

    input_buffer.chars.drain(start..end);
    input_buffer.cursor = start;
    interaction_state.prompt_selection = None;
    true
}

fn primary_input_keys(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        "Read" | "Edit" | "Write" | "NotebookEdit" | "file_read" | "file_edit" | "file_write"
        | "read_file" | "create_file" | "apply_patch" | "view_image" | "list_dir"
        | "edit_notebook_file" | "create_directory" => {
            &["file_path", "path", "filePath", "dirPath", "notebook_path"]
        }
        "Bash" | "bash" | "powershell" | "terminal_capture" | "run_in_terminal" => &["command"],
        "Grep" | "grep" | "grep_search" => &["pattern", "query"],
        "Glob" | "glob" | "file_search" => &["pattern", "query"],
        "WebFetch" | "fetch_webpage" => &["url", "query"],
        "WebSearch" | "semantic_search" => &["query"],
        "Task" | "Agent" | "runSubagent" => &["prompt", "description", "query"],
        _ => &[
            "command",
            "file_path",
            "path",
            "filePath",
            "dirPath",
            "query",
            "pattern",
            "url",
            "prompt",
            "title",
        ],
    }
}

fn primary_input_label(key: &str) -> &'static str {
    match key {
        "file_path" | "path" | "filePath" | "dirPath" | "notebook_path" => "path",
        "command" => "command",
        "query" => "query",
        "pattern" => "pattern",
        "url" => "url",
        "prompt" => "prompt",
        "description" => "description",
        "title" => "title",
        _ => "input",
    }
}

fn primary_input_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(primary_input_string)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn tool_primary_input(call: &code_agent_core::ToolCall) -> Option<ToolPrimaryInput> {
    let payload = serde_json::from_str::<Value>(&call.input_json).ok()?;
    for key in primary_input_keys(&call.name) {
        if let Some(value) = payload.get(*key).and_then(primary_input_string) {
            return Some(ToolPrimaryInput {
                label: primary_input_label(key),
                value,
            });
        }
    }
    None
}

fn message_tool_call(message: &Message) -> Option<&code_agent_core::ToolCall> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::ToolCall { call } => Some(call),
        _ => None,
    })
}

fn message_primary_input(message: &Message) -> Option<ToolPrimaryInput> {
    message_tool_call(message).and_then(tool_primary_input)
}

fn message_action_copy_text(message: &Message) -> Option<String> {
    let text = message_text(message);
    if !text.trim().is_empty() {
        return Some(text);
    }

    if let Some(primary_input) = message_primary_input(message) {
        return Some(primary_input.value);
    }

    let transcript_line = transcript_line_from_message(message);
    (!transcript_line.text.trim().is_empty()).then_some(transcript_line.text)
}

fn message_actions_ui_state(
    interaction_state: &ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> Option<TranscriptMessageActionsState> {
    let selected_item = interaction_state
        .message_actions
        .as_ref()
        .map(|actions| actions.selected_item)?;
    let item = items.iter().find(|item| item.item_index == selected_item)?;

    Some(TranscriptMessageActionsState {
        active_item: selected_item,
        enter_label: (item.message.role == MessageRole::User).then(|| "edit".to_owned()),
        primary_input_label: message_primary_input(&item.message)
            .map(|input| input.label.to_owned()),
    })
}

fn build_repl_ui_state(
    app: &RatatuiApp,
    registry: &code_agent_core::CommandRegistry,
    raw_messages: &[Message],
    pending_view: Option<&PendingReplView>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    input_buffer: &code_agent_ui::InputBuffer,
    status_line: &str,
    progress_message: Option<String>,
    active_pane: PaneKind,
    compact_banner: Option<String>,
    transcript_scroll: u16,
    choice_list: Option<ChoiceListState>,
    command_suggestions: Vec<CommandPaletteEntry>,
    selected_command_suggestion: usize,
    status_marquee_tick: usize,
    interaction_state: &ReplInteractionState,
) -> code_agent_ui::UiState {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let message_action_items = message_action_items_from_runtime(&runtime_messages, pending_view);
    let mut state = app.state_from_messages(runtime_messages.clone(), &registry.all());
    if let Some(pending_view) = pending_view {
        state.queued_inputs = pending_view
            .queued_inputs
            .iter()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect();
    }
    if let Some(pending_view) = pending_view.filter(|view| !view.steps.is_empty()) {
        let first_step_start = pending_view
            .steps
            .first()
            .map(|step| step.start_index.min(runtime_messages.len()))
            .unwrap_or(runtime_messages.len());
        let visible_transcript =
            UiState::from_messages(runtime_messages[..first_step_start].to_vec());
        state.transcript_lines = visible_transcript.transcript_lines;
        state.transcript_preview = visible_transcript.transcript_preview;
        state.pending_step_count = pending_view.steps.len();
        state.pending_transcript_details = pending_view.show_transcript_details;
        if pending_view.show_transcript_details {
            state.transcript_groups = pending_view
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| {
                    let end_index = pending_view
                        .steps
                        .get(index + 1)
                        .map(|next| next.start_index)
                        .unwrap_or(runtime_messages.len())
                        .min(runtime_messages.len());
                    let start_index = step.start_index.min(end_index);
                    let slice = &runtime_messages[start_index..end_index];
                    let assistant_count = slice
                        .iter()
                        .filter(|message| message.role == MessageRole::Assistant)
                        .count();
                    let tool_count = slice
                        .iter()
                        .filter(|message| message.role == MessageRole::Tool)
                        .count();
                    let mut detail_parts = vec![format!(
                        "{} {}",
                        slice.len(),
                        if slice.len() == 1 {
                            "message"
                        } else {
                            "messages"
                        }
                    )];
                    if assistant_count > 0 {
                        detail_parts.push(format!("{} assistant", assistant_count));
                    }
                    if tool_count > 0 {
                        detail_parts.push(format!("{} tool", tool_count));
                    }
                    if let Some(detail) = step
                        .status_detail
                        .as_deref()
                        .filter(|detail| !detail.trim().is_empty())
                    {
                        detail_parts.insert(0, detail.to_owned());
                    }
                    TranscriptGroup {
                        id: step.id(),
                        title: pending_step_title(step),
                        subtitle: Some(detail_parts.join(" · ")),
                        expanded: step.expanded,
                        lines: UiState::from_messages(slice.to_vec()).transcript_lines,
                    }
                })
                .collect();
        }
    }
    apply_repl_header(&mut state, provider, active_model, cwd, session_id);
    let (mut task_items, question_items) = load_task_ui_data(cwd);
    let pending_task_items = pending_view
        .filter(|view| !view.steps.is_empty())
        .map(pending_step_task_entries_for_ui)
        .unwrap_or_default();
    if !pending_task_items.is_empty() {
        task_items.splice(0..0, pending_task_items);
    }
    state.show_input = !interaction_state.transcript_mode;
    state.input_buffer = input_buffer.clone();
    state.transcript_scroll = transcript_scroll;
    state.status_line = status_line.to_owned();
    state.progress_message = progress_message;
    state.progress_verb = pending_view.map(|pending| pending.spinner_verb.clone());
    state.active_pane = Some(
        if interaction_state.transcript_mode
            || interaction_state.message_actions.is_some()
            || interaction_state.transcript_selection.is_some()
        {
            PaneKind::Transcript
        } else {
            active_pane
        },
    );
    state.transcript_mode = interaction_state.transcript_mode;
    state.transcript_search = interaction_state
        .transcript_mode
        .then(|| interaction_state.transcript_search.ui_state());
    state.message_actions = message_actions_ui_state(interaction_state, &message_action_items);
    state.prompt_history_search = interaction_state
        .prompt_history_search
        .as_ref()
        .map(ReplPromptHistorySearchState::ui_state);
    state.prompt_selection = interaction_state.prompt_selection.clone();
    state.transcript_selection = interaction_state.transcript_selection.clone();
    state.choice_list = choice_list;
    state.compact_banner = compact_banner;
    let command_suggestions = if interaction_state.prompt_history_search.is_some() {
        Vec::new()
    } else {
        command_suggestions
    };
    state.command_suggestions = command_suggestions;
    state.selected_command_suggestion = if state.command_suggestions.is_empty() {
        None
    } else {
        Some(selected_command_suggestion.min(state.command_suggestions.len() - 1))
    };
    state.status_marquee_tick = status_marquee_tick;
    state.task_items = task_items;
    state.question_items = question_items;
    state.task_preview =
        pending_task_preview(pending_view).unwrap_or_else(|| recent_task_preview(cwd));
    state.file_preview =
        preview_for_last_file_message(&runtime_messages, cwd).unwrap_or(PanePreview {
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

fn draw_repl_state(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    registry: &code_agent_core::CommandRegistry,
    raw_messages: &[Message],
    pending_view: Option<&PendingReplView>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    input_buffer: &code_agent_ui::InputBuffer,
    status_line: &str,
    progress_message: Option<String>,
    active_pane: PaneKind,
    compact_banner: Option<String>,
    transcript_scroll: u16,
    choice_list: Option<ChoiceListState>,
    selected_command_suggestion: &mut usize,
    vim_state: &code_agent_ui::vim::VimState,
    status_marquee_tick: usize,
    interaction_state: &ReplInteractionState,
) -> Result<()> {
    let suggestions = sync_command_selection(registry, input_buffer, selected_command_suggestion);
    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
    let mut state = build_repl_ui_state(
        &app,
        registry,
        raw_messages,
        pending_view,
        cwd,
        provider,
        active_model,
        session_id,
        input_buffer,
        status_line,
        progress_message,
        active_pane,
        compact_banner,
        transcript_scroll,
        choice_list,
        suggestions,
        *selected_command_suggestion,
        status_marquee_tick,
        interaction_state,
    );
    state.vim_state = vim_state.clone();
    draw_tui(terminal, &state)
}

fn repl_mouse_action(
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    registry: &code_agent_core::CommandRegistry,
    raw_messages: &[Message],
    pending_view: Option<&PendingReplView>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    input_buffer: &code_agent_ui::InputBuffer,
    status_line: &str,
    progress_message: Option<String>,
    active_pane: PaneKind,
    compact_banner: Option<String>,
    transcript_scroll: u16,
    choice_list: Option<ChoiceListState>,
    selected_command_suggestion: usize,
    status_marquee_tick: usize,
    mouse: &MouseEvent,
    interaction_state: &ReplInteractionState,
) -> Result<Option<UiMouseAction>> {
    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
    let state = build_repl_ui_state(
        &app,
        registry,
        raw_messages,
        pending_view,
        cwd,
        provider,
        active_model,
        session_id,
        input_buffer,
        status_line,
        progress_message,
        active_pane,
        compact_banner,
        transcript_scroll,
        choice_list,
        command_suggestions(registry, input_buffer),
        selected_command_suggestion,
        status_marquee_tick,
        interaction_state,
    );
    let size = terminal.size()?;
    Ok(mouse_action_for_position(
        &state,
        size.width,
        size.height,
        mouse.column,
        mouse.row,
    ))
}

fn optimistic_messages_for_prompt(
    raw_messages: &[Message],
    session_id: SessionId,
    prompt_text: &str,
) -> Vec<Message> {
    let mut preview_messages = raw_messages.to_vec();
    let parent_id = raw_messages.last().map(|message| message.id);
    preview_messages.push(build_text_message(
        session_id,
        MessageRole::User,
        prompt_text.to_owned(),
        parent_id,
    ));
    preview_messages
}

#[derive(Clone, Debug)]
struct PendingReplStep {
    step: usize,
    start_index: usize,
    status_label: String,
    status_detail: Option<String>,
    task_status: TaskStatus,
    expanded: bool,
    touched: bool,
}

impl PendingReplStep {
    fn id(&self) -> String {
        format!("pending-step-{}", self.step)
    }
}

#[derive(Clone, Debug)]
struct PendingReplView {
    messages: Vec<Message>,
    spinner_verb: String,
    progress_label: String,
    steps: Vec<PendingReplStep>,
    queued_inputs: Vec<String>,
    show_transcript_details: bool,
}

impl PendingReplView {
    fn new(messages: Vec<Message>, progress_label: impl Into<String>) -> Self {
        let progress_label = progress_label.into();
        Self {
            messages,
            spinner_verb: pending_spinner_verb(&progress_label),
            progress_label,
            steps: Vec::new(),
            queued_inputs: Vec::new(),
            show_transcript_details: false,
        }
    }
}

fn update_pending_repl_view(
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
    messages: &[Message],
    progress_label: impl Into<String>,
) {
    let Some(pending_view) = pending_view else {
        return;
    };
    if let Ok(mut state) = pending_view.lock() {
        state.messages = materialize_runtime_messages(messages);
        state.progress_label = progress_label.into();
    }
}

fn update_pending_repl_step_view(
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
    step: usize,
    step_start_index: usize,
    messages: &[Message],
    progress_label: impl Into<String>,
    status_detail: Option<String>,
    task_status: TaskStatus,
) {
    let Some(pending_view) = pending_view else {
        return;
    };
    if let Ok(mut state) = pending_view.lock() {
        let runtime_messages = materialize_runtime_messages(messages);
        let runtime_start_index = step_start_index.min(runtime_messages.len());
        let progress_label = progress_label.into();
        if !state.steps.iter().any(|entry| entry.step == step) {
            if let Some(previous) = state.steps.last_mut() {
                if !previous.touched {
                    previous.expanded = false;
                }
            }
            state.steps.push(PendingReplStep {
                step,
                start_index: runtime_start_index,
                status_label: progress_label.clone(),
                status_detail: None,
                task_status: task_status.clone(),
                expanded: true,
                touched: false,
            });
        }
        if let Some(entry) = state.steps.iter_mut().find(|entry| entry.step == step) {
            entry.start_index = runtime_start_index.min(runtime_messages.len());
            entry.status_label = progress_label;
            entry.status_detail = status_detail;
            entry.task_status = task_status;
        }
        state.messages = runtime_messages;
        state.progress_label = state
            .steps
            .iter()
            .find(|entry| entry.step == step)
            .map(|entry| {
                compose_pending_progress_label(&entry.status_label, entry.status_detail.as_deref())
            })
            .unwrap_or_else(|| "working".to_owned());
    }
}

fn queue_pending_repl_input(pending_view: &Arc<Mutex<PendingReplView>>, prompt_text: String) {
    if let Ok(mut state) = pending_view.lock() {
        state.queued_inputs.push(prompt_text);
    }
}

fn take_pending_repl_inputs(pending_view: &Arc<Mutex<PendingReplView>>) -> Vec<String> {
    pending_view
        .lock()
        .map(|mut state| mem::take(&mut state.queued_inputs))
        .unwrap_or_default()
}

fn toggle_pending_repl_group(pending_view: &Arc<Mutex<PendingReplView>>, group_id: &str) {
    if let Ok(mut state) = pending_view.lock() {
        if let Some(entry) = state.steps.iter_mut().find(|entry| entry.id() == group_id) {
            entry.expanded = !entry.expanded;
            entry.touched = true;
        }
    }
}

fn toggle_pending_repl_transcript_details(pending_view: &Arc<Mutex<PendingReplView>>) {
    if let Ok(mut state) = pending_view.lock() {
        state.show_transcript_details = !state.show_transcript_details;
    }
}

fn pending_repl_snapshot(pending_view: &Arc<Mutex<PendingReplView>>) -> PendingReplView {
    pending_view
        .lock()
        .map(|state| state.clone())
        .unwrap_or_else(|_| PendingReplView::new(Vec::new(), "working"))
}

fn pending_interrupt_messages(
    session_id: SessionId,
    raw_messages: &[Message],
    pending_view: &PendingReplView,
) -> Vec<Message> {
    let mut interrupt_messages = pending_view
        .messages
        .iter()
        .filter(|message| {
            raw_messages
                .iter()
                .all(|existing| existing.id != message.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let parent_id = interrupt_messages
        .last()
        .map(|message| message.id)
        .or_else(|| raw_messages.last().map(|message| message.id));
    interrupt_messages.push(build_user_interruption_message(session_id, parent_id));
    interrupt_messages
}

fn provider_assistant_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    text: String,
    tool_calls: Vec<code_agent_core::ToolCall>,
    provider: ApiProvider,
    model: &str,
    usage: Option<code_agent_core::TokenUsage>,
) -> Message {
    let mut assistant_message = build_assistant_message(session_id, parent_id, text, tool_calls);
    assistant_message.metadata.provider = Some(provider.to_string());
    assistant_message.metadata.model = Some(model.to_owned());
    assistant_message.metadata.usage = usage;
    assistant_message
}

fn pending_spinner_frame(tick: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
    FRAMES[tick % FRAMES.len()]
}

async fn run_pending_repl_operation<F, T>(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    registry: &code_agent_core::CommandRegistry,
    pending_view: Arc<Mutex<PendingReplView>>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    input_buffer: &mut code_agent_ui::InputBuffer,
    status_line: &str,
    active_pane: &mut PaneKind,
    compact_banner: Option<String>,
    transcript_scroll: &mut u16,
    selected_command_suggestion: &mut usize,
    vim_state: &mut code_agent_ui::vim::VimState,
    interaction_state: &mut ReplInteractionState,
    operation: F,
) -> Result<PendingReplOperationResult<T>>
where
    F: Future<Output = Result<T>>,
{
    let mut operation = std::pin::pin!(operation);
    let mut tick = 0usize;
    let mut compact_banner = compact_banner;

    loop {
        let pending_snapshot = pending_repl_snapshot(&pending_view);
        while event::poll(Duration::from_millis(0))? {
            match event::read()? {
                Event::Resize(width, height) => {
                    terminal.resize(Rect::new(0, 0, width, height))?;
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        clear_prompt_mouse_anchor(interaction_state);
                        interaction_state.transcript_selection = None;
                        scroll_up(transcript_scroll, 3);
                    }
                    MouseEventKind::ScrollDown => {
                        clear_prompt_mouse_anchor(interaction_state);
                        interaction_state.transcript_selection = None;
                        scroll_down(transcript_scroll, 3);
                    }
                    MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left) => {
                        if let Some(action) = repl_mouse_action(
                            terminal,
                            registry,
                            &pending_snapshot.messages,
                            Some(&pending_snapshot),
                            cwd,
                            provider,
                            active_model,
                            session_id,
                            input_buffer,
                            status_line,
                            Some(format!(
                                "{} {}",
                                pending_spinner_frame(tick),
                                pending_snapshot.progress_label
                            )),
                            *active_pane,
                            compact_banner.clone(),
                            *transcript_scroll,
                            None,
                            *selected_command_suggestion,
                            tick,
                            &mouse,
                            interaction_state,
                        )? {
                            match action {
                                UiMouseAction::JumpToBottom
                                    if matches!(
                                        mouse.kind,
                                        MouseEventKind::Down(MouseButton::Left)
                                    ) =>
                                {
                                    clear_prompt_mouse_anchor(interaction_state);
                                    *transcript_scroll = 0;
                                }
                                UiMouseAction::ToggleTranscriptGroup(group_id)
                                    if matches!(
                                        mouse.kind,
                                        MouseEventKind::Down(MouseButton::Left)
                                    ) =>
                                {
                                    clear_prompt_mouse_anchor(interaction_state);
                                    toggle_pending_repl_group(&pending_view, &group_id);
                                }
                                UiMouseAction::SetPromptCursor(cursor) => {
                                    let _ = handle_prompt_mouse_action(
                                        &mouse.kind,
                                        cursor,
                                        interaction_state,
                                        input_buffer,
                                    );
                                }
                                _ => {}
                            }
                        } else if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
                            clear_prompt_mouse_anchor(interaction_state);
                        }
                    }
                    _ => {}
                },
                Event::Paste(text) => {
                    clear_prompt_mouse_anchor(interaction_state);
                    if let Some(search_state) = interaction_state.prompt_history_search.as_mut() {
                        let _ = insert_buffer_text(&mut search_state.input_buffer, &text);
                        sync_prompt_history_search_preview(
                            &prompt_history_from_messages(&pending_snapshot.messages),
                            search_state,
                            input_buffer,
                        );
                    } else if interaction_state.transcript_search.open {
                        let input = &mut interaction_state.transcript_search.input_buffer;
                        let _ = insert_buffer_text(input, &text);
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            &pending_snapshot.messages,
                            Some(&pending_snapshot),
                            cwd,
                            provider,
                            active_model,
                            session_id,
                            input_buffer,
                            status_line,
                            Some(format!(
                                "{} {}",
                                pending_spinner_frame(tick),
                                pending_snapshot.progress_label
                            )),
                            *active_pane,
                            compact_banner.clone(),
                            *transcript_scroll,
                            None,
                            command_suggestions(registry, input_buffer),
                            *selected_command_suggestion,
                            tick,
                            interaction_state,
                        );
                        let size = terminal.size()?;
                        sync_transcript_search_preview(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            transcript_scroll,
                        );
                    } else if !interaction_state.transcript_mode && vim_state.is_insert() {
                        if insert_prompt_text(interaction_state, input_buffer, &text) {
                            *selected_command_suggestion = 0;
                        }
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    clear_prompt_mouse_anchor(interaction_state);
                    if is_paste_shortcut(&key) {
                        if let Some(search_state) = interaction_state.prompt_history_search.as_mut()
                        {
                            if let Some(text) = read_text_from_clipboard() {
                                let _ = insert_buffer_text(&mut search_state.input_buffer, &text);
                                sync_prompt_history_search_preview(
                                    &prompt_history_from_messages(&pending_snapshot.messages),
                                    search_state,
                                    input_buffer,
                                );
                            }
                            continue;
                        }

                        if interaction_state.transcript_search.open {
                            if let Some(text) = read_text_from_clipboard() {
                                let input = &mut interaction_state.transcript_search.input_buffer;
                                let _ = insert_buffer_text(input, &text);
                                let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                                let state = build_repl_ui_state(
                                    &app,
                                    registry,
                                    &pending_snapshot.messages,
                                    Some(&pending_snapshot),
                                    cwd,
                                    provider,
                                    active_model,
                                    session_id,
                                    input_buffer,
                                    status_line,
                                    Some(format!(
                                        "{} {}",
                                        pending_spinner_frame(tick),
                                        pending_snapshot.progress_label
                                    )),
                                    *active_pane,
                                    compact_banner.clone(),
                                    *transcript_scroll,
                                    None,
                                    command_suggestions(registry, input_buffer),
                                    *selected_command_suggestion,
                                    tick,
                                    interaction_state,
                                );
                                let size = terminal.size()?;
                                sync_transcript_search_preview(
                                    &state,
                                    size.width,
                                    size.height,
                                    &mut interaction_state.transcript_search,
                                    transcript_scroll,
                                );
                            }
                            continue;
                        }

                        if !interaction_state.transcript_mode && vim_state.is_insert() {
                            if let Some(text) = read_text_from_clipboard() {
                                if insert_prompt_text(interaction_state, input_buffer, &text) {
                                    *selected_command_suggestion = 0;
                                }
                            }
                            continue;
                        }
                    }
                    if let Some(shortcut) = repl_shortcut_action_for_key(&key, interaction_state) {
                        match shortcut {
                            ReplShortcutAction::CopySelection => {
                                let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                                let state = build_repl_ui_state(
                                    &app,
                                    registry,
                                    &pending_snapshot.messages,
                                    Some(&pending_snapshot),
                                    cwd,
                                    provider,
                                    active_model,
                                    session_id,
                                    input_buffer,
                                    status_line,
                                    Some(format!(
                                        "{} {}",
                                        pending_spinner_frame(tick),
                                        pending_snapshot.progress_label
                                    )),
                                    *active_pane,
                                    compact_banner.clone(),
                                    *transcript_scroll,
                                    None,
                                    command_suggestions(registry, input_buffer),
                                    *selected_command_suggestion,
                                    tick,
                                    interaction_state,
                                );
                                let size = terminal.size()?;
                                if let Some(text) = repl_selection_copy_text(
                                    &state,
                                    size.width,
                                    input_buffer,
                                    interaction_state,
                                ) {
                                    compact_banner = Some(
                                        copy_text_with_fallback_notice(&text, "selection")
                                            .unwrap_or_else(|error| {
                                                format!("Copy failed: {error}")
                                            }),
                                    );
                                }
                                clear_prompt_selection(interaction_state);
                                interaction_state.transcript_selection = None;
                            }
                            ReplShortcutAction::ContextCtrlC => {
                                if cancel_prompt_history_search(interaction_state, input_buffer) {
                                    continue;
                                }
                                if interaction_state.transcript_search.open {
                                    cancel_transcript_search(
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                    );
                                    continue;
                                }
                                if interaction_state.message_actions.is_some() {
                                    interaction_state.message_actions = None;
                                    continue;
                                }
                                if interaction_state.transcript_selection.is_some()
                                    || interaction_state.prompt_selection.is_some()
                                {
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    if let Some(text) = repl_selection_copy_text(
                                        &state,
                                        size.width,
                                        input_buffer,
                                        interaction_state,
                                    ) {
                                        compact_banner = Some(
                                            copy_text_with_fallback_notice(&text, "selection")
                                                .unwrap_or_else(|error| {
                                                    format!("Copy failed: {error}")
                                                }),
                                        );
                                    }
                                    clear_prompt_selection(interaction_state);
                                    interaction_state.transcript_selection = None;
                                    continue;
                                }
                                if interaction_state.transcript_mode {
                                    exit_transcript_mode(interaction_state);
                                    continue;
                                }
                                return Ok(PendingReplOperationResult::Interrupted);
                            }
                            ReplShortcutAction::ToggleTranscriptMode => {
                                if interaction_state.transcript_mode {
                                    exit_transcript_mode(interaction_state);
                                } else {
                                    enter_transcript_mode(interaction_state, active_pane);
                                }
                            }
                            ReplShortcutAction::PromptHistorySearch => {
                                if let Some(search_state) =
                                    interaction_state.prompt_history_search.as_mut()
                                {
                                    let _ = step_prompt_history_search_match(
                                        &prompt_history_from_messages(&pending_snapshot.messages),
                                        search_state,
                                        input_buffer,
                                    );
                                } else {
                                    open_prompt_history_search(interaction_state, input_buffer);
                                }
                            }
                            ReplShortcutAction::EnterMessageActions => {
                                let message_action_items = message_action_items_from_runtime(
                                    &pending_snapshot.messages,
                                    Some(&pending_snapshot),
                                );
                                if enter_message_actions(interaction_state, &message_action_items) {
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    sync_message_action_preview(
                                        &state,
                                        size.width,
                                        size.height,
                                        interaction_state,
                                        transcript_scroll,
                                    );
                                }
                            }
                            ReplShortcutAction::SelectPane(pane) => {
                                *active_pane = pane;
                            }
                            ReplShortcutAction::RotatePaneForward => {
                                *active_pane = rotate_pane(*active_pane, true);
                            }
                            ReplShortcutAction::RotatePaneBackward => {
                                *active_pane = rotate_pane(*active_pane, false);
                            }
                        }
                        continue;
                    }

                    if interaction_state.message_actions.is_some() {
                        let message_action_items = message_action_items_from_runtime(
                            &pending_snapshot.messages,
                            Some(&pending_snapshot),
                        );
                        if selected_message_action_item(interaction_state, &message_action_items)
                            .is_none()
                        {
                            interaction_state.message_actions = None;
                            continue;
                        }

                        let mut selection_changed = false;
                        match key.code {
                            KeyCode::Esc => {
                                interaction_state.message_actions = None;
                            }
                            KeyCode::Enter => {
                                let prompt_text = selected_message_action_item(
                                    interaction_state,
                                    &message_action_items,
                                )
                                .and_then(|item| {
                                    (item.message.role == MessageRole::User)
                                        .then(|| message_text(&item.message))
                                });
                                if let Some(prompt_text) =
                                    prompt_text.filter(|text| !text.trim().is_empty())
                                {
                                    input_buffer.replace(prompt_text);
                                    clear_prompt_selection(interaction_state);
                                    interaction_state.message_actions = None;
                                    if interaction_state.transcript_mode {
                                        exit_transcript_mode(interaction_state);
                                    }
                                }
                            }
                            KeyCode::Char('c') if key.modifiers.is_empty() => {
                                if let Some(text) = selected_message_action_item(
                                    interaction_state,
                                    &message_action_items,
                                )
                                .and_then(|item| message_action_copy_text(&item.message))
                                {
                                    compact_banner = Some(
                                        copy_text_with_fallback_notice(&text, "message")
                                            .unwrap_or_else(|error| {
                                                format!("Copy failed: {error}")
                                            }),
                                    );
                                }
                                interaction_state.message_actions = None;
                            }
                            KeyCode::Char('p') if key.modifiers.is_empty() => {
                                if let Some(primary_input) = selected_message_action_item(
                                    interaction_state,
                                    &message_action_items,
                                )
                                .and_then(|item| message_primary_input(&item.message))
                                {
                                    compact_banner = Some(
                                        copy_text_with_fallback_notice(
                                            &primary_input.value,
                                            primary_input.label,
                                        )
                                        .unwrap_or_else(|error| format!("Copy failed: {error}")),
                                    );
                                }
                                interaction_state.message_actions = None;
                            }
                            KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::PrevUser,
                                );
                            }
                            KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::NextUser,
                                );
                            }
                            KeyCode::Up => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Prev,
                                );
                            }
                            KeyCode::Down => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Next,
                                );
                            }
                            KeyCode::Char('k') if key.modifiers.is_empty() => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Prev,
                                );
                            }
                            KeyCode::Char('j') if key.modifiers.is_empty() => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Next,
                                );
                            }
                            KeyCode::Home => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Top,
                                );
                            }
                            KeyCode::End => {
                                selection_changed = move_message_action_selection(
                                    interaction_state,
                                    &message_action_items,
                                    ReplMessageActionNavigation::Bottom,
                                );
                            }
                            _ => {}
                        }

                        if selection_changed {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                &pending_snapshot.messages,
                                Some(&pending_snapshot),
                                cwd,
                                provider,
                                active_model,
                                session_id,
                                input_buffer,
                                status_line,
                                Some(format!(
                                    "{} {}",
                                    pending_spinner_frame(tick),
                                    pending_snapshot.progress_label
                                )),
                                *active_pane,
                                compact_banner.clone(),
                                *transcript_scroll,
                                None,
                                command_suggestions(registry, input_buffer),
                                *selected_command_suggestion,
                                tick,
                                interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_message_action_preview(
                                &state,
                                size.width,
                                size.height,
                                interaction_state,
                                transcript_scroll,
                            );
                        }

                        continue;
                    }
                    if is_plain_ctrl_char(&key, 'e') {
                        toggle_pending_repl_transcript_details(&pending_view);
                        continue;
                    }

                    if interaction_state.prompt_history_search.is_some() {
                        match key.code {
                            KeyCode::Esc => {
                                let _ =
                                    cancel_prompt_history_search(interaction_state, input_buffer);
                            }
                            KeyCode::Enter => {
                                let _ = accept_prompt_history_search(interaction_state);
                            }
                            KeyCode::Left => {
                                let search_state = interaction_state
                                    .prompt_history_search
                                    .as_mut()
                                    .expect("prompt history search state should exist");
                                search_state.input_buffer.cursor =
                                    search_state.input_buffer.cursor.saturating_sub(1);
                            }
                            KeyCode::Right => {
                                let search_state = interaction_state
                                    .prompt_history_search
                                    .as_mut()
                                    .expect("prompt history search state should exist");
                                search_state.input_buffer.cursor =
                                    (search_state.input_buffer.cursor + 1)
                                        .min(search_state.input_buffer.chars.len());
                            }
                            KeyCode::Home => {
                                let search_state = interaction_state
                                    .prompt_history_search
                                    .as_mut()
                                    .expect("prompt history search state should exist");
                                search_state.input_buffer.cursor = 0;
                            }
                            KeyCode::End => {
                                let search_state = interaction_state
                                    .prompt_history_search
                                    .as_mut()
                                    .expect("prompt history search state should exist");
                                search_state.input_buffer.cursor =
                                    search_state.input_buffer.chars.len();
                            }
                            KeyCode::Backspace => {
                                let should_cancel = interaction_state
                                    .prompt_history_search
                                    .as_ref()
                                    .is_some_and(|search_state| {
                                        search_state.input_buffer.is_empty()
                                    });
                                if should_cancel {
                                    let _ = cancel_prompt_history_search(
                                        interaction_state,
                                        input_buffer,
                                    );
                                } else {
                                    let search_state = interaction_state
                                        .prompt_history_search
                                        .as_mut()
                                        .expect("prompt history search state should exist");
                                    search_state.input_buffer.pop();
                                    sync_prompt_history_search_preview(
                                        &prompt_history_from_messages(&pending_snapshot.messages),
                                        search_state,
                                        input_buffer,
                                    );
                                }
                            }
                            KeyCode::Char(ch)
                                if key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                let search_state = interaction_state
                                    .prompt_history_search
                                    .as_mut()
                                    .expect("prompt history search state should exist");
                                search_state.input_buffer.push(ch);
                                sync_prompt_history_search_preview(
                                    &prompt_history_from_messages(&pending_snapshot.messages),
                                    search_state,
                                    input_buffer,
                                );
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if interaction_state.transcript_mode {
                        if interaction_state.transcript_search.open {
                            match key.code {
                                KeyCode::Esc => {
                                    cancel_transcript_search(
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                    );
                                }
                                KeyCode::Enter => {
                                    interaction_state.transcript_search.open = false;
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    sync_transcript_search_preview(
                                        &state,
                                        size.width,
                                        size.height,
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                    );
                                    if interaction_state.transcript_search.active_item.is_none() {
                                        interaction_state.transcript_search.reset();
                                    }
                                }
                                KeyCode::Left => {
                                    interaction_state.transcript_search.input_buffer.cursor =
                                        interaction_state
                                            .transcript_search
                                            .input_buffer
                                            .cursor
                                            .saturating_sub(1);
                                }
                                KeyCode::Right => {
                                    let input =
                                        &mut interaction_state.transcript_search.input_buffer;
                                    input.cursor = (input.cursor + 1).min(input.chars.len());
                                }
                                KeyCode::Backspace => {
                                    interaction_state.transcript_search.input_buffer.pop();
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    sync_transcript_search_preview(
                                        &state,
                                        size.width,
                                        size.height,
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                    );
                                }
                                KeyCode::Char('n') if key.modifiers.is_empty() => {
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    let _ = step_transcript_search_match(
                                        &state,
                                        size.width,
                                        size.height,
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                        false,
                                    );
                                }
                                KeyCode::Char('N')
                                    if key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT =>
                                {
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    let _ = step_transcript_search_match(
                                        &state,
                                        size.width,
                                        size.height,
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                        true,
                                    );
                                }
                                KeyCode::Char(ch)
                                    if key.modifiers.is_empty()
                                        || key.modifiers == KeyModifiers::SHIFT =>
                                {
                                    interaction_state.transcript_search.input_buffer.push(ch);
                                    let app =
                                        RatatuiApp::new(format!("{provider}  {active_model}"));
                                    let state = build_repl_ui_state(
                                        &app,
                                        registry,
                                        &pending_snapshot.messages,
                                        Some(&pending_snapshot),
                                        cwd,
                                        provider,
                                        active_model,
                                        session_id,
                                        input_buffer,
                                        status_line,
                                        Some(format!(
                                            "{} {}",
                                            pending_spinner_frame(tick),
                                            pending_snapshot.progress_label
                                        )),
                                        *active_pane,
                                        compact_banner.clone(),
                                        *transcript_scroll,
                                        None,
                                        command_suggestions(registry, input_buffer),
                                        *selected_command_suggestion,
                                        tick,
                                        interaction_state,
                                    );
                                    let size = terminal.size()?;
                                    sync_transcript_search_preview(
                                        &state,
                                        size.width,
                                        size.height,
                                        &mut interaction_state.transcript_search,
                                        transcript_scroll,
                                    );
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if let Some(selection_move) = transcript_selection_move_for_key(
                            &key,
                            interaction_state.transcript_selection.is_some(),
                        ) {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                &pending_snapshot.messages,
                                Some(&pending_snapshot),
                                cwd,
                                provider,
                                active_model,
                                session_id,
                                input_buffer,
                                status_line,
                                Some(format!(
                                    "{} {}",
                                    pending_spinner_frame(tick),
                                    pending_snapshot.progress_label
                                )),
                                *active_pane,
                                compact_banner.clone(),
                                *transcript_scroll,
                                None,
                                command_suggestions(registry, input_buffer),
                                *selected_command_suggestion,
                                tick,
                                interaction_state,
                            );
                            let size = terminal.size()?;
                            let selectable_lines =
                                transcript_selectable_lines_for_view(&state, size.width);
                            let _ = move_transcript_selection(
                                interaction_state,
                                &selectable_lines,
                                selection_move,
                            );
                            sync_transcript_selection_preview(
                                &state,
                                size.width,
                                size.height,
                                interaction_state,
                                transcript_scroll,
                            );
                            continue;
                        }

                        if interaction_state.transcript_selection.is_some() {
                            if matches!(key.code, KeyCode::Esc) {
                                interaction_state.transcript_selection = None;
                                continue;
                            }
                            if should_clear_transcript_selection_on_key(&key) {
                                interaction_state.transcript_selection = None;
                            }
                        }

                        match key.code {
                            KeyCode::Esc => {
                                exit_transcript_mode(interaction_state);
                            }
                            KeyCode::Char('q') if key.modifiers.is_empty() => {
                                exit_transcript_mode(interaction_state);
                            }
                            KeyCode::Char('/')
                                if key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                interaction_state.message_actions = None;
                                open_transcript_search(
                                    &mut interaction_state.transcript_search,
                                    *transcript_scroll,
                                );
                            }
                            KeyCode::Char('n') if key.modifiers.is_empty() => {
                                let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                                let state = build_repl_ui_state(
                                    &app,
                                    registry,
                                    &pending_snapshot.messages,
                                    Some(&pending_snapshot),
                                    cwd,
                                    provider,
                                    active_model,
                                    session_id,
                                    input_buffer,
                                    status_line,
                                    Some(format!(
                                        "{} {}",
                                        pending_spinner_frame(tick),
                                        pending_snapshot.progress_label
                                    )),
                                    *active_pane,
                                    compact_banner.clone(),
                                    *transcript_scroll,
                                    None,
                                    command_suggestions(registry, input_buffer),
                                    *selected_command_suggestion,
                                    tick,
                                    interaction_state,
                                );
                                let size = terminal.size()?;
                                let _ = step_transcript_search_match(
                                    &state,
                                    size.width,
                                    size.height,
                                    &mut interaction_state.transcript_search,
                                    transcript_scroll,
                                    false,
                                );
                            }
                            KeyCode::Char('N')
                                if key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                                let state = build_repl_ui_state(
                                    &app,
                                    registry,
                                    &pending_snapshot.messages,
                                    Some(&pending_snapshot),
                                    cwd,
                                    provider,
                                    active_model,
                                    session_id,
                                    input_buffer,
                                    status_line,
                                    Some(format!(
                                        "{} {}",
                                        pending_spinner_frame(tick),
                                        pending_snapshot.progress_label
                                    )),
                                    *active_pane,
                                    compact_banner.clone(),
                                    *transcript_scroll,
                                    None,
                                    command_suggestions(registry, input_buffer),
                                    *selected_command_suggestion,
                                    tick,
                                    interaction_state,
                                );
                                let size = terminal.size()?;
                                let _ = step_transcript_search_match(
                                    &state,
                                    size.width,
                                    size.height,
                                    &mut interaction_state.transcript_search,
                                    transcript_scroll,
                                    true,
                                );
                            }
                            KeyCode::Up => scroll_up(transcript_scroll, 1),
                            KeyCode::Down => scroll_down(transcript_scroll, 1),
                            KeyCode::PageUp => scroll_up(transcript_scroll, 5),
                            KeyCode::PageDown => scroll_down(transcript_scroll, 5),
                            KeyCode::Home => *transcript_scroll = u16::MAX,
                            KeyCode::End => *transcript_scroll = 0,
                            _ => {}
                        }
                        continue;
                    }

                    if vim_state.is_insert() {
                        if let Some(selection_move) = prompt_selection_move_for_key(&key) {
                            let _ = move_prompt_selection(
                                interaction_state,
                                input_buffer,
                                selection_move,
                            );
                            continue;
                        }
                    }

                    if interaction_state.prompt_selection.is_some()
                        && matches!(key.code, KeyCode::Esc)
                    {
                        clear_prompt_selection(interaction_state);
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc if vim_state.enabled => {
                            if matches!(vim_state.mode, code_agent_ui::vim::VimMode::Insert) {
                                vim_state.enter_normal();
                            } else {
                                vim_state.mode = code_agent_ui::vim::VimMode::Normal(
                                    code_agent_ui::vim::CommandState::Idle,
                                );
                            }
                        }
                        KeyCode::Up => {
                            clear_prompt_selection(interaction_state);
                            let suggestions = sync_command_selection(
                                registry,
                                input_buffer,
                                selected_command_suggestion,
                            );
                            if !input_buffer.is_empty() && suggestions.len() > 1 {
                                *selected_command_suggestion = if *selected_command_suggestion == 0
                                {
                                    suggestions.len() - 1
                                } else {
                                    *selected_command_suggestion - 1
                                };
                            } else {
                                scroll_up(transcript_scroll, 1);
                            }
                        }
                        KeyCode::Down => {
                            clear_prompt_selection(interaction_state);
                            let suggestions = sync_command_selection(
                                registry,
                                input_buffer,
                                selected_command_suggestion,
                            );
                            if !input_buffer.is_empty() && suggestions.len() > 1 {
                                *selected_command_suggestion =
                                    (*selected_command_suggestion + 1) % suggestions.len();
                            } else {
                                scroll_down(transcript_scroll, 1);
                            }
                        }
                        KeyCode::PageUp => scroll_up(transcript_scroll, 5),
                        KeyCode::PageDown => scroll_down(transcript_scroll, 5),
                        KeyCode::Home if vim_state.is_insert() => {
                            let _ = set_prompt_cursor(interaction_state, input_buffer, 0);
                        }
                        KeyCode::End if vim_state.is_insert() => {
                            let end_cursor = input_buffer.chars.len();
                            let _ = set_prompt_cursor(interaction_state, input_buffer, end_cursor);
                        }
                        KeyCode::Home => *transcript_scroll = u16::MAX,
                        KeyCode::End => *transcript_scroll = 0,
                        KeyCode::Left if vim_state.is_insert() => {
                            if let Some((start, _)) = interaction_state
                                .prompt_selection
                                .as_ref()
                                .and_then(|selection| {
                                    normalize_prompt_selection(selection, input_buffer.chars.len())
                                })
                            {
                                let _ = set_prompt_cursor(interaction_state, input_buffer, start);
                            } else {
                                let next_cursor = input_buffer.cursor.saturating_sub(1);
                                let _ =
                                    set_prompt_cursor(interaction_state, input_buffer, next_cursor);
                            }
                        }
                        KeyCode::Right if vim_state.is_insert() => {
                            if let Some((_, end)) = interaction_state
                                .prompt_selection
                                .as_ref()
                                .and_then(|selection| {
                                    normalize_prompt_selection(selection, input_buffer.chars.len())
                                })
                            {
                                let _ = set_prompt_cursor(interaction_state, input_buffer, end);
                            } else {
                                let next_cursor =
                                    (input_buffer.cursor + 1).min(input_buffer.chars.len());
                                let _ =
                                    set_prompt_cursor(interaction_state, input_buffer, next_cursor);
                            }
                        }
                        KeyCode::Backspace if vim_state.is_insert() => {
                            if !delete_prompt_selection(interaction_state, input_buffer) {
                                input_buffer.pop();
                            }
                            *selected_command_suggestion = 0;
                        }
                        KeyCode::Char(ch)
                            if vim_state.is_insert()
                                && (key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT) =>
                        {
                            let _ = delete_prompt_selection(interaction_state, input_buffer);
                            input_buffer.push(ch);
                            *selected_command_suggestion = 0;
                        }
                        KeyCode::Enter => {
                            let suggestions = sync_command_selection(
                                registry,
                                input_buffer,
                                selected_command_suggestion,
                            );
                            let prompt_text = input_buffer.as_str().trim().to_owned();
                            if prompt_text.is_empty() {
                                continue;
                            }
                            if let Some(selected) = suggestions.get(*selected_command_suggestion) {
                                let selected_name = selected.name.as_str();
                                if prompt_text.starts_with('/')
                                    && !prompt_text.contains(char::is_whitespace)
                                    && prompt_text != selected_name
                                {
                                    clear_prompt_selection(interaction_state);
                                    apply_selected_command(input_buffer, selected);
                                    continue;
                                }
                            }
                            queue_pending_repl_input(&pending_view, prompt_text);
                            clear_prompt_selection(interaction_state);
                            input_buffer.clear();
                            *selected_command_suggestion = 0;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let snapshot = pending_repl_snapshot(&pending_view);
        draw_repl_state(
            terminal,
            registry,
            &snapshot.messages,
            Some(&snapshot),
            cwd,
            provider,
            active_model,
            session_id,
            input_buffer,
            status_line,
            Some(format!(
                "{} {}",
                pending_spinner_frame(tick),
                snapshot.progress_label
            )),
            *active_pane,
            compact_banner.clone(),
            *transcript_scroll,
            None,
            selected_command_suggestion,
            vim_state,
            tick,
            interaction_state,
        )?;

        tokio::select! {
            result = &mut operation => return result.map(PendingReplOperationResult::Completed),
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                tick = tick.wrapping_add(1);
            }
        }
    }
}

enum PendingReplOperationResult<T> {
    Completed(T),
    Interrupted,
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
            None,
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
                None,
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
            None,
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
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<code_agent_core::TokenUsage>, usize, Option<String>)> {
    const MAX_AGENT_STEPS: usize = 100;

    let provider_tools = tool_definitions(tool_registry);
    let tool_context = ToolContext {
        session_id: Some(session_id),
        cwd: cwd.clone(),
        provider: Some(provider.to_string()),
        model: Some(model.clone()),
        ..ToolContext::default()
    };

    for step in 1..=MAX_AGENT_STEPS {
        let step_start_index = messages.len();
        update_pending_repl_step_view(
            pending_view,
            step,
            step_start_index,
            messages,
            format!("Waiting for response · step {step}"),
            None,
            TaskStatus::Running,
        );
        let provider_client = resolve_provider_client(provider, auth_configured).await?;
        let parent_id = messages.last().map(|message| message.id);
        let mut stream = provider_client
            .start_stream(ProviderRequest {
                model: model.clone(),
                messages: messages.clone(),
                tools: provider_tools.clone(),
                ..ProviderRequest::default()
            })
            .await?;
        let mut response_text = String::new();
        let mut response_tool_calls = Vec::new();
        let mut latest_usage = None;
        let mut stop_reason = None;

        while let Some(event) = stream.next_event().await? {
            match event {
                ProviderEvent::MessageDelta { text } => {
                    response_text.push_str(&text);
                    let preview_message = provider_assistant_message(
                        session_id,
                        parent_id,
                        response_text.clone(),
                        response_tool_calls.clone(),
                        provider,
                        &model,
                        latest_usage.clone(),
                    );
                    let mut preview_messages = messages.clone();
                    preview_messages.push(preview_message);
                    update_pending_repl_step_view(
                        pending_view,
                        step,
                        step_start_index,
                        &preview_messages,
                        format!("Receiving response · step {step}"),
                        preview_detail(&response_text, 1, 96),
                        TaskStatus::Running,
                    );
                }
                ProviderEvent::ToolCall { call } => {
                    let tool_name = call.name.clone();
                    response_tool_calls.push(call);
                    let current_call = response_tool_calls.last().cloned();
                    let preview_message = provider_assistant_message(
                        session_id,
                        parent_id,
                        response_text.clone(),
                        response_tool_calls.clone(),
                        provider,
                        &model,
                        latest_usage.clone(),
                    );
                    let mut preview_messages = messages.clone();
                    preview_messages.push(preview_message);
                    update_pending_repl_step_view(
                        pending_view,
                        step,
                        step_start_index,
                        &preview_messages,
                        format!("Running {}", tool_display_name(&tool_name)),
                        current_call
                            .as_ref()
                            .and_then(pending_tool_detail_from_call),
                        TaskStatus::Running,
                    );
                }
                ProviderEvent::ToolCallBoundary { .. } => {}
                ProviderEvent::Usage { usage } => {
                    latest_usage = Some(usage);
                }
                ProviderEvent::Stop { reason } => {
                    stop_reason = Some(reason);
                    break;
                }
                ProviderEvent::Error { message } => return Err(anyhow!(message)),
            }
        }

        let assistant_message = provider_assistant_message(
            session_id,
            parent_id,
            response_text,
            response_tool_calls.clone(),
            provider,
            &model,
            latest_usage.clone(),
        );
        store.append_message(session_id, &assistant_message).await?;
        messages.push(assistant_message.clone());
        update_pending_repl_step_view(
            pending_view,
            step,
            step_start_index,
            messages,
            if response_tool_calls.is_empty() {
                format!("Completed step {step}")
            } else {
                format!(
                    "Running {}",
                    tool_display_name(&response_tool_calls[0].name)
                )
            },
            response_tool_calls
                .first()
                .and_then(pending_tool_detail_from_call),
            if response_tool_calls.is_empty() {
                TaskStatus::Completed
            } else {
                TaskStatus::Running
            },
        );

        if response_tool_calls.is_empty() {
            return Ok((latest_usage, step, stop_reason));
        }

        for call in response_tool_calls {
            update_pending_repl_step_view(
                pending_view,
                step,
                step_start_index,
                messages,
                format!("Running {}", tool_display_name(&call.name)),
                pending_tool_detail_from_call(&call),
                TaskStatus::Running,
            );
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
            let output_content = output.content;
            let output_is_error = output.is_error;
            let output_metadata = output.metadata;
            let tool_message = build_tool_result_message(
                session_id,
                call.id.clone(),
                output_content,
                output_is_error,
                messages.last().map(|message| message.id),
            );
            store.append_message(session_id, &tool_message).await?;
            let tool_message_id = tool_message.id;
            messages.push(tool_message);
            append_session_messages(
                store,
                messages,
                tool_ui_event_messages(session_id, Some(tool_message_id), &output_metadata),
            )
            .await?;
            update_pending_repl_step_view(
                pending_view,
                step,
                step_start_index,
                messages,
                if output_is_error {
                    format!("{} failed", tool_display_name(&call.name))
                } else {
                    format!("{} completed", tool_display_name(&call.name))
                },
                pending_tool_detail_from_metadata(&call.name, &output_metadata)
                    .or_else(|| pending_tool_detail_from_call(&call)),
                if output_is_error {
                    TaskStatus::Failed
                } else {
                    TaskStatus::Completed
                },
            );
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
    pending_view: Option<Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<CompactionOutcome>, usize, Option<String>, u64, u64)> {
    let parent_id = raw_messages.last().map(|message| message.id);
    let user_message = build_text_message(session_id, MessageRole::User, prompt_text, parent_id);
    store.append_message(session_id, &user_message).await?;
    raw_messages.push(user_message);
    update_pending_repl_view(pending_view.as_ref(), raw_messages, "Waiting for response");

    let estimated_tokens_before =
        estimate_message_tokens(&materialize_runtime_messages(raw_messages));
    let applied_compaction = maybe_auto_compact(store, session_id, raw_messages).await?;
    update_pending_repl_view(pending_view.as_ref(), raw_messages, "Waiting for response");
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
        pending_view.as_ref(),
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
    let mut cli = parse_cli();
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

    resolve_continue_target(&mut cli, &store).await?;

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
    let (session_id, transcript_path, mut existing_messages) =
        choose_active_session(&cli, explicit_resume)?;
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
        apply_repl_header(&mut state, provider, &active_model, &cwd, session_id);
        state.status_line = repl_status(provider, &active_model, session_id);
        if let Some(path) = transcript_path.as_ref() {
            state.compact_banner = Some(format!("resume {}", shorten_path(path, 72)));
        }
        let (width, height) = terminal_size().unwrap_or((100, 28));
        println!("{}", render_tui_to_string(&state, width, height)?);
        return Ok(());
    }

    if cli.repl {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let final_session_id = run_interactive_repl(
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
            auth_source.clone(),
            transcript_path.clone(),
            remote_mode_enabled(&cli),
            ide_bridge_enabled(&cli),
        )
        .await?;
        if let Ok(resume_hint) = current_resume_hint(&store, final_session_id).await {
            print_resume_hint(&resume_hint);
        }
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
            None,
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
mod tests;
