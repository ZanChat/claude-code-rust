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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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

    assert!(created.contains("\"title\": \"review\""));
    assert!(listed.contains("\"count\": 1"));
    assert!(listed.contains("\"status\": \"Running\""));
}

#[tokio::test]
async fn repl_plan_command_enables_plan_mode_and_shows_plan() {
    let root = temp_session_root("repl-plan");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let enabled = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "plan".to_owned(),
            raw_input: "/plan".to_owned(),
            ..CommandInvocation::default()
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

    assert!(enabled.contains("Enabled plan mode"));
    assert!(root.join(".claude/plan-mode.json").exists());

    write_test_file(
        &root.join(".claude/plan.md"),
        "- Inspect the failing command path\n",
    );

    let shown = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "plan".to_owned(),
            raw_input: "/plan".to_owned(),
            ..CommandInvocation::default()
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

    assert!(shown.contains("Current plan"));
    assert!(shown.contains("Inspect the failing command path"));
}

#[tokio::test]
async fn repl_skills_command_formats_skill_list() {
    let root = temp_session_root("repl-skills");
    fs::create_dir_all(root.join(".git")).unwrap();
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    write_test_file(&root.join(".claude/skills/review/SKILL.md"), "# Review\n");
    let cwd = root.join("src/nested");
    fs::create_dir_all(&cwd).unwrap();
    let registry = resolved_command_registry(&cwd, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let output = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "skills".to_owned(),
            raw_input: "/skills".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &cwd,
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

    assert!(output.contains("Skills"));
    assert!(output.contains("/review"));
    assert!(output.contains("project skill"));
}

#[tokio::test]
async fn repl_agents_command_formats_agent_records() {
    let root = temp_session_root("repl-agents");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let created = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "agents".to_owned(),
            args: vec!["create".to_owned(), "review docs".to_owned()],
            raw_input: "/agents create review docs".to_owned(),
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

    let listed = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "agents".to_owned(),
            raw_input: "/agents".to_owned(),
            ..CommandInvocation::default()
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

    assert!(created.contains("Created agent task"));
    assert!(listed.contains("Agents"));
    assert!(listed.contains("review docs"));
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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
    let prompt_text = prompt.as_text();

    assert_eq!(prompt.blocks.len(), 3);
    assert!(prompt_text.contains("You are Claude Code"));
    assert!(prompt_text.contains("Do NOT use bash when a relevant dedicated tool exists"));
    assert!(prompt_text.contains("To read files use file_read"));
    assert!(prompt_text.contains("Model: gemini-3.1-pro-preview"));
    assert!(prompt_text.contains("Use bun."));
    assert!(prompt_text.contains("Read the docs resources before falling back to shell commands."));
}

#[test]
fn runtime_system_prompt_prioritizes_project_instructions_and_caps_budget() {
    let home = temp_session_root("runtime-system-prompt-home");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let root = temp_session_root("runtime-system-prompt-budget");
        let cwd = root.join("workspace/app");
        fs::create_dir_all(&cwd).unwrap();

        write_test_file(&cwd.join("CLAUDE.md"), "# App Rules\nMOST_SPECIFIC_RULE\n");
        write_test_file(
            &cwd.join("CLAUDE.local.md"),
            &format!("# App Local Rules\n{}\n", "APP_LOCAL_RULE ".repeat(400)),
        );
        write_test_file(
            &root.join("workspace/CLAUDE.md"),
            &format!("# Workspace Rules\n{}\n", "WORKSPACE_RULE ".repeat(400)),
        );
        write_test_file(
            &root.join("CLAUDE.md"),
            &format!("# Root Rules\n{}\n", "ROOT_RULE ".repeat(400)),
        );
        write_test_file(&home.join("CLAUDE.md"), "# Home Rules\nHOME_RULE\n");

        let prompt = build_runtime_system_prompt(
            &cwd,
            &compatibility_tool_registry(),
            ApiProvider::OpenAICompatible,
            "gemini-3.1-pro-preview",
            None,
        );
        let semi_static = prompt
            .blocks
            .iter()
            .find(|block| block.stability == ccrust_providers::PromptBlockStability::SemiStatic)
            .expect("semi-static block should exist");

        assert!(semi_static.text.contains("MOST_SPECIFIC_RULE"));
        assert!(semi_static.text.contains("WORKSPACE_RULE"));
        assert!(semi_static.text.contains("[truncated]"));
        assert!(!semi_static.text.contains("HOME_RULE"));
        assert!(semi_static.text.chars().count() <= crate::MAX_INSTRUCTION_TOTAL_CHARS);
    });
}

#[test]
fn runtime_system_prompt_caps_mcp_instruction_budget() {
    let home = temp_session_root("runtime-system-prompt-mcp-home");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let root = temp_session_root("runtime-system-prompt-mcp-budget");
        write_test_file(
            &root.join(".claude-plugin/plugin.json"),
            &format!(
                r#"{{
                  "name": "review-tools",
                  "mcpServers": {{
                    "alpha": {{
                      "url": "https://example.com/alpha",
                      "instructions": "{}"
                    }},
                    "beta": {{
                      "url": "https://example.com/beta",
                      "instructions": "{}"
                    }},
                    "gamma": {{
                      "url": "https://example.com/gamma",
                      "instructions": "{}"
                    }}
                  }}
                }}"#,
                "ALPHA_RULE ".repeat(220),
                "BETA_RULE ".repeat(220),
                "GAMMA_RULE ".repeat(220),
            ),
        );

        let prompt = build_runtime_system_prompt(
            &root,
            &compatibility_tool_registry(),
            ApiProvider::OpenAICompatible,
            "gemini-3.1-pro-preview",
            None,
        );
        let semi_static = prompt
            .blocks
            .iter()
            .find(|block| block.stability == ccrust_providers::PromptBlockStability::SemiStatic)
            .expect("semi-static block should exist");

        assert!(semi_static.text.contains("alpha"));
        assert!(semi_static.text.contains("beta"));
        assert!(semi_static.text.contains("[truncated]"));
        assert!(semi_static.text.chars().count() <= crate::MAX_MCP_TOTAL_CHARS);
    });
}

#[test]
fn usage_command_reports_cache_tokens_and_prompt_metrics() {
    let session_id = SessionId::new_v4();
    let mut assistant =
        build_text_message(session_id, MessageRole::Assistant, "done".to_owned(), None);
    assistant.metadata.provider = Some("openai-compatible".to_owned());
    assistant.metadata.usage = Some(ccrust_core::TokenUsage {
        input_tokens: 10,
        output_tokens: 2,
        cache_creation_input_tokens: 4,
        cache_read_input_tokens: 6,
    });
    apply_runtime_prompt_metrics(
        &mut assistant,
        &RuntimeSystemPromptMetrics {
            static_chars: 100,
            semi_static_chars: 40,
            dynamic_chars: 20,
            static_hash: "static-hash".to_owned(),
            semi_static_hash: "semi-hash".to_owned(),
            dynamic_hash: "dynamic-hash".to_owned(),
            semi_static_fingerprint: "semi-fingerprint".to_owned(),
        },
    );

    let report = render_usage_command(&[assistant]).unwrap();
    let json: serde_json::Value = serde_json::from_str(&report).unwrap();

    assert_eq!(json["input_tokens"], 10);
    assert_eq!(json["output_tokens"], 2);
    assert_eq!(json["cache_creation_input_tokens"], 4);
    assert_eq!(json["cache_read_input_tokens"], 6);
    assert_eq!(json["cache_hit_rate"], 0.3);
    assert_eq!(json["providers"]["openai-compatible"]["response_count"], 1);
    assert_eq!(json["latest_prompt"]["static_chars"], 100);
    assert_eq!(json["latest_prompt"]["semi_static_chars"], 40);
    assert_eq!(json["latest_prompt"]["dynamic_chars"], 20);
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

#[cfg(unix)]
#[test]
fn resolved_command_registry_loads_user_home_symlinked_skill() {
    let root = temp_session_root("registry-project-home-symlinked-skill");
    let home = temp_session_root("registry-user-symlinked-skill");
    let source_root = home.join("source-skills");
    write_test_file(
        &source_root.join("targeted-chatroom/SKILL.md"),
        "# Targeted Chatroom\n",
    );
    fs::create_dir_all(home.join("skills")).unwrap();
    std::os::unix::fs::symlink(
        source_root.join("targeted-chatroom"),
        home.join("skills/targeted-chatroom"),
    )
    .unwrap();
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let registry = runtime.block_on(resolved_command_registry(&root, None));
        let chatroom = registry.resolve("targeted-chatroom").unwrap();

        assert_eq!(chatroom.source, CommandSource::Skill);
        assert!(chatroom
            .origin
            .as_deref()
            .unwrap()
            .contains("targeted-chatroom/SKILL.md"));
    });
}

#[cfg(unix)]
#[test]
fn resolve_prompt_command_prompt_supports_user_home_symlinked_skill() {
    let root = temp_session_root("prompt-project-home-symlinked-skill");
    let home = temp_session_root("prompt-user-symlinked-skill");
    let source_root = home.join("source-skills");
    write_test_file(
        &source_root.join("targeted-chatroom/SKILL.md"),
        "---\narguments: topic\n---\nDiscuss $topic from ${CLAUDE_SKILL_DIR} in session ${CLAUDE_SESSION_ID}.\n",
    );
    fs::create_dir_all(home.join("skills")).unwrap();
    std::os::unix::fs::symlink(
        source_root.join("targeted-chatroom"),
        home.join("skills/targeted-chatroom"),
    )
    .unwrap();
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let registry = runtime.block_on(resolved_command_registry(&root, None));
        let session_id = SessionId::new_v4();
        let prompt = resolve_prompt_command_prompt(
            &registry,
            &CommandInvocation {
                name: "targeted-chatroom".to_owned(),
                args: vec!["debugging".to_owned()],
                raw_input: "/targeted-chatroom debugging".to_owned(),
            },
            &root,
            None,
            session_id,
        )
        .unwrap()
        .unwrap();

        assert!(prompt.contains("Discuss debugging"));
        assert!(prompt.contains("Base directory for this skill:"));
        assert!(prompt.contains(&home.join("skills/targeted-chatroom").display().to_string()));
        assert!(prompt.contains(&session_id.to_string()));
    });
}

#[test]
fn resolved_command_registry_loads_ancestor_project_skill_commands() {
    let root = temp_session_root("registry-ancestor-project-skills");
    fs::create_dir_all(root.join(".git")).unwrap();
    write_test_file(
        &root.join(".claude/commands/review.md"),
        "# Project review\n",
    );
    let cwd = root.join("src/nested");
    fs::create_dir_all(&cwd).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let registry = runtime.block_on(resolved_command_registry(&cwd, None));
    let review = registry.resolve("review").unwrap();

    assert_eq!(review.source, CommandSource::Skill);
    assert!(review
        .origin
        .as_deref()
        .unwrap()
        .contains(&root.display().to_string()));
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

#[test]
fn repl_skill_command_executes_expanded_prompt() {
    let home = temp_session_root("repl-skill-command-home");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
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
            let mut vim_state = ccrust_ui::vim::VimState::default();
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
        });
    });
}

#[tokio::test]
async fn repl_skill_command_reports_empty_prompt_file() {
    let root = temp_session_root("repl-skill-command-empty-prompt");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    write_test_file(&root.join(".claude/skills/seogeo/SKILL.md"), "");
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "seogeo".to_owned(),
            raw_input: "/seogeo".to_owned(),
            ..CommandInvocation::default()
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

    assert!(status.contains("registered but its prompt is empty or unreadable"));
    assert!(status.contains(".claude/skills/seogeo/SKILL.md"));
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
    let mut vim_state = ccrust_ui::vim::VimState::default();
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

    assert!(status.contains("\"example\""));
    assert!(status.contains("\"oauth_device\""));
    assert!(status.contains("\"client_id\": \"client-123\""));
}

#[tokio::test]
async fn builtin_review_command_executes_builtin_prompt() {
    let root = temp_session_root("repl-builtin-review");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "review".to_owned(),
            args: vec!["123".to_owned()],
            raw_input: "/review 123".to_owned(),
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
            && message_text(message).contains("You are an expert code reviewer")
            && message_text(message).contains("PR number: 123")
    }));
}

#[test]
fn resolve_prompt_command_prompt_supports_builtin_review() {
    let root = temp_session_root("builtin-review-prompt");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();

    let prompt = resolve_prompt_command_prompt(
        &registry,
        &CommandInvocation {
            name: "review".to_owned(),
            args: vec!["456".to_owned()],
            raw_input: "/review 456".to_owned(),
        },
        &root,
        None,
        session_id,
    )
    .unwrap()
    .unwrap();

    assert!(prompt.contains("gh pr list"));
    assert!(prompt.contains("PR number: 456"));
}

#[test]
fn resolve_prompt_command_prompt_supports_builtin_parity_prompts() {
    let root = temp_session_root("builtin-parity-prompts");
    let registry = compatibility_command_registry();
    let session_id = SessionId::new_v4();

    for (name, args, expected) in [
        ("batch", "rename all session handlers", "independent units"),
        (
            "btw",
            "does /branch create a new transcript?",
            "quick side question",
        ),
        ("debug", "the command picker is empty", "/status"),
        (
            "update-config",
            "allow Read in project settings",
            ".claude/settings.json",
        ),
    ] {
        let prompt = resolve_prompt_command_prompt(
            &registry,
            &CommandInvocation {
                name: name.to_owned(),
                args: args.split_whitespace().map(str::to_owned).collect(),
                raw_input: format!("/{name} {args}"),
            },
            &root,
            None,
            session_id,
        )
        .unwrap()
        .unwrap();

        assert!(prompt.contains(expected), "missing {expected} for /{name}");
        assert!(prompt.contains(args), "missing args for /{name}");
    }
}

#[test]
fn pending_interrupt_message_follows_ts_submit_interrupt_rules() {
    assert!(should_append_pending_interrupt_message(&[]));
    assert!(!should_append_pending_interrupt_message(&[
        "follow up".to_owned()
    ]));
}

#[test]
fn pending_btw_helpers_extract_question_and_strip_preview_assistant() {
    let session_id = SessionId::new_v4();
    let user = build_text_message(session_id, MessageRole::User, "main task".to_owned(), None);
    let assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "partial answer".to_owned(),
        Some(user.id),
    );
    let invocation = CommandInvocation {
        name: "btw".to_owned(),
        args: vec!["what".to_owned(), "changed?".to_owned()],
        raw_input: "/btw what changed?".to_owned(),
    };

    assert_eq!(
        pending_btw_question(&invocation).as_deref(),
        Some("what changed?")
    );
    assert!(pending_btw_question(&CommandInvocation {
        name: "btw".to_owned(),
        args: vec![],
        raw_input: "/btw".to_owned(),
    })
    .is_none());
    assert_eq!(
        pending_btw_context_messages(&[user.clone(), assistant]),
        vec![user]
    );
}

#[test]
fn pending_overlay_ui_events_chain_after_runtime_messages() {
    let session_id = SessionId::new_v4();
    let base = build_text_message(session_id, MessageRole::User, "main task".to_owned(), None);
    let pending_view = Arc::new(Mutex::new(PendingReplView::new(
        vec![base.clone()],
        "working",
    )));

    let group_id = append_pending_repl_overlay_ui_event(
        &pending_view,
        session_id,
        "/btw what changed?",
        "command",
        None,
    )
    .expect("expected /btw group id");
    let _ = append_pending_repl_overlay_ui_event(
        &pending_view,
        session_id,
        "side answer",
        "command_output",
        Some("/btw".to_owned()),
    );

    let state = pending_view.lock().unwrap();
    assert_eq!(state.transcript_overlay_messages.len(), 2);
    assert_eq!(
        group_id,
        pending_transcript_group_id(&state.transcript_overlay_messages[0])
    );
    assert_eq!(
        state.transcript_overlay_messages[0].parent_id,
        Some(base.id)
    );
    assert_eq!(
        state.transcript_overlay_messages[1].parent_id,
        Some(state.transcript_overlay_messages[0].id)
    );
}

#[tokio::test]
async fn pending_btw_side_question_uses_immediate_side_channel() {
    let root = temp_session_root("pending-btw-side-channel");
    let tool_registry = compatibility_tool_registry();
    let system_prompt = build_runtime_system_prompt(
        &root,
        &tool_registry,
        ApiProvider::OpenAICompatible,
        DEFAULT_OPENAI_REASONING_MODEL,
        None,
    );
    let session_id = SessionId::new_v4();
    let messages = vec![build_text_message(
        session_id,
        MessageRole::User,
        "main task is still running".to_owned(),
        None,
    )];

    let answer = run_pending_btw_side_question(
        ApiProvider::OpenAICompatible,
        DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        session_id,
        system_prompt.blocks,
        messages,
        "what changed?".to_owned(),
        false,
    )
    .await
    .unwrap();

    assert!(answer.contains("what changed?"));
    assert!(answer.contains("side question"));
}

#[test]
fn pending_btw_side_question_runs_on_dedicated_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let root = temp_session_root("pending-btw-dedicated-runtime");
        let tool_registry = compatibility_tool_registry();
        let system_prompt = build_runtime_system_prompt(
            &root,
            &tool_registry,
            ApiProvider::OpenAICompatible,
            DEFAULT_OPENAI_REASONING_MODEL,
            None,
        );
        let session_id = SessionId::new_v4();
        let messages = vec![build_text_message(
            session_id,
            MessageRole::User,
            "main task is still running".to_owned(),
            None,
        )];

        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::spawn_pending_btw_side_question(
                ApiProvider::OpenAICompatible,
                DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
                session_id,
                system_prompt.blocks,
                messages,
                "what changed?".to_owned(),
                false,
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        assert!(answer.contains("what changed?"));
        assert!(answer.contains("side question"));
    });
}

#[test]
fn pending_btw_provider_request_does_not_hard_cap_debug_output() {
    let session_id = SessionId::new_v4();
    let request = pending_btw_provider_request(
        DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        vec![ccrust_providers::SystemPromptBlock::new(
            "system prompt".to_owned(),
            ccrust_providers::PromptBlockStability::Static,
            Some(ccrust_providers::PromptCacheScope::Global),
        )],
        vec![build_text_message(
            session_id,
            MessageRole::User,
            "send back system prompt for debugging".to_owned(),
            None,
        )],
    );

    assert!(request.max_output_tokens.is_none());
    assert!(request.tools.is_empty());
    assert_eq!(request.messages.len(), 1);
}

#[test]
fn settings_commands_persist_preferences_and_effort_env() {
    let home = temp_session_root("command-settings-home");
    let home_path = home.display().to_string();

    with_env_vars(
        &[
            ("CLAUDE_CONFIG_DIR", Some(&home_path)),
            ("REASONING_MODEL_THINK", None),
            ("COMPLETION_MODEL_THINK", None),
        ],
        || {
            let theme = render_theme_command(&CommandInvocation {
                name: "theme".to_owned(),
                args: vec!["dark-ansi".to_owned()],
                raw_input: "/theme dark-ansi".to_owned(),
            })
            .unwrap();
            assert!(theme.contains("Dark mode (ANSI colors only)"));

            let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
            let fast = render_fast_command(
                &CommandInvocation {
                    name: "fast".to_owned(),
                    args: vec!["on".to_owned()],
                    raw_input: "/fast on".to_owned(),
                },
                ApiProvider::OpenAICompatible,
                &active_model,
            )
            .unwrap();
            if let Some(model) = fast.next_model.as_ref() {
                active_model = model.clone();
            }
            assert!(fast.message.contains("Fast mode enabled"));
            assert_eq!(active_model, DEFAULT_OPENAI_COMPLETION_MODEL);

            let effort = render_effort_command(
                Path::new("."),
                &CommandInvocation {
                    name: "effort".to_owned(),
                    args: vec!["max".to_owned()],
                    raw_input: "/effort max".to_owned(),
                },
            )
            .unwrap();
            assert_eq!(effort, "Effort level set to max.");
            assert_eq!(env::var("REASONING_MODEL_THINK").unwrap(), "xhigh");
            assert_eq!(env::var("COMPLETION_MODEL_THINK").unwrap(), "xhigh");

            let chrome = render_chrome_command(&CommandInvocation {
                name: "chrome".to_owned(),
                args: vec!["on".to_owned()],
                raw_input: "/chrome on".to_owned(),
            })
            .unwrap();
            assert!(chrome.contains("enabled by default"));

            let advisor = render_advisor_command(&CommandInvocation {
                name: "advisor".to_owned(),
                args: vec!["opus".to_owned()],
                raw_input: "/advisor opus".to_owned(),
            })
            .unwrap();
            assert_eq!(advisor, "Advisor set to opus.");

            let settings = load_command_settings();
            assert_eq!(settings.theme.as_deref(), Some("dark-ansi"));
            assert!(settings.fast_mode);
            assert_eq!(settings.advisor_model.as_deref(), Some("opus"));
            assert!(settings.chrome_default_enabled);

            let env_file = fs::read_to_string(user_ccrust_env_path()).unwrap();
            assert!(env_file.contains("REASONING_MODEL_THINK=xhigh"));
            assert!(env_file.contains("COMPLETION_MODEL_THINK=xhigh"));
        },
    );
}

#[test]
fn fast_command_reports_when_current_provider_does_not_apply_it() {
    let current = render_fast_command(
        &CommandInvocation {
            name: "fast".to_owned(),
            args: vec![],
            raw_input: "/fast".to_owned(),
        },
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
    )
    .unwrap();
    assert!(current
        .message
        .contains("gemini, chatgpt-codex, and openai-compatible"));

    let enabled = render_fast_command(
        &CommandInvocation {
            name: "fast".to_owned(),
            args: vec!["on".to_owned()],
            raw_input: "/fast on".to_owned(),
        },
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
    )
    .unwrap();
    assert!(enabled
        .message
        .contains("enabled for supported Gemini and OpenAI-family sessions"));
    assert!(enabled.next_model.is_none());
}

#[tokio::test]
async fn rename_tag_and_rewind_commands_persist_session_state() {
    let root = temp_session_root("session-metadata-commands");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    let mut raw_messages = vec![
        build_text_message(
            session_id,
            MessageRole::User,
            "Inspect auth flow".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "Auth flow looks stable.".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::User,
            "Check bridge handoff".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "Bridge handoff needs work.".to_owned(),
            None,
        ),
    ];
    for message in &raw_messages {
        store.append_message(session_id, message).await.unwrap();
    }

    let rename = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "rename".to_owned(),
            args: vec!["Auth handoff".to_owned()],
            raw_input: "/rename Auth handoff".to_owned(),
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
    assert_eq!(rename, "Session renamed to: Auth handoff");

    let tag = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "tag".to_owned(),
            args: vec!["bugfix".to_owned()],
            raw_input: "/tag bugfix".to_owned(),
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
    assert_eq!(tag, "Tagged session with #bugfix.");

    let session_report = render_session_command(&store, session_id).await.unwrap();
    let session_json: serde_json::Value = serde_json::from_str(&session_report).unwrap();
    assert_eq!(session_json["custom_title"], "Auth handoff");
    assert_eq!(session_json["agent_name"], "Auth handoff");
    assert_eq!(session_json["tag"], "bugfix");

    let rewind = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "rewind".to_owned(),
            args: vec!["3".to_owned()],
            raw_input: "/rewind 3".to_owned(),
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

    assert!(rewind.contains("Rewound to before turn 2"));
    assert_eq!(raw_messages.len(), 2);
    assert_eq!(store.load_session(session_id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn repl_branch_command_clones_messages_into_new_session() {
    let root = temp_session_root("repl-branch");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    let mut raw_messages = vec![
        build_text_message(
            session_id,
            MessageRole::User,
            "Does /branch create a new transcript?".to_owned(),
            None,
        ),
        build_text_message(
            session_id,
            MessageRole::Assistant,
            "Yes, it should fork the session state.".to_owned(),
            None,
        ),
    ];

    let output = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "branch".to_owned(),
            args: vec!["Auth handoff".to_owned()],
            raw_input: "/branch Auth handoff".to_owned(),
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

    let branch_session_id = repl_session.session_id;
    let branch_transcript_path = store.transcript_path(branch_session_id).await.unwrap();
    let branched_messages = store.load_session(branch_session_id).await.unwrap();
    let metadata = load_session_metadata_for_path(&branch_transcript_path);

    assert_ne!(branch_session_id, session_id);
    assert!(output.contains("Branched conversation as Auth handoff (Branch)."));
    assert!(output.contains(&resume_command_for_session(session_id)));
    assert!(output.contains(&resume_command_for_session(branch_session_id)));
    assert_eq!(branched_messages.len(), 2);
    assert!(branched_messages
        .iter()
        .all(|message| message.session_id == Some(branch_session_id)));
    assert_eq!(
        message_text(&branched_messages[0]),
        "Does /branch create a new transcript?"
    );
    assert_eq!(raw_messages, branched_messages);
    assert_eq!(
        metadata.custom_title.as_deref(),
        Some("Auth handoff (Branch)")
    );
}

#[test]
fn reload_auth_command_reports_codex_auth_file_status() {
    let home = temp_session_root("reload-auth-home");
    write_test_file(
        &home.join("auth.json"),
        r#"{
              "OPENAI_API_KEY": "sk-test"
            }"#,
    );
    let home_path = home.display().to_string();

    with_env_vars(
        &[
            ("CODEX_HOME", Some(&home_path)),
            ("OPENAI_API_KEY", None),
            ("OPENAI_BASE_URL", None),
        ],
        || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let root = temp_session_root("reload-auth-command");
                let store =
                    ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
                let tool_registry = compatibility_tool_registry();
                let registry = resolved_command_registry(&root, None).await;
                let session_id = SessionId::new_v4();
                let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
                let mut vim_state = ccrust_ui::vim::VimState::default();
                let mut repl_session = repl_session_state(session_id);
                let mut raw_messages = Vec::new();

                let output = handle_repl_slash_command(
                    &registry,
                    CommandInvocation {
                        name: "reload-auth".to_owned(),
                        args: vec![],
                        raw_input: "/reload-auth".to_owned(),
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

                assert!(output.contains(&home.join("auth.json").display().to_string()));
                assert!(output.contains("Has credentials: true"));
                assert!(output.contains("Source: codex_auth_api_key"));
            });
        },
    );
}

#[tokio::test]
async fn context_command_reports_estimated_usage() {
    let root = temp_session_root("context-command");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.join("sessions")));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    let mut assistant = build_text_message(
        session_id,
        MessageRole::Assistant,
        "Summarized answer".to_owned(),
        None,
    );
    assistant.metadata.usage = Some(ccrust_core::TokenUsage {
        input_tokens: 64,
        output_tokens: 12,
        cache_creation_input_tokens: 8,
        cache_read_input_tokens: 4,
    });
    let mut raw_messages = vec![
        build_text_message(
            session_id,
            MessageRole::User,
            "Estimate current context usage".to_owned(),
            None,
        ),
        assistant,
    ];

    let output = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "context".to_owned(),
            args: vec![],
            raw_input: "/context".to_owned(),
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

    assert!(output.contains("Context usage"));
    assert!(output.contains(&format!("Model: {DEFAULT_OPENAI_REASONING_MODEL}")));
    assert!(output.contains("Runtime messages: 2"));
    assert!(output.contains("Estimated tokens:"));
    assert!(output.contains("Latest response usage: input=64 output=12 cache_write=8 cache_read=4"));
}

#[tokio::test]
async fn targeted_command_outputs_do_not_use_placeholder_copy() {
    let root = temp_session_root("command-placeholder-copy");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut vim_state = ccrust_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    let mut raw_messages = vec![build_text_message(
        session_id,
        MessageRole::User,
        "Inspect auth flow".to_owned(),
        None,
    )];

    let cases = vec![
        CommandInvocation {
            name: "add-dir".to_owned(),
            args: vec![],
            raw_input: "/add-dir".to_owned(),
        },
        CommandInvocation {
            name: "theme".to_owned(),
            args: vec![],
            raw_input: "/theme".to_owned(),
        },
        CommandInvocation {
            name: "color".to_owned(),
            args: vec![],
            raw_input: "/color".to_owned(),
        },
        CommandInvocation {
            name: "fast".to_owned(),
            args: vec![],
            raw_input: "/fast".to_owned(),
        },
        CommandInvocation {
            name: "doctor".to_owned(),
            args: vec![],
            raw_input: "/doctor".to_owned(),
        },
        CommandInvocation {
            name: "passes".to_owned(),
            args: vec![],
            raw_input: "/passes".to_owned(),
        },
        CommandInvocation {
            name: "effort".to_owned(),
            args: vec![],
            raw_input: "/effort".to_owned(),
        },
        CommandInvocation {
            name: "context".to_owned(),
            args: vec![],
            raw_input: "/context".to_owned(),
        },
        CommandInvocation {
            name: "mobile".to_owned(),
            args: vec![],
            raw_input: "/mobile".to_owned(),
        },
        CommandInvocation {
            name: "desktop".to_owned(),
            args: vec![],
            raw_input: "/desktop".to_owned(),
        },
        CommandInvocation {
            name: "chrome".to_owned(),
            args: vec![],
            raw_input: "/chrome".to_owned(),
        },
        CommandInvocation {
            name: "advisor".to_owned(),
            args: vec![],
            raw_input: "/advisor".to_owned(),
        },
        CommandInvocation {
            name: "feedback".to_owned(),
            args: vec![],
            raw_input: "/feedback".to_owned(),
        },
        CommandInvocation {
            name: "install-github-app".to_owned(),
            args: vec![],
            raw_input: "/install-github-app".to_owned(),
        },
        CommandInvocation {
            name: "longtask".to_owned(),
            args: vec![],
            raw_input: "/longtask".to_owned(),
        },
        CommandInvocation {
            name: "release-notes".to_owned(),
            args: vec![],
            raw_input: "/release-notes".to_owned(),
        },
        CommandInvocation {
            name: "reload-auth".to_owned(),
            args: vec![],
            raw_input: "/reload-auth".to_owned(),
        },
        CommandInvocation {
            name: "sandbox".to_owned(),
            args: vec![],
            raw_input: "/sandbox".to_owned(),
        },
        CommandInvocation {
            name: "terminal-setup".to_owned(),
            args: vec![],
            raw_input: "/terminal-setup".to_owned(),
        },
        CommandInvocation {
            name: "voice".to_owned(),
            args: vec![],
            raw_input: "/voice".to_owned(),
        },
    ];
    let banned = [
        "compatibility-surface",
        "compatibility state",
        "not persisted yet",
        "intentionally deferred",
        "not bundled",
        "not implemented",
        "not modeled",
        "still minimal",
        "not yet",
    ];

    for invocation in cases {
        let output = handle_repl_slash_command(
            &registry,
            invocation.clone(),
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

        let lowered = output.to_ascii_lowercase();
        for needle in banned {
            assert!(
                !lowered.contains(needle),
                "unexpected placeholder copy '{needle}' in /{} output: {output}",
                invocation.name
            );
        }
    }
}
