use super::*;

#[test]
fn render_ide_command_detects_matching_lockfile() {
    let home = temp_session_root("ide-lockfile-home");
    let workspace = home.join("workspace");
    fs::create_dir_all(workspace.join("src")).unwrap();
    write_test_file(
        &home.join(".claude/ide/48123.lock"),
        &json!({
            "workspaceFolders": [workspace.display().to_string()],
            "ideName": "VS Code",
            "transport": "ws"
        })
        .to_string(),
    );

    let report = render_ide_command_with_home(&workspace, false, None, Some(&home)).unwrap();

    assert!(report.contains("\"status\": \"available\""));
    assert!(report.contains("\"name\": \"VS Code\""));
    assert!(report.contains("ide://127.0.0.1:48123"));
}

#[tokio::test]
async fn lightweight_repl_commands_return_output() {
    let root = temp_session_root("repl-lightweight-commands");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "hello".to_owned(), None),
        build_text_message(session_id, MessageRole::Assistant, "world".to_owned(), None),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let cases = vec![
        ("help", vec![], "/help".to_owned()),
        ("version", vec![], "/version".to_owned()),
        ("status", vec![], "/status".to_owned()),
        ("statusline", vec![], "/statusline".to_owned()),
        ("theme", vec![], "/theme".to_owned()),
        ("vim", vec![], "/vim".to_owned()),
        ("plan", vec![], "/plan".to_owned()),
        ("fast", vec![], "/fast".to_owned()),
        ("passes", vec![], "/passes".to_owned()),
        ("effort", vec![], "/effort".to_owned()),
        ("session", vec![], "/session".to_owned()),
        ("permissions", vec![], "/permissions".to_owned()),
        ("compact", vec![], "/compact".to_owned()),
        ("files", vec![], "/files".to_owned()),
        ("diff", vec![], "/diff".to_owned()),
        ("usage", vec![], "/usage".to_owned()),
        ("remote-env", vec![], "/remote-env".to_owned()),
        ("export", vec![], "/export".to_owned()),
        ("tasks", vec![], "/tasks".to_owned()),
        ("agents", vec![], "/agents".to_owned()),
        ("skills", vec![], "/skills".to_owned()),
        ("reload-plugins", vec![], "/reload-plugins".to_owned()),
        ("hooks", vec![], "/hooks".to_owned()),
        ("output-style", vec![], "/output-style".to_owned()),
        ("remote-control", vec![], "/remote-control".to_owned()),
        ("voice", vec![], "/voice".to_owned()),
        ("exit", vec![], "/exit".to_owned()),
    ];

    for (name, args, raw_input) in cases {
        let output = handle_repl_slash_command(
            &registry,
            CommandInvocation {
                name: name.to_owned(),
                args,
                raw_input,
            },
            &store,
            &tool_registry,
            &root,
            None,
            ApiProvider::ChatGPTCodex,
            &mut active_model,
            &mut repl_session,
            &mut raw_messages,
            false,
            &mut vim_state,
            false,
            false,
        )
        .await
        .unwrap();

        assert!(
            !output.trim().is_empty(),
            "expected non-empty output for /{name}"
        );
    }
}

#[tokio::test]
async fn repl_model_command_switches_active_model() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-model")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "model".to_owned(),
            args: vec![DEFAULT_OPENAI_COMPLETION_MODEL.to_owned()],
            raw_input: format!("/model {DEFAULT_OPENAI_COMPLETION_MODEL}"),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        true,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(active_model, DEFAULT_OPENAI_COMPLETION_MODEL);
    assert!(status.contains("model switched"));
}

#[tokio::test]
async fn repl_model_command_accepts_openai_compatible_custom_model() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "repl-model-openai-compatible",
    )));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let session_id = SessionId::new_v4();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "model".to_owned(),
            args: vec!["gemini-3.1-pro-preview".to_owned()],
            raw_input: "/model gemini-3.1-pro-preview".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        true,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(active_model, "gemini-3.1-pro-preview");
    assert!(status.contains("model switched"));
}

#[tokio::test]
async fn repl_clear_command_resets_transcript_state() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-clear")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let transcript_path = store.transcript_path(session_id).await.unwrap();
    let mut raw_messages = vec![Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
    )];
    let persisted = build_text_message(session_id, MessageRole::User, "persist".to_owned(), None);
    store.append_message(session_id, &persisted).await.unwrap();
    let mut active_model = "claude-sonnet-4-6".to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "clear".to_owned(),
            raw_input: "/clear".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::FirstParty,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("cleared session"));
    assert!(raw_messages.is_empty());
    assert!(!transcript_path.exists());
}

#[tokio::test]
async fn repl_copy_command_writes_latest_assistant_response() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-copy")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "question".to_owned(), None),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "first answer".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "second answer".to_owned(),
            None,
        ),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "copy".to_owned(),
            raw_input: "/copy".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::FirstParty,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("last assistant response"));
    let file_path = status
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Also wrote it to "))
        .map(PathBuf::from)
        .expect("copy command should report a fallback file path");
    assert_eq!(fs::read_to_string(file_path).unwrap(), "second answer");
}

#[tokio::test]
async fn repl_copy_command_supports_explicit_message_index() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("repl-copy-index")));
    let tool_registry = compatibility_tool_registry();
    let root = env::temp_dir();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut raw_messages = vec![
        build_text_message(session_id, MessageRole::User, "question".to_owned(), None),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "first answer".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "second answer".to_owned(),
            None,
        ),
    ];
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "copy".to_owned(),
            args: vec!["2".to_owned()],
            raw_input: "/copy 2".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::FirstParty,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("assistant response #2"));
    let file_path = status
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Also wrote it to "))
        .map(PathBuf::from)
        .expect("copy command should report a fallback file path");
    assert_eq!(fs::read_to_string(file_path).unwrap(), "first answer");
}

#[test]
fn repl_command_ui_event_messages_use_attachment_metadata() {
    let session_id = SessionId::new_v4();
    let input = build_repl_command_input_message(session_id, None, "/config");
    let output = build_repl_command_output_message(session_id, Some(input.id), "config", "ok");

    assert_eq!(input.role, MessageRole::Attachment);
    assert_eq!(
        input.metadata.attributes.get("ui_role").map(String::as_str),
        Some("command")
    );
    assert_eq!(output.role, MessageRole::Attachment);
    assert_eq!(
        output
            .metadata
            .attributes
            .get("ui_role")
            .map(String::as_str),
        Some("command_output")
    );
    assert_eq!(
        output
            .metadata
            .attributes
            .get("ui_author")
            .map(String::as_str),
        Some("/config")
    );
}

#[test]
fn resumable_sessions_exclude_current_session() {
    let current_session = SessionId::new_v4();
    let other_session = SessionId::new_v4();
    let sessions = vec![
        SessionSummary {
            session_id: current_session,
            transcript_path: PathBuf::from(format!("{current_session}.jsonl")),
            modified_at_unix_ms: 20,
            message_count: 3,
            first_prompt: "current".to_owned(),
        },
        SessionSummary {
            session_id: other_session,
            transcript_path: PathBuf::from(format!("{other_session}.jsonl")),
            modified_at_unix_ms: 10,
            message_count: 8,
            first_prompt: "pick me".to_owned(),
        },
    ];

    let filtered = resumable_sessions(sessions, current_session);

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].session_id, other_session);
}

#[test]
fn resume_picker_builds_choice_list_entries() {
    let session_id = SessionId::new_v4();
    let picker = ResumePickerState {
        sessions: vec![SessionSummary {
            session_id,
            transcript_path: PathBuf::from(format!("{session_id}.jsonl")),
            modified_at_unix_ms: 10,
            message_count: 6,
            first_prompt: "Continue with the latest auth edge cases.".to_owned(),
        }],
        selected: 0,
    };

    let choice_list = build_resume_choice_list(&picker);

    assert_eq!(choice_list.title, "Resume conversation");
    assert_eq!(choice_list.items.len(), 1);
    assert!(choice_list.items[0]
        .label
        .contains("Continue with the latest auth edge cases."));
    assert!(choice_list.items[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("6 messages"));
}

#[test]
fn transcript_backed_commands_do_not_echo_results_in_footer() {
    assert!(!should_echo_command_result_in_footer("tasks", true, false));
    assert!(!should_echo_command_result_in_footer(
        "resume", false, false
    ));
    assert!(should_echo_command_result_in_footer("clear", false, false));
    assert!(should_echo_command_result_in_footer("resume", false, true));
}

#[tokio::test]
async fn repl_resume_command_switches_live_session() {
    let root = temp_session_root("repl-resume");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let current_session = SessionId::new_v4();
    let resumed_session = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = vec![build_text_message(
        current_session,
        MessageRole::User,
        "current prompt".to_owned(),
        None,
    )];
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(current_session);

    let resumed_message = build_text_message(
        resumed_session,
        MessageRole::User,
        "resumed output".to_owned(),
        None,
    );
    store
        .append_message(resumed_session, &resumed_message)
        .await
        .unwrap();

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "resume".to_owned(),
            args: vec![resumed_session.to_string()],
            raw_input: format!("/resume {resumed_session}"),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("resumed"));
    assert_eq!(repl_session.session_id, resumed_session);
    assert_eq!(raw_messages.len(), 1);
    assert_eq!(message_text(&raw_messages[0]), "resumed output");
}

#[tokio::test]
async fn repl_tasks_command_creates_and_lists_tasks() {
    let root = temp_session_root("repl-tasks");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let created = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "tasks".to_owned(),
            args: vec![
                "create".to_owned(),
                "title=review".to_owned(),
                "status=running".to_owned(),
            ],
            raw_input: "/tasks create title=review status=running".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();
    let listed = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "tasks".to_owned(),
            raw_input: "/tasks".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(created.contains("\"title\": \"review\""));
    assert!(listed.contains("\"count\": 1"));
    assert!(listed.contains("\"status\": \"Running\""));
}

#[tokio::test]
async fn repl_plugin_command_reports_manifest_details() {
    let root = temp_session_root("repl-plugin");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "review-tools",
              "version": "1.0.0",
              "description": "Review helpers",
              "skills": "./skills/review",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp"
                }
              }
            }"#,
    );
    write_test_file(&root.join("skills/review/SKILL.md"), "# Review\n");

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "plugin".to_owned(),
            raw_input: "/plugin".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("\"name\": \"review-tools\""));
    assert!(status.contains("\"mcp_server_names\""));
    assert!(status.contains("\"skill_names\""));
}

#[test]
fn runtime_system_prompt_loads_instruction_and_mcp_sections() {
    let root = temp_session_root("runtime-system-prompt");
    write_test_file(&root.join("CLAUDE.md"), "# Repo Rules\nUse bun.\n");
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "review-tools",
              "mcpServers": {
                "docs": {
                  "url": "https://example.com/mcp",
                  "instructions": "Read the docs resources before falling back to shell commands."
                }
              }
            }"#,
    );

    let prompt = build_runtime_system_prompt(
        &root,
        &compatibility_tool_registry(),
        ApiProvider::OpenAICompatible,
        "gemini-3.1-pro-preview",
        None,
    );

    assert!(prompt.contains("You are Claude Code"));
    assert!(prompt.contains("Do NOT use bash when a relevant dedicated tool exists"));
    assert!(prompt.contains("To read files use file_read"));
    assert!(prompt.contains("Model: gemini-3.1-pro-preview"));
    assert!(prompt.contains("Use bun."));
    assert!(prompt.contains("Read the docs resources before falling back to shell commands."));
}

#[test]
fn resolved_command_registry_loads_user_home_skill_commands() {
    let root = temp_session_root("registry-project-skills");
    write_test_file(
        &root.join(".claude/commands/review.md"),
        "# Project review\n",
    );
    let home = temp_session_root("registry-user-skills");
    write_test_file(&home.join("commands/review.md"), "# User review\n");
    write_test_file(&home.join("commands/triage.md"), "# User triage\n");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let registry = runtime.block_on(resolved_command_registry(&root, None));
        let review = registry.resolve("review").unwrap();
        let triage = registry.resolve("triage").unwrap();

        assert_eq!(review.source, CommandSource::Skill);
        assert!(review
            .origin
            .as_deref()
            .unwrap()
            .contains(&home.display().to_string()));
        assert_eq!(triage.source, CommandSource::Skill);
        assert!(triage
            .origin
            .as_deref()
            .unwrap()
            .contains(&home.display().to_string()));
    });
}

#[tokio::test]
async fn resolve_prompt_command_prompt_supports_inline_manifest_content() {
    let root = temp_session_root("inline-plugin-command");
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "review-tools",
              "commands": {
                "about": {
                  "content": "---\narguments: topic\n---\nExplain $topic from ${CLAUDE_PLUGIN_ROOT} during ${CLAUDE_SESSION_ID}."
                }
              }
            }"#,
    );

    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let prompt = resolve_prompt_command_prompt(
        &registry,
        &CommandInvocation {
            name: "about".to_owned(),
            args: vec!["plugins".to_owned()],
            raw_input: "/about plugins".to_owned(),
        },
        &root,
        None,
        session_id,
    )
    .unwrap()
    .unwrap();

    assert!(prompt.contains("Explain plugins"));
    assert!(prompt.contains(&root.display().to_string()));
    assert!(prompt.contains(&session_id.to_string()));
}

#[tokio::test]
async fn repl_skill_command_executes_expanded_prompt() {
    let root = temp_session_root("repl-skill-command");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "review-tools",
              "skills": "./skills/review"
            }"#,
    );
    write_test_file(
        &root.join("skills/review/SKILL.md"),
        "---\narguments: target\n---\nReview $target from ${CLAUDE_SKILL_DIR} in session ${CLAUDE_SESSION_ID}.\n",
    );
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "review".to_owned(),
            args: vec!["src/lib.rs".to_owned()],
            raw_input: "/review src/lib.rs".to_owned(),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAICompatible,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("1 steps"));
    assert!(raw_messages.iter().any(|message| {
        message.role == MessageRole::User
            && message_text(message).contains("Base directory for this skill")
            && message_text(message).contains("Review src/lib.rs")
    }));
    assert!(raw_messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message_text(message).contains("Review src/lib.rs")
    }));
}

#[tokio::test]
async fn repl_mcp_command_lists_parsed_servers_and_auth() {
    let root = temp_session_root("repl-mcp");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        r#"{
              "name": "mcp-tools",
              "mcpServers": {
                "example": {
                  "url": "https://example.com/mcp",
                  "auth": {
                    "type": "oauth_device",
                    "clientId": "client-123",
                    "audience": "example"
                  }
                }
              }
            }"#,
    );

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "mcp".to_owned(),
            raw_input: "/mcp".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("\"example\""));
    assert!(status.contains("\"oauth_device\""));
    assert!(status.contains("\"client_id\": \"client-123\""));
}
