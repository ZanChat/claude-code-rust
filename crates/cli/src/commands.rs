use super::*;

use anyhow::Context;
use std::collections::VecDeque;
use std::fs;
use std::io::{stdout, Write as _};
use std::process::{Command as StdCommand, Stdio};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardPath {
    Native,
    TmuxBuffer,
    Osc52,
}

fn should_enable_mouse_capture(term_program: Option<&str>) -> bool {
    !matches!(term_program, Some("vscode"))
}

pub(crate) async fn render_auth_command(provider: ApiProvider, action: &str) -> Result<String> {
    render_auth_command_with_resume(provider, action, None).await
}

pub(crate) fn resume_command_for_session(session_id: SessionId) -> String {
    format!("code-agent-rust --resume {session_id}")
}

pub(crate) async fn latest_resume_hint(
    store: &ActiveSessionStore,
) -> Result<Option<ResumeTargetHint>> {
    Ok(store
        .list_sessions()
        .await?
        .into_iter()
        .next()
        .map(|summary| ResumeTargetHint {
            session_id: summary.session_id,
            transcript_path: summary.transcript_path,
        }))
}

pub(crate) async fn current_resume_hint(
    store: &ActiveSessionStore,
    session_id: SessionId,
) -> Result<ResumeTargetHint> {
    Ok(ResumeTargetHint {
        session_id,
        transcript_path: store.transcript_path(session_id).await?,
    })
}

pub(crate) async fn render_auth_command_with_resume(
    provider: ApiProvider,
    action: &str,
    resume_hint: Option<ResumeTargetHint>,
) -> Result<String> {
    match action {
        "login" => {
            let resolver = EnvironmentAuthResolver;
            let auth = resolver
                .resolve_auth(AuthRequest {
                    provider,
                    profile: None,
                })
                .await?;
            let snapshot_path = if matches!(
                provider,
                ApiProvider::OpenAI
                    | ApiProvider::ChatGPTCodex
                    | ApiProvider::OpenAICompatible
                    | ApiProvider::FirstParty
            ) {
                Some(write_auth_snapshot(provider, &auth)?)
            } else {
                None
            };
            Ok(serde_json::to_string_pretty(&AuthCommandReport {
                provider: provider.to_string(),
                status: "ready".to_owned(),
                auth_source: auth.source,
                hint: Some(auth_hint_for_provider(provider)),
                snapshot_path,
                resume_session_id: None,
                resume_transcript_path: None,
                resume_command: None,
            })?)
        }
        "logout" => Ok(serde_json::to_string_pretty(&AuthCommandReport {
            provider: provider.to_string(),
            status: if clear_auth_snapshot(provider)? {
                "cleared".to_owned()
            } else {
                "no_snapshot".to_owned()
            },
            auth_source: None,
            hint: Some(auth_hint_for_provider(provider)),
            snapshot_path: Some(code_agent_auth_snapshot_path()),
            resume_session_id: resume_hint.as_ref().map(|hint| hint.session_id),
            resume_transcript_path: resume_hint
                .as_ref()
                .map(|hint| hint.transcript_path.clone()),
            resume_command: resume_hint
                .as_ref()
                .map(|hint| resume_command_for_session(hint.session_id)),
        })?),
        other => Err(anyhow!("unsupported auth action: {other}")),
    }
}

pub(crate) async fn render_memory_command(
    invocation: &CommandInvocation,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    provider: ApiProvider,
    model: Option<String>,
) -> Result<String> {
    let action = invocation
        .args
        .first()
        .map(String::as_str)
        .unwrap_or("read");
    let input = match action {
        "read" => json!({ "action": "read" }),
        "write" => json!({
            "action": "write",
            "value": invocation.args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ")
        }),
        other => bail!("unsupported memory action: {other}"),
    };
    let report = tool_registry
        .invoke(
            ToolCallRequest {
                tool_name: "memory".to_owned(),
                input,
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

pub(crate) async fn render_skills_command(
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
) -> Result<String> {
    let runtime = OutOfProcessPluginRuntime;
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    let skills = runtime.discover_skills(&root).await?;
    let commands = runtime.discover_commands(&root).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "root": root,
        "skills": skills,
        "commands": commands.into_iter().map(|spec| command_report(&spec)).collect::<Vec<_>>(),
    }))?)
}

pub(crate) fn render_command_help(registry: &CommandRegistry, remote_only: bool) -> String {
    let commands = if remote_only {
        registry.remote_safe()
    } else {
        registry.all()
    };
    let mut lines = vec!["REPL commands:".to_owned()];
    lines.extend(
        commands
            .into_iter()
            .map(|spec| format!("/{:<16} {}", spec.name, spec.description)),
    );
    lines.join("\n")
}

fn collect_recent_assistant_texts(messages: &[Message], max_items: usize) -> Vec<String> {
    messages
        .iter()
        .rev()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(message_text)
        .filter(|text| !text.trim().is_empty())
        .take(max_items)
        .collect()
}

fn try_copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return run_clipboard_command("pbcopy", &[], text).is_ok();
    }

    #[cfg(target_os = "windows")]
    {
        return run_clipboard_command("clip", &[], text).is_ok()
            || run_clipboard_command("cmd", &["/C", "clip"], text).is_ok();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            && run_clipboard_command("wl-copy", &[], text).is_ok()
        {
            return true;
        }
        if std::env::var_os("DISPLAY").is_some()
            && run_clipboard_command("xclip", &["-selection", "clipboard"], text).is_ok()
        {
            return true;
        }
        return run_clipboard_command("xsel", &["--clipboard", "--input"], text).is_ok();
    }

    #[allow(unreachable_code)]
    false
}

fn run_clipboard_command(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = StdCommand::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch clipboard helper: {program}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .with_context(|| format!("failed to write clipboard payload for {program}"))?;
    }

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for clipboard helper: {program}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("clipboard helper exited with status {status}")
    }
}

fn write_copy_fallback_file(text: &str) -> Result<PathBuf> {
    let copy_dir = std::env::temp_dir().join("code-agent-rust");
    fs::create_dir_all(&copy_dir)?;
    let file_path = copy_dir.join(format!("response-{}.md", Uuid::new_v4()));
    fs::write(&file_path, text)?;
    Ok(file_path)
}

fn wrap_for_multiplexer(sequence: &str) -> String {
    if std::env::var_os("TMUX").is_some() {
        return format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
    }
    if std::env::var_os("STY").is_some() {
        return format!("\x1bP{sequence}\x1b\\");
    }
    sequence.to_owned()
}

fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

fn emit_terminal_sequence(sequence: &str) -> Result<()> {
    let mut output = stdout();
    output.write_all(sequence.as_bytes())?;
    output.flush()?;
    Ok(())
}

fn tmux_load_buffer(text: &str) -> bool {
    if std::env::var_os("TMUX").is_none() {
        return false;
    }

    if std::env::var("LC_TERMINAL").ok().as_deref() == Some("iTerm2") {
        run_clipboard_command("tmux", &["load-buffer", "-"], text).is_ok()
    } else {
        run_clipboard_command("tmux", &["load-buffer", "-w", "-"], text).is_ok()
    }
}

fn copy_text_to_clipboard(text: &str) -> ClipboardPath {
    let local_session = std::env::var_os("SSH_CONNECTION").is_none();
    if local_session && try_copy_to_clipboard(text) {
        if std::env::var_os("TMUX").is_some() {
            let _ = tmux_load_buffer(text);
        }
        return ClipboardPath::Native;
    }

    let osc52 = osc52_sequence(text);
    if tmux_load_buffer(text) {
        let _ = emit_terminal_sequence(&wrap_for_multiplexer(&osc52));
        return ClipboardPath::TmuxBuffer;
    }

    let _ = emit_terminal_sequence(&wrap_for_multiplexer(&osc52));
    ClipboardPath::Osc52
}

fn clipboard_path_label(path: ClipboardPath) -> &'static str {
    match path {
        ClipboardPath::Native => "native clipboard",
        ClipboardPath::TmuxBuffer => "tmux buffer",
        ClipboardPath::Osc52 => "OSC 52",
    }
}

pub(crate) fn copy_text_with_fallback_notice(text: &str, label: &str) -> Result<String> {
    let clipboard_path = copy_text_to_clipboard(text);
    let file_path = write_copy_fallback_file(text)?;
    let line_count = text.lines().count().max(1);
    Ok(format!(
        "Copied {label} ({}, {} lines) via {}\nAlso wrote it to {}",
        text.chars().count(),
        line_count,
        clipboard_path_label(clipboard_path),
        file_path.display()
    ))
}

pub(crate) fn render_copy_command(
    invocation: &CommandInvocation,
    raw_messages: &[Message],
) -> Result<String> {
    let requested_index = match invocation.args.first() {
        None => 1,
        Some(value) => match value.parse::<usize>() {
            Ok(0) => return Ok("usage: /copy [N], where N starts at 1".to_owned()),
            Ok(index) => index,
            Err(_) => return Ok("usage: /copy [N], where N starts at 1".to_owned()),
        },
    };

    let recent_texts = collect_recent_assistant_texts(raw_messages, 20);
    let Some(text) = recent_texts.get(requested_index - 1) else {
        return Ok(if requested_index == 1 {
            "No assistant response available to copy yet.".to_owned()
        } else {
            format!("No assistant response found at index {requested_index}.")
        });
    };

    let response_label = if requested_index == 1 {
        "last assistant response".to_owned()
    } else {
        format!("assistant response #{requested_index}")
    };

    copy_text_with_fallback_notice(text, &response_label)
}

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

pub(crate) fn render_ide_command(
    ide_bridge_active: bool,
    ide_address: Option<&str>,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "connected": ide_bridge_active,
        "bridge_address": ide_address,
        "status": if ide_bridge_active { "connected" } else { "not_connected" },
        "message": if ide_bridge_active {
            "IDE bridge is active for this session."
        } else {
            "IDE auto-detection is not implemented yet in the Rust runtime. Connect an IDE bridge explicitly with --bridge-connect ide://HOST[:PORT] or --bridge-server ide://HOST[:PORT]."
        },
    }))?)
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

pub(crate) async fn handle_repl_slash_command(
    registry: &CommandRegistry,
    invocation: CommandInvocation,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: &mut String,
    repl_session: &mut ReplSessionState,
    raw_messages: &mut Vec<Message>,
    live_runtime: bool,
    vim_state: &mut code_agent_ui::vim::VimState,
    remote_mode: bool,
    ide_bridge_active: bool,
) -> Result<String> {
    if !command_allowed_in_repl(registry, remote_mode, &invocation.name) {
        return Ok(format!(
            "command '/{}' is unavailable in remote mode",
            invocation.name
        ));
    }
    match invocation.name.as_str() {
        "help" => Ok(render_command_help(registry, remote_mode)),
        "version" => Ok(env!("CARGO_PKG_VERSION").to_owned()),
        "config" => {
            if matches!(invocation.args.first().map(String::as_str), Some("migrate")) {
                Ok(serde_json::to_string_pretty(&config_migration_report(provider))?)
            } else {
                Ok(format!(
                    "provider={} model={} session={} runtime={}",
                    provider,
                    active_model,
                    repl_session.session_id,
                    if live_runtime { "live" } else { "offline" }
                ))
            }
        }
        "ide" => render_ide_command(ide_bridge_active, None),
        "model" => {
            let Some(model) = invocation.args.first() else {
                return Ok(format!("current model={active_model}"));
            };
            let catalog = compatibility_model_catalog(provider);
            if !matches!(provider, ApiProvider::OpenAICompatible)
                && catalog.get_model(model).is_none()
            {
                return Ok(format!("unknown compatibility model: {model}"));
            }
            *active_model = model.clone();
            Ok(format!("model switched to {active_model}"))
        }
        "compact" => {
            let estimated_tokens_before =
                estimate_message_tokens(&materialize_runtime_messages(raw_messages));
            let outcome = compact_messages(
                raw_messages,
                &CompactionConfig {
                    trigger: "manual".to_owned(),
                    max_tokens_before: Some(estimated_tokens_before),
                    target_tokens_after: compact_target_tokens(),
                    ..CompactionConfig::default()
                },
            );
            if let Some(outcome) = outcome {
                apply_compaction_outcome(store, repl_session.session_id, raw_messages, &outcome)
                    .await?;
                return Ok(format!(
                    "compacted {} messages to ~{} tokens",
                    outcome.summarized_message_count, outcome.estimated_tokens_after
                ));
            }
            Ok("nothing to compact".to_owned())
        }
        "copy" => render_copy_command(&invocation, raw_messages),
        "clear" => {
            let transcript_path = store.transcript_path(repl_session.session_id).await?;
            if transcript_path.exists() {
                fs::remove_file(&transcript_path)?;
            }
            raw_messages.clear();
            Ok(format!("cleared session {}", repl_session.session_id))
        }
        "resume" => {
            if let Some(target) = invocation.args.first() {
                let transcript_path =
                    resume_repl_session(store, repl_session, raw_messages, target).await?;
                Ok(format!(
                    "resumed {} ({})",
                    repl_session.session_id,
                    shorten_path(&transcript_path, 64)
                ))
            } else {
                Ok(serde_json::to_string_pretty(&store.list_sessions().await?)?)
            }
        }
        "session" => render_session_command(store, repl_session.session_id).await,
        "login" => render_auth_command(provider, "login").await,
        "logout" => {
            let resume_hint = current_resume_hint(store, repl_session.session_id).await?;
            render_auth_command_with_resume(provider, "logout", Some(resume_hint)).await
        }
        "permissions" => render_permissions_command(cwd).await,
        "plugin" => render_plugin_command(&invocation, plugin_root, cwd).await,
        "skills" => render_skills_command(cwd, plugin_root).await,
        "reload-plugins" => render_skills_command(cwd, plugin_root).await,
        "hooks" => render_simple_compat_command(
            "hooks",
            "Hook discovery is exposed through plugin manifests in the Rust runtime.",
        ),
        "output-style" => render_simple_compat_command(
            "output-style",
            "Output styles are discovered from plugin manifests but alternate renderers remain limited.",
        ),
        "mcp" => {
            render_mcp_command(
                &invocation,
                plugin_root,
                tool_registry,
                cwd,
                provider,
                Some(active_model.clone()),
            )
            .await
        }
        "memory" => {
            render_memory_command(&invocation, tool_registry, cwd, provider, Some(active_model.clone())).await
        }
        "files" => render_files_command(raw_messages, cwd),
        "diff" => render_diff_command(raw_messages),
        "usage" | "cost" | "stats" => render_usage_command(raw_messages),
        "status" => render_status_command(
            provider,
            active_model,
            repl_session.session_id,
            live_runtime,
            cwd,
        ),
        "statusline" => render_statusline_command(provider, active_model, repl_session.session_id),
        "theme" => render_theme_command(),
        "vim" => {
            vim_state.enabled = !vim_state.enabled;
            if vim_state.enabled {
                vim_state.enter_normal();
            } else {
                vim_state.mode = code_agent_ui::vim::VimMode::Insert;
            }
            render_vim_command(vim_state.enabled)
        }
        "plan" => render_plan_command(),
        "fast" => render_simple_compat_command(
            "fast",
            "Fast mode uses the same model family with lower latency-focused behavior.",
        ),
        "passes" => render_simple_compat_command(
            "passes",
            "Pass-count tuning is not yet modeled separately in the Rust runtime.",
        ),
        "effort" => render_simple_compat_command(
            "effort",
            "Reasoning effort tuning remains compatibility-surface only in the current build.",
        ),
        "remote-env" => render_simple_compat_command(
            "remote-env",
            "Remote environment reporting currently flows through bridge and session status surfaces.",
        ),
        "export" => render_export_command(store, repl_session.session_id),
        "tasks" => render_tasks_command(&invocation, cwd),
        "agents" => {
            render_agents_command(
                &invocation,
                tool_registry,
                cwd,
                provider,
                Some(active_model.clone()),
                repl_session.session_id,
            )
            .await
        }
        "remote-control" => {
            render_remote_control_command(
                registry,
                &invocation,
                &Cli::default(),
                store,
                tool_registry,
                cwd,
                provider,
                active_model,
                repl_session.session_id,
                raw_messages,
                live_runtime,
            )
            .await
        }
        "voice" => Ok("voice features are intentionally deferred in this build".to_owned()),
        "exit" | "quit" => Ok("exit".to_owned()),
        other => Err(anyhow!("unknown registered REPL command: {other}")),
    }
}

enum ReplSubmissionOutcome {
    Continue,
    Exit,
}

async fn process_repl_submission(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    store: &ActiveSessionStore,
    registry: &code_agent_core::CommandRegistry,
    tool_registry: &ToolRegistry,
    cwd: &PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: &mut String,
    repl_session: &mut ReplSessionState,
    raw_messages: &mut Vec<Message>,
    live_runtime: bool,
    prompt_text: String,
    input_buffer: &mut code_agent_ui::InputBuffer,
    prompt_history: &mut Vec<String>,
    prompt_history_index: &mut Option<usize>,
    prompt_history_draft: &mut Option<code_agent_ui::InputBuffer>,
    transcript_scroll: &mut u16,
    status_line: &mut String,
    status_marquee_tick: &mut usize,
    active_pane: &mut PaneKind,
    compact_banner: &mut Option<String>,
    interaction_state: &mut ReplInteractionState,
    resume_picker: &mut Option<ResumePickerState>,
    selected_command_suggestion: &mut usize,
    vim_state: &mut code_agent_ui::vim::VimState,
    remote_mode: bool,
    ide_bridge_active: bool,
    queued_submissions: &mut VecDeque<String>,
) -> Result<ReplSubmissionOutcome> {
    if prompt_text.trim().is_empty() {
        return Ok(ReplSubmissionOutcome::Continue);
    }
    if should_exit_repl(&prompt_text) {
        return Ok(ReplSubmissionOutcome::Exit);
    }

    push_prompt_history_entry(prompt_history, &prompt_text);
    reset_prompt_history_navigation(prompt_history_index, prompt_history_draft);
    *selected_command_suggestion = 0;
    *compact_banner = None;

    if let Some(invocation) = registry.parse_slash_command(&prompt_text) {
        if invocation.name == "resume" && invocation.args.is_empty() {
            let sessions =
                resumable_sessions(store.list_sessions().await?, repl_session.session_id);
            if sessions.is_empty() {
                *status_line = status_with_detail(
                    repl_status(provider, active_model, repl_session.session_id),
                    "No conversations found to resume",
                );
            } else {
                *resume_picker = Some(ResumePickerState {
                    sessions,
                    selected: 0,
                });
                *status_line = repl_status(provider, active_model, repl_session.session_id);
            }
            *status_marquee_tick = 0;
            return Ok(ReplSubmissionOutcome::Continue);
        }

        let command_name = invocation.name.clone();
        let command_input = invocation.raw_input.clone();
        let command_recorded = should_record_repl_command(&command_name);
        if command_recorded {
            append_session_message(
                store,
                raw_messages,
                build_repl_command_input_message(
                    repl_session.session_id,
                    raw_messages.last().map(|message| message.id),
                    command_input.clone(),
                ),
            )
            .await?;
        }

        let previous_session_id = repl_session.session_id;
        let base_status_line = repl_status(provider, active_model, repl_session.session_id);
        let preview_messages = if command_recorded {
            materialize_runtime_messages(raw_messages)
        } else {
            materialize_runtime_messages(&optimistic_messages_for_command(
                raw_messages,
                repl_session.session_id,
                &command_input,
            ))
        };
        let pending_view = Arc::new(Mutex::new(PendingReplView::new(
            preview_messages,
            format!("running {command_name}"),
        )));
        let active_model_display = active_model.clone();
        let mut pending_vim_state = vim_state.clone();
        let result = run_pending_repl_operation(
            terminal,
            registry,
            pending_view.clone(),
            cwd,
            provider,
            &active_model_display,
            repl_session.session_id,
            input_buffer,
            &base_status_line,
            active_pane,
            compact_banner.clone(),
            transcript_scroll,
            selected_command_suggestion,
            &mut pending_vim_state,
            interaction_state,
            handle_repl_slash_command(
                registry,
                invocation,
                store,
                tool_registry,
                cwd,
                plugin_root,
                provider,
                active_model,
                repl_session,
                raw_messages,
                live_runtime,
                vim_state,
                remote_mode,
                ide_bridge_active,
            ),
        )
        .await;
        queued_submissions.extend(take_pending_repl_inputs(&pending_view));

        match result {
            Ok(PendingReplOperationResult::Completed(next_status)) if next_status == "exit" => {
                return Ok(ReplSubmissionOutcome::Exit)
            }
            Ok(PendingReplOperationResult::Completed(next_status)) => {
                if command_recorded {
                    append_session_message(
                        store,
                        raw_messages,
                        build_repl_command_output_message(
                            repl_session.session_id,
                            raw_messages.last().map(|message| message.id),
                            &command_name,
                            next_status.clone(),
                        ),
                    )
                    .await?;
                }
                if repl_session.session_id != previous_session_id {
                    *prompt_history = prompt_history_from_messages(raw_messages);
                    reset_prompt_history_navigation(prompt_history_index, prompt_history_draft);
                    *transcript_scroll = 0;
                    *compact_banner = repl_session
                        .transcript_path
                        .as_ref()
                        .map(|path| format!("resume {}", shorten_path(path, 72)));
                }
                *status_line = slash_command_footer_status(
                    provider,
                    active_model,
                    repl_session.session_id,
                    &command_name,
                    command_recorded,
                    false,
                    &next_status,
                );
                *status_marquee_tick = 0;
                if next_status.starts_with("compacted ") {
                    *compact_banner = Some(next_status.clone());
                }
            }
            Ok(PendingReplOperationResult::Interrupted) => {
                let interruption_messages = pending_interrupt_messages(
                    repl_session.session_id,
                    raw_messages,
                    &pending_repl_snapshot(&pending_view),
                );
                append_session_messages(store, raw_messages, interruption_messages).await?;
                *status_line = status_with_detail(
                    repl_status(provider, active_model, repl_session.session_id),
                    "Interrupted by user",
                );
                *status_marquee_tick = 0;
            }
            Err(error) => {
                let error_detail = format!("error: {error}");
                *status_line = slash_command_footer_status(
                    provider,
                    active_model,
                    repl_session.session_id,
                    &command_name,
                    command_recorded,
                    true,
                    &error_detail,
                );
                if command_recorded {
                    append_session_message(
                        store,
                        raw_messages,
                        build_repl_command_output_message(
                            repl_session.session_id,
                            raw_messages.last().map(|message| message.id),
                            &command_name,
                            format!("error: {error}"),
                        ),
                    )
                    .await?;
                }
                *status_marquee_tick = 0;
            }
        }

        return Ok(ReplSubmissionOutcome::Continue);
    }

    let base_status_line = repl_status(provider, active_model, repl_session.session_id);
    let preview_messages = materialize_runtime_messages(&optimistic_messages_for_prompt(
        raw_messages,
        repl_session.session_id,
        &prompt_text,
    ));
    let pending_view = Arc::new(Mutex::new(PendingReplView::new(
        preview_messages,
        "waiting for response",
    )));
    let result = run_pending_repl_operation(
        terminal,
        registry,
        pending_view.clone(),
        cwd,
        provider,
        active_model,
        repl_session.session_id,
        input_buffer,
        &base_status_line,
        active_pane,
        compact_banner.clone(),
        transcript_scroll,
        selected_command_suggestion,
        vim_state,
        interaction_state,
        execute_local_turn(
            store,
            tool_registry,
            cwd.clone(),
            provider,
            active_model.clone(),
            repl_session.session_id,
            raw_messages,
            prompt_text,
            live_runtime,
            Some(pending_view.clone()),
        ),
    )
    .await;
    queued_submissions.extend(take_pending_repl_inputs(&pending_view));

    match result {
        Ok(PendingReplOperationResult::Completed((
            applied_compaction,
            turn_count,
            stop_reason,
            _,
            _,
        ))) => {
            *compact_banner = applied_compaction.as_ref().and_then(|outcome| {
                compaction_kind_name(outcome).map(|kind| format!("compacted {kind}"))
            });
            let detail =
                if let Some(kind) = applied_compaction.as_ref().and_then(compaction_kind_name) {
                    format!("{turn_count} steps · {:?} · compact {kind}", stop_reason)
                } else {
                    format!("{turn_count} steps · {:?}", stop_reason)
                };
            *status_line = status_with_detail(
                repl_status(provider, active_model, repl_session.session_id),
                detail,
            );
            *status_marquee_tick = 0;
        }
        Err(error) => {
            *status_line = status_with_detail(
                repl_status(provider, active_model, repl_session.session_id),
                format!("error: {error}"),
            );
            *status_marquee_tick = 0;
        }
        Ok(PendingReplOperationResult::Interrupted) => {
            let interruption_messages = pending_interrupt_messages(
                repl_session.session_id,
                raw_messages,
                &pending_repl_snapshot(&pending_view),
            );
            append_session_messages(store, raw_messages, interruption_messages).await?;
            *status_line = status_with_detail(
                repl_status(provider, active_model, repl_session.session_id),
                "Interrupted by user",
            );
            *status_marquee_tick = 0;
        }
    }

    Ok(ReplSubmissionOutcome::Continue)
}

pub(crate) async fn run_interactive_repl(
    store: &ActiveSessionStore,
    registry: &code_agent_core::CommandRegistry,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    live_runtime: bool,
    auth_source: Option<String>,
    transcript_path: Option<PathBuf>,
    remote_mode: bool,
    ide_bridge_active: bool,
) -> Result<SessionId> {
    let mut active_model = active_model;
    let mut repl_session = ReplSessionState {
        session_id,
        transcript_path,
    };
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut out = stdout();
    let mouse_capture_enabled =
        should_enable_mouse_capture(std::env::var("TERM_PROGRAM").ok().as_deref());
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, Hide)?;
    if mouse_capture_enabled {
        execute!(out, EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut startup_preferences = load_startup_preferences();
    let startup_screens = build_startup_screens(
        provider,
        &active_model,
        repl_session.session_id,
        &cwd,
        store.root_dir(),
        repl_session.transcript_path.as_deref(),
        live_runtime,
        auth_source.as_deref(),
        &startup_preferences,
    );
    let mut initial_input_buffer = code_agent_ui::InputBuffer::new();
    if !startup_screens.is_empty() {
        initial_input_buffer = run_startup_flow(
            &mut terminal,
            provider,
            &active_model,
            repl_session.session_id,
            &cwd,
            &startup_screens,
        )?;
        if !startup_preferences.welcome_seen {
            startup_preferences.welcome_seen = true;
            save_startup_preferences(&startup_preferences)?;
        }
    }

    let loop_result = async {
        let mut input_buffer = initial_input_buffer;
        let mut prompt_history = prompt_history_from_messages(raw_messages);
        let mut prompt_history_index = None;
        let mut prompt_history_draft: Option<code_agent_ui::InputBuffer> = None;
        let mut transcript_scroll = 0u16;
        let mut status_line = repl_status(provider, &active_model, repl_session.session_id);
        let mut status_marquee_tick = 0usize;
        let mut active_pane = PaneKind::Transcript;
        let mut selected_command_suggestion = 0usize;
        let mut compact_banner = None;
        let mut resume_picker = None;
        let mut queued_submissions = VecDeque::new();
        let mut interaction_state = ReplInteractionState::default();
        let mut dirty = true;
        loop {
            if dirty {
                draw_repl_state(
                    &mut terminal,
                    registry,
                    raw_messages,
                    None,
                    &cwd,
                    provider,
                    &active_model,
                    repl_session.session_id,
                    &input_buffer,
                    &status_line,
                    None,
                    active_pane,
                    compact_banner.clone(),
                    transcript_scroll,
                    resume_picker.as_ref().map(build_resume_choice_list),
                    &mut selected_command_suggestion,
                    &vim_state,
                    status_marquee_tick,
                    &interaction_state,
                )?;
                dirty = false;
            }

            if resume_picker.is_none() {
                if let Some(prompt_text) = queued_submissions.pop_front() {
                    match process_repl_submission(
                        &mut terminal,
                        store,
                        registry,
                        tool_registry,
                        &cwd,
                        plugin_root,
                        provider,
                        &mut active_model,
                        &mut repl_session,
                        raw_messages,
                        live_runtime,
                        prompt_text,
                        &mut input_buffer,
                        &mut prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                        &mut transcript_scroll,
                        &mut status_line,
                        &mut status_marquee_tick,
                        &mut active_pane,
                        &mut compact_banner,
                        &mut interaction_state,
                        &mut resume_picker,
                        &mut selected_command_suggestion,
                        &mut vim_state,
                        remote_mode,
                        ide_bridge_active,
                        &mut queued_submissions,
                    )
                    .await?
                    {
                        ReplSubmissionOutcome::Continue => {
                            dirty = true;
                            continue;
                        }
                        ReplSubmissionOutcome::Exit => break,
                    }
                }
            }

            let event = if status_line_needs_marquee(&status_line) {
                if !event::poll(Duration::from_millis(160))? {
                    status_marquee_tick = status_marquee_tick.wrapping_add(1);
                    dirty = true;
                    continue;
                }
                event::read()?
            } else {
                event::read()?
            };
            if let Event::Resize(width, height) = event {
                terminal.resize(Rect::new(0, 0, width, height))?;
                dirty = true;
                continue;
            }
            if let Event::Mouse(mouse) = event {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        interaction_state.transcript_selection = None;
                        scroll_up(&mut transcript_scroll, 3);
                        dirty = true;
                    }
                    MouseEventKind::ScrollDown => {
                        interaction_state.transcript_selection = None;
                        scroll_down(&mut transcript_scroll, 3);
                        dirty = true;
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(action) = repl_mouse_action(
                            &terminal,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            resume_picker.as_ref().map(build_resume_choice_list),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &mouse,
                            &interaction_state,
                        )? {
                            match action {
                                UiMouseAction::JumpToBottom => transcript_scroll = 0,
                                UiMouseAction::ToggleTranscriptGroup(_) => {}
                            }
                            dirty = true;
                        }
                    }
                    _ => {}
                }
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if resume_picker.is_some() {
                enum ResumePickerAction {
                    Close,
                    Resume(SessionSummary),
                }

                let mut picker_action = None;
                if let Some(picker) = resume_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc => {
                            picker_action = Some(ResumePickerAction::Close);
                            dirty = true;
                        }
                        KeyCode::Up => {
                            picker.selected = picker.selected.saturating_sub(1);
                            dirty = true;
                        }
                        KeyCode::Down => {
                            if picker.selected + 1 < picker.sessions.len() {
                                picker.selected += 1;
                            }
                            dirty = true;
                        }
                        KeyCode::PageUp => {
                            picker.selected = picker.selected.saturating_sub(5);
                            dirty = true;
                        }
                        KeyCode::PageDown => {
                            if !picker.sessions.is_empty() {
                                picker.selected =
                                    (picker.selected + 5).min(picker.sessions.len() - 1);
                            }
                            dirty = true;
                        }
                        KeyCode::Home => {
                            picker.selected = 0;
                            dirty = true;
                        }
                        KeyCode::End => {
                            if !picker.sessions.is_empty() {
                                picker.selected = picker.sessions.len() - 1;
                            }
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            picker_action = picker
                                .sessions
                                .get(picker.selected)
                                .cloned()
                                .map(ResumePickerAction::Resume);
                            dirty = true;
                        }
                        _ => {}
                    }
                }

                match picker_action {
                    Some(ResumePickerAction::Close) => {
                        resume_picker = None;
                        status_line = repl_status(provider, &active_model, repl_session.session_id);
                        status_marquee_tick = 0;
                    }
                    Some(ResumePickerAction::Resume(summary)) => {
                        resume_picker = None;
                        let previous_session_id = repl_session.session_id;
                        let transcript_path = resume_repl_session(
                            store,
                            &mut repl_session,
                            raw_messages,
                            &summary.session_id.to_string(),
                        )
                        .await?;
                        if repl_session.session_id != previous_session_id {
                            prompt_history = prompt_history_from_messages(raw_messages);
                            reset_prompt_history_navigation(
                                &mut prompt_history_index,
                                &mut prompt_history_draft,
                            );
                            transcript_scroll = 0;
                        }
                        compact_banner =
                            Some(format!("resume {}", shorten_path(&transcript_path, 72)));
                        status_line = repl_status(provider, &active_model, repl_session.session_id);
                        status_marquee_tick = 0;
                    }
                    None => {}
                }
                continue;
            }

            if matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                if interaction_state.transcript_search.open {
                    cancel_transcript_search(
                        &mut interaction_state.transcript_search,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                    continue;
                }
                if interaction_state.message_actions.is_some() {
                    interaction_state.message_actions = None;
                    dirty = true;
                    continue;
                }
                if interaction_state.transcript_selection.is_some() {
                    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                    let state = build_repl_ui_state(
                        &app,
                        registry,
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    if let Some(text) =
                        transcript_selection_copy_text(&state, size.width, &interaction_state)
                    {
                        compact_banner = Some(
                            copy_text_with_fallback_notice(&text, "selection")
                                .unwrap_or_else(|error| format!("Copy failed: {error}")),
                        );
                    }
                    interaction_state.transcript_selection = None;
                    dirty = true;
                    continue;
                }
                if interaction_state.transcript_mode {
                    exit_transcript_mode(&mut interaction_state);
                    dirty = true;
                    continue;
                }
                break;
            }

            if matches!(key.code, KeyCode::Up)
                && key.modifiers == KeyModifiers::SHIFT
                && interaction_state.message_actions.is_none()
                && interaction_state.transcript_selection.is_none()
                && !interaction_state.transcript_search.open
            {
                let runtime_messages = materialize_runtime_messages(raw_messages);
                let message_action_items =
                    message_action_items_from_runtime(&runtime_messages, None);
                if enter_message_actions(&mut interaction_state, &message_action_items) {
                    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                    let state = build_repl_ui_state(
                        &app,
                        registry,
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    sync_message_action_preview(
                        &state,
                        size.width,
                        size.height,
                        &interaction_state,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                }
                continue;
            }

            if interaction_state.message_actions.is_some() {
                let runtime_messages = materialize_runtime_messages(raw_messages);
                let message_action_items =
                    message_action_items_from_runtime(&runtime_messages, None);
                if selected_message_action_item(&mut interaction_state, &message_action_items)
                    .is_none()
                {
                    interaction_state.message_actions = None;
                    dirty = true;
                    continue;
                }

                let mut selection_changed = false;
                match key.code {
                    KeyCode::Esc => {
                        interaction_state.message_actions = None;
                        dirty = true;
                    }
                    KeyCode::Enter => {
                        let prompt_text = selected_message_action_item(
                            &mut interaction_state,
                            &message_action_items,
                        )
                        .and_then(|item| {
                            (item.message.role == MessageRole::User)
                                .then(|| message_text(&item.message))
                        });
                        if let Some(prompt_text) =
                            prompt_text.filter(|text| !text.trim().is_empty())
                        {
                            input_buffer.replace(prompt_text);
                            interaction_state.message_actions = None;
                            if interaction_state.transcript_mode {
                                exit_transcript_mode(&mut interaction_state);
                            }
                            dirty = true;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.is_empty() => {
                        if let Some(text) = selected_message_action_item(
                            &mut interaction_state,
                            &message_action_items,
                        )
                        .and_then(|item| message_action_copy_text(&item.message))
                        {
                            compact_banner = Some(
                                copy_text_with_fallback_notice(&text, "message")
                                    .unwrap_or_else(|error| format!("Copy failed: {error}")),
                            );
                        }
                        interaction_state.message_actions = None;
                        dirty = true;
                    }
                    KeyCode::Char('p') if key.modifiers.is_empty() => {
                        if let Some(primary_input) = selected_message_action_item(
                            &mut interaction_state,
                            &message_action_items,
                        )
                        .and_then(|item| message_primary_input(&item.message))
                        {
                            compact_banner = Some(
                                copy_text_with_fallback_notice(
                                    &primary_input.value,
                                    primary_input.label,
                                )
                                .unwrap_or_else(|error| format!("Copy failed: {error}")),
                            );
                        }
                        interaction_state.message_actions = None;
                        dirty = true;
                    }
                    KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::PrevUser,
                        );
                    }
                    KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::NextUser,
                        );
                    }
                    KeyCode::Up => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Prev,
                        );
                    }
                    KeyCode::Down => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Next,
                        );
                    }
                    KeyCode::Char('k') if key.modifiers.is_empty() => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Prev,
                        );
                    }
                    KeyCode::Char('j') if key.modifiers.is_empty() => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Next,
                        );
                    }
                    KeyCode::Home => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Top,
                        );
                    }
                    KeyCode::End => {
                        selection_changed = move_message_action_selection(
                            &mut interaction_state,
                            &message_action_items,
                            ReplMessageActionNavigation::Bottom,
                        );
                    }
                    _ => {}
                }

                if selection_changed {
                    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                    let state = build_repl_ui_state(
                        &app,
                        registry,
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    sync_message_action_preview(
                        &state,
                        size.width,
                        size.height,
                        &interaction_state,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                }

                continue;
            }

            if interaction_state.transcript_mode {
                if interaction_state.transcript_search.open {
                    match key.code {
                        KeyCode::Esc => {
                            cancel_transcript_search(
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            interaction_state.transcript_search.open = false;
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            if interaction_state.transcript_search.active_item.is_none() {
                                interaction_state.transcript_search.reset();
                            }
                            dirty = true;
                        }
                        KeyCode::Left => {
                            interaction_state.transcript_search.input_buffer.cursor =
                                interaction_state
                                    .transcript_search
                                    .input_buffer
                                    .cursor
                                    .saturating_sub(1);
                            dirty = true;
                        }
                        KeyCode::Right => {
                            let input = &mut interaction_state.transcript_search.input_buffer;
                            input.cursor = (input.cursor + 1).min(input.chars.len());
                            dirty = true;
                        }
                        KeyCode::Backspace => {
                            interaction_state.transcript_search.input_buffer.pop();
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                        KeyCode::Char(ch)
                            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                        {
                            interaction_state.transcript_search.input_buffer.push(ch);
                            let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                            let state = build_repl_ui_state(
                                &app,
                                registry,
                                raw_messages,
                                None,
                                &cwd,
                                provider,
                                &active_model,
                                repl_session.session_id,
                                &input_buffer,
                                &status_line,
                                None,
                                active_pane,
                                compact_banner.clone(),
                                transcript_scroll,
                                None,
                                command_suggestions(registry, &input_buffer),
                                selected_command_suggestion,
                                status_marquee_tick,
                                &interaction_state,
                            );
                            let size = terminal.size()?;
                            sync_transcript_search_preview(
                                &state,
                                size.width,
                                size.height,
                                &mut interaction_state.transcript_search,
                                &mut transcript_scroll,
                            );
                            dirty = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Some(selection_move) = transcript_selection_move_for_key(
                    &key,
                    interaction_state.transcript_selection.is_some(),
                ) {
                    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                    let state = build_repl_ui_state(
                        &app,
                        registry,
                        raw_messages,
                        None,
                        &cwd,
                        provider,
                        &active_model,
                        repl_session.session_id,
                        &input_buffer,
                        &status_line,
                        None,
                        active_pane,
                        compact_banner.clone(),
                        transcript_scroll,
                        None,
                        command_suggestions(registry, &input_buffer),
                        selected_command_suggestion,
                        status_marquee_tick,
                        &interaction_state,
                    );
                    let size = terminal.size()?;
                    let selectable_lines = transcript_selectable_lines_for_view(&state, size.width);
                    let _ = move_transcript_selection(
                        &mut interaction_state,
                        &selectable_lines,
                        selection_move,
                    );
                    sync_transcript_selection_preview(
                        &state,
                        size.width,
                        size.height,
                        &interaction_state,
                        &mut transcript_scroll,
                    );
                    dirty = true;
                    continue;
                }

                if interaction_state.transcript_selection.is_some() {
                    if matches!(key.code, KeyCode::Esc) {
                        interaction_state.transcript_selection = None;
                        dirty = true;
                        continue;
                    }
                    if should_clear_transcript_selection_on_key(&key) {
                        interaction_state.transcript_selection = None;
                        dirty = true;
                    }
                }

                if matches!(key.code, KeyCode::Char('o'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    exit_transcript_mode(&mut interaction_state);
                    dirty = true;
                    continue;
                }

                match key.code {
                    KeyCode::Esc => {
                        exit_transcript_mode(&mut interaction_state);
                        dirty = true;
                    }
                    KeyCode::Char('q') if key.modifiers.is_empty() => {
                        exit_transcript_mode(&mut interaction_state);
                        dirty = true;
                    }
                    KeyCode::Char('/')
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        interaction_state.message_actions = None;
                        open_transcript_search(
                            &mut interaction_state.transcript_search,
                            transcript_scroll,
                        );
                        dirty = true;
                    }
                    KeyCode::Char('n') if key.modifiers.is_empty() => {
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        if step_transcript_search_match(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            &mut transcript_scroll,
                            false,
                        ) {
                            dirty = true;
                        }
                    }
                    KeyCode::Char('N')
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        let app = RatatuiApp::new(format!("{provider}  {active_model}"));
                        let state = build_repl_ui_state(
                            &app,
                            registry,
                            raw_messages,
                            None,
                            &cwd,
                            provider,
                            &active_model,
                            repl_session.session_id,
                            &input_buffer,
                            &status_line,
                            None,
                            active_pane,
                            compact_banner.clone(),
                            transcript_scroll,
                            None,
                            command_suggestions(registry, &input_buffer),
                            selected_command_suggestion,
                            status_marquee_tick,
                            &interaction_state,
                        );
                        let size = terminal.size()?;
                        if step_transcript_search_match(
                            &state,
                            size.width,
                            size.height,
                            &mut interaction_state.transcript_search,
                            &mut transcript_scroll,
                            true,
                        ) {
                            dirty = true;
                        }
                    }
                    KeyCode::Up => {
                        scroll_up(&mut transcript_scroll, 1);
                        dirty = true;
                    }
                    KeyCode::Down => {
                        scroll_down(&mut transcript_scroll, 1);
                        dirty = true;
                    }
                    KeyCode::PageUp => {
                        scroll_up(&mut transcript_scroll, 5);
                        dirty = true;
                    }
                    KeyCode::PageDown => {
                        scroll_down(&mut transcript_scroll, 5);
                        dirty = true;
                    }
                    KeyCode::Home => {
                        transcript_scroll = u16::MAX;
                        dirty = true;
                    }
                    KeyCode::End => {
                        transcript_scroll = 0;
                        dirty = true;
                    }
                    _ => {}
                }
                continue;
            }

            if matches!(key.code, KeyCode::Char('o'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                enter_transcript_mode(&mut interaction_state, &mut active_pane);
                dirty = true;
                continue;
            }

            if let Some(pane) = pane_from_shortcut(&key) {
                active_pane = pane;
                dirty = true;
                continue;
            }

            match key.code {
                KeyCode::Esc => {
                    if vim_state.enabled {
                        if matches!(vim_state.mode, code_agent_ui::vim::VimMode::Insert) {
                            vim_state.enter_normal();
                        } else {
                            vim_state.mode = code_agent_ui::vim::VimMode::Normal(
                                code_agent_ui::vim::CommandState::Idle,
                            );
                        }
                        dirty = true;
                    }
                }
                KeyCode::Tab => {
                    active_pane = rotate_pane(active_pane, true);
                    dirty = true;
                }
                KeyCode::BackTab => {
                    active_pane = rotate_pane(active_pane, false);
                    dirty = true;
                }
                KeyCode::Up => {
                    let suggestions = sync_command_selection(
                        registry,
                        &input_buffer,
                        &mut selected_command_suggestion,
                    );
                    if suggestions.len() > 1 {
                        selected_command_suggestion = if selected_command_suggestion == 0 {
                            suggestions.len() - 1
                        } else {
                            selected_command_suggestion - 1
                        };
                    } else {
                        navigate_prompt_history_up(
                            &prompt_history,
                            &mut input_buffer,
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                    }
                    dirty = true;
                }
                KeyCode::Down => {
                    let suggestions = sync_command_selection(
                        registry,
                        &input_buffer,
                        &mut selected_command_suggestion,
                    );
                    if suggestions.len() > 1 {
                        selected_command_suggestion =
                            (selected_command_suggestion + 1) % suggestions.len();
                    } else {
                        navigate_prompt_history_down(
                            &prompt_history,
                            &mut input_buffer,
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                    }
                    dirty = true;
                }
                KeyCode::PageUp => {
                    scroll_up(&mut transcript_scroll, 5);
                    dirty = true;
                }
                KeyCode::PageDown => {
                    scroll_down(&mut transcript_scroll, 5);
                    dirty = true;
                }
                KeyCode::Home => {
                    transcript_scroll = u16::MAX;
                    dirty = true;
                }
                KeyCode::End => {
                    transcript_scroll = 0;
                    dirty = true;
                }
                KeyCode::Left if vim_state.is_insert() => {
                    input_buffer.cursor = input_buffer.cursor.saturating_sub(1);
                    dirty = true;
                }
                KeyCode::Right if vim_state.is_insert() => {
                    input_buffer.cursor = (input_buffer.cursor + 1).min(input_buffer.chars.len());
                    dirty = true;
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if vim_state.is_insert() {
                        reset_prompt_history_navigation(
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                        input_buffer.push(ch);
                        selected_command_suggestion = 0;
                        dirty = true;
                    } else {
                        if let code_agent_ui::vim::VimMode::Normal(ref mut cmd_state) =
                            vim_state.mode
                        {
                            let transition = code_agent_ui::vim::handle_normal_key(cmd_state, ch);
                            match transition {
                                code_agent_ui::vim::VimTransition::EnterInsert => {
                                    vim_state.enter_insert();
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::MoveCursor(delta) => {
                                    let mut new_pos = input_buffer.cursor as isize + delta;
                                    if new_pos < 0 {
                                        new_pos = 0;
                                    }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    if new_pos > max_pos as isize && !input_buffer.is_empty() {
                                        new_pos = max_pos as isize;
                                    }
                                    input_buffer.cursor = new_pos as usize;
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::SetCursor(pos) => {
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = pos.min(max_pos);
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::DeleteChars(mut amount) => {
                                    reset_prompt_history_navigation(
                                        &mut prompt_history_index,
                                        &mut prompt_history_draft,
                                    );
                                    while amount > 0
                                        && input_buffer.cursor < input_buffer.chars.len()
                                    {
                                        input_buffer.chars.remove(input_buffer.cursor);
                                        amount -= 1;
                                    }
                                    let max_pos = input_buffer.chars.len().saturating_sub(1);
                                    input_buffer.cursor = input_buffer.cursor.min(max_pos);
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::ReplaceChar(r) => {
                                    reset_prompt_history_navigation(
                                        &mut prompt_history_index,
                                        &mut prompt_history_draft,
                                    );
                                    if input_buffer.cursor < input_buffer.chars.len() {
                                        input_buffer.chars[input_buffer.cursor] = r;
                                    }
                                    dirty = true;
                                }
                                code_agent_ui::vim::VimTransition::None => {}
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    let suggestions = sync_command_selection(
                        registry,
                        &input_buffer,
                        &mut selected_command_suggestion,
                    );
                    let prompt_text = input_buffer.as_str().trim().to_owned();
                    if prompt_text.is_empty() {
                        continue;
                    }
                    if let Some(selected) = suggestions.get(selected_command_suggestion) {
                        let selected_name = selected.name.as_str();
                        if prompt_text.starts_with('/')
                            && !prompt_text.contains(char::is_whitespace)
                            && prompt_text != selected_name
                        {
                            reset_prompt_history_navigation(
                                &mut prompt_history_index,
                                &mut prompt_history_draft,
                            );
                            apply_selected_command(&mut input_buffer, selected);
                            dirty = true;
                            continue;
                        }
                    }
                    input_buffer.clear();
                    match process_repl_submission(
                        &mut terminal,
                        store,
                        registry,
                        tool_registry,
                        &cwd,
                        plugin_root,
                        provider,
                        &mut active_model,
                        &mut repl_session,
                        raw_messages,
                        live_runtime,
                        prompt_text,
                        &mut input_buffer,
                        &mut prompt_history,
                        &mut prompt_history_index,
                        &mut prompt_history_draft,
                        &mut transcript_scroll,
                        &mut status_line,
                        &mut status_marquee_tick,
                        &mut active_pane,
                        &mut compact_banner,
                        &mut interaction_state,
                        &mut resume_picker,
                        &mut selected_command_suggestion,
                        &mut vim_state,
                        remote_mode,
                        ide_bridge_active,
                        &mut queued_submissions,
                    )
                    .await?
                    {
                        ReplSubmissionOutcome::Continue => {}
                        ReplSubmissionOutcome::Exit => break,
                    }
                    dirty = true;
                }
                KeyCode::Backspace => {
                    if vim_state.is_insert() {
                        reset_prompt_history_navigation(
                            &mut prompt_history_index,
                            &mut prompt_history_draft,
                        );
                        input_buffer.pop();
                        selected_command_suggestion = 0;
                    } else {
                        if input_buffer.cursor > 0 {
                            input_buffer.cursor -= 1;
                        }
                    }
                    dirty = true;
                }
                _ => {}
            }
        }

        Ok::<SessionId, anyhow::Error>(repl_session.session_id)
    }
    .await;

    disable_raw_mode().ok();
    if mouse_capture_enabled {
        execute!(
            terminal.backend_mut(),
            Show,
            DisableMouseCapture,
            LeaveAlternateScreen
        )
        .ok();
    } else {
        execute!(terminal.backend_mut(), Show, LeaveAlternateScreen).ok();
    }
    loop_result
}

pub(crate) async fn handle_slash_command(
    registry: &CommandRegistry,
    invocation: CommandInvocation,
    cli: &Cli,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    provider: ApiProvider,
    model: Option<String>,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &[Message],
    live_runtime: bool,
    cwd: &Path,
    auth_source: Option<String>,
) -> Result<()> {
    match invocation.name.as_str() {
        "help" => println!("{}", render_command_help(registry, false)),
        "version" => println!("{}", env!("CARGO_PKG_VERSION")),
        "session" => println!("{}", render_session_command(store, session_id).await?),
        "permissions" => println!("{}", render_permissions_command(cwd).await?),
        "status" => println!("{}", render_status_command(provider, active_model, session_id, live_runtime, cwd)?),
        "ide" => println!("{}", render_ide_command(ide_bridge_enabled(cli), ide_bridge_address(cli))?),
        "statusline" => println!("{}", render_statusline_command(provider, active_model, session_id)?),
        "theme" => println!("{}", render_theme_command()?),
        "vim" => println!("{}", render_vim_command(false)?),
        "plan" => println!("{}", render_plan_command()?),
        "fast" => println!("{}", render_simple_compat_command("fast", "Fast mode uses the same model family with lower latency-focused behavior.")?),
        "passes" => println!("{}", render_simple_compat_command("passes", "Pass-count tuning is not yet modeled separately in the Rust runtime.")?),
        "effort" => println!("{}", render_simple_compat_command("effort", "Reasoning effort tuning remains compatibility-surface only in the current build.")?),
        "skills" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "reload-plugins" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "hooks" => println!("{}", render_simple_compat_command("hooks", "Hook discovery is exposed through plugin manifests in the Rust runtime.")?),
        "output-style" => println!("{}", render_simple_compat_command("output-style", "Output styles are discovered from plugin manifests but alternate renderers remain limited.")?),
        "files" => println!("{}", render_files_command(raw_messages, cwd)?),
        "diff" => println!("{}", render_diff_command(raw_messages)?),
        "usage" | "cost" | "stats" => println!("{}", render_usage_command(raw_messages)?),
        "remote-env" => println!("{}", render_simple_compat_command("remote-env", "Remote environment reporting currently flows through bridge and session status surfaces.")?),
        "export" => println!("{}", render_export_command(store, session_id)?),
        "resume" => {
            if matches!(invocation.args.first().map(String::as_str), Some("import")) {
                let source = invocation
                    .args
                    .get(1)
                    .ok_or_else(|| anyhow!("resume import requires a .jsonl path"))?;
                let imported = import_transcript_to_session_root(
                    &JsonlTranscriptCodec,
                    Path::new(source),
                    store.root_dir(),
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&imported)?);
            } else if let Some(target) = invocation.args.first() {
                let (session_id, transcript_path, messages) =
                    store.load_resume_target(target).await?;
                let runtime_messages = materialize_runtime_messages(&messages);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ResumeReport {
                        session_id,
                        transcript_path,
                        message_count: messages.len(),
                        preview: prompt_preview(&runtime_messages),
                    })?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.list_sessions().await?)?
                );
            }
        }
        "clear" => {
            let target = if let Some(target) = invocation.args.first().cloned() {
                target
            } else if let Some(target) = cli.clear_session.clone() {
                target
            } else if let Some(target) = cli.resume.clone() {
                target
            } else {
                store
                    .list_sessions()
                    .await?
                    .first()
                    .map(|entry| entry.transcript_path.display().to_string())
                    .ok_or_else(|| {
                        anyhow!("clear requires --resume, --clear-session, or an existing session")
                    })?
            };
            let (_, path, _) = store.load_resume_target(&target).await?;
            if path.exists() {
                fs::remove_file(&path)?;
            }
            println!("{}", json!({ "cleared": path }));
        }
        "compact" => {
            let target = if let Some(target) = invocation.args.first().cloned() {
                target
            } else if let Some(target) = cli.resume.clone() {
                target
            } else {
                store
                    .list_sessions()
                    .await?
                    .first()
                    .map(|entry| entry.transcript_path.display().to_string())
                    .ok_or_else(|| anyhow!("compact requires --resume or an existing session"))?
            };
            let (session_id, path, mut messages) = store.load_resume_target(&target).await?;
            let estimated_tokens_before =
                estimate_message_tokens(&materialize_runtime_messages(&messages));
            let outcome = compact_messages(
                &messages,
                &CompactionConfig {
                    kind: BoundaryKind::Compact,
                    trigger: "manual".to_owned(),
                    max_tokens_before: Some(estimated_tokens_before),
                    target_tokens_after: compact_target_tokens(),
                    ..CompactionConfig::default()
                },
            );
            if let Some(outcome) = outcome {
                apply_compaction_outcome(store, session_id, &mut messages, &outcome).await?;
                println!(
                    "{}",
                    json!({
                        "compacted": path,
                        "session_id": session_id,
                        "summarized_message_count": outcome.summarized_message_count,
                        "preserved_message_count": outcome.preserved_message_count,
                        "estimated_tokens_before": outcome.estimated_tokens_before,
                        "estimated_tokens_after": outcome.estimated_tokens_after,
                    })
                );
            } else {
                println!(
                    "{}",
                    json!({
                        "compacted": false,
                        "session_id": session_id,
                        "reason": "already_under_target",
                        "estimated_tokens_before": estimated_tokens_before,
                    })
                );
            }
        }
        "model" => {
            let catalog = compatibility_model_catalog(provider);
            if let Some(selected) = model {
                println!("{}", json!({ "provider": provider, "model": selected }));
            } else {
                println!("{}", serde_json::to_string_pretty(&catalog.list_models())?);
            }
        }
        "config" => {
            if matches!(invocation.args.first().map(String::as_str), Some("migrate")) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config_migration_report(provider))?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "provider": provider,
                        "model": model,
                        "cwd": cwd,
                        "project_dir": get_project_dir(cwd),
                        "session_root": store.root_dir(),
                        "auth_source": auth_source,
                        "auto_compact_threshold_tokens": auto_compact_threshold_tokens(),
                        "compact_target_tokens": compact_target_tokens(),
                    }))?
                );
            }
        }
        "login" => {
            println!("{}", render_auth_command(provider, "login").await?);
        }
        "logout" => {
            println!(
                "{}",
                render_auth_command_with_resume(
                    provider,
                    "logout",
                    latest_resume_hint(&store).await?,
                )
                .await?
            );
        }
        "plugin" => {
            println!(
                "{}",
                render_plugin_command(&invocation, cli.plugin_root.as_ref(), cwd).await?
            );
        }
        "mcp" => {
            println!(
                "{}",
                render_mcp_command(
                    &invocation,
                    cli.plugin_root.as_ref(),
                    tool_registry,
                    cwd,
                    provider,
                    model.clone(),
                )
                .await?
            );
        }
        "memory" => println!(
            "{}",
            render_memory_command(&invocation, tool_registry, cwd, provider, model.clone()).await?
        ),
        "tasks" => println!("{}", render_tasks_command(&invocation, cwd)?),
        "agents" => {
            println!(
                "{}",
                render_agents_command(
                    &invocation,
                    tool_registry,
                    cwd,
                    provider,
                    model.clone(),
                    session_id,
                )
                .await?
            );
        }
        "remote-control" => {
            println!(
                "{}",
                render_remote_control_command(
                    registry,
                    &invocation,
                    cli,
                    store,
                    tool_registry,
                    cwd,
                    provider,
                    active_model,
                    session_id,
                    raw_messages,
                    live_runtime,
                )
                .await?
            );
        }
        "voice" => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": "deferred",
                "message": "voice features are intentionally excluded from the current finish target",
            }))?
        ),
        "exit" => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": "noop",
                "message": "Use /exit or /quit inside --repl to leave the interactive session.",
            }))?
        ),
        other => bail!("unknown registered command: {other}"),
    }
    Ok(())
}
