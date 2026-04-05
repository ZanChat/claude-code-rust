async fn run_pending_repl_operation<F, T>(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    registry: &code_agent_core::CommandRegistry,
    pending_view: Arc<Mutex<PendingReplView>>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    input_buffer: &mut code_agent_ui::InputBuffer,
    prompt_history_index: &mut Option<usize>,
    prompt_history_draft: &mut Option<code_agent_ui::InputBuffer>,
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
                            active_repl_choice_list(cwd, input_buffer, None, interaction_state),
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
                                        MouseEventKind::Up(MouseButton::Left)
                                    ) =>
                                {
                                    clear_prompt_mouse_anchor(interaction_state);
                                    if is_history_transcript_group_id(&group_id) {
                                        toggle_history_transcript_group(
                                            interaction_state,
                                            &group_id,
                                        );
                                    } else {
                                        toggle_pending_repl_group(&pending_view, &group_id);
                                    }
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
                        reset_prompt_history_navigation(
                            prompt_history_index,
                            prompt_history_draft,
                        );
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
                                reset_prompt_history_navigation(
                                    prompt_history_index,
                                    prompt_history_draft,
                                );
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
                            ReplShortcutAction::ToggleTranscriptDetails => {
                                if !pending_snapshot.steps.is_empty() {
                                    toggle_pending_repl_transcript_details(&pending_view);
                                } else {
                                    let group_ids =
                                        history_transcript_group_ids(&pending_snapshot.messages);
                                    let _ = toggle_all_history_transcript_groups(
                                        interaction_state,
                                        &group_ids,
                                    );
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
                                    interaction_state,
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
                            interaction_state,
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
                                let selected_history_group = selected_message_action_item(
                                    interaction_state,
                                    &message_action_items,
                                )
                                .and_then(|item| item.history_group_id.clone());
                                if let Some(group_id) = selected_history_group {
                                    toggle_history_transcript_group(interaction_state, &group_id);
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
                                    continue;
                                }

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

                    if vim_state.is_insert()
                        && handle_prompt_file_picker_key(cwd, &key, input_buffer, interaction_state)
                    {
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
                            navigate_prompt_input_up(
                                registry,
                                input_buffer,
                                selected_command_suggestion,
                                &prompt_history_from_messages(&pending_snapshot.messages),
                                prompt_history_index,
                                prompt_history_draft,
                            );
                        }
                        KeyCode::Down => {
                            clear_prompt_selection(interaction_state);
                            navigate_prompt_input_down(
                                registry,
                                input_buffer,
                                selected_command_suggestion,
                                &prompt_history_from_messages(&pending_snapshot.messages),
                                prompt_history_index,
                                prompt_history_draft,
                            );
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
                            reset_prompt_history_navigation(
                                prompt_history_index,
                                prompt_history_draft,
                            );
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
                            reset_prompt_history_navigation(
                                prompt_history_index,
                                prompt_history_draft,
                            );
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
                                    reset_prompt_history_navigation(
                                        prompt_history_index,
                                        prompt_history_draft,
                                    );
                                    clear_prompt_selection(interaction_state);
                                    apply_selected_command(input_buffer, selected);
                                    continue;
                                }
                            }
                            reset_prompt_history_navigation(
                                prompt_history_index,
                                prompt_history_draft,
                            );
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
            active_repl_choice_list(cwd, input_buffer, None, interaction_state),
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

