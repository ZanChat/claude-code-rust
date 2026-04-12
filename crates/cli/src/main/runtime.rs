fn env_u64(names: &[&str], default: u64) -> u64 {
    for name in names {
        if let Ok(raw) = env::var(name) {
            if let Ok(value) = raw.trim().parse::<u64>() {
                return value;
            }
        }
    }
    default
}

fn auto_compact_threshold_tokens() -> u64 {
    env_u64(
        &[
            "CODE_AGENT_AUTO_COMPACT_THRESHOLD_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_THRESHOLD_TOKENS",
        ],
        24_000,
    )
}

fn compact_target_tokens() -> u64 {
    env_u64(
        &[
            "CODE_AGENT_COMPACT_TARGET_TOKENS",
            "CLAUDE_CODE_COMPACT_TARGET_TOKENS",
        ],
        12_000,
    )
}

fn provider_error_transcript_text(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<ccrust_providers::ProviderRequestError>()
        .map(|error| error.transcript_message().to_owned())
        .unwrap_or_else(|| error.to_string())
}

async fn append_provider_error_message(
    store: &ActiveSessionStore,
    session_id: SessionId,
    messages: &mut Vec<Message>,
    parent_id: Option<uuid::Uuid>,
    provider: ApiProvider,
    model: &str,
    prompt_metrics: &RuntimeSystemPromptMetrics,
    response_text: String,
    tool_calls: Vec<ccrust_core::ToolCall>,
    error_text: String,
) -> Result<()> {
    let mut combined_text = response_text.trim().to_owned();
    let error_text = error_text.trim().to_owned();

    if combined_text.is_empty() {
        combined_text = error_text;
    } else if !error_text.is_empty() {
        combined_text = format!("{combined_text}\n\n{error_text}");
    }

    let assistant_message = provider_assistant_message(
        session_id,
        parent_id,
        combined_text,
        tool_calls,
        provider,
        model,
        None,
        prompt_metrics,
    );
    store.append_message(session_id, &assistant_message).await?;
    messages.push(assistant_message);
    Ok(())
}

async fn apply_compaction_outcome(
    store: &ActiveSessionStore,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    outcome: &CompactionOutcome,
) -> Result<()> {
    store
        .append_message(session_id, &outcome.summary_message)
        .await?;
    store
        .append_message(session_id, &outcome.boundary_message)
        .await?;
    raw_messages.push(outcome.summary_message.clone());
    raw_messages.push(outcome.boundary_message.clone());
    Ok(())
}

async fn maybe_auto_compact(
    store: &ActiveSessionStore,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
) -> Result<Option<CompactionOutcome>> {
    let estimated_tokens_before =
        estimate_message_tokens(&materialize_runtime_messages(raw_messages));
    if estimated_tokens_before <= auto_compact_threshold_tokens() {
        return Ok(None);
    }

    let outcome = compact_messages(
        raw_messages,
        &CompactionConfig {
            kind: BoundaryKind::Compact,
            trigger: "auto".to_owned(),
            max_tokens_before: Some(estimated_tokens_before),
            target_tokens_after: compact_target_tokens(),
            ..CompactionConfig::default()
        },
    );
    if let Some(outcome) = &outcome {
        apply_compaction_outcome(store, session_id, raw_messages, outcome).await?;
    }
    Ok(outcome)
}

async fn execute_agent_tool_call(
    tool_registry: &ToolRegistry,
    tool_context: &ToolContext,
    call: &ccrust_core::ToolCall,
) -> (String, bool, Value) {
    let input = match serde_json::from_str(&call.input_json) {
        Ok(input) => input,
        Err(error) => {
            return (
                format!("invalid tool input JSON: {error}"),
                true,
                Value::Null,
            );
        }
    };

    match tool_registry
        .invoke(
            ToolCallRequest {
                tool_name: call.name.clone(),
                input,
            },
            tool_context,
        )
        .await
    {
        Ok(output) => (output.content, output.is_error, output.metadata),
        Err(error) => (
            format!("Error calling tool ({}): {error}", call.name),
            true,
            Value::Null,
        ),
    }
}

async fn run_agent_turns(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    model: String,
    session_id: SessionId,
    messages: &mut Vec<Message>,
    auth_configured: bool,
    runtime_options: &RuntimeCliOptions,
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<ccrust_core::TokenUsage>, usize, Option<String>)> {
    const MAX_AGENT_STEPS: usize = 100;

    let provider_tools = tool_definitions(tool_registry);
    let system_prompt =
        build_runtime_system_prompt(&cwd, tool_registry, provider, &model, plugin_root);
    maybe_log_runtime_prompt_drift(messages, &system_prompt.metrics);
    let tool_context = ToolContext {
        session_id: Some(session_id),
        cwd: cwd.clone(),
        provider: Some(provider.to_string()),
        model: Some(model.clone()),
        permission_mode: runtime_options.tool_permission_mode.clone(),
        ..ToolContext::default()
    };

    let mut step = 1;
    let mut recent_tools = Vec::new();

    loop {
        let step_start_index = messages.len();
        update_pending_repl_step_view(
            pending_view,
            step,
            step_start_index,
            messages,
            format!("Waiting for response · step {step}"),
            None,
            TaskStatus::Running,
        );
        let parent_id = messages.last().map(|message| message.id);
        let provider_client = match resolve_provider_client(provider, auth_configured).await {
            Ok(provider_client) => provider_client,
            Err(error) => {
                append_provider_error_message(
                    store,
                    session_id,
                    messages,
                    parent_id,
                    provider,
                    &model,
                    &system_prompt.metrics,
                    String::new(),
                    Vec::new(),
                    provider_error_transcript_text(&error),
                )
                .await?;
                return Err(error);
            }
        };
        let mut stream = match provider_client
            .start_stream(ProviderRequest {
                model: model.clone(),
                system_prompt: system_prompt.blocks.clone(),
                messages: messages.clone(),
                tools: provider_tools.clone(),
                ..ProviderRequest::default()
            })
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                append_provider_error_message(
                    store,
                    session_id,
                    messages,
                    parent_id,
                    provider,
                    &model,
                    &system_prompt.metrics,
                    String::new(),
                    Vec::new(),
                    provider_error_transcript_text(&error),
                )
                .await?;
                return Err(error);
            }
        };
        let mut response_text = String::new();
        let mut response_tool_calls = Vec::new();
        let mut latest_usage = None;
        let mut stop_reason = None;

        while let Some(event) = match stream.next_event().await {
            Ok(event) => event,
            Err(error) => {
                append_provider_error_message(
                    store,
                    session_id,
                    messages,
                    parent_id,
                    provider,
                    &model,
                    &system_prompt.metrics,
                    response_text.clone(),
                    response_tool_calls.clone(),
                    provider_error_transcript_text(&error),
                )
                .await?;
                return Err(error);
            }
        } {
            match event {
                ProviderEvent::MessageDelta { text } => {
                    response_text.push_str(&text);
                    let preview_message = provider_assistant_message(
                        session_id,
                        parent_id,
                        response_text.clone(),
                        response_tool_calls.clone(),
                        provider,
                        &model,
                        latest_usage.clone(),
                        &system_prompt.metrics,
                    );
                    let mut preview_messages = messages.clone();
                    preview_messages.push(preview_message);
                    update_pending_repl_step_view(
                        pending_view,
                        step,
                        step_start_index,
                        &preview_messages,
                        format!("Receiving response · step {step}"),
                        preview_detail(&response_text, 1, 96),
                        TaskStatus::Running,
                    );
                }
                ProviderEvent::ToolCall { call } => {
                    let tool_name = call.name.clone();
                    response_tool_calls.push(call);
                    let current_call = response_tool_calls.last().cloned();
                    let preview_message = provider_assistant_message(
                        session_id,
                        parent_id,
                        response_text.clone(),
                        response_tool_calls.clone(),
                        provider,
                        &model,
                        latest_usage.clone(),
                        &system_prompt.metrics,
                    );
                    let mut preview_messages = messages.clone();
                    preview_messages.push(preview_message);
                    update_pending_repl_step_view(
                        pending_view,
                        step,
                        step_start_index,
                        &preview_messages,
                        format!("Running {}", tool_display_name(&tool_name)),
                        current_call
                            .as_ref()
                            .and_then(pending_tool_detail_from_call),
                        TaskStatus::Running,
                    );
                }
                ProviderEvent::ToolCallBoundary { .. } => {}
                ProviderEvent::Usage { usage } => {
                    latest_usage = Some(usage);
                }
                ProviderEvent::Stop { reason } => {
                    stop_reason = Some(reason);
                    break;
                }
                ProviderEvent::Error { message } => {
                    append_provider_error_message(
                        store,
                        session_id,
                        messages,
                        parent_id,
                        provider,
                        &model,
                        &system_prompt.metrics,
                        response_text.clone(),
                        response_tool_calls.clone(),
                        message.clone(),
                    )
                    .await?;
                    return Err(anyhow!(message));
                }
            }
        }

        let assistant_message = provider_assistant_message(
            session_id,
            parent_id,
            response_text,
            response_tool_calls.clone(),
            provider,
            &model,
            latest_usage.clone(),
            &system_prompt.metrics,
        );
        store.append_message(session_id, &assistant_message).await?;
        messages.push(assistant_message.clone());
        update_pending_repl_step_view(
            pending_view,
            step,
            step_start_index,
            messages,
            if response_tool_calls.is_empty() {
                format!("Completed step {step}")
            } else {
                format!(
                    "Running {}",
                    tool_display_name(&response_tool_calls[0].name)
                )
            },
            response_tool_calls
                .first()
                .and_then(pending_tool_detail_from_call),
            if response_tool_calls.is_empty() {
                TaskStatus::Completed
            } else {
                TaskStatus::Running
            },
        );

        if response_tool_calls.is_empty() {
            return Ok((latest_usage, step, stop_reason));
        }

        for call in response_tool_calls {
            update_pending_repl_step_view(
                pending_view,
                step,
                step_start_index,
                messages,
                format!("Running {}", tool_display_name(&call.name)),
                pending_tool_detail_from_call(&call),
                TaskStatus::Running,
            );
            let (output_content, output_is_error, output_metadata) =
                execute_agent_tool_call(tool_registry, &tool_context, &call).await;
            let tool_message = build_tool_result_message(
                session_id,
                call.id.clone(),
                output_content,
                output_is_error,
                messages.last().map(|message| message.id),
            );
            store.append_message(session_id, &tool_message).await?;
            let tool_message_id = tool_message.id;
            messages.push(tool_message);
            append_session_messages(
                store,
                messages,
                tool_ui_event_messages(session_id, Some(tool_message_id), &output_metadata),
            )
            .await?;
            update_pending_repl_step_view(
                pending_view,
                step,
                step_start_index,
                messages,
                if output_is_error {
                    format!("{} failed", tool_display_name(&call.name))
                } else {
                    format!("{} completed", tool_display_name(&call.name))
                },
                pending_tool_detail_from_metadata(&call.name, &output_metadata)
                    .or_else(|| pending_tool_detail_from_call(&call)),
                if output_is_error {
                    TaskStatus::Failed
                } else {
                    TaskStatus::Completed
                },
            );

            recent_tools.push(format!(
                "- {}: {}",
                call.name,
                if output_is_error { "failed" } else { "completed" }
            ));
        }

        if step > 0 && step % MAX_AGENT_STEPS == 0 {
            let summary = recent_tools.join("\n");
            recent_tools.clear();
            let text = format!("Checkpoint: You have executed {MAX_AGENT_STEPS} consecutive steps. Here is a summary of the recent tool runs:\n{summary}\n\nPlease decide whether to continue. If the task is done or stuck, you can stop by replying with your final response. If you need to continue, please explain what you will do next.");
            
            let mut checkpoint_message = Message::new(MessageRole::User, vec![ContentBlock::Text { text }]);
            checkpoint_message.session_id = Some(session_id);
            checkpoint_message.parent_id = messages.last().map(|m| m.id);
            
            store.append_message(session_id, &checkpoint_message).await?;
            messages.push(checkpoint_message);
        }

        if runtime_options
            .max_turns
            .is_some_and(|max_turns| step >= max_turns)
        {
            return Ok((latest_usage, step, Some("max_turns".to_owned())));
        }

        step += 1;
    }
}

async fn execute_local_turn(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    prompt_text: String,
    live_runtime: bool,
    pending_view: Option<Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<CompactionOutcome>, usize, Option<String>, u64, u64)> {
    execute_local_turn_with_options(
        store,
        tool_registry,
        cwd,
        plugin_root,
        provider,
        active_model,
        session_id,
        raw_messages,
        prompt_text,
        live_runtime,
        &RuntimeCliOptions::default(),
        pending_view,
    )
    .await
}

async fn execute_local_turn_with_options(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    prompt_text: String,
    live_runtime: bool,
    runtime_options: &RuntimeCliOptions,
    pending_view: Option<Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<CompactionOutcome>, usize, Option<String>, u64, u64)> {
    let parent_id = raw_messages.last().map(|message| message.id);
    let user_message = build_text_message(session_id, MessageRole::User, prompt_text, parent_id);
    execute_local_turn_with_user_message_options(
        store,
        tool_registry,
        cwd,
        plugin_root,
        provider,
        active_model,
        session_id,
        raw_messages,
        user_message,
        live_runtime,
        runtime_options,
        pending_view,
    )
    .await
}

async fn execute_local_turn_with_user_message_options(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    user_message: Message,
    live_runtime: bool,
    runtime_options: &RuntimeCliOptions,
    pending_view: Option<Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<CompactionOutcome>, usize, Option<String>, u64, u64)> {
    store.append_message(session_id, &user_message).await?;
    raw_messages.push(user_message);
    update_pending_repl_view(pending_view.as_ref(), raw_messages, "Waiting for response");

    let estimated_tokens_before =
        estimate_message_tokens(&materialize_runtime_messages(raw_messages));
    let applied_compaction = maybe_auto_compact(store, session_id, raw_messages).await?;
    update_pending_repl_view(pending_view.as_ref(), raw_messages, "Waiting for response");
    let estimated_tokens_after_compaction = applied_compaction
        .as_ref()
        .map(|outcome| outcome.estimated_tokens_after)
        .unwrap_or_else(|| estimate_message_tokens(&materialize_runtime_messages(raw_messages)));
    let mut runtime_messages = materialize_runtime_messages(raw_messages);
    let turn_result = run_agent_turns(
        store,
        tool_registry,
        cwd,
        plugin_root,
        provider,
        active_model,
        session_id,
        &mut runtime_messages,
        live_runtime,
        runtime_options,
        pending_view.as_ref(),
    )
    .await;
    *raw_messages = store.load_session(session_id).await.unwrap_or_default();

    let (_, turn_count, stop_reason) = turn_result?;

    Ok((
        applied_compaction,
        turn_count,
        stop_reason,
        estimated_tokens_before,
        estimated_tokens_after_compaction,
    ))
}

fn parse_input(input: Option<&str>) -> Result<Value> {
    match input {
        Some(raw) if !raw.trim().is_empty() => Ok(serde_json::from_str(raw)?),
        _ => Ok(json!({})),
    }
}

async fn maybe_notify_auto_connected_ide(
    cwd: &Path,
    explicit_flag: bool,
    home_override: Option<&Path>,
    term_program_override: Option<&str>,
    env_port_override: Option<u16>,
) -> Result<bool> {
    let Some(config) = auto_connected_ide_server_config(
        cwd,
        term_program_override,
        home_override,
        env_port_override,
        explicit_flag,
    ) else {
        return Ok(false);
    };

    send_notification_from_config(&config, "ide_connected", json!({ "pid": std::process::id() }))
        .await?;
    Ok(true)
}

fn resolve_plugin_root_with_override(
    plugin_root: Option<&PathBuf>,
    candidate: Option<&str>,
    cwd: &Path,
) -> PathBuf {
    match candidate {
        Some(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        _ => plugin_root.cloned().unwrap_or_else(|| cwd.to_path_buf()),
    }
}

fn resolve_plugin_root(cli: &Cli, candidate: Option<&str>, cwd: &Path) -> PathBuf {
    resolve_plugin_root_with_override(cli.plugin_root.as_ref(), candidate, cwd)
}

fn command_count(commands: Option<&CommandDefinitions>) -> usize {
    match commands {
        Some(CommandDefinitions::Single(_)) => 1,
        Some(CommandDefinitions::List(items)) => items.len(),
        Some(CommandDefinitions::Mapping(items)) => items.len(),
        None => 0,
    }
}

async fn load_plugin_report(root: PathBuf) -> Result<PluginReport> {
    let runtime = OutOfProcessPluginRuntime;
    let loaded = runtime.load_manifest(&root).await?;
    let skills = runtime.discover_skills(&root).await?;
    let commands = runtime.discover_commands(&root).await?;
    let mut skill_names = skills
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    skill_names.sort();
    let mut command_names = commands
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    command_names.sort();

    let mut mcp_server_names = parse_mcp_server_configs(&loaded.manifest.mcp_servers)
        .into_keys()
        .collect::<Vec<_>>();
    mcp_server_names.sort();

    let mut lsp_server_names = loaded
        .manifest
        .lsp_servers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    lsp_server_names.sort();

    Ok(PluginReport {
        root,
        name: loaded.manifest.name,
        version: loaded.manifest.version,
        description: loaded.manifest.description,
        skill_names,
        command_names,
        mcp_server_names,
        lsp_server_names,
        command_count: command_count(loaded.manifest.commands.as_ref()),
        has_agents: loaded.manifest.agents.is_some(),
        has_output_styles: loaded.manifest.output_styles.is_some(),
    })
}

fn canonical_top_level_command_name(name: &str) -> &str {
    match name {
        "plugins" => "plugin",
        "rc" => "remote-control",
        other => other,
    }
}

fn build_top_level_invocation(name: &str, args: &[String]) -> CommandInvocation {
    let canonical = canonical_top_level_command_name(name);
    let raw_input = if args.is_empty() {
        canonical.to_owned()
    } else {
        format!("{canonical} {}", args.join(" "))
    };

    CommandInvocation {
        name: canonical.to_owned(),
        args: args.to_vec(),
        raw_input,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedServerCommand {
    bind_address: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedOpenCommand {
    address: String,
    prompt: Option<String>,
    output_mode: OutputMode,
}

fn parse_long_command_option(arg: &str) -> Option<(&str, Option<&str>)> {
    let option = arg.strip_prefix("--")?;
    if option.is_empty() {
        return None;
    }

    option
        .split_once('=')
        .map_or_else(|| Some((option, None)), |(name, value)| Some((name, Some(value))))
}

fn consume_command_value(
    args: &[String],
    index: &mut usize,
    display: &str,
    inline_value: Option<&str>,
) -> Result<String> {
    if let Some(value) = inline_value {
        return Ok(value.to_owned());
    }

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

fn parse_output_mode_value(display: &str, value: &str) -> Result<OutputMode> {
    match value {
        "text" => Ok(OutputMode::Text),
        "json" => Ok(OutputMode::Json),
        "stream-json" => Ok(OutputMode::StreamJson),
        _ => bail!(
            "invalid value for {display}: {value} (expected one of: text, json, stream-json)"
        ),
    }
}

fn parse_server_command_args(args: &[String]) -> Result<ParsedServerCommand> {
    let mut host = "0.0.0.0".to_owned();
    let mut port = 0u16;
    let mut index = 0usize;

    while index < args.len() {
        let arg = &args[index];
        if let Some((name, inline_value)) = parse_long_command_option(arg) {
            match name {
                "host" => {
                    host = consume_command_value(args, &mut index, "--host", inline_value)?;
                }
                "port" => {
                    let value = consume_command_value(args, &mut index, "--port", inline_value)?;
                    port = value.parse::<u16>().map_err(|error| {
                        anyhow!("invalid value for --port: {value} ({error})")
                    })?;
                }
                "auth-token" | "unix" | "workspace" | "idle-timeout" | "max-sessions" => {
                    bail!(
                        "ccrust server --{name} is not implemented yet in the Rust CLI"
                    );
                }
                _ => bail!("unknown option: {arg}"),
            }
        } else {
            bail!("unexpected argument for server: {arg}");
        }

        index += 1;
    }

    Ok(ParsedServerCommand {
        bind_address: format!("tcp://{host}:{port}"),
    })
}

fn parse_open_command_args(
    args: &[String],
    default_output_mode: OutputMode,
    global_print_enabled: bool,
) -> Result<ParsedOpenCommand> {
    let mut target = None;
    let mut prompt = None;
    let mut output_mode = default_output_mode;
    let mut print_enabled = global_print_enabled;
    let mut index = 0usize;

    while index < args.len() {
        let arg = &args[index];
        if arg == "-p" {
            print_enabled = true;
            if target.is_some() {
                let next_index = index + 1;
                if let Some(value) = args.get(next_index) {
                    if !value.starts_with('-') {
                        prompt = Some(value.clone());
                        index = next_index;
                    }
                }
            }
        } else if let Some((name, inline_value)) = parse_long_command_option(arg) {
            match name {
                "print" => {
                    print_enabled = true;
                    if let Some(value) = inline_value {
                        prompt = Some(value.to_owned());
                    } else if target.is_some() {
                        let next_index = index + 1;
                        if let Some(value) = args.get(next_index) {
                            if !value.starts_with('-') {
                                prompt = Some(value.clone());
                                index = next_index;
                            }
                        }
                    }
                }
                "output-format" => {
                    let value =
                        consume_command_value(args, &mut index, "--output-format", inline_value)?;
                    output_mode = parse_output_mode_value("--output-format", &value)?;
                }
                _ => bail!("unknown option: {arg}"),
            }
        } else if arg.starts_with('-') {
            bail!("unknown option: {arg}");
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("unexpected argument for open: {arg}");
        }

        index += 1;
    }

    let target = target.ok_or_else(|| anyhow!("open requires a target address"))?;
    if target.starts_with("cc://") || target.starts_with("cc+unix://") {
        bail!(
            "cc:// direct-connect URLs are not implemented yet in the Rust CLI; use tcp://, direct://, ide://, ws://, or wss:// endpoints instead"
        );
    }
    if !print_enabled {
        bail!(
            "interactive open is not implemented yet in the Rust CLI; use 'ccrust open <address> -p [prompt]'"
        );
    }

    Ok(ParsedOpenCommand {
        address: if target.contains("://") {
            target
        } else {
            format!("tcp://{target}")
        },
        prompt,
        output_mode,
    })
}

fn ensure_ssh_command_supported(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "-p" || arg == "--print") {
        bail!("headless (-p/--print) mode is not supported with ccrust ssh");
    }

    if args.iter().all(|arg| arg.starts_with('-')) {
        bail!("Usage: ccrust ssh <user@host | ssh-config-alias> [dir]");
    }

    bail!(
        "ssh remote execution is not implemented yet in the Rust CLI; the current rewrite has no SSH session manager"
    )
}

async fn connect_remote_endpoint_for_cli(
    cli: &Cli,
    address: &str,
    session_id: SessionId,
    prompt: Option<String>,
) -> Result<Vec<RemoteEnvelope>> {
    connect_and_exchange(
        remote_endpoint(address, session_id),
        build_remote_outbound(cli, session_id, prompt, cli.resume.as_deref())?,
        cli.bridge_receive_count.unwrap_or(1),
    )
    .await
}

fn print_remote_envelopes(inbound: &[RemoteEnvelope], output_mode: &OutputMode) -> Result<()> {
    match output_mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(inbound)?),
        OutputMode::StreamJson => {
            for envelope in inbound {
                println!("{}", serde_json::to_string(envelope)?);
            }
        }
        OutputMode::Text => {
            let lines = inbound
                .iter()
                .filter_map(|envelope| match envelope {
                    RemoteEnvelope::Message { message } if message.role == MessageRole::Assistant => {
                        let text = message_text(message);
                        (!text.trim().is_empty()).then_some(text)
                    }
                    RemoteEnvelope::Ack { note } => Some(note.clone()),
                    RemoteEnvelope::Error { message } => Some(format!("error: {message}")),
                    _ => None,
                })
                .collect::<Vec<_>>();

            if lines.is_empty() {
                println!("{}", serde_json::to_string_pretty(inbound)?);
            } else {
                println!("{}", lines.join("\n\n"));
            }
        }
    }

    Ok(())
}

#[derive(Clone, Debug, serde::Serialize)]
struct HeadlessUsageReport {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
struct HeadlessTranscriptEvent {
    r#type: &'static str,
    subtype: &'static str,
    session_id: SessionId,
    message_id: uuid::Uuid,
    parent_id: Option<uuid::Uuid>,
    role: &'static str,
    text: String,
}

#[derive(Clone, Debug, serde::Serialize)]
struct HeadlessResultEvent {
    r#type: &'static str,
    subtype: &'static str,
    duration_ms: u64,
    duration_api_ms: u64,
    is_error: bool,
    num_turns: usize,
    stop_reason: Option<String>,
    session_id: SessionId,
    total_cost_usd: f64,
    usage: HeadlessUsageReport,
    permission_denials: Vec<Value>,
    result: String,
    structured_output: Option<Value>,
    errors: Vec<String>,
}

fn headless_usage_report_from_totals(totals: UsageTotals) -> HeadlessUsageReport {
    HeadlessUsageReport {
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cache_creation_input_tokens: totals.cache_creation_input_tokens,
        cache_read_input_tokens: totals.cache_read_input_tokens,
    }
}

fn validate_root_print_mode(contract: &CliContract) -> Result<()> {
    let print_mode = &contract.global.print_mode;
    if print_mode.input_mode == InputMode::StreamJson
        && print_mode.output_mode != OutputMode::StreamJson
    {
        bail!("Error: --input-format=stream-json requires output-format=stream-json.");
    }
    if print_mode.replay_user_messages
        && (print_mode.input_mode != InputMode::StreamJson
            || print_mode.output_mode != OutputMode::StreamJson)
    {
        bail!(
            "Error: --replay-user-messages requires both --input-format=stream-json and --output-format=stream-json."
        );
    }
    if print_mode.include_partial_messages
        && (!print_mode.enabled || print_mode.output_mode != OutputMode::StreamJson)
    {
        bail!(
            "Error: --include-partial-messages requires --print and --output-format=stream-json."
        );
    }
    if !contract.global.resume.session_persistence && !print_mode.enabled {
        bail!("Error: --no-session-persistence can only be used with --print mode.");
    }
    Ok(())
}

fn read_text_input_from_stdin(prompt: Option<&str>) -> Result<String> {
    let mut combined = prompt.unwrap_or_default().to_owned();
    if !std::io::stdin().is_terminal() {
        let mut stdin_text = String::new();
        std::io::stdin().read_to_string(&mut stdin_text)?;
        let stdin_text = stdin_text.trim_end_matches('\0');
        if !stdin_text.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(stdin_text);
        }
    }
    Ok(combined)
}

fn stream_json_prompt_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return Some(text.to_owned());
            }
            if let Some(prompt) = map.get("prompt").and_then(Value::as_str) {
                return Some(prompt.to_owned());
            }
            if let Some(message) = map.get("message").and_then(Value::as_object) {
                if let Some(text) = message.get("text").and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(content) = message.get("content").and_then(Value::as_array) {
                    let mut parts = Vec::new();
                    for item in content {
                        if let Some(text) = item
                            .as_object()
                            .and_then(|item| item.get("text"))
                            .and_then(Value::as_str)
                        {
                            parts.push(text);
                        }
                    }
                    if !parts.is_empty() {
                        return Some(parts.join(""));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn collect_stream_json_prompts(prompt: Option<&str>) -> Result<Vec<String>> {
    if std::io::stdin().is_terminal() {
        return Ok(prompt
            .map(str::to_owned)
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect());
    }

    let stdin = std::io::stdin();
    let reader = std::io::BufReader::new(stdin.lock());
    let mut prompts = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .map_err(|error| anyhow!("invalid stream-json input line: {error}"))?;
        if let Some(prompt) = stream_json_prompt_from_value(&value) {
            if !prompt.trim().is_empty() {
                prompts.push(prompt);
            }
        }
    }

    if prompts.is_empty() {
        if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
            prompts.push(prompt.to_owned());
        }
    }

    Ok(prompts)
}

fn collect_print_mode_prompts(prompt: Option<&str>, input_mode: &InputMode) -> Result<Vec<String>> {
    match input_mode {
        InputMode::Text => {
            let combined = read_text_input_from_stdin(prompt)?;
            Ok((!combined.trim().is_empty())
                .then_some(combined)
                .into_iter()
                .collect())
        }
        InputMode::StreamJson => collect_stream_json_prompts(prompt),
    }
}

fn resume_cutoff_index(messages: &[Message], target: &str) -> Result<usize> {
    let message_id =
        uuid::Uuid::parse_str(target).map_err(|error| anyhow!("invalid message id '{target}': {error}"))?;
    messages
        .iter()
        .position(|message| message.id == message_id && message.role == MessageRole::Assistant)
        .map(|index| index + 1)
        .ok_or_else(|| anyhow!("could not find assistant message '{target}' in the resumed session"))
}

fn apply_resume_message_cutoff(messages: &mut Vec<Message>, target: Option<&str>) -> Result<()> {
    let Some(target) = target else {
        return Ok(());
    };
    let cutoff = resume_cutoff_index(messages, target)?;
    messages.truncate(cutoff);
    Ok(())
}

fn headless_structured_output(
    json_schema: Option<&str>,
    result_text: &str,
) -> Result<Option<Value>> {
    let Some(schema) = json_schema else {
        return Ok(None);
    };
    let schema_value: Value = serde_json::from_str(schema)
        .map_err(|error| anyhow!("invalid JSON schema supplied to --json-schema: {error}"))?;
    if !schema_value.is_object() {
        bail!("invalid JSON schema supplied to --json-schema: expected a JSON object");
    }
    let output: Value = serde_json::from_str(result_text)
        .map_err(|error| anyhow!("assistant output is not valid JSON for --json-schema: {error}"))?;
    Ok(Some(output))
}

fn headless_transcript_events(
    messages: &[Message],
    include_partial_messages: bool,
) -> Vec<HeadlessTranscriptEvent> {
    let mut events = Vec::new();
    for message in messages {
        let role = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
            _ => continue,
        };
        let text = message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        if include_partial_messages && message.role == MessageRole::Assistant {
            events.push(HeadlessTranscriptEvent {
                r#type: "message_delta",
                subtype: "assistant_delta",
                session_id: message.session_id.unwrap_or_default(),
                message_id: message.id,
                parent_id: message.parent_id,
                role,
                text: text.clone(),
            });
        }
        events.push(HeadlessTranscriptEvent {
            r#type: "message",
            subtype: role,
            session_id: message.session_id.unwrap_or_default(),
            message_id: message.id,
            parent_id: message.parent_id,
            role,
            text,
        });
    }
    events
}

fn emit_headless_error(output_mode: &OutputMode, session_id: SessionId, error: &str) -> Result<()> {
    let result = HeadlessResultEvent {
        r#type: "result",
        subtype: "error_during_execution",
        duration_ms: 0,
        duration_api_ms: 0,
        is_error: true,
        num_turns: 0,
        stop_reason: None,
        session_id,
        total_cost_usd: 0.0,
        usage: HeadlessUsageReport {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        permission_denials: Vec::new(),
        result: String::new(),
        structured_output: None,
        errors: vec![error.to_owned()],
    };
    match output_mode {
        OutputMode::Text => eprintln!("{error}"),
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputMode::StreamJson => println!("{}", serde_json::to_string(&result)?),
    }
    Ok(())
}

async fn run_root_print_mode(
    contract: &CliContract,
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &mut Vec<Message>,
    prompt: Option<&str>,
    live_runtime: bool,
    runtime_options: &RuntimeCliOptions,
) -> Result<()> {
    validate_root_print_mode(contract)?;
    let prompts = collect_print_mode_prompts(prompt, &contract.global.print_mode.input_mode)?;
    if prompts.is_empty() && raw_messages.is_empty() {
        bail!("print mode requires a prompt or stdin input");
    }

    let mut last_result = None;
    for prompt_text in prompts {
        let existing_len = raw_messages.len();
        if contract.global.print_mode.replay_user_messages
            && contract.global.print_mode.output_mode == OutputMode::StreamJson
        {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "type": "message",
                    "subtype": "user",
                    "session_id": session_id,
                    "text": prompt_text,
                }))?
            );
        }

        let started = std::time::Instant::now();
        let turn = execute_local_turn_with_options(
            store,
            tool_registry,
            cwd.to_path_buf(),
            plugin_root,
            provider,
            active_model.to_owned(),
            session_id,
            raw_messages,
            prompt_text,
            live_runtime,
            runtime_options,
            None,
        )
        .await;

        let duration_ms = started.elapsed().as_millis() as u64;
        let (turn_count, stop_reason, is_error) = match turn {
            Ok((_, turn_count, stop_reason, _, _)) => (turn_count, stop_reason, false),
            Err(error) => {
                emit_headless_error(
                    &contract.global.print_mode.output_mode,
                    session_id,
                    &error.to_string(),
                )?;
                return Ok(());
            }
        };

        let new_messages = raw_messages
            .get(existing_len..)
            .map_or_else(Vec::new, ToOwned::to_owned);
        if contract.global.print_mode.output_mode == OutputMode::StreamJson {
            for event in headless_transcript_events(
                &new_messages,
                contract.global.print_mode.include_partial_messages,
            ) {
                println!("{}", serde_json::to_string(&event)?);
            }
        }

        let result_text = new_messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(message_text)
            .unwrap_or_default();
        let structured_output = headless_structured_output(
            contract.global.print_mode.json_schema.as_deref(),
            &result_text,
        )?;
        let usage = headless_usage_report_from_totals(usage_totals_for_messages(&new_messages));
        let result = HeadlessResultEvent {
            r#type: "result",
            subtype: if is_error {
                "error_during_execution"
            } else {
                "success"
            },
            duration_ms,
            duration_api_ms: duration_ms,
            is_error,
            num_turns: turn_count,
            stop_reason,
            session_id,
            total_cost_usd: 0.0,
            usage,
            permission_denials: Vec::new(),
            result: result_text,
            structured_output,
            errors: Vec::new(),
        };

        match contract.global.print_mode.output_mode {
            OutputMode::Text => last_result = Some(result),
            OutputMode::Json => last_result = Some(result),
            OutputMode::StreamJson => println!("{}", serde_json::to_string(&result)?),
        }
    }

    if let Some(result) = last_result {
        match contract.global.print_mode.output_mode {
            OutputMode::Text => {
                if !result.result.is_empty() {
                    println!("{}", result.result);
                } else if let Some(reason) = result.stop_reason {
                    println!("stopped: {reason}");
                }
            }
            OutputMode::Json => println!("{}", serde_json::to_string_pretty(&result)?),
            OutputMode::StreamJson => {}
        }
    }

    Ok(())
}

async fn serve_bridge_session_for_cli(
    store: &ActiveSessionStore,
    tool_registry: &ToolRegistry,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    raw_messages: &[Message],
    live_runtime: bool,
    cwd: &Path,
    runtime_options: &RuntimeCliOptions,
    bind_address: String,
) -> Result<BridgeSessionRecord> {
    let mode = remote_mode_for_address(&bind_address);
    let allow_remote_tools = true;
    let handler = LocalBridgeHandler {
        store,
        tool_registry,
        cwd: cwd.to_path_buf(),
        provider,
        active_model: active_model.to_owned(),
        session_id,
        raw_messages: raw_messages.to_vec(),
        live_runtime,
        runtime_options: runtime_options.clone(),
        allow_remote_tools,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };
    let config = BridgeServerConfig {
        bind_address,
        session_id: Some(session_id),
        allow_remote_tools,
    };

    match mode {
        RemoteMode::DirectConnect | RemoteMode::IdeBridge => serve_direct_session(config, handler).await,
        _ => serve_bridge_session(config, handler).await,
    }
}

async fn dispatch_auth_subcommand(
    provider: ApiProvider,
    cwd: &Path,
    store: &ActiveSessionStore,
    args: &[String],
) -> Result<()> {
    let wants_text = args.iter().any(|arg| arg == "--text");
    let subcommand = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .unwrap_or("status");

    match subcommand {
        "login" => println!("{}", render_auth_command(provider, "login").await?),
        "logout" => {
            println!(
                "{}",
                render_auth_command_with_resume(provider, "logout", latest_resume_hint(store).await?)
                    .await?
            );
        }
        "status" => {
            let resolver = EnvironmentAuthResolver;
            let auth = resolver
                .resolve_auth(AuthRequest {
                    provider,
                    profile: None,
                })
                .await
                .ok();
            let config_path = existing_managed_login_env_path(cwd);
            let snapshot_path = code_agent_auth_snapshot_path();
            let snapshot = snapshot_path.exists().then_some(snapshot_path);
            let report = AuthCommandReport {
                provider: provider.to_string(),
                status: if auth.is_some() {
                    "ready".to_owned()
                } else {
                    "missing".to_owned()
                },
                auth_source: auth.as_ref().and_then(|value| value.source.clone()),
                hint: Some(auth_hint_for_provider(provider)),
                config_path,
                snapshot_path: snapshot,
                resume_session_id: None,
                resume_transcript_path: None,
                resume_command: None,
            };
            if wants_text {
                println!("provider={}", report.provider);
                println!("status={}", report.status);
                if let Some(source) = report.auth_source.as_deref() {
                    println!("auth_source={source}");
                }
                if let Some(hint) = report.hint.as_deref() {
                    println!("hint={hint}");
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        other => bail!("unknown auth subcommand: {other}"),
    }

    Ok(())
}

async fn dispatch_top_level_command(
    contract: &CliContract,
    registry: &CommandRegistry,
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
    _auth_source: Option<String>,
    runtime_options: &RuntimeCliOptions,
) -> Result<bool> {
    if let Some(fast_path) = &contract.fast_path {
        match fast_path {
            FastPathCommand::SubcommandHelp { name } => {
                println!("{}", render_top_level_command_help_text(*name));
                return Ok(true);
            }
            FastPathCommand::Server(_) => {
                let parsed = parse_server_command_args(
                    match &contract.command {
                        TopLevelCommand::Named { args, .. } => args,
                        _ => &[],
                    },
                )?;
                let record = serve_bridge_session_for_cli(
                    store,
                    tool_registry,
                    provider,
                    active_model,
                    session_id,
                    raw_messages,
                    live_runtime,
                    cwd,
                    runtime_options,
                    parsed.bind_address,
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&record)?);
                return Ok(true);
            }
            FastPathCommand::Open { args } => {
                let parsed = parse_open_command_args(
                    args,
                    contract.global.print_mode.output_mode.clone(),
                    contract.global.print_mode.enabled,
                )?;
                let inbound = connect_remote_endpoint_for_cli(
                    cli,
                    &parsed.address,
                    session_id,
                    parsed.prompt,
                )
                .await?;
                print_remote_envelopes(&inbound, &parsed.output_mode)?;
                return Ok(true);
            }
            FastPathCommand::OpenUrl { .. } => {
                bail!(
                    "cc:// direct-connect URLs are not implemented yet in the Rust CLI"
                );
            }
            FastPathCommand::Ssh { args } => {
                ensure_ssh_command_supported(args)?;
            }
            FastPathCommand::Update(command) => {
                match command.name.as_str() {
                    "update" | "upgrade" => println!("{}", render_update_command()?),
                    "install" => println!("{}", render_install_command(&command.args)?),
                    "rollback" => println!("{}", render_rollback_command(&command.args)?),
                    "up" => {
                        bail!("project bootstrap via 'ccrust up' is not implemented yet in the Rust CLI")
                    }
                    other => bail!("top-level command '{other}' is not implemented yet in the Rust CLI"),
                }
                return Ok(true);
            }
            FastPathCommand::Completion { args } => {
                println!("{}", render_completion_command(args)?);
                return Ok(true);
            }
            FastPathCommand::RemoteControl { .. } => {}
        }
    }

    let TopLevelCommand::Named {
        name,
        command_name,
        args,
    } = &contract.command
    else {
        return Ok(false);
    };

    if name == "auth" {
        dispatch_auth_subcommand(provider, cwd, store, args).await?;
        return Ok(true);
    }

    let invocation = build_top_level_invocation(name, args);

    if let Some(resolved) = command_name {
        match resolved {
            TopLevelCommandName::Mcp => {
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
                return Ok(true);
            }
            TopLevelCommandName::Plugin => {
                println!(
                    "{}",
                    render_plugin_command(&invocation, cli.plugin_root.as_ref(), cwd).await?
                );
                return Ok(true);
            }
            TopLevelCommandName::SetupToken => {
                println!("{}", render_auth_command(provider, "login").await?);
                return Ok(true);
            }
            TopLevelCommandName::Agents => {
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
                return Ok(true);
            }
            TopLevelCommandName::RemoteControl => {
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
                return Ok(true);
            }
            TopLevelCommandName::Doctor => {
                println!("{}", render_doctor_command(cwd)?);
                return Ok(true);
            }
            TopLevelCommandName::Task => {
                println!("{}", render_task_cli_command(cwd, args)?);
                return Ok(true);
            }
            TopLevelCommandName::Export => {
                println!("{}", render_export_cli_command(store, args).await?);
                return Ok(true);
            }
            _ => {}
        }
    }

    Ok(false)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let exit_code = match run_main().await {
        Ok(()) => 0,
        Err(error) => {
            let message = error.to_string();
            if let Some(option) = message.strip_prefix("unknown option: ") {
                eprintln!("{}", render_unknown_option_error(option));
            } else {
                eprintln!("error: {message}");
            }
            1
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_main() -> Result<()> {
    let mut cli = parse_cli()?;
    let contract = build_cli_contract(&cli);
    let cwd = env::current_dir()?;
    apply_saved_ui_theme_preference();
    let mut login_config = apply_managed_login_env(&cwd);
    let startup_preferences = load_startup_preferences();
    let provider_selection =
        resolve_launch_provider(cli.provider.as_deref(), &startup_preferences, &login_config)?;
    let provider = provider_selection.provider;
    let prompt = (!cli.prompt.is_empty()).then(|| cli.prompt.join(" "));
    let runtime_options = cli.runtime_options();
    let tool_registry = compatibility_tool_registry();
    let store = ActiveSessionStore::new(
        cwd.clone(),
        cli.session_root
            .clone()
            .or_else(|| env::var_os("CLAUDE_CODE_SESSION_DIR").map(PathBuf::from)),
    );

    let registry = resolved_command_registry(&cwd, cli.plugin_root.as_ref()).await;

    if cli.list_commands {
        println!("{}", render_root_help_text());
        return Ok(());
    }

    if cli.list_sessions {
        println!(
            "{}",
            serde_json::to_string_pretty(&store.list_sessions().await?)?
        );
        return Ok(());
    }

    if cli.show_plugin {
        let root = resolve_plugin_root(&cli, None, &cwd);
        println!(
            "{}",
            serde_json::to_string_pretty(&load_plugin_report(root).await?)?
        );
        return Ok(());
    }

    if cli.list_skills {
        let skills = resolved_skill_entries(&cwd, cli.plugin_root.as_ref()).await?;
        println!("{}", serde_json::to_string_pretty(&skills)?);
        return Ok(());
    }

    if cli.list_mcp {
        let runtime = OutOfProcessPluginRuntime;
        let root = resolve_plugin_root(&cli, None, &cwd);
        let plugin = runtime.load_manifest(&root).await?;
        let parsed = parse_mcp_server_configs(&plugin.manifest.mcp_servers);
        println!("{}", serde_json::to_string_pretty(&parsed)?);
        return Ok(());
    }

    if let Some(FastPathCommand::SubcommandHelp { name }) = &contract.fast_path {
        println!("{}", render_top_level_command_help_text(*name));
        return Ok(());
    }

    resolve_continue_target(&mut cli, &store).await?;

    if let Some(address) = cli.bridge_connect.clone() {
        let session_id = cli
            .resume
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            .unwrap_or_else(Uuid::new_v4);
        let mode = remote_mode_for_address(&address);
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(mode.clone()),
                scheme: match mode {
                    RemoteMode::DirectConnect => "tcp".to_owned(),
                    RemoteMode::IdeBridge => "ide".to_owned(),
                    _ => "ws".to_owned(),
                },
                address,
                session_id: Some(session_id),
                ..RemoteEndpoint::default()
            },
            build_remote_outbound(&cli, session_id, prompt.clone(), cli.resume.as_deref())?,
            cli.bridge_receive_count.unwrap_or(1),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&inbound)?);
        return Ok(());
    }

    if let Some(target) = cli.clear_session.as_deref() {
        let (_, path, _) = store.load_resume_target(target).await?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        println!("{}", json!({ "cleared": path }));
        return Ok(());
    }

    if let Some(tool_name) = cli.tool.as_deref() {
        let report = run_tool(
            tool_name,
            parse_input(cli.input.as_deref())?,
            cwd.clone(),
            provider,
            cli.model.clone(),
            runtime_options.tool_permission_mode.clone(),
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let auth_resolver = EnvironmentAuthResolver;
    let auth = auth_resolver
        .resolve_auth(AuthRequest {
            provider,
            profile: None,
        })
        .await
        .ok();
    let auth_source = auth.as_ref().and_then(|value| value.source.clone());

    let explicit_resume = match cli.resume.as_deref() {
        Some(target) => Some(store.load_resume_target(target).await?),
        None => None,
    };
    let (session_id, transcript_path, mut existing_messages) =
        choose_active_session(&cli, explicit_resume)?;
    apply_resume_message_cutoff(
        &mut existing_messages,
        contract.global.resume.resume_session_at.as_deref(),
    )?;
    let command_settings = load_command_settings();
    let active_model = cli
        .model
        .clone()
        .or_else(|| preferred_model_for_provider(provider, &command_settings))
        .or_else(|| {
            compatibility_model_catalog(provider)
                .list_models()
                .first()
                .map(|model| model.id.clone())
        })
        .ok_or_else(|| anyhow!("no compatibility model catalog entries for {provider}"))?;
    let live_runtime = auth.is_some() && provider_supports_live_runtime(provider);

    if dispatch_top_level_command(
        &contract,
        &registry,
        &cli,
        &store,
        &tool_registry,
        provider,
        cli.model.clone(),
        &active_model,
        session_id,
        &existing_messages,
        live_runtime,
        &cwd,
        auth_source.clone(),
        &runtime_options,
    )
    .await?
    {
        return Ok(());
    }

    if let Some(bind_address) = cli.bridge_server.clone() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let record = serve_bridge_session_for_cli(
            &store,
            &tool_registry,
            provider,
            &active_model,
            session_id,
            &existing_messages,
            live_runtime,
            &cwd,
            &runtime_options,
            bind_address,
        )
        .await?;
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    let term_program = env::var("TERM_PROGRAM").ok();
    let _ = maybe_notify_auto_connected_ide(
        &cwd,
        cli.compat_flag_enabled("ide"),
        None,
        term_program.as_deref(),
        ide_env_port(),
    )
    .await;

    if contract.global.print_mode.enabled {
        run_root_print_mode(
            &contract,
            &store,
            &tool_registry,
            &cwd,
            cli.plugin_root.as_ref(),
            provider,
            &active_model,
            session_id,
            &mut existing_messages,
            prompt.as_deref(),
            live_runtime,
            &runtime_options,
        )
        .await?;
        return Ok(());
    }

    if cli.tui && prompt.is_none() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let runtime_messages = materialize_runtime_messages(&existing_messages);
        let session_usage_totals = usage_totals_for_messages(&existing_messages);
        let total_usage_totals = usage_totals_for_store(&store).await?;
        let title = format!("{provider}  {active_model}");
        let app = RatatuiApp::new(title);
        let mut state = app.state_from_messages(runtime_messages, &registry.all());
        apply_repl_header_with_usage(
            &mut state,
            provider,
            &active_model,
            &cwd,
            session_id,
            session_usage_totals,
            total_usage_totals,
        );
        state.status_line = repl_status(provider, &active_model, session_id);
        if let Some(path) = transcript_path.as_ref() {
            state.compact_banner = Some(format!("resume {}", shorten_path(path, 72)));
        }
        let (width, height) = terminal_size().unwrap_or((100, 28));
        println!("{}", render_tui_to_string(&state, width, height)?);
        return Ok(());
    }

    if should_launch_interactive_repl(&cli, prompt.as_deref()) {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let final_session_id = run_interactive_repl(
            &store,
            &registry,
            &tool_registry,
            cwd.clone(),
            cli.plugin_root.as_ref(),
            provider,
            active_model.clone(),
            session_id,
            &mut existing_messages,
            live_runtime,
            &runtime_options,
            transcript_path.clone(),
            provider_selection.configured,
            matches!(
                provider_selection.source,
                LaunchProviderSource::Default | LaunchProviderSource::Preference
            ),
            &mut login_config,
            remote_mode_enabled(&cli),
            ide_bridge_enabled(&cli),
        )
        .await?;
        if let Ok(resume_hint) = current_resume_hint(&store, final_session_id).await {
            print_resume_hint(&resume_hint);
        }
        return Ok(());
    }

    if let Some(mut prompt_text) = prompt.clone() {
        let mut prompt_command_user_message = None;
        if let Some(invocation) = registry.parse_slash_command(&prompt_text) {
            if let Some(prompt_command_execution) = resolve_prompt_command_execution(
                &registry,
                &invocation,
                &cwd,
                cli.plugin_root.as_ref(),
                session_id,
            )? {
                prompt_text = prompt_command_execution.expanded_prompt.clone();
                prompt_command_user_message = Some(build_prompt_command_user_message(
                    session_id,
                    existing_messages.last().map(|message| message.id),
                    prompt_command_execution.raw_input,
                    prompt_command_execution.transcript_text,
                    prompt_command_execution.expanded_prompt,
                ));
            } else {
            handle_slash_command(
                &registry,
                invocation,
                &cli,
                &store,
                &tool_registry,
                provider,
                cli.model.clone(),
                &active_model,
                session_id,
                &existing_messages,
                live_runtime,
                &cwd,
                auth_source,
            )
            .await?;
            return Ok(());
            }
        }

        let _transcript_path = match transcript_path {
            Some(path) => path,
            None => store.transcript_path(session_id).await?,
        };
        let parent_id = existing_messages.last().map(|message| message.id);
        let user_message = prompt_command_user_message.unwrap_or_else(|| {
            build_text_message(
                session_id,
                MessageRole::User,
                prompt_text.clone(),
                parent_id,
            )
        });
        store.append_message(session_id, &user_message).await?;
        existing_messages.push(user_message);
        let _applied_compaction =
            maybe_auto_compact(&store, session_id, &mut existing_messages).await?;
        let mut runtime_messages = materialize_runtime_messages(&existing_messages);

        let (_assistant_usage, _turn_count, stop_reason) = run_agent_turns(
            &store,
            &tool_registry,
            cwd.clone(),
            cli.plugin_root.as_ref(),
            provider,
            active_model.clone(),
            session_id,
            &mut runtime_messages,
            live_runtime,
            &runtime_options,
            None,
        )
        .await?;

        let updated_messages = store.load_session(session_id).await.unwrap_or_default();
        if let Some(last_assistant) = updated_messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(message_text)
            .filter(|text| !text.trim().is_empty())
        {
            println!("{last_assistant}");
        } else if let Some(reason) = stop_reason {
            println!("stopped: {reason}");
        }
        return Ok(());
    }
    Ok(())
}

fn should_launch_interactive_repl(cli: &Cli, prompt: Option<&str>) -> bool {
    cli.repl || (!cli.tui && prompt.is_none())
}
