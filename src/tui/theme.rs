use ratatui::style::{Color, Modifier, Style};

// iTunes 2004 color palette
pub const SIDEBAR_BG: Color = Color::Rgb(225, 228, 232);
pub const SIDEBAR_TEXT: Color = Color::Rgb(30, 30, 30);
pub const SELECTION_BG: Color = Color::Rgb(56, 117, 215);
pub const SELECTION_TEXT: Color = Color::White;
pub const MAIN_BG: Color = Color::Rgb(255, 255, 255);
pub const ALT_ROW_BG: Color = Color::Rgb(237, 243, 254);
pub const BORDER: Color = Color::Rgb(180, 180, 180);
pub const FOOTER_BG: Color = Color::Rgb(200, 203, 207);
pub const FOOTER_TEXT: Color = Color::Rgb(40, 40, 40);
pub const HEADER_TEXT: Color = Color::Rgb(80, 80, 80);
pub const DIM_TEXT: Color = Color::Rgb(140, 140, 140);
pub const ERROR_TEXT: Color = Color::Rgb(200, 50, 50);
pub const SUCCESS_TEXT: Color = Color::Rgb(50, 160, 50);
pub const PROGRESS_BAR: Color = Color::Rgb(56, 117, 215);
pub const PROGRESS_BG: Color = Color::Rgb(220, 220, 220);

pub fn selected() -> Style {
    Style::default()
        .bg(SELECTION_BG)
        .fg(SELECTION_TEXT)
        .add_modifier(Modifier::BOLD)
}

pub fn sidebar_item() -> Style {
    Style::default().fg(SIDEBAR_TEXT)
}

pub fn sidebar_item_selected() -> Style {
    selected()
}

pub fn header() -> Style {
    Style::default()
        .fg(HEADER_TEXT)
        .add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::default().fg(DIM_TEXT)
}

pub fn footer() -> Style {
    Style::default().bg(FOOTER_BG).fg(FOOTER_TEXT)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}

pub fn error() -> Style {
    Style::default().fg(ERROR_TEXT)
}

pub fn success() -> Style {
    Style::default().fg(SUCCESS_TEXT)
}
