use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
use ratatui::widgets::{Block, Borders};

// Modifier bit constants for const-compatible theme presets.
const BOLD: u16 = Modifier::BOLD.bits();
const DIM: u16 = Modifier::DIM.bits();
const ITALIC: u16 = Modifier::ITALIC.bits();
const NONE: u16 = 0;

/// Animated accent color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentAnim {
    /// No animation; static accent color.
    None,
    /// Sine-wave brightness oscillation.
    Pulse,
    /// Rotate through hue values (rainbow).
    HueCycle,
    /// Shift between accent and a secondary color.
    ColorShift,
}

pub struct Theme {
    pub name: &'static str,
    pub sidebar_bg: Color,
    pub sidebar_text: Color,
    pub selection_bg: Color,
    pub selection_text: Color,
    pub main_bg: Color,
    pub alt_row_bg: Color,
    pub border: Color,
    pub footer_bg: Color,
    pub footer_text: Color,
    pub header_text: Color,
    pub dim_text: Color,
    pub error_text: Color,
    pub success_text: Color,
    pub progress_bar: Color,
    pub progress_bg: Color,
    // Styled borders
    pub border_type: BorderType,
    // Text styling modifiers (stored as raw u16 bits for const compatibility)
    pub header_modifier: u16,
    pub sidebar_modifier: u16,
    pub dim_modifier: u16,
    pub footer_modifier: u16,
    // Animated accent
    pub accent_anim: AccentAnim,
    pub accent_secondary: Color,
}

impl Theme {
    const fn modifier(bits: u16) -> Modifier {
        Modifier::from_bits_truncate(bits)
    }

    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.selection_bg)
            .fg(self.selection_text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn sidebar_item(&self) -> Style {
        Style::default()
            .fg(self.sidebar_text)
            .add_modifier(Self::modifier(self.sidebar_modifier))
    }

    pub fn sidebar_item_selected(&self) -> Style {
        self.selected()
    }

    pub fn header(&self) -> Style {
        Style::default()
            .fg(self.header_text)
            .add_modifier(Self::modifier(self.header_modifier))
    }

    pub fn dim(&self) -> Style {
        Style::default()
            .fg(self.dim_text)
            .add_modifier(Self::modifier(self.dim_modifier))
    }

    pub fn footer(&self) -> Style {
        Style::default()
            .bg(self.footer_bg)
            .fg(self.footer_text)
            .add_modifier(Self::modifier(self.footer_modifier))
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.error_text)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.success_text)
    }

    /// Returns a pre-configured Block with the theme's border style and type.
    pub fn block(&self) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_style(self.border())
            .border_type(self.border_type)
    }

    /// The most "active" accent color for the theme, used for pulse animations.
    pub fn accent_color(&self) -> Color {
        self.progress_bar
    }
}

// -- Built-in theme presets --

pub const THEMES: &[Theme] = &[
    ITUNES_2004,
    GRUVBOX_DARK,
    GRUVBOX_LIGHT,
    EVERFOREST_DARK,
    EVERFOREST_LIGHT,
    MIAMI_NIGHTS,
    IBM_MAINFRAME,
    WINDOWS_95,
    SYSTEM_7,
    BIOS,
    RED_SANDS,
    NEWPORT_LIGHTS,
];

pub const ITUNES_2004: Theme = Theme {
    name: "iTunes 2004",
    sidebar_bg: Color::Rgb(225, 228, 232),
    sidebar_text: Color::Rgb(30, 30, 30),
    selection_bg: Color::Rgb(56, 117, 215),
    selection_text: Color::White,
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(237, 243, 254),
    border: Color::Rgb(180, 180, 180),
    footer_bg: Color::Rgb(200, 203, 207),
    footer_text: Color::Rgb(40, 40, 40),
    header_text: Color::Rgb(80, 80, 80),
    dim_text: Color::Rgb(140, 140, 140),
    error_text: Color::Rgb(200, 50, 50),
    success_text: Color::Rgb(50, 160, 50),
    progress_bar: Color::Rgb(56, 117, 215),
    progress_bg: Color::Rgb(220, 220, 220),
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(56, 117, 215),
};

pub const GRUVBOX_DARK: Theme = Theme {
    name: "Gruvbox Dark",
    sidebar_bg: Color::Rgb(50, 48, 47),
    sidebar_text: Color::Rgb(235, 219, 178),
    selection_bg: Color::Rgb(214, 93, 14),
    selection_text: Color::Rgb(40, 40, 40),
    main_bg: Color::Rgb(40, 40, 40),
    alt_row_bg: Color::Rgb(60, 56, 54),
    border: Color::Rgb(102, 92, 84),
    footer_bg: Color::Rgb(50, 48, 47),
    footer_text: Color::Rgb(189, 174, 147),
    header_text: Color::Rgb(168, 153, 132),
    dim_text: Color::Rgb(124, 111, 100),
    error_text: Color::Rgb(204, 36, 29),
    success_text: Color::Rgb(152, 151, 26),
    progress_bar: Color::Rgb(214, 93, 14),
    progress_bg: Color::Rgb(60, 56, 54),
    border_type: BorderType::Plain,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(214, 93, 14),
};

pub const GRUVBOX_LIGHT: Theme = Theme {
    name: "Gruvbox Light",
    sidebar_bg: Color::Rgb(242, 229, 188),
    sidebar_text: Color::Rgb(60, 56, 54),
    selection_bg: Color::Rgb(175, 58, 3),
    selection_text: Color::Rgb(251, 241, 199),
    main_bg: Color::Rgb(251, 241, 199),
    alt_row_bg: Color::Rgb(235, 219, 178),
    border: Color::Rgb(168, 153, 132),
    footer_bg: Color::Rgb(213, 196, 161),
    footer_text: Color::Rgb(60, 56, 54),
    header_text: Color::Rgb(102, 92, 84),
    dim_text: Color::Rgb(146, 131, 116),
    error_text: Color::Rgb(157, 0, 6),
    success_text: Color::Rgb(121, 116, 14),
    progress_bar: Color::Rgb(175, 58, 3),
    progress_bg: Color::Rgb(213, 196, 161),
    border_type: BorderType::Plain,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::None,
    accent_secondary: Color::Rgb(175, 58, 3),
};

pub const EVERFOREST_DARK: Theme = Theme {
    name: "Everforest Dark",
    sidebar_bg: Color::Rgb(45, 53, 59),
    sidebar_text: Color::Rgb(211, 198, 170),
    selection_bg: Color::Rgb(167, 192, 128),
    selection_text: Color::Rgb(45, 53, 59),
    main_bg: Color::Rgb(39, 46, 51),
    alt_row_bg: Color::Rgb(52, 61, 68),
    border: Color::Rgb(78, 90, 97),
    footer_bg: Color::Rgb(45, 53, 59),
    footer_text: Color::Rgb(167, 192, 128),
    header_text: Color::Rgb(135, 144, 130),
    dim_text: Color::Rgb(90, 101, 99),
    error_text: Color::Rgb(230, 126, 128),
    success_text: Color::Rgb(167, 192, 128),
    progress_bar: Color::Rgb(167, 192, 128),
    progress_bg: Color::Rgb(52, 61, 68),
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: ITALIC,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(167, 192, 128),
};

pub const EVERFOREST_LIGHT: Theme = Theme {
    name: "Everforest Light",
    sidebar_bg: Color::Rgb(239, 239, 225),
    sidebar_text: Color::Rgb(92, 106, 114),
    selection_bg: Color::Rgb(141, 161, 1),
    selection_text: Color::Rgb(253, 246, 227),
    main_bg: Color::Rgb(253, 246, 227),
    alt_row_bg: Color::Rgb(239, 239, 225),
    border: Color::Rgb(186, 189, 175),
    footer_bg: Color::Rgb(221, 222, 208),
    footer_text: Color::Rgb(92, 106, 114),
    header_text: Color::Rgb(130, 140, 130),
    dim_text: Color::Rgb(160, 166, 152),
    error_text: Color::Rgb(241, 104, 100),
    success_text: Color::Rgb(141, 161, 1),
    progress_bar: Color::Rgb(141, 161, 1),
    progress_bg: Color::Rgb(221, 222, 208),
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: ITALIC,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::None,
    accent_secondary: Color::Rgb(141, 161, 1),
};

pub const MIAMI_NIGHTS: Theme = Theme {
    name: "Miami Nights",
    sidebar_bg: Color::Rgb(22, 22, 38),
    sidebar_text: Color::Rgb(226, 183, 234),
    selection_bg: Color::Rgb(233, 69, 96),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(26, 26, 46),
    alt_row_bg: Color::Rgb(32, 32, 58),
    border: Color::Rgb(80, 60, 120),
    footer_bg: Color::Rgb(15, 52, 96),
    footer_text: Color::Rgb(120, 220, 232),
    header_text: Color::Rgb(120, 220, 232),
    dim_text: Color::Rgb(90, 80, 130),
    error_text: Color::Rgb(255, 80, 80),
    success_text: Color::Rgb(80, 255, 180),
    progress_bar: Color::Rgb(233, 69, 96),
    progress_bg: Color::Rgb(32, 32, 58),
    border_type: BorderType::Thick,
    header_modifier: BOLD | ITALIC,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::HueCycle,
    accent_secondary: Color::Rgb(120, 220, 232),
};

pub const IBM_MAINFRAME: Theme = Theme {
    name: "IBM Mainframe",
    sidebar_bg: Color::Rgb(0, 0, 0),
    sidebar_text: Color::Rgb(51, 255, 51),
    selection_bg: Color::Rgb(51, 255, 51),
    selection_text: Color::Rgb(0, 0, 0),
    main_bg: Color::Rgb(0, 0, 0),
    alt_row_bg: Color::Rgb(10, 20, 10),
    border: Color::Rgb(0, 130, 0),
    footer_bg: Color::Rgb(0, 40, 0),
    footer_text: Color::Rgb(51, 255, 51),
    header_text: Color::Rgb(0, 200, 0),
    dim_text: Color::Rgb(0, 100, 0),
    error_text: Color::Rgb(255, 80, 80),
    success_text: Color::Rgb(51, 255, 51),
    progress_bar: Color::Rgb(51, 255, 51),
    progress_bg: Color::Rgb(0, 40, 0),
    border_type: BorderType::Double,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(51, 255, 51),
};

pub const WINDOWS_95: Theme = Theme {
    name: "Windows 95",
    sidebar_bg: Color::Rgb(192, 192, 192),
    sidebar_text: Color::Rgb(0, 0, 0),
    selection_bg: Color::Rgb(0, 0, 128),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(224, 224, 224),
    border: Color::Rgb(128, 128, 128),
    footer_bg: Color::Rgb(192, 192, 192),
    footer_text: Color::Rgb(0, 0, 0),
    header_text: Color::Rgb(0, 0, 128),
    dim_text: Color::Rgb(128, 128, 128),
    error_text: Color::Rgb(255, 0, 0),
    success_text: Color::Rgb(0, 128, 0),
    progress_bar: Color::Rgb(0, 0, 128),
    progress_bg: Color::Rgb(192, 192, 192),
    border_type: BorderType::QuadrantOutside,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::None,
    accent_secondary: Color::Rgb(0, 0, 128),
};

pub const SYSTEM_7: Theme = Theme {
    name: "System 7",
    sidebar_bg: Color::Rgb(221, 221, 221),
    sidebar_text: Color::Rgb(0, 0, 0),
    selection_bg: Color::Rgb(0, 0, 0),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(238, 238, 238),
    border: Color::Rgb(0, 0, 0),
    footer_bg: Color::Rgb(204, 204, 204),
    footer_text: Color::Rgb(0, 0, 0),
    header_text: Color::Rgb(68, 68, 68),
    dim_text: Color::Rgb(136, 136, 136),
    error_text: Color::Rgb(200, 0, 0),
    success_text: Color::Rgb(0, 128, 0),
    progress_bar: Color::Rgb(0, 0, 0),
    progress_bg: Color::Rgb(204, 204, 204),
    border_type: BorderType::Plain,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::None,
    accent_secondary: Color::Rgb(0, 0, 0),
};

pub const BIOS: Theme = Theme {
    name: "BIOS",
    sidebar_bg: Color::Rgb(0, 0, 170),
    sidebar_text: Color::Rgb(170, 170, 170),
    selection_bg: Color::Rgb(170, 170, 170),
    selection_text: Color::Rgb(0, 0, 170),
    main_bg: Color::Rgb(0, 0, 170),
    alt_row_bg: Color::Rgb(0, 0, 140),
    border: Color::Rgb(85, 85, 255),
    footer_bg: Color::Rgb(0, 0, 100),
    footer_text: Color::Rgb(255, 255, 85),
    header_text: Color::Rgb(255, 255, 255),
    dim_text: Color::Rgb(85, 85, 255),
    error_text: Color::Rgb(255, 85, 85),
    success_text: Color::Rgb(85, 255, 85),
    progress_bar: Color::Rgb(255, 255, 85),
    progress_bg: Color::Rgb(0, 0, 100),
    border_type: BorderType::Double,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::ColorShift,
    accent_secondary: Color::Rgb(85, 255, 85),
};

pub const RED_SANDS: Theme = Theme {
    name: "Red Sands",
    sidebar_bg: Color::Rgb(88, 26, 16),
    sidebar_text: Color::Rgb(212, 196, 168),
    selection_bg: Color::Rgb(210, 163, 58),
    selection_text: Color::Rgb(52, 12, 8),
    main_bg: Color::Rgb(122, 37, 24),
    alt_row_bg: Color::Rgb(105, 32, 20),
    border: Color::Rgb(160, 90, 60),
    footer_bg: Color::Rgb(72, 20, 12),
    footer_text: Color::Rgb(212, 196, 168),
    header_text: Color::Rgb(230, 200, 160),
    dim_text: Color::Rgb(140, 90, 70),
    error_text: Color::Rgb(255, 100, 80),
    success_text: Color::Rgb(180, 210, 90),
    progress_bar: Color::Rgb(210, 163, 58),
    progress_bg: Color::Rgb(88, 26, 16),
    border_type: BorderType::Plain,
    header_modifier: BOLD | ITALIC,
    sidebar_modifier: ITALIC,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::ColorShift,
    accent_secondary: Color::Rgb(200, 80, 40),
};

pub const NEWPORT_LIGHTS: Theme = Theme {
    name: "Newport Lights",
    sidebar_bg: Color::Rgb(0, 106, 95),        // deep teal
    sidebar_text: Color::Rgb(230, 240, 235),    // off-white
    selection_bg: Color::Rgb(255, 255, 255),    // white
    selection_text: Color::Rgb(0, 80, 70),      // dark teal
    main_bg: Color::Rgb(0, 130, 115),           // seafoam green
    alt_row_bg: Color::Rgb(0, 118, 105),        // slightly darker seafoam
    border: Color::Rgb(180, 220, 210),          // pale mint
    footer_bg: Color::Rgb(0, 90, 80),           // dark teal
    footer_text: Color::Rgb(230, 240, 235),     // off-white
    header_text: Color::Rgb(255, 255, 255),     // white
    dim_text: Color::Rgb(100, 170, 155),        // muted mint
    error_text: Color::Rgb(255, 100, 80),       // warm red
    success_text: Color::Rgb(180, 255, 200),    // bright mint
    progress_bar: Color::Rgb(255, 255, 255),    // white
    progress_bg: Color::Rgb(0, 80, 70),         // dark teal
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: NONE,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(255, 255, 255),
};

/// Find a theme index by name (case-insensitive). Returns 0 (default) if not found.
pub fn find_theme_index(name: &str) -> usize {
    THEMES
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(name))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_theme_by_name() {
        assert_eq!(find_theme_index("Gruvbox Dark"), 1);
        assert_eq!(find_theme_index("gruvbox dark"), 1);
        assert_eq!(find_theme_index("nonexistent"), 0);
    }

    #[test]
    fn all_themes_have_unique_names() {
        for (i, t) in THEMES.iter().enumerate() {
            for (j, u) in THEMES.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        t.name.to_lowercase(),
                        u.name.to_lowercase(),
                        "Duplicate theme name: {}",
                        t.name
                    );
                }
            }
        }
    }

    #[test]
    fn block_helper_returns_block() {
        for t in THEMES {
            let _ = t.block();
        }
    }

    #[test]
    fn modifier_round_trip() {
        assert_eq!(Theme::modifier(BOLD), Modifier::BOLD);
        assert_eq!(Theme::modifier(DIM), Modifier::DIM);
        assert_eq!(Theme::modifier(BOLD | ITALIC), Modifier::BOLD | Modifier::ITALIC);
        assert_eq!(Theme::modifier(NONE), Modifier::empty());
    }
}
