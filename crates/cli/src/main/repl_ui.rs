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
    let message_action_items =
        message_action_items_from_runtime(&runtime_messages, pending_view, interaction_state);
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
        let visible_history = build_history_transcript_presentation(
            &runtime_messages[..first_step_start],
            &interaction_state.expanded_history_groups,
        );
        let visible_transcript =
            UiState::from_messages(runtime_messages[..first_step_start].to_vec());
        state.transcript_lines = visible_transcript.transcript_lines;
        state.transcript_items = visible_history.items;
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
                        single_item: false,
                        lines: UiState::from_messages(slice.to_vec()).transcript_lines,
                    }
                })
                .collect();
            state.transcript_items.extend(
                state
                    .transcript_groups
                    .iter()
                    .cloned()
                    .map(TranscriptItem::Group),
            );
        }
    } else {
        state.transcript_items = build_history_transcript_presentation(
            &runtime_messages,
            &interaction_state.expanded_history_groups,
        )
        .items;
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
    if choice_list.is_some() {
        return Ok(None);
    }

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
