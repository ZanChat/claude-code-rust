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
    vim_state: &mut ccrust_ui::vim::VimState,
    remote_mode: bool,
    ide_bridge_active: bool,
) -> Result<String> {
    handle_repl_slash_command_with_options(
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
        &RuntimeCliOptions::default(),
        vim_state,
        remote_mode,
        ide_bridge_active,
    )
    .await
}

async fn handle_repl_slash_command_with_options(
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
    runtime_options: &RuntimeCliOptions,
    vim_state: &mut ccrust_ui::vim::VimState,
    remote_mode: bool,
    ide_bridge_active: bool,
) -> Result<String> {
    if !command_allowed_in_repl(registry, remote_mode, &invocation.name) {
        return Ok(format!(
            "command '/{}' is unavailable in remote mode",
            invocation.name
        ));
    }
    if let Some(prompt_command_execution) = resolve_prompt_command_execution(
        registry,
        &invocation,
        cwd,
        plugin_root,
        repl_session.session_id,
    )? {
        let user_message = build_prompt_command_user_message(
            repl_session.session_id,
            raw_messages.last().map(|message| message.id),
            prompt_command_execution.raw_input,
            prompt_command_execution.transcript_text,
            prompt_command_execution.expanded_prompt,
        );
        let (applied_compaction, turn_count, stop_reason, _, _) =
            execute_local_turn_with_user_message_options(
                store,
                tool_registry,
                cwd.to_path_buf(),
                plugin_root,
                provider,
                active_model.clone(),
                repl_session.session_id,
                raw_messages,
                user_message,
                live_runtime,
                runtime_options,
                None,
            )
            .await?;
        let detail = if let Some(kind) = applied_compaction.as_ref().and_then(compaction_kind_name)
        {
            format!("{turn_count} steps · {:?} · compact {kind}", stop_reason)
        } else {
            format!("{turn_count} steps · {:?}", stop_reason)
        };
        return Ok(detail);
    }

    if let Some(spec) = registry.resolve(&invocation.name) {
        if spec.kind == ccrust_core::CommandKind::Prompt {
            let location = spec
                .origin
                .as_deref()
                .map(|origin| format!(" ({origin})"))
                .unwrap_or_default();
            return Ok(format!(
                "command '/{}' is registered but its prompt is empty or unreadable{}",
                invocation.name, location
            ));
        }
    }

    match invocation.name.as_str() {
        "help" => Ok(render_command_help(registry, remote_mode)),
        "version" => Ok(env!("CARGO_PKG_VERSION").to_owned()),
        "add-dir" => render_add_dir_command(cwd),
        "branch" => render_branch_command(store, repl_session, &invocation, raw_messages).await,
        "config" => {
            if matches!(invocation.args.first().map(String::as_str), Some("migrate")) {
                Ok(serde_json::to_string_pretty(&config_migration_report(provider))?)
            } else {
                let mut parts = vec![
                    format!("provider={provider}"),
                    format!("model={active_model}"),
                    format!("session={}", repl_session.session_id),
                    format!("runtime={}", if live_runtime { "live" } else { "offline" }),
                ];
                if ccrust_providers::is_openai_provider(provider) {
                    parts.push(format!(
                        "api_mode={}",
                        ccrust_providers::get_openai_api_mode(provider).as_str()
                    ));
                    parts.push(format!(
                        "transport={}",
                        ccrust_providers::get_openai_transport_mode().as_str()
                    ));
                } else if provider == ApiProvider::Gemini {
                    parts.push(format!(
                        "base_url={}",
                        ccrust_providers::get_gemini_base_url()
                    ));
                }
                Ok(parts.join(" "))
            }
        }
        "ide" => render_ide_command(cwd, ide_bridge_active, None),
        "model" => {
            let Some(model) = invocation.args.first() else {
                return Ok(format!("current model={active_model}"));
            };
            let catalog = compatibility_model_catalog(provider);
            if !matches!(provider, ApiProvider::OpenAICompatible | ApiProvider::Gemini)
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
            let _ = delete_session_metadata_for_path(&transcript_path)?;
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
        "hooks" => render_hooks_command(cwd, plugin_root),
        "feedback" => render_feedback_command(),
        "install-github-app" => render_install_github_app_command(),
        "longtask" => render_longtask_command(),
        "output-style" => render_output_style_command(),
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
        "color" => render_color_command(&invocation),
        "theme" => render_theme_command(&invocation),
        "vim" => {
            vim_state.enabled = !vim_state.enabled;
            if vim_state.enabled {
                vim_state.enter_normal();
            } else {
                vim_state.mode = ccrust_ui::vim::VimMode::Insert;
            }
            render_vim_command(vim_state.enabled)
        }
        "plan" => render_plan_command(cwd, &invocation),
        "doctor" => render_doctor_command(cwd),
        "fast" => {
            let outcome = render_fast_command(&invocation, provider, active_model)?;
            if let Some(model) = outcome.next_model {
                *active_model = model;
            }
            Ok(outcome.message)
        }
        "passes" => render_passes_command(),
        "effort" => render_effort_command(cwd, &invocation),
        "context" => render_context_command(raw_messages, provider, active_model),
        "tag" => render_tag_command(store, repl_session.session_id, &invocation).await,
        "rename" => {
            render_rename_command(store, repl_session.session_id, &invocation, raw_messages).await
        }
        "rewind" => {
            render_rewind_command(store, repl_session.session_id, &invocation, raw_messages).await
        }
        "remote-env" => render_simple_compat_command(
            "remote-env",
            "Remote environment reporting currently flows through bridge and session status surfaces.",
        ),
        "export" => render_export_command(store, repl_session.session_id),
        "mobile" => render_mobile_command(store, repl_session.session_id).await,
        "desktop" => render_desktop_command(store, repl_session.session_id).await,
        "chrome" => render_chrome_command(&invocation),
        "release-notes" => render_release_notes_command(cwd),
        "reload-auth" => render_reload_auth_command(provider),
        "sandbox" => render_sandbox_command(),
        "stickers" => render_simple_compat_command(
            "stickers",
            "Sticker ordering is not available in the Rust runtime.",
        ),
        "terminal-setup" => render_terminal_setup_command(),
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
        "advisor" => render_advisor_command(&invocation),
        "voice" => render_voice_command(),
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
    registry: &ccrust_core::CommandRegistry,
    tool_registry: &ToolRegistry,
    cwd: &PathBuf,
    plugin_root: Option<&PathBuf>,
    provider: &mut ApiProvider,
    active_model: &mut String,
    repl_session: &mut ReplSessionState,
    raw_messages: &mut Vec<Message>,
    live_runtime: &mut bool,
    runtime_options: &RuntimeCliOptions,
    prompt_text: String,
    total_usage_totals: UsageTotals,
    input_buffer: &mut ccrust_ui::InputBuffer,
    prompt_history: &mut Vec<String>,
    global_prompt_history: &mut Vec<String>,
    prompt_history_index: &mut Option<usize>,
    prompt_history_draft: &mut Option<ccrust_ui::InputBuffer>,
    transcript_scroll: &mut u16,
    status_line: &mut String,
    status_marquee_tick: &mut usize,
    active_pane: &mut PaneKind,
    compact_banner: &mut Option<String>,
    interaction_state: &mut ReplInteractionState,
    resume_picker: &mut Option<ResumePickerState>,
    ide_picker: &mut Option<ReplIdePickerState>,
    command_picker: &mut Option<ReplCommandPickerState>,
    connected_ide_bridge: &Option<DetectedIdeCandidate>,
    selected_command_suggestion: &mut usize,
    vim_state: &mut ccrust_ui::vim::VimState,
    login_config: &mut ManagedLoginConfigState,
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
                    repl_runtime_status(
                        *provider,
                        active_model,
                        repl_session.session_id,
                        *live_runtime,
                    ),
                    "No conversations found to resume",
                );
            } else {
                *resume_picker = Some(ResumePickerState {
                    sessions,
                    selected: 0,
                });
                *status_line = repl_runtime_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
                );
            }
            *status_marquee_tick = 0;
            return Ok(ReplSubmissionOutcome::Continue);
        }

        if invocation.name == "ide" && invocation.args.is_empty() {
            *ide_picker = Some(repl_ide_picker_state(cwd, connected_ide_bridge.as_ref()));
            *status_line = repl_runtime_status(
                *provider,
                active_model,
                repl_session.session_id,
                *live_runtime,
            );
            *status_marquee_tick = 0;
            return Ok(ReplSubmissionOutcome::Continue);
        }

        if invocation.args.is_empty() {
            let next_picker = match invocation.name.as_str() {
                "agents" => Some(repl_agents_picker_state(cwd)?),
                "skills" | "reload-plugins" => {
                    Some(repl_skills_picker_state(cwd, plugin_root).await?)
                }
                "theme" => Some(repl_theme_picker_state()),
                "fast" => Some(repl_fast_picker_state(active_model)),
                "effort" => Some(repl_effort_picker_state()),
                "rewind" => Some(repl_rewind_picker_state(raw_messages)),
                "hooks" => Some(repl_hooks_picker_state(cwd, plugin_root)),
                _ => None,
            };

            if let Some(picker) = next_picker {
                *command_picker = Some(picker);
                *status_line = repl_runtime_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
                );
                *status_marquee_tick = 0;
                return Ok(ReplSubmissionOutcome::Continue);
            }
        }

        if matches!(invocation.name.as_str(), "login" | "logout") {
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
                        command_input,
                    ),
                )
                .await?;
            }

            let next_status = if command_name == "login" {
                match run_login_onboarding_flow(
                    terminal,
                    cwd,
                    *provider,
                    active_model,
                    repl_session.session_id,
                    login_config.tracked_path.as_deref(),
                )? {
                    Some(login_outcome) => {
                        apply_runtime_login_values(
                            login_config,
                            login_outcome.config_path.clone(),
                            login_outcome.env_values,
                        );
                        *provider = login_outcome.provider;
                        if let Some(default_model) = default_model_for_provider(*provider) {
                            *active_model = default_model;
                        }
                        let auth = EnvironmentAuthResolver
                            .resolve_auth(AuthRequest {
                                provider: *provider,
                                profile: None,
                            })
                            .await
                            .ok();
                        *live_runtime = auth.is_some() && provider_supports_live_runtime(*provider);
                        format!(
                            "saved login config {}",
                            shorten_path(&login_outcome.config_path, 72)
                        )
                    }
                    None => "login onboarding cancelled".to_owned(),
                }
            } else {
                let mut status_parts = Vec::new();
                if let Some(path) = login_config.tracked_path.clone() {
                    if clear_managed_login_env_file(&path)? {
                        status_parts.push(format!("cleared {}", shorten_path(&path, 72)));
                    } else {
                        status_parts
                            .push(format!("no managed values in {}", shorten_path(&path, 72)));
                    }
                    restore_runtime_login_values(login_config);
                } else {
                    status_parts.push("no tracked login config".to_owned());
                }
                let _ = clear_auth_snapshot(*provider)?;
                let auth = EnvironmentAuthResolver
                    .resolve_auth(AuthRequest {
                        provider: *provider,
                        profile: None,
                    })
                    .await
                    .ok();
                *live_runtime = auth.is_some() && provider_supports_live_runtime(*provider);
                status_parts.join(" · ")
            };

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
            *status_line = slash_command_footer_status(
                *provider,
                active_model,
                repl_session.session_id,
                *live_runtime,
                &command_name,
                command_recorded,
                false,
                &next_status,
            );
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
        let base_status_line = repl_runtime_status(
            *provider,
            active_model,
            repl_session.session_id,
            *live_runtime,
        );
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
            store,
            registry,
            tool_registry,
            pending_view.clone(),
            cwd,
            plugin_root,
            *provider,
            &active_model_display,
            repl_session.session_id,
            *live_runtime,
            total_usage_totals,
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
            handle_repl_slash_command_with_options(
                registry,
                invocation,
                store,
                tool_registry,
                cwd,
                plugin_root,
                *provider,
                active_model,
                repl_session,
                raw_messages,
                *live_runtime,
                runtime_options,
                vim_state,
                remote_mode,
                ide_bridge_active,
            ),
        )
        .await;
        let pending_inputs = take_pending_repl_inputs(&pending_view);
        let should_append_interrupt_message =
            should_append_pending_interrupt_message(&pending_inputs);
        queued_submissions.extend(pending_inputs);

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
                    *global_prompt_history =
                        global_prompt_history_from_store(store, repl_session.session_id)
                            .await
                            .unwrap_or_default();
                }
                *prompt_history = combined_prompt_history(
                    &prompt_history_from_messages(raw_messages),
                    global_prompt_history,
                );
                reset_prompt_history_navigation(prompt_history_index, prompt_history_draft);
                if repl_session.session_id != previous_session_id {
                    *transcript_scroll = 0;
                    *compact_banner = repl_session
                        .transcript_path
                        .as_ref()
                        .map(|path| format!("resume {}", shorten_path(path, 72)));
                }
                *status_line = slash_command_footer_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
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
                if should_append_interrupt_message {
                    let interruption_messages = pending_interrupt_messages(
                        repl_session.session_id,
                        raw_messages,
                        &pending_repl_snapshot(&pending_view),
                    );
                    append_session_messages(store, raw_messages, interruption_messages).await?;
                }
                *status_line = status_with_detail(
                    repl_runtime_status(
                        *provider,
                        active_model,
                        repl_session.session_id,
                        *live_runtime,
                    ),
                    "Interrupted by user",
                );
                *status_marquee_tick = 0;
            }
            Err(error) => {
                let error_detail = format!("error: {error}");
                *status_line = slash_command_footer_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
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

    let base_status_line = repl_runtime_status(
        *provider,
        active_model,
        repl_session.session_id,
        *live_runtime,
    );
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
        store,
        registry,
        tool_registry,
        pending_view.clone(),
        cwd,
        plugin_root,
        *provider,
        active_model,
        repl_session.session_id,
        *live_runtime,
        total_usage_totals,
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
        execute_local_turn_with_options(
            store,
            tool_registry,
            cwd.clone(),
            plugin_root,
            *provider,
            active_model.clone(),
            repl_session.session_id,
            raw_messages,
            prompt_text,
            *live_runtime,
            runtime_options,
            Some(pending_view.clone()),
        ),
    )
    .await;
    let pending_inputs = take_pending_repl_inputs(&pending_view);
    let should_append_interrupt_message =
        should_append_pending_interrupt_message(&pending_inputs);
    queued_submissions.extend(pending_inputs);

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
                repl_runtime_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
                ),
                detail,
            );
            *status_marquee_tick = 0;
        }
        Err(error) => {
            *status_line = status_with_detail(
                repl_runtime_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
                ),
                format!("error: {error}"),
            );
            *status_marquee_tick = 0;
        }
        Ok(PendingReplOperationResult::Interrupted) => {
            if should_append_interrupt_message {
                let interruption_messages = pending_interrupt_messages(
                    repl_session.session_id,
                    raw_messages,
                    &pending_repl_snapshot(&pending_view),
                );
                append_session_messages(store, raw_messages, interruption_messages).await?;
            }
            *status_line = status_with_detail(
                repl_runtime_status(
                    *provider,
                    active_model,
                    repl_session.session_id,
                    *live_runtime,
                ),
                "Interrupted by user",
            );
            *status_marquee_tick = 0;
        }
    }

    Ok(ReplSubmissionOutcome::Continue)
}
