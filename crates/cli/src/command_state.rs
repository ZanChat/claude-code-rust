use anyhow::Result;
use ccrust_providers::{get_openai_completion_model, ApiProvider};
use ccrust_session::claude_config_home_dir;
use ccrust_ui::set_runtime_ui_theme;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const IOS_APP_URL: &str = "https://apps.apple.com/app/claude-by-anthropic/id6473753684";
pub(crate) const ANDROID_APP_URL: &str =
    "https://play.google.com/store/apps/details?id=com.anthropic.claude";
pub(crate) const CHROME_EXTENSION_URL: &str = "https://claude.ai/chrome";
pub(crate) const CHROME_PERMISSIONS_URL: &str = "https://clau.de/chrome/permissions";
pub(crate) const CHROME_RECONNECT_URL: &str = "https://clau.de/chrome/reconnect";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThemePreset {
    pub(crate) value: &'static str,
    pub(crate) label: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) const THEME_PRESETS: [ThemePreset; 7] = [
    ThemePreset {
        value: "auto",
        label: "Auto (match terminal)",
        description: "Follow your terminal palette automatically.",
    },
    ThemePreset {
        value: "dark",
        label: "Dark mode",
        description: "Use the default dark Claude Code palette.",
    },
    ThemePreset {
        value: "light",
        label: "Light mode",
        description: "Use the default light Claude Code palette.",
    },
    ThemePreset {
        value: "dark-daltonized",
        label: "Dark mode (colorblind-friendly)",
        description: "Dark palette adjusted for colorblind accessibility.",
    },
    ThemePreset {
        value: "light-daltonized",
        label: "Light mode (colorblind-friendly)",
        description: "Light palette adjusted for colorblind accessibility.",
    },
    ThemePreset {
        value: "dark-ansi",
        label: "Dark mode (ANSI colors only)",
        description: "Dark palette restricted to ANSI-safe colors.",
    },
    ThemePreset {
        value: "light-ansi",
        label: "Light mode (ANSI colors only)",
        description: "Light palette restricted to ANSI-safe colors.",
    },
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandSettings {
    #[serde(default)]
    pub(crate) theme: Option<String>,
    #[serde(default)]
    pub(crate) fast_mode: bool,
    #[serde(default)]
    pub(crate) advisor_model: Option<String>,
    #[serde(default)]
    pub(crate) chrome_default_enabled: bool,
}

pub(crate) fn command_settings_path() -> PathBuf {
    claude_config_home_dir()
        .join("ccrust")
        .join("command-settings.json")
}

pub(crate) fn load_command_settings() -> CommandSettings {
    let path = command_settings_path();
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CommandSettings>(&raw).ok())
        .unwrap_or_default()
}

pub(crate) fn apply_saved_ui_theme_preference() {
    let settings = load_command_settings();
    set_runtime_ui_theme(settings.theme.as_deref());
}

pub(crate) fn apply_ui_theme_preference(theme: Option<&str>) {
    set_runtime_ui_theme(theme);
}

pub(crate) fn save_command_settings(settings: &CommandSettings) -> Result<()> {
    let path = command_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(settings)?)?;
    Ok(())
}

pub(crate) fn update_command_settings(
    update: impl FnOnce(&mut CommandSettings),
) -> Result<CommandSettings> {
    let mut settings = load_command_settings();
    update(&mut settings);
    save_command_settings(&settings)?;
    Ok(settings)
}

pub(crate) fn normalize_theme_preset(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    THEME_PRESETS
        .iter()
        .find(|preset| preset.value == normalized)
        .map(|preset| preset.value)
}

pub(crate) fn theme_preset(value: &str) -> Option<&'static ThemePreset> {
    let normalized = value.trim().to_ascii_lowercase();
    THEME_PRESETS
        .iter()
        .find(|preset| preset.value == normalized)
}

pub(crate) fn preferred_model_for_provider(
    provider: ApiProvider,
    settings: &CommandSettings,
) -> Option<String> {
    if settings.fast_mode
        && matches!(
            provider,
            ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
        )
    {
        return Some(get_openai_completion_model());
    }

    None
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionMetadata {
    #[serde(default)]
    pub(crate) custom_title: Option<String>,
    #[serde(default)]
    pub(crate) agent_name: Option<String>,
    #[serde(default)]
    pub(crate) tag: Option<String>,
}

impl SessionMetadata {
    pub(crate) fn display_title(&self) -> Option<&str> {
        self.custom_title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.agent_name
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
    }

    fn is_empty(&self) -> bool {
        self.display_title().is_none()
            && self
                .tag
                .as_deref()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
    }
}

pub(crate) fn session_metadata_path(transcript_path: &Path) -> PathBuf {
    transcript_path.with_extension("meta.json")
}

pub(crate) fn load_session_metadata_for_path(transcript_path: &Path) -> SessionMetadata {
    let path = session_metadata_path(transcript_path);
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<SessionMetadata>(&raw).ok())
        .unwrap_or_default()
}

pub(crate) fn save_session_metadata_for_path(
    transcript_path: &Path,
    metadata: &SessionMetadata,
) -> Result<()> {
    let path = session_metadata_path(transcript_path);
    if metadata.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(metadata)?)?;
    Ok(())
}

pub(crate) fn update_session_metadata_for_path(
    transcript_path: &Path,
    update: impl FnOnce(&mut SessionMetadata),
) -> Result<SessionMetadata> {
    let mut metadata = load_session_metadata_for_path(transcript_path);
    update(&mut metadata);
    save_session_metadata_for_path(transcript_path, &metadata)?;
    Ok(metadata)
}

pub(crate) fn delete_session_metadata_for_path(transcript_path: &Path) -> Result<bool> {
    let path = session_metadata_path(transcript_path);
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

pub(crate) fn normalize_session_tag(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('#')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}
