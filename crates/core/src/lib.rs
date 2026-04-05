use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub type SessionId = Uuid;
pub type AgentId = Uuid;
pub type TaskId = Uuid;
pub type QuestionId = Uuid;

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
    Attachment,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolCall { call: ToolCall },
    ToolResult { result: ToolResult },
    Attachment { attachment: AttachmentRef },
    Boundary { boundary: BoundaryMarker },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub session_id: Option<SessionId>,
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub metadata: MessageMetadata,
    pub created_at_unix_ms: i64,
}

impl Message {
    pub fn new(role: MessageRole, blocks: Vec<ContentBlock>) -> Self {
        Self {
            id: Uuid::new_v4(),
            parent_id: None,
            session_id: None,
            role,
            blocks,
            metadata: MessageMetadata::default(),
            created_at_unix_ms: unix_time_ms(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageMetadata {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub usage: Option<TokenUsage>,
    pub tags: Vec<String>,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub output_text: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentRef {
    pub name: String,
    pub uri: String,
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundaryMarker {
    pub kind: BoundaryKind,
    pub summary_message_id: Option<Uuid>,
    pub preserved_tail_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BoundaryKind {
    Compact,
    MicroCompact,
    SessionMemory,
    Resume,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TaskStatus {
    #[default]
    Pending,
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub session_id: Option<SessionId>,
    pub parent_task_id: Option<TaskId>,
    pub agent_id: Option<AgentId>,
    pub title: String,
    pub kind: String,
    pub status: TaskStatus,
    pub input: Option<String>,
    pub output: Option<String>,
    pub question_id: Option<QuestionId>,
    pub transcript_path: Option<PathBuf>,
    pub artifact_path: Option<PathBuf>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl TaskRecord {
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        let now = unix_time_ms();
        Self {
            id: TaskId::new_v4(),
            title: title.into(),
            kind: kind.into(),
            status: TaskStatus::Pending,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            ..Self::default()
        }
    }

    pub fn touch(&mut self) {
        self.updated_at_unix_ms = unix_time_ms();
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunRequest {
    pub session_id: Option<SessionId>,
    pub parent_task_id: Option<TaskId>,
    pub agent_id: Option<AgentId>,
    pub title: String,
    pub prompt: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunResult {
    pub task_id: TaskId,
    pub session_id: Option<SessionId>,
    pub status: TaskStatus,
    pub output: Option<String>,
    pub transcript_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionRequest {
    pub id: QuestionId,
    pub task_id: Option<TaskId>,
    pub prompt: String,
    pub choices: Vec<String>,
    pub context: BTreeMap<String, String>,
    pub created_at_unix_ms: i64,
}

impl QuestionRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            id: QuestionId::new_v4(),
            prompt: prompt.into(),
            created_at_unix_ms: unix_time_ms(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionResponse {
    pub question_id: QuestionId,
    pub answer: String,
    pub metadata: BTreeMap<String, String>,
    pub answered_at_unix_ms: i64,
}

impl QuestionResponse {
    pub fn new(question_id: QuestionId, answer: impl Into<String>) -> Self {
        Self {
            question_id,
            answer: answer.into(),
            answered_at_unix_ms: unix_time_ms(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TaskStoreSnapshot {
    tasks: Vec<TaskRecord>,
    questions: Vec<QuestionRequest>,
    responses: Vec<QuestionResponse>,
}

pub trait TaskStore {
    fn create_task(&self, record: TaskRecord) -> Result<TaskRecord>;
    fn save_task(&self, record: TaskRecord) -> Result<TaskRecord>;
    fn get_task(&self, task_id: TaskId) -> Result<Option<TaskRecord>>;
    fn list_tasks(&self) -> Result<Vec<TaskRecord>>;
    fn record_question(&self, request: QuestionRequest) -> Result<QuestionRequest>;
    fn list_questions(&self) -> Result<Vec<QuestionRequest>>;
    fn answer_question(&self, response: QuestionResponse) -> Result<QuestionResponse>;
    fn list_responses(&self) -> Result<Vec<QuestionResponse>>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalTaskStore {
    pub root: PathBuf,
}

impl LocalTaskStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    pub fn data_path(&self) -> PathBuf {
        self.root.join("tasks.json")
    }

    fn read_snapshot(&self) -> Result<TaskStoreSnapshot> {
        let path = self.data_path();
        if !path.exists() {
            return Ok(TaskStoreSnapshot::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read task store {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to decode task store {}", path.display()))
    }

    fn write_snapshot(&self, snapshot: &TaskStoreSnapshot) -> Result<()> {
        let path = self.data_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create task store dir {}", parent.display()))?;
        }
        fs::write(&path, serde_json::to_vec_pretty(snapshot)?)
            .with_context(|| format!("failed to write task store {}", path.display()))?;
        Ok(())
    }
}

impl TaskStore for LocalTaskStore {
    fn create_task(&self, mut record: TaskRecord) -> Result<TaskRecord> {
        if record.id.is_nil() {
            record.id = TaskId::new_v4();
        }
        if record.created_at_unix_ms == 0 {
            record.created_at_unix_ms = unix_time_ms();
        }
        record.touch();

        let mut snapshot = self.read_snapshot()?;
        snapshot.tasks.retain(|task| task.id != record.id);
        snapshot.tasks.push(record.clone());
        snapshot.tasks.sort_by_key(|task| task.created_at_unix_ms);
        self.write_snapshot(&snapshot)?;
        Ok(record)
    }

    fn save_task(&self, mut record: TaskRecord) -> Result<TaskRecord> {
        if record.created_at_unix_ms == 0 {
            record.created_at_unix_ms = unix_time_ms();
        }
        record.touch();
        let mut snapshot = self.read_snapshot()?;
        snapshot.tasks.retain(|task| task.id != record.id);
        snapshot.tasks.push(record.clone());
        snapshot.tasks.sort_by_key(|task| task.created_at_unix_ms);
        self.write_snapshot(&snapshot)?;
        Ok(record)
    }

    fn get_task(&self, task_id: TaskId) -> Result<Option<TaskRecord>> {
        Ok(self
            .read_snapshot()?
            .tasks
            .into_iter()
            .find(|task| task.id == task_id))
    }

    fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks = self.read_snapshot()?.tasks;
        tasks.sort_by(|left, right| {
            left.updated_at_unix_ms
                .cmp(&right.updated_at_unix_ms)
                .then(left.created_at_unix_ms.cmp(&right.created_at_unix_ms))
        });
        Ok(tasks)
    }

    fn record_question(&self, mut request: QuestionRequest) -> Result<QuestionRequest> {
        if request.id.is_nil() {
            request.id = QuestionId::new_v4();
        }
        if request.created_at_unix_ms == 0 {
            request.created_at_unix_ms = unix_time_ms();
        }
        let mut snapshot = self.read_snapshot()?;
        snapshot
            .questions
            .retain(|question| question.id != request.id);
        snapshot.questions.push(request.clone());
        snapshot
            .questions
            .sort_by_key(|question| question.created_at_unix_ms);
        self.write_snapshot(&snapshot)?;
        Ok(request)
    }

    fn list_questions(&self) -> Result<Vec<QuestionRequest>> {
        let mut questions = self.read_snapshot()?.questions;
        questions.sort_by_key(|question| question.created_at_unix_ms);
        Ok(questions)
    }

    fn answer_question(&self, mut response: QuestionResponse) -> Result<QuestionResponse> {
        if response.answered_at_unix_ms == 0 {
            response.answered_at_unix_ms = unix_time_ms();
        }
        let mut snapshot = self.read_snapshot()?;
        snapshot
            .responses
            .retain(|entry| entry.question_id != response.question_id);
        snapshot.responses.push(response.clone());
        snapshot
            .responses
            .sort_by_key(|entry| entry.answered_at_unix_ms);
        self.write_snapshot(&snapshot)?;
        Ok(response)
    }

    fn list_responses(&self) -> Result<Vec<QuestionResponse>> {
        let mut responses = self.read_snapshot()?.responses;
        responses.sort_by_key(|response| response.answered_at_unix_ms);
        Ok(responses)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRequest {
    pub session_id: Option<SessionId>,
    pub title: String,
    pub prompt: Option<String>,
    pub run_inline: bool,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowTaskRequest {
    pub session_id: Option<SessionId>,
    pub title: String,
    pub prompt: Option<String>,
    pub steps: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowTaskSet {
    pub workflow: TaskRecord,
    pub children: Vec<TaskRecord>,
}

pub fn create_agent_task<S: TaskStore>(store: &S, request: AgentTaskRequest) -> Result<TaskRecord> {
    let mut task = TaskRecord::new(
        "agent",
        if request.title.trim().is_empty() {
            "agent task"
        } else {
            request.title.as_str()
        },
    );
    task.session_id = request.session_id;
    task.input = request.prompt;
    task.metadata = request.metadata;
    task.status = if request.run_inline {
        TaskStatus::Completed
    } else {
        TaskStatus::Pending
    };
    task.output = request
        .run_inline
        .then(|| "agent task created and marked complete".to_owned());
    store.create_task(task)
}

pub fn create_workflow_task_set<S: TaskStore>(
    store: &S,
    request: WorkflowTaskRequest,
) -> Result<WorkflowTaskSet> {
    let mut workflow = TaskRecord::new(
        "workflow",
        if request.title.trim().is_empty() {
            "workflow"
        } else {
            request.title.as_str()
        },
    );
    workflow.session_id = request.session_id;
    workflow.input = request.prompt;
    workflow.metadata = request.metadata;
    let workflow = store.create_task(workflow)?;

    let mut children = Vec::new();
    for step in request
        .steps
        .into_iter()
        .filter(|step| !step.trim().is_empty())
    {
        let mut child = TaskRecord::new("workflow_step", step.clone());
        child.parent_task_id = Some(workflow.id);
        child.session_id = request.session_id;
        child.input = Some(step);
        children.push(store.create_task(child)?);
    }

    Ok(WorkflowTaskSet { workflow, children })
}

pub fn update_task_record<S: TaskStore>(
    store: &S,
    mut task: TaskRecord,
    status: TaskStatus,
    output: Option<String>,
) -> Result<TaskRecord> {
    task.status = status;
    task.output = output;
    store.save_task(task)
}

pub fn resume_tasks_for_question<S: TaskStore>(
    store: &S,
    question_id: QuestionId,
) -> Result<Vec<TaskRecord>> {
    let mut resumed = Vec::new();
    for task in store
        .list_tasks()?
        .into_iter()
        .filter(|task| task.question_id == Some(question_id))
    {
        resumed.push(update_task_record(store, task, TaskStatus::Running, None)?);
    }
    Ok(resumed)
}

pub fn create_coordinator_task<S: TaskStore>(
    store: &S,
    session_id: SessionId,
    instruction: impl Into<String>,
) -> Result<TaskRecord> {
    let instruction = instruction.into();
    let mut task = TaskRecord::new("coordinator", "coordinator directive");
    task.session_id = Some(session_id);
    task.status = TaskStatus::Running;
    task.input = Some(instruction);
    store.create_task(task)
}

pub fn create_coordinator_worker_task<S: TaskStore>(
    store: &S,
    session_id: SessionId,
    parent_task_id: TaskId,
    agent_id: AgentId,
    title: impl Into<String>,
    input: impl Into<String>,
    transcript_path: Option<PathBuf>,
) -> Result<TaskRecord> {
    let mut task = TaskRecord::new("assistant_worker", title.into());
    task.session_id = Some(session_id);
    task.parent_task_id = Some(parent_task_id);
    task.agent_id = Some(agent_id);
    task.status = TaskStatus::Running;
    task.input = Some(input.into());
    task.transcript_path = transcript_path;
    store.create_task(task)
}

pub fn create_coordinator_synthesis_task<S: TaskStore>(
    store: &S,
    session_id: SessionId,
    parent_task_id: TaskId,
    input: impl Into<String>,
) -> Result<TaskRecord> {
    let mut task = TaskRecord::new("assistant_synthesis", "coordinator synthesis");
    task.session_id = Some(session_id);
    task.parent_task_id = Some(parent_task_id);
    task.status = TaskStatus::Running;
    task.input = Some(input.into());
    store.create_task(task)
}

fn trim_task_marker(input: &str) -> &str {
    let trimmed = input.trim();
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return rest.trim();
    }

    let digit_prefix_len = trimmed
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    if digit_prefix_len > 0 {
        let rest = &trimmed[digit_prefix_len..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return rest.trim();
        }
    }

    trimmed
}

pub fn coordinator_tasks(instruction: &str) -> Vec<String> {
    let line_tasks = instruction
        .lines()
        .map(trim_task_marker)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if line_tasks.len() >= 2 {
        return line_tasks.into_iter().take(4).collect();
    }

    let sentence_tasks = instruction
        .split(". ")
        .map(trim_task_marker)
        .filter(|line| !line.is_empty())
        .map(|line| line.trim_end_matches('.').to_owned())
        .collect::<Vec<_>>();
    if sentence_tasks.len() >= 2 {
        return sentence_tasks.into_iter().take(4).collect();
    }

    let trimmed = instruction.trim();
    if trimmed.is_empty() {
        Vec::new()
    } else {
        vec![trimmed.to_owned()]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppEvent {
    CommandDispatched { name: String },
    MessagePersisted { message_id: Uuid },
    ToolStarted { tool_name: String },
    ToolCompleted { tool_name: String },
    CompactApplied { kind: BoundaryKind },
    RemoteConnected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CommandKind {
    #[default]
    Local,
    Prompt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CommandCategory {
    Session,
    Auth,
    #[default]
    Config,
    Tooling,
    Advanced,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CommandSource {
    #[default]
    BuiltIn,
    Plugin,
    Skill,
    Workflow,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub aliases: Vec<String>,
    pub category: CommandCategory,
    pub kind: CommandKind,
    pub interactive: bool,
    pub supports_non_interactive: bool,
    pub requires_provider: bool,
    pub source: CommandSource,
    pub hidden: bool,
    pub remote_safe: bool,
    pub bridge_safe: bool,
    pub origin: Option<String>,
}

impl CommandSpec {
    pub fn with_source(mut self, source: CommandSource, origin: Option<String>) -> Self {
        self.source = source;
        self.origin = origin;
        self
    }

    pub fn with_safety(mut self, remote_safe: bool, bridge_safe: bool) -> Self {
        self.remote_safe = remote_safe;
        self.bridge_safe = bridge_safe;
        self
    }

    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandInvocation {
    pub name: String,
    pub args: Vec<String>,
    pub raw_input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppState {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub prompt: String,
    pub active_command: Option<String>,
    pub provider: Option<String>,
}

impl AppState {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            cwd,
            prompt: String::new(),
            active_command: None,
            provider: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    specs: BTreeMap<String, CommandSpec>,
    aliases: BTreeMap<String, String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: CommandSpec) {
        let canonical = spec.name.to_ascii_lowercase();
        for alias in &spec.aliases {
            self.aliases
                .insert(alias.to_ascii_lowercase(), canonical.clone());
        }
        self.specs.insert(canonical, spec);
    }

    pub fn extend<I>(&mut self, specs: I)
    where
        I: IntoIterator<Item = CommandSpec>,
    {
        for spec in specs {
            self.register(spec);
        }
    }

    pub fn resolve(&self, name: &str) -> Option<&CommandSpec> {
        let lowered = name.to_ascii_lowercase();
        if let Some(spec) = self.specs.get(&lowered) {
            return Some(spec);
        }
        let canonical = self.aliases.get(&lowered)?;
        self.specs.get(canonical)
    }

    pub fn parse_slash_command(&self, input: &str) -> Option<CommandInvocation> {
        let trimmed = input.trim();
        let body = trimmed.strip_prefix('/')?;
        let mut parts = body.split_whitespace();
        let name = parts.next()?;
        let spec = self.resolve(name)?;

        Some(CommandInvocation {
            name: spec.name.clone(),
            args: parts.map(str::to_owned).collect(),
            raw_input: trimmed.to_owned(),
        })
    }

    pub fn all(&self) -> Vec<&CommandSpec> {
        self.specs.values().filter(|spec| !spec.hidden).collect()
    }

    pub fn all_owned(&self) -> Vec<CommandSpec> {
        self.specs
            .values()
            .filter(|spec| !spec.hidden)
            .cloned()
            .collect()
    }

    pub fn remote_safe(&self) -> Vec<&CommandSpec> {
        self.specs
            .values()
            .filter(|spec| !spec.hidden && spec.remote_safe)
            .collect()
    }

    pub fn bridge_safe(&self) -> Vec<&CommandSpec> {
        self.specs
            .values()
            .filter(|spec| !spec.hidden && spec.bridge_safe)
            .collect()
    }

    pub fn is_remote_safe(&self, name: &str) -> bool {
        self.resolve(name)
            .map(|spec| spec.remote_safe)
            .unwrap_or(false)
    }

    pub fn is_bridge_safe(&self, name: &str) -> bool {
        self.resolve(name)
            .map(|spec| spec.bridge_safe)
            .unwrap_or(false)
    }
}

#[allow(clippy::too_many_arguments)]
fn builtin_command(
    name: &str,
    description: &str,
    category: CommandCategory,
    kind: CommandKind,
    aliases: &[&str],
    requires_provider: bool,
    interactive: bool,
    supports_non_interactive: bool,
    remote_safe: bool,
    bridge_safe: bool,
) -> CommandSpec {
    CommandSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        category,
        kind,
        interactive,
        supports_non_interactive,
        requires_provider,
        source: CommandSource::BuiltIn,
        hidden: false,
        remote_safe,
        bridge_safe,
        origin: None,
    }
}

pub fn compatibility_command_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    for spec in [
        builtin_command(
            "help",
            "Show command help and slash-command usage.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "version",
            "Print the current runtime version.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "clear",
            "Clear the current conversation state.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            true,
        ),
        builtin_command(
            "copy",
            "Copy the latest assistant response, or /copy N for an older one.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            true,
        ),
        builtin_command(
            "compact",
            "Compact the current conversation.",
            CommandCategory::Session,
            CommandKind::Prompt,
            &[],
            true,
            true,
            true,
            false,
            true,
        ),
        builtin_command(
            "resume",
            "Resume a saved session or transcript.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "session",
            "Inspect the active local or remote session.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "login",
            "Authenticate against the selected provider.",
            CommandCategory::Auth,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "logout",
            "Clear cached credentials for the selected provider.",
            CommandCategory::Auth,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "permissions",
            "Inspect the active permission policy and pending approvals.",
            CommandCategory::Auth,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "model",
            "Inspect or switch the active model.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            true,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "config",
            "Inspect or update runtime configuration.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "status",
            "Show a compact runtime status summary.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "ide",
            "Inspect IDE bridge compatibility and connection state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "statusline",
            "Inspect statusline rendering state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "theme",
            "Inspect terminal theme compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "vim",
            "Inspect or toggle vim-mode compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "plan",
            "Inspect plan-mode compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "fast",
            "Inspect fast-mode compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "passes",
            "Inspect multi-pass execution compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "effort",
            "Inspect reasoning effort compatibility state.",
            CommandCategory::Config,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "mcp",
            "Manage MCP servers and resources.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "plugin",
            "Manage plugins and compatibility bridges.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "skills",
            "Inspect discovered skills and skill-backed commands.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "reload-plugins",
            "Reload plugin and skill discovery state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "hooks",
            "Inspect plugin hook compatibility state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "output-style",
            "Inspect output-style compatibility state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &["outputstyle"],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "files",
            "Preview workspace files and compatibility-visible file state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            true,
        ),
        builtin_command(
            "diff",
            "Preview recent edit and diff state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            true,
        ),
        builtin_command(
            "remote-env",
            "Inspect remote-control environment compatibility state.",
            CommandCategory::Tooling,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "memory",
            "Inspect durable or session-scoped memory state.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "usage",
            "Inspect usage accounting and token totals.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            false,
        ),
        builtin_command(
            "cost",
            "Inspect cost-style compatibility reporting.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            true,
            true,
        ),
        builtin_command(
            "stats",
            "Inspect runtime statistics and compatibility counters.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "export",
            "Export the active transcript or runtime snapshot.",
            CommandCategory::Session,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "tasks",
            "Manage background, local, or remote agent tasks.",
            CommandCategory::Advanced,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "voice",
            "Control voice capture and playback features.",
            CommandCategory::Advanced,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "remote-control",
            "Start or attach to remote bridge workflows.",
            CommandCategory::Advanced,
            CommandKind::Local,
            &["rc", "remote", "sync", "bridge"],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "agents",
            "Inspect and manage agents and agent definitions.",
            CommandCategory::Advanced,
            CommandKind::Local,
            &[],
            false,
            true,
            true,
            false,
            false,
        ),
        builtin_command(
            "exit",
            "Exit the interactive session.",
            CommandCategory::Advanced,
            CommandKind::Local,
            &["quit"],
            false,
            true,
            true,
            true,
            false,
        ),
    ] {
        registry.register(spec);
    }

    registry
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuntimeMode {
    Interactive,
    NonInteractive,
    RemoteControl,
    Voice,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionPolicy {
    pub mode: Option<PermissionMode>,
    pub allowlisted_tools: Vec<String>,
    pub denylisted_tools: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSelection {
    pub provider: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub cwd: PathBuf,
    pub mode: Option<RuntimeMode>,
    pub session_id: Option<SessionId>,
    pub provider: Option<ProviderSelection>,
    pub permission_policy: PermissionPolicy,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResumeTarget {
    SessionId { session_id: SessionId },
    TranscriptPath { path: PathBuf },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionRequest {
    pub kind: BoundaryKind,
    pub trigger: String,
    pub max_tokens_before: Option<u64>,
    pub target_tokens_after: Option<u64>,
}

impl Default for CompactionRequest {
    fn default() -> Self {
        Self {
            kind: BoundaryKind::Compact,
            trigger: String::new(),
            max_tokens_before: None,
            target_tokens_after: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionPlan {
    pub tool_name: String,
    pub needs_permission: bool,
    pub retryable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnRequest {
    pub input: String,
    pub command: Option<CommandInvocation>,
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TurnResponse {
    pub messages: Vec<Message>,
    pub events: Vec<AppEvent>,
    pub pending_tool: Option<ToolExecutionPlan>,
    pub applied_compaction: Option<CompactionRequest>,
}

#[cfg(test)]
mod tests;
