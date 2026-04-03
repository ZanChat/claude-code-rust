use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use code_agent_core::{
    create_agent_task, create_workflow_task_set, AgentTaskRequest, LocalTaskStore,
    QuestionRequest, SessionId, TaskRecord, TaskStatus, TaskStore, WorkflowTaskRequest,
};
use code_agent_mcp::{
    call_tool_from_config, clear_cached_auth_token, clear_pending_device_flow,
    list_resources_from_config, load_cached_auth_token, load_pending_device_flow,
    parse_mcp_server_configs, poll_oauth_device_flow, read_resource_from_config,
    refresh_oauth_device_token, start_oauth_device_flow, store_cached_auth_token,
    CachedMcpAuthToken, McpAuthConfig, McpServerConfig,
};
use code_agent_plugins::{OutOfProcessPluginRuntime, PluginRuntime};
use reqwest::Method;
use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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
        compatibility_tool(
            "file_read",
            "Read files from the active workspace.",
            ToolKind::FileSystem,
            true,
            false,
        ),
        compatibility_tool(
            "file_write",
            "Write or replace workspace files.",
            ToolKind::FileSystem,
            false,
            true,
        ),
        compatibility_tool(
            "file_edit",
            "Apply targeted edits to an existing file.",
            ToolKind::FileSystem,
            false,
            true,
        ),
        compatibility_tool(
            "glob",
            "Expand glob patterns against the workspace.",
            ToolKind::Search,
            true,
            false,
        ),
        compatibility_tool(
            "grep",
            "Search text in the workspace.",
            ToolKind::Search,
            true,
            false,
        ),
        compatibility_tool(
            "bash",
            "Execute a shell command in the project.",
            ToolKind::Shell,
            false,
            true,
        ),
        compatibility_tool(
            "powershell",
            "Execute a PowerShell command when the runtime requires it.",
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

fn input_string(input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing string field '{key}'"))
}

fn input_string_or(input: &Value, key: &str, default: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn input_bool_or(input: &Value, key: &str, default: bool) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn input_u64_or(input: &Value, key: &str, default: u64) -> u64 {
    input.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn optional_string(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn string_list_field(input: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("field '{key}' must be an array"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("field '{key}' entries must be strings"))
        })
        .collect()
}

fn string_map_field(input: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = input.get(key) else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("field '{key}' must be an object"))?;
    let mut map = BTreeMap::new();
    for (entry_key, entry_value) in object {
        let entry_value = entry_value
            .as_str()
            .ok_or_else(|| anyhow!("field '{key}.{entry_key}' must be a string"))?;
        map.insert(entry_key.clone(), entry_value.to_owned());
    }
    Ok(map)
}

fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn runtime_dir(cwd: &Path) -> PathBuf {
    cwd.join(".code-agent")
}

fn task_store(cwd: &Path) -> LocalTaskStore {
    LocalTaskStore::new(runtime_dir(cwd))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn append_jsonl(path: &Path, value: &Value) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    content.push_str(&serde_json::to_string(value)?);
    content.push('\n');
    fs::write(path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn strip_html_tags(input: &str) -> String {
    let mut text = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn load_mcp_server_config(cwd: &Path, input: &Value) -> Result<McpServerConfig> {
    let root = input
        .get("plugin_root")
        .and_then(Value::as_str)
        .map(|value| resolve_path(cwd, value))
        .unwrap_or_else(|| cwd.to_path_buf());
    let server_name = input_string(input, "server")?;
    let runtime = OutOfProcessPluginRuntime;
    let loaded = runtime
        .load_manifest(&root)
        .await
        .with_context(|| format!("failed to load plugin manifest from {}", root.display()))?;
    let servers = parse_mcp_server_configs(&loaded.manifest.mcp_servers);
    servers
        .get(&server_name)
        .cloned()
        .ok_or_else(|| anyhow!("unknown MCP server '{server_name}' in {}", root.display()))
}

fn collect_files(base: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if base.is_file() {
        files.push(base.to_path_buf());
        return Ok(());
    }

    if !base.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(base).with_context(|| format!("failed to read {}", base.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn normalize_for_match(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    if pattern.starts_with(b"**") {
        let rest = &pattern[2..];
        if rest.is_empty() {
            return true;
        }
        for skip in 0..=text.len() {
            if wildcard_match(rest, &text[skip..]) {
                return true;
            }
        }
        return false;
    }

    match pattern[0] {
        b'*' => {
            if wildcard_match(&pattern[1..], text) {
                return true;
            }
            let mut idx = 0usize;
            while idx < text.len() && text[idx] != b'/' {
                idx += 1;
                if wildcard_match(&pattern[1..], &text[idx..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !text.is_empty() && text[0] != b'/' && wildcard_match(&pattern[1..], &text[1..]),
        ch => !text.is_empty() && ch == text[0] && wildcard_match(&pattern[1..], &text[1..]),
    }
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    wildcard_match(pattern.as_bytes(), path.as_bytes())
}

#[derive(Clone, Debug)]
struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "file_read",
            "Read files from the active workspace.",
            ToolKind::FileSystem,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = resolve_path(&context.cwd, &input_string(&input, "path")?);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: json!({ "path": path }),
        })
    }
}

#[derive(Clone, Debug)]
struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "file_write",
            "Write or replace workspace files.",
            ToolKind::FileSystem,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = resolve_path(&context.cwd, &input_string(&input, "path")?);
        let content = input_string(&input, "content")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, content.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(ToolOutput {
            content: format!("wrote {}", path.display()),
            is_error: false,
            metadata: json!({ "path": path, "bytes": content.len() }),
        })
    }
}

#[derive(Clone, Debug)]
struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "file_edit",
            "Apply targeted edits to an existing file.",
            ToolKind::FileSystem,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = resolve_path(&context.cwd, &input_string(&input, "path")?);
        let old_string = input_string(&input, "old_string")?;
        let new_string = input_string(&input, "new_string")?;
        let replace_all = input_bool_or(&input, "replace_all", false);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let matches = content.matches(&old_string).count();
        if matches == 0 {
            bail!("target string not found in {}", path.display());
        }

        let updated = if replace_all {
            content.replace(&old_string, &new_string)
        } else {
            content.replacen(&old_string, &new_string, 1)
        };
        fs::write(&path, updated.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(ToolOutput {
            content: format!("edited {}", path.display()),
            is_error: false,
            metadata: json!({
                "path": path,
                "replacements": if replace_all { matches } else { 1 },
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "bash",
            "Execute a shell command in the project.",
            ToolKind::Shell,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let command = input_string(&input, "command")?;
        let output = Command::new("bash")
            .arg("-lc")
            .arg(&command)
            .current_dir(&context.cwd)
            .envs(&context.environment)
            .output()
            .with_context(|| format!("failed to execute bash command: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let content = if stderr.is_empty() {
            stdout.to_string()
        } else if stdout.is_empty() {
            stderr.to_string()
        } else {
            format!("{stdout}\n{stderr}")
        };
        Ok(ToolOutput {
            content,
            is_error: !output.status.success(),
            metadata: json!({
                "command": command,
                "exit_code": output.status.code(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "powershell",
            "Execute a PowerShell command when the runtime requires it.",
            ToolKind::Shell,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let command = input_string(&input, "command")?;
        let output = Command::new("pwsh")
            .arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(&command)
            .current_dir(&context.cwd)
            .envs(&context.environment)
            .output()
            .with_context(|| format!("failed to execute pwsh command: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(ToolOutput {
            content: if stderr.is_empty() {
                stdout.to_string()
            } else if stdout.is_empty() {
                stderr.to_string()
            } else {
                format!("{stdout}\n{stderr}")
            },
            is_error: !output.status.success(),
            metadata: json!({
                "command": command,
                "exit_code": output.status.code(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct TerminalCaptureTool;

#[async_trait]
impl Tool for TerminalCaptureTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "terminal_capture",
            "Capture and resume terminal output streams.",
            ToolKind::Shell,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "start");
        let dir = runtime_dir(&context.cwd).join("terminal-captures");
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        match action.as_str() {
            "start" => {
                let command = input_string(&input, "command")?;
                let id = optional_string(&input, "id")
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let shell = input_string_or(&input, "shell", "bash");
                let output = Command::new(&shell)
                    .arg("-lc")
                    .arg(&command)
                    .current_dir(&context.cwd)
                    .envs(&context.environment)
                    .output()
                    .with_context(|| {
                        format!("failed to execute capture command with {shell}: {command}")
                    })?;
                let record = json!({
                    "id": id,
                    "shell": shell,
                    "command": command,
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                    "exit_code": output.status.code(),
                });
                let path = dir.join(format!(
                    "{}.json",
                    record["id"].as_str().unwrap_or("capture")
                ));
                fs::write(&path, serde_json::to_vec_pretty(&record)?)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                let content = record["stdout"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| record["stderr"].as_str().unwrap_or_default())
                    .to_owned();
                Ok(ToolOutput {
                    content,
                    is_error: output.status.code().unwrap_or(1) != 0,
                    metadata: json!({ "path": path, "record": record }),
                })
            }
            "get" | "resume" => {
                let id = input_string(&input, "id")?;
                let path = dir.join(format!("{id}.json"));
                let raw = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let value: Value = serde_json::from_str(&raw)?;
                Ok(ToolOutput {
                    content: value["stdout"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| value["stderr"].as_str().unwrap_or_default())
                        .to_owned(),
                    is_error: value["exit_code"].as_i64().unwrap_or_default() != 0,
                    metadata: json!({ "path": path, "record": value }),
                })
            }
            "list" => {
                let mut sessions = Vec::new();
                if dir.exists() {
                    for entry in fs::read_dir(&dir)? {
                        let path = entry?.path();
                        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                            continue;
                        }
                        let raw = fs::read_to_string(&path)?;
                        sessions.push(serde_json::from_str::<Value>(&raw)?);
                    }
                }
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&sessions)?,
                    is_error: false,
                    metadata: json!({ "count": sessions.len() }),
                })
            }
            other => bail!("unsupported terminal_capture action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_fetch",
            "Fetch remote documents and APIs.",
            ToolKind::Network,
            true,
            true,
        )
    }

    async fn invoke(&self, input: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let url = input_string(&input, "url")?;
        let method = Method::from_bytes(input_string_or(&input, "method", "GET").as_bytes())?;
        let headers = string_map_field(&input, "headers")?;
        let body = input.get("body").and_then(Value::as_str).map(str::to_owned);
        let client = reqwest::Client::new();
        let mut request = client.request(method.clone(), &url);
        for (key, value) in &headers {
            request = request.header(key, value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = request.send().await?;
        let status = response.status();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await?;

        Ok(ToolOutput {
            content: text,
            is_error: !status.is_success(),
            metadata: json!({
                "url": final_url,
                "status": status.as_u16(),
                "method": method.as_str(),
                "content_type": content_type,
                "header_count": headers.len(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct WebSearchTool;

fn collect_search_results(value: &Value, results: &mut Vec<Value>) {
    if let Some(url) = value.get("FirstURL").and_then(Value::as_str) {
        results.push(json!({
            "title": value.get("Text").and_then(Value::as_str).unwrap_or(url),
            "url": url,
            "snippet": value.get("Text").and_then(Value::as_str).unwrap_or_default(),
        }));
    }
    if let Some(items) = value.get("RelatedTopics").and_then(Value::as_array) {
        for item in items {
            collect_search_results(item, results);
        }
    }
    if let Some(items) = value.as_array() {
        for item in items {
            collect_search_results(item, results);
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_search",
            "Search the web for context and sources.",
            ToolKind::Network,
            true,
            true,
        )
    }

    async fn invoke(&self, input: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let query = input_string(&input, "query")?;
        let limit = input_u64_or(&input, "limit", 5) as usize;
        let base_url = input_string_or(&input, "base_url", "https://api.duckduckgo.com/");
        let response = reqwest::Client::new()
            .get(&base_url)
            .query(&[
                ("q", query.as_str()),
                ("format", "json"),
                ("no_html", "1"),
                ("skip_disambig", "1"),
            ])
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        let mut results = Vec::new();
        if let Some(text) = value.get("AbstractText").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                results.push(json!({
                    "title": value.get("Heading").and_then(Value::as_str).unwrap_or("abstract"),
                    "url": value.get("AbstractURL").and_then(Value::as_str).unwrap_or_default(),
                    "snippet": text,
                }));
            }
        }
        collect_search_results(&value, &mut results);
        results.truncate(limit);
        let content = results
            .iter()
            .map(|entry| {
                format!(
                    "{}\n{}\n{}",
                    entry["title"].as_str().unwrap_or_default(),
                    entry["url"].as_str().unwrap_or_default(),
                    entry["snippet"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput {
            content,
            is_error: !status.is_success(),
            metadata: json!({ "query": query, "results": results }),
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct BrowserSessionState {
    id: String,
    current_url: Option<String>,
    history: Vec<String>,
    title: Option<String>,
    page_html: Option<String>,
}

#[derive(Clone, Debug)]
struct WebBrowserTool;

fn browser_session_dir(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("browser")
}

fn browser_session_path(cwd: &Path, session_id: &str) -> PathBuf {
    browser_session_dir(cwd).join(format!("{session_id}.json"))
}

fn load_browser_session(cwd: &Path, session_id: &str) -> Result<BrowserSessionState> {
    let path = browser_session_path(cwd, session_id);
    if !path.exists() {
        return Ok(BrowserSessionState {
            id: session_id.to_owned(),
            ..BrowserSessionState::default()
        });
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode {}", path.display()))?)
}

fn save_browser_session(cwd: &Path, state: &BrowserSessionState) -> Result<PathBuf> {
    let path = browser_session_path(cwd, &state.id);
    ensure_parent_dir(&path)?;
    fs::write(&path, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

#[async_trait]
impl Tool for WebBrowserTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "web_browser",
            "Drive a browser-backed research session.",
            ToolKind::Network,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "open");
        let session_id = optional_string(&input, "session_id")
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut state = load_browser_session(&context.cwd, &session_id)?;
        match action.as_str() {
            "open" | "navigate" => {
                let url = input_string(&input, "url")?;
                let response = reqwest::Client::new().get(&url).send().await?;
                let status = response.status();
                let html = response.text().await?;
                let title = html
                    .split("<title>")
                    .nth(1)
                    .and_then(|rest| rest.split("</title>").next())
                    .map(str::trim)
                    .map(str::to_owned);
                state.current_url = Some(url.clone());
                state.history.push(url.clone());
                state.title = title;
                state.page_html = Some(html.clone());
                let path = save_browser_session(&context.cwd, &state)?;
                Ok(ToolOutput {
                    content: strip_html_tags(&html),
                    is_error: !status.is_success(),
                    metadata: json!({ "session_id": session_id, "path": path, "url": url, "status": status.as_u16(), "title": state.title }),
                })
            }
            "extract_text" => {
                let html = state
                    .page_html
                    .clone()
                    .ok_or_else(|| anyhow!("browser session has no active page"))?;
                Ok(ToolOutput {
                    content: strip_html_tags(&html),
                    is_error: false,
                    metadata: json!({ "session_id": session_id, "url": state.current_url }),
                })
            }
            "history" => Ok(ToolOutput {
                content: state.history.join("\n"),
                is_error: false,
                metadata: json!({ "session_id": session_id, "history": state.history }),
            }),
            "get" => Ok(ToolOutput {
                content: state.page_html.unwrap_or_default(),
                is_error: false,
                metadata: json!({ "session_id": session_id, "url": state.current_url, "title": state.title }),
            }),
            "reset" => {
                let path = browser_session_path(&context.cwd, &session_id);
                if path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("failed to remove {}", path.display()))?;
                }
                Ok(ToolOutput {
                    content: format!("cleared browser session {session_id}"),
                    is_error: false,
                    metadata: json!({ "session_id": session_id, "path": path }),
                })
            }
            other => bail!("unsupported web_browser action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "grep",
            "Search text in the workspace.",
            ToolKind::Search,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let pattern = input_string(&input, "pattern")?;
        let base = resolve_path(&context.cwd, &input_string_or(&input, "path", "."));
        let mut files = Vec::new();
        collect_files(&base, &mut files)?;

        let mut rendered = Vec::new();
        let mut matches = Vec::new();
        for path in files {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if line.contains(&pattern) {
                    let relative = path.strip_prefix(&context.cwd).unwrap_or(&path);
                    rendered.push(format!("{}:{}:{}", relative.display(), index + 1, line));
                    matches.push(json!({
                        "path": relative,
                        "line": index + 1,
                    }));
                }
            }
        }

        Ok(ToolOutput {
            content: rendered.join("\n"),
            is_error: false,
            metadata: json!({
                "pattern": pattern,
                "match_count": matches.len(),
                "matches": matches,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "glob",
            "Expand glob patterns against the workspace.",
            ToolKind::Search,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let pattern = input_string(&input, "pattern")?;
        let base = resolve_path(&context.cwd, &input_string_or(&input, "base", "."));
        let mut files = Vec::new();
        collect_files(&base, &mut files)?;

        let mut matches = Vec::new();
        for path in files {
            let relative = path.strip_prefix(&context.cwd).unwrap_or(&path);
            let normalized = normalize_for_match(relative);
            if glob_matches(&pattern, &normalized) {
                matches.push(normalized);
            }
        }
        matches.sort();

        Ok(ToolOutput {
            content: matches.join("\n"),
            is_error: false,
            metadata: json!({
                "pattern": pattern,
                "match_count": matches.len(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "todo_write",
            "Write structured todo state for the current session.",
            ToolKind::Session,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let todos = input
            .get("items")
            .cloned()
            .or_else(|| input.get("todos").cloned())
            .unwrap_or_else(|| json!([]));
        if !todos.is_array() {
            bail!("todo_write expects an array field named 'items' or 'todos'");
        }
        let path = context.cwd.join(".code-agent").join("todos.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, serde_json::to_vec_pretty(&todos)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(ToolOutput {
            content: format!("wrote {}", path.display()),
            is_error: false,
            metadata: json!({ "path": path, "count": todos.as_array().map(Vec::len).unwrap_or_default() }),
        })
    }
}

#[derive(Clone, Debug)]
struct MemoryTool;

#[async_trait]
impl Tool for MemoryTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "memory",
            "Read or update durable memory state.",
            ToolKind::Session,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = context.cwd.join(".code-agent").join("memory.json");
        let action = input_string_or(&input, "action", "read");
        match action.as_str() {
            "read" => {
                let content = match fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("failed to read {}", path.display()))
                    }
                };
                Ok(ToolOutput {
                    content,
                    is_error: false,
                    metadata: json!({ "path": path, "action": "read" }),
                })
            }
            "write" => {
                let value = input
                    .get("value")
                    .cloned()
                    .or_else(|| input.get("memory").cloned())
                    .unwrap_or_else(|| json!({}));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                let content = serde_json::to_string_pretty(&value)?;
                fs::write(&path, content.as_bytes())
                    .with_context(|| format!("failed to write {}", path.display()))?;
                Ok(ToolOutput {
                    content: format!("wrote {}", path.display()),
                    is_error: false,
                    metadata: json!({ "path": path, "action": "write" }),
                })
            }
            other => bail!("unsupported memory action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct McpTool;

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "mcp",
            "Call a registered MCP tool.",
            ToolKind::Mcp,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let tool_name = input_string(&input, "tool")?;
        let arguments = input.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let result = call_tool_from_config(&config, &tool_name, arguments).await?;

        Ok(ToolOutput {
            content: result.content_text,
            is_error: result.is_error,
            metadata: json!({
                "server": config.name,
                "tool": tool_name,
                "raw": result.raw,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct ListMcpResourcesTool;

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "list_mcp_resources",
            "List resources exposed by MCP servers.",
            ToolKind::Mcp,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let resources = list_resources_from_config(&config).await?;
        let content = resources
            .iter()
            .map(|resource| resource.uri.clone())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: serde_json::to_value(&resources)?,
        })
    }
}

#[derive(Clone, Debug)]
struct ReadMcpResourceTool;

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "read_mcp_resource",
            "Read a resource exposed by an MCP server.",
            ToolKind::Mcp,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let uri = input_string(&input, "uri")?;
        let result = read_resource_from_config(&config, &uri).await?;
        Ok(ToolOutput {
            content: result.content_text,
            is_error: false,
            metadata: json!({
                "server": config.name,
                "uri": uri,
                "raw": result.raw,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct McpAuthTool;

#[async_trait]
impl Tool for McpAuthTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "mcp_auth",
            "Authenticate or refresh MCP credentials.",
            ToolKind::Mcp,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let action = input_string_or(&input, "action", "status");
        match action.as_str() {
            "status" => {
                let cached = load_cached_auth_token(&config)?;
                let pending = load_pending_device_flow(&config)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "server": config.name,
                        "auth": config.auth,
                        "cached": cached,
                        "pending": pending,
                    }))?,
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "has_cached_token": cached.is_some(),
                        "has_pending_device_flow": pending.is_some(),
                    }),
                })
            }
            "set_token" => {
                let access_token = input_string(&input, "access_token")?;
                let cached = CachedMcpAuthToken {
                    access_token,
                    refresh_token: optional_string(&input, "refresh_token"),
                    token_type: optional_string(&input, "token_type"),
                    expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                };
                let path = store_cached_auth_token(&config, &cached)?;
                Ok(ToolOutput {
                    content: format!("stored MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({ "server": config.name, "path": path }),
                })
            }
            "login" => {
                if input.get("access_token").is_some() {
                    let access_token = input_string(&input, "access_token")?;
                    let cached = CachedMcpAuthToken {
                        access_token,
                        refresh_token: optional_string(&input, "refresh_token"),
                        token_type: optional_string(&input, "token_type"),
                        expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                    };
                    let path = store_cached_auth_token(&config, &cached)?;
                    return Ok(ToolOutput {
                        content: format!("stored MCP auth token for {}", config.name),
                        is_error: false,
                        metadata: json!({ "server": config.name, "path": path }),
                    });
                }
                match config.auth {
                    Some(McpAuthConfig::OAuthDevice { .. }) => {
                        let flow = start_oauth_device_flow(&config).await?;
                        Ok(ToolOutput {
                            content: serde_json::to_string_pretty(&flow)?,
                            is_error: false,
                            metadata: json!({
                                "server": config.name,
                                "device_code": flow.device_code,
                                "verification_uri": flow.verification_uri,
                                "verification_uri_complete": flow.verification_uri_complete,
                            }),
                        })
                    }
                    _ => bail!(
                        "mcp auth login requires an access_token unless the server uses oauth_device auth"
                    ),
                }
            }
            "poll" | "poll_device" => {
                let token = poll_oauth_device_flow(
                    &config,
                    optional_string(&input, "device_code").as_deref(),
                )
                .await?;
                Ok(ToolOutput {
                    content: format!("stored MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "access_token": token.access_token,
                        "refresh_token": token.refresh_token,
                        "token_type": token.token_type,
                        "expires_at_unix_ms": token.expires_at_unix_ms,
                    }),
                })
            }
            "refresh" => {
                if input.get("access_token").is_some() {
                    let access_token = input_string(&input, "access_token")?;
                    let cached = CachedMcpAuthToken {
                        access_token,
                        refresh_token: optional_string(&input, "refresh_token"),
                        token_type: optional_string(&input, "token_type"),
                        expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                    };
                    let path = store_cached_auth_token(&config, &cached)?;
                    return Ok(ToolOutput {
                        content: format!("stored MCP auth token for {}", config.name),
                        is_error: false,
                        metadata: json!({ "server": config.name, "path": path }),
                    });
                }
                let token = refresh_oauth_device_token(&config).await?;
                Ok(ToolOutput {
                    content: format!("refreshed MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "access_token": token.access_token,
                        "refresh_token": token.refresh_token,
                        "token_type": token.token_type,
                        "expires_at_unix_ms": token.expires_at_unix_ms,
                    }),
                })
            }
            "clear" | "logout" => {
                let cleared = clear_cached_auth_token(&config)?;
                let pending = clear_pending_device_flow(&config)?;
                Ok(ToolOutput {
                    content: if cleared {
                        format!("cleared MCP auth token for {}", config.name)
                    } else {
                        format!("no MCP auth token cached for {}", config.name)
                    },
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "cleared": cleared,
                        "cleared_pending_device_flow": pending,
                    }),
                })
            }
            other => bail!("unsupported mcp_auth action: {other}"),
        }
    }
}

fn parse_task_status(input: &str) -> Result<TaskStatus> {
    match input.trim() {
        "pending" => Ok(TaskStatus::Pending),
        "running" => Ok(TaskStatus::Running),
        "waiting_for_input" => Ok(TaskStatus::WaitingForInput),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status: {other}"),
    }
}

fn list_task_records(context: &ToolContext) -> Result<Vec<TaskRecord>> {
    task_store(&context.cwd).list_tasks()
}

#[derive(Clone, Debug)]
struct AgentTool;

#[async_trait]
impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "agent",
            "Spawn or resume an agent task.",
            ToolKind::Agent,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "spawn");
        let store = task_store(&context.cwd);
        match action.as_str() {
            "spawn" | "create" => {
                let created = create_agent_task(
                    &store,
                    AgentTaskRequest {
                        session_id: context.session_id,
                        title: input_string_or(&input, "title", "agent task"),
                        prompt: optional_string(&input, "prompt")
                            .or_else(|| optional_string(&input, "instruction")),
                        run_inline: input_bool_or(&input, "run_inline", false),
                        ..AgentTaskRequest::default()
                    },
                )?;
                Ok(ToolOutput {
                    content: format!("created agent task {}", created.id),
                    is_error: false,
                    metadata: serde_json::to_value(&created)?,
                })
            }
            "resume" | "get" => {
                let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
                let task = store
                    .get_task(task_id)?
                    .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&task)?,
                    is_error: false,
                    metadata: serde_json::to_value(&task)?,
                })
            }
            other => bail!("unsupported agent action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_create",
            "Create a background task.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let mut task = TaskRecord::new(
            input_string_or(&input, "kind", "task"),
            input_string_or(&input, "title", "task"),
        );
        task.session_id = context.session_id;
        task.input = optional_string(&input, "input").or_else(|| optional_string(&input, "prompt"));
        task.metadata = string_map_field(&input, "metadata")?;
        let created = task_store(&context.cwd).create_task(task)?;
        Ok(ToolOutput {
            content: format!("created task {}", created.id),
            is_error: false,
            metadata: serde_json::to_value(&created)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_get",
            "Inspect a single background task.",
            ToolKind::Task,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
        let task = task_store(&context.cwd)
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&task)?,
            is_error: false,
            metadata: serde_json::to_value(&task)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskListTool;

#[async_trait]
impl Tool for TaskListTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_list",
            "List background tasks.",
            ToolKind::Task,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let status = optional_string(&input, "status");
        let tasks = list_task_records(context)?
            .into_iter()
            .filter(|task| {
                status
                    .as_ref()
                    .map(|expected| format!("{:?}", task.status).eq_ignore_ascii_case(expected))
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&tasks)?,
            is_error: false,
            metadata: json!({ "count": tasks.len() }),
        })
    }
}

#[derive(Clone, Debug)]
struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_update",
            "Update task metadata or ownership.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
        let store = task_store(&context.cwd);
        let mut task = store
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        if let Some(title) = optional_string(&input, "title") {
            task.title = title;
        }
        if let Some(output) = optional_string(&input, "output") {
            task.output = Some(output);
        }
        if let Some(status) = optional_string(&input, "status") {
            task.status = parse_task_status(&status)?;
        }
        if let Some(metadata) = input.get("metadata") {
            if metadata.is_object() {
                task.metadata = string_map_field(&input, "metadata")?;
            }
        }
        let saved = store.save_task(task)?;
        Ok(ToolOutput {
            content: format!("updated task {}", saved.id),
            is_error: false,
            metadata: serde_json::to_value(&saved)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskStopTool;

#[async_trait]
impl Tool for TaskStopTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_stop",
            "Stop a running task.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
        let store = task_store(&context.cwd);
        let mut task = store
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        task.status = TaskStatus::Cancelled;
        task.output =
            Some(optional_string(&input, "reason").unwrap_or_else(|| "stopped by user".to_owned()));
        let saved = store.save_task(task)?;
        Ok(ToolOutput {
            content: format!("stopped task {}", saved.id),
            is_error: false,
            metadata: serde_json::to_value(&saved)?,
        })
    }
}

#[derive(Clone, Debug)]
struct SendMessageTool;

#[async_trait]
impl Tool for SendMessageTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "send_message",
            "Send a message to a user, teammate, or remote bridge.",
            ToolKind::Ui,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let text = input_string(&input, "message")?;
        let value = json!({
            "session_id": context.session_id,
            "target": optional_string(&input, "target").unwrap_or_else(|| "local".to_owned()),
            "message": text,
        });
        let path = runtime_dir(&context.cwd).join("messages.jsonl");
        append_jsonl(&path, &value)?;
        Ok(ToolOutput {
            content: value["message"].as_str().unwrap_or_default().to_owned(),
            is_error: false,
            metadata: json!({ "path": path, "entry": value }),
        })
    }
}

#[derive(Clone, Debug)]
struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "ask_user_question",
            "Pause execution and request additional user input.",
            ToolKind::Ui,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let mut request = QuestionRequest::new(input_string(&input, "prompt")?);
        request.task_id = optional_string(&input, "task_id")
            .map(|value| uuid::Uuid::parse_str(&value))
            .transpose()?;
        request.choices = string_list_field(&input, "choices")?;
        request.context = string_map_field(&input, "context")?;
        let store = task_store(&context.cwd);
        let stored = store.record_question(request)?;
        if let Some(task_id) = stored.task_id {
            if let Some(mut task) = store.get_task(task_id)? {
                task.status = TaskStatus::WaitingForInput;
                task.question_id = Some(stored.id);
                let _ = store.save_task(task)?;
            }
        }
        Ok(ToolOutput {
            content: format!("question recorded {}", stored.id),
            is_error: false,
            metadata: serde_json::to_value(&stored)?,
        })
    }
}

#[derive(Clone, Debug)]
struct WorkflowTool;

#[async_trait]
impl Tool for WorkflowTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "workflow",
            "Start a workflow-oriented multi-step automation.",
            ToolKind::Agent,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let store = task_store(&context.cwd);
        let workflow = create_workflow_task_set(
            &store,
            WorkflowTaskRequest {
                session_id: context.session_id,
                title: input_string_or(&input, "title", "workflow"),
                prompt: optional_string(&input, "prompt")
                    .or_else(|| optional_string(&input, "description")),
                steps: string_list_field(&input, "steps")?,
                ..WorkflowTaskRequest::default()
            },
        )?;

        Ok(ToolOutput {
            content: format!("created workflow {}", workflow.workflow.id),
            is_error: false,
            metadata: json!({
                "workflow": workflow.workflow,
                "child_task_ids": workflow.children.iter().map(|child| child.id).collect::<Vec<_>>()
            }),
        })
    }
}

pub fn compatibility_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(FileReadTool);
    registry.register(FileWriteTool);
    registry.register(FileEditTool);
    registry.register(GlobTool);
    registry.register(GrepTool);
    registry.register(BashTool);
    registry.register(PowerShellTool);
    registry.register(TerminalCaptureTool);
    registry.register(WebFetchTool);
    registry.register(WebSearchTool);
    registry.register(WebBrowserTool);
    registry.register(McpTool);
    registry.register(ListMcpResourcesTool);
    registry.register(ReadMcpResourceTool);
    registry.register(McpAuthTool);
    registry.register(AgentTool);
    registry.register(TaskCreateTool);
    registry.register(TaskGetTool);
    registry.register(TaskListTool);
    registry.register(TaskUpdateTool);
    registry.register(TaskStopTool);
    registry.register(TodoWriteTool);
    registry.register(MemoryTool);
    registry.register(SendMessageTool);
    registry.register(AskUserQuestionTool);
    registry.register(WorkflowTool);

    registry
}

#[cfg(test)]
mod tests {
    use super::{
        compatibility_tool_registry, compatibility_tool_specs, glob_matches, ToolCallRequest,
        ToolContext, ToolKind,
    };
    use serde_json::json;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("code-agent-tools-{label}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exposes_expected_compatibility_tools() {
        let specs = compatibility_tool_specs();
        let names = specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"mcp"));
        assert!(names.contains(&"agent"));
        assert!(specs.iter().any(|spec| spec.kind == ToolKind::Task));
    }

    #[test]
    fn matches_basic_globs() {
        assert!(glob_matches("src/**/*.rs", "src/cli/main.rs"));
        assert!(glob_matches("*.md", "README.md"));
        assert!(!glob_matches("src/*.rs", "src/cli/main.rs"));
    }

    #[tokio::test]
    async fn reads_and_writes_files_via_registry() {
        let cwd = make_temp_dir("registry");
        let registry = compatibility_tool_registry();
        let context = ToolContext {
            cwd: cwd.clone(),
            ..ToolContext::default()
        };

        registry
            .invoke(
                ToolCallRequest {
                    tool_name: "file_write".to_owned(),
                    input: json!({
                        "path": "notes/example.txt",
                        "content": "hello from rust"
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        let read = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "file_read".to_owned(),
                    input: json!({ "path": "notes/example.txt" }),
                },
                &context,
            )
            .await
            .unwrap();

        assert_eq!(read.content, "hello from rust");
    }

    #[tokio::test]
    async fn edits_and_reads_memory_via_registry() {
        let cwd = make_temp_dir("memory");
        let registry = compatibility_tool_registry();
        let context = ToolContext {
            cwd: cwd.clone(),
            ..ToolContext::default()
        };

        registry
            .invoke(
                ToolCallRequest {
                    tool_name: "file_write".to_owned(),
                    input: json!({
                        "path": "notes/example.txt",
                        "content": "alpha beta gamma"
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        registry
            .invoke(
                ToolCallRequest {
                    tool_name: "file_edit".to_owned(),
                    input: json!({
                        "path": "notes/example.txt",
                        "old_string": "beta",
                        "new_string": "delta"
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        let edited = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "file_read".to_owned(),
                    input: json!({ "path": "notes/example.txt" }),
                },
                &context,
            )
            .await
            .unwrap();
        assert_eq!(edited.content, "alpha delta gamma");

        registry
            .invoke(
                ToolCallRequest {
                    tool_name: "memory".to_owned(),
                    input: json!({
                        "action": "write",
                        "value": { "summary": "remember this" }
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        let memory = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "memory".to_owned(),
                    input: json!({ "action": "read" }),
                },
                &context,
            )
            .await
            .unwrap();
        assert!(memory.content.contains("remember this"));
    }

    #[tokio::test]
    async fn fetches_local_http_content() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let response = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Type: text/plain\r\n",
                    "Content-Length: 11\r\n",
                    "\r\n",
                    "hello fetch"
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let registry = compatibility_tool_registry();
        let output = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "web_fetch".to_owned(),
                    input: json!({ "url": format!("http://{address}") }),
                },
                &ToolContext::default(),
            )
            .await
            .unwrap();

        assert_eq!(output.content, "hello fetch");
        assert_eq!(output.metadata["status"], 200);
    }

    #[tokio::test]
    async fn invokes_live_mcp_tools_from_plugin_manifest() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
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
                let header_text = String::from_utf8_lossy(&request[..header_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("Content-Length")
                            .then_some(value.trim())
                    })
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                while request.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let body = serde_json::from_slice::<serde_json::Value>(
                    &request[header_end..header_end + content_length],
                )
                .unwrap();
                let response = match body["method"].as_str().unwrap() {
                    "tools/call" => json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "result": {
                            "content": [{ "type": "text", "text": "mcp tool result" }],
                            "isError": false
                        }
                    }),
                    "resources/read" => json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "result": {
                            "contents": [{ "uri": "memory://note", "text": "note body" }]
                        }
                    }),
                    other => panic!("unexpected method: {other}"),
                };
                let response_body = response.to_string();
                let response_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response_text.as_bytes()).unwrap();
            }
        });

        let cwd = make_temp_dir("mcp");
        fs::create_dir_all(cwd.join(".claude-plugin")).unwrap();
        fs::write(
            cwd.join(".claude-plugin/plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "name": "demo-plugin",
                "mcpServers": {
                    "demo": {
                        "url": format!("http://{address}")
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let registry = compatibility_tool_registry();
        let context = ToolContext {
            cwd: cwd.clone(),
            ..ToolContext::default()
        };

        let tool_result = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "mcp".to_owned(),
                    input: json!({
                        "server": "demo",
                        "tool": "echo",
                        "arguments": { "value": "hi" }
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        let resource_result = registry
            .invoke(
                ToolCallRequest {
                    tool_name: "read_mcp_resource".to_owned(),
                    input: json!({
                        "server": "demo",
                        "uri": "memory://note"
                    }),
                },
                &context,
            )
            .await
            .unwrap();

        assert_eq!(tool_result.content, "mcp tool result");
        assert_eq!(resource_result.content, "note body");
    }
}
