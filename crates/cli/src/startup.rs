use crate::{auth_hint_for_provider, friendly_auth_source, workspace_is_empty};
use code_agent_session::claude_config_home_dir;
use crossterm::event;

use crate::{apply_repl_header, repl_status, status_with_detail};
use crate::{scroll_down, scroll_up};
use code_agent_ui::{
    draw_terminal as draw_tui, ChoiceListItem, ChoiceListState, PaneKind, RatatuiApp,
    TranscriptLine,
};
use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

use code_agent_ui::{CommandPaletteEntry, PanePreview, UiState};

use code_agent_core::SessionId;

use code_agent_providers::{
    compatibility_model_catalog, get_anthropic_auth_material, get_openai_auth_status,
    is_openai_provider, provider_descriptor, read_provider_auth_snapshot, ApiProvider,
    ModelCatalog, OpenAIAuthSource,
};

use anyhow::Result;

use std::env;
use std::fs;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct StartupPreferences {
    #[serde(default)]
    pub(crate) welcome_seen: bool,
    #[serde(default)]
    pub(crate) selected_provider: Option<ApiProvider>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchProviderSource {
    Cli,
    Env,
    Preference,
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaunchProviderSelection {
    pub(crate) provider: ApiProvider,
    pub(crate) configured: bool,
    pub(crate) source: LaunchProviderSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupScreen {
    pub(crate) title: String,
    pub(crate) body: Vec<String>,
    pub(crate) preview: PanePreview,
    pub(crate) choice_list: Option<ChoiceListState>,
    pub(crate) provider_configured: bool,
    pub(crate) show_input: bool,
    pub(crate) prompt_helper: Option<String>,
    pub(crate) compact_banner: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupFlowResult {
    pub(crate) input_buffer: code_agent_ui::InputBuffer,
    pub(crate) provider: ApiProvider,
}

pub(crate) fn startup_preferences_path() -> PathBuf {
    claude_config_home_dir().join("ccrust").join("startup.json")
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

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn env_var_present(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub(crate) fn resolve_launch_provider(
    explicit: Option<&str>,
    preferences: &StartupPreferences,
) -> Result<LaunchProviderSelection> {
    if let Some(raw) = explicit.filter(|value| !value.trim().is_empty()) {
        return Ok(LaunchProviderSelection {
            provider: raw.parse()?,
            configured: true,
            source: LaunchProviderSource::Cli,
        });
    }

    if let Ok(raw) = env::var("CLAUDE_CODE_API_PROVIDER") {
        if !raw.trim().is_empty() {
            return Ok(LaunchProviderSelection {
                provider: raw.parse()?,
                configured: true,
                source: LaunchProviderSource::Env,
            });
        }
    }

    for (flag, provider) in [
        ("CLAUDE_CODE_USE_BEDROCK", ApiProvider::Bedrock),
        ("CLAUDE_CODE_USE_VERTEX", ApiProvider::Vertex),
        ("CLAUDE_CODE_USE_FOUNDRY", ApiProvider::Foundry),
    ] {
        if env_flag(flag) {
            return Ok(LaunchProviderSelection {
                provider,
                configured: true,
                source: LaunchProviderSource::Env,
            });
        }
    }

    if let Some(provider) = preferences.selected_provider {
        return Ok(LaunchProviderSelection {
            provider,
            configured: true,
            source: LaunchProviderSource::Preference,
        });
    }

    Ok(LaunchProviderSelection {
        provider: ApiProvider::FirstParty,
        configured: false,
        source: LaunchProviderSource::Default,
    })
}

pub(crate) fn default_model_for_provider(provider: ApiProvider) -> Option<String> {
    compatibility_model_catalog(provider)
        .list_models()
        .first()
        .map(|model| model.id.clone())
}

fn openai_auth_source_label(source: OpenAIAuthSource) -> &'static str {
    match source {
        OpenAIAuthSource::OpenAiApiKey => "OPENAI_API_KEY",
        OpenAIAuthSource::CodexAuthApiKey => "codex_auth_api_key",
        OpenAIAuthSource::CodexAuthToken => "codex_auth_token",
        OpenAIAuthSource::None => "none",
    }
}

fn provider_auth_status(provider: ApiProvider) -> (bool, Option<String>) {
    match provider {
        ApiProvider::FirstParty => {
            let auth = get_anthropic_auth_material(provider)
                .or_else(|| read_provider_auth_snapshot(provider));
            (
                auth.is_some(),
                auth.and_then(|material| material.source.filter(|value| !value.trim().is_empty())),
            )
        }
        ApiProvider::OpenAI => {
            let status = get_openai_auth_status(provider);
            if status.has_credentials {
                return (
                    true,
                    Some(openai_auth_source_label(status.source).to_owned()),
                );
            }
            let snapshot = read_provider_auth_snapshot(provider);
            (
                snapshot.is_some(),
                snapshot
                    .and_then(|material| material.source.filter(|value| !value.trim().is_empty())),
            )
        }
        ApiProvider::ChatGPTCodex => {
            let status = get_openai_auth_status(provider);
            if status.has_credentials && status.source == OpenAIAuthSource::CodexAuthToken {
                return (
                    true,
                    Some(openai_auth_source_label(status.source).to_owned()),
                );
            }
            let snapshot = read_provider_auth_snapshot(provider).filter(|material| {
                material
                    .source
                    .as_deref()
                    .is_some_and(|source| source == "codex_auth_token")
            });
            (
                snapshot.is_some(),
                snapshot
                    .and_then(|material| material.source.filter(|value| !value.trim().is_empty())),
            )
        }
        ApiProvider::OpenAICompatible => {
            let status = get_openai_auth_status(provider);
            let has_key = status.has_credentials
                && matches!(
                    status.source,
                    OpenAIAuthSource::OpenAiApiKey | OpenAIAuthSource::CodexAuthApiKey
                );
            let has_base_url = env_var_present("OPENAI_BASE_URL");
            if has_key && has_base_url {
                return (
                    true,
                    Some(openai_auth_source_label(status.source).to_owned()),
                );
            }
            let snapshot = read_provider_auth_snapshot(provider)
                .filter(|material| material.api_key.is_some() && has_base_url);
            (
                snapshot.is_some(),
                snapshot
                    .and_then(|material| material.source.filter(|value| !value.trim().is_empty())),
            )
        }
        ApiProvider::Bedrock | ApiProvider::Vertex | ApiProvider::Foundry => {
            (true, Some("ambient_cloud_auth".to_owned()))
        }
    }
}

fn provider_choice_list(selected_provider: ApiProvider) -> ChoiceListState {
    let items = ApiProvider::ALL
        .iter()
        .map(|provider| {
            let (ready, auth_source) = provider_auth_status(*provider);
            let auth_label = if ready {
                format!("ready via {}", friendly_auth_source(auth_source.as_deref()))
            } else {
                "needs setup".to_owned()
            };
            ChoiceListItem {
                label: provider_descriptor(*provider).display_name,
                detail: Some(format!("{} · {auth_label}", provider.as_str())),
                secondary: None,
            }
        })
        .collect::<Vec<_>>();
    let selected = ApiProvider::ALL
        .iter()
        .position(|provider| *provider == selected_provider)
        .unwrap_or_default();
    ChoiceListState {
        title: "Choose provider".to_owned(),
        subtitle: Some("Select the API backend for this ccrust install.".to_owned()),
        items,
        selected,
        empty_message: None,
    }
}

fn build_provider_setup_screen(
    provider: ApiProvider,
    cwd: &Path,
    provider_configured: bool,
) -> StartupScreen {
    let descriptor = provider_descriptor(provider);
    let default_model =
        default_model_for_provider(provider).unwrap_or_else(|| "unknown".to_owned());
    let (ready, auth_source) = provider_auth_status(provider);
    let auth_summary = if ready {
        format!("ready via {}", friendly_auth_source(auth_source.as_deref()))
    } else {
        format!("needs setup. {}", auth_hint_for_provider(provider))
    };
    let mut body = if provider_configured {
        vec![
            format!("{} is selected for this install.", descriptor.display_name),
            format!("Auth: {auth_summary}"),
        ]
    } else {
        vec![
            "Select the provider to use when ccrust starts without flags.".to_owned(),
            format!("Current selection: {}", descriptor.display_name),
            format!("Auth: {auth_summary}"),
        ]
    };
    body.extend(project_onboarding_lines(cwd));

    StartupScreen {
        title: "Onboarding".to_owned(),
        body,
        preview: PanePreview {
            title: "Next Steps".to_owned(),
            lines: vec![
                format!("provider: {}", provider.as_str()),
                format!("model: {default_model}"),
                "commands: /login /config /model".to_owned(),
                if ready {
                    "press Enter to open the REPL".to_owned()
                } else {
                    "press Enter, then run /login or /config".to_owned()
                },
            ],
        },
        choice_list: Some(provider_choice_list(provider)),
        provider_configured,
        show_input: false,
        prompt_helper: Some("Use ↑/↓ to choose a provider. Enter opens the REPL.".to_owned()),
        compact_banner: Some(if ready {
            "Provider is ready. Enter opens the REPL.".to_owned()
        } else if is_openai_provider(provider) {
            "Provider selected. Enter opens the REPL; then finish login/config.".to_owned()
        } else {
            "Provider selected. Enter opens the REPL.".to_owned()
        }),
    }
}

pub(crate) fn build_startup_screens(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    session_root: &Path,
    transcript_path: Option<&Path>,
    live_runtime: bool,
    provider_configured: bool,
    auth_source: Option<&str>,
    preferences: &StartupPreferences,
) -> Vec<StartupScreen> {
    let _ = (
        active_model,
        session_id,
        session_root,
        auth_source,
        preferences,
    );
    if transcript_path.is_some() {
        return Vec::new();
    }
    if provider_configured && live_runtime {
        return Vec::new();
    }
    vec![build_provider_setup_screen(
        provider,
        cwd,
        provider_configured,
    )]
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
    screen: &StartupScreen,
    index: usize,
    total: usize,
    transcript_scroll: u16,
) -> UiState {
    let mut state = app.initial_state();
    apply_repl_header(&mut state, provider, active_model, cwd, session_id);
    state.status_line = status_with_detail(
        repl_status(provider, active_model, session_id),
        format!("setup {}/{}", index + 1, total),
    );
    state.show_input = screen.show_input;
    state.prompt_helper = screen.prompt_helper.clone();
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
    state.choice_list = screen.choice_list.clone();
    state.transcript_preview = PanePreview {
        title: screen.title.clone(),
        lines: screen.body.clone(),
    };
    state.task_preview = screen.preview.clone();
    state.command_palette = startup_command_palette();
    state.compact_banner = screen.compact_banner.clone();
    state
}

pub(crate) fn run_startup_flow<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    screens: &[StartupScreen],
) -> Result<StartupFlowResult> {
    if screens.is_empty() {
        return Ok(StartupFlowResult {
            input_buffer: code_agent_ui::InputBuffer::new(),
            provider,
        });
    }

    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
    let mut index = 0usize;
    let mut transcript_scroll = 0u16;
    let mut selected_provider = provider;

    loop {
        let screen = if screens[index].choice_list.is_some() {
            build_provider_setup_screen(selected_provider, cwd, screens[index].provider_configured)
        } else {
            screens[index].clone()
        };
        let screen_provider = if screen.choice_list.is_some() {
            selected_provider
        } else {
            provider
        };
        let screen_model = if screen.choice_list.is_some() {
            default_model_for_provider(selected_provider).unwrap_or_else(|| active_model.to_owned())
        } else {
            active_model.to_owned()
        };
        let state = build_startup_ui_state(
            &app,
            screen_provider,
            &screen_model,
            session_id,
            cwd,
            &screen,
            index,
            screens.len(),
            transcript_scroll,
        );
        draw_tui(terminal, &state)?;

        match event::read()? {
            Event::Resize(width, height) => {
                terminal.resize(Rect::new(0, 0, width, height))?;
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if screen.choice_list.is_some() {
                        let current = ApiProvider::ALL
                            .iter()
                            .position(|provider| *provider == selected_provider)
                            .unwrap_or_default();
                        let next = current.saturating_sub(1);
                        selected_provider = ApiProvider::ALL[next];
                    } else {
                        scroll_up(&mut transcript_scroll, 3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if screen.choice_list.is_some() {
                        let current = ApiProvider::ALL
                            .iter()
                            .position(|provider| *provider == selected_provider)
                            .unwrap_or_default();
                        let next = (current + 1).min(ApiProvider::ALL.len().saturating_sub(1));
                        selected_provider = ApiProvider::ALL[next];
                    } else {
                        scroll_down(&mut transcript_scroll, 3);
                    }
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
                    if screen.choice_list.is_some() {
                        let current = ApiProvider::ALL
                            .iter()
                            .position(|provider| *provider == selected_provider)
                            .unwrap_or_default();
                        selected_provider = ApiProvider::ALL[current.saturating_sub(1)];
                    } else {
                        scroll_up(&mut transcript_scroll, 1);
                    }
                }
                KeyCode::Down | KeyCode::PageDown => {
                    if screen.choice_list.is_some() {
                        let current = ApiProvider::ALL
                            .iter()
                            .position(|provider| *provider == selected_provider)
                            .unwrap_or_default();
                        let next = (current + 1).min(ApiProvider::ALL.len().saturating_sub(1));
                        selected_provider = ApiProvider::ALL[next];
                    } else {
                        scroll_down(&mut transcript_scroll, 1);
                    }
                }
                KeyCode::Home => {
                    if screen.choice_list.is_some() {
                        selected_provider = ApiProvider::ALL[0];
                    } else {
                        transcript_scroll = u16::MAX;
                    }
                }
                KeyCode::End => {
                    if screen.choice_list.is_some() {
                        selected_provider = *ApiProvider::ALL.last().unwrap_or(&selected_provider);
                    } else {
                        transcript_scroll = 0;
                    }
                }
                KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char(ch) if key.modifiers.is_empty() && screen.show_input => {
                    let mut input_buffer = code_agent_ui::InputBuffer::new();
                    input_buffer.push(ch);
                    return Ok(StartupFlowResult {
                        input_buffer,
                        provider: screen_provider,
                    });
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(StartupFlowResult {
        input_buffer: code_agent_ui::InputBuffer::new(),
        provider: selected_provider,
    })
}
