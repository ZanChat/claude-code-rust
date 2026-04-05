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
        "ide" => println!("{}", render_ide_command(cwd, ide_bridge_enabled(cli), ide_bridge_address(cli))?),
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
