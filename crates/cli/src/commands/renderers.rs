pub(crate) async fn render_permissions_command(cwd: &Path) -> Result<String> {
    let task_store = task_store_for(cwd);
    let pending = task_store
        .list_tasks()?
        .into_iter()
        .filter(|task| task.status == TaskStatus::WaitingForInput)
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&json!({
        "mode": "ask",
        "pending_requests": pending,
    }))?)
}

pub(crate) async fn render_session_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<String> {
    let transcript_path = store.transcript_path(session_id).await?;
    let messages = store.load_session(session_id).await.unwrap_or_default();
    let runtime_messages = materialize_runtime_messages(&messages);
    let first_prompt = runtime_messages
        .iter()
        .find_map(|message| (message.role == MessageRole::User).then(|| message_text(message)));
    let report = SessionCommandReport {
        session_id,
        session_root: store.root_dir().to_path_buf(),
        transcript_path,
        message_count: messages.len(),
        runtime_message_count: runtime_messages.len(),
        first_prompt,
        last_message_preview: session_preview(&runtime_messages),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}

pub(crate) fn render_status_command(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    live_runtime: bool,
    cwd: &Path,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "provider": provider,
        "model": active_model,
        "session_id": session_id,
        "runtime": if live_runtime { "live" } else { "offline" },
        "task_count": task_store_for(cwd).list_tasks()?.len(),
        "question_count": task_store_for(cwd).list_questions()?.len(),
    }))?)
}

pub(crate) fn render_statusline_command(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "statusline": repl_status(provider, active_model, session_id),
    }))?)
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdeLockfileContent {
    workspace_folders: Option<Vec<String>>,
    ide_name: Option<String>,
    transport: Option<String>,
}

#[derive(Debug)]
struct IdeLockfileInfo {
    workspace_folders: Vec<String>,
    port: u16,
    ide_name: Option<String>,
    use_websocket: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DetectedIdeCandidate {
    pub(crate) name: String,
    pub(crate) port: u16,
    pub(crate) url: String,
    pub(crate) suggested_bridge: String,
    pub(crate) workspace_folders: Vec<String>,
}

fn ide_lockfiles_dir(home_override: Option<&Path>) -> Option<PathBuf> {
    let home = home_override
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
    Some(home.join(".claude/ide"))
}

fn sorted_ide_lockfiles(home_override: Option<&Path>) -> Vec<PathBuf> {
    let Some(lockfiles_dir) = ide_lockfiles_dir(home_override) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(lockfiles_dir) else {
        return Vec::new();
    };

    let mut paths = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("lock")).then_some(path)
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        let left_modified = left
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let right_modified = right
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        right_modified
            .cmp(&left_modified)
            .then_with(|| left.cmp(right))
    });
    paths
}

fn read_ide_lockfile(path: &Path) -> Option<IdeLockfileInfo> {
    let content = fs::read_to_string(path).ok()?;
    let port = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse::<u16>().ok())?;

    if let Ok(parsed) = serde_json::from_str::<IdeLockfileContent>(&content) {
        return Some(IdeLockfileInfo {
            workspace_folders: parsed.workspace_folders.unwrap_or_default(),
            port,
            ide_name: parsed.ide_name,
            use_websocket: parsed.transport.as_deref() == Some("ws"),
        });
    }

    Some(IdeLockfileInfo {
        workspace_folders: content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        port,
        ide_name: None,
        use_websocket: false,
    })
}

fn workspace_matches_ide(cwd: &Path, workspace_folder: &str) -> bool {
    let workspace_path = PathBuf::from(workspace_folder);
    let resolved_workspace = fs::canonicalize(&workspace_path).unwrap_or(workspace_path);
    cwd == resolved_workspace || cwd.starts_with(&resolved_workspace)
}

pub(crate) fn detect_workspace_ides(
    cwd: &Path,
    home_override: Option<&Path>,
) -> Vec<DetectedIdeCandidate> {
    let resolved_cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    sorted_ide_lockfiles(home_override)
        .into_iter()
        .filter_map(|path| read_ide_lockfile(&path))
        .filter(|lockfile| {
            lockfile
                .workspace_folders
                .iter()
                .any(|folder| workspace_matches_ide(&resolved_cwd, folder))
        })
        .map(|lockfile| {
            let url_scheme = if lockfile.use_websocket { "ws" } else { "http" };
            DetectedIdeCandidate {
                name: lockfile.ide_name.unwrap_or_else(|| "IDE".to_owned()),
                port: lockfile.port,
                url: format!("{url_scheme}://127.0.0.1:{}", lockfile.port),
                suggested_bridge: format!("ide://127.0.0.1:{}", lockfile.port),
                workspace_folders: lockfile.workspace_folders,
            }
        })
        .collect()
}

pub(crate) fn render_ide_command_with_home(
    cwd: &Path,
    ide_bridge_active: bool,
    ide_address: Option<&str>,
    home_override: Option<&Path>,
) -> Result<String> {
    let detected = detect_workspace_ides(cwd, home_override);
    let (status, message) = if ide_bridge_active {
        (
            "connected",
            "IDE bridge is active for this session.".to_owned(),
        )
    } else if let Some(candidate) = detected.first() {
        (
            "available",
            format!(
                "Detected {} for this workspace. Connect with {}.",
                candidate.name, candidate.suggested_bridge
            ),
        )
    } else {
        (
            "not_connected",
            "No IDE bridge detected for this workspace. Start a supported IDE with the Claude extension, or connect explicitly with --bridge-connect ide://HOST[:PORT] or --bridge-server ide://HOST[:PORT].".to_owned(),
        )
    };

    Ok(serde_json::to_string_pretty(&json!({
        "connected": ide_bridge_active,
        "bridge_address": ide_address,
        "status": status,
        "workspace": cwd.display().to_string(),
        "message": message,
        "detected": detected,
    }))?)
}

pub(crate) fn render_ide_command(
    cwd: &Path,
    ide_bridge_active: bool,
    ide_address: Option<&str>,
) -> Result<String> {
    render_ide_command_with_home(cwd, ide_bridge_active, ide_address, None)
}

pub(crate) fn render_theme_command() -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "status": "compatible",
        "message": "Theme selection is currently terminal-native in the Rust UI.",
    }))?)
}

pub(crate) fn render_vim_command(enabled: bool) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "enabled": enabled,
        "status": if enabled { "experimental" } else { "disabled" },
        "message": "Full vim state-machine parity is still in progress.",
    }))?)
}

pub(crate) fn render_plan_command() -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "status": "compatibility_surface_only",
        "message": "Plan-mode workflow is tracked outside the Rust runtime core.",
    }))?)
}

pub(crate) fn render_simple_compat_command(name: &str, message: &str) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "command": name,
        "status": "compatibility_surface_only",
        "message": message,
    }))?)
}

pub(crate) fn render_files_command(raw_messages: &[Message], cwd: &Path) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_file_message(&runtime_messages, cwd).unwrap_or(PanePreview {
        title: "File preview".to_owned(),
        lines: vec!["No file preview available yet.".to_owned()],
    });
    Ok(serde_json::to_string_pretty(&preview)?)
}

pub(crate) fn render_diff_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_diff_message(&runtime_messages).unwrap_or(PanePreview {
        title: "Diff preview".to_owned(),
        lines: vec!["No diff preview available yet.".to_owned()],
    });
    Ok(serde_json::to_string_pretty(&preview)?)
}

pub(crate) fn render_usage_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let total = runtime_messages
        .iter()
        .filter_map(|message| message.metadata.usage.as_ref())
        .fold((0u64, 0u64), |(input, output), usage| {
            (input + usage.input_tokens, output + usage.output_tokens)
        });
    Ok(serde_json::to_string_pretty(&json!({
        "input_tokens": total.0,
        "output_tokens": total.1,
        "message_count": runtime_messages.len(),
    }))?)
}

pub(crate) fn render_export_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "session_id": session_id,
        "transcript_path": store.root_dir().join(format!("{session_id}.jsonl")),
        "status": "ready",
    }))?)
}

pub(crate) fn render_tasks_command(invocation: &CommandInvocation, cwd: &Path) -> Result<String> {
    let store = task_store_for(cwd);
    match invocation.args.first().map(String::as_str) {
        Some("create") => {
            let assignments = parse_assignment_args(&invocation.args[1..]);
            let mut task = TaskRecord::new(
                assignments
                    .get("kind")
                    .cloned()
                    .unwrap_or_else(|| "task".to_owned()),
                assignments.get("title").cloned().unwrap_or_else(|| {
                    invocation
                        .args
                        .iter()
                        .skip(1)
                        .filter(|arg| !arg.contains('='))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                }),
            );
            if task.title.trim().is_empty() {
                task.title = "task".to_owned();
            }
            task.input = assignments.get("input").cloned();
            if let Some(status) = assignments.get("status") {
                task.status = parse_task_status(status)?;
            }
            if let Some(session_id) = assignments
                .get("session_id")
                .map(|value| parse_task_id(value))
                .transpose()?
            {
                task.session_id = Some(session_id);
            }
            let created = store.create_task(task)?;
            Ok(serde_json::to_string_pretty(&created)?)
        }
        Some("get") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks get requires a task id"))?,
            )?;
            let task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            Ok(serde_json::to_string_pretty(&task)?)
        }
        Some("update") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks update requires a task id"))?,
            )?;
            let mut task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            let assignments = parse_assignment_args(&invocation.args[2..]);
            if let Some(title) = assignments.get("title") {
                task.title = title.clone();
            }
            if let Some(kind) = assignments.get("kind") {
                task.kind = kind.clone();
            }
            if let Some(output) = assignments.get("output") {
                task.output = Some(output.clone());
            }
            if let Some(status) = assignments.get("status") {
                task.status = parse_task_status(status)?;
            }
            let saved = store.save_task(task)?;
            Ok(serde_json::to_string_pretty(&saved)?)
        }
        Some("stop") => {
            let task_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks stop requires a task id"))?,
            )?;
            let mut task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            task.status = TaskStatus::Cancelled;
            task.output = Some("stopped from slash command".to_owned());
            Ok(serde_json::to_string_pretty(&store.save_task(task)?)?)
        }
        Some("questions") => {
            let questions = store.list_questions()?;
            Ok(serde_json::to_string_pretty(&QuestionCommandReport {
                count: questions.len(),
                questions,
            })?)
        }
        Some("responses") => {
            let responses = store.list_responses()?;
            Ok(serde_json::to_string_pretty(&ResponseCommandReport {
                count: responses.len(),
                responses,
            })?)
        }
        Some("answer") => {
            let question_id = parse_task_id(
                invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("tasks answer requires a question id"))?,
            )?;
            let answer = invocation
                .args
                .iter()
                .skip(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let response = store.answer_question(QuestionResponse::new(question_id, answer))?;
            for mut task in store
                .list_tasks()?
                .into_iter()
                .filter(|task| task.question_id == Some(question_id))
            {
                task.status = TaskStatus::Running;
                let _ = store.save_task(task)?;
            }
            Ok(serde_json::to_string_pretty(&response)?)
        }
        _ => {
            let tasks = store.list_tasks()?;
            Ok(serde_json::to_string_pretty(&TaskCommandReport {
                count: tasks.len(),
                tasks,
            })?)
        }
    }
}

pub(crate) async fn render_agents_command(
    invocation: &CommandInvocation,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
    session_id: SessionId,
) -> Result<String> {
    match invocation.args.first().map(String::as_str) {
        Some("create" | "spawn") => {
            let title = invocation
                .args
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "agent".to_owned(),
                        input: json!({
                            "action": "spawn",
                            "title": if title.trim().is_empty() { "agent task" } else { title.as_str() },
                        }),
                    },
                    &ToolContext {
                        session_id: Some(session_id),
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report.metadata)?)
        }
        Some("get" | "resume") => {
            let task_id = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("agents get requires a task id"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "agent".to_owned(),
                        input: json!({
                            "action": "resume",
                            "task_id": task_id,
                        }),
                    },
                    &ToolContext {
                        session_id: Some(session_id),
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        _ => {
            let tasks = task_store_for(cwd)
                .list_tasks()?
                .into_iter()
                .filter(|task| {
                    matches!(
                        task.kind.as_str(),
                        "agent"
                            | "workflow"
                            | "workflow_step"
                            | "coordinator"
                            | "assistant_worker"
                            | "assistant_synthesis"
                    )
                })
                .collect::<Vec<_>>();
            Ok(serde_json::to_string_pretty(&TaskCommandReport {
                count: tasks.len(),
                tasks,
            })?)
        }
    }
}

pub(crate) async fn render_plugin_command(
    invocation: &CommandInvocation,
    plugin_root: Option<&PathBuf>,
    cwd: &Path,
) -> Result<String> {
    let root_arg = match invocation.args.first().map(String::as_str) {
        Some("bridge-start" | "bridge-stop" | "bridge-status") => {
            invocation.args.get(1).map(String::as_str)
        }
        other => other,
    };
    let root = resolve_plugin_root_with_override(plugin_root, root_arg, cwd);
    let runtime = OutOfProcessPluginRuntime;
    match invocation.args.first().map(String::as_str) {
        Some("bridge-start") => {
            let executable = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("plugin bridge-start requires an executable path"))?;
            let args = invocation
                .args
                .iter()
                .skip(if root_arg.is_some() { 3 } else { 2 })
                .cloned()
                .collect::<Vec<_>>();
            Ok(serde_json::to_string_pretty(
                &runtime
                    .start_bridge(BridgeLaunchRequest {
                        plugin_root: root,
                        executable: Some(executable),
                        args,
                        component: Some("runtime".to_owned()),
                        ..BridgeLaunchRequest::default()
                    })
                    .await?,
            )?)
        }
        Some("bridge-stop") => Ok(serde_json::to_string_pretty(
            &runtime.stop_bridge(&root, Some("runtime")).await?,
        )?),
        Some("bridge-status") => Ok(serde_json::to_string_pretty(
            &runtime.bridge_status(&root, Some("runtime")).await?,
        )?),
        _ => Ok(serde_json::to_string_pretty(
            &load_plugin_report(root).await?,
        )?),
    }
}

pub(crate) async fn render_mcp_command(
    invocation: &CommandInvocation,
    plugin_root: Option<&PathBuf>,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
) -> Result<String> {
    let root_arg = match invocation.args.first().map(String::as_str) {
        Some(
            "auth-status" | "auth-set" | "auth-login" | "auth-poll" | "auth-refresh" | "auth-clear",
        ) => invocation.args.get(1).map(String::as_str),
        other => other,
    };
    let root = resolve_plugin_root_with_override(plugin_root, root_arg, cwd);
    let runtime = OutOfProcessPluginRuntime;
    let plugin = runtime.load_manifest(&root).await?;
    let parsed = parse_mcp_server_configs(&plugin.manifest.mcp_servers);
    match invocation.args.first().map(String::as_str) {
        Some("auth-status") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-status requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "status"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        Some("auth-login") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-login requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "login"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(report.content)
        }
        Some("auth-set") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-set requires a server name"))?;
            let token = invocation
                .args
                .get(if root_arg.is_some() { 3 } else { 2 })
                .ok_or_else(|| anyhow!("mcp auth-set requires an access token"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "set_token",
                            "access_token": token
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-poll") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-poll requires a server name"))?;
            let device_code = invocation
                .args
                .get(if root_arg.is_some() { 3 } else { 2 })
                .cloned();
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "poll",
                            "device_code": device_code,
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-refresh") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-refresh requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "refresh"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        Some("auth-clear") => {
            let server = invocation
                .args
                .get(if root_arg.is_some() { 2 } else { 1 })
                .ok_or_else(|| anyhow!("mcp auth-clear requires a server name"))?;
            let report = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "mcp_auth".to_owned(),
                        input: json!({
                            "plugin_root": root,
                            "server": server,
                            "action": "clear"
                        }),
                    },
                    &ToolContext {
                        cwd: cwd.to_path_buf(),
                        provider: Some(provider.to_string()),
                        model,
                        ..ToolContext::default()
                    },
                )
                .await?;
            Ok(serde_json::to_string_pretty(&report)?)
        }
        _ => Ok(serde_json::to_string_pretty(&parsed)?),
    }
}

pub(crate) async fn render_remote_control_command(
    registry: &CommandRegistry,
    invocation: &CommandInvocation,
    cli: &Cli,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &[Message],
    live_runtime: bool,
) -> Result<String> {
    match invocation.args.first().map(String::as_str) {
        Some("connect") => {
            if !command_allowed_for_bridge(registry, "remote-control") {
                return Ok(serde_json::to_string_pretty(&json!({
                    "status": "blocked",
                    "reason": "remote-control is not bridge-safe in the current registry",
                }))?);
            }
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control connect requires an address"))?;
            let receive_count = invocation
                .args
                .get(2)
                .and_then(|value| value.parse::<usize>().ok())
                .or(cli.bridge_receive_count)
                .unwrap_or(4);
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                build_remote_outbound(cli, session_id, None, cli.resume.as_deref())?,
                receive_count,
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("send") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control send requires an address"))?;
            let prompt_text = invocation
                .args
                .iter()
                .skip(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if prompt_text.trim().is_empty() {
                bail!("remote-control send requires a message");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                build_remote_outbound(cli, session_id, Some(prompt_text), cli.resume.as_deref())?,
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("resume") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control resume requires an address"))?;
            let target = invocation
                .args
                .get(2)
                .ok_or_else(|| anyhow!("remote-control resume requires a session target"))?;
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::ResumeSession {
                    request: ResumeSessionRequest {
                        target: target.clone(),
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("directive") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control directive requires an address"))?;
            let mut agent_id = None;
            let mut instruction_parts = Vec::new();
            for arg in invocation.args.iter().skip(2) {
                if agent_id.is_none() {
                    if let Some(value) = arg.strip_prefix("agent=") {
                        agent_id = Some(value.to_owned());
                        continue;
                    }
                }
                instruction_parts.push(arg.clone());
            }
            let instruction = instruction_parts.join(" ");
            if instruction.trim().is_empty() {
                bail!("remote-control directive requires an instruction");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::AssistantDirective {
                    directive: AssistantDirective {
                        agent_id,
                        instruction,
                        ..AssistantDirective::default()
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("answer") => {
            let address = invocation
                .args
                .get(1)
                .ok_or_else(|| anyhow!("remote-control answer requires an address"))?;
            let question_id = parse_task_id(
                invocation
                    .args
                    .get(2)
                    .ok_or_else(|| anyhow!("remote-control answer requires a question id"))?,
            )?;
            let answer = invocation
                .args
                .iter()
                .skip(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if answer.trim().is_empty() {
                bail!("remote-control answer requires a response");
            }
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::QuestionResponse {
                    response: QuestionResponse::new(question_id, answer),
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("approve" | "deny") => {
            let approved = matches!(invocation.args.first().map(String::as_str), Some("approve"));
            let address = invocation.args.get(1).ok_or_else(|| {
                anyhow!(
                    "remote-control {} requires an address",
                    if approved { "approve" } else { "deny" }
                )
            })?;
            let permission_id = invocation.args.get(2).ok_or_else(|| {
                anyhow!(
                    "remote-control {} requires a permission id",
                    if approved { "approve" } else { "deny" }
                )
            })?;
            let note = invocation
                .args
                .iter()
                .skip(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let inbound = exchange_remote_envelopes(
                address,
                session_id,
                vec![RemoteEnvelope::PermissionResponse {
                    response: code_agent_bridge::RemotePermissionResponse {
                        id: permission_id.clone(),
                        approved,
                        note: (!note.trim().is_empty()).then_some(note),
                    },
                }],
                cli.bridge_receive_count.unwrap_or(4),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&inbound)?)
        }
        Some("serve") => {
            let bind_address = invocation
                .args
                .get(1)
                .cloned()
                .or_else(|| cli.bridge_server.clone())
                .ok_or_else(|| anyhow!("remote-control serve requires a bind address"))?;
            let mode = remote_mode_for_address(&bind_address);
            let handler = LocalBridgeHandler {
                store,
                tool_registry,
                cwd: cwd.to_path_buf(),
                provider,
                active_model: active_model.to_owned(),
                session_id,
                raw_messages: raw_messages.to_vec(),
                live_runtime,
                allow_remote_tools: true,
                pending_permission: None,
                voice_streams: BTreeMap::new(),
            };
            let config = BridgeServerConfig {
                bind_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            };
            let record = match mode {
                RemoteMode::DirectConnect | RemoteMode::IdeBridge => {
                    serve_direct_session(config, handler).await?
                }
                _ => serve_bridge_session(config, handler).await?,
            };
            Ok(serde_json::to_string_pretty(&record)?)
        }
        _ => Ok(serde_json::to_string_pretty(&json!({
            "provider": provider,
            "model": active_model,
            "session_id": session_id,
            "session_root": store.root_dir(),
            "task_count": task_store_for(cwd).list_tasks()?.len(),
            "question_count": task_store_for(cwd).list_questions()?.len(),
            "bridge_server": cli.bridge_server,
            "bridge_connect": cli.bridge_connect,
            "receive_count": cli.bridge_receive_count,
        }))?),
    }
}

