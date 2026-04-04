use crate::{auth_hint_for_provider, friendly_auth_source, short_session_id, workspace_is_empty};
use code_agent_providers::config_migration_report;
use code_agent_session::claude_config_home_dir;
use crossterm::event;

use crate::{apply_repl_header, repl_status, shorten_path, status_with_detail};
use crate::{scroll_down, scroll_up};
use code_agent_ui::{draw_terminal as draw_tui, PaneKind, RatatuiApp, TranscriptLine};
use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

use code_agent_ui::{CommandPaletteEntry, PanePreview, UiState};

use code_agent_core::SessionId;

use code_agent_providers::ApiProvider;

use anyhow::Result;

use std::fs;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct StartupPreferences {
    pub(crate) welcome_seen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupScreen {
    pub(crate) title: String,
    pub(crate) body: Vec<String>,
    pub(crate) preview: PanePreview,
}

pub(crate) fn startup_preferences_path() -> PathBuf {
    claude_config_home_dir()
        .join("code-agent-rust")
        .join("startup.json")
}

pub(crate) fn load_startup_preferences() -> StartupPreferences {
    let path = startup_preferences_path();
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StartupPreferences>(&raw).ok())
        .unwrap_or_default()
}

pub(crate) fn save_startup_preferences(preferences: &StartupPreferences) -> Result<()> {
    let path = startup_preferences_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(preferences)?)?;
    Ok(())
}

pub(crate) fn project_onboarding_lines(cwd: &Path) -> Vec<String> {
    if workspace_is_empty(cwd) {
        return vec![
            "The workspace is empty.".to_owned(),
            "Start by asking the agent to create a new app or clone an existing repository."
                .to_owned(),
        ];
    }

    if !cwd.join("CLAUDE.md").exists() {
        return vec![
            "This project does not have a CLAUDE.md file yet.".to_owned(),
            "Add one with repository-specific instructions, workflows, and validation commands."
                .to_owned(),
        ];
    }

    Vec::new()
}

pub(crate) fn build_startup_screens(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    session_root: &Path,
    transcript_path: Option<&Path>,
    live_runtime: bool,
    auth_source: Option<&str>,
    preferences: &StartupPreferences,
) -> Vec<StartupScreen> {
    let mut screens = Vec::new();
    let auth_summary = if live_runtime {
        format!("ready via {}", friendly_auth_source(auth_source))
    } else {
        format!("offline: {}", friendly_auth_source(auth_source))
    };

    if !preferences.welcome_seen {
        let transcript_label = transcript_path
            .map(|path| shorten_path(path, 44))
            .unwrap_or_else(|| "new session".to_owned());
        screens.push(StartupScreen {
            title: "Welcome".to_owned(),
            body: vec![
                "This REPL is now using a native ratatui runtime with adaptive terminal layouts."
                    .to_owned(),
                format!("Provider: {provider}"),
                format!("Model: {active_model}"),
                format!("Auth: {auth_summary}"),
            ],
            preview: PanePreview {
                title: "Runtime".to_owned(),
                lines: vec![
                    format!("session: {}", short_session_id(session_id)),
                    format!("cwd: {}", shorten_path(cwd, 44)),
                    format!("session root: {}", shorten_path(session_root, 44)),
                    format!("transcript: {transcript_label}"),
                ],
            },
        });
    }

    let mut setup_lines = Vec::new();
    if !live_runtime {
        setup_lines.push(format!(
            "Live provider access is not configured yet. {}",
            auth_hint_for_provider(provider)
        ));
    }
    setup_lines.extend(project_onboarding_lines(cwd));

    if !setup_lines.is_empty() {
        let migration = config_migration_report(provider);
        let mut preview_lines = vec![format!(
            "auth source: {}",
            friendly_auth_source(auth_source)
        )];
        if let Some(path) = migration.codex_auth_path {
            preview_lines.push(format!("codex auth: {}", shorten_path(&path, 44)));
        }
        if let Some(path) = migration.auth_snapshot_path {
            preview_lines.push(format!("snapshot: {}", shorten_path(&path, 44)));
        }
        preview_lines.push("commands: /help /config /ide /login /model".to_owned());

        screens.push(StartupScreen {
            title: "Setup Checklist".to_owned(),
            body: setup_lines,
            preview: PanePreview {
                title: "Next Steps".to_owned(),
                lines: preview_lines,
            },
        });
    }

    screens
}

pub(crate) fn startup_command_palette() -> Vec<CommandPaletteEntry> {
    vec![
        CommandPaletteEntry {
            name: "/help".to_owned(),
            description: "Show the available REPL commands.".to_owned(),
        },
        CommandPaletteEntry {
            name: "/config".to_owned(),
            description: "Inspect the current runtime configuration.".to_owned(),
        },
        CommandPaletteEntry {
            name: "/login".to_owned(),
            description: "Authenticate against the active provider.".to_owned(),
        },
        CommandPaletteEntry {
            name: "/model".to_owned(),
            description: "Inspect or switch the active model.".to_owned(),
        },
    ]
}

pub(crate) fn build_startup_ui_state(
    app: &RatatuiApp,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    screens: &[StartupScreen],
    index: usize,
    transcript_scroll: u16,
) -> UiState {
    let screen = &screens[index];
    let mut state = app.initial_state();
    apply_repl_header(&mut state, provider, active_model, cwd, session_id);
    state.status_line = status_with_detail(
        repl_status(provider, active_model, session_id),
        format!("setup {}/{}", index + 1, screens.len()),
    );
    state.show_input = true;
    state.prompt_helper =
        Some("Type to enter the REPL immediately. Enter also continues.".to_owned());
    state.active_pane = Some(PaneKind::Transcript);
    state.transcript_lines = screen
        .body
        .iter()
        .map(|line| TranscriptLine {
            role: "setup".to_owned(),
            text: line.clone(),
            author_label: None,
        })
        .collect();
    state.transcript_scroll = transcript_scroll;
    state.transcript_preview = PanePreview {
        title: screen.title.clone(),
        lines: screen.body.clone(),
    };
    state.task_preview = screen.preview.clone();
    state.command_palette = startup_command_palette();
    state.compact_banner = Some(if index + 1 == screens.len() {
        "Type to start the REPL. Enter also continues.".to_owned()
    } else {
        "Type to start the REPL now, or Enter for the next screen.".to_owned()
    });
    state
}

pub(crate) fn run_startup_flow<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    screens: &[StartupScreen],
) -> Result<code_agent_ui::InputBuffer> {
    if screens.is_empty() {
        return Ok(code_agent_ui::InputBuffer::new());
    }

    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
    let mut index = 0usize;
    let mut transcript_scroll = 0u16;

    loop {
        let state = build_startup_ui_state(
            &app,
            provider,
            active_model,
            session_id,
            cwd,
            screens,
            index,
            transcript_scroll,
        );
        draw_tui(terminal, &state)?;

        match event::read()? {
            Event::Resize(width, height) => {
                terminal.resize(Rect::new(0, 0, width, height))?;
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    scroll_up(&mut transcript_scroll, 3);
                }
                MouseEventKind::ScrollDown => {
                    scroll_down(&mut transcript_scroll, 3);
                }
                _ => {}
            },
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Tab => {
                    if index + 1 >= screens.len() {
                        break;
                    }
                    index += 1;
                }
                KeyCode::Left | KeyCode::BackTab => {
                    index = index.saturating_sub(1);
                }
                KeyCode::Up | KeyCode::PageUp => {
                    scroll_up(&mut transcript_scroll, 1);
                }
                KeyCode::Down | KeyCode::PageDown => {
                    scroll_down(&mut transcript_scroll, 1);
                }
                KeyCode::Home => {
                    transcript_scroll = u16::MAX;
                }
                KeyCode::End => {
                    transcript_scroll = 0;
                }
                KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char(ch) if key.modifiers.is_empty() => {
                    let mut input_buffer = code_agent_ui::InputBuffer::new();
                    input_buffer.push(ch);
                    return Ok(input_buffer);
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(code_agent_ui::InputBuffer::new())
}
