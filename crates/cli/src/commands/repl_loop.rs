fn drain_mouse_scroll_burst(
    initial_kind: &MouseEventKind,
    pending_event: &mut Option<Event>,
) -> Result<u16> {
    let scroll_up = matches!(initial_kind, MouseEventKind::ScrollUp);
    let scroll_down = matches!(initial_kind, MouseEventKind::ScrollDown);
    if !scroll_up && !scroll_down {
        return Ok(1);
    }

    let mut burst = 1u16;
    while event::poll(Duration::from_millis(0))? {
        let next = event::read()?;
        match next {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            }) if scroll_up => {
                burst = burst.saturating_add(1);
            }
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            }) if scroll_down => {
                burst = burst.saturating_add(1);
            }
            other => {
                *pending_event = Some(other);
                break;
            }
        }
    }

    Ok(burst)
}

pub(crate) async fn run_interactive_repl(
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
    auth_source: Option<String>,
    transcript_path: Option<PathBuf>,
    remote_mode: bool,
    ide_bridge_active: bool,
) -> Result<SessionId> {
    let mut active_model = active_model;
    let mut repl_session = ReplSessionState {
        session_id,
        transcript_path,
    };
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut out = stdout();
    let mouse_capture_enabled =
        should_enable_mouse_capture(std::env::var("TERM_PROGRAM").ok().as_deref());
    enable_raw_mode()?;
    execute!(
        out,
        EnterAlternateScreen,
        Hide,
        crossterm::event::EnableBracketedPaste
    )?;
    if mouse_capture_enabled {
        execute!(out, EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut startup_preferences = load_startup_preferences();
    let startup_screens = build_startup_screens(
        provider,
        &active_model,
        repl_session.session_id,
        &cwd,
        store.root_dir(),
        repl_session.transcript_path.as_deref(),
        live_runtime,
        auth_source.as_deref(),
        &startup_preferences,
    );
    let mut initial_input_buffer = code_agent_ui::InputBuffer::new();
    if !startup_screens.is_empty() {
        initial_input_buffer = run_startup_flow(
            &mut terminal,
            provider,
            &active_model,
            repl_session.session_id,
            &cwd,
            &startup_screens,
        )?;
        if !startup_preferences.welcome_seen {
            startup_preferences.welcome_seen = true;
            save_startup_preferences(&startup_preferences)?;
        }
    }

    let loop_result = async {
        let mut input_buffer = initial_input_buffer;
        let mut prompt_history = prompt_history_from_messages(raw_messages);
        let mut prompt_history_index = None;
        let mut prompt_history_draft: Option<code_agent_ui::InputBuffer> = None;
        let mut transcript_scroll = 0u16;
        let mut status_line = repl_status(provider, &active_model, repl_session.session_id);
        let mut status_marquee_tick = 0usize;
        let mut active_pane = PaneKind::Transcript;
        let mut selected_command_suggestion = 0usize;
        let mut compact_banner = None;
        let mut resume_picker = None;
        let mut ide_picker = None;
        let mut connected_ide_bridge = None;
        let mut queued_submissions = VecDeque::new();
        let mut interaction_state = ReplInteractionState::default();
        let mut pending_event = None;
        let mut dirty = true;
        loop {
            if dirty {
                draw_repl_state(
                    &mut terminal,
                    registry,
                    raw_messages,
                    None,
                    &cwd,
                    provider,
                    &active_model,
                    repl_session.session_id,
                    &input_buffer,
                    &status_line,
                    None,
                    active_pane,
                    compact_banner.clone(),
                    transcript_scroll,
                    active_repl_choice_list(
                        &cwd,
                        &input_buffer,
                        resume_picker
                            .as_ref()
                            .map(build_resume_choice_list)
                            .or_else(|| {
                                ide_picker.as_ref().map(|picker| {
                                    build_ide_choice_list(picker, connected_ide_bridge.as_ref())
                                })
                            }),
                        &mut interaction_state,
                    ),
                    &mut selected_command_suggestion,
                    &vim_state,
                    status_marquee_tick,
                    &interaction_state,
                )?;
                dirty = false;
            }

            if resume_picker.is_none() && ide_picker.is_none() {
                if let Some(prompt_text) = queued_submissions.pop_front() {
                    match process_repl_submission(
                        &mut terminal,
                        store,
                        registry,
                        tool_registry,
                        &cwd,
                        plugin_root,
                        provider,
                        &mut active_model,
                        &mut repl_session,
                        raw_messages,
                        live_runtime,
                        prompt_text,
                        &mut input_buffer,
                        &mut prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                        &mut transcript_scroll,
                        &mut status_line,
                        &mut status_marquee_tick,
                        &mut active_pane,
                        &mut compact_banner,
                        &mut interaction_state,
                        &mut resume_picker,
                        &mut ide_picker,
                        &connected_ide_bridge,
                        &mut selected_command_suggestion,
                        &mut vim_state,
                        remote_mode,
                        ide_bridge_active,
                        &mut queued_submissions,
                    )
                    .await?
                    {
                        ReplSubmissionOutcome::Continue => {
                            dirty = true;
                            continue;
                        }
                        ReplSubmissionOutcome::Exit => break,
                    }
                }
            }

            let event = if let Some(event) = pending_event.take() {
                event
            } else if status_line_needs_marquee(&status_line) {
                if !event::poll(Duration::from_millis(160))? {
                    status_marquee_tick = status_marquee_tick.wrapping_add(1);
                    dirty = true;
                    continue;
                }
                event::read()?
            } else {
                event::read()?
            };
            if let Event::Resize(width, height) = event {
                terminal.resize(Rect::new(0, 0, width, height))?;
                dirty = true;
                continue;
            }
            if let Event::Mouse(mouse) = event {
                if ide_picker.is_some() {
                    continue;
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        clear_prompt_mouse_anchor(&mut interaction_state);
                        interaction_state.transcript_selection = None;
                        let burst = drain_mouse_scroll_burst(&mouse.kind, &mut pending_event)?;
                        scroll_up(&mut transcript_scroll, burst.saturating_mul(3));
                        dirty = true;
                    }
                    MouseEventKind::ScrollDown => {
                        clear_prompt_mouse_anchor(&mut interaction_state);
                        interaction_state.transcript_selection = None;
                        let burst = drain_mouse_scroll_burst(&mouse.kind, &mut pending_event)?;
                        scroll_down(&mut transcript_scroll, burst.saturating_mul(3));
                        dirty = true;
                    }
                    MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left) => {
                        if let Some(action) = repl_mouse_action(
                            &terminal,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            active_repl_choice_list(
                                &cwd,
                                &input_buffer,
                                resume_picker
                                    .as_ref()
                                    .map(build_resume_choice_list)
                                    .or_else(|| {
                                        ide_picker.as_ref().map(|picker| {
                                            build_ide_choice_list(
                                                picker,
                                                connected_ide_bridge.as_ref(),
                                            )
                                        })
                                    }),
                                &mut interaction_state,
                            ),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &mouse,
                            &interaction_state,
                        )? {
                            match action {
                                UiMouseAction::JumpToBottom
                                    if matches!(
                                        mouse.kind,
                                        MouseEventKind::Down(MouseButton::Left)
                                    ) =>
                                {
                                    clear_prompt_mouse_anchor(&mut interaction_state);
                                    transcript_scroll = 0;
                                }
                                UiMouseAction::ToggleTranscriptGroup(group_id)
                                    if matches!(
                                        mouse.kind,
                                        MouseEventKind::Down(MouseButton::Left)
                                    ) =>
                                {
                                    clear_prompt_mouse_anchor(&mut interaction_state);
                                    if is_history_transcript_group_id(&group_id) {
                                        toggle_history_transcript_group(
                                            &mut interaction_state,
                                            &group_id,
                                        );
                                    }
                                }
                                UiMouseAction::SetPromptCursor(cursor) => {
                                    let _ = handle_prompt_mouse_action(
                                        &mouse.kind,
                                        cursor,
                                        &mut interaction_state,
                                        &mut input_buffer,
                                    );
                                }
                                _ => {}
                            }
                            dirty = true;
                        } else if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
                            clear_prompt_mouse_anchor(&mut interaction_state);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if let Event::Paste(text) = event {
                clear_prompt_mouse_anchor(&mut interaction_state);
                if ide_picker.is_some() {
                    continue;
                }
                if let Some(search_state) = interaction_state.prompt_history_search.as_mut() {
                    let _ = insert_buffer_text(&mut search_state.input_buffer, &text);
                    sync_prompt_history_search_preview(
                        &prompt_history,
                        search_state,
                        &mut input_buffer,
                    );
                    dirty = true;
                } else if interaction_state.transcript_search.open {
                    let input = &mut interaction_state.transcript_search.input_buffer;
                    let _ = insert_buffer_text(input, &text);
                    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                    let state = build_repl_ui_state(
                        &app,
                        registry,
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    sync_transcript_search_preview(
                        &state,
                        size.width,
                        size.height,
                        &mut interaction_state.transcript_search,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                } else if !interaction_state.transcript_mode && vim_state.is_insert() {
                    reset_prompt_history_navigation(
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                    );
                    if insert_prompt_text(&mut interaction_state, &mut input_buffer, &text) {
                        selected_command_suggestion = 0;
                        dirty = true;
                    }
                }
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            clear_prompt_mouse_anchor(&mut interaction_state);
            if resume_picker.is_some() {
                enum ResumePickerAction {
                    Close,
                    Resume(SessionSummary),
                }

                let mut picker_action = None;
                if let Some(picker) = resume_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc => {
                            picker_action = Some(ResumePickerAction::Close);
                            dirty = true;
                        }
                        KeyCode::Up => {
                            picker.selected = picker.selected.saturating_sub(1);
                            dirty = true;
                        }
                        KeyCode::Down => {
                            if picker.selected + 1 < picker.sessions.len() {
                                picker.selected += 1;
                            }
                            dirty = true;
                        }
                        KeyCode::PageUp => {
                            picker.selected = picker.selected.saturating_sub(5);
                            dirty = true;
                        }
                        KeyCode::PageDown => {
                            if !picker.sessions.is_empty() {
                                picker.selected =
                                    (picker.selected + 5).min(picker.sessions.len() - 1);
                            }
                            dirty = true;
                        }
                        KeyCode::Home => {
                            picker.selected = 0;
                            dirty = true;
                        }
                        KeyCode::End => {
                            if !picker.sessions.is_empty() {
                                picker.selected = picker.sessions.len() - 1;
                            }
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            picker_action = picker
                                .sessions
                                .get(picker.selected)
                                .cloned()
                                .map(ResumePickerAction::Resume);
                            dirty = true;
                        }
                        _ => {}
                    }
                }

                match picker_action {
                    Some(ResumePickerAction::Close) => {
                        resume_picker = None;
                        status_line = repl_status(provider, &active_model, repl_session.session_id);
                        status_marquee_tick = 0;
                    }
                    Some(ResumePickerAction::Resume(summary)) => {
                        resume_picker = None;
                        let previous_session_id = repl_session.session_id;
                        let transcript_path = resume_repl_session(
                            store,
                            &mut repl_session,
                            raw_messages,
                            &summary.session_id.to_string(),
                        )
                        .await?;
                        if repl_session.session_id != previous_session_id {
                            prompt_history = prompt_history_from_messages(raw_messages);
                            reset_prompt_history_navigation(
                                &mut prompt_history_index,
                                &mut prompt_history_draft,
                            );
                            transcript_scroll = 0;
                        }
                        compact_banner =
                            Some(format!("resume {}", shorten_path(&transcript_path, 72)));
                        status_line = repl_status(provider, &active_model, repl_session.session_id);
                        status_marquee_tick = 0;
                    }
                    None => {}
                }
                continue;
            }

            if ide_picker.is_some() {
                enum IdePickerAction {
                    Cancel,
                    Connect(Option<DetectedIdeCandidate>),
                }

                let mut picker_action = None;
                if let Some(picker) = ide_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc => {
                            picker_action = Some(IdePickerAction::Cancel);
                            dirty = true;
                        }
                        KeyCode::Up => {
                            picker.selected = picker.selected.saturating_sub(1);
                            dirty = true;
                        }
                        KeyCode::Down => {
                            if picker.selected + 1 < picker.candidates.len() {
                                picker.selected += 1;
                            }
                            dirty = true;
                        }
                        KeyCode::PageUp => {
                            picker.selected = picker.selected.saturating_sub(5);
                            dirty = true;
                        }
                        KeyCode::PageDown => {
                            if !picker.candidates.is_empty() {
                                picker.selected =
                                    (picker.selected + 5).min(picker.candidates.len() - 1);
                            }
                            dirty = true;
                        }
                        KeyCode::Home => {
                            picker.selected = 0;
                            dirty = true;
                        }
                        KeyCode::End => {
                            if !picker.candidates.is_empty() {
                                picker.selected = picker.candidates.len() - 1;
                            }
                            dirty = true;
                        }
                        KeyCode::Enter | KeyCode::Tab => {
                            picker_action = Some(IdePickerAction::Connect(
                                picker.candidates.get(picker.selected).cloned(),
                            ));
                            dirty = true;
                        }
                        _ if is_plain_ctrl_char(&key, 'c') => {
                            picker_action = Some(IdePickerAction::Cancel);
                            dirty = true;
                        }
                        _ => {}
                    }
                }

                match picker_action {
                    Some(IdePickerAction::Cancel) => {
                        ide_picker = None;
                        status_line = repl_status(provider, &active_model, repl_session.session_id);
                        status_marquee_tick = 0;
                    }
                    Some(IdePickerAction::Connect(Some(candidate))) => {
                        let message = format!(
                            "Connected to {} via {}",
                            candidate.name, candidate.suggested_bridge
                        );
                        compact_banner = Some(message.clone());
                        connected_ide_bridge = Some(candidate);
                        ide_picker = None;
                        status_line = status_with_detail(
                            repl_status(provider, &active_model, repl_session.session_id),
                            message,
                        );
                        status_marquee_tick = 0;
                    }
                    Some(IdePickerAction::Connect(None)) => {
                        ide_picker = None;
                        status_line = status_with_detail(
                            repl_status(provider, &active_model, repl_session.session_id),
                            "No IDE bridge detected for this workspace",
                        );
                        status_marquee_tick = 0;
                    }
                    None => {}
                }
                continue;
            }

            if is_paste_shortcut(&key) {
                if let Some(search_state) = interaction_state.prompt_history_search.as_mut() {
                    if let Some(text) = read_text_from_clipboard() {
                        let _ = insert_buffer_text(&mut search_state.input_buffer, &text);
                        sync_prompt_history_search_preview(
                            &prompt_history,
                            search_state,
                            &mut input_buffer,
                        );
                        dirty = true;
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
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        sync_transcript_search_preview(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            &mut transcript_scroll,
                        );
                        dirty = true;
                    }
                    continue;
                }

                if !interaction_state.transcript_mode && vim_state.is_insert() {
                    if let Some(text) = read_text_from_clipboard() {
                        reset_prompt_history_navigation(
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                        if insert_prompt_text(&mut interaction_state, &mut input_buffer, &text) {
                            selected_command_suggestion = 0;
                            dirty = true;
                        }
                    }
                    continue;
                }
            }

            if let Some(shortcut) = repl_shortcut_action_for_key(&key, &interaction_state) {
                match shortcut {
                    ReplShortcutAction::CopySelection => {
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        if let Some(text) = repl_selection_copy_text(
                            &state,
                            size.width,
                            &input_buffer,
                            &interaction_state,
                        ) {
                            compact_banner = Some(
                                copy_text_with_fallback_notice(&text, "selection")
                                    .unwrap_or_else(|error| format!("Copy failed: {error}")),
                            );
                        }
                        clear_prompt_selection(&mut interaction_state);
                        interaction_state.transcript_selection = None;
                        dirty = true;
                    }
                    ReplShortcutAction::ContextCtrlC => {
                        if cancel_prompt_history_search(&mut interaction_state, &mut input_buffer) {
                            dirty = true;
                            continue;
                        }
                        if interaction_state.transcript_search.open {
                            cancel_transcript_search(
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                            continue;
                        }
                        if interaction_state.message_actions.is_some() {
                            interaction_state.message_actions = None;
                            dirty = true;
                            continue;
                        }
                        if interaction_state.transcript_selection.is_some()
                            || interaction_state.prompt_selection.is_some()
                        {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            if let Some(text) = repl_selection_copy_text(
                                &state,
                                size.width,
                                &input_buffer,
                                &interaction_state,
                            ) {
                                compact_banner = Some(
                                    copy_text_with_fallback_notice(&text, "selection")
                                        .unwrap_or_else(|error| format!("Copy failed: {error}")),
                                );
                            }
                            clear_prompt_selection(&mut interaction_state);
                            interaction_state.transcript_selection = None;
                            dirty = true;
                            continue;
                        }
                        if interaction_state.transcript_mode {
                            exit_transcript_mode(&mut interaction_state);
                            dirty = true;
                            continue;
                        }
                        break;
                    }
                    ReplShortcutAction::ToggleTranscriptMode => {
                        if interaction_state.transcript_mode {
                            exit_transcript_mode(&mut interaction_state);
                        } else {
                            enter_transcript_mode(&mut interaction_state, &mut active_pane);
                        }
                        dirty = true;
                    }
                    ReplShortcutAction::ToggleTranscriptDetails => {
                        let group_ids = history_transcript_group_ids(
                            &materialize_runtime_messages(raw_messages),
                        );
                        if toggle_all_history_transcript_groups(&mut interaction_state, &group_ids)
                        {
                            dirty = true;
                        }
                    }
                    ReplShortcutAction::PromptHistorySearch => {
                        if let Some(search_state) = interaction_state.prompt_history_search.as_mut()
                        {
                            let _ = step_prompt_history_search_match(
                                &prompt_history,
                                search_state,
                                &mut input_buffer,
                            );
                        } else {
                            open_prompt_history_search(&mut interaction_state, &input_buffer);
                        }
                        dirty = true;
                    }
                    ReplShortcutAction::EnterMessageActions => {
                        let runtime_messages = materialize_runtime_messages(raw_messages);
                        let message_action_items = message_action_items_from_runtime(
                            &runtime_messages,
                            None,
                            &interaction_state,
                        );
                        if enter_message_actions(&mut interaction_state, &message_action_items) {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_message_action_preview(
                                &state,
                                size.width,
                                size.height,
                                &interaction_state,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                    }
                    ReplShortcutAction::SelectPane(pane) => {
                        active_pane = pane;
                        dirty = true;
                    }
                    ReplShortcutAction::RotatePaneForward => {
                        active_pane = rotate_pane(active_pane, true);
                        dirty = true;
                    }
                    ReplShortcutAction::RotatePaneBackward => {
                        active_pane = rotate_pane(active_pane, false);
                        dirty = true;
                    }
                }
                continue;
            }

            if vim_state.is_insert()
                && handle_prompt_file_picker_key(
                    &cwd,
                    &key,
                    &mut input_buffer,
                    &mut interaction_state,
                )
            {
                dirty = true;
                continue;
            }

            if interaction_state.message_actions.is_some() {
                let runtime_messages = materialize_runtime_messages(raw_messages);
                let message_action_items =
                    message_action_items_from_runtime(&runtime_messages, None, &interaction_state);
                if selected_message_action_item(&mut interaction_state, &message_action_items)
                    .is_none()
                {
                    interaction_state.message_actions = None;
                    dirty = true;
                    continue;
                }

                let mut selection_changed = false;
                match key.code {
                    KeyCode::Esc => {
                        interaction_state.message_actions = None;
                        dirty = true;
                    }
                    KeyCode::Enter => {
                        let selected_history_group = selected_message_action_item(
                            &mut interaction_state,
                            &message_action_items,
                        )
                        .and_then(|item| item.history_group_id.clone());
                        if let Some(group_id) = selected_history_group {
                            toggle_history_transcript_group(&mut interaction_state, &group_id);
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_message_action_preview(
                                &state,
                                size.width,
                                size.height,
                                &interaction_state,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                            continue;
                        }

                        let prompt_text = selected_message_action_item(
                            &mut interaction_state,
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
                            clear_prompt_selection(&mut interaction_state);
                            interaction_state.message_actions = None;
                            if interaction_state.transcript_mode {
                                exit_transcript_mode(&mut interaction_state);
                            }
                            dirty = true;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.is_empty() => {
                        if let Some(text) = selected_message_action_item(
                            &mut interaction_state,
                            &message_action_items,
                        )
                        .and_then(|item| message_action_copy_text(&item.message))
                        {
                            compact_banner = Some(
                                copy_text_with_fallback_notice(&text, "message")
                                    .unwrap_or_else(|error| format!("Copy failed: {error}")),
                            );
                        }
                        interaction_state.message_actions = None;
                        dirty = true;
                    }
                    KeyCode::Char('p') if key.modifiers.is_empty() => {
                        if let Some(primary_input) = selected_message_action_item(
                            &mut interaction_state,
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
                        dirty = true;
                    }
                    KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::PrevUser,
                        );
                    }
                    KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::NextUser,
                        );
                    }
                    KeyCode::Up => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Prev,
                        );
                    }
                    KeyCode::Down => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Next,
                        );
                    }
                    KeyCode::Char('k') if key.modifiers.is_empty() => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Prev,
                        );
                    }
                    KeyCode::Char('j') if key.modifiers.is_empty() => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Next,
                        );
                    }
                    KeyCode::Home => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Top,
                        );
                    }
                    KeyCode::End => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
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
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    sync_message_action_preview(
                        &state,
                        size.width,
                        size.height,
                        &interaction_state,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                }

                continue;
            }

            if interaction_state.prompt_history_search.is_some() {
                match key.code {
                    KeyCode::Esc => {
                        let _ =
                            cancel_prompt_history_search(&mut interaction_state, &mut input_buffer);
                        dirty = true;
                    }
                    KeyCode::Enter => {
                        let _ = accept_prompt_history_search(&mut interaction_state);
                        dirty = true;
                    }
                    KeyCode::Left => {
                        let search_state = interaction_state
                            .prompt_history_search
                            .as_mut()
                            .expect("prompt history search state should exist");
                        search_state.input_buffer.cursor =
                            search_state.input_buffer.cursor.saturating_sub(1);
                        dirty = true;
                    }
                    KeyCode::Right => {
                        let search_state = interaction_state
                            .prompt_history_search
                            .as_mut()
                            .expect("prompt history search state should exist");
                        search_state.input_buffer.cursor = (search_state.input_buffer.cursor + 1)
                            .min(search_state.input_buffer.chars.len());
                        dirty = true;
                    }
                    KeyCode::Home => {
                        let search_state = interaction_state
                            .prompt_history_search
                            .as_mut()
                            .expect("prompt history search state should exist");
                        search_state.input_buffer.cursor = 0;
                        dirty = true;
                    }
                    KeyCode::End => {
                        let search_state = interaction_state
                            .prompt_history_search
                            .as_mut()
                            .expect("prompt history search state should exist");
                        search_state.input_buffer.cursor = search_state.input_buffer.chars.len();
                        dirty = true;
                    }
                    KeyCode::Backspace => {
                        let should_cancel = interaction_state
                            .prompt_history_search
                            .as_ref()
                            .is_some_and(|search_state| search_state.input_buffer.is_empty());
                        if should_cancel {
                            let _ = cancel_prompt_history_search(
                                &mut interaction_state,
                                &mut input_buffer,
                            );
                        } else {
                            let search_state = interaction_state
                                .prompt_history_search
                                .as_mut()
                                .expect("prompt history search state should exist");
                            search_state.input_buffer.pop();
                            sync_prompt_history_search_preview(
                                &prompt_history,
                                search_state,
                                &mut input_buffer,
                            );
                        }
                        dirty = true;
                    }
                    KeyCode::Char(ch)
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        let search_state = interaction_state
                            .prompt_history_search
                            .as_mut()
                            .expect("prompt history search state should exist");
                        search_state.input_buffer.push(ch);
                        sync_prompt_history_search_preview(
                            &prompt_history,
                            search_state,
                            &mut input_buffer,
                        );
                        dirty = true;
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
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            interaction_state.transcript_search.open = false;
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            if interaction_state.transcript_search.active_item.is_none() {
                                interaction_state.transcript_search.reset();
                            }
                            dirty = true;
                        }
                        KeyCode::Left => {
                            interaction_state.transcript_search.input_buffer.cursor =
                                interaction_state
                                    .transcript_search
                                    .input_buffer
                                    .cursor
                                    .saturating_sub(1);
                            dirty = true;
                        }
                        KeyCode::Right => {
                            let input = &mut interaction_state.transcript_search.input_buffer;
                            input.cursor = (input.cursor + 1).min(input.chars.len());
                            dirty = true;
                        }
                        KeyCode::Backspace => {
                            interaction_state.transcript_search.input_buffer.pop();
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                        KeyCode::Char('n') if key.modifiers.is_empty() => {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            if step_transcript_search_match(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                                false,
                            ) {
                                dirty = true;
                            }
                        }
                        KeyCode::Char('N')
                            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                        {
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            if step_transcript_search_match(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                                true,
                            ) {
                                dirty = true;
                            }
                        }
                        KeyCode::Char(ch)
                            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                        {
                            interaction_state.transcript_search.input_buffer.push(ch);
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
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
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    let selectable_lines = transcript_selectable_lines_for_view(&state, size.width);
                    let _ = move_transcript_selection(
                        &mut interaction_state,
                        &selectable_lines,
                        selection_move,
                    );
                    sync_transcript_selection_preview(
                        &state,
                        size.width,
                        size.height,
                        &interaction_state,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                    continue;
                }

                if interaction_state.transcript_selection.is_some() {
                    if matches!(key.code, KeyCode::Esc) {
                        interaction_state.transcript_selection = None;
                        dirty = true;
                        continue;
                    }
                    if should_clear_transcript_selection_on_key(&key) {
                        interaction_state.transcript_selection = None;
                        dirty = true;
                    }
                }

                match key.code {
                    KeyCode::Esc => {
                        exit_transcript_mode(&mut interaction_state);
                        dirty = true;
                    }
                    KeyCode::Char('q') if key.modifiers.is_empty() => {
                        exit_transcript_mode(&mut interaction_state);
                        dirty = true;
                    }
                    KeyCode::Char('/')
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        interaction_state.message_actions = None;
                        open_transcript_search(
                            &mut interaction_state.transcript_search,
                            transcript_scroll,
                        );
                        dirty = true;
                    }
                    KeyCode::Char('n') if key.modifiers.is_empty() => {
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        if step_transcript_search_match(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            &mut transcript_scroll,
                            false,
                        ) {
                            dirty = true;
                        }
                    }
                    KeyCode::Char('N')
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        if step_transcript_search_match(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            &mut transcript_scroll,
                            true,
                        ) {
                            dirty = true;
                        }
                    }
                    KeyCode::Up => {
                        scroll_up(&mut transcript_scroll, 1);
                        dirty = true;
                    }
                    KeyCode::Down => {
                        scroll_down(&mut transcript_scroll, 1);
                        dirty = true;
                    }
                    KeyCode::PageUp => {
                        scroll_up(&mut transcript_scroll, 5);
                        dirty = true;
                    }
                    KeyCode::PageDown => {
                        scroll_down(&mut transcript_scroll, 5);
                        dirty = true;
                    }
                    KeyCode::Home => {
                        transcript_scroll = u16::MAX;
                        dirty = true;
                    }
                    KeyCode::End => {
                        transcript_scroll = 0;
                        dirty = true;
                    }
                    _ => {}
                }
                continue;
            }

            if vim_state.is_insert() {
                if let Some(selection_move) = prompt_selection_move_for_key(&key) {
                    if move_prompt_selection(
                        &mut interaction_state,
                        &mut input_buffer,
                        selection_move,
                    ) {
                        dirty = true;
                    }
                    continue;
                }
            }

            if interaction_state.prompt_selection.is_some() && matches!(key.code, KeyCode::Esc) {
                clear_prompt_selection(&mut interaction_state);
                dirty = true;
                continue;
            }

            match key.code {
                KeyCode::Esc => {
                    if vim_state.enabled {
                        if matches!(vim_state.mode, code_agent_ui::vim::VimMode::Insert) {
                            vim_state.enter_normal();
                        } else {
                            vim_state.mode = code_agent_ui::vim::VimMode::Normal(
                                code_agent_ui::vim::CommandState::Idle,
                            );
                        }
                        dirty = true;
                    }
                }
                KeyCode::Up => {
                    clear_prompt_selection(&mut interaction_state);
                    navigate_prompt_input_up(
                        registry,
                        &mut input_buffer,
                        &mut selected_command_suggestion,
                        &prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                    );
                    dirty = true;
                }
                KeyCode::Down => {
                    clear_prompt_selection(&mut interaction_state);
                    navigate_prompt_input_down(
                        registry,
                        &mut input_buffer,
                        &mut selected_command_suggestion,
                        &prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                    );
                    dirty = true;
                }
                KeyCode::PageUp => {
                    scroll_up(&mut transcript_scroll, 5);
                    dirty = true;
                }
                KeyCode::PageDown => {
                    scroll_down(&mut transcript_scroll, 5);
                    dirty = true;
                }
                KeyCode::Home if vim_state.is_insert() => {
                    dirty = set_prompt_cursor(&mut interaction_state, &mut input_buffer, 0);
                }
                KeyCode::End if vim_state.is_insert() => {
                    let end_cursor = input_buffer.chars.len();
                    dirty =
                        set_prompt_cursor(&mut interaction_state, &mut input_buffer, end_cursor);
                }
                KeyCode::Home => {
                    clear_prompt_selection(&mut interaction_state);
                    transcript_scroll = u16::MAX;
                    dirty = true;
                }
                KeyCode::End => {
                    clear_prompt_selection(&mut interaction_state);
                    transcript_scroll = 0;
                    dirty = true;
                }
                KeyCode::Left if vim_state.is_insert() => {
                    if let Some((start, _)) =
                        interaction_state
                            .prompt_selection
                            .as_ref()
                            .and_then(|selection| {
                                normalize_prompt_selection(selection, input_buffer.chars.len())
                            })
                    {
                        dirty = set_prompt_cursor(&mut interaction_state, &mut input_buffer, start);
                    } else {
                        let next_cursor = input_buffer.cursor.saturating_sub(1);
                        dirty = set_prompt_cursor(
                            &mut interaction_state,
                            &mut input_buffer,
                            next_cursor,
                        );
                    }
                }
                KeyCode::Right if vim_state.is_insert() => {
                    if let Some((_, end)) =
                        interaction_state
                            .prompt_selection
                            .as_ref()
                            .and_then(|selection| {
                                normalize_prompt_selection(selection, input_buffer.chars.len())
                            })
                    {
                        dirty = set_prompt_cursor(&mut interaction_state, &mut input_buffer, end);
                    } else {
                        let next_cursor = (input_buffer.cursor + 1).min(input_buffer.chars.len());
                        dirty = set_prompt_cursor(
                            &mut interaction_state,
                            &mut input_buffer,
                            next_cursor,
                        );
                    }
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if vim_state.is_insert() {
                        reset_prompt_history_navigation(
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                        let _ = delete_prompt_selection(&mut interaction_state, &mut input_buffer);
                        input_buffer.push(ch);
                        selected_command_suggestion = 0;
                        dirty = true;
                    } else {
                        if let code_agent_ui::vim::VimMode::Normal(ref mut cmd_state) =
                            vim_state.mode
                        {
                            let transition = code_agent_ui::vim::handle_normal_key(cmd_state, ch);
                            match transition {
                                code_agent_ui::vim::VimTransition::EnterInsert => {
                                    vim_state.enter_insert();
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::MoveCursor(delta) => {
                                    let mut new_pos = input_buffer.cursor as isize + delta;
                                    if new_pos < 0 {
                                        new_pos = 0;
                                    }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    if new_pos > max_pos as isize && !input_buffer.is_empty() {
                                        new_pos = max_pos as isize;
                                    }
                                    input_buffer.cursor = new_pos as usize;
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::SetCursor(pos) => {
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = pos.min(max_pos);
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::DeleteChars(mut amount) => {
                                    reset_prompt_history_navigation(
                                        &mut prompt_history_index,
                                        &mut prompt_history_draft,
                                    );
                                    while amount > 0
                                        && input_buffer.cursor < input_buffer.chars.len()
                                    {
                                        input_buffer.chars.remove(input_buffer.cursor);
                                        amount -= 1;
                                    }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = input_buffer.cursor.min(max_pos);
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::ReplaceChar(r) => {
                                    reset_prompt_history_navigation(
                                        &mut prompt_history_index,
                                        &mut prompt_history_draft,
                                    );
                                    if input_buffer.cursor < input_buffer.chars.len() {
                                        input_buffer.chars[input_buffer.cursor] = r;
                                    }
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::None => {}
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    let suggestions = sync_command_selection(
                        registry,
                        &input_buffer,
                        &mut selected_command_suggestion,
                    );
                    let prompt_text = input_buffer.as_str().trim().to_owned();
                    if prompt_text.is_empty() {
                        continue;
                    }
                    if let Some(selected) = suggestions.get(selected_command_suggestion) {
                        let selected_name = selected.name.as_str();
                        if prompt_text.starts_with('/')
                            && !prompt_text.contains(char::is_whitespace)
                            && prompt_text != selected_name
                        {
                            reset_prompt_history_navigation(
                                &mut prompt_history_index,
                                &mut prompt_history_draft,
                            );
                            clear_prompt_selection(&mut interaction_state);
                            apply_selected_command(&mut input_buffer, selected);
                            dirty = true;
                            continue;
                        }
                    }
                    clear_prompt_selection(&mut interaction_state);
                    input_buffer.clear();
                    match process_repl_submission(
                        &mut terminal,
                        store,
                        registry,
                        tool_registry,
                        &cwd,
                        plugin_root,
                        provider,
                        &mut active_model,
                        &mut repl_session,
                        raw_messages,
                        live_runtime,
                        prompt_text,
                        &mut input_buffer,
                        &mut prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                        &mut transcript_scroll,
                        &mut status_line,
                        &mut status_marquee_tick,
                        &mut active_pane,
                        &mut compact_banner,
                        &mut interaction_state,
                        &mut resume_picker,
                        &mut ide_picker,
                        &connected_ide_bridge,
                        &mut selected_command_suggestion,
                        &mut vim_state,
                        remote_mode,
                        ide_bridge_active,
                        &mut queued_submissions,
                    )
                    .await?
                    {
                        ReplSubmissionOutcome::Continue => {}
                        ReplSubmissionOutcome::Exit => break,
                    }
                    dirty = true;
                }
                KeyCode::Backspace => {
                    if vim_state.is_insert() {
                        reset_prompt_history_navigation(
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                        if !delete_prompt_selection(&mut interaction_state, &mut input_buffer) {
                            input_buffer.pop();
                        }
                        selected_command_suggestion = 0;
                    } else {
                        if input_buffer.cursor > 0 {
                            input_buffer.cursor -= 1;
                        }
                    }
                    dirty = true;
                }
                _ => {}
            }
        }

        Ok::<SessionId, anyhow::Error>(repl_session.session_id)
    }
    .await;

    disable_raw_mode().ok();
    if mouse_capture_enabled {
        execute!(
            terminal.backend_mut(),
            Show,
            crossterm::event::DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        )
        .ok();
    } else {
        execute!(
            terminal.backend_mut(),
            Show,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen
        )
        .ok();
    }
    loop_result
}
