#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiTheme {
    #[default]
    Dark,
    Light,
    DarkDaltonized,
    LightDaltonized,
    DarkAnsi,
    LightAnsi,
}

std::thread_local! {
    static UI_THEME_OVERRIDE: std::cell::Cell<Option<UiTheme>> = const { std::cell::Cell::new(None) };
}

pub fn set_runtime_ui_theme(setting: Option<&str>) -> UiTheme {
    let theme = UiTheme::from_setting(setting);
    UiTheme::set_current(theme);
    theme
}

impl UiTheme {
    pub fn current() -> Self {
        UI_THEME_OVERRIDE.with(|slot| slot.get()).unwrap_or_default()
    }

    pub fn set_current(theme: Self) {
        UI_THEME_OVERRIDE.with(|slot| slot.set(Some(theme)));
    }

    pub fn from_setting(setting: Option<&str>) -> Self {
        match setting.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("auto") => Self::auto(),
            Some(value) => Self::from_name(value).unwrap_or_default(),
            None => Self::default(),
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "dark-daltonized" => Some(Self::DarkDaltonized),
            "light-daltonized" => Some(Self::LightDaltonized),
            "dark-ansi" => Some(Self::DarkAnsi),
            "light-ansi" => Some(Self::LightAnsi),
            _ => None,
        }
    }

    fn auto() -> Self {
        if terminal_background_is_light() {
            Self::Light
        } else {
            Self::Dark
        }
    }

    fn is_light(self) -> bool {
        matches!(self, Self::Light | Self::LightDaltonized | Self::LightAnsi)
    }

    fn is_ansi(self) -> bool {
        matches!(self, Self::DarkAnsi | Self::LightAnsi)
    }

    fn is_daltonized(self) -> bool {
        matches!(self, Self::DarkDaltonized | Self::LightDaltonized)
    }

    pub fn info_color(self) -> Color {
        if self.is_light() || self.is_daltonized() {
            Color::Blue
        } else {
            Color::Cyan
        }
    }

    pub fn warning_color(self) -> Color {
        if self.is_light() && !self.is_ansi() {
            Color::Rgb(150, 108, 30)
        } else {
            Color::Yellow
        }
    }

    pub fn error_color(self) -> Color {
        if self.is_light() && !self.is_ansi() {
            Color::Rgb(171, 43, 63)
        } else {
            Color::Red
        }
    }

    pub fn success_color(self) -> Color {
        match self {
            Self::DarkDaltonized => Color::Cyan,
            Self::LightDaltonized if !self.is_ansi() => Color::Rgb(0, 102, 102),
            Self::LightDaltonized => Color::Cyan,
            _ => Color::Green,
        }
    }

    pub fn task_color(self) -> Color {
        if self.is_light() && !self.is_ansi() {
            Color::Rgb(135, 0, 255)
        } else {
            Color::Magenta
        }
    }

    pub fn role_color(self, role: &str) -> Color {
        match role {
            "user" | "command" => self.info_color(),
            "assistant" | "command_output" => self.success_color(),
            "tool" => self.warning_color(),
            "task" | "setup" => self.task_color(),
            _ => self.info_color(),
        }
    }

    pub fn muted_color(self) -> Color {
        Color::DarkGray
    }

    pub fn search_background_color(self) -> Color {
        if self.is_light() {
            if self.is_ansi() {
                Color::Cyan
            } else {
                Color::Rgb(180, 213, 255)
            }
        } else {
            Color::DarkGray
        }
    }

    pub fn search_foreground_color(self) -> Color {
        if self.is_light() {
            Color::Black
        } else {
            Color::White
        }
    }

    pub fn accent_background_color(self) -> Color {
        self.info_color()
    }

    pub fn accent_foreground_color(self) -> Color {
        if self.is_light() {
            Color::White
        } else {
            Color::Black
        }
    }

    pub fn selection_background_color(self) -> Color {
        if self.is_light() {
            self.search_background_color()
        } else {
            Color::Yellow
        }
    }

    pub fn selection_foreground_color(self) -> Color {
        Color::Black
    }

    pub fn muted_style(self) -> Style {
        Style::default().fg(self.muted_color())
    }

    pub fn title_style(self) -> Style {
        Style::default()
            .fg(self.info_color())
            .add_modifier(Modifier::BOLD)
    }

    pub fn warning_style(self) -> Style {
        Style::default().fg(self.warning_color())
    }

    pub fn warning_bold_style(self) -> Style {
        self.warning_style().add_modifier(Modifier::BOLD)
    }

    pub fn accent_highlight_style(self) -> Style {
        Style::default()
            .fg(self.accent_foreground_color())
            .bg(self.accent_background_color())
            .add_modifier(Modifier::BOLD)
    }

    pub fn accent_detail_style(self) -> Style {
        Style::default()
            .fg(self.accent_foreground_color())
            .bg(self.accent_background_color())
    }

    pub fn search_highlight_style(self) -> Style {
        Style::default()
            .fg(self.search_foreground_color())
            .bg(self.search_background_color())
            .add_modifier(Modifier::BOLD)
    }

    pub fn selection_highlight_style(self) -> Style {
        Style::default()
            .fg(self.selection_foreground_color())
            .bg(self.selection_background_color())
    }

    pub fn cursor_style(self) -> Style {
        if self.is_light() {
            Style::default().fg(Color::White).bg(Color::Black)
        } else {
            Style::default().fg(Color::Black).bg(Color::White)
        }
    }
}

fn terminal_background_is_light() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| {
            value
                .rsplit(';')
                .find_map(|segment| segment.trim().parse::<u16>().ok())
        })
        .is_some_and(|background| (7..=15).contains(&background))
}