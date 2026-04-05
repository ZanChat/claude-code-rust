fn render_frame(frame: &mut Frame<'_>, state: &UiState) {
    let area = frame.area();
    let layout = layout_mode(area, state);
    if matches!(layout, LayoutMode::TooSmall) {
        render_too_small(frame, area, state);
        return;
    }

    let active_pane = state.active_pane_or_default();
    let overlay_visible = overlay_kind(state).is_some();
    let suggestions_visible =
        state.show_input && !overlay_visible && !state.command_suggestions.is_empty();
    let footer_width = area.width.saturating_sub(4);
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

    if header_height > 0 {
        let header_inner = Rect::new(
            vertical[0].x.saturating_add(2),
            vertical[0].y,
            vertical[0].width.saturating_sub(4),
            vertical[0].height,
        );
        if header_inner.width > 0 && header_inner.height > 0 {
            frame.render_widget(header_widget(state, header_inner.width), header_inner);
        }
    }

    let body_area = vertical[1];
    render_body(frame, state, body_area);

    if state.show_input {
        let activity_area = vertical[2];
        let suggestion_area = vertical[3];
        let input_area = vertical[4];
        let footer_area = vertical[5];

        if activity_height > 0 {
            let inner = Rect::new(
                activity_area.x.saturating_add(2),
                activity_area.y,
                activity_area.width.saturating_sub(4),
                activity_area.height,
            );
            if inner.width > 0 && inner.height > 0 {
                frame.render_widget(activity_widget(state), inner);
            }
        }
        if suggestion_height > 0 {
            frame.render_widget(Clear, suggestion_area);
            let inner = Rect::new(
                suggestion_area.x.saturating_add(2),
                suggestion_area.y,
                suggestion_area.width.saturating_sub(4),
                suggestion_area.height,
            );
            if inner.width > 0 && inner.height > 0 {
                frame.render_widget(command_suggestions_widget(state), inner);
            }
        }
        frame.render_widget(input_widget(state), input_area);
        let footer_inner = Rect::new(
            footer_area.x.saturating_add(2),
            footer_area.y,
            footer_area.width.saturating_sub(4),
            footer_area.height,
        );
        if footer_inner.width > 0 && footer_inner.height > 0 {
            frame.render_widget(
                footer_widget(
                    state,
                    active_pane,
                    layout,
                    footer_width,
                    suggestions_visible,
                ),
                footer_inner,
            );
        }
    } else {
        let footer_area = vertical[2];
        let footer_inner = Rect::new(
            footer_area.x.saturating_add(2),
            footer_area.y,
            footer_area.width.saturating_sub(4),
            footer_area.height,
        );
        if footer_inner.width > 0 && footer_inner.height > 0 {
            if state.transcript_mode {
                frame.render_widget(
                    footer_widget(
                        state,
                        active_pane,
                        layout,
                        footer_width,
                        suggestions_visible,
                    ),
                    footer_inner,
                );
            } else {
                frame.render_widget(
                    Paragraph::new(vec![compose_footer_line(
                        &line_text(&status_line(state)),
                        pane_shortcut_label(),
                        footer_width,
                    )])
                    .wrap(Wrap { trim: false }),
                    footer_inner,
                );
            }
        }
    }

    render_overlay(frame, state, area);
}

pub fn draw_terminal<B: Backend>(terminal: &mut Terminal<B>, state: &UiState) -> Result<()> {
    terminal.draw(|frame| render_frame(frame, state))?;
    Ok(())
}

pub fn render_to_string(state: &UiState, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    draw_terminal(&mut terminal, state)?;

    let buffer = terminal.backend().buffer().clone();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_owned());
    }
    Ok(lines.join("\n"))
}
