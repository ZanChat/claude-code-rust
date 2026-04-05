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
