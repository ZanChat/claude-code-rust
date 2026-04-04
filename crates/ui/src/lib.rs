use anyhow::Result;
use code_agent_core::{CommandSpec, ContentBlock, Message, MessageRole, TaskStatus};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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

    fn number(self) -> usize {
        match self {
            Self::Transcript => 1,
            Self::Diff => 2,
            Self::FileViewer => 3,
            Self::Tasks => 4,
            Self::Permissions => 5,
            Self::Logs => 6,
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
    pub transcript_scroll: u16,
    pub status_line: String,
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
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        ContentBlock::ToolCall { call } => Some(call.name.as_str()),
                        ContentBlock::ToolResult { result } => Some(result.output_text.as_str()),
                        ContentBlock::Boundary { boundary } => Some(match boundary.kind {
                            code_agent_core::BoundaryKind::Compact => "[compact boundary]",
                            code_agent_core::BoundaryKind::MicroCompact => {
                                "[micro-compact boundary]"
                            }
                            code_agent_core::BoundaryKind::SessionMemory => {
                                "[session-memory boundary]"
                            }
                            code_agent_core::BoundaryKind::Resume => "[resume boundary]",
                        }),
                        ContentBlock::Attachment { attachment } => Some(attachment.name.as_str()),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
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

fn header_lines(state: &UiState, layout: LayoutMode, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let mode_label = if state.vim_state.is_insert() {
        "insert"
    } else {
        "vim"
    };
    let overlay_label =
        overlay_kind(state).map(|pane| format!("focus {}", pane.title().to_ascii_lowercase()));

    if matches!(layout, LayoutMode::Compact) {
        let overlay_width = overlay_label.as_ref().map_or(0, |label| label.len() + 2);
        let status_width = width.saturating_sub(mode_label.len() + overlay_width + 4);
        let mut status_line = vec![
            Span::raw(truncate_middle(&state.status_line, status_width)),
            Span::raw("  "),
        ];
        if let Some(label) = overlay_label {
            status_line.push(Span::styled(label, Style::default().fg(Color::Green)));
            status_line.push(Span::raw("  "));
        }
        status_line.push(Span::styled(
            mode_label.to_owned(),
            Style::default().fg(Color::Magenta),
        ));
        return vec![
            Line::from(Span::styled(
                "code-agent-rust",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(status_line),
        ];
    }

    let overlay_width = overlay_label.as_ref().map_or(0, |label| label.len() + 2);
    let reserved = "code-agent-rust".len() + mode_label.len() + overlay_width + 6;
    let mut line = vec![
        Span::styled(
            "code-agent-rust",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(truncate_middle(
            &state.status_line,
            width.saturating_sub(reserved),
        )),
        Span::raw("  "),
    ];
    if let Some(label) = overlay_label {
        line.push(Span::styled(label, Style::default().fg(Color::Green)));
        line.push(Span::raw("  "));
    }
    line.push(Span::styled(
        mode_label.to_owned(),
        Style::default().fg(Color::Magenta),
    ));
    vec![Line::from(line)]
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

fn transcript_title(state: &UiState) -> String {
    if state.transcript_lines.is_empty() {
        "Transcript".to_owned()
    } else if state.transcript_scroll > 0 {
        format!(
            "Transcript · {} entries · scroll {}",
            state.transcript_lines.len(),
            state.transcript_scroll
        )
    } else {
        format!("Transcript · {} entries", state.transcript_lines.len())
    }
}

fn transcript_widget(state: &UiState) -> Paragraph<'static> {
    let lines =
        if state.transcript_lines.is_empty() {
            let mut lines = vec![
                Line::from(Span::styled(
                    "No transcript messages yet.",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from("Type a prompt below or start with / to browse commands."),
            ];
            if !state.command_palette.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Suggested commands",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(
                    state.command_palette.iter().take(4).map(|entry| {
                        Line::from(format!("{:<12} {}", entry.name, entry.description))
                    }),
                );
            }
            lines
        } else {
            state
                .transcript_lines
                .iter()
                .map(|line| {
                    Line::from(vec![
                        Span::styled(
                            format!("{:>9} ", clip_line(&line.role.to_ascii_uppercase(), 9)),
                            role_style(&line.role),
                        ),
                        Span::raw(line.text.clone()),
                    ])
                })
                .collect::<Vec<_>>()
        };

    Paragraph::new(lines)
        .scroll((state.transcript_scroll, 0))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(transcript_title(state))
                .borders(Borders::ALL),
        )
}

fn preview_widget(preview: PanePreview) -> Paragraph<'static> {
    let preview_lines = if preview.lines.is_empty() {
        vec![Line::from("No details available.")]
    } else {
        preview
            .lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>()
    };

    Paragraph::new(preview_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().title(preview.title).borders(Borders::ALL))
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
                lines.push(Line::from(Span::styled(
                    format!("  {}", truncate_middle(detail, 72)),
                    Style::default().fg(Color::DarkGray),
                )));
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

fn tasks_widget(state: &UiState, title: &str, detailed: bool) -> Paragraph<'static> {
    Paragraph::new(task_lines(state, if detailed { 6 } else { 3 }, detailed))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(title.to_owned())
                .borders(Borders::ALL),
        )
}

fn permission_widget(state: &UiState, title: &str) -> Paragraph<'static> {
    let lines = if let Some(prompt) = &state.permission_prompt {
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
    };

    Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(title.to_owned())
            .borders(Borders::ALL),
    )
}

fn activity_lines(state: &UiState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(progress) = &state.progress_message {
        lines.push(Line::from(vec![
            Span::styled(
                "LIVE ",
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
        .take(3)
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
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate_middle(detail, 84)),
                Style::default().fg(Color::DarkGray),
            )));
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
                    "ASK  ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(prompt.summary.clone()),
            ]));
        }
    }

    lines
}

fn activity_widget(state: &UiState) -> Paragraph<'static> {
    let title = if state.progress_message.is_some() {
        "Live Turn"
    } else {
        "Activity"
    };
    Paragraph::new(activity_lines(state))
        .wrap(Wrap { trim: false })
        .block(Block::default().title(title).borders(Borders::ALL))
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
    let width = width.max(1) as usize;
    let mut count = 0u16;
    for line in text.split('\n') {
        let char_count = line.chars().count().max(1);
        count = count.saturating_add(((char_count - 1) / width + 1) as u16);
    }
    count.max(1)
}

fn prompt_content_height(
    state: &UiState,
    active_pane: PaneKind,
    layout: LayoutMode,
    width: u16,
) -> u16 {
    let suggestions_visible = !state.command_suggestions.is_empty();
    let helper = if let Some(helper) = state.prompt_helper.as_deref() {
        helper.to_owned()
    } else if suggestions_visible {
        "Up/Down selects a command. Enter completes the selection first.".to_owned()
    } else if state.input_buffer.is_empty() {
        "Type a prompt or start with / to browse commands.".to_owned()
    } else {
        "Enter submits the prompt.".to_owned()
    };

    let prompt_text = format!("> {}", state.input_buffer.as_str());
    let status = line_text(&status_line(state));
    let navigation = navigation_hint(active_pane, layout, suggestions_visible);

    wrapped_line_count(&prompt_text, width)
        .saturating_add(wrapped_line_count(&helper, width))
        .saturating_add(wrapped_line_count(&navigation, width))
        .saturating_add(wrapped_line_count(&status, width))
}

fn navigation_hint(active_pane: PaneKind, layout: LayoutMode, suggestions_visible: bool) -> String {
    let pane_shortcut = if cfg!(target_os = "macos") {
        "Cmd+1-6"
    } else {
        "Ctrl+1-6"
    };
    let suggestion_hint = if suggestions_visible {
        "Up/Down commands"
    } else {
        "Up/Down transcript"
    };
    let focus_label = if matches!(active_pane, PaneKind::Transcript) {
        "focus transcript".to_owned()
    } else {
        format!("focus {} overlay", active_pane.title().to_ascii_lowercase())
    };

    if matches!(layout, LayoutMode::Compact) {
        format!("{focus_label}  Tab cycle  {pane_shortcut}  {suggestion_hint}")
    } else {
        format!(
            "{focus_label}  Tab/Shift-Tab cycle  {pane_shortcut}  {suggestion_hint}  Ctrl-C exit"
        )
    }
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

fn input_widget(state: &UiState, active_pane: PaneKind, layout: LayoutMode) -> Paragraph<'static> {
    let suggestions_visible = !state.command_suggestions.is_empty();
    let helper = if let Some(helper) = state.prompt_helper.as_deref() {
        helper.to_owned()
    } else if suggestions_visible {
        "Up/Down selects a command. Enter completes the selection first.".to_owned()
    } else if state.input_buffer.is_empty() {
        "Type a prompt or start with / to browse commands.".to_owned()
    } else {
        "Enter submits the prompt.".to_owned()
    };

    let status = status_line(state);
    let lines = vec![
        input_prompt_line(state),
        Line::from(Span::styled(helper, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            navigation_hint(active_pane, layout, suggestions_visible),
            Style::default().fg(Color::DarkGray),
        )),
        status,
    ];

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().title("Prompt").borders(Borders::ALL))
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

    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().title("Suggestions").borders(Borders::ALL))
}

fn footer_lines(state: &UiState, active_pane: PaneKind, layout: LayoutMode) -> Vec<Line<'static>> {
    vec![
        status_line(state),
        Line::from(navigation_hint(active_pane, layout, false)),
    ]
}

fn overlay_title(kind: PaneKind, preview_title: &str) -> String {
    let pane_label = format!("pane {}/{}", kind.number(), PaneKind::ALL.len());
    if preview_title.is_empty() || preview_title == kind.title() {
        format!("{} · {}", kind.title(), pane_label)
    } else {
        format!("{preview_title} · {pane_label}")
    }
}

fn overlay_line_count(state: &UiState, kind: PaneKind) -> usize {
    match kind {
        PaneKind::Tasks => task_lines(state, 6, true).len(),
        PaneKind::Permissions => {
            if state.permission_prompt.is_some() {
                3
            } else {
                1
            }
        }
        _ => pane_preview(state, kind).lines.len().max(1),
    }
}

fn overlay_rect(body_area: Rect, layout: LayoutMode, desired_lines: usize) -> Option<Rect> {
    if body_area.width < 28 || body_area.height < 8 {
        return None;
    }

    let max_height = body_area.height.saturating_sub(2);
    if max_height < 6 {
        return None;
    }

    let preferred_height = (desired_lines as u16).saturating_add(2).max(6);
    let height_cap = if matches!(layout, LayoutMode::Compact) {
        max_height.min(10)
    } else {
        max_height.min((body_area.height / 2).max(12))
    };
    let height = preferred_height.min(height_cap.max(6));

    let max_width = body_area
        .width
        .saturating_sub(if matches!(layout, LayoutMode::Compact) {
            2
        } else {
            5
        });
    if max_width < 26 {
        return None;
    }

    let width = if matches!(layout, LayoutMode::Compact) {
        max_width
    } else {
        ((body_area.width.saturating_mul(45)) / 100)
            .clamp(38, 58)
            .min(max_width)
    };

    let x = if matches!(layout, LayoutMode::Compact) {
        body_area.x + 1
    } else {
        body_area.x + body_area.width.saturating_sub(width + 2)
    };
    let y = body_area.y + body_area.height.saturating_sub(height + 1);
    Some(Rect::new(x, y, width, height))
}

fn render_overlay(frame: &mut Frame<'_>, state: &UiState, body_area: Rect, layout: LayoutMode) {
    let Some(kind) = overlay_kind(state) else {
        return;
    };
    let Some(area) = overlay_rect(body_area, layout, overlay_line_count(state, kind)) else {
        return;
    };

    let preview = pane_preview(state, kind);
    let title = overlay_title(kind, &preview.title);
    frame.render_widget(Clear, area);
    match kind {
        PaneKind::Tasks => frame.render_widget(tasks_widget(state, &title, true), area),
        PaneKind::Permissions => frame.render_widget(permission_widget(state, &title), area),
        _ => frame.render_widget(preview_widget(PanePreview { title, ..preview }), area),
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

fn render_body(frame: &mut Frame<'_>, state: &UiState, body_area: Rect, layout: LayoutMode) {
    frame.render_widget(transcript_widget(state), body_area);
    render_overlay(frame, state, body_area, layout);
}

fn render_frame(frame: &mut Frame<'_>, state: &UiState) {
    let area = frame.area();
    let layout = layout_mode(area, state);
    if matches!(layout, LayoutMode::TooSmall) {
        render_too_small(frame, area, state);
        return;
    }

    let header_height = if matches!(layout, LayoutMode::Compact) {
        2
    } else {
        1
    };
    let mut constraints = vec![Constraint::Length(header_height)];
    if state.compact_banner.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let body_min_height = if state.show_input {
        6
    } else if matches!(layout, LayoutMode::Compact) {
        7
    } else {
        8
    };
    constraints.push(Constraint::Min(body_min_height));
    let activity_height = if state.show_input {
        let lines = activity_lines(state);
        if lines.is_empty() {
            0
        } else {
            let max_height = if matches!(layout, LayoutMode::Compact) {
                6
            } else {
                8
            };
            ((lines.len() as u16).saturating_add(2)).min(max_height)
        }
    } else {
        0
    };
    if state.show_input {
        let base_input_height = if matches!(layout, LayoutMode::Compact) {
            COMPACT_INPUT_HEIGHT
        } else {
            STANDARD_INPUT_HEIGHT
        };
        let active_pane = state.active_pane_or_default();
        let prompt_width = area.width.saturating_sub(2);
        let content_height = prompt_content_height(state, active_pane, layout, prompt_width);
        let input_height = content_height.saturating_add(2).max(base_input_height).min(
            if matches!(layout, LayoutMode::Compact) {
                11
            } else {
                12
            },
        );
        let banner_height = u16::from(state.compact_banner.is_some());
        let reserved =
            header_height + banner_height + body_min_height + activity_height + input_height;
        let available_for_suggestions = area.height.saturating_sub(reserved);
        let max_suggestion_height =
            (state.command_suggestions.len().min(MAX_VISIBLE_SUGGESTIONS) as u16).saturating_add(2);
        let suggestion_height = if available_for_suggestions >= 3 {
            available_for_suggestions.min(max_suggestion_height)
        } else {
            0
        };

        if activity_height > 0 {
            constraints.push(Constraint::Length(activity_height));
        }
        if suggestion_height > 0 {
            constraints.push(Constraint::Length(suggestion_height));
        }
        constraints.push(Constraint::Length(input_height));
    } else {
        constraints.push(Constraint::Length(2));
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut index = 0;
    let header_area = vertical[index];
    index += 1;
    let banner_area = if state.compact_banner.is_some() {
        let area = vertical[index];
        index += 1;
        Some(area)
    } else {
        None
    };
    let body_area = vertical[index];
    index += 1;
    let activity_area = if state.show_input && activity_height > 0 {
        let area = vertical[index];
        index += 1;
        Some(area)
    } else {
        None
    };
    let active_pane = state.active_pane_or_default();
    let footer_area = if state.show_input {
        None
    } else {
        Some(vertical[index])
    };
    let suggestion_area = if state.show_input && vertical.len() > index + 1 {
        Some(vertical[index])
    } else {
        None
    };
    let input_area = if state.show_input {
        vertical[vertical.len() - 1]
    } else {
        footer_area.expect("footer area must exist when prompt is hidden")
    };

    let header =
        Paragraph::new(header_lines(state, layout, header_area.width)).wrap(Wrap { trim: true });
    frame.render_widget(header, header_area);

    if let (Some(area), Some(text)) = (banner_area, state.compact_banner.as_deref()) {
        let banner = Paragraph::new(Line::from(vec![
            Span::styled("banner ", Style::default().fg(Color::Yellow)),
            Span::raw(text.to_owned()),
        ]))
        .wrap(Wrap { trim: true });
        frame.render_widget(banner, area);
    }

    render_body(frame, state, body_area, layout);

    if state.show_input {
        if let Some(area) = activity_area {
            frame.render_widget(activity_widget(state), area);
        }
        if let Some(area) = suggestion_area {
            frame.render_widget(command_suggestions_widget(state), area);
        }
        frame.render_widget(input_widget(state, active_pane, layout), input_area);
    } else {
        if let Some(area) = footer_area {
            let footer =
                Paragraph::new(footer_lines(state, active_pane, layout)).wrap(Wrap { trim: true });
            frame.render_widget(footer, area);
        }
    }
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

        assert!(rendered.contains("Transcript"));
        assert!(rendered.contains("No transcript messages yet."));
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

        assert!(rendered.contains("Permission: bash") || rendered.contains("Permissions"));
        assert!(rendered.contains("Approve once"));
        assert!(rendered.contains("auto compact applied"));
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

        assert!(rendered.contains("pane 2/6"));
        assert!(rendered.contains("Diff preview"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("old line"));
        assert!(rendered.contains("new line"));
        let pane_shortcut = if cfg!(target_os = "macos") {
            "Cmd+1-6"
        } else {
            "Ctrl+1-6"
        };
        assert!(rendered.contains(pane_shortcut));
    }

    #[test]
    fn renders_compact_layout_for_narrow_terminals() {
        let mut state = RatatuiApp::new("compact").initial_state();
        state.transcript_lines = vec![super::TranscriptLine {
            role: "user".to_owned(),
            text: "This layout should collapse cleanly when the terminal is narrow.".to_owned(),
        }];
        state.show_input = true;
        state.task_preview.title = "Setup".to_owned();
        state.task_preview.lines = vec!["Check auth".to_owned(), "Add CLAUDE.md".to_owned()];
        state.active_pane = Some(PaneKind::Tasks);

        let rendered = render_to_string(&state, 60, 24).unwrap();

        assert!(rendered.contains("pane 4/6"));
        assert!(rendered.contains("Check auth"));
        assert!(rendered.contains("Prompt"));
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

        assert!(rendered.contains("Prompt"));
        assert!(rendered.contains("Suggestions"));
        assert!(rendered.contains("/help"));
    }

    #[test]
    fn transcript_widget_supports_scroll_offset() {
        let mut state = RatatuiApp::new("scroll").initial_state();
        state.transcript_lines = vec![
            super::TranscriptLine {
                role: "user".to_owned(),
                text: "line one".to_owned(),
            },
            super::TranscriptLine {
                role: "assistant".to_owned(),
                text: "line two".to_owned(),
            },
            super::TranscriptLine {
                role: "assistant".to_owned(),
                text: "line three".to_owned(),
            },
        ];
        state.transcript_scroll = 2;

        let rendered = render_to_string(&state, 80, 12).unwrap();

        assert!(!rendered.contains("line one"));
        assert!(rendered.contains("line three"));
    }

    #[test]
    fn renders_long_backend_error_in_prompt() {
        let mut state = RatatuiApp::new("error").initial_state();
        state.show_input = true;
        state.status_line = "chatgpt-codex · gpt-5.4 · s:12345678 · error: ChatGPT Codex request failed with status 400 Bad Request: body.input.0.call_id: Field required".to_owned();

        let rendered = render_to_string(&state, 80, 24).unwrap();

        assert!(rendered.contains("Prompt"));
        assert!(rendered.contains("call_id"));
        assert!(rendered.contains("Field required"));
    }
}
