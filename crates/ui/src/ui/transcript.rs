fn transcript_role(message: &Message) -> String {
    message
        .metadata
        .attributes
        .get(UI_ROLE_ATTRIBUTE)
        .cloned()
        .unwrap_or_else(|| format!("{:?}", message.role).to_lowercase())
}

pub fn transcript_line_from_message(message: &Message) -> TranscriptLine {
    TranscriptLine {
        role: transcript_role(message),
        text: message
            .blocks
            .iter()
            .filter_map(content_block_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        author_label: transcript_author_label(message),
    }
}

fn content_block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text { text } => Some(text.clone()),
        ContentBlock::ToolCall { call } => {
            Some(format!("Tool call: {}\n{}", call.name, call.input_json))
        }
        ContentBlock::ToolResult { result } => Some(result.output_text.clone()),
        ContentBlock::Boundary { boundary } => Some(
            match boundary.kind {
                code_agent_core::BoundaryKind::Compact => "[compact boundary]",
                code_agent_core::BoundaryKind::MicroCompact => "[micro-compact boundary]",
                code_agent_core::BoundaryKind::SessionMemory => "[session-memory boundary]",
                code_agent_core::BoundaryKind::Resume => "[resume boundary]",
            }
            .to_owned(),
        ),
        ContentBlock::Attachment { attachment } => Some(attachment.name.clone()),
    }
}

fn summarize_transcript(messages: &[Message], transcript_lines: &[TranscriptLine]) -> PanePreview {
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .count();
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .count();
    let last_user = transcript_lines
        .iter()
        .rev()
        .find(|line| line.role == "user")
        .map(|line| clip_line(&line.text, 88))
        .unwrap_or_else(|| "none".to_owned());
    let last_assistant = transcript_lines
        .iter()
        .rev()
        .find(|line| line.role == "assistant")
        .map(|line| clip_line(&line.text, 88))
        .unwrap_or_else(|| "none".to_owned());

    PanePreview {
        title: "Session".to_owned(),
        lines: vec![
            format!("messages: {}", messages.len()),
            format!("assistant turns: {}", assistant_messages),
            format!("tool results: {}", tool_messages),
            format!("last user: {last_user}"),
            format!("last assistant: {last_assistant}"),
        ],
    }
}

fn clip_line(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }

    let mut clipped = trimmed
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    clipped.push_str("...");
    clipped
}

pub trait UiRenderer {
    fn draw(&mut self, state: &UiState) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct RatatuiApp {
    pub title: String,
}

impl RatatuiApp {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
        }
    }

    pub fn initial_state(&self) -> UiState {
        UiState {
            status_line: self.title.clone(),
            active_pane: Some(PaneKind::Transcript),
            ..UiState::default()
        }
    }

    pub fn state_from_messages(
        &self,
        messages: Vec<Message>,
        commands: &[&CommandSpec],
    ) -> UiState {
        let mut state = UiState::from_messages(messages);
        state.status_line = self.title.clone();
        state.load_command_palette(commands);
        state
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMode {
    TooSmall,
    Compact,
    Standard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayKind {
    ChoiceList,
    Pane(PaneKind),
}

fn pane_preview(state: &UiState, pane: PaneKind) -> PanePreview {
    match pane {
        PaneKind::Transcript => state.transcript_preview.clone(),
        PaneKind::Diff => {
            if state.diff_preview.title.is_empty() && state.diff_preview.lines.is_empty() {
                PanePreview {
                    title: "Diff preview".to_owned(),
                    lines: vec!["No diff preview available yet.".to_owned()],
                }
            } else {
                state.diff_preview.clone()
            }
        }
        PaneKind::FileViewer => {
            if state.file_preview.title.is_empty() && state.file_preview.lines.is_empty() {
                PanePreview {
                    title: "File preview".to_owned(),
                    lines: vec!["No file preview available yet.".to_owned()],
                }
            } else {
                state.file_preview.clone()
            }
        }
        PaneKind::Tasks => {
            if state.task_preview.title.is_empty() && state.task_preview.lines.is_empty() {
                PanePreview {
                    title: "Tasks".to_owned(),
                    lines: vec!["No task activity yet.".to_owned()],
                }
            } else {
                state.task_preview.clone()
            }
        }
        PaneKind::Permissions => match &state.permission_prompt {
            Some(prompt) => PanePreview {
                title: format!("Permission: {}", prompt.tool_name),
                lines: vec![
                    prompt.summary.clone(),
                    format!("allow: {}", prompt.allow_once_label),
                    format!("deny: {}", prompt.deny_label),
                ],
            },
            None => PanePreview {
                title: "Permissions".to_owned(),
                lines: vec!["No pending permission prompts.".to_owned()],
            },
        },
        PaneKind::Logs => {
            if state.log_preview.title.is_empty() && state.log_preview.lines.is_empty() {
                PanePreview {
                    title: "Logs".to_owned(),
                    lines: vec!["No runtime logs yet.".to_owned()],
                }
            } else {
                state.log_preview.clone()
            }
        }
    }
}

fn layout_mode(area: Rect, state: &UiState) -> LayoutMode {
    let min_height = if state.show_input {
        MIN_REPL_HEIGHT
    } else {
        MIN_HEIGHT
    };
    let compact_height = if state.show_input {
        COMPACT_REPL_HEIGHT
    } else {
        COMPACT_HEIGHT
    };

    if area.width < MIN_WIDTH || area.height < min_height {
        LayoutMode::TooSmall
    } else if area.width < COMPACT_WIDTH || area.height < compact_height {
        LayoutMode::Compact
    } else {
        LayoutMode::Standard
    }
}

fn overlay_kind(state: &UiState) -> Option<OverlayKind> {
    if state.choice_list.is_some() {
        return Some(OverlayKind::ChoiceList);
    }

    if state.permission_prompt.is_some() {
        return Some(OverlayKind::Pane(PaneKind::Permissions));
    }

    match state.active_pane_or_default() {
        PaneKind::Transcript => None,
        pane => Some(OverlayKind::Pane(pane)),
    }
}

fn pane_shortcut_label_for_terminal(term_program: Option<&str>) -> &'static str {
    if cfg!(target_os = "macos") {
        if term_program == Some("vscode") {
            "Ctrl/Alt+1-6"
        } else if term_program == Some("Apple_Terminal") {
            "Alt+1-6"
        } else {
            "Cmd/Ctrl/Alt+1-6"
        }
    } else {
        "Ctrl/Alt+1-6"
    }
}

fn pane_shortcut_label() -> &'static str {
    pane_shortcut_label_for_terminal(std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn pending_details_toggle_label(state: &UiState) -> Option<&'static str> {
    (state.pending_step_count > 0).then_some(if state.pending_transcript_details {
        "Ctrl+E hide details"
    } else {
        "Ctrl+E show details"
    })
}

fn history_group_toggle_label(state: &UiState) -> Option<&'static str> {
    let mut has_history_groups = false;
    let mut all_expanded = true;

    for item in resolved_transcript_items(state) {
        let TranscriptItem::Group(group) = item else {
            continue;
        };
        if !group.single_item {
            continue;
        }
        has_history_groups = true;
        all_expanded &= group.expanded;
    }

    has_history_groups.then_some(if all_expanded {
        "Ctrl+E collapse history"
    } else {
        "Ctrl+E expand history"
    })
}

fn transcript_details_toggle_label(state: &UiState) -> Option<&'static str> {
    pending_details_toggle_label(state).or_else(|| history_group_toggle_label(state))
}

fn truncate_middle(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let left_len = (max_chars - 3) / 2;
    let right_len = max_chars - 3 - left_len;
    let left = text.chars().take(left_len).collect::<String>();
    let right = text
        .chars()
        .rev()
        .take(right_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{left}...{right}")
}

fn push_chunked_line(wrapped: &mut Vec<String>, text: &str, width: usize) {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        wrapped.push(String::new());
        return;
    }

    for chunk in chars.chunks(width.max(1)) {
        wrapped.push(chunk.iter().collect::<String>());
    }
}

fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut wrapped = Vec::new();

    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            wrapped.push(String::new());
            continue;
        }

        let preserve_spacing = raw_line.starts_with(char::is_whitespace)
            || raw_line.contains("  ")
            || raw_line.contains('\t');
        if preserve_spacing {
            push_chunked_line(&mut wrapped, raw_line, width);
            continue;
        }

        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            let word_width = line_width(word);
            if current.is_empty() {
                if word_width > width {
                    push_chunked_line(&mut wrapped, word, width);
                } else {
                    current.push_str(word);
                }
                continue;
            }

            let next_width = line_width(&current).saturating_add(1 + word_width);
            if next_width <= width {
                current.push(' ');
                current.push_str(word);
                continue;
            }

            wrapped.push(current);
            current = String::new();
            if word_width > width {
                push_chunked_line(&mut wrapped, word, width);
            } else {
                current.push_str(word);
            }
        }

        if !current.is_empty() {
            wrapped.push(current);
        }
    }

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }

    wrapped
}

fn line_width(text: &str) -> usize {
    text.chars().count()
}

fn role_style(role: &str) -> Style {
    match role {
        "user" => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        "command" => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        "assistant" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "command_output" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "tool" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "task" => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        "setup" => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        "user" => "You",
        "command" => "You",
        "assistant" => "Assistant",
        "command_output" => "Command",
        "tool" => "Tool",
        "task" => "Task",
        "setup" => "Setup",
        _ => "Info",
    }
}

fn transcript_author_label(message: &Message) -> Option<String> {
    if let Some(author) = message.metadata.attributes.get(UI_AUTHOR_ATTRIBUTE) {
        return Some(author.clone());
    }

    match message.role {
        MessageRole::Assistant => Some(assistant_author_label(&message.metadata)),
        _ => None,
    }
}

fn assistant_author_label(metadata: &MessageMetadata) -> String {
    let model = metadata
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let channel = metadata
        .attributes
        .get("channel")
        .map(String::as_str)
        .or(metadata.provider.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (model, channel) {
        (Some(model), Some(channel)) => format!("{model}({channel})"),
        (Some(model), None) => model.to_owned(),
        (None, Some(channel)) => format!("Assistant({channel})"),
        (None, None) => "Assistant".to_owned(),
    }
}

fn append_wrapped_transcript_line(
    lines: &mut Vec<Line<'static>>,
    transcript_line: &TranscriptLine,
    width: u16,
) {
    let width = width.max(1) as usize;
    let label = transcript_line
        .author_label
        .as_deref()
        .unwrap_or(role_label(&transcript_line.role));
    let label_prefix = format!("{label}  ");
    let label_style = role_style(&transcript_line.role);

    if transcript_line.text.trim().is_empty() {
        lines.push(Line::from(Span::styled(label_prefix, label_style)));
        return;
    }

    let inline_label = line_width(&label_prefix) + 6 < width;
    if inline_label {
        let continuation_prefix = " ".repeat(line_width(&label_prefix));
        let mut wrapped = wrap_plain_text(
            &transcript_line.text,
            width.saturating_sub(line_width(&label_prefix)).max(1),
        )
        .into_iter();

        if let Some(first) = wrapped.next() {
            lines.push(Line::from(vec![
                Span::styled(label_prefix.clone(), label_style),
                Span::raw(first),
            ]));
        }

        for segment in wrapped {
            lines.push(Line::from(vec![
                Span::raw(continuation_prefix.clone()),
                Span::raw(segment),
            ]));
        }
        return;
    }

    lines.push(Line::from(Span::styled(label.to_owned(), label_style)));
    let continuation_prefix = "  ".to_owned();
    for segment in wrap_plain_text(
        &transcript_line.text,
        width
            .saturating_sub(line_width(&continuation_prefix))
            .max(1),
    ) {
        lines.push(Line::from(vec![
            Span::raw(continuation_prefix.clone()),
            Span::raw(segment),
        ]));
    }
}

#[derive(Clone, Debug)]
enum TranscriptRenderLineKind {
    Regular,
    GroupHeader(String),
}

#[derive(Clone, Debug)]
struct TranscriptRenderLine {
    line: Line<'static>,
    kind: TranscriptRenderLineKind,
    item_index: Option<usize>,
    plain_text: String,
}

fn regular_render_line(
    line: Line<'static>,
    item_index: Option<usize>,
    plain_text: impl Into<String>,
) -> TranscriptRenderLine {
    TranscriptRenderLine {
        line,
        kind: TranscriptRenderLineKind::Regular,
        item_index,
        plain_text: plain_text.into(),
    }
}

fn group_header_render_line(
    id: &str,
    line: Line<'static>,
    item_index: Option<usize>,
    plain_text: impl Into<String>,
) -> TranscriptRenderLine {
    TranscriptRenderLine {
        line,
        kind: TranscriptRenderLineKind::GroupHeader(id.to_owned()),
        item_index,
        plain_text: plain_text.into(),
    }
}

fn styled_line(line: Line<'static>, style: Style) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content.to_string(), span.style.patch(style)))
            .collect::<Vec<_>>(),
    )
}

fn slice_chars(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<String>()
}

fn push_span(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
}

fn highlight_line_range(
    line: Line<'static>,
    start: usize,
    end: usize,
    style: Style,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut offset = 0usize;

    for span in line.spans {
        let content = span.content.to_string();
        let span_len = content.chars().count();
        let span_start = offset;
        let span_end = offset + span_len;

        if end <= span_start || start >= span_end {
            push_span(&mut spans, content, span.style);
            offset = span_end;
            continue;
        }

        let local_start = start.saturating_sub(span_start).min(span_len);
        let local_end = end.saturating_sub(span_start).min(span_len);
        push_span(
            &mut spans,
            slice_chars(&content, 0, local_start),
            span.style,
        );
        push_span(
            &mut spans,
            slice_chars(&content, local_start, local_end),
            span.style.patch(style),
        );
        push_span(
            &mut spans,
            slice_chars(&content, local_end, span_len),
            span.style,
        );
        offset = span_end;
    }

    Line::from(spans)
}

fn search_highlight_style() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

fn message_action_highlight_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn selection_highlight_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::Yellow)
}

fn normalize_selection(
    selection: &TranscriptSelectionState,
) -> Option<(TranscriptSelectionPoint, TranscriptSelectionPoint)> {
    if selection.anchor == selection.focus {
        return None;
    }

    if selection.anchor.line_index < selection.focus.line_index
        || (selection.anchor.line_index == selection.focus.line_index
            && selection.anchor.column <= selection.focus.column)
    {
        Some((selection.anchor.clone(), selection.focus.clone()))
    } else {
        Some((selection.focus.clone(), selection.anchor.clone()))
    }
}

fn selection_range_for_line(
    selection: &TranscriptSelectionState,
    line_index: usize,
    line_len: usize,
) -> Option<(usize, usize)> {
    let (start, end) = normalize_selection(selection)?;
    if line_index < start.line_index || line_index > end.line_index {
        return None;
    }

    let range_start = if line_index == start.line_index {
        start.column.min(line_len)
    } else {
        0
    };
    let range_end = if line_index == end.line_index {
        end.column.min(line_len)
    } else {
        line_len
    };

    (range_start < range_end).then_some((range_start, range_end))
}

fn prompt_selection_range(
    selection: &PromptSelectionState,
    input_len: usize,
) -> Option<(usize, usize)> {
    let start = selection.anchor.min(selection.focus).min(input_len);
    let end = selection.anchor.max(selection.focus).min(input_len);
    (start < end).then_some((start, end))
}

fn highlight_transcript_render_line(
    state: &UiState,
    render_line: &TranscriptRenderLine,
    visual_index: usize,
) -> Line<'static> {
    let mut line = render_line.line.clone();

    if render_line
        .item_index
        .zip(
            state
                .transcript_search
                .as_ref()
                .and_then(|search| search.active_item),
        )
        .is_some_and(|(item_index, active_item)| item_index == active_item)
    {
        line = styled_line(line, search_highlight_style());
    }

    if render_line
        .item_index
        .zip(
            state
                .message_actions
                .as_ref()
                .map(|actions| actions.active_item),
        )
        .is_some_and(|(item_index, active_item)| item_index == active_item)
    {
        line = styled_line(line, message_action_highlight_style());
    }

    if let Some(selection) = state.transcript_selection.as_ref() {
        let line_len = render_line.plain_text.chars().count();
        if let Some((start, end)) = selection_range_for_line(selection, visual_index, line_len) {
            line = highlight_line_range(line, start, end, selection_highlight_style());
        }
    }

    line
}

fn indent_line(line: Line<'static>, indent: &str) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(indent.to_owned()));
    spans.extend(line.spans);
    Line::from(spans)
}

fn wrapped_transcript_lines(transcript_line: &TranscriptLine, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    append_wrapped_transcript_line(&mut lines, transcript_line, width);
    lines
}

fn resolved_transcript_items(state: &UiState) -> Vec<TranscriptItem> {
    if !state.transcript_items.is_empty() {
        return state.transcript_items.clone();
    }

    let mut items = state
        .transcript_lines
        .iter()
        .cloned()
        .map(TranscriptItem::Line)
        .collect::<Vec<_>>();
    items.extend(
        state
            .transcript_groups
            .iter()
            .cloned()
            .map(TranscriptItem::Group),
    );
    items
}

fn group_header_lines(group: &TranscriptGroup, width: u16) -> Vec<Line<'static>> {
    if group.single_item {
        return single_item_group_header_lines(group, width);
    }

    let mut lines = Vec::new();
    let icon = if group.expanded { "▼" } else { "▶" };
    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let subtitle_style = Style::default().fg(Color::DarkGray);

    for segment in wrap_plain_text(&format!("{icon} {}", group.title), width.max(1) as usize) {
        lines.push(Line::from(Span::styled(segment, title_style)));
    }

    let subtitle = group.subtitle.as_deref().map(|value| {
        format!(
            "{value} · click to {}",
            if group.expanded { "collapse" } else { "expand" }
        )
    });
    if let Some(subtitle) = subtitle {
        for segment in wrap_plain_text(&subtitle, width.saturating_sub(2).max(1) as usize) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(segment, subtitle_style),
            ]));
        }
    }

    lines
}

fn single_item_group_header_lines(group: &TranscriptGroup, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let icon_style = if group.expanded {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let summary_style = Style::default().add_modifier(Modifier::DIM);
    let hint_style = Style::default().fg(Color::DarkGray);

    let content_width = width.saturating_sub(2).max(1) as usize;
    for (index, segment) in wrap_plain_text(&group.title, content_width)
        .into_iter()
        .enumerate()
    {
        lines.push(if index == 0 {
            Line::from(vec![
                Span::styled(if group.expanded { "▼" } else { "▶" }, icon_style),
                Span::raw(" "),
                Span::styled(segment, summary_style),
            ])
        } else {
            Line::from(vec![Span::raw("  "), Span::styled(segment, summary_style)])
        });
    }

    if !group.expanded {
        if let Some(subtitle) = group
            .subtitle
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            for (index, segment) in
                wrap_plain_text(subtitle, width.saturating_sub(5).max(1) as usize)
                    .into_iter()
                    .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(if index == 0 { "  ⎿ " } else { "    " }, hint_style),
                    Span::styled(segment, hint_style),
                ]));
            }
        }
    }

    lines
}

fn single_item_group_detail_style(role: &str) -> Style {
    match role {
        "history_tool_call" => Style::default().add_modifier(Modifier::BOLD),
        "history_tool_error" => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn single_item_group_detail_lines(group: &TranscriptGroup, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for transcript_line in &group.lines {
        if transcript_line.text.trim().is_empty() {
            continue;
        }

        if transcript_line.role == "history_tool_call" && !lines.is_empty() {
            lines.push(Line::from(""));
        }

        let (first_prefix, continuation_prefix, content_width) = match transcript_line.role.as_str()
        {
            "history_tool_call" => ("  ", "  ", width.saturating_sub(2).max(1) as usize),
            _ => ("  ⎿ ", "    ", width.saturating_sub(5).max(1) as usize),
        };

        let style = single_item_group_detail_style(&transcript_line.role);
        for (index, segment) in wrap_plain_text(&transcript_line.text, content_width)
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::raw(if index == 0 {
                    first_prefix.to_owned()
                } else {
                    continuation_prefix.to_owned()
                }),
                Span::styled(segment, style),
            ]));
        }
    }

    lines
}

fn empty_transcript_render_lines(state: &UiState, width: u16) -> Vec<TranscriptRenderLine> {
    if !state.input_buffer.is_empty()
        || !state.queued_inputs.is_empty()
        || state.progress_message.is_some()
    {
        return vec![regular_render_line(
            Line::from(Span::styled(
                "Transcript",
                Style::default().fg(Color::DarkGray),
            )),
            None,
            "Transcript",
        )];
    }

    let mut lines = vec![
        regular_render_line(
            Line::from(Span::styled(
                "Start a conversation",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            None,
            "Start a conversation",
        ),
        regular_render_line(
            Line::from(Span::styled(
                "Type a prompt below or start with / to browse commands.",
                Style::default().fg(Color::DarkGray),
            )),
            None,
            "Type a prompt below or start with / to browse commands.",
        ),
    ];
    if !state.command_palette.is_empty() {
        lines.push(regular_render_line(Line::from(""), None, ""));
        for entry in state.command_palette.iter().take(4) {
            let combined = format!("{}  {}", entry.name, entry.description);
            for segment in wrap_plain_text(&combined, width.max(1) as usize) {
                lines.push(regular_render_line(
                    Line::from(segment.clone()),
                    None,
                    segment,
                ));
            }
        }
    }
    lines
}

fn transcript_visual_lines(state: &UiState, width: u16) -> Vec<TranscriptRenderLine> {
    let items = resolved_transcript_items(state);
    if items.is_empty() {
        return empty_transcript_render_lines(state, width);
    }

    let mut lines = Vec::new();
    let mut item_index = 0usize;

    for (index, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::Line(transcript_line) => {
                for line in wrapped_transcript_lines(transcript_line, width) {
                    let plain_text = line_text(&line);
                    lines.push(regular_render_line(line, Some(item_index), plain_text));
                }
                item_index += 1;
            }
            TranscriptItem::Group(group) => {
                let group_item_index = item_index;
                for line in group_header_lines(group, width) {
                    let plain_text = line_text(&line);
                    lines.push(group_header_render_line(
                        &group.id,
                        line,
                        Some(group_item_index),
                        plain_text,
                    ));
                }
                item_index += 1;

                if group.expanded && !group.lines.is_empty() {
                    if group.single_item {
                        for line in single_item_group_detail_lines(group, width) {
                            let plain_text = line_text(&line);
                            lines.push(regular_render_line(
                                line,
                                Some(group_item_index),
                                plain_text,
                            ));
                        }
                    } else {
                        lines.push(regular_render_line(Line::from(""), None, ""));
                        for (line_index, transcript_line) in group.lines.iter().enumerate() {
                            for line in
                                wrapped_transcript_lines(transcript_line, width.saturating_sub(2))
                            {
                                let line = indent_line(line, "  ");
                                let plain_text = line_text(&line);
                                lines.push(regular_render_line(line, Some(item_index), plain_text));
                            }
                            item_index += 1;
                            if line_index + 1 < group.lines.len() {
                                lines.push(regular_render_line(Line::from(""), None, ""));
                            }
                        }
                    }
                }
            }
        }

        if index + 1 < items.len() {
            lines.push(regular_render_line(Line::from(""), None, ""));
        }
    }

    for (visual_index, render_line) in lines.iter_mut().enumerate() {
        render_line.line = highlight_transcript_render_line(state, render_line, visual_index);
    }

    lines
}

fn transcript_searchable_items(state: &UiState) -> Vec<(usize, String)> {
    let mut items = Vec::new();
    let mut item_index = 0usize;

    for item in resolved_transcript_items(state) {
        match item {
            TranscriptItem::Line(transcript_line) => {
                items.push((item_index, transcript_line.text));
                item_index += 1;
            }
            TranscriptItem::Group(group) => {
                let mut header_text = group.title.clone();
                if let Some(subtitle) = group
                    .subtitle
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    header_text.push('\n');
                    header_text.push_str(subtitle);
                }

                if group.single_item {
                    for transcript_line in &group.lines {
                        if !transcript_line.text.trim().is_empty() {
                            header_text.push('\n');
                            header_text.push_str(&transcript_line.text);
                        }
                    }
                    items.push((item_index, header_text));
                    item_index += 1;
                } else {
                    items.push((item_index, header_text));
                    item_index += 1;

                    if group.expanded {
                        for transcript_line in &group.lines {
                            items.push((item_index, transcript_line.text.clone()));
                            item_index += 1;
                        }
                    }
                }
            }
        }
    }

    items
}

pub fn transcript_search_match_items(state: &UiState, query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let needle = query.to_lowercase();
    transcript_searchable_items(state)
        .into_iter()
        .filter_map(|(item_index, text)| {
            text.to_lowercase().contains(&needle).then_some(item_index)
        })
        .collect()
}

fn footer_height_for_state(state: &UiState) -> u16 {
    if state.show_input || state.transcript_mode {
        2
    } else {
        1
    }
}

fn transcript_body_area_for_view(state: &UiState, width: u16, height: u16) -> Option<Rect> {
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

    Some(vertical[1])
}

fn scroll_for_visual_line(
    total_lines: usize,
    viewport_height: u16,
    visual_line: usize,
) -> Option<u16> {
    if total_lines == 0 || viewport_height == 0 {
        return None;
    }

    let max_scroll = total_lines.saturating_sub(viewport_height as usize) as u16;
    let desired = total_lines.saturating_sub(viewport_height as usize + visual_line) as u16;
    Some(desired.min(max_scroll))
}

pub fn transcript_search_scroll_for_view(
    state: &UiState,
    width: u16,
    height: u16,
    item_index: usize,
) -> Option<u16> {
    let body_area = transcript_body_area_for_view(state, width, height)?;
    let lines = transcript_visual_lines(state, body_area.width);
    let visual_line = lines
        .iter()
        .position(|line| line.item_index == Some(item_index))?;
    let no_sticky = scroll_for_visual_line(lines.len(), body_area.height, visual_line)?;
    if no_sticky == 0 {
        return Some(0);
    }

    scroll_for_visual_line(
        lines.len(),
        body_area.height.saturating_sub(1).max(1),
        visual_line,
    )
}

pub fn transcript_visual_scroll_for_view(
    state: &UiState,
    width: u16,
    height: u16,
    visual_line: usize,
) -> Option<u16> {
    let body_area = transcript_body_area_for_view(state, width, height)?;
    let lines = transcript_visual_lines(state, body_area.width);
    let no_sticky = scroll_for_visual_line(lines.len(), body_area.height, visual_line)?;
    if no_sticky == 0 {
        return Some(0);
    }

    scroll_for_visual_line(
        lines.len(),
        body_area.height.saturating_sub(1).max(1),
        visual_line,
    )
}

pub fn transcript_selectable_lines_for_view(
    state: &UiState,
    width: u16,
) -> Vec<TranscriptSelectableLine> {
    transcript_visual_lines(state, width)
        .into_iter()
        .enumerate()
        .filter_map(|(line_index, line)| match line.kind {
            TranscriptRenderLineKind::Regular => {
                line.item_index.map(|item_index| TranscriptSelectableLine {
                    item_index,
                    line_index,
                    text: line.plain_text,
                })
            }
            TranscriptRenderLineKind::GroupHeader(_) => None,
        })
        .collect()
}

pub fn transcript_selection_text_for_view(
    state: &UiState,
    width: u16,
    selection: &TranscriptSelectionState,
) -> Option<String> {
    let selectable_lines = transcript_selectable_lines_for_view(state, width);
    let (start, end) = normalize_selection(selection)?;
    let mut segments = Vec::new();

    for line in selectable_lines {
        if line.line_index < start.line_index || line.line_index > end.line_index {
            continue;
        }

        let line_len = line.text.chars().count();
        let slice_start = if line.line_index == start.line_index {
            start.column.min(line_len)
        } else {
            0
        };
        let slice_end = if line.line_index == end.line_index {
            end.column.min(line_len)
        } else {
            line_len
        };
        if slice_start >= slice_end {
            continue;
        }

        segments.push(slice_chars(&line.text, slice_start, slice_end));
    }

    (!segments.is_empty()).then(|| segments.join("\n"))
}

fn clamped_transcript_scroll(
    total_lines: usize,
    viewport_height: u16,
    requested_scroll: u16,
) -> u16 {
    let max_scroll = total_lines.saturating_sub(viewport_height as usize) as u16;
    requested_scroll.min(max_scroll)
}

fn transcript_viewport_from_lines(
    all_lines: &[TranscriptRenderLine],
    height: u16,
    requested_scroll: u16,
) -> (Vec<TranscriptRenderLine>, u16) {
    if height == 0 {
        return (Vec::new(), 0);
    }

    let scroll = clamped_transcript_scroll(all_lines.len(), height, requested_scroll);
    let start = all_lines
        .len()
        .saturating_sub(height as usize + scroll as usize);
    let end = (start + height as usize).min(all_lines.len());
    (all_lines[start..end].to_vec(), scroll)
}

fn last_user_prompt_excerpt(state: &UiState, width: u16, transcript_scroll: u16) -> Option<String> {
    if transcript_scroll == 0 {
        return None;
    }

    last_user_prompt_text(state)
        .map(|text| truncate_middle(text, width.saturating_sub(4) as usize))
}

fn last_user_prompt_text(state: &UiState) -> Option<&str> {
    if !state.transcript_items.is_empty() {
        return state
            .transcript_items
            .iter()
            .rev()
            .find_map(last_user_prompt_text_for_item);
    }

    state
        .transcript_groups
        .iter()
        .rev()
        .find_map(last_user_prompt_text_for_group)
        .or_else(|| {
            state
                .transcript_lines
                .iter()
                .rev()
                .find(|line| line.role == "user")
                .map(|line| line.text.as_str())
        })
}

fn last_user_prompt_text_for_item(item: &TranscriptItem) -> Option<&str> {
    match item {
        TranscriptItem::Line(line) if line.role == "user" => Some(line.text.as_str()),
        TranscriptItem::Line(_) => None,
        TranscriptItem::Group(group) => last_user_prompt_text_for_group(group),
    }
}

fn last_user_prompt_text_for_group(group: &TranscriptGroup) -> Option<&str> {
    group.lines
        .iter()
        .rev()
        .find(|line| line.role == "user")
        .map(|line| line.text.as_str())
}

fn sticky_prompt_widget(
    state: &UiState,
    width: u16,
    transcript_scroll: u16,
) -> Option<Paragraph<'static>> {
    let text = last_user_prompt_excerpt(state, width, transcript_scroll)?;
    Some(
        Paragraph::new(Line::from(vec![
            Span::styled("▸ ", Style::default().fg(Color::DarkGray)),
            Span::styled(text, Style::default().fg(Color::White)),
        ]))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(Color::DarkGray)),
    )
}

fn scroll_pill_widget(transcript_scroll: u16) -> Option<Paragraph<'static>> {
    if transcript_scroll == 0 {
        return None;
    }

    let label = if transcript_scroll == 1 {
        " Jump to bottom ".to_owned()
    } else {
        format!(" Jump to bottom · {} lines up ", transcript_scroll)
    };

    Some(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
    )
}
