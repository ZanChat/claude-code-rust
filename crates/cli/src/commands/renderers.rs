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
    let metadata = load_session_metadata_for_path(&transcript_path);
    let messages = store.load_session(session_id).await.unwrap_or_default();
    let runtime_messages = materialize_runtime_messages(&messages);
    let first_prompt = runtime_messages
        .iter()
        .find_map(|message| (message.role == MessageRole::User).then(|| message_text(message)));
    let report = SessionCommandReport {
        session_id,
        session_root: store.root_dir().to_path_buf(),
        transcript_path,
        custom_title: metadata.custom_title,
        agent_name: metadata.agent_name,
        tag: metadata.tag,
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
    let settings = load_command_settings();
    Ok(serde_json::to_string_pretty(&json!({
        "provider": provider,
        "model": active_model,
        "session_id": session_id,
        "runtime": if live_runtime { "live" } else { "offline" },
        "task_count": task_store_for(cwd).list_tasks()?.len(),
        "question_count": task_store_for(cwd).list_questions()?.len(),
        "theme": settings.theme,
        "fast_mode": settings.fast_mode,
        "advisor_model": settings.advisor_model,
        "chrome_default_enabled": settings.chrome_default_enabled,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FastCommandOutcome {
    pub(crate) message: String,
    pub(crate) next_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RewindCandidate {
    pub(crate) raw_index: usize,
    pub(crate) turn_number: usize,
    pub(crate) preview: String,
}

fn usage_line(command: &str, args: &str) -> String {
    format!("Usage: /{command} {args}")
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rewindable_user_text(message: &Message) -> Option<String> {
    if message.role != MessageRole::User {
        return None;
    }

    let text = collapse_whitespace(&message_text(message));
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }

    Some(trimmed.to_owned())
}

pub(crate) fn rewind_candidates(raw_messages: &[Message]) -> Vec<RewindCandidate> {
    let mut turn_number = 0usize;
    let mut candidates = raw_messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            let text = rewindable_user_text(message)?;
            turn_number += 1;
            Some(RewindCandidate {
                raw_index: index + 1,
                turn_number,
                preview: preview_lines_from_text(text, 1, 72).join(" "),
            })
        })
        .collect::<Vec<_>>();
    candidates.reverse();
    candidates
}

fn suggested_session_title(raw_messages: &[Message]) -> Option<String> {
    raw_messages
        .iter()
        .rev()
        .filter_map(rewindable_user_text)
        .next()
        .map(|text| preview_lines_from_text(text, 1, 64).join(" "))
}

fn write_transcript_messages(path: &Path, messages: &[Message]) -> Result<()> {
    if messages.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut output = String::new();
    for message in messages {
        output.push_str(&serde_json::to_string(message)?);
        output.push('\n');
    }
    fs::write(path, output)?;
    Ok(())
}

async fn persist_rewound_session(
    store: &ActiveSessionStore,
    session_id: SessionId,
    raw_messages: &[Message],
) -> Result<PathBuf> {
    let transcript_path = store.transcript_path(session_id).await?;
    write_transcript_messages(&transcript_path, raw_messages)?;
    Ok(transcript_path)
}

fn effort_display_name(value: &str) -> &str {
    match value {
        "xhigh" => "max",
        other => other,
    }
}

fn current_effort_summary() -> String {
    let reasoning = env::var("REASONING_MODEL_THINK")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let completion = env::var("COMPLETION_MODEL_THINK")
        .ok()
        .filter(|value| !value.trim().is_empty());

    match (reasoning.as_deref(), completion.as_deref()) {
        (None, None) => "Effort level: auto (reasoning=max, completion=max)".to_owned(),
        (Some(reasoning), Some(completion)) if reasoning == completion => {
            format!("Current effort level: {}", effort_display_name(reasoning))
        }
        (Some(reasoning), Some(completion)) => format!(
            "Current effort level: reasoning={} · completion={}",
            effort_display_name(reasoning),
            effort_display_name(completion)
        ),
        (Some(reasoning), None) => format!(
            "Current effort level: reasoning={} · completion=auto",
            effort_display_name(reasoning)
        ),
        (None, Some(completion)) => format!(
            "Current effort level: reasoning=auto · completion={}",
            effort_display_name(completion)
        ),
    }
}

async fn handoff_lines(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<(PathBuf, SessionMetadata, Vec<String>)> {
    let transcript_path = store.transcript_path(session_id).await?;
    let metadata = load_session_metadata_for_path(&transcript_path);
    let mut lines = vec![format!("Session: {session_id}")];
    if let Some(title) = metadata.display_title() {
        lines.push(format!("Title: {title}"));
    }
    if let Some(tag) = metadata.tag.as_deref().filter(|value| !value.trim().is_empty()) {
        lines.push(format!("Tag: #{tag}"));
    }
    lines.push(format!("Transcript: {}", transcript_path.display()));
    Ok((transcript_path, metadata, lines))
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

fn runtime_dir(cwd: &Path) -> PathBuf {
    cwd.join(".claude")
}

fn plan_mode_state_path(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("plan-mode.json")
}

fn plan_file_path(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("plan.md")
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn plan_mode_is_active(cwd: &Path) -> bool {
    safe_read_text(&plan_mode_state_path(cwd))
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|value| value.get("active").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn activate_plan_mode(cwd: &Path) -> Result<PathBuf> {
    let path = plan_mode_state_path(cwd);
    ensure_parent_dir(&path)?;
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "active": true,
            "cwd": cwd,
        }))?,
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn open_path_in_editor(path: &Path) -> Result<()> {
    let editor = std::env::var_os("VISUAL").or_else(|| std::env::var_os("EDITOR"));
    let status = if let Some(editor) = editor {
        StdCommand::new(editor)
            .arg(path)
            .status()
            .with_context(|| format!("failed to open {} in editor", path.display()))?
    } else {
        #[cfg(target_os = "macos")]
        {
            StdCommand::new("open")
                .arg("-t")
                .arg(path)
                .status()
                .with_context(|| format!("failed to open {} with open -t", path.display()))?
        }
        #[cfg(not(target_os = "macos"))]
        {
            bail!("set VISUAL or EDITOR to use /plan open")
        }
    };

    if !status.success() {
        bail!("editor exited unsuccessfully while opening {}", path.display());
    }
    Ok(())
}

fn task_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::WaitingForInput => "waiting",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn short_task_id(task_id: uuid::Uuid) -> String {
    task_id
        .to_string()
        .split('-')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn is_agent_task_kind(kind: &str) -> bool {
    matches!(
        kind,
        "agent"
            | "workflow"
            | "workflow_step"
            | "coordinator"
            | "assistant_worker"
            | "assistant_synthesis"
    )
}

fn render_preview(title: String, lines: Vec<String>) -> String {
    std::iter::once(title)
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n")
}

fn skill_entry_source_label(entry: &ccrust_plugins::SkillEntry, cwd: &Path) -> String {
    match entry.source {
        ccrust_plugins::SkillSource::Manifest => "plugin".to_owned(),
        ccrust_plugins::SkillSource::LegacyCommandsDir => {
            if entry.path.starts_with(claude_config_home_dir()) {
                "user command".to_owned()
            } else if entry.path.starts_with(cwd) {
                "project command".to_owned()
            } else {
                "command".to_owned()
            }
        }
        ccrust_plugins::SkillSource::LegacySkillsDir => {
            if entry.path.starts_with(claude_config_home_dir()) {
                "user skill".to_owned()
            } else if entry.path.starts_with(cwd) {
                "project skill".to_owned()
            } else {
                "skill".to_owned()
            }
        }
    }
}

fn hook_summary_lines(root: &Path, hooks: &Value) -> Vec<String> {
    match hooks {
        Value::String(path) => vec![format!(
            "- {} ({})",
            path.trim_start_matches("./"),
            root.join(path.trim_start_matches("./")).display()
        )],
        Value::Array(entries) => entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Value::String(path) => format!(
                    "- {} ({})",
                    path.trim_start_matches("./"),
                    root.join(path.trim_start_matches("./")).display()
                ),
                Value::Object(map) => format!(
                    "- Inline hook config #{} ({})",
                    index + 1,
                    map.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
                other => format!("- Hook entry #{} ({other})", index + 1),
            })
            .collect(),
        Value::Object(map) => vec![format!(
            "- Inline hook config ({})",
            map.keys().cloned().collect::<Vec<_>>().join(", ")
        )],
        _ => Vec::new(),
    }
}

fn format_agent_task_record(task: &TaskRecord) -> String {
    let mut lines = vec![format!(
        "Agent {}",
        if task.title.trim().is_empty() {
            short_task_id(task.id)
        } else {
            task.title.clone()
        }
    )];
    lines.push(format!("ID: {}", task.id));
    lines.push(format!("Status: {}", task_status_label(task.status.clone())));
    lines.push(format!("Kind: {}", task.kind));
    if let Some(session_id) = task.session_id {
        lines.push(format!("Session: {session_id}"));
    }
    if let Some(transcript_path) = task.transcript_path.as_ref() {
        lines.push(format!("Transcript: {}", transcript_path.display()));
    }
    if let Some(artifact_path) = task.artifact_path.as_ref() {
        lines.push(format!("Artifact: {}", artifact_path.display()));
    }
    if let Some(output) = task.output.as_ref().filter(|value| !value.trim().is_empty()) {
        lines.push(String::new());
        lines.push("Output:".to_owned());
        lines.extend(preview_lines_from_text(output.clone(), 12, 96));
    }
    lines.join("\n")
}

pub(crate) fn render_theme_command(invocation: &CommandInvocation) -> Result<String> {
    let current_settings = load_command_settings();
    let current_preset = current_settings
        .theme
        .as_deref()
        .and_then(theme_preset)
        .unwrap_or(&THEME_PRESETS[0]);
    let Some(arg) = invocation.args.first().map(String::as_str) else {
        return Ok([
            format!("Current theme: {}", current_preset.label),
            usage_line(
                "theme",
                "[auto|dark|light|dark-daltonized|light-daltonized|dark-ansi|light-ansi]",
            ),
        ]
        .join("\n"));
    };

    let Some(theme_value) = normalize_theme_preset(arg) else {
        return Ok([
            format!("Unknown theme preset: {arg}"),
            usage_line(
                "theme",
                "[auto|dark|light|dark-daltonized|light-daltonized|dark-ansi|light-ansi]",
            ),
        ]
        .join("\n"));
    };

    let preset = theme_preset(theme_value).unwrap_or(&THEME_PRESETS[0]);
    update_command_settings(|settings| settings.theme = Some(theme_value.to_owned()))?;
    apply_ui_theme_preference(Some(theme_value));
    Ok(format!("Theme preference saved as {}.", preset.label))
}

pub(crate) fn render_fast_command(
    invocation: &CommandInvocation,
    provider: ApiProvider,
    active_model: &str,
) -> Result<FastCommandOutcome> {
    let settings = load_command_settings();
    let provider_supports_fast_mode = matches!(
        provider,
        ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
    );
    let Some(arg) = invocation.args.first().map(|value| value.trim().to_ascii_lowercase()) else {
        let status = if settings.fast_mode && !provider_supports_fast_mode {
            "ON (inactive for current provider)"
        } else if settings.fast_mode {
            "ON"
        } else {
            "OFF"
        };
        let provider_note = if provider_supports_fast_mode {
            String::new()
        } else {
            format!(
                "\nSaved fast-mode preferences only affect chatgpt-codex and openai-compatible sessions."
            )
        };
        return Ok(FastCommandOutcome {
            message: format!(
                "Fast mode: {status}\nCurrent model: {active_model}{provider_note}\n{}",
                usage_line("fast", "[on|off]")
            ),
            next_model: None,
        });
    };

    let enable = match arg.as_str() {
        "on" | "enable" => true,
        "off" | "disable" => false,
        _ => {
            return Ok(FastCommandOutcome {
                message: usage_line("fast", "[on|off]"),
                next_model: None,
            });
        }
    };

    let settings = update_command_settings(|settings| settings.fast_mode = enable)?;
    let next_model = preferred_model_for_provider(provider, &settings)
        .filter(|model| enable && model != active_model);
    let message = if let Some(model) = next_model.as_deref() {
        format!("Fast mode enabled. Switched to {model} for this session.")
    } else if enable {
        if provider_supports_fast_mode {
            "Fast mode enabled.".to_owned()
        } else {
            "Fast mode enabled for supported OpenAI-family sessions. The current provider does not change models for fast mode.".to_owned()
        }
    } else {
        "Fast mode disabled.".to_owned()
    };

    Ok(FastCommandOutcome { message, next_model })
}

pub(crate) fn render_effort_command(cwd: &Path, invocation: &CommandInvocation) -> Result<String> {
    let Some(arg) = invocation.args.first().map(|value| value.trim().to_ascii_lowercase()) else {
        return Ok([
            current_effort_summary(),
            usage_line("effort", "[low|medium|high|max|auto]"),
        ]
        .join("\n"));
    };

    let updates = match arg.as_str() {
        "auto" | "unset" => BTreeMap::from([
            ("REASONING_MODEL_THINK".to_owned(), None),
            ("COMPLETION_MODEL_THINK".to_owned(), None),
        ]),
        "low" | "medium" | "high" => BTreeMap::from([
            ("REASONING_MODEL_THINK".to_owned(), Some(arg.clone())),
            ("COMPLETION_MODEL_THINK".to_owned(), Some(arg.clone())),
        ]),
        "max" | "xhigh" => BTreeMap::from([
            ("REASONING_MODEL_THINK".to_owned(), Some("xhigh".to_owned())),
            ("COMPLETION_MODEL_THINK".to_owned(), Some("xhigh".to_owned())),
        ]),
        _ => {
            return Ok([
                format!("Invalid effort level: {arg}"),
                usage_line("effort", "[low|medium|high|max|auto]"),
            ]
            .join("\n"));
        }
    };

    persist_managed_env_updates(cwd, None, &updates)?;
    apply_managed_env_updates(&updates);

    Ok(match arg.as_str() {
        "auto" | "unset" => "Effort level set to auto.".to_owned(),
        "xhigh" => "Effort level set to max.".to_owned(),
        other => format!("Effort level set to {other}."),
    })
}

pub(crate) async fn render_tag_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
    invocation: &CommandInvocation,
) -> Result<String> {
    let Some(arg) = invocation_argument_string(invocation) else {
        return Ok([
            usage_line("tag", "<tag-name>"),
            "Run the same tag again to remove it from the current session.".to_owned(),
        ]
        .join("\n"));
    };

    let normalized = normalize_session_tag(&arg);
    if normalized.is_empty() {
        return Ok("Tag name cannot be empty.".to_owned());
    }

    let transcript_path = store.transcript_path(session_id).await?;
    let current = load_session_metadata_for_path(&transcript_path);
    if current.tag.as_deref() == Some(normalized.as_str()) {
        update_session_metadata_for_path(&transcript_path, |metadata| metadata.tag = None)?;
        return Ok(format!("Removed tag #{normalized}."));
    }

    let previous = current.tag;
    update_session_metadata_for_path(&transcript_path, |metadata| {
        metadata.tag = Some(normalized.clone());
    })?;
    Ok(match previous {
        Some(previous) if !previous.trim().is_empty() => {
            format!("Updated session tag from #{previous} to #{normalized}.")
        }
        _ => format!("Tagged session with #{normalized}."),
    })
}

pub(crate) async fn render_rename_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
    invocation: &CommandInvocation,
    raw_messages: &[Message],
) -> Result<String> {
    let transcript_path = store.transcript_path(session_id).await?;
    let new_name = if let Some(arg) = invocation_argument_string(invocation) {
        collapse_whitespace(&arg)
    } else {
        suggested_session_title(raw_messages).ok_or_else(|| {
            anyhow!("Could not generate a name yet. Usage: /rename <name>")
        })?
    };

    if new_name.trim().is_empty() {
        return Ok("Session name cannot be empty.".to_owned());
    }

    update_session_metadata_for_path(&transcript_path, |metadata| {
        metadata.custom_title = Some(new_name.clone());
        metadata.agent_name = Some(new_name.clone());
    })?;
    Ok(format!("Session renamed to: {new_name}"))
}

pub(crate) async fn render_rewind_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
    invocation: &CommandInvocation,
    raw_messages: &mut Vec<Message>,
) -> Result<String> {
    let candidates = rewind_candidates(raw_messages);
    if candidates.is_empty() {
        return Ok("No conversation turns are available to rewind.".to_owned());
    }

    let Some(arg) = invocation.args.first() else {
        return Ok([
            "Use the /rewind picker to choose a turn, or run /rewind <message-index>."
                .to_owned(),
            format!(
                "Latest turn: {} ({})",
                candidates[0].turn_number, candidates[0].preview
            ),
        ]
        .join("\n"));
    };

    let Ok(raw_index) = arg.parse::<usize>() else {
        return Ok("/rewind expects a numeric message index from the picker.".to_owned());
    };
    let Some(candidate) = candidates.iter().find(|candidate| candidate.raw_index == raw_index) else {
        return Ok(format!("No rewind target found for message index {raw_index}."));
    };

    raw_messages.truncate(candidate.raw_index.saturating_sub(1));
    let transcript_path = persist_rewound_session(store, session_id, raw_messages).await?;
    Ok(format!(
        "Rewound to before turn {}: {}\nTranscript: {}",
        candidate.turn_number,
        candidate.preview,
        transcript_path.display()
    ))
}

pub(crate) async fn render_mobile_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<String> {
    let (_, _, mut lines) = handoff_lines(store, session_id).await?;
    lines.insert(0, "Mobile handoff".to_owned());
    lines.push(String::new());
    lines.push(format!("iOS: {IOS_APP_URL}"));
    lines.push(format!("Android: {ANDROID_APP_URL}"));
    Ok(lines.join("\n"))
}

pub(crate) async fn render_desktop_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<String> {
    let (_, _, mut lines) = handoff_lines(store, session_id).await?;
    lines.insert(0, "Desktop handoff".to_owned());
    lines.insert(1, format!("Resume: {}", resume_command_for_session(session_id)));
    Ok(lines.join("\n"))
}

pub(crate) fn render_chrome_command(invocation: &CommandInvocation) -> Result<String> {
    let current_settings = load_command_settings();
    let Some(arg) = invocation.args.first().map(|value| value.trim().to_ascii_lowercase()) else {
        return Ok([
            format!(
                "Claude in Chrome default: {}",
                if current_settings.chrome_default_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
            format!("Install: {CHROME_EXTENSION_URL}"),
            format!("Permissions: {CHROME_PERMISSIONS_URL}"),
            format!("Reconnect: {CHROME_RECONNECT_URL}"),
            usage_line("chrome", "[on|off|install|permissions|reconnect]"),
        ]
        .join("\n"));
    };

    match arg.as_str() {
        "on" | "enable" => {
            update_command_settings(|settings| settings.chrome_default_enabled = true)?;
            Ok("Claude in Chrome is enabled by default.".to_owned())
        }
        "off" | "disable" => {
            update_command_settings(|settings| settings.chrome_default_enabled = false)?;
            Ok("Claude in Chrome is disabled by default.".to_owned())
        }
        "install" => Ok(format!("Open the extension page:\n{CHROME_EXTENSION_URL}")),
        "permissions" => Ok(format!(
            "Open the permissions page:\n{CHROME_PERMISSIONS_URL}"
        )),
        "reconnect" => Ok(format!("Open the reconnect page:\n{CHROME_RECONNECT_URL}")),
        _ => Ok(usage_line("chrome", "[on|off|install|permissions|reconnect]")),
    }
}

pub(crate) fn render_advisor_command(invocation: &CommandInvocation) -> Result<String> {
    let current_settings = load_command_settings();
    let Some(arg) = invocation_argument_string(invocation) else {
        return Ok(match current_settings.advisor_model {
            Some(model) => format!(
                "Advisor: {model}\nUse /advisor unset to disable or /advisor <model> to change it."
            ),
            None => "Advisor: not set\nUse /advisor <model> to enable it.".to_owned(),
        });
    };

    let normalized = collapse_whitespace(&arg).to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok("Advisor model cannot be empty.".to_owned());
    }

    if matches!(normalized.as_str(), "unset" | "off") {
        let previous = current_settings.advisor_model;
        update_command_settings(|settings| settings.advisor_model = None)?;
        return Ok(match previous {
            Some(model) => format!("Advisor disabled (was {model})."),
            None => "Advisor already unset.".to_owned(),
        });
    }

    update_command_settings(|settings| settings.advisor_model = Some(normalized.clone()))?;
    Ok(format!("Advisor set to {normalized}."))
}

pub(crate) fn render_vim_command(enabled: bool) -> Result<String> {
    Ok(if enabled {
        "Vim mode enabled. Insert and normal mode are available in the Rust REPL.".to_owned()
    } else {
        "Vim mode disabled. The prompt is using standard editing shortcuts.".to_owned()
    })
}

pub(crate) fn render_plan_command(cwd: &Path, invocation: &CommandInvocation) -> Result<String> {
    let plan_path = plan_file_path(cwd);

    if matches!(invocation.args.first().map(String::as_str), Some("open")) {
        if !plan_path.exists() {
            ensure_parent_dir(&plan_path)?;
            if !plan_mode_is_active(cwd) {
                let _ = activate_plan_mode(cwd)?;
            }
            fs::write(&plan_path, b"")
                .with_context(|| format!("failed to initialize {}", plan_path.display()))?;
        }
        open_path_in_editor(&plan_path)?;
        return Ok(format!("Opened plan in editor: {}", plan_path.display()));
    }

    if !plan_mode_is_active(cwd) {
        let state_path = activate_plan_mode(cwd)?;
        let detail = invocation.args.join(" ");
        if detail.trim().is_empty() {
            return Ok(format!(
                "Enabled plan mode. State is recorded at {}.",
                state_path.display()
            ));
        }
        return Ok(format!(
            "Enabled plan mode. Re-run your planning request now that plan mode is active: {}",
            detail.trim()
        ));
    }

    let Some(plan_content) = safe_read_text(&plan_path) else {
        return Ok("Already in plan mode. No plan written yet.".to_owned());
    };

    let mut lines = vec!["Current plan".to_owned(), plan_path.display().to_string()];
    lines.push(String::new());
    lines.extend(preview_lines_from_text(plan_content, 24, 96));
    lines.push(String::new());
    lines.push("Run /plan open to edit the plan in your editor.".to_owned());
    Ok(lines.join("\n"))
}

pub(crate) fn render_simple_compat_command(name: &str, message: &str) -> Result<String> {
    Ok(format!("/{name}\n{message}"))
}

pub(crate) fn render_hooks_command(cwd: &Path, plugin_root: Option<&PathBuf>) -> Result<String> {
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    let lines = load_plugin_manifest_sync(&root)
        .and_then(|manifest| manifest.hooks)
        .map(|hooks| hook_summary_lines(&root, &hooks))
        .unwrap_or_default();

    if lines.is_empty() {
        return Ok("Hooks\nNo hook configuration found for this workspace.".to_owned());
    }

    Ok(std::iter::once("Hooks".to_owned())
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) fn render_output_style_command() -> Result<String> {
    Ok("/output-style has been deprecated. Use /config to change your output style, or set it in your settings file. Changes take effect on the next session.".to_owned())
}

pub(crate) fn render_files_command(raw_messages: &[Message], cwd: &Path) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_file_message(&runtime_messages, cwd).unwrap_or(PanePreview {
        title: "File preview".to_owned(),
        lines: vec!["No file preview available yet.".to_owned()],
    });
    Ok(render_preview(preview.title, preview.lines))
}

pub(crate) fn render_diff_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let preview = preview_for_last_diff_message(&runtime_messages).unwrap_or(PanePreview {
        title: "Diff preview".to_owned(),
        lines: vec!["No diff preview available yet.".to_owned()],
    });
    Ok(render_preview(preview.title, preview.lines))
}

pub(crate) fn render_usage_command(raw_messages: &[Message]) -> Result<String> {
    let runtime_messages = materialize_runtime_messages(raw_messages);
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_creation_input_tokens = 0u64;
    let mut cache_read_input_tokens = 0u64;
    let mut latest_prompt_metrics = None;
    let mut per_provider = BTreeMap::new();

    for message in &runtime_messages {
        if let Some(metrics) = parse_runtime_prompt_metrics(message) {
            latest_prompt_metrics = Some(metrics);
        }

        let Some(usage) = message.metadata.usage.as_ref() else {
            continue;
        };
        input_tokens += usage.input_tokens;
        output_tokens += usage.output_tokens;
        cache_creation_input_tokens += usage.cache_creation_input_tokens;
        cache_read_input_tokens += usage.cache_read_input_tokens;

        let provider_name = message
            .metadata
            .provider
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let provider_totals = per_provider
            .entry(provider_name)
            .or_insert((0u64, 0u64, 0u64, 0u64, 0u64));
        provider_totals.0 += usage.input_tokens;
        provider_totals.1 += usage.output_tokens;
        provider_totals.2 += usage.cache_creation_input_tokens;
        provider_totals.3 += usage.cache_read_input_tokens;
        provider_totals.4 += 1;
    }

    let total_input_related_tokens =
        input_tokens + cache_creation_input_tokens + cache_read_input_tokens;
    let cache_hit_rate = if total_input_related_tokens == 0 {
        0.0
    } else {
        ((cache_read_input_tokens as f64 / total_input_related_tokens as f64) * 1000.0).round()
            / 1000.0
    };

    let per_provider = per_provider
        .into_iter()
        .map(
            |(
                provider,
                (
                    provider_input,
                    provider_output,
                    provider_cache_creation,
                    provider_cache_read,
                    response_count,
                ),
            )| {
                let provider_total_input =
                    provider_input + provider_cache_creation + provider_cache_read;
                let provider_cache_hit_rate = if provider_total_input == 0 {
                    0.0
                } else {
                    ((provider_cache_read as f64 / provider_total_input as f64) * 1000.0).round()
                        / 1000.0
                };
                (
                    provider,
                    json!({
                        "input_tokens": provider_input,
                        "output_tokens": provider_output,
                        "cache_creation_input_tokens": provider_cache_creation,
                        "cache_read_input_tokens": provider_cache_read,
                        "cache_hit_rate": provider_cache_hit_rate,
                        "response_count": response_count,
                    }),
                )
            },
        )
        .collect::<BTreeMap<_, _>>();

    let mut report = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_creation_input_tokens": cache_creation_input_tokens,
        "cache_read_input_tokens": cache_read_input_tokens,
        "cache_hit_rate": cache_hit_rate,
        "message_count": runtime_messages.len(),
        "providers": per_provider,
    });

    if let Some(metrics) = latest_prompt_metrics {
        report["latest_prompt"] = json!({
            "static_chars": metrics.static_chars,
            "semi_static_chars": metrics.semi_static_chars,
            "dynamic_chars": metrics.dynamic_chars,
        });
        if prompt_cache_debug_enabled() {
            report["latest_prompt"]["hashes"] = json!({
                "static": metrics.static_hash,
                "semi_static": metrics.semi_static_hash,
                "dynamic": metrics.dynamic_hash,
                "semi_static_fingerprint": metrics.semi_static_fingerprint,
            });
        }
    }

    Ok(serde_json::to_string_pretty(&report)?)
}

pub(crate) fn render_export_command(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<String> {
    Ok(format!(
        "Transcript export ready\nSession: {session_id}\nPath: {}",
        store.root_dir().join(format!("{session_id}.jsonl")).display()
    ))
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
            let task = serde_json::from_value::<TaskRecord>(report.metadata.clone())
                .or_else(|_| serde_json::from_str::<TaskRecord>(&report.content))?;
            Ok(format!(
                "Created agent task\n{}\n\nUse /agents get {} to inspect it.",
                format_agent_task_record(&task),
                task.id
            ))
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
            let task = serde_json::from_value::<TaskRecord>(report.metadata.clone())
                .or_else(|_| serde_json::from_str::<TaskRecord>(&report.content))?;
            Ok(format_agent_task_record(&task))
        }
        _ => {
            let tasks = task_store_for(cwd)
                .list_tasks()?
                .into_iter()
                .filter(|task| is_agent_task_kind(task.kind.as_str()))
                .collect::<Vec<_>>();
            if tasks.is_empty() {
                return Ok("Agents\nNo agents found. Use /agents create <title> to start one.".to_owned());
            }

            let mut lines = vec![format!("Agents ({})", tasks.len())];
            lines.push("Use /agents create <title> to start a new delegated task.".to_owned());
            lines.extend(tasks.into_iter().map(|task| {
                format!(
                    "- [{}] {} ({})",
                    task_status_label(task.status),
                    if task.title.trim().is_empty() {
                        "agent task".to_owned()
                    } else {
                        task.title
                    },
                    short_task_id(task.id)
                )
            }));
            Ok(lines.join("\n"))
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
                    response: ccrust_bridge::RemotePermissionResponse {
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
