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
        "add-dir" => println!("{}", render_add_dir_command(cwd)?),
        "branch" => {
            let requested_title = invocation_argument_string(&invocation);
            let messages = if raw_messages.is_empty() {
                store.load_session(session_id).await.unwrap_or_default()
            } else {
                raw_messages.to_vec()
            };
            let outcome = create_branch_session(
                store,
                session_id,
                &messages,
                requested_title.as_deref(),
            )
            .await?;
            println!("{}", render_branch_command_message(&outcome, false));
        }
        "session" => println!("{}", render_session_command(store, session_id).await?),
        "permissions" => println!("{}", render_permissions_command(cwd).await?),
        "status" => println!("{}", render_status_command(provider, active_model, session_id, live_runtime, cwd)?),
        "ide" => println!("{}", render_ide_command(cwd, ide_bridge_enabled(cli), ide_bridge_address(cli))?),
        "statusline" => println!("{}", render_statusline_command(provider, active_model, session_id)?),
        "color" => println!("{}", render_color_command(&invocation)?),
        "theme" => println!("{}", render_theme_command(&invocation)?),
        "vim" => println!("{}", render_vim_command(false)?),
        "plan" => println!("{}", render_plan_command(cwd, &invocation)?),
        "doctor" => println!("{}", render_doctor_command(cwd)?),
        "fast" => println!("{}", render_fast_command(&invocation, provider, active_model)?.message),
        "passes" => println!("{}", render_passes_command()?),
        "effort" => println!("{}", render_effort_command(cwd, &invocation)?),
        "context" => println!("{}", render_context_command(raw_messages, provider, active_model)?),
        "tag" => println!("{}", render_tag_command(store, session_id, &invocation).await?),
        "rename" => {
            let messages = if raw_messages.is_empty() {
                store.load_session(session_id).await.unwrap_or_default()
            } else {
                raw_messages.to_vec()
            };
            println!("{}", render_rename_command(store, session_id, &invocation, &messages).await?);
        }
        "rewind" => {
            let mut messages = if raw_messages.is_empty() {
                store.load_session(session_id).await.unwrap_or_default()
            } else {
                raw_messages.to_vec()
            };
            println!("{}", render_rewind_command(store, session_id, &invocation, &mut messages).await?);
        }
        "skills" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "reload-plugins" => println!("{}", render_skills_command(cwd, cli.plugin_root.as_ref()).await?),
        "hooks" => println!("{}", render_hooks_command(cwd, cli.plugin_root.as_ref())?),
        "feedback" => println!("{}", render_feedback_command()?),
        "install-github-app" => println!("{}", render_install_github_app_command()?),
        "longtask" => println!("{}", render_longtask_command()?),
        "mobile" => println!("{}", render_mobile_command(store, session_id).await?),
        "desktop" => println!("{}", render_desktop_command(store, session_id).await?),
        "chrome" => println!("{}", render_chrome_command(&invocation)?),
        "release-notes" => println!("{}", render_release_notes_command(cwd)?),
        "reload-auth" => println!("{}", render_reload_auth_command(provider)?),
        "sandbox" => println!("{}", render_sandbox_command()?),
        "stickers" => println!(
            "{}",
            render_simple_compat_command(
                "stickers",
                "Sticker ordering is not available in the Rust runtime.",
            )?
        ),
        "terminal-setup" => println!("{}", render_terminal_setup_command()?),
        "output-style" => println!("{}", render_output_style_command()?),
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
            let _ = delete_session_metadata_for_path(&path)?;
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
        "advisor" => println!("{}", render_advisor_command(&invocation)?),
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
        "voice" => println!("{}", render_voice_command()?),
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
