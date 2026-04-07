use super::*;
use crate::UsageTotals;

#[tokio::test]
async fn continue_flag_resolves_latest_session_explicitly() {
    let root = temp_session_root("continue-latest");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root));
    let session_id = SessionId::new_v4();
    let persisted = build_text_message(session_id, MessageRole::User, "resume me".to_owned(), None);
    store.append_message(session_id, &persisted).await.unwrap();

    let mut cli = Cli::default();
    resolve_continue_target(&mut cli, &store).await.unwrap();
    assert!(cli.resume.is_none());

    cli.continue_latest = true;
    resolve_continue_target(&mut cli, &store).await.unwrap();
    assert_eq!(cli.resume, Some(session_id.to_string()));
}

#[tokio::test]
async fn continue_flag_errors_when_no_session_exists() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("continue-none")));
    let mut cli = Cli {
        continue_latest: true,
        ..Cli::default()
    };

    let error = resolve_continue_target(&mut cli, &store).await.unwrap_err();

    assert!(error
        .to_string()
        .contains("No conversation found to continue"));
}

#[test]
fn choose_active_session_starts_new_session_without_explicit_resume() {
    let cli = Cli::default();

    let (session_id, transcript_path, messages) = choose_active_session(&cli, None).unwrap();

    assert!(transcript_path.is_none());
    assert!(messages.is_empty());
    assert_ne!(session_id, SessionId::nil());
}

#[tokio::test]
async fn logout_report_includes_resume_command() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("logout-resume")));
    let session_id = SessionId::new_v4();
    let report = render_auth_command_with_resume(
        ApiProvider::ChatGPTCodex,
        "logout",
        Some(super::ResumeTargetHint {
            session_id,
            transcript_path: store.transcript_path(session_id).await.unwrap(),
        }),
    )
    .await
    .unwrap();
    let output: serde_json::Value = serde_json::from_str(&report).unwrap();

    assert_eq!(
        output
            .get("resume_session_id")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        Some(session_id.to_string())
    );
    assert_eq!(
        output
            .get("resume_command")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        Some(format!("ccrust --resume {session_id}"))
    );
}

#[tokio::test]
async fn resume_hint_text_matches_repl_exit_message() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("resume-hint")));
    let session_id = SessionId::new_v4();
    let transcript_path = store.transcript_path(session_id).await.unwrap();
    fs::write(&transcript_path, "{}\n").unwrap();

    let hint = ResumeTargetHint {
        session_id,
        transcript_path,
    };

    assert_eq!(
        resume_hint_text(&hint),
        Some(format!(
            "\nResume this session with:\nccrust --resume {session_id}\n"
        ))
    );
}

#[tokio::test]
async fn resume_hint_text_skips_missing_transcript() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "resume-hint-missing",
    )));
    let session_id = SessionId::new_v4();
    let hint = ResumeTargetHint {
        session_id,
        transcript_path: store.transcript_path(session_id).await.unwrap(),
    };

    assert_eq!(resume_hint_text(&hint), None);
}

#[test]
fn build_repl_ui_state_handles_empty_command_suggestions() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let input = ccrust_ui::InputBuffer::new();
    let state = build_repl_ui_state(
        &app,
        &registry,
        &[],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        UsageTotals::default(),
        &input,
        "status",
        None,
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    assert!(state.show_input);
    assert_eq!(state.transcript_scroll, 0);
    assert!(state.selected_command_suggestion.is_none());
    assert!(state.command_suggestions.is_empty());
    assert_eq!(
        state.header_subtitle.as_deref(),
        Some("gpt-5.4 · chatgpt-codex")
    );
}

#[test]
fn build_repl_ui_state_adds_session_and_total_usage_to_header_context() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let mut assistant =
        build_text_message(session_id, MessageRole::Assistant, "Done.".to_owned(), None);
    assistant.metadata.usage = Some(ccrust_core::TokenUsage {
        input_tokens: 12,
        output_tokens: 4,
        cache_creation_input_tokens: 2,
        cache_read_input_tokens: 2,
    });

    let state = build_repl_ui_state(
        &app,
        &registry,
        &[assistant],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals {
            input_tokens: 40,
            output_tokens: 10,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 1,
            response_count: 3,
        },
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    let header_context = state.header_context.unwrap_or_default();
    assert!(header_context.contains("session 20 tok (2 cached)"));
    assert!(header_context.contains("total 54 tok (1 cached)"));
}

#[test]
fn build_repl_ui_state_groups_pending_steps() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "inspect".to_owned(), None);
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "calling list_dir".to_owned(),
        Some(user.id),
    );
    let tool = build_tool_result_message(
        session_id,
        "tool-call-1".to_owned(),
        "src\nCargo.toml".to_owned(),
        false,
        Some(assistant.id),
    );
    let messages = vec![user, assistant, tool];
    let pending_view = PendingReplView {
        messages: materialize_runtime_messages(&messages),
        transcript_overlay_messages: Vec::new(),
        spinner_verb: "Running list_dir".to_owned(),
        progress_label: "running list_dir".to_owned(),
        steps: vec![PendingReplStep {
            step: 1,
            start_index: 1,
            status_label: "running list_dir".to_owned(),
            status_detail: Some("src/main.rs".to_owned()),
            task_status: TaskStatus::Running,
            expanded: false,
            touched: false,
        }],
        queued_inputs: vec!["follow up after this".to_owned()],
        show_transcript_details: false,
    };

    let state = build_repl_ui_state(
        &app,
        &registry,
        &messages,
        Some(&pending_view),
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        Some("working".to_owned()),
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    assert_eq!(state.transcript_lines.len(), 1);
    assert_eq!(state.transcript_groups.len(), 1);
    assert!(!state.transcript_groups[0].expanded);
    assert_eq!(
        state.transcript_groups[0].title,
        "Step 1 · running list_dir · src/main.rs"
    );
    assert_eq!(state.pending_step_count, 1);
    assert!(!state.pending_transcript_details);
    assert!(!state.task_items.is_empty());
    assert_eq!(state.task_items[0].status, TaskStatus::Running);
    assert!(state.task_items[0]
        .title
        .contains("Step 1 · running list_dir"));
    assert_eq!(state.queued_inputs, vec!["follow up after this".to_owned()]);
}

#[test]
fn build_repl_ui_state_shows_pending_overlay_messages_in_transcript() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "main task".to_owned(), None);
    let command = build_repl_command_input_message(session_id, Some(user.id), "/btw what changed?");
    let pending_view = PendingReplView {
        messages: materialize_runtime_messages(std::slice::from_ref(&user)),
        transcript_overlay_messages: vec![
            command.clone(),
            build_repl_command_output_message(
                session_id,
                Some(command.id),
                "btw",
                "This is the side-question answer.\nIt should stay fully visible while expanded.",
            ),
        ],
        spinner_verb: "Working".to_owned(),
        progress_label: "working".to_owned(),
        steps: vec![PendingReplStep {
            step: 1,
            start_index: 1,
            status_label: "working".to_owned(),
            status_detail: None,
            task_status: TaskStatus::Running,
            expanded: false,
            touched: false,
        }],
        queued_inputs: Vec::new(),
        show_transcript_details: false,
    };

    let state = build_repl_ui_state(
        &app,
        &registry,
        std::slice::from_ref(&user),
        Some(&pending_view),
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        Some("working".to_owned()),
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState {
            expanded_history_groups: BTreeSet::from([pending_transcript_group_id(&command)]),
            ..ReplInteractionState::default()
        },
    );

    let rendered = ccrust_ui::render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("/btw what changed?"));
    assert!(rendered.contains("/btw"));
    assert!(rendered.contains("This is the side-question answer."));
    assert!(rendered.contains("It should stay fully visible while expanded."));
}

#[test]
fn build_repl_ui_state_collapses_history_tool_runs_into_single_group() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let user = build_text_message(
        session_id,
        MessageRole::User,
        "inspect repo".to_owned(),
        None,
    );
    let read_call = build_tool_call_message(
        session_id,
        "tool-call-read",
        "read_file",
        r#"{"file_path":"src/main.rs"}"#,
        Some(user.id),
    );
    let read_result = build_tool_result_message(
        session_id,
        "tool-call-read".to_owned(),
        "fn main() {}".to_owned(),
        false,
        Some(read_call.id),
    );
    let search_call = build_tool_call_message(
        session_id,
        "tool-call-search",
        "grep_search",
        r#"{"pattern":"build_repl_ui_state"}"#,
        Some(read_result.id),
    );
    let search_result = build_tool_result_message(
        session_id,
        "tool-call-search".to_owned(),
        "crates/cli/src/main.rs: build_repl_ui_state".to_owned(),
        false,
        Some(search_call.id),
    );
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "Found the relevant flow.".to_owned(),
        Some(search_result.id),
    );
    let messages = vec![
        user,
        read_call,
        read_result,
        search_call,
        search_result,
        assistant,
    ];

    let state = build_repl_ui_state(
        &app,
        &registry,
        &messages,
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    assert_eq!(state.transcript_items.len(), 3);
    let TranscriptItem::Group(group) = &state.transcript_items[1] else {
        panic!("expected grouped history item");
    };
    assert!(group.single_item);
    assert!(!group.expanded);
    assert!(group.title.contains("Read 1 file"));
    assert!(group.title.contains("searched 1 query"));
}

#[test]
fn build_repl_ui_state_allows_clicking_history_group_title_text() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let user = build_text_message(
        session_id,
        MessageRole::User,
        "inspect repo".to_owned(),
        None,
    );
    let read_call = build_tool_call_message(
        session_id,
        "tool-call-read",
        "read_file",
        r#"{"file_path":"src/main.rs"}"#,
        Some(user.id),
    );
    let read_result = build_tool_result_message(
        session_id,
        "tool-call-read".to_owned(),
        "fn main() {}".to_owned(),
        false,
        Some(read_call.id),
    );
    let search_call = build_tool_call_message(
        session_id,
        "tool-call-search",
        "grep_search",
        r#"{"pattern":"build_repl_ui_state"}"#,
        Some(read_result.id),
    );
    let search_result = build_tool_result_message(
        session_id,
        "tool-call-search".to_owned(),
        "crates/cli/src/main.rs: build_repl_ui_state".to_owned(),
        false,
        Some(search_call.id),
    );
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "Found the relevant flow.".to_owned(),
        Some(search_result.id),
    );
    let messages = vec![
        user,
        read_call,
        read_result,
        search_call,
        search_result,
        assistant,
    ];

    let state = build_repl_ui_state(
        &app,
        &registry,
        &messages,
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    let TranscriptItem::Group(group) = &state.transcript_items[1] else {
        panic!("expected grouped history item");
    };
    let expected_action = ccrust_ui::UiMouseAction::ToggleTranscriptGroup(group.id.clone());

    let mut saw_title_hit = false;
    for row in 0..24 {
        for column in 4..100 {
            if ccrust_ui::mouse_action_for_position(&state, 100, 24, column, row)
                == Some(expected_action.clone())
            {
                saw_title_hit = true;
                break;
            }
        }
        if saw_title_hit {
            break;
        }
    }

    assert!(saw_title_hit);
}

#[test]
fn build_repl_ui_state_keeps_message_action_indices_for_grouped_history() {
    let app = ccrust_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let user = build_text_message(
        session_id,
        MessageRole::User,
        "inspect repo".to_owned(),
        None,
    );
    let read_call = build_tool_call_message(
        session_id,
        "tool-call-read",
        "read_file",
        r#"{"file_path":"src/main.rs"}"#,
        Some(user.id),
    );
    let read_result = build_tool_result_message(
        session_id,
        "tool-call-read".to_owned(),
        "fn main() {}".to_owned(),
        false,
        Some(read_call.id),
    );
    let messages = vec![user, read_call, read_result];
    let mut interaction_state = ReplInteractionState::default();
    interaction_state.message_actions = Some(super::ReplMessageActionState { selected_item: 1 });

    let state = build_repl_ui_state(
        &app,
        &registry,
        &messages,
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &interaction_state,
    );

    assert_eq!(
        state
            .message_actions
            .as_ref()
            .and_then(|actions| actions.primary_input_label.as_deref()),
        Some("path")
    );
}

#[test]
fn pending_interrupt_messages_preserve_partial_preview_before_marker() {
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "inspect".to_owned(), None);
    let partial = build_text_message(
        session_id,
        MessageRole::Assistant,
        "partial answer".to_owned(),
        Some(user.id),
    );
    let pending_view = PendingReplView {
        messages: vec![user.clone(), partial.clone()],
        transcript_overlay_messages: Vec::new(),
        spinner_verb: "Working".to_owned(),
        progress_label: "Working".to_owned(),
        steps: Vec::new(),
        queued_inputs: Vec::new(),
        show_transcript_details: false,
    };

    let interrupt_messages = pending_interrupt_messages(session_id, &[user], &pending_view);

    assert_eq!(interrupt_messages.len(), 2);
    assert_eq!(message_text(&interrupt_messages[0]), "partial answer");
    assert_eq!(interrupt_messages[1].role, MessageRole::User);
    assert_eq!(
        message_text(&interrupt_messages[1]),
        "[Request interrupted by user]"
    );
}

#[test]
fn toggle_pending_repl_transcript_details_switches_visibility() {
    let pending_view = Arc::new(Mutex::new(PendingReplView {
        messages: Vec::new(),
        transcript_overlay_messages: Vec::new(),
        spinner_verb: "Working".to_owned(),
        progress_label: "Working".to_owned(),
        steps: vec![
            PendingReplStep {
                step: 1,
                start_index: 0,
                status_label: "working".to_owned(),
                status_detail: None,
                task_status: TaskStatus::Running,
                expanded: false,
                touched: false,
            },
            PendingReplStep {
                step: 2,
                start_index: 0,
                status_label: "working".to_owned(),
                status_detail: None,
                task_status: TaskStatus::Completed,
                expanded: true,
                touched: false,
            },
        ],
        queued_inputs: Vec::new(),
        show_transcript_details: false,
    }));

    toggle_pending_repl_transcript_details(&pending_view);
    {
        let state = pending_view.lock().unwrap();
        assert!(state.show_transcript_details);
        assert!(state.steps[1].expanded);
        assert!(state.steps.iter().all(|entry| !entry.touched));
    }

    toggle_pending_repl_transcript_details(&pending_view);
    let state = pending_view.lock().unwrap();
    assert!(!state.show_transcript_details);
    assert!(state.steps.iter().all(|entry| !entry.expanded));
}

#[test]
fn toggle_pending_repl_group_updates_visibility_state() {
    let pending_view = Arc::new(Mutex::new(PendingReplView {
        messages: Vec::new(),
        transcript_overlay_messages: Vec::new(),
        spinner_verb: "Working".to_owned(),
        progress_label: "Working".to_owned(),
        steps: vec![PendingReplStep {
            step: 1,
            start_index: 0,
            status_label: "working".to_owned(),
            status_detail: None,
            task_status: TaskStatus::Running,
            expanded: false,
            touched: false,
        }],
        queued_inputs: Vec::new(),
        show_transcript_details: false,
    }));

    toggle_pending_repl_group(&pending_view, "pending-step-1");
    {
        let state = pending_view.lock().unwrap();
        assert!(state.show_transcript_details);
        assert!(state.steps[0].expanded);
        assert!(state.steps[0].touched);
    }

    toggle_pending_repl_group(&pending_view, "pending-step-1");
    let state = pending_view.lock().unwrap();
    assert!(!state.show_transcript_details);
    assert!(!state.steps[0].expanded);
}

#[test]
fn toggle_all_history_transcript_groups_expands_and_collapses() {
    let group_ids = vec!["history-group-a".to_owned(), "history-group-b".to_owned()];
    let mut interaction_state = ReplInteractionState::default();

    assert!(toggle_all_history_transcript_groups(
        &mut interaction_state,
        &group_ids,
    ));
    assert_eq!(
        interaction_state.expanded_history_groups,
        group_ids.iter().cloned().collect()
    );

    assert!(toggle_all_history_transcript_groups(
        &mut interaction_state,
        &group_ids,
    ));
    assert!(interaction_state.expanded_history_groups.is_empty());
}

#[test]
fn prompt_file_picker_lists_matching_workspace_files() {
    let root = temp_session_root("file-picker-list");
    write_test_file(&root.join("src/main.rs"), "fn main() {}\n");
    write_test_file(&root.join("src/lib.rs"), "pub fn helper() {}\n");

    let mut input_buffer = ccrust_ui::InputBuffer::new();
    input_buffer.replace("inspect @src/ma");
    let mut interaction_state = ReplInteractionState::default();

    let choice_list =
        prompt_file_picker_choice_list(&root, &input_buffer, &mut interaction_state).unwrap();

    assert_eq!(choice_list.title, "File picker");
    assert_eq!(choice_list.selected, 0);
    assert_eq!(choice_list.items[0].label, "src/main.rs");
}

#[test]
fn prompt_file_picker_inserts_selected_match_into_prompt() {
    let root = temp_session_root("file-picker-apply");
    write_test_file(&root.join("src/main.rs"), "fn main() {}\n");

    let mut input_buffer = ccrust_ui::InputBuffer::new();
    input_buffer.replace("inspect @src/ma");
    let mut interaction_state = ReplInteractionState::default();

    assert!(handle_prompt_file_picker_key(
        &root,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut input_buffer,
        &mut interaction_state,
    ));
    assert_eq!(input_buffer.as_str(), "inspect @src/main.rs ");
}

#[test]
fn ide_picker_lists_matching_workspace_bridges() {
    let home = temp_session_root("ide-picker-home");
    let workspace = home.join("workspace");
    fs::create_dir_all(workspace.join("src")).unwrap();
    write_test_file(
        &home.join(".claude/ide/48123.lock"),
        &json!({
            "workspaceFolders": [workspace.display().to_string()],
            "ideName": "VS Code",
            "transport": "ws"
        })
        .to_string(),
    );

    let picker = repl_ide_picker_state_with_home(&workspace, None, Some(&home));
    let choice_list = build_ide_choice_list(&picker, None);

    assert_eq!(choice_list.title, "IDE bridge");
    assert_eq!(choice_list.selected, 0);
    assert_eq!(choice_list.items[0].label, "VS Code");
    assert_eq!(
        choice_list.items[0].detail.as_deref(),
        Some("ide://127.0.0.1:48123")
    );
}

#[tokio::test]
async fn skills_picker_lists_discovered_skills() {
    let root = temp_session_root("skills-picker");
    write_test_file(&root.join(".claude/skills/review/SKILL.md"), "# Review\n");

    let picker = repl_skills_picker_state(&root, None).await.unwrap();
    let choice_list = build_command_choice_list(&picker);

    assert_eq!(choice_list.title, "Skills");
    assert_eq!(choice_list.items.len(), 1);
    assert_eq!(choice_list.items[0].label, "/review");
    match &picker.items[0].action {
        ReplCommandPickerAction::PrefillInput { input, .. } => {
            assert_eq!(input, "/review ");
        }
        other => panic!("unexpected picker action: {other:?}"),
    }
}

#[test]
fn agents_picker_includes_create_new_option() {
    let root = temp_session_root("agents-picker");
    let store = task_store_for(&root);
    let mut task = TaskRecord::new("agent", "Review auth flow");
    task.status = TaskStatus::Running;
    store.create_task(task).unwrap();

    let picker = repl_agents_picker_state(&root).unwrap();
    let choice_list = build_command_choice_list(&picker);

    assert_eq!(choice_list.title, "Agents");
    assert_eq!(choice_list.items[0].label, "Create new agent");
    assert_eq!(choice_list.items[1].label, "Review auth flow");
}

#[test]
fn hooks_picker_lists_manifest_entries() {
    let root = temp_session_root("hooks-picker");
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "hook-tools",
              "hooks": ["./hooks/pre_tool.json", { "postToolUse": true }]
            }"#,
    );

    let picker = repl_hooks_picker_state(&root, None);
    let choice_list = build_command_choice_list(&picker);

    assert_eq!(choice_list.title, "Hooks");
    assert_eq!(choice_list.items.len(), 2);
    assert_eq!(choice_list.items[0].label, "hooks/pre_tool.json");
    assert!(choice_list.items[1].label.contains("Inline hook config #2"));
}

#[test]
fn message_actions_show_expand_and_collapse_for_history_groups() {
    let session_id = SessionId::new_v4();
    let tool_call = build_tool_call_message(
        session_id,
        "call-1",
        "read_file",
        r#"{"path":"src/lib.rs"}"#,
        None,
    );
    let tool_result = build_tool_result_message(
        session_id,
        "call-1".to_owned(),
        "pub fn helper() {}".to_owned(),
        false,
        None,
    );
    let runtime_messages = materialize_runtime_messages(&vec![tool_call, tool_result]);
    let mut interaction_state = ReplInteractionState::default();
    let items = message_action_items_from_runtime(&runtime_messages, None, &interaction_state);

    assert!(enter_message_actions(&mut interaction_state, &items));
    assert_eq!(
        message_actions_ui_state(&interaction_state, &items)
            .and_then(|state| state.enter_label)
            .as_deref(),
        Some("expand")
    );

    let group_id = items[0]
        .history_group_id
        .clone()
        .expect("history group item should expose a group id");
    interaction_state.expanded_history_groups.insert(group_id);

    assert_eq!(
        message_actions_ui_state(&interaction_state, &items)
            .and_then(|state| state.enter_label)
            .as_deref(),
        Some("collapse")
    );
}

#[test]
fn build_repl_ui_state_hides_prompt_in_transcript_mode() {
    let app = ccrust_ui::RatatuiApp::new("repl-transcript");
    let registry = compatibility_command_registry();
    let mut search_input = ccrust_ui::InputBuffer::new();
    search_input.replace("error");
    let interaction_state = ReplInteractionState {
        transcript_mode: true,
        expanded_history_groups: BTreeSet::new(),
        transcript_search: super::ReplTranscriptSearchState {
            input_buffer: search_input,
            open: true,
            active_item: Some(0),
            ..Default::default()
        },
        prompt_history_search: None,
        message_actions: None,
        prompt_selection: None,
        prompt_mouse_anchor: None,
        transcript_selection: None,
        ..Default::default()
    };

    let state = build_repl_ui_state(
        &app,
        &registry,
        &[],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Tasks,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &interaction_state,
    );

    assert!(!state.show_input);
    assert!(state.transcript_mode);
    assert_eq!(state.active_pane, Some(ccrust_ui::PaneKind::Transcript));
    assert!(state.transcript_search.is_some());
}

#[test]
fn build_repl_ui_state_keeps_prompt_visible_for_message_actions() {
    let app = ccrust_ui::RatatuiApp::new("repl-message-actions");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let assistant_tool_call = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            call: ccrust_core::ToolCall {
                id: "tool-call-1".to_owned(),
                name: "read_file".to_owned(),
                input_json: json!({
                    "path": "src/main.rs"
                })
                .to_string(),
                thought_signature: None,
            },
        }],
    );
    let interaction_state = ReplInteractionState {
        message_actions: Some(super::ReplMessageActionState { selected_item: 0 }),
        ..Default::default()
    };

    let state = build_repl_ui_state(
        &app,
        &registry,
        &[assistant_tool_call],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        session_id,
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Tasks,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &interaction_state,
    );

    assert!(state.show_input);
    assert_eq!(state.active_pane, Some(ccrust_ui::PaneKind::Transcript));
    assert_eq!(
        state
            .message_actions
            .as_ref()
            .and_then(|actions| actions.primary_input_label.as_deref()),
        Some("path")
    );
    assert_eq!(
        state
            .message_actions
            .as_ref()
            .and_then(|actions| actions.enter_label.as_deref()),
        Some("expand")
    );
}

#[test]
fn build_repl_ui_state_exposes_transcript_selection() {
    let app = ccrust_ui::RatatuiApp::new("repl-selection");
    let registry = compatibility_command_registry();
    let interaction_state = ReplInteractionState {
        transcript_mode: true,
        transcript_selection: Some(TranscriptSelectionState {
            anchor: TranscriptSelectionPoint {
                line_index: 0,
                column: 2,
            },
            focus: TranscriptSelectionPoint {
                line_index: 0,
                column: 5,
            },
        }),
        ..Default::default()
    };

    let state = build_repl_ui_state(
        &app,
        &registry,
        &[build_text_message(
            SessionId::new_v4(),
            MessageRole::Assistant,
            "selection target".to_owned(),
            None,
        )],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        UsageTotals::default(),
        &ccrust_ui::InputBuffer::new(),
        "status",
        None,
        ccrust_ui::PaneKind::Tasks,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &interaction_state,
    );

    assert_eq!(
        state.transcript_selection,
        interaction_state.transcript_selection
    );
}

#[test]
fn message_action_copy_prefers_tool_primary_input() {
    let assistant_tool_call = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            call: ccrust_core::ToolCall {
                id: "tool-call-1".to_owned(),
                name: "run_in_terminal".to_owned(),
                input_json: json!({
                    "command": "cargo test -p ccrust-ui"
                })
                .to_string(),
                thought_signature: None,
            },
        }],
    );

    assert_eq!(
        message_primary_input(&assistant_tool_call)
            .as_ref()
            .map(|input| input.label),
        Some("command")
    );
    assert_eq!(
        message_action_copy_text(&assistant_tool_call).as_deref(),
        Some("cargo test -p ccrust-ui")
    );
}

#[tokio::test]
async fn config_migrate_reports_compatibility_inputs() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("config-migrate")));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let invocation = CommandInvocation {
        name: "config".to_owned(),
        args: vec!["migrate".to_owned()],
        raw_input: "/config migrate".to_owned(),
    };
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        invocation,
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("\"provider\": \"openai-compatible\""));
}

#[tokio::test]
async fn repl_config_command_reports_runtime_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-config")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = "claude-sonnet-4-6".to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

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
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("provider=firstParty"));
    assert!(status.contains("runtime=offline"));
}

#[tokio::test]
async fn repl_config_command_reports_openai_routing_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "repl-config-openai-routing",
    )));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = "gpt-5.4".to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

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
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("provider=openai-compatible"));
    assert!(status.contains("api_mode=auto"));
    assert!(status.contains("transport=auto"));
}

#[tokio::test]
async fn repl_ide_command_reports_bridge_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-ide")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = "claude-sonnet-4-6".to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let disconnected = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "ide".to_owned(),
            raw_input: "/ide".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::FirstParty,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    let connected = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "ide".to_owned(),
            raw_input: "/ide".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::FirstParty,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        true,
    )
    .await
    .unwrap();

    assert!(!disconnected.contains("\"status\": \"connected\""));
    assert!(connected.contains("\"status\": \"connected\""));
}

#[test]
fn command_pickers_queue_real_theme_fast_and_effort_commands() {
    let home = temp_session_root("picker-settings-home");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let theme_picker = repl_theme_picker_state();
        assert_eq!(theme_picker.title, "Theme");
        match &theme_picker.items[0].action {
            ReplCommandPickerAction::QueueInput { input, .. } => {
                assert_eq!(input, "/theme auto");
            }
            other => panic!("expected queue input action, got {other:?}"),
        }

        let fast_picker = repl_fast_picker_state(DEFAULT_OPENAI_REASONING_MODEL);
        assert_eq!(fast_picker.title, "Fast mode");
        match &fast_picker.items[0].action {
            ReplCommandPickerAction::QueueInput { input, .. } => {
                assert_eq!(input, "/fast on");
            }
            other => panic!("expected queue input action, got {other:?}"),
        }

        let effort_picker = repl_effort_picker_state();
        assert_eq!(effort_picker.title, "Effort");
        assert!(effort_picker.items.iter().any(|entry| matches!(
            entry.action,
            ReplCommandPickerAction::QueueInput { ref input, .. } if input == "/effort max"
        )));
    });
}

#[test]
fn rewind_picker_lists_user_turns_from_latest_to_oldest() {
    let session_id = SessionId::new_v4();
    let messages = vec![
        build_text_message(
            session_id,
            MessageRole::User,
            "first prompt".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "first reply".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::User,
            "second prompt".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "second reply".to_owned(),
            None,
        ),
    ];

    let picker = repl_rewind_picker_state(&messages);

    assert_eq!(picker.title, "Rewind");
    assert_eq!(picker.items.len(), 2);
    assert_eq!(picker.items[0].item.label, "Turn 2");
    assert_eq!(
        picker.items[0].item.detail.as_deref(),
        Some("second prompt")
    );
    match &picker.items[0].action {
        ReplCommandPickerAction::QueueInput { input, .. } => {
            assert_eq!(input, "/rewind 3");
        }
        other => panic!("expected queue input action, got {other:?}"),
    }
}

#[test]
fn resume_picker_item_prefers_saved_title_and_tag() {
    let root = temp_session_root("resume-picker-metadata");
    let transcript_path = root.join("session.jsonl");
    write_test_file(&transcript_path, "");
    update_session_metadata_for_path(&transcript_path, |metadata| {
        metadata.custom_title = Some("Auth handoff".to_owned());
        metadata.tag = Some("bugfix".to_owned());
    })
    .unwrap();

    let item = resume_picker_item(&SessionSummary {
        session_id: SessionId::new_v4(),
        transcript_path,
        modified_at_unix_ms: 0,
        message_count: 4,
        first_prompt: "fallback prompt".to_owned(),
    });

    assert!(item.label.contains("Auth handoff"));
    assert!(item
        .detail
        .as_deref()
        .is_some_and(|value| value.contains("#bugfix")));
}
