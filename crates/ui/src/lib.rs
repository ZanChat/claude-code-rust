use anyhow::Result;
use code_agent_core::{CommandSpec, ContentBlock, Message, MessageRole};
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Terminal;
use std::collections::VecDeque;

pub mod vim;

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

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub messages: Vec<Message>,
    pub transcript_lines: Vec<TranscriptLine>,
    pub status_line: String,
    pub input_buffer: InputBuffer,
    pub command_palette: Vec<CommandPaletteEntry>,
    pub active_pane: Option<PaneKind>,
    pub notifications: VecDeque<Notification>,
    pub permission_prompt: Option<PermissionPromptState>,
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

    let mut clipped = trimmed.chars().take(max_chars.saturating_sub(3)).collect::<String>();
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

fn active_pane_preview(state: &UiState) -> PanePreview {
    match state.active_pane_or_default() {
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

pub fn render_to_string(state: &UiState, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(frame.area());
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(vertical[2]);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(8),
            ])
            .split(body[1]);

        let active_pane = state.active_pane_or_default();
        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                "code-agent-rust",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(state.status_line.clone()),
            Span::raw("  "),
            Span::styled(
                format!("pane={}", active_pane.title()),
                Style::default().fg(Color::Green),
            ),
            Span::raw("  "),
            Span::styled(
                if state.vim_state.is_insert() { "insert" } else { "vim" },
                Style::default().fg(Color::Magenta),
            ),
        ]));
        frame.render_widget(header, vertical[0]);

        let banner = match state.compact_banner.as_deref() {
            Some(text) => Paragraph::new(Line::from(vec![
                Span::styled("banner ", Style::default().fg(Color::Yellow)),
                Span::raw(text.to_owned()),
            ])),
            None => Paragraph::new(Line::from("")),
        };
        frame.render_widget(banner, vertical[1]);

        let transcript_items = if state.transcript_lines.is_empty() {
            vec![ListItem::new(Line::from("No transcript messages yet."))]
        } else {
            state
                .transcript_lines
                .iter()
                .map(|line| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:>10} ", line.role),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(line.text.clone()),
                    ]))
                })
                .collect::<Vec<_>>()
        };
        let transcript = List::new(transcript_items)
            .block(Block::default().title("Transcript").borders(Borders::ALL));
        frame.render_widget(transcript, body[0]);

        let tab_titles = PaneKind::ALL
            .iter()
            .enumerate()
            .map(|(index, kind)| Line::from(format!("[{}] {}", index + 1, kind.title())))
            .collect::<Vec<_>>();
        let selected = PaneKind::ALL
            .iter()
            .position(|kind| *kind == active_pane)
            .unwrap_or(0);
        let tabs = Tabs::new(tab_titles)
            .select(selected)
            .divider(" ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().title("Panes").borders(Borders::ALL));
        frame.render_widget(tabs, sidebar[0]);

        let preview = active_pane_preview(state);
        let preview_lines = if preview.lines.is_empty() {
            vec![Line::from("No details available.")]
        } else {
            preview
                .lines
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect::<Vec<_>>()
        };
        let preview_widget = Paragraph::new(preview_lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(preview.title).borders(Borders::ALL));
        frame.render_widget(preview_widget, sidebar[1]);

        let command_lines = if state.command_palette.is_empty() {
            vec![Line::from("No commands loaded.")]
        } else {
            state
                .command_palette
                .iter()
                .take(6)
                .map(|entry| Line::from(format!("{:<18} {}", entry.name, entry.description)))
                .collect::<Vec<_>>()
        };
        let commands = Paragraph::new(command_lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().title("Commands").borders(Borders::ALL));
        frame.render_widget(commands, sidebar[2]);

        let primary_footer = if let Some(prompt) = &state.permission_prompt {
            Line::from(vec![
                Span::styled("permission ", Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    "{} -> {} / {}",
                    prompt.tool_name, prompt.allow_once_label, prompt.deny_label
                )),
            ])
        } else if let Some(notification) = state.notifications.back() {
            Line::from(vec![
                Span::styled(
                    format!("{} ", notification.title),
                    notification.level.unwrap_or(StatusLevel::Info).style(),
                ),
                Span::raw(notification.body.clone()),
            ])
        } else if !state.input_buffer.is_empty() {
            let text = state.input_buffer.as_str();
            let pos = state.input_buffer.cursor;
            if pos < text.chars().count() {
                let left: String = text.chars().take(pos).collect();
                let cursor_char: String = text.chars().skip(pos).take(1).collect();
                let right: String = text.chars().skip(pos + 1).collect();
                Line::from(vec![
                    Span::raw("input: "),
                    Span::raw(left),
                    Span::styled(cursor_char, Style::default().bg(Color::White).fg(Color::Black)),
                    Span::raw(right),
                ])
            } else {
                Line::from(vec![
                    Span::raw(format!("input: {}", text)),
                    Span::styled(" ", Style::default().bg(Color::White)),
                ])
            }
        } else {
            Line::from(state.status_line.clone())
        };
        let footer = Paragraph::new(vec![
            primary_footer,
            Line::from("keys: Tab/Shift-Tab or 1-6 switch panes"),
        ]);
        frame.render_widget(footer, vertical[3]);
    })?;

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
    fn renders_transcript_and_command_palette() {
        let app = RatatuiApp::new("session preview");
        let state = app.state_from_messages(
            vec![Message::new(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "Render this transcript".to_owned(),
                }],
            )],
            &compatibility_command_registry().all(),
        );

        let rendered = render_to_string(&state, 100, 24).unwrap();

        assert!(rendered.contains("Transcript"));
        assert!(rendered.contains("Render this transcript"));
        assert!(rendered.contains("/compact"));
        assert!(rendered.contains("[1] Transcript"));
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

        assert!(rendered.contains("Permission: bash"));
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

        assert!(rendered.contains("Diff preview"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("old line"));
        assert!(rendered.contains("new line"));
        assert!(rendered.contains("keys: Tab/Shift-Tab or 1-6 switch panes"));
    }
}
