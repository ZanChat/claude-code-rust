use super::*;

#[test]
fn parses_fixture_backed_slash_commands() {
    let fixture_path = workspace_root().join("fixtures/command-golden/slash-commands.json");
    let fixture =
        serde_json::from_str::<SlashCommandFixture>(&fs::read_to_string(fixture_path).unwrap())
            .unwrap();
    let registry = compatibility_command_registry();

    for case in fixture.cases {
        let parsed = registry.parse_slash_command(&case.input).unwrap();
        assert_eq!(parsed.name, case.name);
        assert_eq!(parsed.args, case.args);
    }
}

#[test]
fn builtin_commands_are_wired_for_repl_and_noninteractive_handlers() {
    let registry_commands = compatibility_command_registry()
        .all_owned()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    let repl_commands = repl_handled_command_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let noninteractive_commands = noninteractive_handled_command_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    assert_eq!(repl_commands, registry_commands);
    assert_eq!(noninteractive_commands, registry_commands);
}

#[test]
fn startup_screens_cover_first_run_and_project_setup() {
    let root = temp_session_root("startup-first-run");
    let session_root = root.join(".sessions");
    fs::create_dir_all(&session_root).unwrap();

    let screens = build_startup_screens(
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        &root,
        &session_root,
        None,
        true,
        Some("codex_auth_token"),
        &StartupPreferences::default(),
    );

    assert_eq!(screens.len(), 2);
    assert_eq!(screens[0].title, "Welcome");
    assert!(screens[0]
        .body
        .iter()
        .any(|line| line.contains("ratatui runtime")));
    assert!(screens[1]
        .body
        .iter()
        .any(|line| line.contains("CLAUDE.md") || line.contains("workspace is empty")));
}

#[test]
fn startup_screens_skip_completed_workspace() {
    let root = temp_session_root("startup-complete");
    let session_root = root.join(".sessions");
    fs::create_dir_all(&session_root).unwrap();
    write_test_file(&root.join("CLAUDE.md"), "# instructions\n");

    let screens = build_startup_screens(
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        &root,
        &session_root,
        None,
        true,
        Some("codex_auth_token"),
        &StartupPreferences { welcome_seen: true },
    );

    assert!(screens.is_empty());
}

#[test]
fn startup_ui_state_shows_prompt_and_scroll_state() {
    let app = code_agent_ui::RatatuiApp::new("startup");
    let screens = vec![super::StartupScreen {
        title: "Setup".to_owned(),
        body: vec!["line one".to_owned(), "line two".to_owned()],
        preview: code_agent_ui::PanePreview {
            title: "Next".to_owned(),
            lines: vec!["step".to_owned()],
        },
    }];

    let state = build_startup_ui_state(
        &app,
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        Path::new("/tmp/project"),
        &screens,
        0,
        2,
    );

    assert!(state.show_input);
    assert_eq!(state.transcript_scroll, 2);
    assert_eq!(
        state.prompt_helper.as_deref(),
        Some("Type to enter the REPL immediately. Enter also continues.")
    );
    assert_eq!(state.active_pane, Some(code_agent_ui::PaneKind::Transcript));
    assert!(state
        .header_context
        .as_deref()
        .is_some_and(|value| value.contains("/tmp/project")));
}

#[test]
fn task_entries_for_ui_preserve_parent_child_structure() {
    let now = current_time_ms();
    let worker_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();

    let mut root = TaskRecord::new("workflow", "Review workspace");
    root.status = TaskStatus::Running;
    root.updated_at_unix_ms = now - 2_000;
    root.agent_id = Some(worker_id);

    let mut child_running = TaskRecord::new("workflow_step", "Inspect failing tests");
    child_running.parent_task_id = Some(root.id);
    child_running.status = TaskStatus::Running;
    child_running.input = Some("Open the failing fixture".to_owned());
    child_running.updated_at_unix_ms = now - 1_500;
    child_running.metadata.insert(
        "blocked_by".to_owned(),
        "11111111-1111-4111-8111-111111111111, 22222222-2222-4222-8222-222222222222".to_owned(),
    );

    let mut child_recent = TaskRecord::new("workflow_step", "Summarize blockers");
    child_recent.parent_task_id = Some(root.id);
    child_recent.status = TaskStatus::Completed;
    child_recent.output = Some("Missing integration fixture".to_owned());
    child_recent.updated_at_unix_ms = now - 500;

    let mut follow_up = TaskRecord::new("task", "Follow up with maintainer");
    follow_up.status = TaskStatus::Pending;
    follow_up.updated_at_unix_ms = now - 10_000;

    let root_id = root.id.to_string();
    let entries = task_entries_for_ui(vec![follow_up, child_recent, root, child_running]);

    assert_eq!(entries[0].title, "Review workspace");
    assert_eq!(entries[0].tree_prefix, "");
    assert_eq!(entries[0].owner_label.as_deref(), Some("aaaaaaaa"));
    assert_eq!(
        entries.last().map(|entry| entry.title.as_str()),
        Some("Follow up with maintainer")
    );

    let child_entries = entries
        .iter()
        .filter(|entry| entry.parent_id.as_deref() == Some(root_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(child_entries.len(), 2);
    let running_child = child_entries
        .iter()
        .find(|entry| entry.title == "Inspect failing tests")
        .expect("running child should exist");
    let completed_child = child_entries
        .iter()
        .find(|entry| entry.title == "Summarize blockers")
        .expect("completed child should exist");
    assert_eq!(running_child.tree_prefix, "└─ ");
    assert_eq!(completed_child.tree_prefix, "├─ ");
    assert_eq!(
        running_child.blocker_labels,
        vec!["11111111".to_owned(), "22222222".to_owned()]
    );
    assert!(child_entries.iter().any(|entry| entry.is_recent_completion));
}

#[test]
fn command_suggestions_follow_slash_prefixes() {
    let registry = compatibility_command_registry();
    let mut input = code_agent_ui::InputBuffer::new();
    input.replace("/h");

    let suggestions = command_suggestions(&registry, &input);

    assert!(!suggestions.is_empty());
    assert_eq!(suggestions[0].name, "/help");
    assert!(suggestions.iter().all(|entry| entry.name.starts_with("/h")));
}

#[test]
fn command_suggestions_stop_after_command_arguments_start() {
    let registry = compatibility_command_registry();
    let mut input = code_agent_ui::InputBuffer::new();
    input.replace(format!("/model {DEFAULT_OPENAI_REASONING_MODEL}"));

    let suggestions = command_suggestions(&registry, &input);

    assert!(suggestions.is_empty());
}

#[test]
fn prompt_up_prioritizes_command_suggestions_before_history() {
    let registry = compatibility_command_registry();
    let mut input = code_agent_ui::InputBuffer::new();
    input.replace("/m");
    let history = vec!["older prompt".to_owned()];
    let mut selected_command_suggestion = 0usize;
    let mut history_index = None;
    let mut history_draft = None;

    let suggestions = command_suggestions(&registry, &input);
    assert!(suggestions.len() > 1);

    navigate_prompt_input_up(
        &registry,
        &mut input,
        &mut selected_command_suggestion,
        &history,
        &mut history_index,
        &mut history_draft,
    );

    assert_eq!(input.as_str(), "/m");
    assert_eq!(selected_command_suggestion, suggestions.len() - 1);
    assert!(history_index.is_none());
    assert!(history_draft.is_none());
}

#[test]
fn prompt_down_prioritizes_command_suggestions_before_history() {
    let registry = compatibility_command_registry();
    let mut input = code_agent_ui::InputBuffer::new();
    input.replace("/m");
    let history = vec!["older prompt".to_owned()];
    let mut selected_command_suggestion = 0usize;
    let mut history_index = None;
    let mut history_draft = None;

    let suggestions = command_suggestions(&registry, &input);
    assert!(suggestions.len() > 1);

    navigate_prompt_input_down(
        &registry,
        &mut input,
        &mut selected_command_suggestion,
        &history,
        &mut history_index,
        &mut history_draft,
    );

    assert_eq!(input.as_str(), "/m");
    assert_eq!(selected_command_suggestion, 1);
    assert!(history_index.is_none());
    assert!(history_draft.is_none());
}

#[test]
fn pane_shortcut_accepts_supported_platform_modifiers() {
    let plain = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);
    assert!(pane_from_shortcut(&plain).is_none());

    let control_shortcut = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL);
    assert_eq!(
        pane_from_shortcut(&control_shortcut),
        Some(code_agent_ui::PaneKind::Transcript)
    );

    let super_shortcut = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::SUPER);
    assert_eq!(
        pane_from_shortcut(&super_shortcut),
        Some(code_agent_ui::PaneKind::Transcript)
    );

    let alt_shortcut = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
    assert_eq!(
        pane_from_shortcut(&alt_shortcut),
        Some(code_agent_ui::PaneKind::Transcript)
    );

    let shifted_control_shortcut = KeyEvent::new(
        KeyCode::Char('1'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(pane_from_shortcut(&shifted_control_shortcut).is_none());
}

#[test]
fn pane_shortcut_accepts_apple_terminal_option_symbols() {
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('¡'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::Transcript)
    );
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('™'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::Diff)
    );
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('£'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::FileViewer)
    );
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('¢'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::Tasks)
    );
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('∞'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::Permissions)
    );
    assert_eq!(
        pane_from_shortcut_for_terminal(
            &KeyEvent::new(KeyCode::Char('§'), KeyModifiers::NONE),
            Some("Apple_Terminal")
        ),
        Some(code_agent_ui::PaneKind::Logs)
    );
    assert!(pane_from_shortcut_for_terminal(
        &KeyEvent::new(KeyCode::Char('¡'), KeyModifiers::NONE),
        Some("vscode")
    )
    .is_none());
}

#[test]
fn selection_copy_shortcut_matches_explicit_copy_bindings() {
    let terminal_copy = KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(is_selection_copy_shortcut(&terminal_copy));

    let kitty_copy = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER);
    assert!(is_selection_copy_shortcut(&kitty_copy));

    let plain_ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!is_selection_copy_shortcut(&plain_ctrl_c));
}

#[test]
fn paste_shortcut_matches_expected_bindings() {
    let super_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER);
    assert!(is_paste_shortcut(&super_v));

    let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert!(is_paste_shortcut(&ctrl_v));

    let shift_insert = KeyEvent::new(KeyCode::Insert, KeyModifiers::SHIFT);
    assert!(is_paste_shortcut(&shift_insert));

    let plain_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE);
    assert!(!is_paste_shortcut(&plain_v));
}

#[test]
fn repl_shortcut_routing_is_modifier_specific() {
    let mut interaction_state = ReplInteractionState::default();

    let ctrl_shift_c = KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_shift_c, &interaction_state),
        None
    );

    interaction_state.transcript_selection = Some(TranscriptSelectionState {
        anchor: TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        },
        focus: TranscriptSelectionPoint {
            line_index: 0,
            column: 1,
        },
    });
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_shift_c, &interaction_state),
        Some(ReplShortcutAction::CopySelection)
    );

    interaction_state.transcript_selection = None;
    interaction_state.prompt_selection = Some(PromptSelectionState {
        anchor: 1,
        focus: 3,
    });
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_shift_c, &interaction_state),
        Some(ReplShortcutAction::CopySelection)
    );

    interaction_state.transcript_mode = true;
    interaction_state.transcript_search.open = true;
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_o, &interaction_state),
        Some(ReplShortcutAction::ToggleTranscriptMode)
    );

    let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_e, &interaction_state),
        Some(ReplShortcutAction::ToggleTranscriptDetails)
    );
}

#[test]
fn ctrl_r_enters_prompt_history_search_only_from_prompt_mode() {
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    let interaction_state = ReplInteractionState::default();

    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_r, &interaction_state),
        Some(ReplShortcutAction::PromptHistorySearch)
    );

    let mut transcript_state = ReplInteractionState::default();
    transcript_state.transcript_mode = true;
    assert_eq!(
        repl_shortcut_action_for_key(&ctrl_r, &transcript_state),
        None
    );
}

#[test]
fn prompt_selection_extracts_selected_input_text() {
    let mut input_buffer = code_agent_ui::InputBuffer::new();
    input_buffer.replace("abcdef");
    input_buffer.cursor = 1;
    let mut interaction_state = ReplInteractionState::default();

    assert!(move_prompt_selection(
        &mut interaction_state,
        &mut input_buffer,
        PromptSelectionMove::Right,
    ));
    assert!(move_prompt_selection(
        &mut interaction_state,
        &mut input_buffer,
        PromptSelectionMove::Right,
    ));

    assert_eq!(
        interaction_state.prompt_selection,
        Some(PromptSelectionState {
            anchor: 1,
            focus: 3
        })
    );
    assert_eq!(
        prompt_selection_text(&input_buffer, &interaction_state).as_deref(),
        Some("bc")
    );
}

#[test]
fn delete_prompt_selection_removes_selected_range() {
    let mut input_buffer = code_agent_ui::InputBuffer::new();
    input_buffer.replace("abcdef");
    let mut interaction_state = ReplInteractionState::default();
    interaction_state.prompt_selection = Some(PromptSelectionState {
        anchor: 2,
        focus: 5,
    });

    assert!(delete_prompt_selection(
        &mut interaction_state,
        &mut input_buffer
    ));

    assert_eq!(input_buffer.as_str(), "abf");
    assert_eq!(input_buffer.cursor, 2);
    assert!(interaction_state.prompt_selection.is_none());
}

#[test]
fn insert_prompt_text_replaces_selected_range() {
    let mut input_buffer = code_agent_ui::InputBuffer::new();
    input_buffer.replace("abcdef");
    let mut interaction_state = ReplInteractionState::default();
    interaction_state.prompt_selection = Some(PromptSelectionState {
        anchor: 1,
        focus: 4,
    });

    assert!(insert_prompt_text(
        &mut interaction_state,
        &mut input_buffer,
        "XYZ",
    ));

    assert_eq!(input_buffer.as_str(), "aXYZef");
    assert_eq!(input_buffer.cursor, 4);
    assert!(interaction_state.prompt_selection.is_none());
}

#[test]
fn prompt_mouse_drag_updates_cursor_and_selection() {
    let mut input_buffer = code_agent_ui::InputBuffer::new();
    input_buffer.replace("abcdef");
    let mut interaction_state = ReplInteractionState::default();

    assert!(handle_prompt_mouse_action(
        &MouseEventKind::Down(MouseButton::Left),
        1,
        &mut interaction_state,
        &mut input_buffer,
    ));
    assert_eq!(input_buffer.cursor, 1);
    assert_eq!(interaction_state.prompt_mouse_anchor, Some(1));
    assert!(interaction_state.prompt_selection.is_none());

    assert!(handle_prompt_mouse_action(
        &MouseEventKind::Drag(MouseButton::Left),
        4,
        &mut interaction_state,
        &mut input_buffer,
    ));
    assert_eq!(input_buffer.cursor, 4);
    assert_eq!(
        interaction_state.prompt_selection,
        Some(PromptSelectionState {
            anchor: 1,
            focus: 4,
        })
    );

    assert!(!handle_prompt_mouse_action(
        &MouseEventKind::Up(MouseButton::Left),
        4,
        &mut interaction_state,
        &mut input_buffer,
    ));
    assert!(interaction_state.prompt_mouse_anchor.is_none());
}

#[test]
fn repl_exit_detection_accepts_plain_and_slash_forms() {
    assert!(should_exit_repl("quit"));
    assert!(should_exit_repl("exit"));
    assert!(should_exit_repl("/quit"));
    assert!(should_exit_repl("/exit"));
    assert!(!should_exit_repl("please exit"));
}

#[test]
fn prompt_history_seeds_from_user_messages_only() {
    let session_id = SessionId::new_v4();
    let messages = vec![
        build_text_message(session_id, MessageRole::User, "first".to_owned(), None),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "ignore".to_owned(),
            None,
        ),
        build_text_message(session_id, MessageRole::User, "second".to_owned(), None),
        build_text_message(session_id, MessageRole::User, "second".to_owned(), None),
        build_text_message(session_id, MessageRole::User, "first".to_owned(), None),
    ];

    let history = prompt_history_from_messages(&messages);

    assert_eq!(history, vec!["first", "second", "first"]);
}

#[test]
fn prompt_history_navigation_restores_draft_after_latest_entry() {
    let history = vec!["alpha".to_owned(), "beta".to_owned()];
    let mut input = code_agent_ui::InputBuffer::new();
    input.replace("draft");
    let mut history_index = None;
    let mut history_draft = None;

    assert!(navigate_prompt_history_up(
        &history,
        &mut input,
        &mut history_index,
        &mut history_draft
    ));
    assert_eq!(input.as_str(), "beta");
    assert_eq!(history_index, Some(1));

    assert!(navigate_prompt_history_up(
        &history,
        &mut input,
        &mut history_index,
        &mut history_draft
    ));
    assert_eq!(input.as_str(), "alpha");
    assert_eq!(history_index, Some(0));

    assert!(navigate_prompt_history_down(
        &history,
        &mut input,
        &mut history_index,
        &mut history_draft
    ));
    assert_eq!(input.as_str(), "beta");
    assert_eq!(history_index, Some(1));

    assert!(navigate_prompt_history_down(
        &history,
        &mut input,
        &mut history_index,
        &mut history_draft
    ));
    assert_eq!(input.as_str(), "draft");
    assert_eq!(history_index, None);
    assert!(history_draft.is_none());
}

#[test]
fn prompt_history_search_matches_are_newest_first_and_unique() {
    let history = vec![
        "alpha build".to_owned(),
        "beta fix".to_owned(),
        "alpha build".to_owned(),
        "gamma".to_owned(),
    ];

    let matches = prompt_history_search_matches(&history, "alpha");

    assert_eq!(matches, vec![2]);
}

#[test]
fn prompt_history_search_preview_cancel_and_accept_follow_ts_behavior() {
    let history = vec![
        "beta one".to_owned(),
        "alpha beta".to_owned(),
        "beta two".to_owned(),
    ];
    let mut input_buffer = code_agent_ui::InputBuffer::new();
    input_buffer.replace("draft prompt");
    let mut interaction_state = ReplInteractionState::default();

    open_prompt_history_search(&mut interaction_state, &input_buffer);
    let search_state = interaction_state
        .prompt_history_search
        .as_mut()
        .expect("search state should exist");
    search_state.input_buffer.replace("beta");
    sync_prompt_history_search_preview(&history, search_state, &mut input_buffer);

    assert_eq!(input_buffer.as_str(), "beta two");
    assert_eq!(search_state.ui_state().active_match, Some(1));
    assert_eq!(search_state.ui_state().match_count, 3);
    assert!(!search_state.ui_state().failed_match);

    let _ = step_prompt_history_search_match(&history, search_state, &mut input_buffer);
    assert_eq!(input_buffer.as_str(), "alpha beta");
    assert_eq!(search_state.ui_state().active_match, Some(2));

    let _ = step_prompt_history_search_match(&history, search_state, &mut input_buffer);
    assert_eq!(input_buffer.as_str(), "beta one");
    assert_eq!(search_state.ui_state().active_match, Some(3));

    assert!(!step_prompt_history_search_match(
        &history,
        search_state,
        &mut input_buffer,
    ));
    assert!(search_state.ui_state().failed_match);
    assert_eq!(input_buffer.as_str(), "beta one");

    assert!(accept_prompt_history_search(&mut interaction_state));
    assert!(interaction_state.prompt_history_search.is_none());
    assert_eq!(input_buffer.as_str(), "beta one");

    open_prompt_history_search(&mut interaction_state, &input_buffer);
    let search_state = interaction_state
        .prompt_history_search
        .as_mut()
        .expect("search state should exist");
    search_state.input_buffer.replace("nomatch");
    sync_prompt_history_search_preview(&history, search_state, &mut input_buffer);
    assert!(search_state.ui_state().failed_match);
    assert_eq!(input_buffer.as_str(), "beta one");

    assert!(cancel_prompt_history_search(
        &mut interaction_state,
        &mut input_buffer,
    ));
    assert!(interaction_state.prompt_history_search.is_none());
    assert_eq!(input_buffer.as_str(), "beta one");
}
