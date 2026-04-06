use anyhow::Result;
use ccrust_core::{
    CommandSpec, ContentBlock, Message, MessageMetadata, MessageRole, TaskStatus, TokenUsage,
};
use ccrust_session::estimate_message_tokens;
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;

pub mod vim;

include!("ui/theme.rs");
include!("ui/types.rs");
include!("ui/transcript.rs");
include!("ui/tasks.rs");
include!("ui/chrome.rs");
include!("ui/render.rs");

#[cfg(test)]
mod tests;
