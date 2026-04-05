fn header_lines(state: &UiState, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(title) = state
        .header_title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        push_wrapped_styled_lines(
            &mut lines,
            title,
            width,
            Style::default().add_modifier(Modifier::BOLD),
        );
    }

    if let Some(subtitle) = state
        .header_subtitle
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        push_wrapped_styled_lines(
            &mut lines,
            subtitle,
            width,
            Style::default().fg(Color::DarkGray),
        );
    }

    if let Some(context) = state
        .header_context
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        push_wrapped_styled_lines(
            &mut lines,
            context,
            width,
            Style::default().fg(Color::DarkGray),
        );
    }

    lines
}

fn header_height(state: &UiState, width: u16) -> u16 {
    header_lines(state, width).len() as u16
}

fn header_widget(state: &UiState, width: u16) -> Paragraph<'static> {
    Paragraph::new(header_lines(state, width)).wrap(Wrap { trim: false })
}

fn status_line(state: &UiState) -> Line<'static> {
    if let Some(prompt) = &state.permission_prompt {
        return Line::from(vec![
            Span::styled("permission ", Style::default().fg(Color::Yellow)),
            Span::raw(format!(
                "{} -> {} / {}",
                prompt.tool_name, prompt.allow_once_label, prompt.deny_label
            )),
        ]);
    }

    if let Some(notification) = state.notifications.back() {
        return Line::from(vec![
            Span::styled(
                format!("{} ", notification.title),
                notification.level.unwrap_or(StatusLevel::Info).style(),
            ),
            Span::raw(notification.body.clone()),
        ]);
    }

    Line::from(state.status_line.clone())
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    wrap_plain_text(text, width.max(1) as usize).len().max(1) as u16
}

fn wrapped_lines_height(lines: &[Line<'_>], width: u16) -> u16 {
    lines
        .iter()
        .map(|line| wrapped_line_count(&line_text(line), width))
        .fold(0u16, u16::saturating_add)
        .max(1)
}

fn prompt_row_height(state: &UiState, width: u16) -> u16 {
    let prompt_text = format!("> {}", state.input_buffer.as_str());
    wrapped_line_count(&prompt_text, width)
        .max(1)
        .saturating_add(2)
        .clamp(3, 6)
}

fn navigation_hint(
    state: &UiState,
    active_pane: PaneKind,
    layout: LayoutMode,
    suggestions_visible: bool,
) -> String {
    let suggestion_hint = if suggestions_visible {
        "Up/Down choose"
    } else {
        "Up/Down scroll"
    };
    let focus_label = if matches!(active_pane, PaneKind::Transcript) {
        "Transcript".to_owned()
    } else {
        format!("{} open", active_pane.title())
    };
    let pending_hint = transcript_details_toggle_label(state)
        .map(|hint| format!("  {hint}"))
        .unwrap_or_default();

    if matches!(layout, LayoutMode::Compact) {
        format!(
            "{focus_label}  {} panes  {suggestion_hint}{pending_hint}",
            pane_shortcut_label()
        )
    } else {
        format!(
            "{focus_label}  Tab cycle  {} panes  {suggestion_hint}{pending_hint}  Ctrl-C exit",
            pane_shortcut_label()
        )
    }
}

fn compose_footer_line(left: &str, right: &str, width: u16) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(String::new());
    }
    if right.is_empty() {
        return Line::from(truncate_middle(left, width));
    }

    let right = truncate_middle(right, width.saturating_sub(1));
    let right_len = right.chars().count();
    if right_len >= width {
        return Line::from(right);
    }

    let left_max = width.saturating_sub(right_len + 1);
    let left = truncate_middle(left, left_max);
    let left_len = left.chars().count();
    let gap = width.saturating_sub(left_len + right_len).max(1);
    Line::from(format!("{left}{}{right}", " ".repeat(gap)))
}

fn marquee_text(text: &str, width: u16, tick: usize) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }

    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return text.to_owned();
    }

    let cycle_len = chars.len() + 3;
    let mut looped = chars.clone();
    looped.extend([' ', ' ', ' ']);
    looped.extend(chars);
    let start = tick % cycle_len;
    looped
        .into_iter()
        .skip(start)
        .take(width)
        .collect::<String>()
}

fn footer_primary_text(state: &UiState, suggestions_visible: bool) -> String {
    if let Some(actions) = state.message_actions.as_ref() {
        let mut parts = vec!["Message actions".to_owned()];
        if let Some(label) = actions.enter_label.as_deref() {
            parts.push(format!("Enter {label}"));
        }
        parts.push("c copy".to_owned());
        if let Some(label) = actions.primary_input_label.as_deref() {
            parts.push(format!("p copy {label}"));
        }
        parts.push("Up/Down navigate".to_owned());
        parts.push("Esc back".to_owned());
        return parts.join(" · ");
    }
    if let Some(search) = state.prompt_history_search.as_ref() {
        let query = search.input_buffer.as_str();
        if query.trim().is_empty() {
            return "History search · Type to search · Enter keep · Esc cancel".to_owned();
        }
        if search.match_count == 0 {
            return format!(
                "History search · {query} · no matches · Enter keep · Esc cancel · Ctrl+R next"
            );
        }
        if search.failed_match {
            return format!(
                "History search · {query} · end of matches · Enter keep · Esc cancel · Ctrl+R next"
            );
        }
        return format!(
            "History search · {}/{} match{} · Enter keep · Esc cancel · Ctrl+R next",
            search.active_match.unwrap_or(1),
            search.match_count,
            if search.match_count == 1 { "" } else { "es" }
        );
    }
    if let Some(search) = state.transcript_search.as_ref() {
        let query = search.input_buffer.as_str();
        let matches = transcript_search_match_items(state, &query);
        if search.open {
            if matches.is_empty() {
                return "Search · Enter keep · Esc cancel".to_owned();
            }
            let active_position = search
                .active_item
                .and_then(|item| matches.iter().position(|candidate| *candidate == item))
                .map(|index| index + 1)
                .unwrap_or(1);
            return format!(
                "Search · {active_position}/{} match{} · Enter keep · Esc cancel · n/N next",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            );
        }
        if !query.trim().is_empty() {
            if matches.is_empty() {
                return format!("Transcript mode · / {query} · no matches · q exit");
            }
            let active_position = search
                .active_item
                .and_then(|item| matches.iter().position(|candidate| *candidate == item))
                .map(|index| index + 1)
                .unwrap_or(1);
            return format!(
                "Transcript mode · / {query} · {active_position}/{} match{} · n/N next · q exit",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            );
        }
    }
    if state.transcript_mode {
        let toggle_hint = history_group_toggle_label(state)
            .map(|hint| format!(" · {hint}"))
            .unwrap_or_default();
        return format!("Transcript mode · / search · q or Esc exit{toggle_hint}");
    }
    if state.vim_state.is_insert() {
        return "-- INSERT --".to_owned();
    }
    if suggestions_visible {
        return "Command suggestions".to_owned();
    }
    if state.permission_prompt.is_some() {
        return "Waiting for permission".to_owned();
    }
    if !state.queued_inputs.is_empty() {
        let pending_hint = transcript_details_toggle_label(state)
            .map(|hint| format!(" · {hint}"))
            .unwrap_or_default();
        return format!(
            "Working · Ctrl+C to interrupt · {} queued{pending_hint}",
            state.queued_inputs.len(),
        );
    }
    if state.progress_message.is_some() || state.pending_step_count > 0 {
        if let Some(hint) = transcript_details_toggle_label(state) {
            return format!("Working · Ctrl+C to interrupt · {hint}");
        }
        return "Working · Ctrl+C to interrupt".to_owned();
    }
    if let Some(helper) = state.prompt_helper.as_deref() {
        return helper.to_owned();
    }
    if state.input_buffer.is_empty() {
        "Type a prompt or /command".to_owned()
    } else {
        "Enter to send".to_owned()
    }
}

fn footer_widget(
    state: &UiState,
    active_pane: PaneKind,
    layout: LayoutMode,
    width: u16,
    suggestions_visible: bool,
) -> Paragraph<'static> {
    if let Some(search) = state
        .transcript_search
        .as_ref()
        .filter(|search| search.open)
    {
        let search_text = search.input_buffer.as_str();
        let search_line = if search.input_buffer.cursor < search_text.chars().count() {
            let left = search_text
                .chars()
                .take(search.input_buffer.cursor)
                .collect::<String>();
            let cursor_char = search_text
                .chars()
                .skip(search.input_buffer.cursor)
                .take(1)
                .collect::<String>();
            let right = search_text
                .chars()
                .skip(search.input_buffer.cursor + 1)
                .collect::<String>();
            Line::from(vec![
                Span::raw("/ "),
                Span::raw(left),
                Span::styled(
                    cursor_char,
                    Style::default().bg(Color::White).fg(Color::Black),
                ),
                Span::raw(right),
            ])
        } else {
            Line::from(vec![
                Span::raw(format!("/ {search_text}")),
                Span::styled(" ", Style::default().bg(Color::White)),
            ])
        };

        let hint = compose_footer_line(
            &footer_primary_text(state, suggestions_visible),
            pane_shortcut_label(),
            width,
        );
        return Paragraph::new(vec![search_line, hint]).wrap(Wrap { trim: false });
    }

    let secondary_text = line_text(&status_line(state));
    let primary = compose_footer_line(
        &footer_primary_text(state, suggestions_visible),
        &navigation_hint(state, active_pane, layout, suggestions_visible),
        width,
    );
    let secondary = Line::from(Span::styled(
        marquee_text(&secondary_text, width, state.status_marquee_tick),
        Style::default().fg(Color::DarkGray),
    ));

    Paragraph::new(vec![primary, secondary]).wrap(Wrap { trim: false })
}

fn input_prompt_line(state: &UiState) -> Line<'static> {
    let text = state.input_buffer.as_str();
    let text_len = text.chars().count();

    if let Some((start, end)) = state
        .prompt_selection
        .as_ref()
        .and_then(|selection| prompt_selection_range(selection, text_len))
    {
        let left = text.chars().take(start).collect::<String>();
        let selected = text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect::<String>();
        let right = text.chars().skip(end).collect::<String>();
        let mut spans = vec![Span::raw("> ")];
        if !left.is_empty() {
            spans.push(Span::raw(left));
        }
        if !selected.is_empty() {
            spans.push(Span::styled(selected, selection_highlight_style()));
        }
        if !right.is_empty() {
            spans.push(Span::raw(right));
        }
        return Line::from(spans);
    }

    if let Some(search) = state.prompt_history_search.as_ref() {
        let query = search.input_buffer.as_str();
        if !query.trim().is_empty() && !search.failed_match {
            if let Some((start, end)) = prompt_history_match_range(&text, &query) {
                let left = text.chars().take(start).collect::<String>();
                let matched = text
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect::<String>();
                let right = text.chars().skip(end).collect::<String>();
                let mut spans = vec![Span::raw("> ")];
                if !left.is_empty() {
                    spans.push(Span::raw(left));
                }
                if !matched.is_empty() {
                    spans.push(Span::styled(
                        matched,
                        Style::default().bg(Color::Cyan).fg(Color::Black),
                    ));
                }
                if !right.is_empty() {
                    spans.push(Span::raw(right));
                }
                return Line::from(spans);
            }
        }
    }

    let pos = state.input_buffer.cursor.min(text.chars().count());
    if pos < text_len {
        let left = text.chars().take(pos).collect::<String>();
        let cursor_char = text.chars().skip(pos).take(1).collect::<String>();
        let right = text.chars().skip(pos + 1).collect::<String>();
        return Line::from(vec![
            Span::raw("> "),
            Span::raw(left),
            Span::styled(
                cursor_char,
                Style::default().bg(Color::White).fg(Color::Black),
            ),
            Span::raw(right),
        ]);
    }

    Line::from(vec![
        Span::raw(format!("> {text}")),
        Span::styled(" ", Style::default().bg(Color::White)),
    ])
}

fn input_widget(state: &UiState) -> Paragraph<'static> {
    Paragraph::new(vec![input_prompt_line(state)])
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::TOP | Borders::BOTTOM))
}

fn prompt_history_match_range(text: &str, query: &str) -> Option<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }

    let byte_index = text.rfind(query)?;
    let start = text[..byte_index].chars().count();
    let end = start + query.chars().count();
    Some((start, end))
}

fn command_suggestions_widget(state: &UiState) -> Paragraph<'static> {
    let lines = state
        .command_suggestions
        .iter()
        .take(MAX_VISIBLE_SUGGESTIONS)
        .enumerate()
        .map(|(index, entry)| {
            let selected = state.selected_command_suggestion == Some(index);
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let prefix = if selected { "> " } else { "  " };
            Line::from(Span::styled(
                format!("{prefix}{:<14} {}", entry.name, entry.description),
                style,
            ))
        })
        .collect::<Vec<_>>();

    Paragraph::new(lines).wrap(Wrap { trim: true })
}

fn overlay_title(kind: PaneKind, preview_title: &str) -> String {
    if preview_title.is_empty() || preview_title == kind.title() {
        kind.title().to_owned()
    } else {
        format!("{} · {}", kind.title(), preview_title)
    }
}

fn choice_list_lines(choice_list: &ChoiceListState) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        choice_list.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ))];

    lines.push(Line::from(Span::styled(
        choice_list
            .subtitle
            .clone()
            .unwrap_or_else(|| "Enter to select · Esc to cancel".to_owned()),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    if choice_list.items.is_empty() {
        lines.push(Line::from(
            choice_list
                .empty_message
                .clone()
                .unwrap_or_else(|| "No choices available.".to_owned()),
        ));
        return lines;
    }

    let max_visible = 8usize;
    let selected = choice_list
        .selected
        .min(choice_list.items.len().saturating_sub(1));
    let start = selected
        .saturating_sub(max_visible / 2)
        .min(choice_list.items.len().saturating_sub(max_visible));
    let end = (start + max_visible).min(choice_list.items.len());

    for (offset, item) in choice_list.items[start..end].iter().enumerate() {
        let index = start + offset;
        let is_selected = index == selected;
        let prefix = if is_selected { "> " } else { "  " };
        let item_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let detail_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut spans = vec![Span::styled(format!("{prefix}{}", item.label), item_style)];
        if let Some(detail) = item
            .detail
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            spans.push(Span::styled(" - ".to_owned(), detail_style));
            spans.push(Span::styled(detail.to_owned(), detail_style));
        }
        lines.push(Line::from(spans));
        if let Some(secondary) = item
            .secondary
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(Line::from(Span::styled(
                format!("    {secondary}"),
                detail_style,
            )));
        }
    }

    if choice_list.items.len() > max_visible {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} of {}", selected + 1, choice_list.items.len()),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines
}

fn overlay_content_lines(state: &UiState, kind: OverlayKind) -> Vec<Line<'static>> {
    match kind {
        OverlayKind::ChoiceList => state
            .choice_list
            .as_ref()
            .map(choice_list_lines)
            .unwrap_or_default(),
        OverlayKind::Pane(kind) => {
            let preview = pane_preview(state, kind);
            let mut lines = vec![Line::from(Span::styled(
                overlay_title(kind, &preview.title),
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            lines.push(Line::from(""));

            match kind {
                PaneKind::Tasks => lines.extend(task_lines(state, 8, true)),
                PaneKind::Permissions => lines.extend(permission_lines(state)),
                _ => {
                    if preview.lines.is_empty() {
                        lines.push(Line::from("No details available."));
                    } else {
                        lines.extend(preview.lines.into_iter().map(Line::from));
                    }
                }
            }

            lines
        }
    }
}

fn overlay_rect(area: Rect, desired_lines: usize) -> Option<Rect> {
    if area.width < 28 || area.height < 8 {
        return None;
    }

    let max_height = area.height.saturating_sub(MODAL_TRANSCRIPT_PEEK);
    if max_height < 5 {
        return None;
    }

    let preferred_height = (desired_lines as u16).saturating_add(3).max(6);
    let height = preferred_height.min(max_height);
    let y = area.y + area.height.saturating_sub(height);
    Some(Rect::new(area.x, y, area.width, height))
}

fn render_overlay(frame: &mut Frame<'_>, state: &UiState, area: Rect) {
    let Some(kind) = overlay_kind(state) else {
        return;
    };
    let lines = overlay_content_lines(state, kind);
    let desired_height = wrapped_lines_height(&lines, area.width.saturating_sub(4)) as usize;
    let Some(sheet_area) = overlay_rect(area, desired_height) else {
        return;
    };

    let divider_area = Rect::new(sheet_area.x, sheet_area.y, sheet_area.width, 1);
    let content_area = Rect::new(
        sheet_area.x.saturating_add(2),
        sheet_area.y.saturating_add(1),
        sheet_area.width.saturating_sub(4),
        sheet_area.height.saturating_sub(1),
    );
    frame.render_widget(Clear, sheet_area);
    frame.render_widget(
        Paragraph::new(Line::from("▔".repeat(sheet_area.width as usize)))
            .style(Style::default().fg(Color::Yellow)),
        divider_area,
    );
    if content_area.width > 0 && content_area.height > 0 {
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            content_area,
        );
    }
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let min_height = if state.show_input {
        MIN_REPL_HEIGHT
    } else {
        MIN_HEIGHT
    };
    let comfortable_height = if state.show_input {
        COMPACT_REPL_HEIGHT
    } else {
        COMPACT_HEIGHT
    };
    let width_hint = MIN_WIDTH.max(COMPACT_WIDTH);
    let notice = Paragraph::new(vec![
        Line::from(Span::styled(
            "code-agent-rust",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "Terminal too small. Need at least {}x{}.",
            MIN_WIDTH, min_height
        )),
        Line::from(format!(
            "For a comfortable REPL, use about {}x{} or wider.",
            width_hint, comfortable_height
        )),
        Line::from("Resize the terminal to continue."),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().title("Display").borders(Borders::ALL));
    frame.render_widget(notice, area);
}

#[derive(Clone, Debug)]
struct TranscriptBodyLayout {
    header_area: Option<Rect>,
    transcript_area: Rect,
    visible_lines: Vec<TranscriptRenderLine>,
    effective_scroll: u16,
}

fn transcript_body_layout(state: &UiState, body_area: Rect) -> Option<TranscriptBodyLayout> {
    if body_area.width == 0 || body_area.height == 0 {
        return None;
    }

    let (_, initial_scroll) = transcript_viewport(state, body_area.width, body_area.height);
    let sticky_visible = initial_scroll > 0 && body_area.height > 1;
    let transcript_height = if sticky_visible {
        body_area.height.saturating_sub(1)
    } else {
        body_area.height
    };
    let (visible_lines, effective_scroll) =
        transcript_viewport(state, body_area.width, transcript_height);
    let (header_area, transcript_area) = if sticky_visible {
        (
            Some(Rect::new(body_area.x, body_area.y, body_area.width, 1)),
            Rect::new(
                body_area.x,
                body_area.y.saturating_add(1),
                body_area.width,
                body_area.height.saturating_sub(1),
            ),
        )
    } else {
        (None, body_area)
    };

    Some(TranscriptBodyLayout {
        header_area,
        transcript_area,
        visible_lines,
        effective_scroll,
    })
}

fn render_body(frame: &mut Frame<'_>, state: &UiState, body_area: Rect) {
    let Some(layout) = transcript_body_layout(state, body_area) else {
        return;
    };

    if let Some(area) = layout.header_area {
        if let Some(widget) = sticky_prompt_widget(state, area.width, layout.effective_scroll) {
            frame.render_widget(widget, area);
        }
    }
    if layout.transcript_area.width > 0 && layout.transcript_area.height > 0 {
        let lines = layout
            .visible_lines
            .iter()
            .map(|line| line.line.clone())
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), layout.transcript_area);
        if let Some(pill) = scroll_pill_widget(layout.effective_scroll) {
            let pill_area = Rect::new(
                layout.transcript_area.x,
                layout.transcript_area.y + layout.transcript_area.height.saturating_sub(1),
                layout.transcript_area.width,
                1,
            );
            frame.render_widget(pill, pill_area);
        }
    }
}

fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn prompt_cursor_action_for_position(
    state: &UiState,
    prompt_area: Rect,
    column: u16,
    row: u16,
) -> Option<UiMouseAction> {
    if !state.show_input || prompt_area.width == 0 || prompt_area.height <= 2 {
        return None;
    }

    let prompt_inner = Rect::new(
        prompt_area.x,
        prompt_area.y.saturating_add(1),
        prompt_area.width,
        prompt_area.height.saturating_sub(2),
    );
    if !point_in_rect(column, row, prompt_inner) {
        return None;
    }

    let prompt_text = format!("> {}", state.input_buffer.as_str());
    let wrapped = wrap_plain_text(
        &prompt_text,
        prompt_area.width.saturating_sub(2).max(1) as usize,
    );
    let local_row = row.saturating_sub(prompt_inner.y) as usize;
    let prompt_index = if let Some(line) = wrapped.get(local_row) {
        let prefix = wrapped
            .iter()
            .take(local_row)
            .map(|segment| segment.chars().count())
            .sum::<usize>();
        let local_column = column.saturating_sub(prompt_inner.x) as usize;
        prefix + local_column.min(line.chars().count())
    } else {
        prompt_text.chars().count()
    };

    Some(UiMouseAction::SetPromptCursor(
        prompt_index
            .saturating_sub(2)
            .min(state.input_buffer.chars.len()),
    ))
}

pub fn mouse_action_for_position(
    state: &UiState,
    width: u16,
    height: u16,
    column: u16,
    row: u16,
) -> Option<UiMouseAction> {
    let area = Rect::new(0, 0, width, height);
    let layout = layout_mode(area, state);
    if matches!(layout, LayoutMode::TooSmall) {
        return None;
    }

    let overlay_visible = overlay_kind(state).is_some();
    let suggestions_visible =
        state.show_input && !overlay_visible && !state.command_suggestions.is_empty();
    let header_width = area.width.saturating_sub(4);
    let header_height = header_height(state, header_width);
    let activity_width = area.width.saturating_sub(4);
    let activity_content = activity_lines(state);
    let mut activity_height =
        if should_show_activity_section(state, overlay_visible, suggestions_visible) {
            wrapped_lines_height(&activity_content, activity_width).min(
                if matches!(layout, LayoutMode::Compact) {
                    6
                } else {
                    10
                },
            )
        } else {
            0
        };
    let mut suggestion_height = if suggestions_visible {
        let suggestion_lines = state
            .command_suggestions
            .iter()
            .take(MAX_VISIBLE_SUGGESTIONS)
            .enumerate()
            .map(|(index, entry)| {
                let selected = state.selected_command_suggestion == Some(index);
                let prefix = if selected { "> " } else { "  " };
                Line::from(format!("{prefix}{:<14} {}", entry.name, entry.description))
            })
            .collect::<Vec<_>>();
        wrapped_lines_height(&suggestion_lines, area.width.saturating_sub(4))
            .min(MAX_VISIBLE_SUGGESTIONS as u16 + 2)
    } else {
        0
    };
    let prompt_height = if state.show_input {
        prompt_row_height(state, area.width.saturating_sub(2)).max(
            if matches!(layout, LayoutMode::Compact) {
                COMPACT_INPUT_HEIGHT.saturating_sub(3)
            } else {
                STANDARD_INPUT_HEIGHT.saturating_sub(3)
            },
        )
    } else {
        0
    };
    let mut footer_height = footer_height_for_state(state);
    let transcript_min_height = if state.show_input {
        if matches!(layout, LayoutMode::Compact) {
            4
        } else {
            5
        }
    } else if matches!(layout, LayoutMode::Compact) {
        7
    } else {
        8
    };

    if state.show_input {
        let max_reserved = area.height.saturating_sub(transcript_min_height);
        while activity_height + suggestion_height + prompt_height + footer_height > max_reserved {
            if suggestion_height > 0 {
                suggestion_height -= 1;
                continue;
            }
            if activity_height > 0 {
                activity_height -= 1;
                continue;
            }
            if footer_height > 1 {
                footer_height -= 1;
                continue;
            }
            break;
        }
    }

    let vertical = if state.show_input {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Min(transcript_min_height),
                Constraint::Length(activity_height),
                Constraint::Length(suggestion_height),
                Constraint::Length(prompt_height),
                Constraint::Length(footer_height),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Min(transcript_min_height),
                Constraint::Length(footer_height),
            ])
            .split(area)
    };
    let body_area = vertical[1];
    let body_layout = transcript_body_layout(state, body_area)?;

    if let Some(header_area) = body_layout.header_area {
        if point_in_rect(column, row, header_area) {
            return Some(UiMouseAction::JumpToBottom);
        }
    }
    if body_layout.effective_scroll > 0 {
        let pill_area = Rect::new(
            body_layout.transcript_area.x,
            body_layout.transcript_area.y + body_layout.transcript_area.height.saturating_sub(1),
            body_layout.transcript_area.width,
            1,
        );
        if point_in_rect(column, row, pill_area) {
            return Some(UiMouseAction::JumpToBottom);
        }
    }
    if state.show_input {
        let input_area = vertical[4];
        if let Some(action) = prompt_cursor_action_for_position(state, input_area, column, row) {
            return Some(action);
        }
    }
    if !point_in_rect(column, row, body_layout.transcript_area) {
        return None;
    }

    let line_index = row.saturating_sub(body_layout.transcript_area.y) as usize;
    match body_layout
        .visible_lines
        .get(line_index)
        .map(|line| &line.kind)
    {
        Some(TranscriptRenderLineKind::GroupHeader(id)) => {
            Some(UiMouseAction::ToggleTranscriptGroup(id.clone()))
        }
        _ => None,
    }
}

