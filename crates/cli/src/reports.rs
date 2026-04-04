use code_agent_core::{QuestionRequest, QuestionResponse, SessionId, TaskRecord};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub(crate) struct StartupReport {
    pub(crate) provider: String,
    pub(crate) model: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) project_dir: PathBuf,
    pub(crate) session_root: PathBuf,
    pub(crate) command_count: usize,
    pub(crate) prompt: Option<String>,
    pub(crate) parsed_command: Option<String>,
    pub(crate) active_session_id: Option<SessionId>,
    pub(crate) transcript_path: Option<PathBuf>,
    pub(crate) auth_source: Option<String>,
    pub(crate) turn_count: usize,
    pub(crate) stop_reason: Option<String>,
    pub(crate) applied_compaction: Option<String>,
    pub(crate) estimated_tokens_before: Option<u64>,
    pub(crate) estimated_tokens_after: Option<u64>,
    pub(crate) note: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResumeReport {
    pub(crate) session_id: SessionId,
    pub(crate) transcript_path: PathBuf,
    pub(crate) message_count: usize,
    pub(crate) preview: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ToolRunReport {
    pub(crate) tool: String,
    pub(crate) ok: bool,
    pub(crate) output: String,
    pub(crate) metadata: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginReport {
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) skill_names: Vec<String>,
    pub(crate) command_names: Vec<String>,
    pub(crate) mcp_server_names: Vec<String>,
    pub(crate) lsp_server_names: Vec<String>,
    pub(crate) command_count: usize,
    pub(crate) has_agents: bool,
    pub(crate) has_output_styles: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct CommandReport {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) category: String,
    pub(crate) kind: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) remote_safe: bool,
    pub(crate) bridge_safe: bool,
    pub(crate) requires_provider: bool,
    pub(crate) origin: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionCommandReport {
    pub(crate) session_id: SessionId,
    pub(crate) session_root: PathBuf,
    pub(crate) transcript_path: PathBuf,
    pub(crate) message_count: usize,
    pub(crate) runtime_message_count: usize,
    pub(crate) first_prompt: Option<String>,
    pub(crate) last_message_preview: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AuthCommandReport {
    pub(crate) provider: String,
    pub(crate) status: String,
    pub(crate) auth_source: Option<String>,
    pub(crate) hint: Option<String>,
    pub(crate) snapshot_path: Option<PathBuf>,
    pub(crate) resume_session_id: Option<SessionId>,
    pub(crate) resume_transcript_path: Option<PathBuf>,
    pub(crate) resume_command: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TaskCommandReport {
    pub(crate) count: usize,
    pub(crate) tasks: Vec<TaskRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuestionCommandReport {
    pub(crate) count: usize,
    pub(crate) questions: Vec<QuestionRequest>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponseCommandReport {
    pub(crate) count: usize,
    pub(crate) responses: Vec<QuestionResponse>,
}
