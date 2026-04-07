use crate::{
    auth_hint_for_provider, friendly_auth_source, persist_managed_login_env, user_ccrust_env_path,
    workspace_is_empty, ManagedLoginConfigState,
};
use ccrust_session::claude_config_home_dir;
use crossterm::event;

use crate::{
    apply_repl_header, is_paste_shortcut, read_text_from_clipboard, repl_status, status_with_detail,
};
use crate::{scroll_down, scroll_up};
use ccrust_ui::{
    draw_terminal as draw_tui, ChoiceListItem, ChoiceListState, PaneKind, RatatuiApp,
    TranscriptLine,
};
use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ccrust_ui::{CommandPaletteEntry, PanePreview, UiState};

use ccrust_core::SessionId;

use ccrust_providers::{
    compatibility_model_catalog, get_anthropic_auth_material, get_gemini_auth_status,
    get_gemini_base_url, get_gemini_completion_model, get_gemini_reasoning_model,
    get_openai_api_mode, get_openai_auth_status, get_openai_completion_model,
    get_openai_completion_think_level, get_openai_family_capabilities, get_openai_reasoning_model,
    get_openai_reasoning_think_level, get_openai_transport_mode, is_openai_provider,
    provider_descriptor, read_provider_auth_snapshot, ApiProvider, ModelCatalog, OpenAIApiMode,
    OpenAIAuthSource, OpenAITransportMode,
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
    ConfigFile,
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
    pub(crate) input_buffer: ccrust_ui::InputBuffer,
    pub(crate) provider: ApiProvider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenAICompatiblePreset {
    OpenAI,
    OpenRouter,
    Gemini,
    Custom,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LoginConfigDraft {
    provider: ApiProvider,
    anthropic_api_key: String,
    gemini_api_key: String,
    gemini_base_url: String,
    openai_api_key: String,
    openai_base_url: String,
    openai_api_mode: OpenAIApiMode,
    openai_transport: OpenAITransportMode,
    reasoning_model: String,
    completion_model: String,
    reasoning_model_think: String,
    completion_model_think: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoginOnboardingOutcome {
    pub(crate) provider: ApiProvider,
    pub(crate) config_path: PathBuf,
    pub(crate) env_values: BTreeMap<String, String>,
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

pub(crate) fn resolve_launch_provider(
    explicit: Option<&str>,
    preferences: &StartupPreferences,
    login_config: &ManagedLoginConfigState,
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
                source: if login_config.provider_from_file {
                    LaunchProviderSource::ConfigFile
                } else {
                    LaunchProviderSource::Env
                },
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
            configured: false,
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
        ApiProvider::Gemini => {
            let status = get_gemini_auth_status();
            if status.has_credentials {
                return (
                    true,
                    Some(
                        match status.source {
                            ccrust_providers::GeminiAuthSource::GeminiApiKey => "GEMINI_API_KEY",
                            ccrust_providers::GeminiAuthSource::GoogleApiKey => "GOOGLE_API_KEY",
                            ccrust_providers::GeminiAuthSource::OpenAiApiKey => "OPENAI_API_KEY",
                            ccrust_providers::GeminiAuthSource::None => "none",
                        }
                        .to_owned(),
                    ),
                );
            }
            let snapshot = read_provider_auth_snapshot(provider);
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
                "press Enter to continue setup".to_owned(),
            ],
        },
        choice_list: Some(provider_choice_list(provider)),
        provider_configured,
        show_input: false,
        prompt_helper: Some("Use ↑/↓ to choose a provider. Enter continues setup.".to_owned()),
        compact_banner: Some(if ready {
            "Provider is ready. Enter continues setup.".to_owned()
        } else if is_openai_provider(provider) {
            "Provider selected. Enter continues to login setup.".to_owned()
        } else {
            "Provider selected. Enter continues to login setup.".to_owned()
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
        live_runtime,
        auth_source,
        preferences,
    );
    if transcript_path.is_some() {
        return Vec::new();
    }
    if provider_configured {
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
            token_label: None,
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
            input_buffer: ccrust_ui::InputBuffer::new(),
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
                    let mut input_buffer = ccrust_ui::InputBuffer::new();
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
        input_buffer: ccrust_ui::InputBuffer::new(),
        provider: selected_provider,
    })
}

fn onboarding_status_line(
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    step_label: &str,
) -> String {
    status_with_detail(
        repl_status(provider, active_model, session_id),
        format!("login onboarding · {step_label}"),
    )
}

fn masked_input_buffer(input_buffer: &ccrust_ui::InputBuffer) -> ccrust_ui::InputBuffer {
    let mut masked = ccrust_ui::InputBuffer::new();
    masked.chars = vec!['*'; input_buffer.chars.len()];
    masked.cursor = input_buffer.cursor.min(masked.chars.len());
    masked
}

pub(crate) fn insert_onboarding_input_text(
    input_buffer: &mut ccrust_ui::InputBuffer,
    text: &str,
) -> bool {
    let mut inserted = false;
    for ch in text.chars().filter(|ch| !matches!(ch, '\r' | '\n')) {
        input_buffer.push(ch);
        inserted = true;
    }
    inserted
}

fn build_onboarding_screen(
    title: &str,
    body: Vec<String>,
    preview_title: &str,
    preview_lines: Vec<String>,
    choice_list: Option<ChoiceListState>,
    show_input: bool,
    prompt_helper: &str,
    compact_banner: Option<String>,
) -> StartupScreen {
    StartupScreen {
        title: title.to_owned(),
        body,
        preview: PanePreview {
            title: preview_title.to_owned(),
            lines: preview_lines,
        },
        choice_list,
        provider_configured: false,
        show_input,
        prompt_helper: Some(prompt_helper.to_owned()),
        compact_banner,
    }
}

fn draw_onboarding_state<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    step_label: &str,
    screen: &StartupScreen,
    input_buffer: Option<&ccrust_ui::InputBuffer>,
    secret_input: bool,
) -> Result<()> {
    let app = RatatuiApp::new(format!("{provider}  {active_model}"));
    let mut state = build_startup_ui_state(
        &app,
        provider,
        active_model,
        session_id,
        cwd,
        screen,
        0,
        1,
        0,
    );
    state.status_line = onboarding_status_line(provider, active_model, session_id, step_label);
    if let Some(buffer) = input_buffer {
        state.input_buffer = if secret_input {
            masked_input_buffer(buffer)
        } else {
            buffer.clone()
        };
    }
    draw_tui(terminal, &state)?;
    Ok(())
}

fn onboarding_choice_list(
    title: &str,
    subtitle: &str,
    selected: usize,
    items: Vec<ChoiceListItem>,
) -> ChoiceListState {
    ChoiceListState {
        title: title.to_owned(),
        subtitle: Some(subtitle.to_owned()),
        items,
        selected,
        empty_message: None,
    }
}

fn run_onboarding_choice_step<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    step_label: &str,
    title: &str,
    body: Vec<String>,
    preview_title: &str,
    preview_lines: Vec<String>,
    mut choice_list: ChoiceListState,
    compact_banner: Option<String>,
) -> Result<Option<usize>> {
    loop {
        let screen = build_onboarding_screen(
            title,
            body.clone(),
            preview_title,
            preview_lines.clone(),
            Some(choice_list.clone()),
            false,
            "Use ↑/↓ to choose. Enter accepts. Esc cancels.",
            compact_banner.clone(),
        );
        draw_onboarding_state(
            terminal,
            provider,
            active_model,
            session_id,
            cwd,
            step_label,
            &screen,
            None,
            false,
        )?;

        match event::read()? {
            Event::Resize(width, height) => {
                terminal.resize(Rect::new(0, 0, width, height))?;
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    choice_list.selected = choice_list.selected.saturating_sub(1);
                }
                MouseEventKind::ScrollDown => {
                    choice_list.selected =
                        (choice_list.selected + 1).min(choice_list.items.len().saturating_sub(1));
                }
                _ => {}
            },
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up | KeyCode::Left => {
                    choice_list.selected = choice_list.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Right => {
                    choice_list.selected =
                        (choice_list.selected + 1).min(choice_list.items.len().saturating_sub(1));
                }
                KeyCode::Home => choice_list.selected = 0,
                KeyCode::End => {
                    choice_list.selected = choice_list.items.len().saturating_sub(1);
                }
                KeyCode::Enter => return Ok(Some(choice_list.selected)),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn run_onboarding_input_step<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    step_label: &str,
    title: &str,
    body: Vec<String>,
    preview_title: &str,
    preview_lines: Vec<String>,
    initial_value: String,
    secret_input: bool,
    required: bool,
    validator: Option<fn(&str) -> bool>,
) -> Result<Option<String>> {
    let mut input_buffer = ccrust_ui::InputBuffer::new();
    input_buffer.replace(initial_value);
    let mut compact_banner = None;

    loop {
        let screen = build_onboarding_screen(
            title,
            body.clone(),
            preview_title,
            preview_lines.clone(),
            None,
            true,
            "Enter accepts the current value. Cmd/Ctrl+V pastes. Esc cancels onboarding.",
            compact_banner.clone(),
        );
        draw_onboarding_state(
            terminal,
            provider,
            active_model,
            session_id,
            cwd,
            step_label,
            &screen,
            Some(&input_buffer),
            secret_input,
        )?;

        match event::read()? {
            Event::Resize(width, height) => {
                terminal.resize(Rect::new(0, 0, width, height))?;
            }
            Event::Paste(text) => {
                if insert_onboarding_input_text(&mut input_buffer, &text) {
                    compact_banner = None;
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && is_paste_shortcut(&key) => {
                if let Some(text) = read_text_from_clipboard() {
                    if insert_onboarding_input_text(&mut input_buffer, &text) {
                        compact_banner = None;
                    }
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => {
                    let value = input_buffer.as_str();
                    let trimmed = value.trim();
                    if required && trimmed.is_empty() {
                        compact_banner = Some("A value is required for this field.".to_owned());
                        continue;
                    }
                    if let Some(validate) = validator {
                        if !trimmed.is_empty() && !validate(trimmed) {
                            compact_banner =
                                Some("Expected one of: low, medium, high, xhigh.".to_owned());
                            continue;
                        }
                    }
                    return Ok(Some(trimmed.to_owned()));
                }
                KeyCode::Backspace => {
                    input_buffer.pop();
                    compact_banner = None;
                }
                KeyCode::Left => {
                    input_buffer.cursor = input_buffer.cursor.saturating_sub(1);
                }
                KeyCode::Right => {
                    input_buffer.cursor = (input_buffer.cursor + 1).min(input_buffer.chars.len());
                }
                KeyCode::Home => input_buffer.cursor = 0,
                KeyCode::End => input_buffer.cursor = input_buffer.chars.len(),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    input_buffer.push(ch);
                    compact_banner = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn openai_compatible_preset_choice_list(selected: OpenAICompatiblePreset) -> ChoiceListState {
    let items = vec![
        ChoiceListItem {
            label: "OpenAI".to_owned(),
            detail: Some("Official OpenAI API".to_owned()),
            secondary: None,
        },
        ChoiceListItem {
            label: "OpenRouter".to_owned(),
            detail: Some("Preset base URL for OpenRouter".to_owned()),
            secondary: None,
        },
        ChoiceListItem {
            label: "Gemini".to_owned(),
            detail: Some("Google Gemini OpenAI-compatible endpoint".to_owned()),
            secondary: None,
        },
        ChoiceListItem {
            label: "Custom".to_owned(),
            detail: Some("Choose your own OpenAI-compatible base URL".to_owned()),
            secondary: None,
        },
    ];
    let selected = match selected {
        OpenAICompatiblePreset::OpenAI => 0,
        OpenAICompatiblePreset::OpenRouter => 1,
        OpenAICompatiblePreset::Gemini => 2,
        OpenAICompatiblePreset::Custom => 3,
    };
    onboarding_choice_list(
        "Choose preset",
        "Pick a prefilled OpenAI-compatible target.",
        selected,
        items,
    )
}

fn infer_openai_compatible_preset(base_url: &str) -> OpenAICompatiblePreset {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed == "https://api.openai.com/v1" {
        return OpenAICompatiblePreset::OpenAI;
    }
    if trimmed.contains("openrouter.ai") {
        return OpenAICompatiblePreset::OpenRouter;
    }
    if trimmed.contains("generativelanguage.googleapis.com") {
        return OpenAICompatiblePreset::Gemini;
    }
    OpenAICompatiblePreset::Custom
}

fn openai_compatible_preset_base_url(preset: OpenAICompatiblePreset) -> &'static str {
    match preset {
        OpenAICompatiblePreset::OpenAI => "https://api.openai.com/v1",
        OpenAICompatiblePreset::OpenRouter => "https://openrouter.ai/api/v1",
        OpenAICompatiblePreset::Gemini => {
            "https://generativelanguage.googleapis.com/v1beta/openai/"
        }
        OpenAICompatiblePreset::Custom => "",
    }
}

fn openai_compatible_think_level(value: &str) -> bool {
    matches!(value, "low" | "medium" | "high" | "xhigh")
}

fn openai_api_mode_choice_list(
    selected: OpenAIApiMode,
    allow_chat_completions: bool,
) -> (ChoiceListState, Vec<OpenAIApiMode>) {
    let mut modes = vec![OpenAIApiMode::Auto, OpenAIApiMode::Responses];
    if allow_chat_completions {
        modes.push(OpenAIApiMode::ChatCompletions);
    }

    let items = modes
        .iter()
        .map(|mode| match mode {
            OpenAIApiMode::Auto => ChoiceListItem {
                label: "Auto".to_owned(),
                detail: Some(
                    "Prefer Responses, then fall back to chat completions when needed".to_owned(),
                ),
                secondary: None,
            },
            OpenAIApiMode::Responses => ChoiceListItem {
                label: "Responses".to_owned(),
                detail: Some("Use the Responses API only".to_owned()),
                secondary: None,
            },
            OpenAIApiMode::ChatCompletions => ChoiceListItem {
                label: "Chat Completions".to_owned(),
                detail: Some("Use chat completions only".to_owned()),
                secondary: None,
            },
        })
        .collect::<Vec<_>>();
    let selected = modes
        .iter()
        .position(|mode| *mode == selected)
        .unwrap_or_default();

    (
        onboarding_choice_list(
            "Choose API mode",
            "Select which OpenAI-family API shape should be preferred.",
            selected,
            items,
        ),
        modes,
    )
}

fn openai_transport_choice_list(
    selected: OpenAITransportMode,
    chat_completions_only: bool,
    allow_websocket: bool,
    allow_sse: bool,
    allow_rest: bool,
) -> (ChoiceListState, Vec<OpenAITransportMode>) {
    let modes = if chat_completions_only {
        vec![OpenAITransportMode::Rest]
    } else {
        let mut modes = vec![OpenAITransportMode::Auto];
        if allow_websocket {
            modes.push(OpenAITransportMode::WebSocket);
        }
        if allow_sse {
            modes.push(OpenAITransportMode::Sse);
        }
        if allow_rest {
            modes.push(OpenAITransportMode::Rest);
        }
        modes
    };

    let items = modes
        .iter()
        .map(|mode| match mode {
            OpenAITransportMode::Auto => ChoiceListItem {
                label: "Auto".to_owned(),
                detail: Some("Try WebSocket, then SSE, then REST".to_owned()),
                secondary: None,
            },
            OpenAITransportMode::WebSocket => ChoiceListItem {
                label: "WebSocket".to_owned(),
                detail: Some("Use the Responses websocket transport".to_owned()),
                secondary: None,
            },
            OpenAITransportMode::Sse => ChoiceListItem {
                label: "SSE".to_owned(),
                detail: Some("Use streaming Responses over HTTP".to_owned()),
                secondary: None,
            },
            OpenAITransportMode::Rest => ChoiceListItem {
                label: "REST".to_owned(),
                detail: Some(if chat_completions_only {
                    "Chat completions use plain REST only".to_owned()
                } else {
                    "Use non-streaming Responses over HTTP".to_owned()
                }),
                secondary: None,
            },
        })
        .collect::<Vec<_>>();
    let selected = modes
        .iter()
        .position(|mode| *mode == selected)
        .unwrap_or_default();

    (
        onboarding_choice_list(
            "Choose transport",
            "Select how OpenAI-family requests should be transported.",
            selected,
            items,
        ),
        modes,
    )
}

fn run_openai_routing_onboarding_step<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    cwd: &Path,
    api_step_label: &str,
    transport_step_label: &str,
    draft: &mut LoginConfigDraft,
    base_url: Option<&str>,
) -> Result<bool> {
    let capabilities = get_openai_family_capabilities(provider, base_url);
    let (api_choice_list, api_modes) = openai_api_mode_choice_list(
        draft.openai_api_mode,
        capabilities.supports_chat_completions,
    );
    let api_choice = run_onboarding_choice_step(
        terminal,
        provider,
        active_model,
        session_id,
        cwd,
        api_step_label,
        "OpenAI API Mode",
        vec![
            "Responses is preferred when the provider supports it.".to_owned(),
            "Auto mode falls back to chat completions when Responses is unavailable.".to_owned(),
        ],
        "Preview",
        vec![
            format!("provider: {provider}"),
            format!("base_url: {}", base_url.unwrap_or("(default)")),
        ],
        api_choice_list,
        Some("Enter accepts the selection. Esc cancels onboarding.".to_owned()),
    )?;
    let Some(api_choice) = api_choice else {
        return Ok(false);
    };
    draft.openai_api_mode = api_modes[api_choice];

    let chat_completions_only = draft.openai_api_mode == OpenAIApiMode::ChatCompletions;
    let force_all_transports = draft.openai_api_mode == OpenAIApiMode::Responses;
    let (transport_choice_list, transport_modes) = openai_transport_choice_list(
        if chat_completions_only {
            OpenAITransportMode::Rest
        } else {
            draft.openai_transport
        },
        chat_completions_only,
        force_all_transports || capabilities.supports_websocket,
        force_all_transports || capabilities.supports_sse,
        chat_completions_only || force_all_transports || capabilities.supports_rest,
    );
    let transport_choice = run_onboarding_choice_step(
        terminal,
        provider,
        active_model,
        session_id,
        cwd,
        transport_step_label,
        "OpenAI Transport",
        vec![
            "Auto transport tries WebSocket first, then SSE, then plain REST.".to_owned(),
            if chat_completions_only {
                "Chat completions uses REST only.".to_owned()
            } else {
                "You can pin a transport when the endpoint requires it.".to_owned()
            },
        ],
        "Preview",
        vec![
            format!("api mode: {}", draft.openai_api_mode.as_str()),
            format!("base_url: {}", base_url.unwrap_or("(default)")),
        ],
        transport_choice_list,
        Some("Enter accepts the selection. Esc cancels onboarding.".to_owned()),
    )?;
    let Some(transport_choice) = transport_choice else {
        return Ok(false);
    };
    draft.openai_transport = transport_modes[transport_choice];

    Ok(true)
}

fn login_draft_from_environment(provider: ApiProvider) -> LoginConfigDraft {
    let mut draft = LoginConfigDraft {
        provider,
        anthropic_api_key: env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
        gemini_api_key: env::var("GEMINI_API_KEY")
            .or_else(|_| env::var("GOOGLE_API_KEY"))
            .or_else(|_| env::var("OPENAI_API_KEY"))
            .unwrap_or_default(),
        gemini_base_url: get_gemini_base_url(),
        openai_api_key: env::var("OPENAI_API_KEY").unwrap_or_default(),
        openai_base_url: env::var("OPENAI_BASE_URL").unwrap_or_default(),
        openai_api_mode: get_openai_api_mode(provider),
        openai_transport: get_openai_transport_mode(),
        reasoning_model: if provider == ApiProvider::Gemini {
            get_gemini_reasoning_model()
        } else {
            env::var("REASONING_MODEL").unwrap_or_else(|_| get_openai_reasoning_model())
        },
        completion_model: if provider == ApiProvider::Gemini {
            get_gemini_completion_model()
        } else {
            env::var("COMPLETION_MODEL").unwrap_or_else(|_| get_openai_completion_model())
        },
        reasoning_model_think: env::var("REASONING_MODEL_THINK")
            .unwrap_or_else(|_| get_openai_reasoning_think_level()),
        completion_model_think: env::var("COMPLETION_MODEL_THINK")
            .unwrap_or_else(|_| get_openai_completion_think_level()),
    };

    if provider == ApiProvider::OpenAICompatible {
        let preset = infer_openai_compatible_preset(&draft.openai_base_url);
        if draft.openai_base_url.trim().is_empty() {
            draft.openai_base_url = openai_compatible_preset_base_url(preset).to_owned();
        }
        if preset == OpenAICompatiblePreset::Gemini {
            if draft.reasoning_model.trim().is_empty() {
                draft.reasoning_model = "gemini-3.1-pro-preview".to_owned();
            }
            if draft.completion_model.trim().is_empty() {
                draft.completion_model = "gemini-3.1-pro-preview".to_owned();
            }
            if draft.reasoning_model_think.trim().is_empty() {
                draft.reasoning_model_think = "high".to_owned();
            }
            if draft.completion_model_think.trim().is_empty() {
                draft.completion_model_think = "medium".to_owned();
            }
        }
    }

    draft
}

fn apply_openai_compatible_preset_defaults(
    draft: &mut LoginConfigDraft,
    preset: OpenAICompatiblePreset,
) {
    draft.provider = ApiProvider::OpenAICompatible;
    draft.openai_base_url = openai_compatible_preset_base_url(preset).to_owned();
    if draft.reasoning_model.trim().is_empty() {
        draft.reasoning_model = if preset == OpenAICompatiblePreset::Gemini {
            "gemini-3.1-pro-preview".to_owned()
        } else {
            get_openai_reasoning_model()
        };
    }
    if draft.completion_model.trim().is_empty() {
        draft.completion_model = if preset == OpenAICompatiblePreset::Gemini {
            "gemini-3.1-pro-preview".to_owned()
        } else {
            get_openai_completion_model()
        };
    }
    if draft.reasoning_model_think.trim().is_empty() {
        draft.reasoning_model_think = if preset == OpenAICompatiblePreset::Gemini {
            "high".to_owned()
        } else {
            get_openai_reasoning_think_level()
        };
    }
    if draft.completion_model_think.trim().is_empty() {
        draft.completion_model_think = if preset == OpenAICompatiblePreset::Gemini {
            "medium".to_owned()
        } else {
            get_openai_completion_think_level()
        };
    }
}

fn managed_login_env_values(draft: &LoginConfigDraft) -> BTreeMap<String, String> {
    let mut values = BTreeMap::from([(
        "CLAUDE_CODE_API_PROVIDER".to_owned(),
        draft.provider.to_string(),
    )]);

    if is_openai_provider(draft.provider) {
        if draft.openai_api_mode != OpenAIApiMode::Auto {
            values.insert(
                "OPENAI_API_MODE".to_owned(),
                draft.openai_api_mode.as_str().to_owned(),
            );
        }
        if draft.openai_transport != OpenAITransportMode::Auto {
            values.insert(
                "OPENAI_TRANSPORT".to_owned(),
                draft.openai_transport.as_str().to_owned(),
            );
        }
    }

    match draft.provider {
        ApiProvider::FirstParty => {
            if !draft.anthropic_api_key.trim().is_empty() {
                values.insert(
                    "ANTHROPIC_API_KEY".to_owned(),
                    draft.anthropic_api_key.trim().to_owned(),
                );
            }
        }
        ApiProvider::Gemini => {
            for (key, value) in [
                ("GEMINI_API_KEY", draft.gemini_api_key.trim()),
                ("GEMINI_BASE_URL", draft.gemini_base_url.trim()),
                ("GEMINI_REASONING_MODEL", draft.reasoning_model.trim()),
                ("GEMINI_COMPLETION_MODEL", draft.completion_model.trim()),
                ("REASONING_MODEL_THINK", draft.reasoning_model_think.trim()),
                (
                    "COMPLETION_MODEL_THINK",
                    draft.completion_model_think.trim(),
                ),
            ] {
                if !value.is_empty() {
                    values.insert(key.to_owned(), value.to_owned());
                }
            }
        }
        ApiProvider::OpenAICompatible => {
            for (key, value) in [
                ("OPENAI_API_KEY", draft.openai_api_key.trim()),
                ("OPENAI_BASE_URL", draft.openai_base_url.trim()),
                ("REASONING_MODEL", draft.reasoning_model.trim()),
                ("COMPLETION_MODEL", draft.completion_model.trim()),
                ("REASONING_MODEL_THINK", draft.reasoning_model_think.trim()),
                (
                    "COMPLETION_MODEL_THINK",
                    draft.completion_model_think.trim(),
                ),
            ] {
                if !value.is_empty() {
                    values.insert(key.to_owned(), value.to_owned());
                }
            }
        }
        ApiProvider::ChatGPTCodex
        | ApiProvider::Bedrock
        | ApiProvider::Vertex
        | ApiProvider::Foundry => {}
    }

    values
}

fn onboarding_target_path_preview(cwd: &Path, tracked_path: Option<&Path>) -> String {
    tracked_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| user_ccrust_env_path())
        .display()
        .to_string()
        .replace(&cwd.display().to_string(), ".")
}

pub(crate) fn run_login_onboarding_flow<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    cwd: &Path,
    provider: ApiProvider,
    active_model: &str,
    session_id: SessionId,
    tracked_path: Option<&Path>,
) -> Result<Option<LoginOnboardingOutcome>> {
    let provider_index = ApiProvider::ALL
        .iter()
        .position(|candidate| *candidate == provider)
        .unwrap_or_default();
    let provider_choice = run_onboarding_choice_step(
        terminal,
        provider,
        active_model,
        session_id,
        cwd,
        "1/2",
        "Login Setup",
        vec![
            "Choose the provider to save into ccrust config.".to_owned(),
            format!(
                "Config path: {}",
                onboarding_target_path_preview(cwd, tracked_path)
            ),
        ],
        "Saved Config",
        vec![
            "ccrust writes login-related settings to .env.ccrust".to_owned(),
            "CLI flags and real environment variables still override the saved values.".to_owned(),
        ],
        provider_choice_list(ApiProvider::ALL[provider_index]),
        Some("Esc skips onboarding and opens the REPL.".to_owned()),
    )?;
    let Some(provider_choice) = provider_choice else {
        return Ok(None);
    };
    let selected_provider = ApiProvider::ALL[provider_choice];
    let mut draft = login_draft_from_environment(selected_provider);

    if selected_provider == ApiProvider::OpenAICompatible {
        let preset = infer_openai_compatible_preset(&draft.openai_base_url);
        let preset_choice = run_onboarding_choice_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "2/10",
            "OpenAI-Compatible Preset",
            vec![
                "Pick a preset base URL for the OpenAI-compatible provider.".to_owned(),
                "You can still edit the fields before saving.".to_owned(),
            ],
            "Preset Details",
            vec![
                "OpenAI uses the official OpenAI API.".to_owned(),
                "OpenRouter and Gemini prefill their OpenAI-compatible base URLs.".to_owned(),
            ],
            openai_compatible_preset_choice_list(preset),
            Some("Enter accepts the preset. Esc cancels onboarding.".to_owned()),
        )?;
        let Some(preset_choice) = preset_choice else {
            return Ok(None);
        };
        let preset = match preset_choice {
            0 => OpenAICompatiblePreset::OpenAI,
            1 => OpenAICompatiblePreset::OpenRouter,
            2 => OpenAICompatiblePreset::Gemini,
            _ => OpenAICompatiblePreset::Custom,
        };
        apply_openai_compatible_preset_defaults(&mut draft, preset);

        draft.openai_api_key = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "3/10",
            "API Key",
            vec!["Enter the API key for the selected OpenAI-compatible endpoint.".to_owned()],
            "Preview",
            vec![
                format!("provider: {}", selected_provider),
                format!("base_url: {}", draft.openai_base_url),
            ],
            draft.openai_api_key.clone(),
            true,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.openai_base_url = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "4/10",
            "Base URL",
            vec!["Edit the OpenAI-compatible base URL if needed.".to_owned()],
            "Preview",
            vec![format!("provider: {}", selected_provider)],
            draft.openai_base_url.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        let openai_base_url = draft.openai_base_url.clone();
        if !run_openai_routing_onboarding_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "5/10",
            "6/10",
            &mut draft,
            Some(openai_base_url.as_str()),
        )? {
            return Ok(None);
        }
        draft.reasoning_model = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "7/10",
            "Reasoning Model",
            vec!["Set the model used for thinking-enabled turns.".to_owned()],
            "Preview",
            vec![format!("base_url: {}", draft.openai_base_url)],
            draft.reasoning_model.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.completion_model = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "8/10",
            "Completion Model",
            vec!["Set the model used for standard turns and utility calls.".to_owned()],
            "Preview",
            vec![format!("reasoning model: {}", draft.reasoning_model)],
            draft.completion_model.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.reasoning_model_think = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "9/10",
            "Reasoning Think",
            vec![
                "Set the reasoning effort for the reasoning model.".to_owned(),
                "Use one of: low, medium, high, xhigh.".to_owned(),
            ],
            "Preview",
            vec![format!("completion model: {}", draft.completion_model)],
            draft.reasoning_model_think.clone(),
            false,
            true,
            Some(openai_compatible_think_level),
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.completion_model_think = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "10/10",
            "Completion Think",
            vec![
                "Set the reasoning effort for the completion model.".to_owned(),
                "Use one of: low, medium, high, xhigh.".to_owned(),
            ],
            "Preview",
            vec![format!("reasoning think: {}", draft.reasoning_model_think)],
            draft.completion_model_think.clone(),
            false,
            true,
            Some(openai_compatible_think_level),
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
    } else if selected_provider == ApiProvider::Gemini {
        draft.gemini_api_key = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "2/5",
            "Gemini API Key",
            vec![
                "Enter a Gemini Developer API key. GOOGLE_API_KEY also works, but onboarding saves it as GEMINI_API_KEY."
                    .to_owned(),
            ],
            "Preview",
            vec![format!("provider: {selected_provider}")],
            draft.gemini_api_key.clone(),
            true,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.gemini_base_url = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "3/5",
            "Gemini Base URL",
            vec![
                "Leave the default Gemini API base URL unless you need a proxy or custom endpoint."
                    .to_owned(),
            ],
            "Preview",
            vec![format!("api key source: GEMINI_API_KEY")],
            draft.gemini_base_url.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.reasoning_model = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "4/5",
            "Reasoning Model",
            vec!["Set the default Gemini model for main conversational turns.".to_owned()],
            "Preview",
            vec![format!("base_url: {}", draft.gemini_base_url)],
            draft.reasoning_model.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
        draft.completion_model = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "5/5",
            "Fast Model",
            vec![
                "Set the Gemini model used for fast mode and lightweight utility turns.".to_owned(),
            ],
            "Preview",
            vec![format!("reasoning model: {}", draft.reasoning_model)],
            draft.completion_model.clone(),
            false,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
    } else if selected_provider == ApiProvider::ChatGPTCodex {
        if !run_openai_routing_onboarding_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "2/3",
            "3/3",
            &mut draft,
            None,
        )? {
            return Ok(None);
        }
    } else if selected_provider == ApiProvider::FirstParty {
        draft.anthropic_api_key = match run_onboarding_input_step(
            terminal,
            selected_provider,
            active_model,
            session_id,
            cwd,
            "2/2",
            "Anthropic API Key",
            vec!["Enter the Anthropic API key to save into ccrust config.".to_owned()],
            "Preview",
            vec![format!("provider: {}", selected_provider)],
            draft.anthropic_api_key.clone(),
            true,
            true,
            None,
        )? {
            Some(value) => value,
            None => return Ok(None),
        };
    }

    let env_values = managed_login_env_values(&draft);
    let config_path = persist_managed_login_env(cwd, tracked_path, &env_values)?;
    Ok(Some(LoginOnboardingOutcome {
        provider: selected_provider,
        config_path,
        env_values,
    }))
}
