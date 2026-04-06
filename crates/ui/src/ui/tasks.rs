fn task_kind_label(kind: &str) -> Option<&str> {
    match kind {
        "" | "task" | "workflow" | "workflow_step" => None,
        "assistant_worker" => Some("worker"),
        "assistant_synthesis" => Some("synthesis"),
        other => Some(other),
    }
}

fn task_status_visual(task: &TaskUiEntry) -> (&'static str, Style, Style) {
    let theme = UiTheme::current();
    match task.status {
        TaskStatus::Pending => ("○", theme.muted_style(), Style::default()),
        TaskStatus::Running => (
            "●",
            Style::default()
                .fg(theme.info_color())
                .add_modifier(Modifier::BOLD),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        TaskStatus::WaitingForInput => (
            "◆",
            Style::default()
                .fg(theme.warning_color())
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(theme.warning_color())
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Completed if task.is_recent_completion => (
            "✓",
            Style::default()
                .fg(theme.success_color())
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.success_color()),
        ),
        TaskStatus::Completed => (
            "✓",
            Style::default().fg(theme.success_color()),
            Style::default()
                .fg(theme.muted_color())
                .add_modifier(Modifier::DIM),
        ),
        TaskStatus::Failed => (
            "✕",
            Style::default().fg(theme.error_color()).add_modifier(Modifier::BOLD),
            Style::default().fg(theme.error_color()),
        ),
        TaskStatus::Cancelled => (
            "◌",
            Style::default().fg(theme.task_color()),
            Style::default()
                .fg(theme.muted_color())
                .add_modifier(Modifier::DIM),
        ),
    }
}

fn task_header_line(task: &TaskUiEntry, render_as_root: bool) -> Line<'static> {
    let theme = UiTheme::current();
    let (icon, icon_style, title_style) = task_status_visual(task);
    let mut spans = Vec::new();

    if !render_as_root && !task.tree_prefix.is_empty() {
        spans.push(Span::styled(task.tree_prefix.clone(), theme.muted_style()));
    }

    spans.push(Span::styled(format!("{icon} "), icon_style));
    spans.push(Span::styled(task.title.clone(), title_style));

    if let Some(owner) = task
        .owner_label
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        spans.push(Span::styled(
            format!(" (@{owner})"),
            Style::default().fg(theme.info_color()),
        ));
    }

    if !task.blocker_labels.is_empty() {
        let blockers = task
            .blocker_labels
            .iter()
            .map(|label| format!("#{label}"))
            .collect::<Vec<_>>()
            .join(", ");
        spans.push(Span::styled(
            format!("  ➤ blocked by {blockers}"),
            theme.muted_style(),
        ));
    }

    if let Some(kind) = task_kind_label(&task.kind) {
        spans.push(Span::styled(format!("  [{kind}]"), theme.muted_style()));
    }

    Line::from(spans)
}

fn indented_detail_lines(text: &str, indent: &str, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut detail_lines = text
        .split('\n')
        .map(str::trim_end)
        .filter(|line| !line.is_empty());

    for line in detail_lines.by_ref().take(2) {
        lines.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }

    if detail_lines.next().is_some() {
        lines.push(Line::from(Span::styled(format!("{indent}…"), style)));
    }

    lines
}

fn task_prefers_input(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::WaitingForInput
    )
}

fn task_detail_text(task: &TaskUiEntry) -> Option<&str> {
    let detail = if task_prefers_input(&task.status) {
        task.input.as_deref().or(task.output.as_deref())
    } else {
        task.output.as_deref().or(task.input.as_deref())
    }?
    .trim();

    if detail.is_empty() || detail.eq_ignore_ascii_case(task.title.trim()) {
        None
    } else {
        Some(detail)
    }
}

fn hidden_task_summary(tasks: &[TaskUiEntry]) -> Option<Line<'static>> {
    if tasks.is_empty() {
        return None;
    }

    let running = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Running | TaskStatus::WaitingForInput
            )
        })
        .count();
    let pending = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Pending)
        .count();
    let completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let failed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled))
        .count();

    let mut parts = Vec::new();
    if running > 0 {
        parts.push(format!("{running} active"));
    }
    if pending > 0 {
        parts.push(format!("{pending} pending"));
    }
    if completed > 0 {
        parts.push(format!("{completed} done"));
    }
    if failed > 0 {
        parts.push(format!("{failed} stopped"));
    }

    Some(Line::from(Span::styled(
        format!("… +{}", parts.join(", ")),
        UiTheme::current().muted_style(),
    )))
}

fn task_lines(state: &UiState, max_items: usize, detailed: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if state.task_items.is_empty() && state.question_items.is_empty() {
        if state.task_preview.lines.is_empty() {
            lines.push(Line::from("No task activity yet."));
        } else {
            lines.extend(
                state
                    .task_preview
                    .lines
                    .iter()
                    .take(max_items)
                    .map(|line| Line::from(line.clone())),
            );
        }
        return lines;
    }

    for task in state.task_items.iter().take(max_items) {
        lines.push(task_header_line(task, false));

        if detailed {
            if let Some(detail) = task_detail_text(task) {
                lines.extend(indented_detail_lines(
                    detail,
                    &task.detail_prefix,
                    UiTheme::current().muted_style(),
                ));
            }
        }
    }

    if let Some(summary) = hidden_task_summary(state.task_items.get(max_items..).unwrap_or(&[])) {
        lines.push(summary);
    }

    if detailed {
        for question in state.question_items.iter().take(2) {
            lines.push(Line::from(vec![
                Span::styled(
                    "ASK  ",
                    UiTheme::current().warning_bold_style(),
                ),
                Span::raw(question.prompt.clone()),
            ]));
            if !question.choices.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  choices: {}", question.choices.join(", ")),
                    UiTheme::current().muted_style(),
                )));
            }
        }
    }

    lines
}

fn progress_spinner_frame(tick: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
    FRAMES[tick % FRAMES.len()]
}

fn sanitize_progress_detail(detail: &str) -> &str {
    let trimmed = detail.trim();
    for prefix in ["- ", "\\ ", "| ", "/ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    trimmed
}

fn spinner_headline(message: &str) -> String {
    if message.ends_with('…') || message.ends_with("...") {
        message.to_owned()
    } else {
        format!("{message}…")
    }
}

fn progress_line(state: &UiState) -> Option<Line<'static>> {
    if state.progress_message.is_none() && state.progress_verb.is_none() {
        return None;
    }

    let detail = state
        .progress_message
        .as_deref()
        .map(sanitize_progress_detail)
        .filter(|detail| !detail.is_empty());
    let verb = state
        .progress_verb
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(detail)
        .unwrap_or("Working");
    let detail = state
        .progress_verb
        .as_deref()
        .and(detail)
        .filter(|detail| !detail.eq_ignore_ascii_case(verb));

    let mut spans = vec![
        Span::styled(
            format!("{} ", progress_spinner_frame(state.status_marquee_tick)),
            Style::default()
                .fg(UiTheme::current().info_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            spinner_headline(verb),
            Style::default()
                .fg(UiTheme::current().info_color())
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(detail) = detail {
        spans.push(Span::styled(" · ", UiTheme::current().muted_style()));
        spans.push(Span::styled(detail.to_owned(), UiTheme::current().muted_style()));
    }

    Some(Line::from(spans))
}

fn permission_lines(state: &UiState) -> Vec<Line<'static>> {
    if let Some(prompt) = &state.permission_prompt {
        vec![
            Line::from(vec![
                Span::styled(
                    "ASK  ",
                    UiTheme::current().warning_bold_style(),
                ),
                Span::raw(prompt.tool_name.clone()),
            ]),
            Line::from(prompt.summary.clone()),
            Line::from(Span::styled(
                format!("{} / {}", prompt.allow_once_label, prompt.deny_label),
                UiTheme::current().muted_style(),
            )),
        ]
    } else {
        vec![Line::from("No pending permission prompts.")]
    }
}

fn activity_lines(state: &UiState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(text) = &state.compact_banner {
        lines.push(Line::from(vec![
            Span::styled(
                "compact  ",
                UiTheme::current().warning_bold_style(),
            ),
            Span::raw(text.clone()),
        ]));
    }

    if let Some(progress) = progress_line(state) {
        lines.push(progress);
    }

    let active_task_ids = state
        .task_items
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Running | TaskStatus::WaitingForInput
            )
        })
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();

    for task in state
        .task_items
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Running | TaskStatus::WaitingForInput
            )
        })
        .take(5)
    {
        let render_as_root = task
            .parent_id
            .as_deref()
            .map(|parent_id| !active_task_ids.contains(&parent_id))
            .unwrap_or(true);

        lines.push(task_header_line(task, render_as_root));
        if let Some(detail) = task_detail_text(task) {
            lines.extend(indented_detail_lines(
                detail,
                if render_as_root {
                    "  "
                } else {
                    &task.detail_prefix
                },
                UiTheme::current().muted_style(),
            ));
        }
    }

    for queued_input in state.queued_inputs.iter().take(3) {
        lines.push(Line::from(vec![
            Span::styled(
                "queue  ",
                UiTheme::current().muted_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled(queued_input.clone(), UiTheme::current().muted_style()),
        ]));
    }
    if state.queued_inputs.len() > 3 {
        lines.push(Line::from(Span::styled(
            format!(
                "queue  +{} more follow-up messages",
                state.queued_inputs.len() - 3
            ),
            UiTheme::current().muted_style(),
        )));
    }

    if let Some(question) = state.question_items.first() {
        lines.push(Line::from(vec![
            Span::styled(
                "ASK  ",
                UiTheme::current().warning_bold_style(),
            ),
            Span::raw(question.prompt.clone()),
        ]));
    }

    if lines.is_empty() {
        if let Some(prompt) = &state.permission_prompt {
            lines.push(Line::from(vec![
                Span::styled(
                    "wait  ",
                    UiTheme::current().warning_bold_style(),
                ),
                Span::raw(prompt.summary.clone()),
            ]));
        } else if let Some(notification) = state.notifications.back() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}  ", notification.title),
                    notification.level.unwrap_or(StatusLevel::Info).style(),
                ),
                Span::raw(notification.body.clone()),
            ]));
        }
    }

    lines
}

fn should_show_activity_section(
    state: &UiState,
    overlay_visible: bool,
    suggestions_visible: bool,
) -> bool {
    state.show_input
        && !overlay_visible
        && (!suggestions_visible
            || state.progress_message.is_some()
            || !state.queued_inputs.is_empty())
}

fn activity_widget(state: &UiState) -> Paragraph<'static> {
    Paragraph::new(activity_lines(state)).wrap(Wrap { trim: false })
}

fn push_wrapped_styled_lines(lines: &mut Vec<Line<'static>>, text: &str, width: u16, style: Style) {
    for segment in wrap_plain_text(text, width.max(1) as usize) {
        lines.push(Line::from(Span::styled(segment, style)));
    }
}

