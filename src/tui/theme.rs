use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
use ratatui::widgets::{Block, Borders};
use throbber_widgets_tui::symbols::throbber::{
    Set, ASCII, BLACK_CIRCLE, BRAILLE_EIGHT, BRAILLE_ONE, BRAILLE_SIX, BRAILLE_SIX_DOUBLE, OGHAM_A,
    OGHAM_B, QUADRANT_BLOCK, VERTICAL_BLOCK, WHITE_CIRCLE, WHITE_SQUARE,
};

use super::anim::{
    PlayerSkin, SKIN_BIOS, SKIN_EVERFOREST_DARK, SKIN_EVERFOREST_LIGHT, SKIN_GRUVBOX_DARK,
    SKIN_GRUVBOX_LIGHT, SKIN_IBM, SKIN_ITUNES, SKIN_NEWPORT, SKIN_RED_SANDS, SKIN_SYSTEM7,
    SKIN_TOKYO_NIGHT, SKIN_WIN95,
};

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
    // Now-playing animation skin and sidebar spinner set.
    pub player_skin: &'static PlayerSkin,
    pub spinner_set: &'static Set,
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
    TOKYO_NIGHT,
    IBM_MAINFRAME,
    AMBER_CRT,
    WINDOWS_95,
    SYSTEM_7,
    BIOS,
    RED_SANDS,
    NEWPORT_LIGHTS,
    NEXTSTEP,
    WINAMP_CLASSIC,
    ZUNE_ORIGINAL,
];

pub const ITUNES_2004: Theme = Theme {
    name: "iTunes 2004",
    sidebar_bg: Color::Rgb(225, 228, 232),
    sidebar_text: Color::Rgb(30, 30, 30),
    selection_bg: Color::Rgb(56, 117, 215),
    selection_text: Color::White,
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(222, 232, 250),
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
    player_skin: &SKIN_ITUNES,
    spinner_set: &BRAILLE_EIGHT,
};

pub const GRUVBOX_DARK: Theme = Theme {
    name: "Gruvbox Dark",
    sidebar_bg: Color::Rgb(50, 48, 47),
    sidebar_text: Color::Rgb(235, 219, 178),
    selection_bg: Color::Rgb(214, 93, 14),
    selection_text: Color::Rgb(40, 40, 40),
    main_bg: Color::Rgb(40, 40, 40),
    alt_row_bg: Color::Rgb(68, 64, 60),
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
    player_skin: &SKIN_GRUVBOX_DARK,
    spinner_set: &BRAILLE_SIX_DOUBLE,
};

pub const GRUVBOX_LIGHT: Theme = Theme {
    name: "Gruvbox Light",
    sidebar_bg: Color::Rgb(242, 229, 188),
    sidebar_text: Color::Rgb(60, 56, 54),
    selection_bg: Color::Rgb(175, 58, 3),
    selection_text: Color::Rgb(251, 241, 199),
    main_bg: Color::Rgb(251, 241, 199),
    alt_row_bg: Color::Rgb(226, 208, 162),
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
    player_skin: &SKIN_GRUVBOX_LIGHT,
    spinner_set: &BRAILLE_SIX,
};

pub const EVERFOREST_DARK: Theme = Theme {
    name: "Everforest Dark",
    sidebar_bg: Color::Rgb(45, 53, 59),
    sidebar_text: Color::Rgb(211, 198, 170),
    selection_bg: Color::Rgb(167, 192, 128),
    selection_text: Color::Rgb(45, 53, 59),
    main_bg: Color::Rgb(39, 46, 51),
    alt_row_bg: Color::Rgb(58, 70, 78),
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
    player_skin: &SKIN_EVERFOREST_DARK,
    spinner_set: &OGHAM_A,
};

pub const EVERFOREST_LIGHT: Theme = Theme {
    name: "Everforest Light",
    sidebar_bg: Color::Rgb(239, 239, 225),
    sidebar_text: Color::Rgb(92, 106, 114),
    selection_bg: Color::Rgb(141, 161, 1),
    selection_text: Color::Rgb(253, 246, 227),
    main_bg: Color::Rgb(253, 246, 227),
    alt_row_bg: Color::Rgb(230, 226, 210),
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
    player_skin: &SKIN_EVERFOREST_LIGHT,
    spinner_set: &OGHAM_B,
};

pub const TOKYO_NIGHT: Theme = Theme {
    name: "Tokyo Night",
    sidebar_bg: Color::Rgb(22, 22, 30),
    sidebar_text: Color::Rgb(192, 202, 245),
    selection_bg: Color::Rgb(54, 74, 130),
    selection_text: Color::Rgb(192, 202, 245),
    main_bg: Color::Rgb(26, 27, 38),
    alt_row_bg: Color::Rgb(36, 40, 59),
    border: Color::Rgb(65, 72, 104),
    footer_bg: Color::Rgb(22, 22, 30),
    footer_text: Color::Rgb(122, 162, 247),
    header_text: Color::Rgb(187, 154, 247),
    dim_text: Color::Rgb(114, 125, 163),
    error_text: Color::Rgb(247, 118, 142),
    success_text: Color::Rgb(158, 206, 106),
    progress_bar: Color::Rgb(122, 162, 247),
    progress_bg: Color::Rgb(36, 40, 59),
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::HueCycle,
    accent_secondary: Color::Rgb(125, 207, 255),
    player_skin: &SKIN_TOKYO_NIGHT,
    spinner_set: &BLACK_CIRCLE,
};

pub const IBM_MAINFRAME: Theme = Theme {
    name: "IBM Mainframe",
    sidebar_bg: Color::Rgb(0, 0, 0),
    sidebar_text: Color::Rgb(40, 200, 80),
    selection_bg: Color::Rgb(40, 200, 80),
    selection_text: Color::Rgb(0, 0, 0),
    main_bg: Color::Rgb(0, 0, 0),
    alt_row_bg: Color::Rgb(14, 26, 14),
    border: Color::Rgb(0, 110, 40),
    footer_bg: Color::Rgb(0, 30, 10),
    footer_text: Color::Rgb(40, 200, 80),
    header_text: Color::Rgb(0, 170, 60),
    dim_text: Color::Rgb(0, 85, 30),
    error_text: Color::Rgb(220, 70, 70),
    success_text: Color::Rgb(40, 200, 80),
    progress_bar: Color::Rgb(40, 200, 80),
    progress_bg: Color::Rgb(0, 30, 10),
    border_type: BorderType::Double,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(40, 200, 80),
    player_skin: &SKIN_IBM,
    spinner_set: &VERTICAL_BLOCK,
};

pub const AMBER_CRT: Theme = Theme {
    name: "Amber CRT",
    sidebar_bg: Color::Rgb(0, 0, 0),
    sidebar_text: Color::Rgb(255, 176, 0),
    selection_bg: Color::Rgb(255, 176, 0),
    selection_text: Color::Rgb(20, 10, 0),
    main_bg: Color::Rgb(0, 0, 0),
    alt_row_bg: Color::Rgb(30, 18, 0),
    border: Color::Rgb(170, 100, 0),
    footer_bg: Color::Rgb(40, 22, 0),
    footer_text: Color::Rgb(255, 176, 0),
    header_text: Color::Rgb(255, 200, 60),
    dim_text: Color::Rgb(140, 80, 0),
    error_text: Color::Rgb(255, 90, 60),
    success_text: Color::Rgb(255, 210, 80),
    progress_bar: Color::Rgb(255, 176, 0),
    progress_bg: Color::Rgb(40, 22, 0),
    border_type: BorderType::Double,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(255, 200, 60),
    player_skin: &SKIN_IBM,
    spinner_set: &VERTICAL_BLOCK,
};

pub const WINDOWS_95: Theme = Theme {
    name: "Windows 95",
    sidebar_bg: Color::Rgb(192, 192, 192),
    sidebar_text: Color::Rgb(0, 0, 0),
    selection_bg: Color::Rgb(0, 0, 128),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(210, 210, 210),
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
    player_skin: &SKIN_WIN95,
    spinner_set: &WHITE_SQUARE,
};

pub const SYSTEM_7: Theme = Theme {
    name: "System 7",
    sidebar_bg: Color::Rgb(204, 204, 204),
    sidebar_text: Color::Rgb(0, 0, 0),
    selection_bg: Color::Rgb(0, 0, 0),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(255, 255, 255),
    alt_row_bg: Color::Rgb(238, 238, 238),
    border: Color::Rgb(0, 0, 0),
    footer_bg: Color::Rgb(204, 204, 204),
    footer_text: Color::Rgb(0, 0, 0),
    header_text: Color::Rgb(51, 51, 51),
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
    player_skin: &SKIN_SYSTEM7,
    spinner_set: &QUADRANT_BLOCK,
};

pub const BIOS: Theme = Theme {
    name: "BIOS",
    sidebar_bg: Color::Rgb(0, 0, 170),
    sidebar_text: Color::Rgb(170, 170, 170),
    selection_bg: Color::Rgb(170, 170, 170),
    selection_text: Color::Rgb(0, 0, 170),
    main_bg: Color::Rgb(0, 0, 170),
    alt_row_bg: Color::Rgb(0, 0, 120),
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
    player_skin: &SKIN_BIOS,
    spinner_set: &ASCII,
};

pub const RED_SANDS: Theme = Theme {
    name: "Red Sands",
    sidebar_bg: Color::Rgb(88, 26, 16),
    sidebar_text: Color::Rgb(212, 196, 168),
    selection_bg: Color::Rgb(210, 163, 58),
    selection_text: Color::Rgb(52, 12, 8),
    main_bg: Color::Rgb(122, 37, 24),
    alt_row_bg: Color::Rgb(90, 26, 16),
    border: Color::Rgb(160, 90, 60),
    footer_bg: Color::Rgb(72, 20, 12),
    footer_text: Color::Rgb(212, 196, 168),
    header_text: Color::Rgb(230, 200, 160),
    dim_text: Color::Rgb(212, 176, 136),
    error_text: Color::Rgb(255, 100, 80),
    success_text: Color::Rgb(180, 210, 90),
    progress_bar: Color::Rgb(210, 163, 58),
    progress_bg: Color::Rgb(88, 26, 16),
    border_type: BorderType::Plain,
    header_modifier: BOLD | ITALIC,
    sidebar_modifier: ITALIC,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::ColorShift,
    accent_secondary: Color::Rgb(200, 80, 40),
    player_skin: &SKIN_RED_SANDS,
    spinner_set: &BRAILLE_ONE,
};

pub const NEWPORT_LIGHTS: Theme = Theme {
    name: "Newport Lights",
    sidebar_bg: Color::Rgb(0, 106, 95),      // deep teal
    sidebar_text: Color::Rgb(230, 240, 235), // off-white
    selection_bg: Color::Rgb(255, 255, 255), // white
    selection_text: Color::Rgb(0, 80, 70),   // dark teal
    main_bg: Color::Rgb(0, 130, 115),        // seafoam green
    alt_row_bg: Color::Rgb(0, 108, 95),      // darker seafoam
    border: Color::Rgb(180, 220, 210),       // pale mint
    footer_bg: Color::Rgb(0, 90, 80),        // dark teal
    footer_text: Color::Rgb(230, 240, 235),  // off-white
    header_text: Color::Rgb(255, 255, 255),  // white
    dim_text: Color::Rgb(220, 240, 230),     // near-white mint (readable on seafoam)
    error_text: Color::Rgb(255, 100, 80),    // warm red
    success_text: Color::Rgb(180, 255, 200), // bright mint
    progress_bar: Color::Rgb(255, 255, 255), // white
    progress_bg: Color::Rgb(0, 80, 70),      // dark teal
    border_type: BorderType::Rounded,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::Pulse,
    accent_secondary: Color::Rgb(255, 255, 255),
    player_skin: &SKIN_NEWPORT,
    spinner_set: &WHITE_CIRCLE,
};

pub const NEXTSTEP: Theme = Theme {
    name: "NeXTSTEP",
    sidebar_bg: Color::Rgb(43, 43, 43),
    sidebar_text: Color::Rgb(230, 230, 230),
    selection_bg: Color::Rgb(96, 112, 140),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(170, 170, 170),
    alt_row_bg: Color::Rgb(153, 153, 153),
    border: Color::Rgb(30, 30, 30),
    footer_bg: Color::Rgb(43, 43, 43),
    footer_text: Color::Rgb(230, 230, 230),
    header_text: Color::Rgb(20, 20, 20),
    dim_text: Color::Rgb(85, 85, 85),
    error_text: Color::Rgb(170, 40, 40),
    success_text: Color::Rgb(40, 110, 60),
    progress_bar: Color::Rgb(96, 112, 140),
    progress_bg: Color::Rgb(136, 136, 136),
    border_type: BorderType::Thick,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: NONE,
    footer_modifier: NONE,
    accent_anim: AccentAnim::None,
    accent_secondary: Color::Rgb(96, 112, 140),
    player_skin: &SKIN_SYSTEM7,
    spinner_set: &BRAILLE_SIX,
};

pub const WINAMP_CLASSIC: Theme = Theme {
    name: "WinAmp Classic",
    sidebar_bg: Color::Rgb(26, 26, 26),
    sidebar_text: Color::Rgb(0, 255, 0),
    selection_bg: Color::Rgb(0, 255, 0),
    selection_text: Color::Rgb(0, 0, 0),
    main_bg: Color::Rgb(20, 20, 20),
    alt_row_bg: Color::Rgb(32, 32, 32),
    border: Color::Rgb(74, 74, 74),
    footer_bg: Color::Rgb(10, 10, 10),
    footer_text: Color::Rgb(0, 255, 0),
    header_text: Color::Rgb(0, 255, 0),
    dim_text: Color::Rgb(0, 120, 0),
    error_text: Color::Rgb(255, 60, 60),
    success_text: Color::Rgb(255, 220, 0),
    progress_bar: Color::Rgb(0, 255, 0),
    progress_bg: Color::Rgb(20, 20, 20),
    border_type: BorderType::Plain,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::ColorShift,
    accent_secondary: Color::Rgb(255, 220, 0),
    player_skin: &SKIN_IBM,
    spinner_set: &VERTICAL_BLOCK,
};

pub const ZUNE_ORIGINAL: Theme = Theme {
    name: "Zune Original",
    sidebar_bg: Color::Rgb(10, 10, 10),
    sidebar_text: Color::Rgb(235, 235, 235),
    selection_bg: Color::Rgb(232, 0, 164),
    selection_text: Color::Rgb(255, 255, 255),
    main_bg: Color::Rgb(0, 0, 0),
    alt_row_bg: Color::Rgb(21, 21, 21),
    border: Color::Rgb(42, 42, 42),
    footer_bg: Color::Rgb(20, 20, 20),
    footer_text: Color::Rgb(235, 141, 0),
    header_text: Color::Rgb(232, 0, 164),
    dim_text: Color::Rgb(110, 110, 110),
    error_text: Color::Rgb(255, 80, 80),
    success_text: Color::Rgb(166, 226, 46),
    progress_bar: Color::Rgb(232, 0, 164),
    progress_bg: Color::Rgb(26, 26, 26),
    border_type: BorderType::Plain,
    header_modifier: BOLD,
    sidebar_modifier: NONE,
    dim_modifier: DIM,
    footer_modifier: BOLD,
    accent_anim: AccentAnim::ColorShift,
    accent_secondary: Color::Rgb(235, 141, 0),
    player_skin: &SKIN_TOKYO_NIGHT,
    spinner_set: &BLACK_CIRCLE,
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
    fn red_sands_is_not_newport_skin() {
        // Regression: THEMES grew and index-keyed skin lookup drifted, so Red Sands
        // rendered the Newport cigarette art. Skin now lives on the Theme struct —
        // this test guards the pairing.
        let red_sands = &THEMES[find_theme_index("Red Sands")];
        let art: String = (red_sands.player_skin.art_fn)(true, 0).concat();
        assert!(
            !art.contains("NEWPORT"),
            "Red Sands skin leaked Newport art: {}",
            art
        );
    }

    #[test]
    fn newport_lights_uses_newport_skin() {
        let newport = &THEMES[find_theme_index("Newport Lights")];
        let art: String = (newport.player_skin.art_fn)(true, 0).concat();
        assert!(
            art.contains("NEWPORT"),
            "Newport Lights skin missing expected art: {}",
            art
        );
    }

    #[test]
    fn every_theme_has_nonempty_skin_and_spinner() {
        for t in THEMES {
            assert!(
                !t.player_skin.play.is_empty(),
                "{} has empty play glyph",
                t.name
            );
            assert!(
                !(t.player_skin.art_fn)(true, 0).is_empty(),
                "{} has empty art",
                t.name
            );
            assert!(
                !t.spinner_set.symbols.is_empty(),
                "{} has empty spinner set",
                t.name
            );
        }
    }

    #[test]
    fn modifier_round_trip() {
        assert_eq!(Theme::modifier(BOLD), Modifier::BOLD);
        assert_eq!(Theme::modifier(DIM), Modifier::DIM);
        assert_eq!(
            Theme::modifier(BOLD | ITALIC),
            Modifier::BOLD | Modifier::ITALIC
        );
        assert_eq!(Theme::modifier(NONE), Modifier::empty());
    }
}
