enum ClipboardPath {
    Native,
    TmuxBuffer,
    Osc52,
}

pub(crate) fn should_enable_mouse_capture(term_program: Option<&str>) -> bool {
    let _ = term_program;
    true
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
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    let skills = resolved_skill_entries(cwd, plugin_root).await?;
    let commands = resolved_dynamic_commands(cwd, plugin_root).await;
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

fn run_clipboard_capture_command(program: &str, args: &[&str]) -> Result<String> {
    let output = StdCommand::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to launch clipboard helper: {program}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("clipboard helper exited with status {}", output.status)
    }
}

fn try_read_from_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        return run_clipboard_capture_command("pbpaste", &[]).ok();
    }

    #[cfg(target_os = "windows")]
    {
        return run_clipboard_capture_command(
            "powershell",
            &["-NoProfile", "-Command", "Get-Clipboard"],
        )
        .ok()
        .or_else(|| {
            run_clipboard_capture_command("pwsh", &["-NoProfile", "-Command", "Get-Clipboard"]).ok()
        });
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Ok(text) = run_clipboard_capture_command("wl-paste", &["--no-newline"]) {
                return Some(text);
            }
        }
        if std::env::var_os("DISPLAY").is_some() {
            if let Ok(text) =
                run_clipboard_capture_command("xclip", &["-selection", "clipboard", "-o"])
            {
                return Some(text);
            }
        }
        if let Ok(text) = run_clipboard_capture_command("xsel", &["--clipboard", "--output"]) {
            return Some(text);
        }
    }

    #[allow(unreachable_code)]
    None
}

pub(crate) fn read_text_from_clipboard() -> Option<String> {
    let local_session = std::env::var_os("SSH_CONNECTION").is_none();
    if !local_session {
        return None;
    }

    try_read_from_clipboard().filter(|text| !text.is_empty())
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
