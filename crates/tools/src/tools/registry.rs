#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    FileSystem,
    Shell,
    Search,
    Network,
    Mcp,
    Agent,
    Task,
    Session,
    Ui,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub kind: ToolKind,
    pub input_schema: RootSchema,
    pub read_only: bool,
    pub needs_permission: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolContext {
    pub session_id: Option<SessionId>,
    pub cwd: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub permission_mode: Option<ToolPermissionMode>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub tool_name: String,
    pub input: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub metadata: Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        let spec = tool.spec();
        self.tools.insert(spec.name.clone(), Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .tools
            .values()
            .map(|tool| tool.spec())
            .collect::<Vec<_>>();
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        specs
    }

    pub async fn invoke(
        &self,
        request: ToolCallRequest,
        context: &ToolContext,
    ) -> Result<ToolOutput> {
        let tool = self
            .get(&request.tool_name)
            .ok_or_else(|| anyhow!("unknown tool: {}", request.tool_name))?;
        tool.invoke(request.input, context).await
    }
}

fn compatibility_tool(
    name: &str,
    description: &str,
    kind: ToolKind,
    read_only: bool,
    needs_permission: bool,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        kind,
        input_schema: RootSchema::default(),
        read_only,
        needs_permission,
    }
}

pub fn compatibility_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "file_read".to_owned(),
            description:
                "Read workspace files directly. Use this instead of shell commands like cat, head, tail, or sed when you need file contents."
                    .to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileReadToolInput),
            read_only: true,
            needs_permission: false,
        },
        ToolSpec {
            name: "file_write".to_owned(),
            description:
                "Create or replace workspace files. Use this instead of shell redirection or here-docs."
                    .to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileWriteToolInput),
            read_only: false,
            needs_permission: true,
        },
        ToolSpec {
            name: "file_edit".to_owned(),
            description:
                "Apply targeted edits to an existing file. Use this instead of sed, awk, perl, or shell one-liners."
                    .to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileEditToolInput),
            read_only: false,
            needs_permission: true,
        },
        compatibility_tool(
            "glob",
            "Expand glob patterns against the workspace.",
            ToolKind::Search,
            true,
            false,
        ),
        compatibility_tool(
            "grep",
            "Search text in workspace files directly instead of running grep or rg through bash.",
            ToolKind::Search,
            true,
            false,
        ),
        compatibility_tool(
            "bash",
            "Execute a shell command in the project only when no dedicated tool fits. Do not use this for reading or editing workspace files.",
            ToolKind::Shell,
            false,
            true,
        ),
        compatibility_tool(
            "powershell",
            "Execute a PowerShell command only when the runtime requires it and no dedicated tool fits.",
            ToolKind::Shell,
            false,
            true,
        ),
        compatibility_tool(
            "terminal_capture",
            "Capture and resume terminal output streams.",
            ToolKind::Shell,
            true,
            false,
        ),
        compatibility_tool(
            "web_fetch",
            "Fetch remote documents and APIs.",
            ToolKind::Network,
            true,
            true,
        ),
        compatibility_tool(
            "web_search",
            "Search the web for context and sources.",
            ToolKind::Network,
            true,
            true,
        ),
        compatibility_tool(
            "web_browser",
            "Drive a browser-backed research session.",
            ToolKind::Network,
            false,
            true,
        ),
        compatibility_tool(
            "mcp",
            "Call a registered MCP tool.",
            ToolKind::Mcp,
            false,
            true,
        ),
        compatibility_tool(
            "list_mcp_resources",
            "List resources exposed by MCP servers.",
            ToolKind::Mcp,
            true,
            false,
        ),
        compatibility_tool(
            "read_mcp_resource",
            "Read a resource exposed by an MCP server.",
            ToolKind::Mcp,
            true,
            false,
        ),
        compatibility_tool(
            "mcp_auth",
            "Authenticate or refresh MCP credentials.",
            ToolKind::Mcp,
            false,
            true,
        ),
        compatibility_tool(
            "agent",
            "Spawn or resume an agent task.",
            ToolKind::Agent,
            false,
            true,
        ),
        compatibility_tool(
            "task_create",
            "Create a background task.",
            ToolKind::Task,
            false,
            true,
        ),
        compatibility_tool(
            "task_get",
            "Inspect a single background task.",
            ToolKind::Task,
            true,
            false,
        ),
        compatibility_tool(
            "task_list",
            "List background tasks.",
            ToolKind::Task,
            true,
            false,
        ),
        compatibility_tool(
            "task_update",
            "Update task metadata or ownership.",
            ToolKind::Task,
            false,
            true,
        ),
        compatibility_tool(
            "task_stop",
            "Stop a running task.",
            ToolKind::Task,
            false,
            true,
        ),
        compatibility_tool(
            "todo_write",
            "Write structured todo state for the current session.",
            ToolKind::Session,
            false,
            false,
        ),
        compatibility_tool(
            "memory",
            "Read or update durable memory state.",
            ToolKind::Session,
            false,
            true,
        ),
        compatibility_tool(
            "send_message",
            "Send a message to a user, teammate, or remote bridge.",
            ToolKind::Ui,
            false,
            false,
        ),
        compatibility_tool(
            "ask_user_question",
            "Pause execution and request additional user input.",
            ToolKind::Ui,
            false,
            false,
        ),
        compatibility_tool(
            "workflow",
            "Start a workflow-oriented multi-step automation.",
            ToolKind::Agent,
            false,
            true,
        ),
    ]
}

