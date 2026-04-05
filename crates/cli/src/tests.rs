use super::{
    accept_prompt_history_search, build_ide_choice_list, build_repl_command_input_message,
    build_repl_command_output_message, build_repl_ui_state, build_resume_choice_list,
    build_runtime_system_prompt, build_startup_screens, build_startup_ui_state, build_text_message,
    build_tool_result_message, cancel_prompt_history_search, choose_active_session,
    command_suggestions, current_time_ms, delete_prompt_selection, enter_message_actions,
    handle_prompt_file_picker_key, handle_prompt_mouse_action, handle_repl_slash_command,
    insert_prompt_text, is_paste_shortcut, is_selection_copy_shortcut, message_action_copy_text,
    message_action_items_from_runtime, message_actions_ui_state, message_primary_input,
    message_text, move_prompt_selection, navigate_prompt_history_down, navigate_prompt_history_up,
    navigate_prompt_input_down, navigate_prompt_input_up, open_prompt_history_search,
    pane_from_shortcut, pane_from_shortcut_for_terminal, pending_interrupt_messages,
    prompt_file_picker_choice_list, prompt_history_from_messages, prompt_history_search_matches,
    prompt_selection_text, render_auth_command_with_resume, render_ide_command_with_home,
    render_remote_control_command, repl_ide_picker_state_with_home, repl_shortcut_action_for_key,
    resolve_continue_target, resolve_prompt_command_prompt, resolved_command_registry,
    resumable_sessions, resume_hint_text, should_echo_command_result_in_footer, should_exit_repl,
    step_prompt_history_search_match, sync_prompt_history_search_preview, task_entries_for_ui,
    toggle_all_history_transcript_groups, toggle_pending_repl_transcript_details,
    ActiveSessionStore, Cli, LocalBridgeHandler, Message, MessageRole, PendingReplStep,
    PendingReplView, PromptSelectionMove, ReplInteractionState, ReplMessageActionState,
    ReplSessionState, ReplShortcutAction, ReplTranscriptSearchState, ResumePickerState,
    ResumeTargetHint, StartupPreferences, StartupScreen,
};
use crate::commands::should_enable_mouse_capture;
use code_agent_bridge::{
    base64_encode, serve_direct_session, AssistantDirective, BridgeServerConfig,
    BridgeSessionHandler, RemoteEnvelope, RemotePermissionResponse, ResumeSessionRequest,
    VoiceFrame,
};
use code_agent_core::{
    compatibility_command_registry, CommandInvocation, CommandSource, ContentBlock, SessionId,
    TaskRecord, TaskStatus, ToolCall,
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

mod commands;
mod remote;
mod session_ui;
mod ui_basics;
