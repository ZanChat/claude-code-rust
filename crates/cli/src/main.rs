#![allow(clippy::too_many_arguments)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::single_match)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::useless_vec)]
mod helpers;
use helpers::*;
mod commands;
use commands::*;
mod spinner_verbs;
use spinner_verbs::sample_spinner_verb;
mod startup;
use startup::*;
mod session;
use session::*;
mod cli_args;
use cli_args::*;
mod reports;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use code_agent_bridge::{
    base64_decode, base64_encode, connect_and_exchange, serve_bridge_session, serve_direct_session,
    AssistantDirective, BridgeServerConfig, BridgeSessionHandler, RemoteEndpoint, RemoteEnvelope,
    RemoteMode, RemotePermissionRequest, RemoteSessionState, ResumeSessionRequest, VoiceFrame,
};
use code_agent_core::{
    compatibility_command_registry, coordinator_tasks, create_coordinator_synthesis_task,
    create_coordinator_task, create_coordinator_worker_task, resume_tasks_for_question,
    update_task_record, AppEvent, BoundaryKind, CommandInvocation, CommandRegistry, CommandSource,
    CommandSpec, ContentBlock, LocalTaskStore as CoreLocalTaskStore, Message, MessageRole,
    QuestionRequest, QuestionResponse, SessionId, TaskRecord, TaskStatus, TaskStore,
};
use code_agent_mcp::parse_mcp_server_configs;
use code_agent_plugins::{
    BridgeLaunchRequest, CommandDefinitions, OutOfProcessPluginRuntime, PluginManifest,
    PluginRuntime, PLUGIN_MANIFEST_PATH, SKILL_FILE_NAME,
};
use code_agent_providers::{
    build_provider, clear_auth_snapshot, code_agent_auth_snapshot_path,
    compatibility_model_catalog, config_migration_report, get_anthropic_credential_hint,
    get_openai_credential_hint, resolve_api_provider, write_auth_snapshot, ApiProvider,
    AuthRequest, AuthResolver, EnvironmentAuthResolver, ModelCatalog, ProviderEvent,
    ProviderRequest, ProviderToolDefinition,
};
use code_agent_session::{
    agent_transcript_path_for, claude_config_home_dir, compact_messages, estimate_message_tokens,
    extract_last_json_string_field, get_project_dir, import_transcript_to_session_root,
    materialize_runtime_messages, CompactionConfig, CompactionOutcome, JsonlTranscriptCodec,
    SessionSummary, TranscriptCodec,
};
use code_agent_tools::{compatibility_tool_registry, ToolCallRequest, ToolContext, ToolRegistry};
use code_agent_ui::{
    draw_terminal as draw_tui, mouse_action_for_position, render_to_string as render_tui_to_string,
    transcript_line_from_message, transcript_search_match_items, transcript_search_scroll_for_view,
    transcript_selectable_lines_for_view, transcript_selection_text_for_view,
    transcript_visual_scroll_for_view, ChoiceListItem, ChoiceListState, CommandPaletteEntry,
    Notification, PaneKind, PanePreview, PermissionPromptState, PromptHistorySearchState,
    PromptSelectionState, QuestionUiEntry, RatatuiApp, StatusLevel, TaskUiEntry, TranscriptGroup,
    TranscriptItem, TranscriptLine, TranscriptMessageActionsState, TranscriptSearchState,
    TranscriptSelectableLine, TranscriptSelectionPoint, TranscriptSelectionState, UiMouseAction,
    UiState,
};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size as terminal_size, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use reports::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::future::Future;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

#[cfg(test)]
use code_agent_providers::EchoProvider;

const UI_EVENT_TAG: &str = "ui_event";
const UI_ROLE_ATTRIBUTE: &str = "ui_role";
const UI_AUTHOR_ATTRIBUTE: &str = "ui_author";
const REQUEST_INTERRUPTED_MESSAGE: &str = "[Request interrupted by user]";
const RECENT_COMPLETED_TTL_MS: i64 = 30_000;
const HISTORY_TRANSCRIPT_GROUP_PREFIX: &str = "history-group-";
const FILE_PICKER_MAX_INDEXED_FILES: usize = 5_000;
const FILE_PICKER_MAX_RESULTS: usize = 12;

include!("main/session_messages.rs");
include!("main/ui_helpers.rs");
include!("main/system_prompt.rs");
include!("main/prompt_commands.rs");
include!("main/repl_picker.rs");
include!("main/transcript_actions.rs");
include!("main/repl_ui.rs");
include!("main/pending_state.rs");
include!("main/pending_runtime.rs");
include!("main/bridge.rs");
include!("main/runtime.rs");

#[cfg(test)]
mod tests;
