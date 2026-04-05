use super::{
    footer_primary_text, input_prompt_line, mouse_action_for_position,
    pane_shortcut_label_for_terminal, pending_details_toggle_label, progress_line,
    render_to_string, task_lines, transcript_search_match_items, transcript_search_scroll_for_view,
    transcript_selectable_lines_for_view, transcript_selection_text_for_view,
    transcript_visual_lines, ChoiceListItem, ChoiceListState, InputBuffer, Notification, PaneKind,
    PermissionPromptState, PromptHistorySearchState, PromptSelectionState, RatatuiApp, StatusLevel,
    TaskUiEntry, TranscriptGroup, TranscriptItem, TranscriptLine, TranscriptMessageActionsState,
    TranscriptSearchState, TranscriptSelectionPoint, TranscriptSelectionState, UiMouseAction,
};
use code_agent_core::{
    compatibility_command_registry, ContentBlock, Message, MessageRole, TaskStatus,
};
use ratatui::style::Color;
use std::collections::BTreeMap;

#[test]
fn renders_transcript_empty_state_and_commands() {
    let app = RatatuiApp::new("session preview");
    let state = app.state_from_messages(vec![], &compatibility_command_registry().all());

    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("Start a conversation"));
    assert!(rendered.contains("/help") || rendered.contains("/clear"));
}

#[test]
fn renders_permission_prompt_and_banner() {
    let mut state = RatatuiApp::new("permissions").initial_state();
    state.active_pane = Some(PaneKind::Permissions);
    state.compact_banner = Some("auto compact applied".to_owned());
    state.permission_prompt = Some(PermissionPromptState {
        tool_name: "bash".to_owned(),
        summary: "Remote tool execution requires approval".to_owned(),
        allow_once_label: "Approve once".to_owned(),
        deny_label: "Deny".to_owned(),
    });

    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("Permissions"));
    assert!(rendered.contains("bash"));
    assert!(rendered.contains("Approve once"));
}

#[test]
fn renders_file_diff_task_and_log_previews() {
    let mut state = RatatuiApp::new("preview panes").initial_state();
    state.active_pane = Some(PaneKind::Diff);
    state.diff_preview.title = "Diff preview".to_owned();
    state.diff_preview.lines = vec![
        "path: src/main.rs".to_owned(),
        "--- before ---".to_owned(),
        "old line".to_owned(),
        "+++ after +++".to_owned(),
        "new line".to_owned(),
    ];
    state.file_preview.title = "File preview".to_owned();
    state.file_preview.lines = vec!["fn main() {".to_owned(), "}".to_owned()];
    state.task_preview.title = "Tasks".to_owned();
    state.task_preview.lines = vec!["running build".to_owned()];
    state.log_preview.title = "Logs".to_owned();
    state.log_preview.lines = vec!["remote bridge connected".to_owned()];
    state.push_notification(Notification {
        title: "info".to_owned(),
        body: "pane updated".to_owned(),
        level: Some(StatusLevel::Info),
    });

    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("Diff"));
    assert!(rendered.contains("Diff preview"));
    assert!(rendered.contains("src/main.rs"));
    assert!(rendered.contains("old line"));
    assert!(rendered.contains("new line"));
}

#[test]
fn renders_compact_layout_for_narrow_terminals() {
    let mut state = RatatuiApp::new("compact").initial_state();
    state.transcript_lines = vec![super::TranscriptLine {
        role: "user".to_owned(),
        text: "This layout should collapse cleanly when the terminal is narrow.".to_owned(),
        author_label: None,
    }];
    state.show_input = true;
    state.task_preview.title = "Setup".to_owned();
    state.task_preview.lines = vec!["Check auth".to_owned(), "Add CLAUDE.md".to_owned()];
    state.active_pane = Some(PaneKind::Tasks);

    let rendered = render_to_string(&state, 60, 24).unwrap();

    assert!(rendered.contains("Tasks"));
    assert!(rendered.contains("Check auth"));
    assert!(rendered.contains("Add CLAUDE.md"));
}

#[test]
fn renders_too_small_notice() {
    let mut state = RatatuiApp::new("tiny").initial_state();
    state.show_input = true;

    let rendered = render_to_string(&state, 40, 12).unwrap();

    assert!(rendered.contains("Terminal too small"));
    assert!(rendered.contains("comfortable REPL") || rendered.contains("Resize"));
}

#[test]
fn pane_shortcut_label_matches_supported_shortcuts() {
    let expected = if cfg!(target_os = "macos") {
        "Cmd/Ctrl/Alt+1-6"
    } else {
        "Ctrl/Alt+1-6"
    };

    assert_eq!(pane_shortcut_label_for_terminal(None), expected);

    if cfg!(target_os = "macos") {
        assert_eq!(
            pane_shortcut_label_for_terminal(Some("vscode")),
            "Ctrl/Alt+1-6"
        );
        assert_eq!(
            pane_shortcut_label_for_terminal(Some("Apple_Terminal")),
            "Alt+1-6"
        );
    }
}

#[test]
fn pending_details_toggle_label_tracks_visibility() {
    let mut state = RatatuiApp::new("toggle").initial_state();
    state.pending_step_count = 1;

    assert_eq!(
        pending_details_toggle_label(&state),
        Some("Ctrl+E show details")
    );

    state.pending_transcript_details = true;
    assert_eq!(
        pending_details_toggle_label(&state),
        Some("Ctrl+E hide details")
    );

    state.pending_step_count = 0;
    assert_eq!(pending_details_toggle_label(&state), None);
}

#[test]
fn renders_prompt_and_command_suggestions() {
    let app = RatatuiApp::new("suggestions");
    let mut state = app.state_from_messages(
        vec![Message::new(
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: "Ready".to_owned(),
            }],
        )],
        &compatibility_command_registry().all(),
    );
    state.show_input = true;
    state.input_buffer.replace("/h");
    state.command_suggestions = vec![
        super::CommandPaletteEntry {
            name: "/help".to_owned(),
            description: "Show the available REPL commands.".to_owned(),
        },
        super::CommandPaletteEntry {
            name: "/hooks".to_owned(),
            description: "Inspect hook integration.".to_owned(),
        },
    ];
    state.selected_command_suggestion = Some(0);

    let rendered = render_to_string(&state, 100, 26).unwrap();

    assert!(rendered.contains("/help"));
    assert!(rendered.contains("/hooks"));
}

#[test]
fn hides_transcript_empty_state_while_typing_prompt() {
    let app = RatatuiApp::new("typing");
    let mut state = app.state_from_messages(vec![], &compatibility_command_registry().all());
    state.show_input = true;
    state.input_buffer.replace("hello world");

    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(!rendered.contains("Start a conversation"));
    assert!(!rendered.contains("Type a prompt below or start with /"));
    assert!(rendered.contains("Transcript"));
    assert!(rendered.contains("hello world"));
}

#[test]
fn renders_queued_follow_up_prompts_during_activity() {
    let mut state = RatatuiApp::new("queue").initial_state();
    state.show_input = true;
    state.vim_state.enabled = true;
    state.vim_state.enter_normal();
    state.progress_message = Some("/ Waiting for response".to_owned());
    state.queued_inputs = vec![
        "follow up with the failing test details".to_owned(),
        "/tasks".to_owned(),
    ];

    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert_eq!(
        footer_primary_text(&state, false),
        "Working · Ctrl+C to interrupt · 2 queued"
    );
    assert!(rendered.contains("Waiting for response"));
    assert!(rendered.contains("queue"));
    assert!(rendered.contains("follow up with the failing test details"));
    assert!(rendered.contains("/tasks"));
}

#[test]
fn footer_advertises_pending_detail_toggle_while_working() {
    let mut state = RatatuiApp::new("pending-details").initial_state();
    state.show_input = true;
    state.vim_state.enabled = true;
    state.vim_state.enter_normal();
    state.progress_message = Some("/ Waiting for response".to_owned());
    state.pending_step_count = 2;

    assert_eq!(
        footer_primary_text(&state, false),
        "Working · Ctrl+C to interrupt · Ctrl+E show details"
    );

    state.pending_transcript_details = true;
    assert_eq!(
        footer_primary_text(&state, false),
        "Working · Ctrl+C to interrupt · Ctrl+E hide details"
    );
}

#[test]
fn task_lines_render_tree_prefixes_and_hidden_summary() {
    let mut state = RatatuiApp::new("task-tree").initial_state();
    state.task_items = vec![
        TaskUiEntry {
            id: "root".to_owned(),
            parent_id: None,
            title: "Review workspace".to_owned(),
            kind: "workflow".to_owned(),
            status: TaskStatus::Running,
            owner_label: Some("builder".to_owned()),
            blocker_labels: Vec::new(),
            input: None,
            output: None,
            tree_prefix: String::new(),
            detail_prefix: "  ".to_owned(),
            is_recent_completion: false,
        },
        TaskUiEntry {
            id: "child-1".to_owned(),
            parent_id: Some("root".to_owned()),
            title: "Inspect failing tests".to_owned(),
            kind: "workflow_step".to_owned(),
            status: TaskStatus::Running,
            owner_label: None,
            blocker_labels: vec!["2".to_owned(), "3".to_owned()],
            input: Some("Open the failing fixture".to_owned()),
            output: None,
            tree_prefix: "├─ ".to_owned(),
            detail_prefix: "│    ".to_owned(),
            is_recent_completion: false,
        },
        TaskUiEntry {
            id: "child-2".to_owned(),
            parent_id: Some("root".to_owned()),
            title: "Summarize blockers".to_owned(),
            kind: "workflow_step".to_owned(),
            status: TaskStatus::Completed,
            owner_label: None,
            blocker_labels: Vec::new(),
            input: None,
            output: Some("Missing integration fixture".to_owned()),
            tree_prefix: "└─ ".to_owned(),
            detail_prefix: "     ".to_owned(),
            is_recent_completion: true,
        },
        TaskUiEntry {
            id: "later".to_owned(),
            parent_id: None,
            title: "Follow up with maintainer".to_owned(),
            kind: "task".to_owned(),
            status: TaskStatus::Pending,
            owner_label: None,
            blocker_labels: Vec::new(),
            input: None,
            output: None,
            tree_prefix: String::new(),
            detail_prefix: "  ".to_owned(),
            is_recent_completion: false,
        },
    ];

    let lines = task_lines(&state, 3, true);
    let texts = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert!(texts
        .iter()
        .any(|line| line.contains("Review workspace (@builder)")));
    assert!(texts
        .iter()
        .any(|line| line.contains("├─ ● Inspect failing tests  ➤ blocked by #2, #3")));
    assert!(texts
        .iter()
        .any(|line| line == "│    Open the failing fixture"));
    assert!(texts.iter().any(|line| line == "└─ ✓ Summarize blockers"));
    assert!(texts.iter().any(|line| line == "… +1 pending"));
}

#[test]
fn progress_line_uses_spinner_verb_and_styles() {
    let mut state = RatatuiApp::new("spinner").initial_state();
    state.progress_verb = Some("Crafting".to_owned());
    state.progress_message = Some("/ Waiting for response".to_owned());
    state.status_marquee_tick = 1;

    let line = progress_line(&state).expect("progress line should exist");

    assert_eq!(line.spans[0].content.as_ref(), "◓ ");
    assert_eq!(line.spans[1].content.as_ref(), "Crafting…");
    assert_eq!(line.spans[2].content.as_ref(), " · ");
    assert_eq!(line.spans[3].content.as_ref(), "Waiting for response");
    assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    assert_eq!(line.spans[1].style.fg, Some(Color::Cyan));
}

#[test]
fn renders_choice_list_overlay() {
    let mut state = RatatuiApp::new("picker").initial_state();
    state.show_input = true;
    state.choice_list = Some(ChoiceListState {
        title: "Resume conversation".to_owned(),
        subtitle: Some("Enter to select · Esc to cancel".to_owned()),
        items: vec![
            ChoiceListItem {
                label: "s:77777777  Continue with auth edge cases".to_owned(),
                detail: Some("6 messages · fixtures/transcripts/7777.jsonl".to_owned()),
                secondary: None,
            },
            ChoiceListItem {
                label: "s:88888888  Rework tool transcript rendering".to_owned(),
                detail: Some("12 messages · fixtures/transcripts/8888.jsonl".to_owned()),
                secondary: None,
            },
        ],
        selected: 0,
        empty_message: Some("No conversations found to resume.".to_owned()),
    });

    let rendered = render_to_string(&state, 100, 26).unwrap();

    assert!(rendered.contains("Resume conversation"));
    assert!(rendered.contains("Enter to select"));
    assert!(rendered.contains("s:77777777  Continue with auth edge cases"));
    assert!(rendered.contains("fixtures/transcripts/7777.jsonl"));
}

#[test]
fn transcript_widget_supports_scroll_offset() {
    let mut state = RatatuiApp::new("scroll").initial_state();
    state.transcript_lines = (1..=8)
        .map(|index| super::TranscriptLine {
            role: if index == 1 {
                "user".to_owned()
            } else {
                "assistant".to_owned()
            },
            text: format!("line {index}"),
            author_label: None,
        })
        .collect();
    let pinned = render_to_string(&state, 60, 10).unwrap();
    state.transcript_scroll = u16::MAX;

    let scrolled = render_to_string(&state, 60, 10).unwrap();

    assert_ne!(pinned, scrolled);
    assert!(!pinned.contains("Jump to bottom"));
    assert!(scrolled.contains("Jump to bottom"));
}

#[test]
fn assistant_rows_use_model_and_channel_author_label() {
    let mut assistant = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: "Ready".to_owned(),
        }],
    );
    assistant.metadata.model = Some("gemini-3.1-pro-preview".to_owned());
    assistant.metadata.provider = Some("openai-compatible".to_owned());

    let state = RatatuiApp::new("authors")
        .state_from_messages(vec![assistant], &compatibility_command_registry().all());
    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("gemini-3.1-pro-preview(openai-compatible)"));
}

#[test]
fn attachment_ui_events_render_with_custom_roles_and_authors() {
    let mut command = Message::new(
        MessageRole::Attachment,
        vec![ContentBlock::Text {
            text: "/tasks list".to_owned(),
        }],
    );
    command
        .metadata
        .attributes
        .insert("ui_role".to_owned(), "command".to_owned());

    let mut output = Message::new(
        MessageRole::Attachment,
        vec![ContentBlock::Text {
            text: "{\"count\":1}".to_owned(),
        }],
    );
    output.metadata.attributes = BTreeMap::from([
        ("ui_role".to_owned(), "command_output".to_owned()),
        ("ui_author".to_owned(), "/tasks".to_owned()),
    ]);

    let mut task = Message::new(
        MessageRole::Attachment,
        vec![ContentBlock::Text {
            text: "running review workspace [workflow]".to_owned(),
        }],
    );
    task.metadata.attributes = BTreeMap::from([
        ("ui_role".to_owned(), "task".to_owned()),
        ("ui_author".to_owned(), "Task".to_owned()),
    ]);

    let state = RatatuiApp::new("events").state_from_messages(
        vec![command, output, task],
        &compatibility_command_registry().all(),
    );
    let rendered = render_to_string(&state, 100, 24).unwrap();

    assert!(rendered.contains("You  /tasks list"));
    assert!(rendered.contains("/tasks  {\"count\":1}"));
    assert!(rendered.contains("Task  running review workspace [workflow]"));
}

#[test]
fn renders_runtime_header() {
    let mut state = RatatuiApp::new("header").initial_state();
    state.header_title = Some("code-agent-rust v0.1.0".to_owned());
    state.header_subtitle = Some("gemini-3.1-pro-preview · openai-compatible".to_owned());
    state.header_context = Some("/Users/pengfeiduan/workspace/code-agent-rust".to_owned());

    let rendered = render_to_string(&state, 80, 24).unwrap();

    assert!(rendered.contains("code-agent-rust v0.1.0"));
    assert!(rendered.contains("gemini-3.1-pro-preview"));
    assert!(rendered.contains("workspace/code-agent-rust"));
}

#[test]
fn wraps_long_runtime_header_content() {
    let mut state = RatatuiApp::new("wrapped header").initial_state();
    state.header_title = Some("code-agent-rust v0.1.0".to_owned());
    state.header_subtitle =
        Some("gemini-3.1-pro-preview · openai-compatible · reasoning".to_owned());
    state.header_context =
        Some("/Users/pengfeiduan/workspace/code-agent-rust/examples/very/long/path".to_owned());

    let rendered = render_to_string(&state, 48, 20).unwrap();

    assert!(rendered.contains("gemini-3.1-pro-preview"));
    assert!(rendered.contains("openai-compatible"));
    assert!(rendered.contains("workspace/code-agent-rust"));
}

#[test]
fn transcript_groups_render_and_toggle_from_mouse_hit_testing() {
    let mut state = RatatuiApp::new("groups").initial_state();
    state.transcript_groups = vec![TranscriptGroup {
        id: "pending-step-1".to_owned(),
        title: "Step 1 · running list_dir".to_owned(),
        subtitle: Some("2 messages".to_owned()),
        expanded: false,
        single_item: false,
        lines: vec![TranscriptLine {
            role: "assistant".to_owned(),
            text: "Tool call: list_dir".to_owned(),
            author_label: Some("gpt-5.4(chatgpt-codex)".to_owned()),
        }],
    }];

    let rendered = render_to_string(&state, 80, 24).unwrap();
    let action = mouse_action_for_position(&state, 80, 24, 1, 0);

    assert!(rendered.contains("Step 1"));
    assert_eq!(
        action,
        Some(UiMouseAction::ToggleTranscriptGroup(
            "pending-step-1".to_owned()
        ))
    );
}

#[test]
fn prompt_mouse_hit_testing_reports_cursor_targets() {
    let mut state = RatatuiApp::new("prompt-mouse").initial_state();
    state.show_input = true;
    state.input_buffer.replace("abcdef");

    let mut saw_start = false;
    let mut saw_middle = false;
    for row in 0..24 {
        for column in 0..80 {
            match mouse_action_for_position(&state, 80, 24, column, row) {
                Some(UiMouseAction::SetPromptCursor(0)) => saw_start = true,
                Some(UiMouseAction::SetPromptCursor(3)) => saw_middle = true,
                _ => {}
            }
        }
    }

    assert!(saw_start);
    assert!(saw_middle);
}

#[test]
fn transcript_item_history_group_arrow_has_click_target() {
    let mut state = RatatuiApp::new("history-arrow-hit").initial_state();
    state.transcript_items = vec![TranscriptItem::Group(TranscriptGroup {
        id: "history-group-1".to_owned(),
        title: "Read 2 files".to_owned(),
        subtitle: Some("3 messages · src/lib.rs".to_owned()),
        expanded: false,
        single_item: true,
        lines: vec![TranscriptLine {
            role: "history_tool_call".to_owned(),
            text: "Read src/lib.rs".to_owned(),
            author_label: None,
        }],
    })];

    let mut saw_toggle = false;
    for row in 0..24 {
        for column in 0..80 {
            if mouse_action_for_position(&state, 80, 24, column, row)
                == Some(UiMouseAction::ToggleTranscriptGroup(
                    "history-group-1".to_owned(),
                ))
            {
                saw_toggle = true;
                break;
            }
        }
        if saw_toggle {
            break;
        }
    }

    assert!(saw_toggle);
}

#[test]
fn history_group_toggle_hit_testing_covers_visible_header_text() {
    let mut state = RatatuiApp::new("history-arrow-scope").initial_state();
    state.transcript_items = vec![TranscriptItem::Group(TranscriptGroup {
        id: "history-group-1".to_owned(),
        title: "Ran 6 commands".to_owned(),
        subtitle: Some("12 messages · src/lib.rs".to_owned()),
        expanded: false,
        single_item: true,
        lines: vec![TranscriptLine {
            role: "history_tool_call".to_owned(),
            text: "Read src/lib.rs".to_owned(),
            author_label: None,
        }],
    })];

    assert_eq!(
        mouse_action_for_position(&state, 80, 24, 0, 0),
        Some(UiMouseAction::ToggleTranscriptGroup(
            "history-group-1".to_owned()
        ))
    );
    assert_eq!(
        mouse_action_for_position(&state, 80, 24, 4, 0),
        Some(UiMouseAction::ToggleTranscriptGroup(
            "history-group-1".to_owned()
        ))
    );
    assert_eq!(mouse_action_for_position(&state, 80, 24, 40, 0), None);
}

#[test]
fn choice_list_renders_label_and_detail_inline() {
    let mut state = RatatuiApp::new("choice-list-inline").initial_state();
    state.choice_list = Some(ChoiceListState {
        title: "File picker".to_owned(),
        subtitle: None,
        items: vec![ChoiceListItem {
            label: "src/main.rs".to_owned(),
            detail: Some("Insert @src/main.rs".to_owned()),
            secondary: None,
        }],
        selected: 0,
        empty_message: None,
    });

    let rendered = render_to_string(&state, 120, 20).unwrap();

    assert!(rendered.contains("src/main.rs - Insert @src/main.rs"));
}

#[test]
fn renders_long_backend_error_in_prompt() {
    let mut state = RatatuiApp::new("error").initial_state();
    state.show_input = true;
    state.status_line = "chatgpt-codex · gpt-5.4 · s:12345678 · error: ChatGPT Codex request failed with status 400 Bad Request: body.input.0.call_id: Field required".to_owned();
    let initial = render_to_string(&state, 80, 24).unwrap();

    state.status_marquee_tick = 56;
    let scrolled = render_to_string(&state, 80, 24).unwrap();

    assert!(initial.contains("chatgpt-codex"));
    assert!(scrolled.contains("call_id") || scrolled.contains("Field required"));
}

#[test]
fn footer_switches_to_message_actions_hints() {
    let mut state = RatatuiApp::new("message-actions").initial_state();
    state.show_input = true;
    state.message_actions = Some(TranscriptMessageActionsState {
        active_item: 0,
        enter_label: None,
        primary_input_label: Some("path".to_owned()),
    });

    let footer = footer_primary_text(&state, false);

    assert!(footer.contains("Message actions"));
    assert!(footer.contains("c copy"));
    assert!(footer.contains("p copy path"));
    assert!(footer.contains("Up/Down navigate"));
    assert!(footer.contains("Esc back"));
    assert!(!footer.contains("Enter reuse"));
}

#[test]
fn footer_switches_to_prompt_history_search_hints() {
    let mut state = RatatuiApp::new("history-search").initial_state();
    state.show_input = true;
    let mut query = InputBuffer::new();
    query.replace("beta");
    state.prompt_history_search = Some(PromptHistorySearchState {
        input_buffer: query,
        active_match: Some(2),
        match_count: 3,
        failed_match: false,
    });

    let footer = footer_primary_text(&state, false);

    assert!(footer.contains("History search"));
    assert!(footer.contains("2/3 matches"));
    assert!(footer.contains("Ctrl+R next"));
}

#[test]
fn footer_shows_enter_edit_for_user_message_actions() {
    let mut state = RatatuiApp::new("message-actions-user").initial_state();
    state.show_input = true;
    state.message_actions = Some(TranscriptMessageActionsState {
        active_item: 0,
        enter_label: Some("edit".to_owned()),
        primary_input_label: None,
    });

    let footer = footer_primary_text(&state, false);

    assert!(footer.contains("Enter edit"));
    assert!(footer.contains("c copy"));
}

#[test]
fn transcript_search_matches_visible_items() {
    let mut state = RatatuiApp::new("search").initial_state();
    state.transcript_lines = vec![
        TranscriptLine {
            role: "user".to_owned(),
            text: "first prompt".to_owned(),
            author_label: None,
        },
        TranscriptLine {
            role: "assistant".to_owned(),
            text: "error output".to_owned(),
            author_label: None,
        },
    ];
    state.transcript_groups = vec![TranscriptGroup {
        id: "pending-step-1".to_owned(),
        title: "Step 1".to_owned(),
        subtitle: Some("error detail".to_owned()),
        expanded: true,
        single_item: false,
        lines: vec![TranscriptLine {
            role: "assistant".to_owned(),
            text: "resolved".to_owned(),
            author_label: None,
        }],
    }];

    assert_eq!(transcript_search_match_items(&state, "error"), vec![1, 2]);
}

#[test]
fn single_item_transcript_groups_search_hidden_children() {
    let mut state = RatatuiApp::new("search-grouped-history").initial_state();
    state.transcript_items = vec![TranscriptItem::Group(TranscriptGroup {
        id: "history-group-1".to_owned(),
        title: "Read 2 files".to_owned(),
        subtitle: Some("3 messages · src/lib.rs".to_owned()),
        expanded: false,
        single_item: true,
        lines: vec![TranscriptLine {
            role: "assistant".to_owned(),
            text: "needle inside collapsed child".to_owned(),
            author_label: None,
        }],
    })];

    assert_eq!(transcript_search_match_items(&state, "needle"), vec![0]);
}

#[test]
fn single_item_transcript_groups_render_compact_details_when_expanded() {
    let mut state = RatatuiApp::new("history-tree").initial_state();
    state.transcript_items = vec![TranscriptItem::Group(TranscriptGroup {
        id: "history-group-1".to_owned(),
        title: "Read 2 files".to_owned(),
        subtitle: Some("Use Ctrl+R to review".to_owned()),
        expanded: true,
        single_item: true,
        lines: vec![
            TranscriptLine {
                role: "history_tool_call".to_owned(),
                text: "Read src/lib.rs".to_owned(),
                author_label: None,
            },
            TranscriptLine {
                role: "history_tool_result".to_owned(),
                text: "pub fn render_to_string(...)".to_owned(),
                author_label: None,
            },
        ],
    })];

    let rendered = render_to_string(&state, 80, 24).unwrap();

    assert!(rendered.contains("▼ Read 2 files"));
    assert!(rendered.contains("  Read src/lib.rs"));
    assert!(rendered.contains("  ⎿ pub fn render_to_string(...)"));
    assert!(!rendered.contains("├"));
    assert!(!rendered.contains("└"));
}

#[test]
fn transcript_search_scroll_targets_match() {
    let mut state = RatatuiApp::new("search-scroll").initial_state();
    state.transcript_mode = true;
    let mut input = super::InputBuffer::new();
    input.replace("line 1");
    state.transcript_search = Some(TranscriptSearchState {
        input_buffer: input,
        open: false,
        active_item: Some(0),
    });
    state.transcript_lines = (1..=14)
        .map(|index| TranscriptLine {
            role: "assistant".to_owned(),
            text: format!("line {index}"),
            author_label: None,
        })
        .collect();

    assert!(transcript_search_scroll_for_view(&state, 72, 12, 0).is_some_and(|scroll| scroll > 0));
}

#[test]
fn transcript_selection_text_uses_visual_line_slices() {
    let mut state = RatatuiApp::new("selection").initial_state();
    state.transcript_lines = vec![TranscriptLine {
        role: "assistant".to_owned(),
        text: "abcdef".to_owned(),
        author_label: None,
    }];

    let selectable_lines = transcript_selectable_lines_for_view(&state, 80);
    let text = &selectable_lines[0].text;
    let offset = text.find("abcdef").unwrap();
    let selection = TranscriptSelectionState {
        anchor: TranscriptSelectionPoint {
            line_index: selectable_lines[0].line_index,
            column: offset + 1,
        },
        focus: TranscriptSelectionPoint {
            line_index: selectable_lines[0].line_index,
            column: offset + 4,
        },
    };

    assert_eq!(
        transcript_selection_text_for_view(&state, 80, &selection).as_deref(),
        Some("bcd")
    );
}

#[test]
fn transcript_selection_highlights_exact_range() {
    let mut state = RatatuiApp::new("selection-highlight").initial_state();
    state.transcript_lines = vec![TranscriptLine {
        role: "assistant".to_owned(),
        text: "abcdef".to_owned(),
        author_label: None,
    }];
    let selectable_lines = transcript_selectable_lines_for_view(&state, 80);
    let text = &selectable_lines[0].text;
    let offset = text.find("abcdef").unwrap();
    state.transcript_selection = Some(TranscriptSelectionState {
        anchor: TranscriptSelectionPoint {
            line_index: selectable_lines[0].line_index,
            column: offset + 1,
        },
        focus: TranscriptSelectionPoint {
            line_index: selectable_lines[0].line_index,
            column: offset + 4,
        },
    });

    let lines = transcript_visual_lines(&state, 80);

    assert!(lines[0].line.spans.iter().any(|span| {
        span.content.as_ref() == "bcd" && span.style.bg == Some(super::Color::Yellow)
    }));
}

#[test]
fn prompt_selection_highlights_exact_range() {
    let mut state = RatatuiApp::new("prompt-selection").initial_state();
    state.show_input = true;
    state.input_buffer.replace("abcdef");
    state.prompt_selection = Some(PromptSelectionState {
        anchor: 1,
        focus: 4,
    });

    let line = input_prompt_line(&state);

    assert!(line.spans.iter().any(|span| {
        span.content.as_ref() == "bcd" && span.style.bg == Some(super::Color::Yellow)
    }));
}

#[test]
fn prompt_history_search_highlights_current_match() {
    let mut state = RatatuiApp::new("prompt-history-highlight").initial_state();
    state.show_input = true;
    state.input_buffer.replace("alpha beta gamma");
    let mut query = InputBuffer::new();
    query.replace("beta");
    state.prompt_history_search = Some(PromptHistorySearchState {
        input_buffer: query,
        active_match: Some(1),
        match_count: 2,
        failed_match: false,
    });

    let line = input_prompt_line(&state);

    assert!(line.spans.iter().any(|span| {
        span.content.as_ref() == "beta" && span.style.bg == Some(super::Color::Cyan)
    }));
}

#[test]
fn message_actions_highlight_selected_transcript_item() {
    let mut state = RatatuiApp::new("action-highlight").initial_state();
    state.message_actions = Some(TranscriptMessageActionsState {
        active_item: 0,
        enter_label: Some("edit".to_owned()),
        primary_input_label: None,
    });
    state.transcript_lines = vec![TranscriptLine {
        role: "user".to_owned(),
        text: "selected row".to_owned(),
        author_label: None,
    }];

    let lines = transcript_visual_lines(&state, 80);

    assert!(lines[0]
        .line
        .spans
        .iter()
        .any(|span| span.style.bg == Some(super::Color::Cyan)));
}
