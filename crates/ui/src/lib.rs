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

include!("ui/types.rs");
include!("ui/transcript.rs");
include!("ui/tasks.rs");
include!("ui/chrome.rs");
include!("ui/render.rs");

#[cfg(test)]
mod tests;
