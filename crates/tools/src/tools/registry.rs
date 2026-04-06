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
    compatibility_tool_registry().specs()
}
