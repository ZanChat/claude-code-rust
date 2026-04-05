#[derive(Clone, Debug)]
struct FileReadTool;

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

#[derive(Clone, Debug)]
struct FileWriteTool;

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

#[derive(Clone, Debug)]
struct FileEditTool;

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

#[derive(Clone, Debug)]
struct BashTool;

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
        let output = Command::new("bash")
            .kill_on_drop(true)
            .arg("-lc")
            .arg(&command)
            .current_dir(&context.cwd)
            .envs(&context.environment)
            .output()
            .await
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
        let output = Command::new("pwsh")
            .kill_on_drop(true)
            .arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(&command)
            .current_dir(&context.cwd)
            .envs(&context.environment)
            .output()
            .await
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
                let output = Command::new(&shell)
                    .kill_on_drop(true)
                    .arg("-lc")
                    .arg(&command)
                    .current_dir(&context.cwd)
                    .envs(&context.environment)
                    .output()
                    .await
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
