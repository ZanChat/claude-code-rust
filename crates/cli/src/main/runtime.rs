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
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
) -> Result<(Option<code_agent_core::TokenUsage>, usize, Option<String>)> {
    const MAX_AGENT_STEPS: usize = 100;

    let provider_tools = tool_definitions(tool_registry);
    let system_prompt = build_runtime_system_prompt(&cwd, tool_registry, provider, plugin_root);
    let tool_context = ToolContext {
        session_id: Some(session_id),
        cwd: cwd.clone(),
        provider: Some(provider.to_string()),
        model: Some(model.clone()),
        ..ToolContext::default()
    };

    for step in 1..=MAX_AGENT_STEPS {
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
        let provider_client = resolve_provider_client(provider, auth_configured).await?;
        let parent_id = messages.last().map(|message| message.id);
        let request_messages = provider_request_messages(&system_prompt, messages);
        let mut stream = provider_client
            .start_stream(ProviderRequest {
                model: model.clone(),
                messages: request_messages,
                tools: provider_tools.clone(),
                ..ProviderRequest::default()
            })
            .await?;
        let mut response_text = String::new();
        let mut response_tool_calls = Vec::new();
        let mut latest_usage = None;
        let mut stop_reason = None;

        while let Some(event) = stream.next_event().await? {
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
                ProviderEvent::Error { message } => return Err(anyhow!(message)),
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
            let input = serde_json::from_str(&call.input_json).unwrap_or_else(|_| json!({}));
            let output = tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: call.name.clone(),
                        input,
                    },
                    &tool_context,
                )
                .await?;
            let output_content = output.content;
            let output_is_error = output.is_error;
            let output_metadata = output.metadata;
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
        }
    }

    Err(anyhow!("agent loop exceeded tool iteration limit"))
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
    let parent_id = raw_messages.last().map(|message| message.id);
    let user_message = build_text_message(session_id, MessageRole::User, prompt_text, parent_id);
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
    let (_, turn_count, stop_reason) = run_agent_turns(
        store,
        tool_registry,
        cwd,
        plugin_root,
        provider,
        active_model,
        session_id,
        &mut runtime_messages,
        live_runtime,
        pending_view.as_ref(),
    )
    .await?;
    *raw_messages = store.load_session(session_id).await.unwrap_or_default();

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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut cli = parse_cli();
    let provider = resolve_api_provider(cli.provider.as_deref())?;
    let cwd = env::current_dir()?;
    let project_dir = get_project_dir(&cwd);
    let prompt = (!cli.prompt.is_empty()).then(|| cli.prompt.join(" "));
    let tool_registry = compatibility_tool_registry();
    let store = ActiveSessionStore::new(
        cwd.clone(),
        cli.session_root
            .clone()
            .or_else(|| env::var_os("CLAUDE_CODE_SESSION_DIR").map(PathBuf::from)),
    );

    let registry = resolved_command_registry(&cwd, cli.plugin_root.as_ref()).await;

    if cli.list_commands {
        println!("{}", render_command_help(&registry, false));
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
        let runtime = OutOfProcessPluginRuntime;
        let root = resolve_plugin_root(&cli, None, &cwd);
        let skills = runtime.discover_skills(&root).await?;
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
    let parsed_command = prompt
        .as_deref()
        .and_then(|input| registry.parse_slash_command(input))
        .map(|command| {
            if command.args.is_empty() {
                command.name
            } else {
                format!("{} {}", command.name, command.args.join(" "))
            }
        });

    let explicit_resume = match cli.resume.as_deref() {
        Some(target) => Some(store.load_resume_target(target).await?),
        None => None,
    };
    let (session_id, transcript_path, mut existing_messages) =
        choose_active_session(&cli, explicit_resume)?;
    let active_model = cli
        .model
        .clone()
        .or_else(|| {
            compatibility_model_catalog(provider)
                .list_models()
                .first()
                .map(|model| model.id.clone())
        })
        .ok_or_else(|| anyhow!("no compatibility model catalog entries for {provider}"))?;
    let live_runtime = auth.is_some() && provider_supports_live_runtime(provider);

    if let Some(bind_address) = cli.bridge_server.clone() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let mode = remote_mode_for_address(&bind_address);
        let allow_remote_tools = true;
        let handler = LocalBridgeHandler {
            store: &store,
            tool_registry: &tool_registry,
            cwd: cwd.clone(),
            provider,
            active_model: active_model.clone(),
            session_id,
            raw_messages: existing_messages,
            live_runtime,
            allow_remote_tools,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        let config = BridgeServerConfig {
            bind_address,
            session_id: Some(session_id),
            allow_remote_tools,
        };
        let record = match mode {
            RemoteMode::DirectConnect | RemoteMode::IdeBridge => {
                serve_direct_session(config, handler).await?
            }
            _ => serve_bridge_session(config, handler).await?,
        };
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    if cli.tui && prompt.is_none() {
        if existing_messages.is_empty() && transcript_path.is_some() {
            existing_messages = store.load_session(session_id).await.unwrap_or_default();
        }
        let runtime_messages = materialize_runtime_messages(&existing_messages);
        let title = format!("{provider}  {active_model}");
        let app = RatatuiApp::new(title);
        let mut state = app.state_from_messages(runtime_messages, &registry.all());
        apply_repl_header(&mut state, provider, &active_model, &cwd, session_id);
        state.status_line = repl_status(provider, &active_model, session_id);
        if let Some(path) = transcript_path.as_ref() {
            state.compact_banner = Some(format!("resume {}", shorten_path(path, 72)));
        }
        let (width, height) = terminal_size().unwrap_or((100, 28));
        println!("{}", render_tui_to_string(&state, width, height)?);
        return Ok(());
    }

    if cli.repl {
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
            auth_source.clone(),
            transcript_path.clone(),
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
        if let Some(invocation) = registry.parse_slash_command(&prompt_text) {
            if let Some(expanded_prompt) = resolve_prompt_command_prompt(
                &registry,
                &invocation,
                &cwd,
                cli.plugin_root.as_ref(),
                session_id,
            )? {
                prompt_text = expanded_prompt;
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

        let transcript_path = match transcript_path {
            Some(path) => path,
            None => store.transcript_path(session_id).await?,
        };
        let parent_id = existing_messages.last().map(|message| message.id);
        let user_message = build_text_message(
            session_id,
            MessageRole::User,
            prompt_text.clone(),
            parent_id,
        );
        store.append_message(session_id, &user_message).await?;
        existing_messages.push(user_message.clone());
        let estimated_tokens_before =
            estimate_message_tokens(&materialize_runtime_messages(&existing_messages));
        let applied_compaction =
            maybe_auto_compact(&store, session_id, &mut existing_messages).await?;
        let estimated_tokens_after = applied_compaction
            .as_ref()
            .map(|outcome| outcome.estimated_tokens_after)
            .or(Some(estimate_message_tokens(
                &materialize_runtime_messages(&existing_messages),
            )));
        let mut runtime_messages = materialize_runtime_messages(&existing_messages);

        let (_assistant_usage, turn_count, stop_reason) = run_agent_turns(
            &store,
            &tool_registry,
            cwd.clone(),
            cli.plugin_root.as_ref(),
            provider,
            active_model.clone(),
            session_id,
            &mut runtime_messages,
            live_runtime,
            None,
        )
        .await?;

        let report = StartupReport {
            provider: provider.to_string(),
            model: Some(active_model),
            cwd,
            project_dir,
            session_root: store.root_dir().to_path_buf(),
            command_count: registry.all().len(),
            prompt: Some(prompt_text),
            parsed_command: None,
            active_session_id: Some(session_id),
            transcript_path: Some(transcript_path),
            auth_source: auth_source.clone(),
            turn_count,
            stop_reason,
            applied_compaction: applied_compaction.as_ref().and_then(compaction_kind_name),
            estimated_tokens_before: Some(estimated_tokens_before),
            estimated_tokens_after,
            note: "Provider-backed runtime is active. Sessions, compaction, slash commands, tool execution, TUI REPL, MCP transport execution, bridge server/client flows, and multi-step agent turns now persist locally.",
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let report = StartupReport {
        provider: provider.to_string(),
        model: cli.model.clone(),
        cwd,
        project_dir,
        session_root: store.root_dir().to_path_buf(),
        command_count: registry.all().len(),
        prompt,
        parsed_command,
        active_session_id: Some(session_id),
        transcript_path,
        auth_source,
        turn_count: 0,
        stop_reason: None,
        applied_compaction: None,
        estimated_tokens_before: None,
        estimated_tokens_after: None,
        note: "Local runtime shell is active. Use --list-sessions, --resume, --tool, --repl, or a slash command prompt to exercise persisted sessions, tools, plugins, MCP, and remote-control flows.",
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

