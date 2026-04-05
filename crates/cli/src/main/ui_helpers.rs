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
