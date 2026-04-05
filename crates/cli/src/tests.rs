use super::{
    accept_prompt_history_search, build_repl_command_input_message,
    build_repl_command_output_message, build_repl_ui_state, build_resume_choice_list,
    build_startup_screens, build_startup_ui_state, build_text_message, build_tool_result_message,
    cancel_prompt_history_search, choose_active_session, command_suggestions, current_time_ms,
    delete_prompt_selection, handle_prompt_file_picker_key, handle_prompt_mouse_action,
    handle_repl_slash_command, insert_prompt_text, is_paste_shortcut, is_selection_copy_shortcut,
    message_action_copy_text, message_primary_input, message_text, move_prompt_selection,
    navigate_prompt_history_down, navigate_prompt_history_up, open_prompt_history_search,
    pane_from_shortcut, pane_from_shortcut_for_terminal, pending_interrupt_messages,
    prompt_file_picker_choice_list, prompt_history_from_messages, prompt_history_search_matches,
    prompt_selection_text, render_auth_command_with_resume, render_ide_command_with_home,
    render_remote_control_command, repl_shortcut_action_for_key, resolve_continue_target,
    resolved_command_registry, resumable_sessions, resume_hint_text,
    should_echo_command_result_in_footer, should_exit_repl, step_prompt_history_search_match,
    sync_prompt_history_search_preview, task_entries_for_ui, toggle_all_history_transcript_groups,
    toggle_pending_repl_transcript_details, ActiveSessionStore, Cli, LocalBridgeHandler, Message,
    MessageRole, PendingReplStep, PendingReplView, PromptSelectionMove, ReplInteractionState,
    ReplSessionState, ReplShortcutAction, ResumePickerState, ResumeTargetHint, StartupPreferences,
};
use code_agent_bridge::{
    base64_encode, serve_direct_session, AssistantDirective, BridgeServerConfig,
    BridgeSessionHandler, RemoteEnvelope, RemotePermissionResponse, ResumeSessionRequest,
    VoiceFrame,
};
use code_agent_core::{
    compatibility_command_registry, CommandInvocation, ContentBlock, SessionId, TaskRecord,
    TaskStatus, ToolCall,
};
use code_agent_providers::{
    ApiProvider, DEFAULT_OPENAI_COMPLETION_MODEL, DEFAULT_OPENAI_REASONING_MODEL,
};
use code_agent_session::{materialize_runtime_messages, LocalSessionStore, SessionSummary};
use code_agent_tools::compatibility_tool_registry;
use code_agent_ui::{
    PromptSelectionState, TranscriptItem, TranscriptSelectionPoint, TranscriptSelectionState,
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
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

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
    let root = env::temp_dir().join(format!("code-agent-rust-{label}-{}", Uuid::new_v4()));
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

fn repl_handled_command_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "help",
        "version",
        "copy",
        "config",
        "status",
        "ide",
        "statusline",
        "theme",
        "vim",
        "plan",
        "fast",
        "passes",
        "effort",
        "model",
        "compact",
        "clear",
        "resume",
        "session",
        "login",
        "logout",
        "permissions",
        "plugin",
        "skills",
        "reload-plugins",
        "hooks",
        "output-style",
        "mcp",
        "memory",
        "files",
        "diff",
        "usage",
        "cost",
        "stats",
        "remote-env",
        "export",
        "tasks",
        "agents",
        "remote-control",
        "voice",
        "exit",
    ])
}

fn noninteractive_handled_command_names() -> BTreeSet<&'static str> {
    repl_handled_command_names()
}

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
        Some(format!("code-agent-rust --resume {session_id}"))
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
            "\nResume this session with:\ncode-agent-rust --resume {session_id}\n"
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
    let app = code_agent_ui::RatatuiApp::new("repl");
    let registry = compatibility_command_registry();
    let input = code_agent_ui::InputBuffer::new();
    let state = build_repl_ui_state(
        &app,
        &registry,
        &[],
        None,
        Path::new("."),
        ApiProvider::ChatGPTCodex,
        DEFAULT_OPENAI_REASONING_MODEL,
        SessionId::new_v4(),
        &input,
        "status",
        None,
        code_agent_ui::PaneKind::Transcript,
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
fn build_repl_ui_state_groups_pending_steps() {
    let app = code_agent_ui::RatatuiApp::new("repl");
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        Some("working".to_owned()),
        code_agent_ui::PaneKind::Transcript,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &ReplInteractionState::default(),
    );

    assert_eq!(state.transcript_lines.len(), 1);
    assert!(state.transcript_groups.is_empty());
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
fn build_repl_ui_state_collapses_history_tool_runs_into_single_group() {
    let app = code_agent_ui::RatatuiApp::new("repl");
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        None,
        code_agent_ui::PaneKind::Transcript,
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
fn build_repl_ui_state_keeps_message_action_indices_for_grouped_history() {
    let app = code_agent_ui::RatatuiApp::new("repl");
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        None,
        code_agent_ui::PaneKind::Transcript,
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
        assert!(state.steps.iter().all(|entry| !entry.touched));
    }

    toggle_pending_repl_transcript_details(&pending_view);
    let state = pending_view.lock().unwrap();
    assert!(!state.show_transcript_details);
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

    let mut input_buffer = code_agent_ui::InputBuffer::new();
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

    let mut input_buffer = code_agent_ui::InputBuffer::new();
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
fn build_repl_ui_state_hides_prompt_in_transcript_mode() {
    let app = code_agent_ui::RatatuiApp::new("repl-transcript");
    let registry = compatibility_command_registry();
    let mut search_input = code_agent_ui::InputBuffer::new();
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        None,
        code_agent_ui::PaneKind::Tasks,
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
    assert_eq!(state.active_pane, Some(code_agent_ui::PaneKind::Transcript));
    assert!(state.transcript_search.is_some());
}

#[test]
fn build_repl_ui_state_keeps_prompt_visible_for_message_actions() {
    let app = code_agent_ui::RatatuiApp::new("repl-message-actions");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();
    let assistant_tool_call = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            call: code_agent_core::ToolCall {
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        None,
        code_agent_ui::PaneKind::Tasks,
        None,
        0,
        None,
        Vec::new(),
        0,
        0,
        &interaction_state,
    );

    assert!(state.show_input);
    assert_eq!(state.active_pane, Some(code_agent_ui::PaneKind::Transcript));
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
        None
    );
}

#[test]
fn build_repl_ui_state_exposes_transcript_selection() {
    let app = code_agent_ui::RatatuiApp::new("repl-selection");
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
        &code_agent_ui::InputBuffer::new(),
        "status",
        None,
        code_agent_ui::PaneKind::Tasks,
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
            call: code_agent_core::ToolCall {
                id: "tool-call-1".to_owned(),
                name: "run_in_terminal".to_owned(),
                input_json: json!({
                    "command": "cargo test -p code-agent-ui"
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
        Some("cargo test -p code-agent-ui")
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
    let mut vim_state = code_agent_ui::vim::VimState::default();
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
        ApiProvider::OpenAI,
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

    assert!(status.contains("\"provider\": \"openai\""));
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
    let mut vim_state = code_agent_ui::vim::VimState::default();
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
async fn repl_ide_command_reports_bridge_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-ide")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = "claude-sonnet-4-6".to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
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
fn render_ide_command_detects_matching_lockfile() {
    let home = temp_session_root("ide-lockfile-home");
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

    let report = render_ide_command_with_home(&workspace, false, None, Some(&home)).unwrap();

    assert!(report.contains("\"status\": \"available\""));
    assert!(report.contains("\"name\": \"VS Code\""));
    assert!(report.contains("ide://127.0.0.1:48123"));
}

#[tokio::test]
async fn lightweight_repl_commands_return_output() {
    let root = temp_session_root("repl-lightweight-commands");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "hello".to_owned(), None),
        build_text_message(session_id, MessageRole::Assistant, "world".to_owned(), None),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let cases = vec![
        ("help", vec![], "/help".to_owned()),
        ("version", vec![], "/version".to_owned()),
        ("status", vec![], "/status".to_owned()),
        ("statusline", vec![], "/statusline".to_owned()),
        ("theme", vec![], "/theme".to_owned()),
        ("vim", vec![], "/vim".to_owned()),
        ("plan", vec![], "/plan".to_owned()),
        ("fast", vec![], "/fast".to_owned()),
        ("passes", vec![], "/passes".to_owned()),
        ("effort", vec![], "/effort".to_owned()),
        ("session", vec![], "/session".to_owned()),
        ("permissions", vec![], "/permissions".to_owned()),
        ("compact", vec![], "/compact".to_owned()),
        ("files", vec![], "/files".to_owned()),
        ("diff", vec![], "/diff".to_owned()),
        ("usage", vec![], "/usage".to_owned()),
        ("remote-env", vec![], "/remote-env".to_owned()),
        ("export", vec![], "/export".to_owned()),
        ("tasks", vec![], "/tasks".to_owned()),
        ("agents", vec![], "/agents".to_owned()),
        ("skills", vec![], "/skills".to_owned()),
        ("reload-plugins", vec![], "/reload-plugins".to_owned()),
        ("hooks", vec![], "/hooks".to_owned()),
        ("output-style", vec![], "/output-style".to_owned()),
        ("remote-control", vec![], "/remote-control".to_owned()),
        ("voice", vec![], "/voice".to_owned()),
        ("exit", vec![], "/exit".to_owned()),
    ];

    for (name, args, raw_input) in cases {
        let output = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: name.to_owned(),
                args,
                raw_input,
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::ChatGPTCodex,
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

        assert!(
            !output.trim().is_empty(),
            "expected non-empty output for /{name}"
        );
    }
}

#[tokio::test]
async fn repl_model_command_switches_active_model() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-model")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "model".to_owned(),
            args: vec![DEFAULT_OPENAI_COMPLETION_MODEL.to_owned()],
            raw_input: format!("/model {DEFAULT_OPENAI_COMPLETION_MODEL}"),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        true,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(active_model, DEFAULT_OPENAI_COMPLETION_MODEL);
    assert!(status.contains("model switched"));
}

#[tokio::test]
async fn repl_model_command_accepts_openai_compatible_custom_model() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "repl-model-openai-compatible",
    )));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "model".to_owned(),
            args: vec!["gemini-3.1-pro-preview".to_owned()],
            raw_input: "/model gemini-3.1-pro-preview".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        true,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(active_model, "gemini-3.1-pro-preview");
    assert!(status.contains("model switched"));
}

#[tokio::test]
async fn repl_clear_command_resets_transcript_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-clear")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let transcript_path = store.transcript_path(session_id).await.unwrap();
    let mut raw_messages = vec![Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
    )];
    let persisted = build_text_message(session_id, MessageRole::User, "persist".to_owned(), None);
    store.append_message(session_id, &persisted).await.unwrap();
    let mut active_model = "claude-sonnet-4-6".to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "clear".to_owned(),
            raw_input: "/clear".to_owned(),
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

    assert!(status.contains("cleared session"));
    assert!(raw_messages.is_empty());
    assert!(!transcript_path.exists());
}

#[tokio::test]
async fn repl_copy_command_writes_latest_assistant_response() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-copy")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "question".to_owned(), None),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "first answer".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "second answer".to_owned(),
            None,
        ),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "copy".to_owned(),
            raw_input: "/copy".to_owned(),
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

    assert!(status.contains("last assistant response"));
    let file_path = status
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Also wrote it to "))
        .map(PathBuf::from)
        .expect("copy command should report a fallback file path");
    assert_eq!(fs::read_to_string(file_path).unwrap(), "second answer");
}

#[tokio::test]
async fn repl_copy_command_supports_explicit_message_index() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-copy-index")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "question".to_owned(), None),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "first answer".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "second answer".to_owned(),
            None,
        ),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "copy".to_owned(),
            args: vec!["2".to_owned()],
            raw_input: "/copy 2".to_owned(),
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

    assert!(status.contains("assistant response #2"));
    let file_path = status
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Also wrote it to "))
        .map(PathBuf::from)
        .expect("copy command should report a fallback file path");
    assert_eq!(fs::read_to_string(file_path).unwrap(), "first answer");
}

#[test]
fn repl_command_ui_event_messages_use_attachment_metadata() {
    let session_id = SessionId::new_v4();
    let input = build_repl_command_input_message(session_id, None, "/config");
    let output = build_repl_command_output_message(session_id, Some(input.id), "config", "ok");

    assert_eq!(input.role, MessageRole::Attachment);
    assert_eq!(
        input.metadata.attributes.get("ui_role").map(String::as_str),
        Some("command")
    );
    assert_eq!(output.role, MessageRole::Attachment);
    assert_eq!(
        output
            .metadata
            .attributes
            .get("ui_role")
            .map(String::as_str),
        Some("command_output")
    );
    assert_eq!(
        output
            .metadata
            .attributes
            .get("ui_author")
            .map(String::as_str),
        Some("/config")
    );
}

#[test]
fn resumable_sessions_exclude_current_session() {
    let current_session = SessionId::new_v4();
    let other_session = SessionId::new_v4();
    let sessions = vec![
        SessionSummary {
            session_id: current_session,
            transcript_path: PathBuf::from(format!("{current_session}.jsonl")),
            modified_at_unix_ms: 20,
            message_count: 3,
            first_prompt: "current".to_owned(),
        },
        SessionSummary {
            session_id: other_session,
            transcript_path: PathBuf::from(format!("{other_session}.jsonl")),
            modified_at_unix_ms: 10,
            message_count: 8,
            first_prompt: "pick me".to_owned(),
        },
    ];

    let filtered = resumable_sessions(sessions, current_session);

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].session_id, other_session);
}

#[test]
fn resume_picker_builds_choice_list_entries() {
    let session_id = SessionId::new_v4();
    let picker = ResumePickerState {
        sessions: vec![SessionSummary {
            session_id,
            transcript_path: PathBuf::from(format!("{session_id}.jsonl")),
            modified_at_unix_ms: 10,
            message_count: 6,
            first_prompt: "Continue with the latest auth edge cases.".to_owned(),
        }],
        selected: 0,
    };

    let choice_list = build_resume_choice_list(&picker);

    assert_eq!(choice_list.title, "Resume conversation");
    assert_eq!(choice_list.items.len(), 1);
    assert!(choice_list.items[0]
        .label
        .contains("Continue with the latest auth edge cases."));
    assert!(choice_list.items[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("6 messages"));
}

#[test]
fn transcript_backed_commands_do_not_echo_results_in_footer() {
    assert!(!should_echo_command_result_in_footer("tasks", true, false));
    assert!(!should_echo_command_result_in_footer(
        "resume", false, false
    ));
    assert!(should_echo_command_result_in_footer("clear", false, false));
    assert!(should_echo_command_result_in_footer("resume", false, true));
}

#[tokio::test]
async fn repl_resume_command_switches_live_session() {
    let root = temp_session_root("repl-resume");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let current_session = SessionId::new_v4();
    let resumed_session = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = vec![build_text_message(
        current_session,
        MessageRole::User,
        "current prompt".to_owned(),
        None,
    )];
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(current_session);

    let resumed_message = build_text_message(
        resumed_session,
        MessageRole::User,
        "resumed output".to_owned(),
        None,
    );
    store
        .append_message(resumed_session, &resumed_message)
        .await
        .unwrap();

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "resume".to_owned(),
            args: vec![resumed_session.to_string()],
            raw_input: format!("/resume {resumed_session}"),
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

    assert!(status.contains("resumed"));
    assert_eq!(repl_session.session_id, resumed_session);
    assert_eq!(raw_messages.len(), 1);
    assert_eq!(message_text(&raw_messages[0]), "resumed output");
}

#[tokio::test]
async fn repl_tasks_command_creates_and_lists_tasks() {
    let root = temp_session_root("repl-tasks");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let created = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "tasks".to_owned(),
            args: vec![
                "create".to_owned(),
                "title=review".to_owned(),
                "status=running".to_owned(),
            ],
            raw_input: "/tasks create title=review status=running".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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
    let listed = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "tasks".to_owned(),
            raw_input: "/tasks".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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

    assert!(created.contains("\"title\": \"review\""));
    assert!(listed.contains("\"count\": 1"));
    assert!(listed.contains("\"status\": \"Running\""));
}

#[tokio::test]
async fn repl_plugin_command_reports_manifest_details() {
    let root = temp_session_root("repl-plugin");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "review-tools",
              "version": "1.0.0",
              "description": "Review helpers",
              "skills": "./skills/review",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp"
                }
              }
            }"#,
    );
    write_test_file(&root.join("skills/review/SKILL.md"), "# Review\n");

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "plugin".to_owned(),
            raw_input: "/plugin".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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

    assert!(status.contains("\"name\": \"review-tools\""));
    assert!(status.contains("\"mcp_server_names\""));
    assert!(status.contains("\"skill_names\""));
}

#[tokio::test]
async fn repl_mcp_command_lists_parsed_servers_and_auth() {
    let root = temp_session_root("repl-mcp");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "mcp-tools",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp",
                  "auth": {
                    "type": "oauth_device",
                    "clientId": "client-123",
                    "audience": "example"
                  }
                }
              }
            }"#,
    );

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "mcp".to_owned(),
            raw_input: "/mcp".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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

    assert!(status.contains("\"example\""));
    assert!(status.contains("\"oauth_device\""));
    assert!(status.contains("\"client_id\": \"client-123\""));
}

#[tokio::test]
async fn repl_mcp_auth_login_starts_device_flow() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
                .unwrap();
            let header_text = String::from_utf8(request[..header_end].to_vec()).unwrap();
            let path = header_text
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let response = match path.as_str() {
                "/.well-known/oauth-authorization-server" => json!({
                    "device_authorization_endpoint": format!("http://{address}/device_authorization"),
                    "token_endpoint": format!("http://{address}/token")
                }),
                "/device_authorization" => json!({
                    "device_code": "device-123",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://verify.example.com",
                    "verification_uri_complete": "https://verify.example.com/complete",
                    "expires_in": 900,
                    "interval": 5
                }),
                other => panic!("unexpected auth path: {other}"),
            };
            let body = response.to_string();
            let response_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            std::io::Write::write_all(&mut stream, response_text.as_bytes()).unwrap();
        }
    });

    let root = temp_session_root("repl-mcp-auth");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        &format!(
            r#"{{
                  "name": "mcp-tools",
                  "mcpServers": {{
                    "example": {{
                      "url": "http://{address}/mcp",
                      "auth": {{
                        "type": "oauth_device",
                        "clientId": "client-123",
                        "audience": "example"
                      }}
                    }}
                  }}
                }}"#
        ),
    );

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "mcp".to_owned(),
            args: vec![
                "auth-login".to_owned(),
                root.display().to_string(),
                "example".to_owned(),
            ],
            raw_input: format!("/mcp auth-login {} example", root.display()),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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

    assert!(status.contains("\"device_code\": \"device-123\""));
    assert!(status.contains("\"verification_uri\""));
}

#[tokio::test]
async fn repl_remote_control_reports_local_state() {
    let root = temp_session_root("repl-remote");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "remote-control".to_owned(),
            raw_input: "/remote-control".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
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

    assert!(status.contains("\"session_id\""));
    assert!(status.contains("\"task_count\""));
    assert!(status.contains("\"question_count\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_control_directive_command_reaches_bridge() {
    let root = temp_session_root("remote-directive");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let address = temp_tcp_address();
    let server_root = root.clone();
    let server_address = address.clone();
    let server = tokio::spawn(async move {
        let server_store = ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
        let server_tool_registry = compatibility_tool_registry();
        let handler = LocalBridgeHandler {
            store: &server_store,
            tool_registry: &server_tool_registry,
            cwd: server_root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        serve_direct_session(
            BridgeServerConfig {
                bind_address: server_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            },
            handler,
        )
        .await
        .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let output = render_remote_control_command(
        &registry,
        &CommandInvocation {
            name: "remote-control".to_owned(),
            args: vec![
                "directive".to_owned(),
                address.clone(),
                "agent=coordinator".to_owned(),
                "delegate the review".to_owned(),
            ],
            raw_input: format!(
                "/remote-control directive {address} agent=coordinator delegate the review"
            ),
        },
        &Cli {
            bridge_receive_count: Some(24),
            ..Cli::default()
        },
        &store,
        &tool_registry,
        &root,
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
        session_id,
        &[],
        false,
    )
    .await
    .unwrap();
    let record = server.await.unwrap();

    assert!(output.contains("assistant_synthesis") || output.contains("delegate the review"));
    assert!(record
        .envelopes
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::AssistantDirective { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_control_answer_command_round_trips_question_response() {
    let root = temp_session_root("remote-answer");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let question_id = Uuid::new_v4();
    let address = temp_tcp_address();
    let server_root = root.clone();
    let server_address = address.clone();
    let server = tokio::spawn(async move {
        let server_store = ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
        let server_tool_registry = compatibility_tool_registry();
        let handler = LocalBridgeHandler {
            store: &server_store,
            tool_registry: &server_tool_registry,
            cwd: server_root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        serve_direct_session(
            BridgeServerConfig {
                bind_address: server_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            },
            handler,
        )
        .await
        .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let output = render_remote_control_command(
        &registry,
        &CommandInvocation {
            name: "remote-control".to_owned(),
            args: vec![
                "answer".to_owned(),
                address.clone(),
                question_id.to_string(),
                "approved".to_owned(),
            ],
            raw_input: format!("/remote-control answer {address} {question_id} approved"),
        },
        &Cli {
            bridge_receive_count: Some(8),
            ..Cli::default()
        },
        &store,
        &tool_registry,
        &root,
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
        session_id,
        &[],
        false,
    )
    .await
    .unwrap();
    let record = server.await.unwrap();

    assert!(output.contains(&question_id.to_string()));
    assert!(record.envelopes.iter().any(|envelope| {
        matches!(
            envelope,
            RemoteEnvelope::QuestionResponse { response }
                if response.question_id == question_id
        )
    }));
}

#[tokio::test]
async fn local_bridge_handler_runs_prompt_turns() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("bridge-prompt")));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::Message {
            message: build_text_message(
                session_id,
                MessageRole::User,
                "bridge hello".to_owned(),
                None,
            ),
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(|envelope| match envelope {
        RemoteEnvelope::Message { message } => message.blocks.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text.contains("bridge hello"))
        ),
        _ => false,
    }));
}

#[tokio::test]
async fn local_bridge_handler_supports_assistant_and_voice_inputs() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "bridge-assistant",
    )));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let directive = handler
        .on_envelope(&RemoteEnvelope::AssistantDirective {
            directive: AssistantDirective {
                agent_id: Some("coordinator".to_owned()),
                instruction: "delegate the review".to_owned(),
                ..AssistantDirective::default()
            },
        })
        .await
        .unwrap();
    let voice = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"voice hello"),
                sequence: 1,
                stream_id: Some("voice".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    let directive_messages = directive
        .iter()
        .filter(|envelope| matches!(envelope, RemoteEnvelope::Message { .. }))
        .count();
    assert!(directive_messages >= 2);
    assert!(voice
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
}

#[tokio::test]
async fn local_bridge_handler_emits_tool_call_and_result_envelopes() {
    let root = temp_session_root("bridge-tool");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root,
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::Message {
            message: build_text_message(
                session_id,
                MessageRole::User,
                "tool:memory {\"action\":\"write\",\"value\":{\"note\":\"ok\"}}".to_owned(),
                None,
            ),
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(
        |envelope| matches!(envelope, RemoteEnvelope::ToolCall { call } if call.name == "memory")
    ));
    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if result.tool_call_id == "echo_tool_call")));
}

#[tokio::test]
async fn local_bridge_handler_buffers_streamed_voice_frames() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "bridge-voice-stream",
    )));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let partial = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"voice "),
                sequence: 1,
                stream_id: Some("stream-a".to_owned()),
                is_final: false,
            },
        })
        .await
        .unwrap();
    let final_chunk = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"hello"),
                sequence: 2,
                stream_id: Some("stream-a".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    assert!(partial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
    assert!(final_chunk
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
}

#[tokio::test]
async fn local_bridge_handler_persists_binary_voice_frames() {
    let root = temp_session_root("bridge-voice-binary");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root.clone(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let first = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "audio/wav".to_owned(),
                payload_base64: base64_encode(&[0, 255]),
                sequence: 1,
                stream_id: Some("binary-stream".to_owned()),
                is_final: false,
            },
        })
        .await
        .unwrap();
    let second = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "audio/wav".to_owned(),
                payload_base64: base64_encode(&[12, 13]),
                sequence: 2,
                stream_id: Some("binary-stream".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    let saved_path = second
        .iter()
        .find_map(|envelope| match envelope {
            RemoteEnvelope::Ack { note } => {
                note.strip_prefix("voice_frame_saved:").map(PathBuf::from)
            }
            _ => None,
        })
        .expect("missing voice_frame_saved ack");

    assert!(first.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
    assert!(saved_path.exists());
    assert_eq!(fs::read(saved_path).unwrap(), vec![0, 255, 12, 13]);
}

#[tokio::test]
async fn local_bridge_handler_requires_permission_for_remote_tool_calls() {
    let root = temp_session_root("bridge-remote-tool");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root,
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: false,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let initial = handler
        .on_envelope(&RemoteEnvelope::ToolCall {
            call: code_agent_core::ToolCall {
                id: "remote-write".to_owned(),
                name: "file_write".to_owned(),
                input_json: json!({
                    "path": "remote.txt",
                    "content": "ok"
                })
                .to_string(),
                thought_signature: None,
            },
        })
        .await
        .unwrap();

    let request_id = initial
        .iter()
        .find_map(|envelope| match envelope {
            RemoteEnvelope::PermissionRequest { request } => Some(request.id.clone()),
            _ => None,
        })
        .expect("missing permission request");

    let approved = handler
        .on_envelope(&RemoteEnvelope::PermissionResponse {
            response: RemotePermissionResponse {
                id: request_id,
                approved: true,
                note: None,
            },
        })
        .await
        .unwrap();

    assert!(initial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::PermissionRequest { request } if request.tool_name == "file_write")));
    assert!(approved.iter().any(
        |envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if !result.is_error)
    ));
}

#[tokio::test]
async fn local_bridge_handler_resumes_existing_sessions() {
    let root = temp_session_root("bridge-resume");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root));
    let tool_registry = compatibility_tool_registry();
    let resumed_session = SessionId::new_v4();
    let persisted = build_text_message(
        resumed_session,
        MessageRole::Assistant,
        "resumed output".to_owned(),
        None,
    );
    store
        .append_message(resumed_session, &persisted)
        .await
        .unwrap();

    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id: SessionId::new_v4(),
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::ResumeSession {
            request: ResumeSessionRequest {
                target: resumed_session.to_string(),
            },
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Message { message } if message_text(message).contains("resumed output"))));
    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::SessionState { state } if state.session_id == Some(resumed_session))));
}
