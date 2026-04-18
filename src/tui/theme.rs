use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
use ratatui::widgets::{Block, Borders};
use throbber_widgets_tui::symbols::throbber::{
    Set, ASCII, BLACK_CIRCLE, BRAILLE_EIGHT, BRAILLE_ONE, BRAILLE_SIX, BRAILLE_SIX_DOUBLE, OGHAM_A,
    OGHAM_B, QUADRANT_BLOCK, VERTICAL_BLOCK, WHITE_SQUARE,
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
    // 8 Braille frames spin smoothly in every terminal font we care about;
    // the old WHITE_CIRCLE set (◷◶◵◴) had only 4 frames and its glyphs
    // rendered inconsistently in some fonts, showing up as a skipped frame.
    spinner_set: &BRAILLE_EIGHT,
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
    // Warm chocolate matching the original Zune 30 brown hardware finish.
    main_bg: Color::Rgb(92, 51, 23),
    alt_row_bg: Color::Rgb(77, 43, 19),
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

/// Look up a theme by name (case-insensitive). Falls back to the default theme
/// (`all_themes()[0]`) if no match is found. Searches the combined built-in +
/// user-defined registry.
pub fn theme_by_name(name: &str) -> &'static Theme {
    all_themes()
        .iter()
        .copied()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| all_themes()[0])
}

/// Position of `t` within the combined theme list. Only called when opening
/// the theme picker, so the linear scan cost is negligible. Pointer equality
/// would be tempting but isn't reliable across uses of the `THEMES` const.
pub fn theme_position(t: &Theme) -> usize {
    all_themes()
        .iter()
        .position(|x| x.name == t.name)
        .unwrap_or(0)
}

// -- Combined registry: built-ins + user-defined themes from config --

static ALL_THEMES: std::sync::OnceLock<Vec<&'static Theme>> = std::sync::OnceLock::new();
static BUILTIN_REFS: std::sync::OnceLock<Vec<&'static Theme>> = std::sync::OnceLock::new();

fn builtin_refs() -> &'static [&'static Theme] {
    BUILTIN_REFS
        .get_or_init(|| THEMES.iter().collect())
        .as_slice()
}

/// Returns the combined theme list (built-ins + user-defined). Falls back to
/// just the built-ins if [`init_themes`] has not been called (e.g. in unit
/// tests that don't go through the TUI entry point).
pub fn all_themes() -> &'static [&'static Theme] {
    ALL_THEMES
        .get()
        .map(|v| v.as_slice())
        .unwrap_or_else(builtin_refs)
}

/// Merge the user-defined `themes` from config into the global registry.
/// Idempotent (subsequent calls are ignored). User themes whose `base` is
/// missing, whose name collides with a built-in, or whose field strings fail
/// to parse are logged to stderr and skipped.
pub fn init_themes(user_themes: &std::collections::BTreeMap<String, crate::config::UserTheme>) {
    if ALL_THEMES.get().is_some() {
        return;
    }
    let mut combined: Vec<&'static Theme> = THEMES.iter().collect();
    for (name, ut) in user_themes {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            eprintln!("zytunes: skipping user theme with empty name");
            continue;
        }
        if THEMES.iter().any(|t| t.name.eq_ignore_ascii_case(trimmed)) {
            eprintln!(
                "zytunes: skipping user theme '{trimmed}' — name collides with a built-in theme"
            );
            continue;
        }
        match build_user_theme(trimmed, ut) {
            Ok(t) => combined.push(Box::leak(Box::new(t))),
            Err(e) => eprintln!("zytunes: skipping user theme '{trimmed}': {e}"),
        }
    }
    let _ = ALL_THEMES.set(combined);
}

/// Parse a hex color string (`#rrggbb` or `#rgb`, case-insensitive). Returns
/// `None` for any other format so callers can produce a pointed error message.
pub fn parse_hex_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        3 => {
            let mut bytes = [0u8; 3];
            for (i, ch) in hex.chars().enumerate() {
                let v = ch.to_digit(16)? as u8;
                bytes[i] = (v << 4) | v;
            }
            Some(Color::Rgb(bytes[0], bytes[1], bytes[2]))
        }
        _ => None,
    }
}

fn parse_color_opt(s: Option<&String>, fallback: Color, field: &str) -> Result<Color, String> {
    match s {
        Some(v) => parse_hex_color(v)
            .ok_or_else(|| format!("invalid hex color for '{field}': {v:?} (expected #rrggbb)")),
        None => Ok(fallback),
    }
}

fn parse_border_type(s: Option<&String>, fallback: BorderType) -> Result<BorderType, String> {
    let Some(v) = s else { return Ok(fallback) };
    match v.trim().to_ascii_lowercase().as_str() {
        "plain" => Ok(BorderType::Plain),
        "rounded" => Ok(BorderType::Rounded),
        "double" => Ok(BorderType::Double),
        "thick" => Ok(BorderType::Thick),
        "quadrantoutside" | "quadrant_outside" => Ok(BorderType::QuadrantOutside),
        "quadrantinside" | "quadrant_inside" => Ok(BorderType::QuadrantInside),
        _ => Err(format!(
            "invalid border_type {v:?} (expected plain|rounded|double|thick|quadrantoutside|quadrantinside)"
        )),
    }
}

fn parse_modifier(s: Option<&String>, fallback: u16, field: &str) -> Result<u16, String> {
    let Some(v) = s else { return Ok(fallback) };
    let mut bits: u16 = 0;
    for part in v.split(['+', '|', ',']) {
        let part = part.trim();
        if part.is_empty() || part.eq_ignore_ascii_case("none") {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "bold" => bits |= BOLD,
            "dim" => bits |= DIM,
            "italic" => bits |= ITALIC,
            other => {
                return Err(format!(
                    "invalid {field} token {other:?} (expected bold|dim|italic|none)"
                ))
            }
        }
    }
    Ok(bits)
}

fn parse_accent_anim(s: Option<&String>, fallback: AccentAnim) -> Result<AccentAnim, String> {
    let Some(v) = s else { return Ok(fallback) };
    match v.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(AccentAnim::None),
        "pulse" => Ok(AccentAnim::Pulse),
        "huecycle" | "hue_cycle" | "hue-cycle" => Ok(AccentAnim::HueCycle),
        "colorshift" | "color_shift" | "color-shift" => Ok(AccentAnim::ColorShift),
        _ => Err(format!(
            "invalid accent_anim {v:?} (expected none|pulse|huecycle|colorshift)"
        )),
    }
}

/// Build a runtime `Theme` from a user config entry. Errors surface as human
/// readable strings for the stderr log in [`init_themes`].
///
/// The resulting `Theme` leaks its `name` into `'static` storage; callers that
/// keep the returned value alive beyond the process lifetime must account for
/// that. In practice we only call this from `init_themes`, which leaks the
/// whole `Theme` too.
pub fn build_user_theme(name: &str, ut: &crate::config::UserTheme) -> Result<Theme, String> {
    const DEFAULT_BASE: &str = "iTunes 2004";
    let base_name = ut.base.as_deref().unwrap_or(DEFAULT_BASE);
    let base = THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(base_name))
        .ok_or_else(|| format!("base theme {base_name:?} is not a built-in"))?;

    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());

    Ok(Theme {
        name: static_name,
        sidebar_bg: parse_color_opt(ut.sidebar_bg.as_ref(), base.sidebar_bg, "sidebar_bg")?,
        sidebar_text: parse_color_opt(ut.sidebar_text.as_ref(), base.sidebar_text, "sidebar_text")?,
        selection_bg: parse_color_opt(ut.selection_bg.as_ref(), base.selection_bg, "selection_bg")?,
        selection_text: parse_color_opt(
            ut.selection_text.as_ref(),
            base.selection_text,
            "selection_text",
        )?,
        main_bg: parse_color_opt(ut.main_bg.as_ref(), base.main_bg, "main_bg")?,
        alt_row_bg: parse_color_opt(ut.alt_row_bg.as_ref(), base.alt_row_bg, "alt_row_bg")?,
        border: parse_color_opt(ut.border.as_ref(), base.border, "border")?,
        footer_bg: parse_color_opt(ut.footer_bg.as_ref(), base.footer_bg, "footer_bg")?,
        footer_text: parse_color_opt(ut.footer_text.as_ref(), base.footer_text, "footer_text")?,
        header_text: parse_color_opt(ut.header_text.as_ref(), base.header_text, "header_text")?,
        dim_text: parse_color_opt(ut.dim_text.as_ref(), base.dim_text, "dim_text")?,
        error_text: parse_color_opt(ut.error_text.as_ref(), base.error_text, "error_text")?,
        success_text: parse_color_opt(ut.success_text.as_ref(), base.success_text, "success_text")?,
        progress_bar: parse_color_opt(ut.progress_bar.as_ref(), base.progress_bar, "progress_bar")?,
        progress_bg: parse_color_opt(ut.progress_bg.as_ref(), base.progress_bg, "progress_bg")?,
        border_type: parse_border_type(ut.border_type.as_ref(), base.border_type)?,
        header_modifier: parse_modifier(
            ut.header_modifier.as_ref(),
            base.header_modifier,
            "header_modifier",
        )?,
        sidebar_modifier: parse_modifier(
            ut.sidebar_modifier.as_ref(),
            base.sidebar_modifier,
            "sidebar_modifier",
        )?,
        dim_modifier: parse_modifier(ut.dim_modifier.as_ref(), base.dim_modifier, "dim_modifier")?,
        footer_modifier: parse_modifier(
            ut.footer_modifier.as_ref(),
            base.footer_modifier,
            "footer_modifier",
        )?,
        accent_anim: parse_accent_anim(ut.accent_anim.as_ref(), base.accent_anim)?,
        accent_secondary: parse_color_opt(
            ut.accent_secondary.as_ref(),
            base.accent_secondary,
            "accent_secondary",
        )?,
        player_skin: base.player_skin,
        spinner_set: base.spinner_set,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_theme_by_name() {
        assert_eq!(theme_by_name("Gruvbox Dark").name, "Gruvbox Dark");
        assert_eq!(theme_by_name("gruvbox dark").name, "Gruvbox Dark");
        assert_eq!(theme_by_name("nonexistent").name, THEMES[0].name);
    }

    #[test]
    fn theme_position_round_trips() {
        for (i, t) in THEMES.iter().enumerate() {
            assert_eq!(theme_position(t), i);
        }
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
        let red_sands = theme_by_name("Red Sands");
        let art: String = (red_sands.player_skin.art_fn)(true, 0).concat();
        assert!(
            !art.contains("NEWPORT"),
            "Red Sands skin leaked Newport art: {}",
            art
        );
    }

    #[test]
    fn newport_lights_uses_newport_skin() {
        let newport = theme_by_name("Newport Lights");
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

    #[test]
    fn parse_hex_color_accepts_six_digit() {
        assert_eq!(
            parse_hex_color("#ff00aa"),
            Some(Color::Rgb(0xff, 0x00, 0xaa))
        );
        assert_eq!(parse_hex_color("#FFFFFF"), Some(Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn parse_hex_color_accepts_short_form() {
        // "#abc" expands to "#aabbcc".
        assert_eq!(parse_hex_color("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn parse_hex_color_rejects_malformed() {
        assert!(parse_hex_color("ff00aa").is_none(), "missing #");
        assert!(parse_hex_color("#ffgg00").is_none(), "non-hex digit");
        assert!(parse_hex_color("#ff00").is_none(), "wrong length");
        assert!(parse_hex_color("").is_none(), "empty");
    }

    #[test]
    fn build_user_theme_inherits_from_base() {
        let ut = crate::config::UserTheme {
            base: Some("Gruvbox Dark".into()),
            selection_bg: Some("#ff00aa".into()),
            ..Default::default()
        };
        let base = theme_by_name("Gruvbox Dark");
        let built = build_user_theme("Custom", &ut).expect("build succeeds");
        assert_eq!(built.name, "Custom");
        assert_eq!(built.selection_bg, Color::Rgb(0xff, 0x00, 0xaa));
        // Untouched fields inherit from the base verbatim.
        assert_eq!(built.sidebar_bg, base.sidebar_bg);
        assert_eq!(built.border_type, base.border_type);
        assert_eq!(built.accent_anim, base.accent_anim);
    }

    #[test]
    fn build_user_theme_defaults_base_to_itunes() {
        let ut = crate::config::UserTheme::default();
        let built = build_user_theme("Blank", &ut).expect("default base works");
        let itunes = theme_by_name("iTunes 2004");
        assert_eq!(built.sidebar_bg, itunes.sidebar_bg);
        assert_eq!(built.progress_bar, itunes.progress_bar);
    }

    #[test]
    fn build_user_theme_rejects_unknown_base() {
        let ut = crate::config::UserTheme {
            base: Some("Not A Real Theme".into()),
            ..Default::default()
        };
        assert!(build_user_theme("Broken", &ut).is_err());
    }

    #[test]
    fn build_user_theme_rejects_bad_color() {
        let ut = crate::config::UserTheme {
            selection_bg: Some("not-a-color".into()),
            ..Default::default()
        };
        let err = match build_user_theme("Broken", &ut) {
            Err(e) => e,
            Ok(_) => panic!("bad color should fail"),
        };
        assert!(err.contains("selection_bg"), "error mentions field: {err}");
    }

    #[test]
    fn parse_modifier_accepts_combined_tokens() {
        assert_eq!(
            parse_modifier(Some(&"bold+italic".to_string()), 0, "x").unwrap(),
            BOLD | ITALIC
        );
        assert_eq!(
            parse_modifier(Some(&"Bold | Dim".to_string()), 0, "x").unwrap(),
            BOLD | DIM
        );
        assert_eq!(
            parse_modifier(Some(&"none".to_string()), BOLD, "x").unwrap(),
            0,
            "explicit 'none' overrides the base"
        );
    }

    #[test]
    fn parse_modifier_rejects_garbage() {
        let err = parse_modifier(Some(&"bold+weird".to_string()), 0, "header").unwrap_err();
        assert!(err.contains("header"), "error includes field: {err}");
    }

    #[test]
    fn parse_border_type_round_trips_variants() {
        for (s, expected) in [
            ("plain", BorderType::Plain),
            ("rounded", BorderType::Rounded),
            ("double", BorderType::Double),
            ("thick", BorderType::Thick),
            ("quadrantoutside", BorderType::QuadrantOutside),
        ] {
            assert_eq!(
                parse_border_type(Some(&s.to_string()), BorderType::Plain).unwrap(),
                expected,
                "variant {s}"
            );
        }
        assert!(parse_border_type(Some(&"wavy".to_string()), BorderType::Plain).is_err());
    }

    #[test]
    fn parse_accent_anim_variants() {
        for (s, expected) in [
            ("none", AccentAnim::None),
            ("pulse", AccentAnim::Pulse),
            ("huecycle", AccentAnim::HueCycle),
            ("hue_cycle", AccentAnim::HueCycle),
            ("colorshift", AccentAnim::ColorShift),
        ] {
            assert_eq!(
                parse_accent_anim(Some(&s.to_string()), AccentAnim::None).unwrap(),
                expected,
                "variant {s}"
            );
        }
        assert!(parse_accent_anim(Some(&"strobe".to_string()), AccentAnim::None).is_err());
    }
}
