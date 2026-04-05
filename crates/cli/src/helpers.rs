use super::*;
use std::collections::BTreeSet;

pub(crate) fn task_store_for(cwd: &Path) -> CoreLocalTaskStore {
    CoreLocalTaskStore::new(cwd.join(".code-agent"))
}

pub(crate) fn command_report(spec: &CommandSpec) -> CommandReport {
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

pub(crate) async fn resolved_command_registry(
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

pub(crate) fn session_preview(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        let text = message_text(message);
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| preview_lines_from_text(trimmed.to_owned(), 1, 72).join(" "))
    })
}

pub(crate) fn auth_hint_for_provider(provider: ApiProvider) -> String {
    if matches!(
        provider,
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
    ) {
        get_openai_credential_hint(provider)
    } else {
        get_anthropic_credential_hint(provider)
    }
}

pub(crate) fn parse_task_id(value: &str) -> Result<uuid::Uuid> {
    Ok(uuid::Uuid::parse_str(value)?)
}

pub(crate) fn parse_task_status(value: &str) -> Result<TaskStatus> {
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

pub(crate) fn prompt_preview(messages: &[Message]) -> Vec<String> {
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

pub(crate) fn shorten_middle(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let left_len = (max_chars - 3) / 2;
    let right_len = max_chars - 3 - left_len;
    let left = text.chars().take(left_len).collect::<String>();
    let right = text
        .chars()
        .rev()
        .take(right_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{left}...{right}")
}

pub(crate) fn shorten_path(path: &Path, max_chars: usize) -> String {
    shorten_middle(&path.display().to_string(), max_chars)
}

pub(crate) fn short_session_id(session_id: SessionId) -> String {
    session_id
        .to_string()
        .split('-')
        .next()
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn repl_status(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
) -> String {
    format!(
        "{provider} · {active_model} · s:{}",
        short_session_id(session_id)
    )
}

pub(crate) fn repl_header_title() -> String {
    format!("code-agent-rust v{}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn repl_header_subtitle(provider: ApiProvider, active_model: &str) -> String {
    format!("{active_model} · {provider}")
}

pub(crate) fn repl_header_context(cwd: &Path, session_id: SessionId) -> String {
    format!("{} · s:{}", cwd.display(), short_session_id(session_id))
}

pub(crate) fn apply_repl_header(
    state: &mut code_agent_ui::UiState,
    provider: ApiProvider,
    active_model: &str,
    cwd: &Path,
    session_id: SessionId,
) {
    state.header_title = Some(repl_header_title());
    state.header_subtitle = Some(repl_header_subtitle(provider, active_model));
    state.header_context = Some(repl_header_context(cwd, session_id));
}

pub(crate) fn status_with_detail(base: String, detail: impl AsRef<str>) -> String {
    let detail = detail.as_ref().trim();
    if detail.is_empty() {
        return base;
    }

    format!("{base} · {detail}")
}

pub(crate) fn workspace_is_empty(cwd: &Path) -> bool {
    fs::read_dir(cwd)
        .ok()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

pub(crate) fn friendly_auth_source(source: Option<&str>) -> String {
    match source {
        Some("codex_auth_token") => "Codex refreshable token".to_owned(),
        Some("codex_auth_api_key") => "Codex API key".to_owned(),
        Some("OPENAI_API_KEY") => "OPENAI_API_KEY".to_owned(),
        Some("ANTHROPIC_API_KEY") => "ANTHROPIC_API_KEY".to_owned(),
        Some("CLAUDE_CODE_OAUTH_TOKEN") => "CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
        Some("ANTHROPIC_AUTH_TOKEN") => "ANTHROPIC_AUTH_TOKEN".to_owned(),
        Some("ambient_cloud_auth") => "ambient cloud auth".to_owned(),
        Some(other) => other.replace('_', " "),
        None => "not configured".to_owned(),
    }
}

pub(crate) fn command_palette_entries(registry: &CommandRegistry) -> Vec<CommandPaletteEntry> {
    registry
        .all()
        .iter()
        .map(|command| CommandPaletteEntry {
            name: format!("/{}", command.name),
            description: command.description.clone(),
        })
        .collect()
}

pub(crate) fn slash_command_query(input_buffer: &code_agent_ui::InputBuffer) -> Option<String> {
    let text = input_buffer.as_str();
    if !text.starts_with('/') {
        return None;
    }

    let first_token = text.split_whitespace().next().unwrap_or_default();
    if first_token.is_empty() || !first_token.starts_with('/') {
        return None;
    }
    if text.contains(char::is_whitespace) && text.trim() != first_token {
        return None;
    }

    Some(first_token.trim_start_matches('/').to_ascii_lowercase())
}

pub(crate) fn command_suggestions(
    registry: &CommandRegistry,
    input_buffer: &code_agent_ui::InputBuffer,
) -> Vec<CommandPaletteEntry> {
    let Some(query) = slash_command_query(input_buffer) else {
        return Vec::new();
    };

    let mut exact_matches = Vec::new();
    let mut prefix_matches = Vec::new();
    for entry in command_palette_entries(registry) {
        let command = entry.name.trim_start_matches('/').to_ascii_lowercase();
        if query.is_empty() || command == query {
            exact_matches.push(entry);
        } else if command.starts_with(&query) {
            prefix_matches.push(entry);
        }
    }
    exact_matches.extend(prefix_matches);
    exact_matches
}

pub(crate) fn sync_command_selection(
    registry: &CommandRegistry,
    input_buffer: &code_agent_ui::InputBuffer,
    selected_index: &mut usize,
) -> Vec<CommandPaletteEntry> {
    let suggestions = command_suggestions(registry, input_buffer);
    if suggestions.is_empty() || *selected_index >= suggestions.len() {
        *selected_index = 0;
    }
    suggestions
}

pub(crate) fn apply_selected_command(
    input_buffer: &mut code_agent_ui::InputBuffer,
    entry: &CommandPaletteEntry,
) {
    input_buffer.replace(format!("{} ", entry.name));
}

pub(crate) fn scroll_up(scroll: &mut u16, amount: u16) {
    *scroll = scroll.saturating_add(amount);
}

pub(crate) fn scroll_down(scroll: &mut u16, amount: u16) {
    *scroll = scroll.saturating_sub(amount);
}

pub(crate) fn push_prompt_history_entry(history: &mut Vec<String>, prompt_text: &str) {
    let prompt_text = prompt_text.trim();
    if prompt_text.is_empty() {
        return;
    }
    if history
        .last()
        .is_some_and(|previous| previous == prompt_text)
    {
        return;
    }
    history.push(prompt_text.to_owned());
}

pub(crate) fn prompt_history_from_messages(raw_messages: &[Message]) -> Vec<String> {
    let mut history = Vec::new();
    for message in raw_messages {
        if message.role == MessageRole::User {
            push_prompt_history_entry(&mut history, &message_text(message));
        }
    }
    history
}

pub(crate) fn reset_prompt_history_navigation(
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
) {
    *history_index = None;
    *history_draft = None;
}

pub(crate) fn navigate_prompt_history_up(
    history: &[String],
    input_buffer: &mut code_agent_ui::InputBuffer,
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
) -> bool {
    if history.is_empty() {
        return false;
    }

    let next_index = match *history_index {
        Some(0) => 0,
        Some(index) => index.saturating_sub(1),
        None => {
            *history_draft = Some(input_buffer.clone());
            history.len() - 1
        }
    };
    *history_index = Some(next_index);
    input_buffer.replace(history[next_index].clone());
    true
}

pub(crate) fn navigate_prompt_history_down(
    history: &[String],
    input_buffer: &mut code_agent_ui::InputBuffer,
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
) -> bool {
    let Some(current_index) = *history_index else {
        return false;
    };

    if current_index + 1 < history.len() {
        let next_index = current_index + 1;
        *history_index = Some(next_index);
        input_buffer.replace(history[next_index].clone());
    } else {
        *history_index = None;
        if let Some(draft) = history_draft.take() {
            *input_buffer = draft;
        } else {
            input_buffer.clear();
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptInputNavigationDirection {
    Up,
    Down,
}

fn navigate_prompt_input(
    registry: &CommandRegistry,
    input_buffer: &mut code_agent_ui::InputBuffer,
    selected_command_suggestion: &mut usize,
    history: &[String],
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
    direction: PromptInputNavigationDirection,
) {
    let suggestions = sync_command_selection(registry, input_buffer, selected_command_suggestion);
    if suggestions.len() > 1 {
        match direction {
            PromptInputNavigationDirection::Up => {
                *selected_command_suggestion = if *selected_command_suggestion == 0 {
                    suggestions.len() - 1
                } else {
                    *selected_command_suggestion - 1
                };
            }
            PromptInputNavigationDirection::Down => {
                *selected_command_suggestion =
                    (*selected_command_suggestion + 1) % suggestions.len();
            }
        }
        return;
    }

    match direction {
        PromptInputNavigationDirection::Up => {
            let _ = navigate_prompt_history_up(history, input_buffer, history_index, history_draft);
        }
        PromptInputNavigationDirection::Down => {
            let _ =
                navigate_prompt_history_down(history, input_buffer, history_index, history_draft);
        }
    }
}

pub(crate) fn navigate_prompt_input_up(
    registry: &CommandRegistry,
    input_buffer: &mut code_agent_ui::InputBuffer,
    selected_command_suggestion: &mut usize,
    history: &[String],
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
) {
    navigate_prompt_input(
        registry,
        input_buffer,
        selected_command_suggestion,
        history,
        history_index,
        history_draft,
        PromptInputNavigationDirection::Up,
    )
}

pub(crate) fn navigate_prompt_input_down(
    registry: &CommandRegistry,
    input_buffer: &mut code_agent_ui::InputBuffer,
    selected_command_suggestion: &mut usize,
    history: &[String],
    history_index: &mut Option<usize>,
    history_draft: &mut Option<code_agent_ui::InputBuffer>,
) {
    navigate_prompt_input(
        registry,
        input_buffer,
        selected_command_suggestion,
        history,
        history_index,
        history_draft,
        PromptInputNavigationDirection::Down,
    )
}

pub(crate) fn prompt_history_search_matches(history: &[String], query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut seen = BTreeSet::new();
    let mut matches = Vec::new();

    for (index, entry) in history.iter().enumerate().rev() {
        let normalized = entry.trim();
        if normalized.is_empty() {
            continue;
        }
        if !normalized.contains(query) {
            continue;
        }
        if seen.insert(normalized.to_owned()) {
            matches.push(index);
        }
    }

    matches
}
