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

const UI_ROLE_ATTRIBUTE: &str = "ui_role";
const UI_AUTHOR_ATTRIBUTE: &str = "ui_author";

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
pub struct TranscriptGroup {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub expanded: bool,
    pub single_item: bool,
    pub lines: Vec<TranscriptLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptItem {
    Line(TranscriptLine),
    Group(TranscriptGroup),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiMouseAction {
    JumpToBottom,
    ToggleTranscriptGroup(String),
    SetPromptCursor(usize),
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
pub struct ChoiceListItem {
    pub label: String,
    pub detail: Option<String>,
    pub secondary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChoiceListState {
    pub title: String,
    pub subtitle: Option<String>,
    pub items: Vec<ChoiceListItem>,
    pub selected: usize,
    pub empty_message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskUiEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub kind: String,
    pub status: TaskStatus,
    pub owner_label: Option<String>,
    pub blocker_labels: Vec<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub tree_prefix: String,
    pub detail_prefix: String,
    pub is_recent_completion: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionUiEntry {
    pub prompt: String,
    pub choices: Vec<String>,
    pub task_title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSearchState {
    pub input_buffer: InputBuffer,
    pub open: bool,
    pub active_item: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptMessageActionsState {
    pub active_item: usize,
    pub enter_label: Option<String>,
    pub primary_input_label: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistorySearchState {
    pub input_buffer: InputBuffer,
    pub active_match: Option<usize>,
    pub match_count: usize,
    pub failed_match: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSelectionPoint {
    pub line_index: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSelectionState {
    pub anchor: TranscriptSelectionPoint,
    pub focus: TranscriptSelectionPoint,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptSelectionState {
    pub anchor: usize,
    pub focus: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptSelectableLine {
    pub item_index: usize,
    pub line_index: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub messages: Vec<Message>,
    pub transcript_items: Vec<TranscriptItem>,
    pub transcript_lines: Vec<TranscriptLine>,
    pub transcript_groups: Vec<TranscriptGroup>,
    pub queued_inputs: Vec<String>,
    pub header_title: Option<String>,
    pub header_subtitle: Option<String>,
    pub header_context: Option<String>,
    pub transcript_scroll: u16,
    pub status_line: String,
    pub status_marquee_tick: usize,
    pub input_buffer: InputBuffer,
    pub prompt_helper: Option<String>,
    pub show_input: bool,
    pub transcript_mode: bool,
    pub transcript_search: Option<TranscriptSearchState>,
    pub message_actions: Option<TranscriptMessageActionsState>,
    pub prompt_history_search: Option<PromptHistorySearchState>,
    pub prompt_selection: Option<PromptSelectionState>,
    pub transcript_selection: Option<TranscriptSelectionState>,
    pub command_palette: Vec<CommandPaletteEntry>,
    pub command_suggestions: Vec<CommandPaletteEntry>,
    pub selected_command_suggestion: Option<usize>,
    pub active_pane: Option<PaneKind>,
    pub choice_list: Option<ChoiceListState>,
    pub notifications: VecDeque<Notification>,
    pub permission_prompt: Option<PermissionPromptState>,
    pub progress_message: Option<String>,
    pub progress_verb: Option<String>,
    pub pending_step_count: usize,
    pub pending_transcript_details: bool,
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
            .map(transcript_line_from_message)
            .collect::<Vec<_>>();
        let transcript_items = transcript_lines
            .iter()
            .cloned()
            .map(TranscriptItem::Line)
            .collect::<Vec<_>>();

        let mut state = Self {
            messages,
            transcript_items,
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
                wrap_plain_text(&subtitle, width.saturating_sub(5).max(1) as usize)
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

fn transcript_viewport(
    state: &UiState,
    width: u16,
    height: u16,
) -> (Vec<TranscriptRenderLine>, u16) {
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

    for item in resolved_transcript_items(state).into_iter().rev() {
        match item {
            TranscriptItem::Line(line) if line.role == "user" => {
                return Some(truncate_middle(
                    &line.text,
                    width.saturating_sub(4) as usize,
                ));
            }
            TranscriptItem::Group(group) => {
                if let Some(line) = group.lines.iter().rev().find(|line| line.role == "user") {
                    return Some(truncate_middle(
                        &line.text,
                        width.saturating_sub(4) as usize,
                    ));
                }
            }
            _ => {}
        }
    }

    None
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

fn task_kind_label(kind: &str) -> Option<&str> {
    match kind {
        "" | "task" | "workflow" | "workflow_step" => None,
        "assistant_worker" => Some("worker"),
        "assistant_synthesis" => Some("synthesis"),
        other => Some(other),
    }
}

fn task_status_visual(task: &TaskUiEntry) -> (&'static str, Style, Style) {
    match task.status {
        TaskStatus::Pending => ("○", Style::default().fg(Color::DarkGray), Style::default()),
        TaskStatus::Running => (
            "●",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        TaskStatus::WaitingForInput => (
            "◆",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Completed if task.is_recent_completion => (
            "✓",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Green),
        ),
        TaskStatus::Completed => (
            "✓",
            Style::default().fg(Color::Green),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        TaskStatus::Failed => (
            "✕",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Red),
        ),
        TaskStatus::Cancelled => (
            "◌",
            Style::default().fg(Color::Magenta),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    }
}

fn task_header_line(task: &TaskUiEntry, render_as_root: bool) -> Line<'static> {
    let (icon, icon_style, title_style) = task_status_visual(task);
    let mut spans = Vec::new();

    if !render_as_root && !task.tree_prefix.is_empty() {
        spans.push(Span::styled(
            task.tree_prefix.clone(),
            Style::default().fg(Color::DarkGray),
        ));
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
            Style::default().fg(Color::Cyan),
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
            Style::default().fg(Color::DarkGray),
        ));
    }

    if let Some(kind) = task_kind_label(&task.kind) {
        spans.push(Span::styled(
            format!("  [{kind}]"),
            Style::default().fg(Color::DarkGray),
        ));
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
        Style::default().fg(Color::DarkGray),
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
                    Style::default().fg(Color::DarkGray),
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            spinner_headline(verb),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(detail) = detail {
        spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            detail.to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Some(Line::from(spans))
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
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    for queued_input in state.queued_inputs.iter().take(3) {
        lines.push(Line::from(vec![
            Span::styled(
                "queue  ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(queued_input.clone(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    if state.queued_inputs.len() > 3 {
        lines.push(Line::from(Span::styled(
            format!(
                "queue  +{} more follow-up messages",
                state.queued_inputs.len() - 3
            ),
            Style::default().fg(Color::DarkGray),
        )));
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

        lines.push(Line::from(Span::styled(
            format!("{prefix}{}", item.label),
            item_style,
        )));
        if let Some(detail) = item
            .detail
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(Line::from(Span::styled(
                format!("    {detail}"),
                detail_style,
            )));
        }
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
        if index + 1 < end {
            lines.push(Line::from(""));
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

#[cfg(test)]
mod tests {
    use super::{
        footer_primary_text, input_prompt_line, mouse_action_for_position,
        pane_shortcut_label_for_terminal, pending_details_toggle_label, progress_line,
        render_to_string, task_lines, transcript_search_match_items,
        transcript_search_scroll_for_view, transcript_selectable_lines_for_view,
        transcript_selection_text_for_view, transcript_visual_lines, ChoiceListItem,
        ChoiceListState, InputBuffer, Notification, PaneKind, PermissionPromptState,
        PromptHistorySearchState, PromptSelectionState, RatatuiApp, StatusLevel, TaskUiEntry,
        TranscriptGroup, TranscriptItem, TranscriptLine, TranscriptMessageActionsState,
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

        assert!(
            transcript_search_scroll_for_view(&state, 72, 12, 0).is_some_and(|scroll| scroll > 0)
        );
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
}
