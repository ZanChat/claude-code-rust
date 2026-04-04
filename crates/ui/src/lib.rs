use anyhow::Result;
use code_agent_core::{
    CommandSpec, ContentBlock, Message, MessageMetadata, MessageRole, TaskStatus,
};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;

pub mod vim;

const MIN_WIDTH: u16 = 48;
const MIN_HEIGHT: u16 = 10;
const MIN_REPL_HEIGHT: u16 = 15;
const COMPACT_WIDTH: u16 = 92;
const COMPACT_HEIGHT: u16 = 20;
const COMPACT_REPL_HEIGHT: u16 = 24;
const STANDARD_INPUT_HEIGHT: u16 = 6;
const COMPACT_INPUT_HEIGHT: u16 = 6;
const MAX_VISIBLE_SUGGESTIONS: usize = 4;
const MODAL_TRANSCRIPT_PEEK: u16 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputBuffer {
    pub chars: Vec<char>,
    pub cursor: usize,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn as_str(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn push(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn pop(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn replace(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

impl StatusLevel {
    fn style(self) -> Style {
        match self {
            Self::Info => Style::default().fg(Color::Cyan),
            Self::Warning => Style::default().fg(Color::Yellow),
            Self::Error => Style::default().fg(Color::Red),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneKind {
    Transcript,
    Diff,
    FileViewer,
    Tasks,
    Permissions,
    Logs,
}

impl PaneKind {
    pub const ALL: [Self; 6] = [
        Self::Transcript,
        Self::Diff,
        Self::FileViewer,
        Self::Tasks,
        Self::Permissions,
        Self::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Transcript => "Transcript",
            Self::Diff => "Diff",
            Self::FileViewer => "File",
            Self::Tasks => "Tasks",
            Self::Permissions => "Permissions",
            Self::Logs => "Logs",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub level: Option<StatusLevel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptLine {
    pub role: String,
    pub text: String,
    pub author_label: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionPromptState {
    pub tool_name: String,
    pub summary: String,
    pub allow_once_label: String,
    pub deny_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteEntry {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct PanePreview {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskUiEntry {
    pub title: String,
    pub kind: String,
    pub status: TaskStatus,
    pub input: Option<String>,
    pub output: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionUiEntry {
    pub prompt: String,
    pub choices: Vec<String>,
    pub task_title: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub messages: Vec<Message>,
    pub transcript_lines: Vec<TranscriptLine>,
    pub header_title: Option<String>,
    pub header_subtitle: Option<String>,
    pub header_context: Option<String>,
    pub transcript_scroll: u16,
    pub status_line: String,
    pub status_marquee_tick: usize,
    pub input_buffer: InputBuffer,
    pub prompt_helper: Option<String>,
    pub show_input: bool,
    pub command_palette: Vec<CommandPaletteEntry>,
    pub command_suggestions: Vec<CommandPaletteEntry>,
    pub selected_command_suggestion: Option<usize>,
    pub active_pane: Option<PaneKind>,
    pub notifications: VecDeque<Notification>,
    pub permission_prompt: Option<PermissionPromptState>,
    pub progress_message: Option<String>,
    pub task_items: Vec<TaskUiEntry>,
    pub question_items: Vec<QuestionUiEntry>,
    pub vim_state: vim::VimState,
    pub compact_banner: Option<String>,
    pub transcript_preview: PanePreview,
    pub task_preview: PanePreview,
    pub diff_preview: PanePreview,
    pub file_preview: PanePreview,
    pub log_preview: PanePreview,
}

impl UiState {
    pub fn from_messages(messages: Vec<Message>) -> Self {
        let transcript_lines = messages
            .iter()
            .map(|message| TranscriptLine {
                role: format!("{:?}", message.role).to_lowercase(),
                text: message
                    .blocks
                    .iter()
                    .filter_map(content_block_text)
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                author_label: transcript_author_label(message),
            })
            .collect::<Vec<_>>();

        let mut state = Self {
            messages,
            transcript_lines,
            active_pane: Some(PaneKind::Transcript),
            ..Self::default()
        };
        state.transcript_preview = summarize_transcript(&state.messages, &state.transcript_lines);
        state
    }

    pub fn load_command_palette(&mut self, commands: &[&CommandSpec]) {
        self.command_palette = commands
            .iter()
            .map(|command| CommandPaletteEntry {
                name: format!("/{}", command.name),
                description: command.description.clone(),
            })
            .collect();
    }

    pub fn push_notification(&mut self, notification: Notification) {
        self.notifications.push_back(notification);
    }

    pub fn active_pane_or_default(&self) -> PaneKind {
        self.active_pane.unwrap_or(PaneKind::Transcript)
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

fn overlay_kind(state: &UiState) -> Option<PaneKind> {
    if state.permission_prompt.is_some() {
        return Some(PaneKind::Permissions);
    }

    match state.active_pane_or_default() {
        PaneKind::Transcript => None,
        pane => Some(pane),
    }
}

fn pane_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+1-6"
    } else {
        "Ctrl+1-6"
    }
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
        "assistant" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "tool" => Style::default()
            .fg(Color::Yellow)
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
        "assistant" => "Assistant",
        "tool" => "Tool",
        "setup" => "Setup",
        _ => "Info",
    }
}

fn transcript_author_label(message: &Message) -> Option<String> {
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

fn transcript_visual_lines(state: &UiState, width: u16) -> Vec<Line<'static>> {
    if state.transcript_lines.is_empty() {
        let mut lines = vec![
            Line::from(Span::styled(
                "Start a conversation",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Type a prompt below or start with / to browse commands.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        if !state.command_palette.is_empty() {
            lines.push(Line::from(""));
            for entry in state.command_palette.iter().take(4) {
                let combined = format!("{}  {}", entry.name, entry.description);
                for segment in wrap_plain_text(&combined, width.max(1) as usize) {
                    lines.push(Line::from(segment));
                }
            }
        }
        return lines;
    }

    let mut lines = Vec::new();
    for (index, transcript_line) in state.transcript_lines.iter().enumerate() {
        append_wrapped_transcript_line(&mut lines, transcript_line, width);
        if index + 1 < state.transcript_lines.len() {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn clamped_transcript_scroll(
    total_lines: usize,
    viewport_height: u16,
    requested_scroll: u16,
) -> u16 {
    let max_scroll = total_lines.saturating_sub(viewport_height as usize) as u16;
    requested_scroll.min(max_scroll)
}

fn transcript_viewport(state: &UiState, width: u16, height: u16) -> (Vec<Line<'static>>, u16) {
    let all_lines = transcript_visual_lines(state, width);
    if height == 0 {
        return (Vec::new(), 0);
    }

    let scroll = clamped_transcript_scroll(all_lines.len(), height, state.transcript_scroll);
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

    state
        .transcript_lines
        .iter()
        .rev()
        .find(|line| line.role == "user")
        .map(|line| truncate_middle(&line.text, width.saturating_sub(4) as usize))
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

fn task_status_style(status: &TaskStatus) -> (&'static str, Style) {
    match status {
        TaskStatus::Pending => ("PEND", Style::default().fg(Color::DarkGray)),
        TaskStatus::Running => (
            "RUN ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::WaitingForInput => (
            "WAIT",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Completed => (
            "DONE",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Failed => (
            "FAIL",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Cancelled => ("STOP", Style::default().fg(Color::Magenta)),
    }
}

fn indented_detail_lines(text: &str, indent: &str, style: Style) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(Span::styled(format!("{indent}{line}"), style)))
        .collect()
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
        let (badge, style) = task_status_style(&task.status);
        lines.push(Line::from(vec![
            Span::styled(format!("{badge} "), style),
            Span::raw(task.title.clone()),
            Span::styled(
                format!("  [{}]", task.kind),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        if detailed {
            if let Some(detail) = task.output.as_ref().or(task.input.as_ref()) {
                lines.extend(indented_detail_lines(
                    detail,
                    "  ",
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    if detailed {
        for question in state.question_items.iter().take(2) {
            lines.push(Line::from(vec![
                Span::styled(
                    "ASK  ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(question.prompt.clone()),
            ]));
            if !question.choices.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  choices: {}", question.choices.join(", ")),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines
}

fn permission_lines(state: &UiState) -> Vec<Line<'static>> {
    if let Some(prompt) = &state.permission_prompt {
        vec![
            Line::from(vec![
                Span::styled(
                    "ASK  ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(prompt.tool_name.clone()),
            ]),
            Line::from(prompt.summary.clone()),
            Line::from(Span::styled(
                format!("{} / {}", prompt.allow_once_label, prompt.deny_label),
                Style::default().fg(Color::DarkGray),
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
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(text.clone()),
        ]));
    }

    if let Some(progress) = &state.progress_message {
        lines.push(Line::from(vec![
            Span::styled(
                "live  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(progress.clone()),
        ]));
    }

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
        let (badge, style) = task_status_style(&task.status);
        lines.push(Line::from(vec![
            Span::styled(format!("{badge} "), style),
            Span::raw(task.title.clone()),
            Span::styled(
                format!("  [{}]", task.kind),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        if let Some(detail) = task.output.as_ref().or(task.input.as_ref()) {
            lines.extend(indented_detail_lines(
                detail,
                "  ",
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if let Some(question) = state.question_items.first() {
        lines.push(Line::from(vec![
            Span::styled(
                "ASK  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(question.prompt.clone()),
        ]));
    }

    if lines.is_empty() {
        if let Some(prompt) = &state.permission_prompt {
            lines.push(Line::from(vec![
                Span::styled(
                    "wait  ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
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

fn activity_widget(state: &UiState) -> Paragraph<'static> {
    Paragraph::new(activity_lines(state)).wrap(Wrap { trim: false })
}

fn push_wrapped_styled_lines(lines: &mut Vec<Line<'static>>, text: &str, width: u16, style: Style) {
    for segment in wrap_plain_text(text, width.max(1) as usize) {
        lines.push(Line::from(Span::styled(segment, style)));
    }
}

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

fn navigation_hint(active_pane: PaneKind, layout: LayoutMode, suggestions_visible: bool) -> String {
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

    if matches!(layout, LayoutMode::Compact) {
        format!(
            "{focus_label}  {} panes  {suggestion_hint}",
            pane_shortcut_label()
        )
    } else {
        format!(
            "{focus_label}  Tab cycle  {} panes  {suggestion_hint}  Ctrl-C exit",
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
    if state.vim_state.is_insert() {
        return "-- INSERT --".to_owned();
    }
    if suggestions_visible {
        return "Command suggestions".to_owned();
    }
    if state.permission_prompt.is_some() {
        return "Waiting for permission".to_owned();
    }
    if state.progress_message.is_some() {
        return "Working".to_owned();
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
    let secondary_text = line_text(&status_line(state));
    let primary = compose_footer_line(
        &footer_primary_text(state, suggestions_visible),
        &navigation_hint(active_pane, layout, suggestions_visible),
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
    let pos = state.input_buffer.cursor.min(text.chars().count());
    if pos < text.chars().count() {
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

fn overlay_content_lines(state: &UiState, kind: PaneKind) -> Vec<Line<'static>> {
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

fn render_body(frame: &mut Frame<'_>, state: &UiState, body_area: Rect) {
    if body_area.width == 0 || body_area.height == 0 {
        return;
    }

    let (_, initial_scroll) = transcript_viewport(state, body_area.width, body_area.height);
    let sticky_visible = initial_scroll > 0 && body_area.height > 1;
    let transcript_height = if sticky_visible {
        body_area.height.saturating_sub(1)
    } else {
        body_area.height
    };
    let (transcript_lines, effective_scroll) =
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

    if let Some(area) = header_area {
        if let Some(widget) = sticky_prompt_widget(state, area.width, effective_scroll) {
            frame.render_widget(widget, area);
        }
    }
    if transcript_area.width > 0 && transcript_area.height > 0 {
        frame.render_widget(Paragraph::new(transcript_lines), transcript_area);
        if let Some(pill) = scroll_pill_widget(effective_scroll) {
            let pill_area = Rect::new(
                transcript_area.x,
                transcript_area.y + transcript_area.height.saturating_sub(1),
                transcript_area.width,
                1,
            );
            frame.render_widget(pill, pill_area);
        }
    }
}

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
    let mut activity_height = if state.show_input && !overlay_visible && !suggestions_visible {
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
    let mut footer_height = if state.show_input { 2 } else { 1 };
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

#[cfg(test)]
mod tests {
    use super::{
        render_to_string, Notification, PaneKind, PermissionPromptState, RatatuiApp, StatusLevel,
    };
    use code_agent_core::{compatibility_command_registry, ContentBlock, Message, MessageRole};

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
}
