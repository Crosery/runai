use ratatui::prelude::Color;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    pub fn toggle(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }
}

/// All colors used in the TUI, grouped by semantic role.
#[derive(Clone, Copy)]
pub struct Theme {
    // Brand
    pub brand: Color,
    pub version: Color,

    // Tabs
    pub tab_active: Color,
    pub tab_inactive: Color,

    // Status bar
    pub status_skills: Color,
    pub status_mcps: Color,
    pub status_dim: Color,

    // List items
    pub item_enabled: Color,
    pub item_disabled: Color,
    pub item_name: Color,
    pub item_desc: Color,
    pub item_selected_bg: Color,
    pub item_kind: Color,
    pub item_kind_mcp: Color,

    // MCP status tags
    pub tag_enabled: Color,
    pub tag_warning: Color,

    // Borders and backgrounds
    pub border: Color,
    pub bg: Color,
    pub dialog_bg: Color,

    // Text
    pub text: Color,
    pub text_dim: Color,
    pub text_highlight: Color,

    // Help keys
    pub help_key: Color,
    pub help_text: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            brand: Color::Rgb(232, 149, 74),
            version: Color::Rgb(120, 120, 140),
            tab_active: Color::Rgb(56, 164, 252),
            tab_inactive: Color::Gray,
            status_skills: Color::Rgb(52, 211, 153),
            status_mcps: Color::Rgb(129, 140, 248),
            status_dim: Color::DarkGray,
            item_enabled: Color::Rgb(52, 211, 153),
            item_disabled: Color::DarkGray,
            item_name: Color::White,
            item_desc: Color::Gray,
            item_selected_bg: Color::Rgb(25, 25, 35),
            item_kind: Color::Rgb(232, 149, 74),
            item_kind_mcp: Color::Rgb(129, 140, 248),
            tag_enabled: Color::Rgb(52, 211, 153),
            tag_warning: Color::Rgb(251, 191, 36),
            border: Color::Rgb(40, 40, 50),
            bg: Color::Reset,
            dialog_bg: Color::Rgb(25, 25, 35),
            text: Color::White,
            text_dim: Color::DarkGray,
            text_highlight: Color::Rgb(56, 164, 252),
            help_key: Color::Rgb(232, 149, 74),
            help_text: Color::DarkGray,
        }
    }

    pub fn light() -> Self {
        Self {
            brand: Color::Rgb(180, 100, 30),
            version: Color::Rgb(120, 120, 140),
            tab_active: Color::Rgb(30, 110, 200),
            tab_inactive: Color::Rgb(120, 120, 120),
            status_skills: Color::Rgb(20, 150, 80),
            status_mcps: Color::Rgb(90, 90, 200),
            status_dim: Color::Rgb(120, 120, 120),
            item_enabled: Color::Rgb(20, 150, 80),
            item_disabled: Color::Rgb(160, 160, 160),
            item_name: Color::Black,
            item_desc: Color::Rgb(80, 80, 80),
            item_selected_bg: Color::Rgb(220, 225, 235),
            item_kind: Color::Rgb(180, 100, 30),
            item_kind_mcp: Color::Rgb(90, 90, 200),
            tag_enabled: Color::Rgb(20, 150, 80),
            tag_warning: Color::Rgb(200, 150, 0),
            border: Color::Rgb(200, 200, 210),
            bg: Color::Reset,
            dialog_bg: Color::Rgb(240, 240, 245),
            text: Color::Black,
            text_dim: Color::Rgb(120, 120, 120),
            text_highlight: Color::Rgb(30, 110, 200),
            help_key: Color::Rgb(180, 100, 30),
            help_text: Color::Rgb(120, 120, 120),
        }
    }

    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_toggle_switches_mode() {
        assert_eq!(ThemeMode::Dark.toggle(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggle(), ThemeMode::Dark);
    }

    #[test]
    fn dark_and_light_themes_have_distinct_colors() {
        let dark = Theme::dark();
        let light = Theme::light();
        // Key colors should differ between themes
        assert_ne!(format!("{:?}", dark.text), format!("{:?}", light.text));
        assert_ne!(
            format!("{:?}", dark.item_selected_bg),
            format!("{:?}", light.item_selected_bg)
        );
        assert_ne!(format!("{:?}", dark.border), format!("{:?}", light.border));
    }
}
