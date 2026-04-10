use crate::cli_args::ts_top_level_cli_option_specs;
use crate::cli_graph::{top_level_command_specs, ROOT_COMMAND_NAME};
use crate::helpers::{parse_task_id, parse_task_status, task_store_for};
use crate::reports::TaskCommandReport;
use crate::session::ActiveSessionStore;
use anyhow::{anyhow, bail, Context, Result};
use ccrust_core::{
    BoundaryKind, ContentBlock, LocalTaskStore as CoreLocalTaskStore, Message, MessageRole,
    SessionId, TaskRecord, TaskStatus, TaskStore,
};
use ccrust_session::{
    claude_config_home_dir, sanitize_path, JsonlTranscriptCodec, TranscriptCodec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DEFAULT_TASK_LIST_ID: &str = "tasklist";
const INSTALL_HISTORY_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallPaths {
    pub(crate) destination_path: PathBuf,
    pub(crate) state_path: PathBuf,
    pub(crate) snapshot_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InstallSnapshot {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) created_at_unix_ms: i64,
    pub(crate) path: PathBuf,
    pub(crate) source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InstallState {
    pub(crate) current: Option<InstallSnapshot>,
    pub(crate) history: Vec<InstallSnapshot>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct InstallCommandReport {
    pub(crate) action: &'static str,
    pub(crate) destination: PathBuf,
    pub(crate) source: String,
    pub(crate) label: String,
    pub(crate) current_snapshot: Option<PathBuf>,
    pub(crate) previous_snapshot: Option<PathBuf>,
    pub(crate) skipped: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct RollbackCommandReport {
    pub(crate) action: &'static str,
    pub(crate) destination: PathBuf,
    pub(crate) restored_snapshot: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) available: Vec<String>,
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn task_store_for_list(cwd: &Path, list_id: Option<&str>) -> CoreLocalTaskStore {
    match list_id.filter(|value| !value.trim().is_empty()) {
        None | Some(DEFAULT_TASK_LIST_ID) => task_store_for(cwd),
        Some(list_id) => CoreLocalTaskStore::new(
            cwd.join(".claude")
                .join("tasklists")
                .join(sanitize_path(list_id)),
        ),
    }
}

fn take_option_value(args: &[String], index: &mut usize, display: &str) -> Result<String> {
    let next_index = *index + 1;
    let value = args
        .get(next_index)
        .ok_or_else(|| anyhow!("missing value for {display}"))?;
    if value.starts_with('-') {
        bail!("missing value for {display}");
    }
    *index = next_index;
    Ok(value.clone())
}

fn task_owner_value(task: &TaskRecord) -> Option<String> {
    task.metadata
        .get("owner")
        .cloned()
        .or_else(|| task.agent_id.map(|agent_id| agent_id.to_string()))
}

fn human_task_list(tasks: &[TaskRecord]) -> String {
    if tasks.is_empty() {
        return "No tasks found.".to_owned();
    }

    let mut lines = vec![format!("Tasks ({})", tasks.len())];
    lines.extend(tasks.iter().map(|task| {
        let mut suffix = Vec::new();
        if let Some(owner) = task_owner_value(task) {
            suffix.push(format!("owner={owner}"));
        }
        if let Some(description) = task
            .input
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            suffix.push(format!("description={description}"));
        }
        if suffix.is_empty() {
            format!("- [{:?}] {} ({})", task.status, task.title, task.id)
        } else {
            format!(
                "- [{:?}] {} ({}) {}",
                task.status,
                task.title,
                task.id,
                suffix.join(" ")
            )
        }
    }));
    lines.join("\n")
}

pub(crate) fn render_task_cli_command(cwd: &Path, args: &[String]) -> Result<String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            let mut description = None;
            let mut list_id = None;
            let mut subject = Vec::new();
            let mut index = 1usize;
            while index < args.len() {
                match args[index].as_str() {
                    "-d" | "--description" => {
                        description = Some(take_option_value(args, &mut index, "--description")?);
                    }
                    "-l" | "--list" => {
                        list_id = Some(take_option_value(args, &mut index, "--list")?);
                    }
                    value if value.starts_with('-') => bail!("unknown option: {value}"),
                    value => subject.push(value.to_owned()),
                }
                index += 1;
            }

            if subject.is_empty() {
                bail!("task create requires a subject");
            }

            let store = task_store_for_list(cwd, list_id.as_deref());
            let mut task = TaskRecord::new("task", subject.join(" "));
            task.input = description;
            let created = store.create_task(task)?;
            Ok(serde_json::to_string_pretty(&created)?)
        }
        Some("list") => {
            let mut list_id = None;
            let mut pending_only = false;
            let mut json = false;
            let mut index = 1usize;
            while index < args.len() {
                match args[index].as_str() {
                    "-l" | "--list" => {
                        list_id = Some(take_option_value(args, &mut index, "--list")?);
                    }
                    "--pending" => pending_only = true,
                    "--json" => json = true,
                    value => bail!("unknown option: {value}"),
                }
                index += 1;
            }

            let store = task_store_for_list(cwd, list_id.as_deref());
            let tasks = store
                .list_tasks()?
                .into_iter()
                .filter(|task| !pending_only || task.status == TaskStatus::Pending)
                .collect::<Vec<_>>();
            if json {
                Ok(serde_json::to_string_pretty(&TaskCommandReport {
                    count: tasks.len(),
                    tasks,
                })?)
            } else {
                Ok(human_task_list(&tasks))
            }
        }
        Some("get") => {
            let mut list_id = None;
            let mut task_id = None;
            let mut index = 1usize;
            while index < args.len() {
                match args[index].as_str() {
                    "-l" | "--list" => {
                        list_id = Some(take_option_value(args, &mut index, "--list")?);
                    }
                    value if value.starts_with('-') => bail!("unknown option: {value}"),
                    value if task_id.is_none() => task_id = Some(parse_task_id(value)?),
                    value => bail!("unexpected argument for task get: {value}"),
                }
                index += 1;
            }

            let task_id = task_id.ok_or_else(|| anyhow!("task get requires a task id"))?;
            let store = task_store_for_list(cwd, list_id.as_deref());
            let task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            Ok(serde_json::to_string_pretty(&task)?)
        }
        Some("update") => {
            let mut list_id = None;
            let mut status = None;
            let mut subject = None;
            let mut description = None;
            let mut owner = None;
            let mut clear_owner = false;
            let mut task_id = None;
            let mut index = 1usize;
            while index < args.len() {
                match args[index].as_str() {
                    "-l" | "--list" => {
                        list_id = Some(take_option_value(args, &mut index, "--list")?);
                    }
                    "-s" | "--status" => {
                        status = Some(take_option_value(args, &mut index, "--status")?);
                    }
                    "--subject" => {
                        subject = Some(take_option_value(args, &mut index, "--subject")?);
                    }
                    "-d" | "--description" => {
                        description = Some(take_option_value(args, &mut index, "--description")?);
                    }
                    "--owner" => {
                        owner = Some(take_option_value(args, &mut index, "--owner")?);
                    }
                    "--clear-owner" => clear_owner = true,
                    value if value.starts_with('-') => bail!("unknown option: {value}"),
                    value if task_id.is_none() => task_id = Some(parse_task_id(value)?),
                    value => bail!("unexpected argument for task update: {value}"),
                }
                index += 1;
            }

            let task_id = task_id.ok_or_else(|| anyhow!("task update requires a task id"))?;
            let store = task_store_for_list(cwd, list_id.as_deref());
            let mut task = store
                .get_task(task_id)?
                .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
            if let Some(subject) = subject {
                task.title = subject;
            }
            if let Some(description) = description {
                task.input = Some(description);
            }
            if let Some(status) = status.as_deref() {
                task.status = parse_task_status(status)?;
            }
            if clear_owner {
                task.metadata.remove("owner");
                task.agent_id = None;
            }
            if let Some(owner) = owner {
                task.metadata.insert("owner".to_owned(), owner.clone());
                task.agent_id = uuid::Uuid::parse_str(&owner).ok();
            }
            let saved = store.save_task(task)?;
            Ok(serde_json::to_string_pretty(&saved)?)
        }
        Some("dir") => {
            let mut list_id = None;
            let mut index = 1usize;
            while index < args.len() {
                match args[index].as_str() {
                    "-l" | "--list" => {
                        list_id = Some(take_option_value(args, &mut index, "--list")?);
                    }
                    value => bail!("unknown option: {value}"),
                }
                index += 1;
            }

            let store = task_store_for_list(cwd, list_id.as_deref());
            Ok(store.root_dir().display().to_string())
        }
        Some(other) => bail!("unknown task subcommand: {other}"),
        None => bail!("task requires a subcommand"),
    }
}

fn parse_json_transcript(path: &Path) -> Result<Vec<Message>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read transcript {}", path.display()))?;
    if let Ok(messages) = serde_json::from_str::<Vec<Message>>(&raw) {
        return Ok(messages);
    }

    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode transcript {}", path.display()))?;
    if let Some(messages) = value.get("messages") {
        return serde_json::from_value(messages.clone()).with_context(|| {
            format!(
                "failed to decode 'messages' from transcript {}",
                path.display()
            )
        });
    }

    bail!("unsupported JSON transcript shape in {}", path.display())
}

async fn load_export_source(
    store: &ActiveSessionStore,
    source: &str,
) -> Result<(Option<SessionId>, PathBuf, Vec<Message>)> {
    let source_path = PathBuf::from(source);
    if source_path.exists() {
        let messages = match source_path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => parse_json_transcript(&source_path)?,
            _ => JsonlTranscriptCodec.read_messages(&source_path).await?,
        };
        let session_id = messages.iter().find_map(|message| message.session_id);
        return Ok((session_id, source_path, messages));
    }

    if let Ok(index) = source.parse::<usize>() {
        let mut sessions = store.list_sessions().await?;
        sessions.sort_by(|left, right| right.modified_at_unix_ms.cmp(&left.modified_at_unix_ms));
        let session = sessions
            .get(index)
            .ok_or_else(|| anyhow!("no session found at export index {index}"))?;
        let messages = JsonlTranscriptCodec
            .read_messages(&session.transcript_path)
            .await?;
        return Ok((
            Some(session.session_id),
            session.transcript_path.clone(),
            messages,
        ));
    }

    let session_id = SessionId::parse_str(source)
        .map_err(|error| anyhow!("invalid export source '{source}': {error}"))?;
    let transcript_path = store.transcript_path(session_id).await?;
    let messages = JsonlTranscriptCodec.read_messages(&transcript_path).await?;
    Ok((Some(session_id), transcript_path, messages))
}

fn export_block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text { text } => Some(text.clone()),
        ContentBlock::ToolCall { call } => {
            Some(format!("[tool call] {} {}", call.name, call.input_json))
        }
        ContentBlock::ToolResult { result } => Some(format!(
            "[tool result{}] {}",
            if result.is_error { " error" } else { "" },
            result.output_text
        )),
        ContentBlock::Attachment { attachment } => Some(format!(
            "[attachment] {} {}",
            attachment.name, attachment.uri
        )),
        ContentBlock::Boundary { boundary } => Some(match boundary.kind {
            BoundaryKind::Compact => "[compact boundary]".to_owned(),
            BoundaryKind::MicroCompact => "[micro-compact boundary]".to_owned(),
            BoundaryKind::SessionMemory => "[session-memory boundary]".to_owned(),
            BoundaryKind::Resume => "[resume boundary]".to_owned(),
        }),
    }
}

fn render_export_transcript(
    session_id: Option<SessionId>,
    transcript_path: &Path,
    messages: &[Message],
) -> String {
    let mut sections = Vec::new();
    sections.push(format!("Transcript: {}", transcript_path.display()));
    if let Some(session_id) = session_id {
        sections.push(format!("Session: {session_id}"));
    }
    sections.push(String::new());

    for message in messages {
        let blocks = message
            .blocks
            .iter()
            .filter_map(export_block_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            continue;
        }
        let role = match message.role {
            MessageRole::System => "System",
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::Tool => "Tool",
            MessageRole::Attachment => "Attachment",
        };
        sections.push(format!("{role}:"));
        sections.push(blocks.join("\n"));
        sections.push(String::new());
    }

    sections.join("\n")
}

pub(crate) async fn render_export_cli_command(
    store: &ActiveSessionStore,
    args: &[String],
) -> Result<String> {
    let source = args
        .first()
        .ok_or_else(|| anyhow!("export requires a source"))?;
    let output_path = args
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("export requires an output file path"))?;
    if args.len() > 2 {
        bail!("export accepts exactly two arguments: <source> <outputFile>");
    }

    let (session_id, transcript_path, messages) = load_export_source(store, source).await?;
    let rendered = render_export_transcript(session_id, &transcript_path, &messages);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create export directory {}", parent.display()))?;
    }
    fs::write(&output_path, rendered)
        .with_context(|| format!("failed to write export {}", output_path.display()))?;

    Ok(format!("Exported transcript to {}", output_path.display()))
}

fn completion_root_options() -> Vec<String> {
    let mut options = BTreeSet::new();
    for option in [
        "--help",
        "-h",
        "--version",
        "-v",
        "--provider",
        "--session-root",
        "--print-workspace",
        "--list-commands",
        "--list-sessions",
        "--tui",
        "--repl",
        "--plugin-root",
        "--show-plugin",
        "--list-skills",
        "--list-mcp",
        "--bridge-server",
        "--bridge-connect",
        "--bridge-receive-count",
        "--assistant-directive",
        "--assistant-agent",
        "--voice-text",
        "--voice-file",
        "--voice-format",
        "--clear-session",
        "--tool",
        "--input",
    ] {
        options.insert(option.to_owned());
    }
    for spec in ts_top_level_cli_option_specs() {
        options.insert(format!("--{}", spec.canonical));
        for alias in spec.aliases {
            options.insert(format!("--{alias}"));
        }
        for short in spec.short_aliases {
            options.insert(format!("-{short}"));
        }
    }
    options.into_iter().collect()
}

fn completion_commands() -> Vec<String> {
    let mut commands = top_level_command_specs()
        .iter()
        .map(|spec| spec.primary.to_owned())
        .collect::<Vec<_>>();
    commands.sort();
    commands
}

fn completion_subcommands(command: &str) -> &'static [&'static str] {
    match command {
        "auth" => &["login", "logout", "status"],
        "mcp" => &[
            "add",
            "add-json",
            "add-from-claude-desktop",
            "get",
            "list",
            "remove",
            "reset-project-choices",
            "serve",
        ],
        "plugin" => &[
            "validate",
            "list",
            "install",
            "uninstall",
            "enable",
            "disable",
            "update",
            "marketplace",
        ],
        "task" => &["create", "list", "get", "update", "dir"],
        "completion" => &["bash", "zsh", "fish"],
        _ => &[],
    }
}

fn render_bash_completion() -> String {
    let commands = completion_commands().join(" ");
    let root_options = completion_root_options().join(" ");
    format!(
        "# bash completion for {name}\n_{name}_completion() {{\n  local cur prev cmd\n  cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  prev=\"${{COMP_WORDS[COMP_CWORD-1]}}\"\n  cmd=\"${{COMP_WORDS[1]}}\"\n\n  if [[ $COMP_CWORD -eq 1 ]]; then\n    COMPREPLY=( $(compgen -W \"{commands} {root_options}\" -- \"$cur\") )\n    return\n  fi\n\n  case \"$cmd\" in\n    auth) COMPREPLY=( $(compgen -W \"login logout status\" -- \"$cur\") ) ;;\n    mcp) COMPREPLY=( $(compgen -W \"add add-json add-from-claude-desktop get list remove reset-project-choices serve\" -- \"$cur\") ) ;;\n    plugin) COMPREPLY=( $(compgen -W \"validate list install uninstall enable disable update marketplace\" -- \"$cur\") ) ;;\n    task) COMPREPLY=( $(compgen -W \"create list get update dir\" -- \"$cur\") ) ;;\n    completion) COMPREPLY=( $(compgen -W \"bash zsh fish\" -- \"$cur\") ) ;;\n    *) COMPREPLY=( $(compgen -W \"{root_options}\" -- \"$cur\") ) ;;\n  esac\n}}\ncomplete -F _{name}_completion {name}\n",
        name = ROOT_COMMAND_NAME,
    )
}

fn render_zsh_completion() -> String {
    let commands = completion_commands()
        .into_iter()
        .map(|command| format!("'{command}'"))
        .collect::<Vec<_>>()
        .join(" ");
    let options = completion_root_options()
        .into_iter()
        .map(|option| format!("'{option}'"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "#compdef {name}\n\nlocal -a commands\ncommands=({commands})\nlocal -a options\noptions=({options})\n\nif (( CURRENT == 2 )); then\n  _describe 'command' commands\n  _describe 'option' options\n  return\nfi\n\ncase \"${{words[2]}}\" in\n  auth) _describe 'auth command' 'login' 'logout' 'status' ;;\n  mcp) _describe 'mcp command' 'add' 'add-json' 'add-from-claude-desktop' 'get' 'list' 'remove' 'reset-project-choices' 'serve' ;;\n  plugin) _describe 'plugin command' 'validate' 'list' 'install' 'uninstall' 'enable' 'disable' 'update' 'marketplace' ;;\n  task) _describe 'task command' 'create' 'list' 'get' 'update' 'dir' ;;\n  completion) _describe 'shell' 'bash' 'zsh' 'fish' ;;\n  *) _describe 'option' options ;;\n esac\n",
        name = ROOT_COMMAND_NAME,
    )
}

fn render_fish_completion() -> String {
    let mut lines = vec![format!("# fish completion for {ROOT_COMMAND_NAME}")];
    lines.push(format!(
        "complete -c {ROOT_COMMAND_NAME} -n '__fish_use_subcommand' -a \"{}\"",
        completion_commands().join(" ")
    ));
    for option in completion_root_options() {
        if let Some(long) = option.strip_prefix("--") {
            lines.push(format!("complete -c {ROOT_COMMAND_NAME} -l {long}",));
        } else if let Some(short) = option.strip_prefix('-') {
            lines.push(format!("complete -c {ROOT_COMMAND_NAME} -s {short}",));
        }
    }
    for command in ["auth", "mcp", "plugin", "task", "completion"] {
        let subcommands = completion_subcommands(command);
        if !subcommands.is_empty() {
            lines.push(format!(
                "complete -c {ROOT_COMMAND_NAME} -n '__fish_seen_subcommand_from {command}' -a \"{}\"",
                subcommands.join(" ")
            ));
        }
    }
    lines.join("\n")
}

pub(crate) fn render_completion_command(args: &[String]) -> Result<String> {
    let shell = args
        .first()
        .ok_or_else(|| anyhow!("completion requires a shell: bash, zsh, or fish"))?;
    let mut output = None;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                output = Some(PathBuf::from(take_option_value(
                    args, &mut index, "--output",
                )?))
            }
            value => bail!("unknown option: {value}"),
        }
        index += 1;
    }

    let script = match shell.as_str() {
        "bash" => render_bash_completion(),
        "zsh" => render_zsh_completion(),
        "fish" => render_fish_completion(),
        other => bail!("unsupported shell for completion: {other}"),
    };

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create completion output dir {}",
                    parent.display()
                )
            })?;
        }
        fs::write(&path, script)
            .with_context(|| format!("failed to write completion script {}", path.display()))?;
        Ok(format!(
            "Wrote {shell} completion script to {}",
            path.display()
        ))
    } else {
        Ok(script)
    }
}

fn cargo_bin_dir() -> PathBuf {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .unwrap_or_else(|| PathBuf::from(".cargo"))
        .join("bin")
}

pub(crate) fn default_install_paths() -> InstallPaths {
    let state_root = claude_config_home_dir().join("ccrust");
    InstallPaths {
        destination_path: cargo_bin_dir().join(ROOT_COMMAND_NAME),
        state_path: state_root.join("install-state.json"),
        snapshot_dir: state_root.join("install-history"),
    }
}

fn read_install_state(path: &Path) -> Result<InstallState> {
    if !path.exists() {
        return Ok(InstallState::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read install state {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode install state {}", path.display()))
}

fn write_install_state(path: &Path, state: &InstallState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create install state dir {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("failed to write install state {}", path.display()))?;
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn copy_executable(source: &Path, destination: &Path) -> Result<()> {
    if same_file(source, destination) {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create install destination dir {}",
                parent.display()
            )
        })?;
    }
    let temporary_path = destination.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::copy(source, &temporary_path).with_context(|| {
        format!(
            "failed to copy executable from {} to {}",
            source.display(),
            temporary_path.display()
        )
    })?;
    let permissions = fs::metadata(source)?.permissions();
    fs::set_permissions(&temporary_path, permissions)?;
    fs::rename(&temporary_path, destination).with_context(|| {
        format!(
            "failed to move executable into place at {}",
            destination.display()
        )
    })?;
    Ok(())
}

fn snapshot_binary(
    source: &Path,
    snapshot_dir: &Path,
    label: &str,
    source_label: &str,
) -> Result<InstallSnapshot> {
    fs::create_dir_all(snapshot_dir).with_context(|| {
        format!(
            "failed to create install snapshot dir {}",
            snapshot_dir.display()
        )
    })?;
    let id = Uuid::new_v4().to_string();
    let created_at_unix_ms = unix_time_ms();
    let file_name = format!(
        "{}-{}-{}",
        sanitize_path(label),
        created_at_unix_ms,
        sanitize_path(&id)
    );
    let path = snapshot_dir.join(file_name);
    copy_executable(source, &path)?;
    Ok(InstallSnapshot {
        id,
        label: label.to_owned(),
        created_at_unix_ms,
        path,
        source: source_label.to_owned(),
    })
}

pub(crate) fn install_binary_from_source(
    paths: &InstallPaths,
    source: &Path,
    label: &str,
    force: bool,
    action: &'static str,
) -> Result<InstallCommandReport> {
    if !source.exists() {
        bail!("install source does not exist: {}", source.display());
    }

    let same_destination =
        paths.destination_path.exists() && same_file(source, &paths.destination_path);
    if same_destination {
        let state = read_install_state(&paths.state_path)?;
        return Ok(InstallCommandReport {
            action,
            destination: paths.destination_path.clone(),
            source: source.display().to_string(),
            label: label.to_owned(),
            current_snapshot: state.current.as_ref().map(|snapshot| snapshot.path.clone()),
            previous_snapshot: None,
            skipped: !force,
        });
    }

    let mut state = read_install_state(&paths.state_path)?;
    let previous_snapshot = if paths.destination_path.exists() {
        let snapshot = snapshot_binary(
            &paths.destination_path,
            &paths.snapshot_dir,
            "rollback",
            "pre-install",
        )?;
        state.history.insert(0, snapshot.clone());
        Some(snapshot.path)
    } else {
        None
    };
    state.history.truncate(INSTALL_HISTORY_LIMIT);

    copy_executable(source, &paths.destination_path)?;
    let current_snapshot = snapshot_binary(
        &paths.destination_path,
        &paths.snapshot_dir,
        label,
        &source.display().to_string(),
    )?;
    state.current = Some(current_snapshot.clone());
    write_install_state(&paths.state_path, &state)?;

    Ok(InstallCommandReport {
        action,
        destination: paths.destination_path.clone(),
        source: source.display().to_string(),
        label: label.to_owned(),
        current_snapshot: Some(current_snapshot.path),
        previous_snapshot,
        skipped: false,
    })
}

fn render_install_report(report: &InstallCommandReport) -> String {
    let mut lines = vec![format!("{} complete", report.action.to_uppercase())];
    lines.push(format!("Destination: {}", report.destination.display()));
    lines.push(format!("Source: {}", report.source));
    lines.push(format!("Label: {}", report.label));
    if let Some(snapshot) = report.current_snapshot.as_ref() {
        lines.push(format!("Snapshot: {}", snapshot.display()));
    }
    if let Some(previous) = report.previous_snapshot.as_ref() {
        lines.push(format!("Previous snapshot: {}", previous.display()));
    }
    if report.skipped {
        lines.push("Status: already installed".to_owned());
    }
    lines.join("\n")
}

fn resolve_install_source(paths: &InstallPaths, target: Option<&str>) -> Result<(PathBuf, String)> {
    match target {
        None | Some("stable") | Some("latest") | Some("current") => Ok((
            env::current_exe().context("failed to resolve current executable for install")?,
            env!("CARGO_PKG_VERSION").to_owned(),
        )),
        Some(target) => {
            let target_path = PathBuf::from(target);
            if target_path.exists() {
                let label = target_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or(target)
                    .to_owned();
                return Ok((target_path, label));
            }

            let state = read_install_state(&paths.state_path)?;
            if let Some(snapshot) = state
                .history
                .iter()
                .chain(state.current.iter())
                .find(|snapshot| snapshot.label == target || snapshot.id == target)
            {
                return Ok((snapshot.path.clone(), snapshot.label.clone()));
            }

            bail!("install target '{target}' is not available locally")
        }
    }
}

pub(crate) fn render_install_command(args: &[String]) -> Result<String> {
    let mut target = None;
    let mut force = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--force" => force = true,
            value if value.starts_with('-') => bail!("unknown option: {value}"),
            value if target.is_none() => target = Some(value.to_owned()),
            value => bail!("unexpected argument for install: {value}"),
        }
        index += 1;
    }

    let paths = default_install_paths();
    let (source, label) = resolve_install_source(&paths, target.as_deref())?;
    let report = install_binary_from_source(&paths, &source, &label, force, "install")?;
    Ok(render_install_report(&report))
}

pub(crate) fn render_update_command() -> Result<String> {
    let paths = default_install_paths();
    let (source, label) = resolve_install_source(&paths, Some("current"))?;
    let report = install_binary_from_source(&paths, &source, &label, true, "update")?;
    Ok(render_install_report(&report))
}

fn rollback_selection(
    state: &InstallState,
    target: Option<&str>,
    safe: bool,
) -> Result<(usize, InstallSnapshot)> {
    if state.history.is_empty() {
        bail!("no rollback snapshots are available")
    }
    if safe {
        return Ok((0, state.history[0].clone()));
    }
    let target = target.unwrap_or("1");
    if let Ok(index) = target.parse::<usize>() {
        if index == 0 {
            bail!("rollback index must be greater than 0")
        }
        let history_index = index - 1;
        let snapshot = state
            .history
            .get(history_index)
            .cloned()
            .ok_or_else(|| anyhow!("no rollback snapshot available at index {index}"))?;
        return Ok((history_index, snapshot));
    }

    state
        .history
        .iter()
        .enumerate()
        .find(|(_, snapshot)| {
            snapshot.label == target
                || snapshot.id == target
                || snapshot
                    .path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value == target)
        })
        .map(|(index, snapshot)| (index, snapshot.clone()))
        .ok_or_else(|| anyhow!("no rollback snapshot matches '{target}'"))
}

pub(crate) fn rollback_from_state(
    paths: &InstallPaths,
    target: Option<&str>,
    dry_run: bool,
    safe: bool,
) -> Result<RollbackCommandReport> {
    let mut state = read_install_state(&paths.state_path)?;
    let available = state
        .history
        .iter()
        .enumerate()
        .map(|(index, snapshot)| {
            format!(
                "{}: {} ({})",
                index + 1,
                snapshot.label,
                snapshot.path.display()
            )
        })
        .collect::<Vec<_>>();
    let (index, selected) = rollback_selection(&state, target, safe)?;
    if dry_run {
        return Ok(RollbackCommandReport {
            action: "rollback",
            destination: paths.destination_path.clone(),
            restored_snapshot: Some(selected.path),
            dry_run: true,
            available,
        });
    }

    let previous_snapshot = if paths.destination_path.exists() {
        Some(snapshot_binary(
            &paths.destination_path,
            &paths.snapshot_dir,
            "rollback-previous",
            "pre-rollback",
        )?)
    } else {
        None
    };
    copy_executable(&selected.path, &paths.destination_path)?;
    let current_snapshot = snapshot_binary(
        &paths.destination_path,
        &paths.snapshot_dir,
        &selected.label,
        "rollback",
    )?;
    state.history.remove(index);
    if let Some(previous_snapshot) = previous_snapshot {
        state.history.insert(0, previous_snapshot);
    }
    state.history.truncate(INSTALL_HISTORY_LIMIT);
    state.current = Some(current_snapshot.clone());
    write_install_state(&paths.state_path, &state)?;

    Ok(RollbackCommandReport {
        action: "rollback",
        destination: paths.destination_path.clone(),
        restored_snapshot: Some(current_snapshot.path),
        dry_run: false,
        available,
    })
}

fn render_rollback_report(report: &RollbackCommandReport) -> String {
    let mut lines = vec![if report.dry_run {
        "Rollback preview".to_owned()
    } else {
        "Rollback complete".to_owned()
    }];
    lines.push(format!("Destination: {}", report.destination.display()));
    if let Some(path) = report.restored_snapshot.as_ref() {
        lines.push(format!("Selected snapshot: {}", path.display()));
    }
    if !report.available.is_empty() {
        lines.push(String::new());
        lines.push("Available snapshots:".to_owned());
        lines.extend(report.available.iter().map(|entry| format!("  {entry}")));
    }
    lines.join("\n")
}

pub(crate) fn render_rollback_command(args: &[String]) -> Result<String> {
    let mut target = None;
    let mut list = false;
    let mut dry_run = false;
    let mut safe = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "-l" | "--list" => list = true,
            "--dry-run" => dry_run = true,
            "--safe" => safe = true,
            value if value.starts_with('-') => bail!("unknown option: {value}"),
            value if target.is_none() => target = Some(value.to_owned()),
            value => bail!("unexpected argument for rollback: {value}"),
        }
        index += 1;
    }

    let paths = default_install_paths();
    let state = read_install_state(&paths.state_path)?;
    if list {
        let report = RollbackCommandReport {
            action: "rollback",
            destination: paths.destination_path.clone(),
            restored_snapshot: state.current.as_ref().map(|snapshot| snapshot.path.clone()),
            dry_run: true,
            available: state
                .history
                .iter()
                .enumerate()
                .map(|(index, snapshot)| {
                    format!(
                        "{}: {} ({})",
                        index + 1,
                        snapshot.label,
                        snapshot.path.display()
                    )
                })
                .collect(),
        };
        return Ok(render_rollback_report(&report));
    }

    let report = rollback_from_state(&paths, target.as_deref(), dry_run, safe)?;
    Ok(render_rollback_report(&report))
}
