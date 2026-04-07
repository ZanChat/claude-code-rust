#[derive(Clone, Debug)]
struct FileReadTool;

#[derive(Clone, Debug)]
struct ReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_read".to_owned(),
            description: "Read files from the active workspace.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileReadToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let FileReadToolInput { path } = file_read_input(input)?;
        let path = resolve_path(&context.cwd, &path);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: json!({ "path": path }),
        })
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Read".to_owned(),
            description: "Read files from the active workspace.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileReadToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("file_read", input, context).await
    }
}

#[derive(Clone, Debug)]
struct FileWriteTool;

#[derive(Clone, Debug)]
struct WriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_write".to_owned(),
            description: "Write or replace workspace files.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileWriteToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let FileWriteToolInput { path, content } = parse_tool_input(input)?;
        let path = resolve_path(&context.cwd, &path);
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

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Write".to_owned(),
            description: "Write or replace workspace files.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileWriteToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("file_write", input, context).await
    }
}

#[derive(Clone, Debug)]
struct FileEditTool;

#[derive(Clone, Debug)]
struct EditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_edit".to_owned(),
            description: "Apply targeted edits to an existing file.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileEditToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let FileEditToolInput {
            path,
            old_string,
            new_string,
            replace_all,
        } = parse_tool_input(input)?;
        let path = resolve_path(&context.cwd, &path);
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

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Edit".to_owned(),
            description: "Apply targeted edits to an existing file.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(FileEditToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("file_edit", input, context).await
    }
}

const SHELL_OUTPUT_CAPTURE_LIMIT: usize = 128 * 1024;
const SHELL_OUTPUT_TRUNCATED_MARKER: &str = "\n\n[output truncated]";

#[derive(Clone, Debug)]
struct ShellCommandOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    truncated: bool,
}

async fn read_command_stream<R>(mut stream: R) -> Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }

        let remaining = SHELL_OUTPUT_CAPTURE_LIMIT.saturating_sub(buffer.len());
        if remaining > 0 {
            let keep = read.min(remaining);
            buffer.extend_from_slice(&chunk[..keep]);
        }
        if read > remaining {
            truncated = true;
        }
    }

    Ok((buffer, truncated))
}

fn combined_shell_output(stdout: &str, stderr: &str, truncated: bool) -> String {
    let mut content = if stderr.is_empty() {
        stdout.to_owned()
    } else if stdout.is_empty() {
        stderr.to_owned()
    } else {
        format!("{stdout}\n{stderr}")
    };

    if truncated {
        if !content.is_empty() {
            content.push_str(SHELL_OUTPUT_TRUNCATED_MARKER);
        } else {
            content = SHELL_OUTPUT_TRUNCATED_MARKER.trim_start().to_owned();
        }
    }

    content
}

async fn run_shell_command(
    shell: &str,
    shell_args: &[&str],
    command: &str,
    context: &ToolContext,
) -> Result<ShellCommandOutput> {
    let mut child = Command::new(shell);
    child
        .kill_on_drop(true)
        .args(shell_args)
        .arg(command)
        .current_dir(&context.cwd)
        .envs(&context.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child
        .spawn()
        .with_context(|| format!("failed to execute {shell} command: {command}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for {shell} command"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for {shell} command"))?;

    let stdout_task = tokio::spawn(async move { read_command_stream(stdout).await });
    let stderr_task = tokio::spawn(async move { read_command_stream(stderr).await });
    let status = child.wait().await?;
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| anyhow!("failed to join stdout capture task: {error}"))??;
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| anyhow!("failed to join stderr capture task: {error}"))??;

    Ok(ShellCommandOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code(),
        truncated: stdout_truncated || stderr_truncated,
    })
}

#[derive(Clone, Debug)]
struct BashTool;

#[derive(Clone, Debug)]
struct BashCompatTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: "Execute a shell command in the project.".to_owned(),
            kind: ToolKind::Shell,
            input_schema: schemars::schema_for!(ShellCommandToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let command = shell_command_input(&input)?;
        let output = run_shell_command("bash", &["-lc"], &command, context).await?;
        Ok(ToolOutput {
            content: combined_shell_output(&output.stdout, &output.stderr, output.truncated),
            is_error: output.exit_code.unwrap_or(1) != 0,
            metadata: json!({
                "command": command,
                "exit_code": output.exit_code,
                "truncated_output": output.truncated,
            }),
        })
    }
}

#[async_trait]
impl Tool for BashCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Bash".to_owned(),
            description: "Execute a shell command in the project.".to_owned(),
            kind: ToolKind::Shell,
            input_schema: schemars::schema_for!(ShellCommandToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("bash", input, context).await
    }
}

#[derive(Clone, Debug)]
struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "powershell".to_owned(),
            description: "Execute a PowerShell command when the runtime requires it.".to_owned(),
            kind: ToolKind::Shell,
            input_schema: schemars::schema_for!(ShellCommandToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let command = shell_command_input(&input)?;
        let output = run_shell_command("pwsh", &["-NoLogo", "-NoProfile", "-Command"], &command, context).await?;
        Ok(ToolOutput {
            content: combined_shell_output(&output.stdout, &output.stderr, output.truncated),
            is_error: output.exit_code.unwrap_or(1) != 0,
            metadata: json!({
                "command": command,
                "exit_code": output.exit_code,
                "truncated_output": output.truncated,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct TerminalCaptureTool;

#[async_trait]
impl Tool for TerminalCaptureTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_capture".to_owned(),
            description: "Capture and resume terminal output streams.".to_owned(),
            kind: ToolKind::Shell,
            input_schema: schemars::schema_for!(TerminalCaptureToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "start");
        let dir = runtime_dir(&context.cwd).join("terminal-captures");
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        match action.as_str() {
            "start" => {
                let command = shell_command_input(&input)?;
                let id = optional_string(&input, "id")
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let shell = input_string_or(&input, "shell", "bash");
                let output = run_shell_command(&shell, &["-lc"], &command, context)
                    .await
                    .with_context(|| {
                        format!("failed to execute capture command with {shell}: {command}")
                    })?;
                let record = json!({
                    "id": id,
                    "shell": shell,
                    "command": command,
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "exit_code": output.exit_code,
                    "truncated_output": output.truncated,
                });
                let path = dir.join(format!(
                    "{}.json",
                    record["id"].as_str().unwrap_or("capture")
                ));
                fs::write(&path, serde_json::to_vec_pretty(&record)?)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                let content = combined_shell_output(
                    record["stdout"].as_str().unwrap_or_default(),
                    record["stderr"].as_str().unwrap_or_default(),
                    record["truncated_output"].as_bool().unwrap_or(false),
                );
                Ok(ToolOutput {
                    content,
                    is_error: output.exit_code.unwrap_or(1) != 0,
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
                    content: combined_shell_output(
                        value["stdout"].as_str().unwrap_or_default(),
                        value["stderr"].as_str().unwrap_or_default(),
                        value["truncated_output"].as_bool().unwrap_or(false),
                    ),
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
struct NotebookEditTool;

fn notebook_source_value(source: &str) -> Value {
    Value::Array(
        source
            .split_inclusive('\n')
            .map(|line| Value::String(line.to_owned()))
            .collect(),
    )
}

fn notebook_cell_index(cells: &[Value], cell_id: &str) -> Option<usize> {
    cells.iter().position(|cell| {
        cell.get("id")
            .and_then(Value::as_str)
            .map(|id| id == cell_id)
            .unwrap_or(false)
    })
}

fn notebook_cell(
    cell_id: String,
    cell_type: &str,
    new_source: &str,
) -> Result<Value> {
    match cell_type {
        "code" => Ok(json!({
            "cell_type": "code",
            "execution_count": Value::Null,
            "id": cell_id,
            "metadata": {},
            "outputs": [],
            "source": notebook_source_value(new_source),
        })),
        "markdown" => Ok(json!({
            "cell_type": "markdown",
            "id": cell_id,
            "metadata": {},
            "source": notebook_source_value(new_source),
        })),
        other => bail!("unsupported notebook cell_type: {other}"),
    }
}

#[async_trait]
impl Tool for NotebookEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "NotebookEdit".to_owned(),
            description: "Edit Jupyter notebook cells in a .ipynb file.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(NotebookEditCompatInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let NotebookEditCompatInput {
            notebook_path,
            cell_id,
            new_source,
            cell_type,
            edit_mode,
        } = parse_tool_input(input)?;
        let path = resolve_path(&context.cwd, &notebook_path);
        if path.extension().and_then(|ext| ext.to_str()) != Some("ipynb") {
            bail!("NotebookEdit expects a .ipynb file: {}", path.display());
        }

        let raw =
            fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let mut notebook: Value =
            serde_json::from_str(&raw).with_context(|| format!("failed to decode {}", path.display()))?;
        let cells = notebook
            .get_mut("cells")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("notebook is missing a cells array"))?;
        let mode = edit_mode.unwrap_or_else(|| "replace".to_owned());

        let (resolved_cell_id, message) = match mode.as_str() {
            "replace" => {
                let cell_id = cell_id.ok_or_else(|| anyhow!("replace mode requires cell_id"))?;
                let new_source =
                    new_source.ok_or_else(|| anyhow!("replace mode requires new_source"))?;
                let Some(index) = notebook_cell_index(cells, &cell_id) else {
                    bail!("notebook cell not found: {cell_id}");
                };
                let cell = cells
                    .get_mut(index)
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| anyhow!("notebook cell is malformed"))?;
                cell.insert("source".to_owned(), notebook_source_value(&new_source));
                if let Some(cell_type) = cell_type {
                    cell.insert("cell_type".to_owned(), Value::String(cell_type));
                }
                (
                    cell_id.clone(),
                    format!("updated notebook cell {cell_id} in {}", path.display()),
                )
            }
            "insert" => {
                let new_source =
                    new_source.ok_or_else(|| anyhow!("insert mode requires new_source"))?;
                let cell_type =
                    cell_type.unwrap_or_else(|| "code".to_owned());
                let anchor_cell_id = cell_id.clone();
                let new_cell_id = uuid::Uuid::new_v4().to_string();
                let insert_at = cell_id
                    .as_deref()
                    .and_then(|id| notebook_cell_index(cells, id).map(|index| index + 1))
                    .unwrap_or(0);
                cells.insert(insert_at, notebook_cell(new_cell_id.clone(), &cell_type, &new_source)?);
                (
                    new_cell_id.clone(),
                    match anchor_cell_id {
                        Some(anchor) => format!(
                            "inserted notebook cell {new_cell_id} after {anchor} in {}",
                            path.display()
                        ),
                        None => {
                            format!("inserted notebook cell {new_cell_id} in {}", path.display())
                        }
                    },
                )
            }
            "delete" => {
                let cell_id = cell_id.ok_or_else(|| anyhow!("delete mode requires cell_id"))?;
                let Some(index) = notebook_cell_index(cells, &cell_id) else {
                    bail!("notebook cell not found: {cell_id}");
                };
                cells.remove(index);
                (
                    cell_id.clone(),
                    format!("deleted notebook cell {cell_id} from {}", path.display()),
                )
            }
            other => bail!("unsupported notebook edit_mode: {other}"),
        };

        fs::write(&path, serde_json::to_vec_pretty(&notebook)?)
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(ToolOutput {
            content: message,
            is_error: false,
            metadata: json!({
                "path": path,
                "cell_id": resolved_cell_id,
                "edit_mode": mode,
            }),
        })
    }
}
