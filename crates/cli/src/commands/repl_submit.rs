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
        "ide" => render_ide_command(cwd, ide_bridge_active, None),
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
    ide_picker: &mut Option<ReplIdePickerState>,
    connected_ide_bridge: &Option<DetectedIdeCandidate>,
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

        if invocation.name == "ide" && invocation.args.is_empty() {
            *ide_picker = Some(repl_ide_picker_state(cwd, connected_ide_bridge.as_ref()));
            *status_line = repl_status(provider, active_model, repl_session.session_id);
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
            prompt_history_index,
            prompt_history_draft,
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
        prompt_history_index,
        prompt_history_draft,
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

