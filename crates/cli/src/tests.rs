use super::{
    accept_prompt_history_search, append_pending_repl_overlay_ui_event,
    append_provider_error_message, apply_managed_login_env, apply_resume_message_cutoff,
    apply_runtime_prompt_metrics, build_command_choice_list, build_ide_choice_list,
    build_prompt_command_user_message, build_repl_command_input_message,
    build_repl_command_output_message, build_repl_ui_state, build_resume_choice_list,
    build_runtime_system_prompt, build_startup_screens, build_startup_ui_state, build_text_message,
    build_tool_result_message, cancel_prompt_history_search, choose_active_session,
    combined_prompt_history, command_suggestions, connect_remote_endpoint_for_cli, current_time_ms,
    delete_prompt_selection, enter_message_actions, execute_local_turn,
    execute_local_turn_with_options, execute_local_turn_with_user_message_options,
    handle_prompt_file_picker_key, handle_prompt_mouse_action, handle_repl_slash_command,
    insert_onboarding_input_text, insert_prompt_text, is_paste_shortcut,
    is_selection_copy_shortcut, load_command_settings, load_session_metadata_for_path,
    maybe_notify_auto_connected_ide, message_action_copy_text, message_action_items_from_runtime,
    message_actions_ui_state, message_primary_input, message_text, move_prompt_selection,
    move_transcript_selection, navigate_prompt_history_down, navigate_prompt_history_up,
    navigate_prompt_input_down, navigate_prompt_input_up, open_prompt_history_search,
    pane_from_shortcut, pane_from_shortcut_for_terminal, parse_cli_from, parse_open_command_args,
    parse_server_command_args, pending_btw_context_messages, pending_btw_provider_request,
    pending_btw_question, pending_interrupt_messages, pending_transcript_group_id,
    project_ccrust_env_path, prompt_file_picker_choice_list, prompt_history_from_messages,
    prompt_history_search_matches, prompt_selection_text, provider_error_transcript_text,
    render_advisor_command, render_auth_command_with_resume, render_chrome_command,
    render_command_help, render_effort_command, render_fast_command, render_ide_command_with_home,
    render_remote_control_command, render_session_command, render_theme_command,
    render_usage_command, repl_agents_picker_state, repl_effort_picker_state,
    repl_fast_picker_state, repl_hooks_picker_state, repl_ide_picker_state_with_home,
    repl_keyboard_enhancement_flags, repl_rewind_picker_state, repl_shortcut_action_for_key,
    repl_skills_picker_state, repl_theme_picker_state, resolve_continue_target,
    resolve_launch_provider, resolve_prompt_command_prompt, resolved_command_registry,
    resumable_sessions, resume_command_for_session, resume_hint_text, resume_picker_item,
    run_pending_btw_side_question, should_append_pending_interrupt_message,
    should_echo_command_result_in_footer, should_exit_repl, should_launch_interactive_repl,
    step_prompt_history_search_match, sync_prompt_history_search_preview,
    sync_transcript_selection_preview, task_entries_for_ui, task_store_for,
    toggle_all_history_transcript_groups, toggle_pending_repl_group,
    toggle_pending_repl_transcript_details, transcript_selection_move_for_key,
    ts_top_level_cli_option_specs, update_session_metadata_for_path, user_ccrust_env_path,
    validate_root_print_mode, ActiveSessionStore, Cli, CompatOptionSpec, LaunchProviderSource,
    LocalBridgeHandler, ManagedLoginConfigState, Message, MessageRole, PendingReplStep,
    PendingReplView, PromptSelectionMove, ReplCommandPickerAction, ReplInteractionState,
    ReplMessageActionState, ReplSessionState, ReplShortcutAction, ReplTranscriptSearchState,
    ResumePickerState, ResumeTargetHint, RuntimeCliOptions, RuntimeSystemPromptMetrics,
    StartupPreferences, StartupScreen, TranscriptSelectionMove,
};
use crate::cli_contract::{build_cli_contract, FastPathCommand, OutputMode, TopLevelCommand};
use crate::cli_graph::{
    render_root_help_text, render_unknown_option_error, top_level_command_specs,
};
use crate::commands::should_enable_mouse_capture;
use crate::top_level_commands::{
    install_binary_from_source, render_completion_command, render_export_cli_command,
    render_task_cli_command, rollback_from_state, InstallPaths,
};
use ccrust_bridge::{
    base64_encode, serve_direct_session, AssistantDirective, BridgeServerConfig,
    BridgeSessionHandler, RemoteEnvelope, RemotePermissionResponse, ResumeSessionRequest,
    VoiceFrame,
};
use ccrust_core::{
    compatibility_command_registry, CommandInvocation, CommandSource, ContentBlock, SessionId,
    TaskRecord, TaskStatus, TaskStore, ToolCall,
};
use ccrust_providers::{
    ApiProvider, ProviderRequestError, DEFAULT_OPENAI_COMPLETION_MODEL,
    DEFAULT_OPENAI_REASONING_MODEL,
};
use ccrust_session::{materialize_runtime_messages, LocalSessionStore, SessionSummary};
use ccrust_tools::{compatibility_tool_registry, ToolPermissionMode};
use ccrust_ui::{
    transcript_selectable_lines_for_view, PromptSelectionState, RatatuiApp, TranscriptItem,
    TranscriptLine, TranscriptSelectionPoint, TranscriptSelectionState,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

fn repl_session_state(session_id: SessionId) -> ReplSessionState {
    ReplSessionState {
        session_id,
        transcript_path: None,
    }
}

fn build_tool_call_message(
    session_id: SessionId,
    tool_call_id: &str,
    tool_name: &str,
    input_json: &str,
    parent_id: Option<uuid::Uuid>,
) -> Message {
    let mut message = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            call: ToolCall {
                id: tool_call_id.to_owned(),
                name: tool_name.to_owned(),
                input_json: input_json.to_owned(),
                thought_signature: None,
            },
        }],
    );
    message.session_id = Some(session_id);
    message.parent_id = parent_id;
    message
}

fn sample_args_for_ts_option(spec: &CompatOptionSpec) -> Vec<String> {
    let mut args = vec![format!("--{}", spec.canonical)];
    args.extend(spec.sample_values.iter().map(|value| (*value).to_owned()));
    args
}

struct EchoBridgeHandler;

#[async_trait::async_trait]
impl BridgeSessionHandler for EchoBridgeHandler {
    async fn on_envelope(
        &mut self,
        envelope: &RemoteEnvelope,
    ) -> anyhow::Result<Vec<RemoteEnvelope>> {
        match envelope {
            RemoteEnvelope::Message { message } => Ok(vec![RemoteEnvelope::Message {
                message: build_text_message(
                    message.session_id.unwrap_or_else(SessionId::new_v4),
                    MessageRole::Assistant,
                    format!("echo: {}", message_text(message)),
                    Some(message.id),
                ),
            }]),
            _ => Ok(vec![RemoteEnvelope::Ack {
                note: "ignored".to_owned(),
            }]),
        }
    }
}

#[test]
fn root_help_matches_golden_fixture() {
    let fixture_path = workspace_root().join("fixtures/command-golden/cli-root-help.txt");
    let expected = fs::read_to_string(fixture_path).unwrap();

    assert_eq!(render_root_help_text(), expected.trim_end());
}

#[test]
fn unknown_option_error_matches_golden_fixture() {
    let fixture_path = workspace_root().join("fixtures/command-golden/cli-unknown-option.txt");
    let expected = fs::read_to_string(fixture_path).unwrap();

    assert_eq!(render_unknown_option_error("--bogus"), expected.trim_end());
}

#[test]
fn root_help_lists_visible_top_level_commands() {
    let help = render_root_help_text();

    for spec in top_level_command_specs().iter().filter(|spec| !spec.hidden) {
        assert!(
            help.contains(spec.primary),
            "missing {} in root help",
            spec.primary
        );
    }
}

#[test]
fn root_help_omits_hidden_and_removed_top_level_commands() {
    let help = render_root_help_text();
    for command in ["remote-control"] {
        assert!(
            !help.contains(command),
            "{command} should not appear in root help"
        );
    }
}

#[test]
fn root_help_includes_restored_real_top_level_commands_and_provider_flag() {
    let help = render_root_help_text();
    for command in [
        "update",
        "install",
        "rollback",
        "task",
        "export",
        "completion",
    ] {
        assert!(
            help.contains(command),
            "{command} should appear in root help"
        );
    }
    assert!(help.contains("--provider <provider>"));
}

#[test]
fn parse_cli_accepts_all_ts_top_level_flags() {
    for spec in ts_top_level_cli_option_specs() {
        let args = sample_args_for_ts_option(spec);
        let cli = parse_cli_from(args.clone()).unwrap_or_else(|error| {
            panic!("failed to parse {}: {error}", args.join(" "));
        });
        assert!(
            cli.compat_flag_enabled(spec.canonical),
            "expected {} to be recorded",
            spec.canonical
        );
        assert!(
            cli.prompt.is_empty(),
            "{} should not become prompt text",
            spec.canonical
        );
    }
}

#[test]
fn parse_cli_accepts_optional_ts_flags_without_values() {
    for canonical in [
        "debug",
        "from-pr",
        "remote",
        "remote-control",
        "resume",
        "tasks",
        "teleport",
        "worktree",
    ] {
        let cli = parse_cli_from(vec![format!("--{canonical}")]).unwrap();
        assert!(cli.compat_flag_enabled(canonical));
    }

    let cli = parse_cli_from(vec!["--resume".to_owned()]).unwrap();
    assert!(cli.continue_latest);
    assert!(cli.resume.is_none());
}

#[test]
fn parse_cli_accepts_ts_aliases_and_short_forms() {
    let allowed_tools = parse_cli_from(vec![
        "--allowedTools".to_owned(),
        "Read".to_owned(),
        "Edit".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        allowed_tools.compat_values.get("allowed-tools"),
        Some(&vec!["Read".to_owned(), "Edit".to_owned()])
    );

    let remote_control = parse_cli_from(vec!["--rc".to_owned(), "desk".to_owned()]).unwrap();
    assert_eq!(
        remote_control.compat_values.get("remote-control"),
        Some(&vec!["desk".to_owned()])
    );

    let debug = parse_cli_from(vec!["-d".to_owned(), "trace".to_owned()]).unwrap();
    assert_eq!(
        debug.compat_values.get("debug"),
        Some(&vec!["trace".to_owned()])
    );

    let print = parse_cli_from(vec!["-p".to_owned()]).unwrap();
    assert!(print.compat_flag_enabled("print"));

    let resume = parse_cli_from(vec!["-r".to_owned(), "session-123".to_owned()]).unwrap();
    assert_eq!(resume.resume.as_deref(), Some("session-123"));

    let worktree = parse_cli_from(vec!["-w".to_owned(), "branch".to_owned()]).unwrap();
    assert_eq!(
        worktree.compat_values.get("worktree"),
        Some(&vec!["branch".to_owned()])
    );
}

#[test]
fn parse_cli_rejects_unknown_options() {
    let error = parse_cli_from(vec!["--not-a-real-flag".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("unknown option"));
}

#[test]
fn parse_cli_preserves_top_level_subcommand_tokens() {
    let cli = parse_cli_from(vec![
        "mcp".to_owned(),
        "list".to_owned(),
        "--json".to_owned(),
    ])
    .unwrap();

    assert_eq!(
        cli.command_tokens,
        vec!["mcp".to_owned(), "list".to_owned(), "--json".to_owned()]
    );
    assert!(cli.prompt.is_empty());
}

#[test]
fn parse_cli_keeps_slash_command_names_as_prompt_text() {
    for prompt in [
        "review this diff",
        "btw explain the error",
        "plan the refactor",
    ] {
        let cli = parse_cli_from(vec![prompt.to_owned()]).unwrap();
        assert!(
            cli.command_tokens.is_empty(),
            "{prompt} should stay a prompt"
        );
        assert_eq!(cli.prompt, vec![prompt.to_owned()]);
    }
}

#[test]
fn cli_contract_classifies_root_print_mode() {
    let cli = parse_cli_from(vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "summarize this diff".to_owned(),
    ])
    .unwrap();
    let contract = build_cli_contract(&cli);

    assert!(contract.global.print_mode.enabled);
    assert_eq!(
        contract.global.print_mode.output_mode,
        OutputMode::StreamJson
    );
    assert!(matches!(
        contract.command,
        TopLevelCommand::RootPrompt {
            prompt: Some(ref prompt)
        } if prompt == "summarize this diff"
    ));
}

#[test]
fn cli_contract_promotes_top_level_command_help() {
    let cli = parse_cli_from(vec!["plugin".to_owned(), "--help".to_owned()]).unwrap();
    let contract = build_cli_contract(&cli);

    assert!(matches!(
        contract.command,
        TopLevelCommand::Named { ref name, .. } if name == "plugin"
    ));
    assert!(matches!(
        contract.fast_path,
        Some(FastPathCommand::SubcommandHelp { .. })
    ));
}

#[test]
fn validate_root_print_mode_rejects_invalid_stream_json_combinations() {
    let cli = parse_cli_from(vec![
        "--input-format".to_owned(),
        "stream-json".to_owned(),
        "hello".to_owned(),
    ])
    .unwrap();
    let contract = build_cli_contract(&cli);
    let error = validate_root_print_mode(&contract).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: --input-format=stream-json requires output-format=stream-json."
    );

    let cli = parse_cli_from(vec![
        "--print".to_owned(),
        "--include-partial-messages".to_owned(),
        "hello".to_owned(),
    ])
    .unwrap();
    let contract = build_cli_contract(&cli);
    let error = validate_root_print_mode(&contract).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: --include-partial-messages requires --print and --output-format=stream-json."
    );

    let cli = parse_cli_from(vec![
        "--print".to_owned(),
        "--replay-user-messages".to_owned(),
        "hello".to_owned(),
    ])
    .unwrap();
    let contract = build_cli_contract(&cli);
    let error = validate_root_print_mode(&contract).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: --replay-user-messages requires both --input-format=stream-json and --output-format=stream-json."
    );
}

#[test]
fn apply_resume_message_cutoff_truncates_after_matching_assistant() {
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "first".to_owned(), None);
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "done".to_owned(),
        Some(user.id),
    );
    let trailing = build_text_message(
        session_id,
        MessageRole::User,
        "second".to_owned(),
        Some(assistant.id),
    );
    let mut messages = vec![user, assistant.clone(), trailing];

    apply_resume_message_cutoff(&mut messages, Some(&assistant.id.to_string())).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages.last().unwrap().id, assistant.id);
}

#[test]
fn server_fast_path_parses_host_and_port() {
    let parsed = parse_server_command_args(&[
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        "8123".to_owned(),
    ])
    .unwrap();

    assert_eq!(parsed.bind_address, "tcp://127.0.0.1:8123");
}

#[test]
fn task_cli_create_and_list_round_trip() {
    let root = temp_session_root("task-cli-round-trip");

    let created = render_task_cli_command(
        &root,
        &[
            "create".to_owned(),
            "ship-docs".to_owned(),
            "-d".to_owned(),
            "write readme updates".to_owned(),
        ],
    )
    .unwrap();
    let created_task: TaskRecord = serde_json::from_str(&created).unwrap();
    assert_eq!(created_task.title, "ship-docs");
    assert_eq!(created_task.input.as_deref(), Some("write readme updates"));

    let listed = render_task_cli_command(&root, &["list".to_owned(), "--json".to_owned()]).unwrap();
    let report: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(report["count"], json!(1));
    assert_eq!(report["tasks"][0]["id"], json!(created_task.id));
}

#[test]
fn task_cli_update_supports_subject_status_and_owner() {
    let root = temp_session_root("task-cli-update");
    let created =
        render_task_cli_command(&root, &["create".to_owned(), "draft".to_owned()]).unwrap();
    let created_task: TaskRecord = serde_json::from_str(&created).unwrap();

    let updated = render_task_cli_command(
        &root,
        &[
            "update".to_owned(),
            created_task.id.to_string(),
            "--subject".to_owned(),
            "done".to_owned(),
            "--status".to_owned(),
            "completed".to_owned(),
            "--owner".to_owned(),
            "reviewer-1".to_owned(),
        ],
    )
    .unwrap();
    let updated_task: TaskRecord = serde_json::from_str(&updated).unwrap();
    assert_eq!(updated_task.title, "done");
    assert_eq!(updated_task.status, TaskStatus::Completed);
    assert_eq!(
        updated_task.metadata.get("owner").map(String::as_str),
        Some("reviewer-1")
    );
}

#[tokio::test]
async fn export_cli_writes_rendered_transcript_text() {
    let root = temp_session_root("export-cli");
    let store = ActiveSessionStore::new(root.clone(), Some(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "hello".to_owned(), None);
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "world".to_owned(),
        Some(user.id),
    );
    store.append_message(session_id, &user).await.unwrap();
    store.append_message(session_id, &assistant).await.unwrap();

    let output_path = root.join("exports/out.txt");
    let message = render_export_cli_command(
        &store,
        &[session_id.to_string(), output_path.display().to_string()],
    )
    .await
    .unwrap();
    assert!(message.contains("Exported transcript"));
    let written = fs::read_to_string(output_path).unwrap();
    assert!(written.contains("User:"));
    assert!(written.contains("hello"));
    assert!(written.contains("Assistant:"));
    assert!(written.contains("world"));
}

#[test]
fn completion_command_generates_bash_script() {
    let script = render_completion_command(&["bash".to_owned()]).unwrap();
    assert!(script.contains("complete -F _ccrust_completion ccrust"));
    assert!(script.contains("task"));
    assert!(script.contains("--provider"));
}

#[test]
fn install_and_rollback_helpers_round_trip_local_binary() {
    let root = temp_session_root("install-rollback");
    let source_a = root.join("source-a");
    let source_b = root.join("source-b");
    write_test_file(&source_a, "binary-a");
    write_test_file(&source_b, "binary-b");
    let paths = InstallPaths {
        destination_path: root.join("bin/ccrust"),
        state_path: root.join("state/install-state.json"),
        snapshot_dir: root.join("state/history"),
    };

    let first = install_binary_from_source(&paths, &source_a, "a", true, "install").unwrap();
    assert!(!first.skipped);
    assert_eq!(
        fs::read_to_string(&paths.destination_path).unwrap(),
        "binary-a"
    );

    let second = install_binary_from_source(&paths, &source_b, "b", true, "install").unwrap();
    assert!(second.previous_snapshot.is_some());
    assert_eq!(
        fs::read_to_string(&paths.destination_path).unwrap(),
        "binary-b"
    );

    let rollback = rollback_from_state(&paths, Some("1"), false, false).unwrap();
    assert!(!rollback.dry_run);
    assert!(rollback.restored_snapshot.is_some());
    assert_eq!(
        fs::read_to_string(&paths.destination_path).unwrap(),
        "binary-a"
    );
}

#[test]
fn open_fast_path_requires_headless_mode() {
    let error = parse_open_command_args(&["127.0.0.1:8123".to_owned()], OutputMode::Text, false)
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("interactive open is not implemented yet"));
}

#[test]
fn open_fast_path_normalizes_raw_tcp_targets() {
    let parsed = parse_open_command_args(
        &[
            "127.0.0.1:8123".to_owned(),
            "-p".to_owned(),
            "ping".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
        ],
        OutputMode::Text,
        false,
    )
    .unwrap();

    assert_eq!(parsed.address, "tcp://127.0.0.1:8123");
    assert_eq!(parsed.prompt.as_deref(), Some("ping"));
    assert_eq!(parsed.output_mode, OutputMode::StreamJson);
}

#[test]
fn open_fast_path_rejects_cc_urls() {
    let error = parse_open_command_args(
        &["cc://host".to_owned(), "-p".to_owned()],
        OutputMode::Text,
        false,
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("cc:// direct-connect URLs are not implemented yet"));
}

#[tokio::test]
async fn connect_remote_endpoint_helper_exchanges_prompt_messages() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let bind_address = format!("tcp://{address}");
    let server_task = tokio::spawn(async move {
        serve_direct_session(
            BridgeServerConfig {
                bind_address,
                ..BridgeServerConfig::default()
            },
            EchoBridgeHandler,
        )
        .await
    });
    tokio::task::yield_now().await;

    let cli = Cli {
        bridge_receive_count: Some(1),
        ..Cli::default()
    };
    let session_id = SessionId::new_v4();
    let inbound = connect_remote_endpoint_for_cli(
        &cli,
        &format!("tcp://{address}"),
        session_id,
        Some("status please".to_owned()),
    )
    .await
    .unwrap();
    let record = server_task.await.unwrap().unwrap();

    assert!(record.envelopes.iter().any(
        |envelope| matches!(envelope, RemoteEnvelope::Message { message } if message.role == MessageRole::User)
    ));
    assert!(inbound.iter().any(|envelope| {
        matches!(envelope, RemoteEnvelope::Message { message } if message_text(message) == "echo: status please")
    }));
}

#[test]
fn runtime_cli_options_map_permission_modes() {
    let denied =
        parse_cli_from(vec!["--permission-mode".to_owned(), "dontAsk".to_owned()]).unwrap();
    assert_eq!(
        denied.runtime_options().tool_permission_mode,
        Some(ToolPermissionMode::Deny)
    );

    let ask = parse_cli_from(vec![
        "--permission-mode".to_owned(),
        "acceptEdits".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        ask.runtime_options().tool_permission_mode,
        Some(ToolPermissionMode::Ask)
    );

    let allowed = parse_cli_from(vec!["--dangerously-skip-permissions".to_owned()]).unwrap();
    assert_eq!(
        allowed.runtime_options().tool_permission_mode,
        Some(ToolPermissionMode::Allow)
    );

    let max_turns = parse_cli_from(vec!["--max-turns".to_owned(), "3".to_owned()]).unwrap();
    assert_eq!(max_turns.runtime_options().max_turns, Some(3));
}

#[tokio::test]
async fn execute_local_turn_respects_max_turns_override() {
    let root = temp_session_root("execute-local-turn-max-turns");
    let file_path = root.join("example.txt");
    write_test_file(&file_path, "alpha\n");

    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let tool_registry = compatibility_tool_registry();
    let mut raw_messages = Vec::new();
    let runtime_options = RuntimeCliOptions {
        max_turns: Some(1),
        tool_permission_mode: None,
    };

    let (_, turn_count, stop_reason, _, _) = execute_local_turn_with_options(
        &store,
        &tool_registry,
        root.clone(),
        None,
        ApiProvider::Gemini,
        "gemini-2.5-pro".to_owned(),
        session_id,
        &mut raw_messages,
        format!(
            "tool:file_read {}",
            json!({ "path": file_path.to_string_lossy() })
        ),
        false,
        &runtime_options,
        None,
    )
    .await
    .unwrap();

    assert_eq!(turn_count, 1);
    assert_eq!(stop_reason.as_deref(), Some("max_turns"));
    assert_eq!(raw_messages.len(), 3);
}

#[test]
fn prompt_history_uses_raw_prompt_command_input_and_session_entries_sort_last() {
    let mut prompt_command_message = Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "<command-name>debug</command-name>".to_owned(),
        }],
    );
    prompt_command_message.metadata.attributes.insert(
        ccrust_core::PROMPT_COMMAND_RAW_INPUT_ATTRIBUTE.to_owned(),
        "/debug failing test".to_owned(),
    );

    let history = prompt_history_from_messages(&[prompt_command_message]);
    assert_eq!(history, vec!["/debug failing test".to_owned()]);

    assert_eq!(
        combined_prompt_history(
            &["/debug failing test".to_owned()],
            &[
                "/global older prompt".to_owned(),
                "/debug failing test".to_owned(),
            ],
        ),
        vec![
            "/global older prompt".to_owned(),
            "/debug failing test".to_owned(),
        ]
    );
}

#[tokio::test]
async fn prompt_command_user_message_hides_expanded_prompt_in_transcript_but_sends_it_to_provider()
{
    let root = temp_session_root("hidden-prompt-command");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let tool_registry = compatibility_tool_registry();
    let mut raw_messages = Vec::new();
    let user_message = build_prompt_command_user_message(
        session_id,
        None,
        "/debug failing test".to_owned(),
        "<command-name>debug</command-name><command-args>failing test</command-args>".to_owned(),
        "Expanded prompt body".to_owned(),
    );

    let (_, turn_count, stop_reason, _, _) = execute_local_turn_with_user_message_options(
        &store,
        &tool_registry,
        root,
        None,
        ApiProvider::OpenAICompatible,
        DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        session_id,
        &mut raw_messages,
        user_message,
        false,
        &RuntimeCliOptions::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(turn_count, 1);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(
        message_text(&raw_messages[0]),
        "<command-name>debug</command-name><command-args>failing test</command-args>"
    );
    assert_eq!(
        raw_messages[0]
            .metadata
            .attributes
            .get(ccrust_core::PROMPT_COMMAND_RAW_INPUT_ATTRIBUTE)
            .map(String::as_str),
        Some("/debug failing test")
    );
    assert!(message_text(raw_messages.last().unwrap()).contains("Expanded prompt body"));
}

#[test]
fn shift_up_starts_transcript_selection_and_scrolls_focus_into_view() {
    let key = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(
        transcript_selection_move_for_key(&key, false),
        Some(TranscriptSelectionMove::Up)
    );

    let mut state = RatatuiApp::new("selection-scroll").initial_state();
    state.transcript_lines = (1..=20)
        .map(|index| TranscriptLine {
            role: "assistant".to_owned(),
            text: format!("line {index}"),
            author_label: Some("Assistant(test)".to_owned()),
            token_label: Some("12 tok".to_owned()),
        })
        .collect();
    let selectable_lines = transcript_selectable_lines_for_view(&state, 80);
    let mut interaction_state = ReplInteractionState::default();

    let focus = move_transcript_selection(
        &mut interaction_state,
        &selectable_lines,
        TranscriptSelectionMove::Up,
    )
    .unwrap();

    assert!(interaction_state.transcript_selection.is_some());
    assert!(focus.line_index < selectable_lines.last().unwrap().line_index);

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
    let mut scroll = 0;
    sync_transcript_selection_preview(&state, 80, 12, &interaction_state, &mut scroll);
    assert!(scroll > 0);
}

#[test]
fn provider_error_transcript_text_prefers_rich_provider_errors() {
    let error = anyhow::anyhow!(ProviderRequestError::new(
        "brief summary",
        "full transcript body",
    ));

    assert_eq!(
        provider_error_transcript_text(&error),
        "full transcript body"
    );
}

#[tokio::test]
async fn append_provider_error_message_records_assistant_output() {
    let root = temp_session_root("provider-error-transcript");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let mut messages = Vec::new();
    let user_message = build_text_message(session_id, MessageRole::User, "hello".to_owned(), None);
    store
        .append_message(session_id, &user_message)
        .await
        .unwrap();
    messages.push(user_message.clone());

    append_provider_error_message(
        &store,
        session_id,
        &mut messages,
        Some(user_message.id),
        ApiProvider::Gemini,
        "gemini-2.5-pro",
        &RuntimeSystemPromptMetrics::default(),
        "partial response".to_owned(),
        Vec::new(),
        "full provider error".to_owned(),
    )
    .await
    .unwrap();

    let persisted = store.load_session(session_id).await.unwrap();
    assert_eq!(persisted.len(), 2);
    assert_eq!(persisted[1].role, MessageRole::Assistant);
    assert_eq!(persisted[1].metadata.provider.as_deref(), Some("gemini"));
    assert_eq!(
        persisted[1].metadata.model.as_deref(),
        Some("gemini-2.5-pro")
    );
    assert!(persisted[1].blocks.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text }
            if text == "partial response\n\nfull provider error"
    )));
}

#[tokio::test]
async fn execute_local_turn_refreshes_raw_messages_after_provider_error() {
    struct EnvVarsGuard {
        previous: Vec<(String, Option<String>)>,
    }

    impl Drop for EnvVarsGuard {
        fn drop(&mut self) {
            for (key, previous) in self.previous.iter().rev() {
                match previous {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_session_root("execute-local-turn-provider-error");
    let env_restore = EnvVarsGuard {
        previous: vec![
            ("CODEX_HOME".to_owned(), env::var("CODEX_HOME").ok()),
            ("GEMINI_API_KEY".to_owned(), env::var("GEMINI_API_KEY").ok()),
            ("GOOGLE_API_KEY".to_owned(), env::var("GOOGLE_API_KEY").ok()),
            ("OPENAI_API_KEY".to_owned(), env::var("OPENAI_API_KEY").ok()),
        ],
    };
    env::set_var("CODEX_HOME", root.join("codex-home"));
    env::remove_var("GEMINI_API_KEY");
    env::remove_var("GOOGLE_API_KEY");
    env::remove_var("OPENAI_API_KEY");

    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let tool_registry = compatibility_tool_registry();
    let mut raw_messages = Vec::new();

    let error = execute_local_turn(
        &store,
        &tool_registry,
        root.clone(),
        None,
        ApiProvider::Gemini,
        "gemini-2.5-pro".to_owned(),
        session_id,
        &mut raw_messages,
        "hello".to_owned(),
        true,
        None,
    )
    .await
    .unwrap_err();

    drop(env_restore);

    assert!(error.to_string().contains("GEMINI_API_KEY"));
    assert_eq!(raw_messages.len(), 2);
    assert_eq!(raw_messages[1].role, MessageRole::Assistant);
    assert!(message_text(&raw_messages[1]).contains("GEMINI_API_KEY"));
}

#[tokio::test]
async fn execute_local_turn_continues_after_tool_invocation_error() {
    let root = temp_session_root("execute-local-turn-tool-invoke-error");
    let file_path = root.join("example.txt");
    write_test_file(&file_path, "alpha\n");

    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let tool_registry = compatibility_tool_registry();
    let mut raw_messages = Vec::new();
    let prompt = format!(
        "tool:file_edit {}",
        json!({
            "path": file_path.to_string_lossy(),
            "old_string": "missing",
            "new_string": "beta",
        })
    );

    let (_, turn_count, stop_reason, _, _) = execute_local_turn(
        &store,
        &tool_registry,
        root.clone(),
        None,
        ApiProvider::Gemini,
        "gemini-2.5-pro".to_owned(),
        session_id,
        &mut raw_messages,
        prompt,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(turn_count, 2);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(raw_messages.len(), 4);

    let tool_result = raw_messages
        .iter()
        .find_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::ToolResult { result } => Some(result),
                _ => None,
            })
        })
        .expect("expected tool result");
    assert!(tool_result.is_error);
    assert!(tool_result
        .output_text
        .contains("Error calling tool (file_edit):"));
    assert!(tool_result.output_text.contains("target string not found"));
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "alpha\n");

    assert!(message_text(raw_messages.last().unwrap()).contains("echo tool result"));
    assert!(message_text(raw_messages.last().unwrap()).contains("target string not found"));
}

#[tokio::test]
async fn execute_local_turn_continues_after_invalid_tool_call_json() {
    let root = temp_session_root("execute-local-turn-invalid-tool-json");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let session_id = SessionId::new_v4();
    let tool_registry = compatibility_tool_registry();
    let mut raw_messages = Vec::new();

    let (_, turn_count, stop_reason, _, _) = execute_local_turn(
        &store,
        &tool_registry,
        root.clone(),
        None,
        ApiProvider::Gemini,
        "gemini-2.5-pro".to_owned(),
        session_id,
        &mut raw_messages,
        "tool:file_read {not-json".to_owned(),
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(turn_count, 2);
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(raw_messages.len(), 4);

    let tool_result = raw_messages
        .iter()
        .find_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::ToolResult { result } => Some(result),
                _ => None,
            })
        })
        .expect("expected tool result");
    assert!(tool_result.is_error);
    assert!(tool_result.output_text.contains("invalid tool input JSON"));
    assert!(message_text(raw_messages.last().unwrap()).contains("invalid tool input JSON"));
}

use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
struct SlashCommandFixture {
    cases: Vec<SlashCommandCase>,
}

#[derive(Deserialize)]
struct SlashCommandCase {
    input: String,
    name: String,
    args: Vec<String>,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn temp_session_root(label: &str) -> PathBuf {
    let root = env::temp_dir().join(format!("ccrust-{label}-{}", Uuid::new_v4()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn temp_tcp_address() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    format!("tcp://{address}")
}

fn write_test_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn with_env_var<T>(key: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(&self.key, value),
                None => env::remove_var(&self.key),
            }
        }
    }

    let _guard = ENV_LOCK.lock().unwrap();
    let restore = EnvVarGuard {
        key: key.to_owned(),
        previous: env::var(key).ok(),
    };
    match value {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }
    let result = f();
    drop(restore);
    result
}

fn with_env_vars<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
    struct EnvVarsGuard {
        previous: Vec<(String, Option<String>)>,
    }

    impl Drop for EnvVarsGuard {
        fn drop(&mut self) {
            for (key, previous) in self.previous.iter().rev() {
                match previous {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    let _guard = ENV_LOCK.lock().unwrap();
    let restore = EnvVarsGuard {
        previous: vars
            .iter()
            .map(|(key, _)| ((*key).to_owned(), env::var(key).ok()))
            .collect(),
    };
    for (key, value) in vars {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
    let result = f();
    drop(restore);
    result
}

fn repl_handled_command_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "add-dir",
        "batch",
        "branch",
        "btw",
        "color",
        "context",
        "debug",
        "doctor",
        "feedback",
        "help",
        "copy",
        "config",
        "status",
        "ide",
        "statusline",
        "theme",
        "vim",
        "plan",
        "init",
        "insights",
        "install-github-app",
        "review",
        "security-review",
        "simplify",
        "fast",
        "passes",
        "effort",
        "tag",
        "rename",
        "rewind",
        "reload-auth",
        "model",
        "compact",
        "clear",
        "version",
        "resume",
        "session",
        "login",
        "logout",
        "permissions",
        "plugin",
        "pr-comments",
        "release-notes",
        "sandbox",
        "skills",
        "reload-plugins",
        "hooks",
        "longtask",
        "output-style",
        "mcp",
        "memory",
        "mobile",
        "desktop",
        "chrome",
        "terminal-setup",
        "files",
        "diff",
        "usage",
        "cost",
        "stats",
        "remote-env",
        "export",
        "tasks",
        "agents",
        "advisor",
        "remote-control",
        "stickers",
        "update-config",
        "voice",
        "exit",
    ])
}

fn noninteractive_handled_command_names() -> BTreeSet<&'static str> {
    repl_handled_command_names()
}

mod commands;
mod parity;
mod remote;
mod session_ui;
mod ui_basics;
