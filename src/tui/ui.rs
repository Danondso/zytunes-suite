use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Padding, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of a string in terminal cells. Uses standard Unicode EAW
/// measurement (ambiguous-width chars counted as 1 cell) to match what
/// common terminals actually render, and more importantly what ratatui uses
/// internally when laying out spans and widgets. CJK Wide chars are 2 cells
/// under either measurement, so this still fixes the Japanese-overflow case.
#[inline]
fn disp_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

#[inline]
fn char_disp_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

use crate::anim;
use crate::app::{
    format_duration, format_with_commas, AlbumArtCache, App, BrowseMode, DevicePresence,
    DeviceStatus, NowPlaying, Panel, PlaybackState, SidebarEntry, SidebarMode, SortColumn,
    SyncStatus, TrackInfo,
};
use crate::theme;
use zytunes::library::Track;

/// Responsive layout dimensions computed from terminal size.
///
/// Three width tiers: Compact (<100), Standard (100-139), Full (>=140).
/// Height tier: now-playing hidden when < 20 rows.
pub(crate) struct LayoutMetrics {
    device_width: u16,
    sidebar_width: u16,
    album_width: u16,
    keys_width: u16,
    show_now_playing: bool,
    show_zip_art: bool,
    footer_left_width: u16,
    player_art_width: u16,
}

impl LayoutMetrics {
    /// `show_player` is the final resolved decision from the caller — it
    /// already accounts for the user's preference and the current playback
    /// state. LayoutMetrics itself only enforces the hard height floor below
    /// which the panel can't physically fit.
    fn new(area: Rect, show_keys: bool, has_album_browser: bool, show_player: bool) -> Self {
        let w = area.width;
        let h = area.height;
        let show_now_playing = show_player && h >= 12;

        if w < 100 {
            // Compact: hide device panel, force-hide keys, narrow sidebar.
            LayoutMetrics {
                device_width: 0,
                sidebar_width: 20,
                album_width: if has_album_browser { 22 } else { 0 },
                keys_width: 0,
                show_now_playing,
                show_zip_art: false,
                footer_left_width: 12,
                player_art_width: 0,
            }
        } else if w < 140 {
            // Standard: narrower device panel, force-hide keys.
            LayoutMetrics {
                device_width: 28,
                sidebar_width: 24,
                album_width: if has_album_browser { 24 } else { 0 },
                keys_width: 0,
                show_now_playing,
                show_zip_art: true,
                footer_left_width: 16,
                player_art_width: 14,
            }
        } else {
            // Full: current behavior.
            LayoutMetrics {
                device_width: 36,
                sidebar_width: if has_album_browser { 24 } else { 28 },
                album_width: if has_album_browser { 30 } else { 0 },
                keys_width: if show_keys { 24 } else { 0 },
                show_now_playing,
                show_zip_art: true,
                footer_left_width: 16,
                player_art_width: 16,
            }
        }
    }

    /// Returns (device_w, sidebar_w, album_w, keys_w) for album art
    /// pre-render calculations in the event loop.
    // Only the binary's run loop calls this; under `tui-testing` the lib
    // build sees a dead-code warning since main.rs isn't part of the lib
    // compilation unit.
    #[allow(dead_code)]
    pub(crate) fn panel_widths(
        width: u16,
        show_keys: bool,
        has_album_browser: bool,
    ) -> (u16, u16, u16, u16) {
        if width < 100 {
            (0, 20, if has_album_browser { 22 } else { 0 }, 0)
        } else if width < 140 {
            (28, 24, if has_album_browser { 24 } else { 0 }, 0)
        } else {
            (
                36,
                if has_album_browser { 24 } else { 28 },
                if has_album_browser { 30 } else { 0 },
                if show_keys { 24 } else { 0 },
            )
        }
    }
}

fn throbber_symbol(state: &ThrobberState, theme: &theme::Theme) -> String {
    Throbber::default()
        .throbber_set(theme.spinner_set.clone())
        .use_type(WhichUse::Spin)
        .to_symbol_span(state)
        .content
        .trim()
        .to_string()
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    if app.loading_library {
        draw_startup(f, app, size);
        return;
    }

    let m = LayoutMetrics::new(
        size,
        app.show_keys,
        app.has_album_browser(),
        app.should_show_player(size.height),
    );

    // Outer horizontal: device left | middle content | keys right
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(m.device_width),
            Constraint::Min(40),
            Constraint::Length(m.keys_width),
        ])
        .split(size);

    // Left column: device info + sync queue + log
    if m.device_width > 0 {
        draw_device_left_panel(f, app, outer[0]);
    }

    // Middle: browser area + optional now-playing + footer
    let middle = if m.show_now_playing {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(8),
                Constraint::Length(9),
                Constraint::Length(3),
            ])
            .split(outer[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(3)])
            .split(outer[1])
    };

    // Browser columns: sidebar | optional albums | tracks
    let browser = if m.album_width > 0 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(m.sidebar_width),
                Constraint::Length(m.album_width),
                Constraint::Min(20),
            ])
            .split(middle[0])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(m.sidebar_width), Constraint::Min(30)])
            .split(middle[0])
    };

    draw_sidebar(f, app, browser[0]);
    if m.album_width > 0 {
        draw_album_browser(f, app, browser[1]);
        draw_track_list(f, app, browser[2], m.show_zip_art);
    } else {
        draw_track_list(f, app, browser[1], m.show_zip_art);
    }

    if m.show_now_playing {
        if let Some(ref np) = app.now_playing {
            draw_now_playing(f, app, np, middle[1], m.player_art_width);
        }
    }
    let footer_idx = if m.show_now_playing { 2 } else { 1 };
    draw_footer(f, app, middle[footer_idx], m.footer_left_width);

    if m.keys_width > 0 {
        draw_keys_panel(f, app, outer[2]);
    }

    // Removal confirmation overlay.
    if let Some(ref paths) = app.pending_removal {
        draw_confirm_removal(f, app, paths.len());
    }

    if app.pending_cache_clear {
        draw_confirm_cache_clear(f, app);
    }

    // Toast overlay.
    if let Some((ref msg, _, is_error)) = app.toast_message {
        draw_toast(f, app, msg, is_error);
    }

    // Help overlay.
    if app.show_help {
        draw_help_overlay(f, app);
    }

    // Theme picker overlay.
    if app.show_theme_picker {
        draw_theme_picker(f, app);
    }

    // Track-info popup. Drawn before search so an active search input still
    // sits on top, matching the precedence in the input dispatcher.
    if app.show_track_info {
        draw_track_info_overlay(f, app);
    }

    // Search overlay.
    if app.search_active {
        draw_search_overlay(f, app);
    }
}

fn draw_startup(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let symbol = throbber_symbol(&app.throbber_state, app.theme());
    let pulse = anim::animated_accent(
        t.accent_color(),
        t.accent_secondary,
        t.accent_anim,
        app.anim_frame,
        40,
    );

    let revealed = anim::typing_reveal("zytunes", app.anim_frame);

    // Pick the panel width first so we can size the phrase/bar/counter.
    let w = 48u16.min(area.width);
    let h = 11u16.min(area.height);

    // Inner content width (panel minus borders).
    let inner_w = w.saturating_sub(2) as usize;
    // Fixed-width display slot so shorter/longer phrases all take the same
    // number of columns; otherwise a centered line jumps as phrases rotate.
    // Reserve ` {symbol} ` (3 cells) before the phrase.
    let phrase_slot = inner_w.saturating_sub(3);
    let phrase = app.scan_phrase.as_deref().unwrap_or("Scanning library...");
    let phrase_trunc = truncate(phrase, phrase_slot);
    let phrase_padded = pad_right_to_width(&phrase_trunc, phrase_slot);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            revealed,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", symbol), Style::default().fg(pulse)),
            Span::raw(phrase_padded),
        ]),
    ];

    // Progress bar + counter, both fixed-width so centering stays stable.
    if let Some((done, total)) = app.scan_progress {
        if total > 0 {
            let bar_width = inner_w.saturating_sub(2); // 1 cell padding each side
            let filled = ((done as f64 / total as f64) * bar_width as f64).round() as usize;
            let filled = filled.min(bar_width);
            let bar: String = std::iter::repeat_n('\u{2588}', filled)
                .chain(std::iter::repeat_n('\u{2591}', bar_width - filled))
                .collect();
            // Zero-pad the completed count so digits don't grow the string
            // while scanning (e.g. "     1 / 60000" → "  1234 / 60000").
            let total_digits = total.to_string().len();
            let counter = format!("{done:>total_digits$} / {total}");
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(bar, Style::default().fg(pulse))));
            lines.push(Line::from(Span::styled(counter, t.dim())));
        }
    }

    let block = t
        .block()
        .border_style(t.dim())
        .title(" Starting ")
        .title_alignment(Alignment::Center);

    // All dynamic content is now fixed-width per frame, so centering is stable.
    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);

    // Center the panel.
    let h = if app.scan_progress.is_some() { h } else { 10 };
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let centered = Rect::new(x, y, w, h);

    f.render_widget(Clear, centered);
    f.render_widget(paragraph, centered);
}

/// Foreground color for the sidebar's `✓`/`◐` sync-status glyph.
///
/// The unselected accent is `selection_bg` (the theme's highlight color).
/// But the selected row *also* has `selection_bg` as its background, so that
/// same color would render the glyph invisible. Falling back to
/// `selection_text` keeps the glyph readable on the highlight without
/// inventing a new palette slot.
fn sidebar_icon_accent(theme: &theme::Theme, selected: bool) -> ratatui::style::Color {
    if selected {
        theme.selection_text
    } else {
        theme.selection_bg
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::Library;
    let mode_label = match app.sidebar_mode {
        SidebarMode::Artists => "Artists",
        SidebarMode::Albums => "Albums",
    };
    let browse_prefix = match app.browse_mode {
        BrowseMode::Library => "",
        BrowseMode::Device => match app.device.family {
            Some(zytunes::device::DeviceFamily::Ipod) => "iPod: ",
            _ => "Zune: ",
        },
    };

    let title = format!(" {}{} ", browse_prefix, mode_label);
    let border_style = if is_active {
        t.active_border()
    } else {
        t.border()
    };
    let block = t
        .block()
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(t.sidebar_bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.sidebar_items.is_empty() {
        let msg = match app.browse_mode {
            BrowseMode::Library => {
                if app.library.is_none() {
                    "No library loaded"
                } else {
                    "(empty)"
                }
            }
            BrowseMode::Device => {
                if app.device.tracks.is_empty() {
                    "No tracks on device"
                } else {
                    "(empty)"
                }
            }
        };
        let p = Paragraph::new(msg).style(t.dim());
        f.render_widget(p, inner);
        return;
    }

    let visible_height = inner.height as usize;
    let scroll = compute_scroll(
        app.sidebar_selected,
        visible_height,
        app.sidebar_items.len(),
    );

    // Show device presence indicators in Library browse mode for both
    // Artists (keyed by artist name) and Albums (keyed by "artist — album").
    let show_device_status = app.browse_mode == BrowseMode::Library
        && (matches!(app.sidebar_mode, SidebarMode::Artists | SidebarMode::Albums))
        && (!app.artist_device_status.is_empty() || !app.album_device_status.is_empty());

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, entry)| {
            let style = if i == app.sidebar_selected {
                t.sidebar_item_selected()
            } else {
                t.sidebar_item()
            };
            let cursor = if i == app.sidebar_selected {
                "> "
            } else {
                "  "
            };
            let name = entry.display();
            let (icon, icon_style) = if show_device_status {
                let presence = match entry {
                    SidebarEntry::Artist(artist) => app.artist_device_status.get(artist).copied(),
                    SidebarEntry::Album { artist, album } => app
                        .album_device_status
                        .get(&(artist.clone(), album.clone()))
                        .copied(),
                };
                let accent_fg = sidebar_icon_accent(t, i == app.sidebar_selected);
                match presence {
                    Some(DevicePresence::Full) => ("✓ ", style.fg(accent_fg)),
                    Some(DevicePresence::Partial) => ("◐ ", style.fg(accent_fg)),
                    _ => ("  ", style),
                }
            } else {
                ("", style)
            };
            let inner_w = inner.width as usize;
            let cursor_w = disp_width(cursor);
            let icon_w = disp_width(icon);
            let max_name = inner_w.saturating_sub(cursor_w + icon_w);
            let is_selected = i == app.sidebar_selected;
            let name_trunc = if is_selected && disp_width(&name) > max_name && max_name > 0 {
                marquee(&name, max_name, app.anim_frame)
            } else {
                truncate(&name, max_name).to_string()
            };
            // Pad to full row width so the selection background reaches the
            // right border (see album browser for rationale).
            let used = cursor_w + icon_w + disp_width(&name_trunc);
            let trailing = if used < inner_w {
                " ".repeat(inner_w - used)
            } else {
                String::new()
            };
            let line = Line::from(vec![
                Span::styled(cursor, style),
                Span::styled(icon, icon_style),
                Span::styled(name_trunc, style),
                Span::styled(trailing, style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn draw_album_browser(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::Albums;
    let header = app
        .sidebar_items
        .get(app.sidebar_selected)
        .map(|e| e.display().into_owned())
        .unwrap_or_default();
    let title = format!(
        " {} ",
        truncate(&header, area.width.saturating_sub(4) as usize)
    );
    let border_style = if is_active {
        t.active_border()
    } else {
        Style::default().fg(t.accent_secondary)
    };
    let block = t
        .block()
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(t.sidebar_bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.album_list.is_empty() {
        let p = Paragraph::new("No albums").style(t.dim());
        f.render_widget(p, inner);
        return;
    }

    // Build a flat list of rows: year headers interleaved with album items.
    // Each entry is (optional album index, display line).
    let mut rows: Vec<(Option<usize>, String, bool)> = Vec::new(); // (album_idx, label, is_header)
    let mut last_year: Option<Option<u32>> = None;
    for (i, album) in app.album_list.iter().enumerate() {
        if last_year != Some(album.year) {
            let header = match album.year {
                Some(y) => format!("\u{2500} {} \u{2500}", y),
                None => "\u{2500} Unknown \u{2500}".to_string(),
            };
            rows.push((None, header, true));
            last_year = Some(album.year);
        }
        let is_selected = i == app.album_selected;
        let prefix = if is_selected { "> " } else { "  " };
        let prefix_w = disp_width(prefix);
        let icon = match app
            .album_device_status
            .get(&(album.artist.clone(), album.name.clone()))
        {
            Some(DevicePresence::Full) => "✓ ",
            Some(DevicePresence::Partial) => "◐ ",
            _ => "",
        };
        let icon_w = disp_width(icon);
        let inner_w = inner.width as usize;
        let max_name = inner_w.saturating_sub(prefix_w + icon_w);
        let display_name = if is_selected && disp_width(&album.name) > max_name && max_name > 0 {
            marquee(&album.name, max_name, app.anim_frame)
        } else {
            truncate(&album.name, max_name).to_string()
        };
        // Pad the row to the full interior width. CJK text is narrower in
        // cells than char count, and relying on ratatui to extend the row
        // background leaves gaps on some terminals — explicit trailing
        // spaces guarantee the selection highlight reaches the right border.
        let mut label = format!("{}{}{}", prefix, icon, display_name);
        let rendered = disp_width(&label);
        if rendered < inner_w {
            label.push_str(&" ".repeat(inner_w - rendered));
        }
        rows.push((Some(i), label, false));
    }

    // Find scroll position: we want the selected album visible.
    // Find its row index in the flat list.
    let selected_row = rows
        .iter()
        .position(|(idx, _, _)| *idx == Some(app.album_selected))
        .unwrap_or(0);
    let visible_height = inner.height as usize;
    let scroll = compute_scroll(selected_row, visible_height, rows.len());

    let items: Vec<ListItem> = rows
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|(idx, label, is_header)| {
            if *is_header {
                ListItem::new(label.as_str()).style(t.dim())
            } else {
                let is_selected = *idx == Some(app.album_selected);
                let style = if is_selected {
                    t.sidebar_item_selected()
                } else {
                    t.sidebar_item()
                };
                ListItem::new(label.as_str()).style(style)
            }
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn draw_track_list(f: &mut Frame, app: &App, area: Rect, show_zip_art: bool) {
    if app.has_album_browser() {
        draw_album_detail(f, app, area, show_zip_art);
    } else {
        draw_track_table(f, app, area);
    }
}

fn draw_album_detail(f: &mut Frame, app: &App, area: Rect, show_zip_art: bool) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::TrackList;
    let border_style = if is_active {
        t.active_border()
    } else {
        t.border()
    };

    let album = app.album_list.get(app.album_selected);
    let album_name = album.map(|a| a.name.as_str()).unwrap_or("No Album");
    let album_artist = album.map(|a| a.artist.as_str()).unwrap_or("");
    let title = format!(
        " {} ",
        truncate(album_name, area.width.saturating_sub(4) as usize)
    );

    let block = t
        .block()
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Build album info strings for embedding in the art.
    let track_count = app.track_list.len();
    let total_dur: u64 = app.track_list.iter().filter_map(|t| t.duration_ms).sum();
    let dur_str = if total_dur > 0 {
        let mins = total_dur / 60_000;
        if mins >= 60 {
            format!("{} hr {} min", mins / 60, mins % 60)
        } else {
            format!("{} min", mins)
        }
    } else {
        String::new()
    };
    let year_str = album
        .and_then(|a| a.year)
        .map(|y| y.to_string())
        .unwrap_or_default();

    // Show zip art only if the tier allows it AND there's enough width
    // (31 for art + 12 for tracks = 43 minimum).
    if show_zip_art && inner.width >= 43 {
        // Zip disk ASCII art by mga — https://www.asciiart.eu/art/324546af3173c962
        // Album/artist/track info embedded into the disk body and label.
        let art_lines = build_zip_art(
            app,
            album_name,
            album_artist,
            &year_str,
            track_count,
            &dur_str,
        );

        // Side-by-side: zip art on the left, track listing + album art on the right.
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(31), Constraint::Min(12)])
            .split(inner);

        let art = Paragraph::new(art_lines);
        f.render_widget(art, cols[0]);

        // Split right column: tracks on top, album art (boxed) below.
        let art_rows = app
            .album_art_cache
            .as_ref()
            .map(|c| c.rows() as u16)
            .unwrap_or(0);
        // +2 for top/bottom border, +1 for 1-row padding on top (0 on bottom).
        let art_panel_rows = if art_rows > 0 { art_rows + 3 } else { 0 };
        let right_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(art_panel_rows)])
            .split(cols[1]);

        draw_album_track_list(f, app, right_split[0]);
        // Extend the art slot one column right and one row down so the art
        // panel's right and bottom borders overlap (share) the outer detail
        // block's right and bottom border columns/row.
        let art_slot = Rect {
            x: right_split[1].x,
            y: right_split[1].y,
            width: right_split[1].width + 1,
            height: right_split[1].height + 1,
        };
        draw_album_art_panel(f, app, art_slot, border_style);
    } else {
        // Not enough width or compact tier: full-width track list, no zip art.
        draw_album_track_list(f, app, inner);
    }
}

fn draw_album_art_panel(f: &mut Frame, app: &App, area: Rect, outer_border_style: Style) {
    if area.height < 3 {
        return;
    }

    // The art is typically narrower than the full column (image aspect ratio).
    // Shrink the panel horizontally to hug the art, anchored to the right edge
    // of the slot so its right border coincides with the outer detail block's
    // right border.
    let art_w = app
        .album_art_cache
        .as_ref()
        .map(|c| c.width() as u16)
        .unwrap_or(0);
    if art_w == 0 {
        return;
    }

    let t = app.theme();
    let title = " Album Art ";
    // Panel sizing horizontally: art + 2 border + 4 (2+2) padding.
    // Title must also fit across the top.
    let min_w = (title.chars().count() as u16 + 2).max(art_w + 6);
    let panel_w = min_w.min(area.width);
    // Right-anchor: panel's right edge = slot's right edge.
    let panel_area = Rect {
        x: area.x + area.width.saturating_sub(panel_w),
        y: area.y,
        width: panel_w,
        height: area.height,
    };

    // Interior padding: 2 left/right, 1 top, 0 bottom. inner() accounts for
    // both border and padding.
    let block = t
        .block()
        .border_style(t.border())
        .title(title)
        .style(Style::default().bg(t.main_bg))
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(panel_area);
    f.render_widget(block, panel_area);

    // The panel's top-right and bottom-left corners land on the outer block's
    // border line. Replace them with T-junction glyphs so the outer line
    // appears to pass through, and style them to match the outer block's
    // active/inactive color — not the art panel's own border — since the
    // outer line is what the junction visually extends.
    if let Some((right_t, up_t)) = junction_chars(t.border_type) {
        let junction_style = outer_border_style.bg(t.main_bg);
        let buf = f.buffer_mut();
        if panel_area.width > 0 {
            let tr_x = panel_area.x + panel_area.width - 1;
            if let Some(cell) = buf.cell_mut((tr_x, panel_area.y)) {
                cell.set_symbol(right_t).set_style(junction_style);
            }
        }
        if panel_area.height > 0 {
            let bl_y = panel_area.y + panel_area.height - 1;
            if let Some(cell) = buf.cell_mut((panel_area.x, bl_y)) {
                cell.set_symbol(up_t).set_style(junction_style);
            }
        }
    }

    draw_album_art_inline(f, app, inner);
}

/// T-junction glyphs matching a given border type: (right-side T, bottom-side T).
/// Returns `None` for border types that lack clean single-glyph junctions
/// (e.g. quadrant block borders), in which case we leave the corners as-is.
fn junction_chars(bt: ratatui::widgets::BorderType) -> Option<(&'static str, &'static str)> {
    use ratatui::widgets::BorderType;
    match bt {
        // Plain and Rounded share junction glyphs — rounded corners only differ
        // at corners, not at T-intersections.
        BorderType::Plain | BorderType::Rounded => Some(("\u{2524}", "\u{2534}")), // ┤ ┴
        BorderType::Thick => Some(("\u{252B}", "\u{253B}")),                       // ┫ ┻
        BorderType::Double => Some(("\u{2563}", "\u{2569}")),                      // ╣ ╩
        _ => None,
    }
}

/// T-junction glyphs where a vertical divider meets the outer horizontal border:
/// (top = down-T, bottom = up-T). Returns `None` for border types without clean
/// single-glyph junctions.
fn vertical_divider_junctions(
    bt: ratatui::widgets::BorderType,
) -> Option<(&'static str, &'static str)> {
    use ratatui::widgets::BorderType;
    match bt {
        BorderType::Plain | BorderType::Rounded => Some(("\u{252C}", "\u{2534}")), // ┬ ┴
        BorderType::Thick => Some(("\u{2533}", "\u{253B}")),                       // ┳ ┻
        BorderType::Double => Some(("\u{2566}", "\u{2569}")),                      // ╦ ╩
        _ => None,
    }
}

fn draw_album_art_inline(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let t = app.theme();
    let pad_style = Style::default().bg(t.main_bg);

    let cache = match app.album_art_cache.as_ref() {
        Some(c) if c.rows() > 0 => c,
        _ => return,
    };
    let art_w = cache.width() as u16;

    let x_offset = area.width.saturating_sub(art_w) / 2;
    let pad: String = " ".repeat(x_offset as usize);
    let take_w = area.width.saturating_sub(x_offset) as usize;
    let take_h = area.height as usize;

    // Per-row prefix: the padding span (if any). Shared across both branches.
    let prefix = |spans: &mut Vec<Span<'static>>| {
        if x_offset > 0 {
            spans.push(Span::styled(pad.clone(), pad_style));
        }
    };

    let lines: Vec<Line> = match cache {
        AlbumArtCache::Halfblock(rows) => rows
            .iter()
            .take(take_h)
            .map(|row| {
                let mut spans = Vec::with_capacity(row.len() + 1);
                prefix(&mut spans);
                for &(_, fg, bg) in row.iter().take(take_w) {
                    spans.push(Span::styled(
                        "▀",
                        Style::default()
                            .fg(Color::Rgb(fg[0], fg[1], fg[2]))
                            .bg(Color::Rgb(bg[0], bg[1], bg[2])),
                    ));
                }
                Line::from(spans)
            })
            .collect(),
        AlbumArtCache::Ascii(rows) => rows
            .iter()
            .take(take_h)
            .map(|row| {
                let mut spans = Vec::with_capacity(row.len() + 1);
                prefix(&mut spans);
                for &(ch, fg) in row.iter().take(take_w) {
                    let mut buf = [0u8; 4];
                    spans.push(Span::styled(
                        ch.encode_utf8(&mut buf).to_string(),
                        Style::default()
                            .fg(Color::Rgb(fg[0], fg[1], fg[2]))
                            .bg(t.main_bg),
                    ));
                }
                Line::from(spans)
            })
            .collect(),
    };

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_album_track_list(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    if app.track_list.is_empty() {
        let p = Paragraph::new("  No tracks").style(t.dim());
        f.render_widget(p, area);
        return;
    }

    let is_active = app.active_panel == Panel::TrackList;

    // Determine if this is a multi-disc album.
    let disc_count = {
        let mut discs: Vec<Option<u32>> = app.track_list.iter().map(|t| t.disc_number).collect();
        discs.dedup();
        discs.len()
    };
    let is_multi_disc = disc_count > 1;

    // Build a flat list of rows, interleaving disc headers for multi-disc albums.
    // Each entry: (Option<track_index>, Row).
    let mut flat_rows: Vec<(Option<usize>, Row)> = Vec::new();
    let mut last_disc: Option<Option<u32>> = None;
    for (i, track) in app.track_list.iter().enumerate() {
        if is_multi_disc && last_disc != Some(track.disc_number) {
            let disc_label = match track.disc_number {
                Some(d) => format!("\u{2500}\u{2500} Disc {} \u{2500}\u{2500}", d),
                None => "\u{2500}\u{2500} Disc ? \u{2500}\u{2500}".to_string(),
            };
            flat_rows.push((
                None,
                Row::new(vec![
                    Cell::from(""),
                    Cell::from(disc_label),
                    Cell::from(""),
                    Cell::from(""),
                ])
                .style(t.dim()),
            ));
            last_disc = Some(track.disc_number);
        }

        let is_selected = i == app.track_selected && is_active;
        // Stripe by track position (the loop index), so disc headers don't
        // desync alternation.
        let bg = if is_selected {
            t.selection_bg
        } else if i.is_multiple_of(2) {
            t.main_bg
        } else {
            t.alt_row_bg
        };
        let fg = if is_selected {
            t.selection_text
        } else {
            t.sidebar_text
        };

        let num = track
            .track_number
            .map(|n| format!("{}.", n))
            .unwrap_or_default();
        let dur = track.duration_ms.map(format_duration).unwrap_or_default();
        // Em-dash for tracks without a device-side playcount (library rows,
        // never-played tracks, or formats the device didn't report).
        let plays = track
            .play_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string());

        let name_raw = if track.on_device {
            format!("✓ {}", track.name)
        } else {
            track.name.clone()
        };
        // Name column width = total - 4 (num) - 6 (dur) - 6 (plays) - 3 gutters.
        let name_w = (area.width as usize).saturating_sub(19);
        let display_name = if is_selected && disp_width(&name_raw) > name_w && name_w > 0 {
            marquee(&name_raw, name_w, app.anim_frame)
        } else {
            name_raw
        };

        flat_rows.push((
            Some(i),
            Row::new(vec![
                Cell::from(num),
                Cell::from(display_name),
                Cell::from(dur),
                Cell::from(plays),
            ])
            .style(Style::default().bg(bg).fg(fg)),
        ));
    }

    // Header row labels each column. Without this users can't tell what
    // the rightmost number columns mean — particularly Plays, which is
    // new and not self-evident.
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("Title"),
        Cell::from("Dur"),
        Cell::from("Plays"),
    ])
    .style(t.header())
    .height(1);

    // Scroll based on the selected track's position in the flat list.
    let selected_flat = flat_rows
        .iter()
        .position(|(idx, _)| *idx == Some(app.track_selected))
        .unwrap_or(0);
    // -1 row reserved for the header.
    let visible_height = (area.height as usize).saturating_sub(1);
    let scroll = compute_scroll(selected_flat, visible_height, flat_rows.len());

    let visible_rows: Vec<Row> = flat_rows
        .into_iter()
        .skip(scroll)
        .take(visible_height)
        .map(|(_, row)| row)
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(12),
        Constraint::Length(6),
        Constraint::Length(6),
    ];

    let table = Table::new(visible_rows, widths).header(header);
    f.render_widget(table, area);
}

fn draw_track_table(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::TrackList;
    let title = format!(" Tracks ({}) ", app.track_list.len());
    let border_style = if is_active {
        t.active_border()
    } else {
        t.border()
    };
    let block = t
        .block()
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(t.main_bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.track_list.is_empty() {
        let msg = if app.library.is_none() {
            "Load a library to browse tracks"
        } else {
            "Select an item from the sidebar"
        };
        let p = Paragraph::new(msg).style(t.dim());
        f.render_widget(p, inner);
        return;
    }

    // Column sort indicator.
    let sort_indicator = |col: SortColumn| -> &str {
        if col == app.sort_column {
            if app.sort_ascending {
                " ^"
            } else {
                " v"
            }
        } else {
            ""
        }
    };

    let header_cells = [
        Cell::from(format!("#{}", sort_indicator(SortColumn::Number))),
        Cell::from(format!("Name{}", sort_indicator(SortColumn::Name))),
        Cell::from(format!("Artist{}", sort_indicator(SortColumn::Artist))),
        Cell::from(format!("Album{}", sort_indicator(SortColumn::Album))),
        Cell::from(format!("Dur{}", sort_indicator(SortColumn::Duration))),
        Cell::from(format!("Fmt{}", sort_indicator(SortColumn::Format))),
        // Plays column shows the device-side playcount when present.
        // `—` means "not surfaced for this device family" (Zune until
        // Phase 4b lands the read path) or "never played".
        Cell::from("Plays"),
    ];
    let header = Row::new(header_cells).style(t.header()).height(1);

    let visible_height = inner.height.saturating_sub(1) as usize; // minus header
    let scroll = compute_scroll(app.track_selected, visible_height, app.track_list.len());

    let rows: Vec<Row> = app
        .track_list
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, track)| {
            let bg = if i == app.track_selected {
                t.selection_bg
            } else if i.is_multiple_of(2) {
                t.main_bg
            } else {
                t.alt_row_bg
            };
            let fg = if i == app.track_selected {
                t.selection_text
            } else {
                t.sidebar_text
            };

            let num = track
                .track_number
                .map(|n| n.to_string())
                .unwrap_or_default();
            let dur = track.duration_ms.map(format_duration).unwrap_or_default();
            let kind = track
                .kind
                .as_deref()
                .unwrap_or("")
                .replace(" audio file", "")
                .replace("MPEG", "MP3");

            let name_raw = if track.on_device {
                format!("✓ {}", track.name)
            } else {
                track.name.clone()
            };
            // Table name column is ~30% of the flex area (widths below sum to
            // 75% + 22 fixed cols: 4 num + 6 dur + 6 fmt + 6 plays). Estimate
            // the rendered width so the selected row's marquee matches what
            // ratatui will actually show.
            let flex = (inner.width as usize).saturating_sub(22);
            let name_w = flex * 30 / 100;
            let is_row_selected = i == app.track_selected;
            let display_name = if is_row_selected && disp_width(&name_raw) > name_w && name_w > 0 {
                marquee(&name_raw, name_w, app.anim_frame)
            } else {
                name_raw
            };

            // Em-dash (`—`) renders 1 cell wide and reads as "no value"
            // without being mistaken for the numeric `0`.
            let plays = track
                .play_count
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string());

            Row::new(vec![
                Cell::from(num),
                Cell::from(display_name),
                Cell::from(track.artist.clone()),
                Cell::from(track.album.clone()),
                Cell::from(dur),
                Cell::from(kind),
                Cell::from(plays),
            ])
            .style(Style::default().bg(bg).fg(fg))
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Percentage(30),
        Constraint::Percentage(20),
        Constraint::Percentage(25),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
    ];

    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, inner);
}

fn draw_device_left_panel(f: &mut Frame, app: &App, area: Rect) {
    // Split left column into 3 vertical sections: device info, sync queue, log.
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ])
        .split(area);

    draw_device_info(f, app, sections[0]);
    draw_sync_queue(f, app, sections[1]);
    draw_sync_log(f, app, sections[2]);
}

fn draw_device_info(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::Device;
    let border_style = if is_active {
        t.active_border()
    } else {
        t.border()
    };

    let title = match app.device.status {
        DeviceStatus::Disconnected => " Device [c] ".to_string(),
        DeviceStatus::Detecting | DeviceStatus::Connecting => " Device ".to_string(),
        DeviceStatus::Connected => {
            let name = app.device.name.as_deref().unwrap_or("Zune");
            format!(" {} ", name)
        }
    };

    let block = t
        .block()
        .border_style(border_style)
        .title(title)
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(t.main_bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    match app.device.status {
        DeviceStatus::Disconnected => {
            let zune_art = build_zune_art("No Device", "Press [C]");
            let pulse = anim::animated_accent(
                t.dim_text,
                t.accent_secondary,
                theme::AccentAnim::Pulse,
                app.anim_frame,
                60,
            );
            let art_lines: Vec<Line> = zune_art
                .iter()
                .map(|l| {
                    Line::from(Span::styled(l.as_str(), Style::default().fg(pulse)))
                        .alignment(Alignment::Center)
                })
                .collect();
            f.render_widget(Paragraph::new(art_lines), inner);
        }
        DeviceStatus::Detecting | DeviceStatus::Connecting => {
            let conn_frame = app
                .connection_anim_start
                .map(|start| app.anim_frame.wrapping_sub(start))
                .unwrap_or(0);
            let (screen1, screen2) = anim::connection_screen_lines(conn_frame);
            let zune_art = build_zune_art(screen1, screen2);
            let pulse = anim::animated_accent(
                t.accent_color(),
                t.accent_secondary,
                t.accent_anim,
                app.anim_frame,
                40,
            );
            let art_lines: Vec<Line> = zune_art
                .iter()
                .map(|l| {
                    Line::from(Span::styled(l.as_str(), Style::default().fg(pulse)))
                        .alignment(Alignment::Center)
                })
                .collect();
            let p = Paragraph::new(art_lines);
            f.render_widget(p, inner);
        }
        DeviceStatus::Connected => {
            draw_device_info_connected(f, app, inner);
        }
    }
}

fn draw_device_info_connected(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_syncing = matches!(app.sync.status, SyncStatus::Running { .. });
    let is_busy = app.device.loading_tracks;

    // Screen content: two lines inside the screen area.
    let (screen_line1, screen_line2) = if is_syncing {
        let symbol = throbber_symbol(&app.throbber_state, app.theme());
        (symbol, "Syncing...".to_string())
    } else if is_busy {
        let symbol = throbber_symbol(&app.throbber_state, app.theme());
        (symbol, "Loading...".to_string())
    } else {
        let count = app.device.tracks.len();
        let formatted = format_with_commas(count);
        (formatted, "tracks".to_string())
    };

    let zune_art = build_zune_art(&screen_line1, &screen_line2);

    let art_color = if is_syncing || is_busy {
        anim::animated_accent(
            t.accent_color(),
            t.accent_secondary,
            t.accent_anim,
            app.anim_frame,
            40,
        )
    } else {
        t.dim_text
    };

    let mut lines: Vec<Line> = Vec::new();

    // Zune ASCII art (centered).
    for l in &zune_art {
        lines.push(
            Line::from(Span::styled(l.as_str(), Style::default().fg(art_color)))
                .alignment(Alignment::Center),
        );
    }

    // Device info lines below art.
    if let Some(ref fw) = app.device.firmware {
        lines.push(Line::from(vec![
            Span::styled(" FW: ", t.dim()),
            Span::raw(fw.as_str()),
        ]));
    }
    if let Some(ref mfr) = app.device.manufacturer {
        lines.push(Line::from(vec![
            Span::styled(" Mfr: ", t.dim()),
            Span::raw(mfr.as_str()),
        ]));
    }
    if let Some(ref mode) = app.device.usb_mode {
        lines.push(Line::from(vec![
            Span::styled(" USB: ", t.dim()),
            Span::raw(mode.as_str()),
        ]));
    }
    if let Some(ref serial) = app.device.serial {
        let display = if serial.chars().count() > 12 {
            format!("{}...", serial.chars().take(12).collect::<String>())
        } else {
            serial.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(" S/N: ", t.dim()),
            Span::raw(display),
        ]));
    }
    if let Some(ref sync_status) = app.device.sync_status {
        lines.push(Line::from(vec![
            Span::styled(" Sync: ", t.dim()),
            Span::raw(sync_status.as_str()),
        ]));
    }

    // Acquired items (only shown when > 0).
    if app.device.acquired_items > 0 {
        lines.push(Line::from(vec![
            Span::styled(" Acquired: ", t.dim()),
            Span::raw(format!(
                "{} item{}",
                app.device.acquired_items,
                if app.device.acquired_items == 1 {
                    ""
                } else {
                    "s"
                }
            )),
        ]));
    }

    // Storage info.
    if let Some(ref storage) = app.device.storage {
        let total_gb = storage.total_bytes as f64 / 1_073_741_824.0;
        let free_gb = storage.free_bytes as f64 / 1_073_741_824.0;
        let used_gb = storage.used_bytes as f64 / 1_073_741_824.0;
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            " {:.1}/{:.1} GB ({:.1} free)",
            used_gb, total_gb, free_gb
        )));
        let bar_width = (area.width as usize).saturating_sub(8).min(26);
        let filled = (bar_width as f64 * storage.used_percent as f64 / 100.0) as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar_chars = anim::progress_bar_with_shine(filled, empty, app.anim_frame);
        let shine_color = anim::pulse_color(t.progress_bar, app.anim_frame, 20);
        let mut bar_spans = vec![Span::raw(" [")];
        for (ch, is_shine) in &bar_chars {
            let color = if *is_shine {
                shine_color
            } else if *ch == '=' {
                t.progress_bar
            } else {
                t.progress_bg
            };
            bar_spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        }
        bar_spans.push(Span::raw(format!("] {}%", storage.used_percent)));
        lines.push(Line::from(bar_spans));
    }

    if !app.device.loading_tracks {
        lines.push(Line::from(format!(
            " {} tracks on device",
            app.device.tracks.len()
        )));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, area);
}

fn draw_sync_queue(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::SyncQueue;
    let border_style = if is_active {
        t.active_border()
    } else {
        t.border()
    };

    match app.sync.status {
        SyncStatus::Running { current, total } => {
            let symbol = throbber_symbol(&app.throbber_state, app.theme());
            let title = format!(" {} {}/{} ", symbol, current, total);
            let block = t
                .block()
                .border_style(border_style)
                .title(title)
                .style(Style::default().bg(t.main_bg));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(inner);

            let track_color = anim::pulse_color(t.sidebar_text, app.anim_frame, 40);
            let status_line = format!(" {}", app.sync.current_track);
            f.render_widget(
                Paragraph::new(status_line).style(Style::default().fg(track_color)),
                chunks[0],
            );

            // Render the queue list below the progress line so mid-sync
            // additions are visible. Without this, the whole queue panel is
            // just the progress header and users can't see items they
            // enqueued while the sync was running.
            if !app.sync.queue.is_empty() {
                let row_w = chunks[1].width as usize;
                let items: Vec<ListItem> = app
                    .sync
                    .queue
                    .iter()
                    .enumerate()
                    .map(|(i, q)| {
                        let is_selected = i == app.sync.queue_selected && is_active;
                        let style = if is_selected {
                            t.sidebar_item_selected()
                        } else {
                            Style::default().fg(t.sidebar_text).bg(t.main_bg)
                        };
                        let cursor = if is_selected { "> " } else { "  " };
                        let n = q.tracks.len();
                        let suffix =
                            format!(" ({} {})", n, if n == 1 { "track" } else { "tracks" });
                        let cursor_w = disp_width(cursor);
                        let suffix_w = disp_width(&suffix);
                        let max_label = row_w.saturating_sub(cursor_w + suffix_w);
                        let label_trunc =
                            if is_selected && disp_width(&q.label) > max_label && max_label > 0 {
                                marquee(&q.label, max_label, app.anim_frame)
                            } else {
                                truncate(&q.label, max_label).to_string()
                            };
                        ListItem::new(format!("{}{}{}", cursor, label_trunc, suffix)).style(style)
                    })
                    .collect();
                f.render_widget(List::new(items), chunks[1]);
            }
        }
        SyncStatus::Idle => {
            if app.browse_mode == BrowseMode::Device && !app.removal_queue.is_empty() {
                // Show removal queue in device mode.
                let title = format!(" Remove Queue ({}) ", app.removal_queue.len());
                let block = t
                    .block()
                    .border_style(Style::default().fg(t.error_text))
                    .title(title)
                    .style(Style::default().bg(t.main_bg));
                let inner = block.inner(area);
                f.render_widget(block, area);

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(2), Constraint::Length(1)])
                    .split(inner);

                let items: Vec<ListItem> = app
                    .removal_queue
                    .iter()
                    .map(|(path, _)| {
                        let name = path.rsplit('/').next().unwrap_or(path);
                        ListItem::new(format!("  x {}", name))
                            .style(Style::default().fg(t.error_text).bg(t.main_bg))
                    })
                    .collect();
                let list = List::new(items);
                f.render_widget(list, chunks[0]);

                let hints = "D:delete C:clr";
                f.render_widget(Paragraph::new(hints).style(t.dim()), chunks[1]);
            } else {
                // Show sync queue in library mode.
                let total_tracks = app.total_queue_tracks();
                let title = format!(" Queue {} / {} trk ", app.sync.queue.len(), total_tracks);
                let block = t
                    .block()
                    .border_style(border_style)
                    .title(title)
                    .style(Style::default().bg(t.main_bg));
                let inner = block.inner(area);
                f.render_widget(block, area);

                if app.sync.queue.is_empty() {
                    let p = Paragraph::new("Press 'a' to add.").style(t.dim());
                    f.render_widget(p, inner);
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(2), Constraint::Length(1)])
                        .split(inner);
                    let row_w = chunks[0].width as usize;

                    let items: Vec<ListItem> = app
                        .sync
                        .queue
                        .iter()
                        .enumerate()
                        .map(|(i, q)| {
                            let is_selected = i == app.sync.queue_selected && is_active;
                            let style = if is_selected {
                                t.sidebar_item_selected()
                            } else {
                                Style::default().fg(t.sidebar_text).bg(t.main_bg)
                            };
                            // Cursor mirrors the sidebar: "> " on the selected
                            // row, "  " elsewhere, so padding and selection
                            // background stay consistent.
                            let cursor = if is_selected { "> " } else { "  " };
                            let n = q.tracks.len();
                            let suffix =
                                format!(" ({} {})", n, if n == 1 { "track" } else { "tracks" });
                            let cursor_w = disp_width(cursor);
                            let suffix_w = disp_width(&suffix);
                            let max_label = row_w.saturating_sub(cursor_w + suffix_w);
                            let label_trunc =
                                if is_selected && disp_width(&q.label) > max_label && max_label > 0
                                {
                                    marquee(&q.label, max_label, app.anim_frame)
                                } else {
                                    truncate(&q.label, max_label).to_string()
                                };
                            ListItem::new(format!("{}{}{}", cursor, label_trunc, suffix))
                                .style(style)
                        })
                        .collect();

                    let list = List::new(items);
                    f.render_widget(list, chunks[0]);

                    let hints = "S:sync d:rm C:clr";
                    f.render_widget(Paragraph::new(hints).style(t.dim()), chunks[1]);
                }
            }
        }
    }
}

fn draw_sync_log(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let block = t
        .block()
        .border_style(Style::default().fg(t.progress_bar))
        .title(" Log ")
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.sync.log.is_empty() {
        let p = Paragraph::new("No messages yet.").style(t.dim());
        f.render_widget(p, inner);
        return;
    }

    // Show messages with scroll support. log_scroll=0 means pinned to bottom.
    let visible = inner.height as usize;
    let end = app.sync.log.len().saturating_sub(app.sync.log_scroll);
    let start = end.saturating_sub(visible);
    let lines: Vec<Line> = app.sync.log[start..end]
        .iter()
        .map(|msg| {
            let style = if msg.contains("FAILED") {
                Style::default().fg(t.error_text)
            } else if msg.starts_with("  OK") || msg.contains("Done:") {
                Style::default().fg(t.success_text)
            } else {
                Style::default().fg(t.sidebar_text)
            };
            Line::from(Span::styled(msg.as_str(), style))
        })
        .collect();

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn draw_keys_panel(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let block = t
        .block()
        .title(" Keys [h] ")
        .style(Style::default().bg(t.sidebar_bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // Global keys — always shown.
    let global = [
        ("q", "Quit"),
        ("h", "Toggle keys"),
        ("?", "Full help"),
        ("Tab", "Next panel"),
        ("S-Tab", "Prev panel"),
        ("1/2/3", "Art/Alb/Plist"),
        ("4", "Sync queue"),
        ("v", "Lib/Device view"),
        ("t", "Theme picker"),
        ("T", "Art style"),
        ("P", "Player panel"),
        ("/", "Search"),
        ("c", "Connect"),
        ("X", "Clear cache"),
    ];
    lines.push(Line::from(Span::styled(
        " Global",
        Style::default()
            .fg(t.header_text)
            .add_modifier(Modifier::BOLD),
    )));
    for (key, desc) in &global {
        lines.push(key_line(app, key, desc));
    }

    lines.push(Line::from(""));

    // Context-sensitive keys.
    let is_device_mode = app.browse_mode == BrowseMode::Device;
    let add_label = if is_device_mode {
        "Remove"
    } else {
        "Add to queue"
    };
    let add_track_label = if is_device_mode {
        "Remove track"
    } else {
        "Add track"
    };
    let add_all_label = if is_device_mode {
        "Remove all"
    } else {
        "Add all"
    };
    let add_album_label = if is_device_mode {
        "Remove album"
    } else {
        "Add album"
    };

    let (section, keys): (&str, Vec<(&str, &str)>) = match app.active_panel {
        Panel::Library => (
            if is_device_mode {
                " Zune Library"
            } else {
                " Library"
            },
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("\u{2190}\u{2192}", "Skip A\u{2192}B\u{2192}C"),
                ("Enter", "Select"),
                ("a", add_label),
            ],
        ),
        Panel::Albums => (
            " Albums",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("Enter", "View tracks"),
                ("a", add_album_label),
            ],
        ),
        Panel::TrackList => (
            " Tracks",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("s", "Cycle sort"),
                ("a", add_track_label),
                ("A", add_all_label),
            ],
        ),
        Panel::Device => (
            " Device",
            if is_device_mode {
                vec![("r", "Refresh"), ("d", "Disconnect"), ("U", "Dedupe")]
            } else {
                vec![("r", "Refresh"), ("d", "Disconnect")]
            },
        ),
        Panel::SyncQueue => (
            " Queue",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("Enter/S", "Sync"),
                ("d", "Remove item"),
                ("C", "Clear all"),
                ("Esc", "Cancel sync"),
            ],
        ),
    };

    lines.push(Line::from(Span::styled(
        section,
        Style::default()
            .fg(t.header_text)
            .add_modifier(Modifier::BOLD),
    )));
    for (key, desc) in &keys {
        lines.push(key_line(app, key, desc));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn key_line<'a>(app: &App, key: &'a str, desc: &'a str) -> Line<'a> {
    let t = app.theme();
    Line::from(vec![
        Span::styled(
            format!(" {:>6} ", key),
            Style::default()
                .fg(t.selection_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(t.sidebar_text)),
    ])
}

fn draw_now_playing(f: &mut Frame, app: &App, np: &NowPlaying, area: Rect, art_width: u16) {
    let t = app.theme();
    let skin = app.theme().player_skin;

    let state_icon = match np.state {
        PlaybackState::Playing => skin.play,
        PlaybackState::Paused => skin.pause,
        PlaybackState::Stopped => skin.play,
    };

    let border_color = if np.state == PlaybackState::Playing {
        anim::animated_accent(
            t.accent_color(),
            t.accent_secondary,
            t.accent_anim,
            app.anim_frame,
            40,
        )
    } else {
        t.border
    };

    // Single unified block for the entire now-playing panel.
    let title = format!(" {} Now Playing ", state_icon);
    let block = t
        .block()
        .border_style(Style::default().fg(border_color))
        .title(title)
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split inner area: info (left) | divider + art (right). The right column
    // reserves 1 column for a vertical divider so the art sits inside its own
    // bordered sub-panel that connects to the outer block via T-junctions.
    let right_col_width = art_width + 1;
    let show_art = art_width > 0 && inner.width > right_col_width + 20;
    let (info_area, art_area) = if show_art {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(right_col_width)])
            .split(inner);
        (cols[0], Some(cols[1]))
    } else {
        (inner, None)
    };

    // --- Art (right side, inside the shared block) ---
    if let Some(art_rect) = art_area {
        // Draw a LEFT-only sub-border; its inner area holds the art.
        let divider_style = Style::default().fg(border_color).bg(t.main_bg);
        let divider_block = Block::default()
            .borders(Borders::LEFT)
            .border_type(t.border_type)
            .border_style(divider_style)
            .style(Style::default().bg(t.main_bg));
        let art_inner = divider_block.inner(art_rect);
        f.render_widget(divider_block, art_rect);

        // Patch the cells where the divider meets the outer top/bottom borders
        // with T-junction glyphs so the seams read as a single continuous frame.
        if let Some((down_t, up_t)) = vertical_divider_junctions(t.border_type) {
            let buf = f.buffer_mut();
            if let Some(cell) = buf.cell_mut((art_rect.x, area.y)) {
                cell.set_symbol(down_t).set_style(divider_style);
            }
            let bot_y = area.y + area.height.saturating_sub(1);
            if let Some(cell) = buf.cell_mut((art_rect.x, bot_y)) {
                cell.set_symbol(up_t).set_style(divider_style);
            }
        }

        let art_frame = np.paused_frame.unwrap_or(app.anim_frame);
        let art_lines = (skin.art_fn)(true, art_frame);
        let art_color = if np.state == PlaybackState::Playing {
            anim::animated_accent(
                t.accent_color(),
                t.accent_secondary,
                t.accent_anim,
                app.anim_frame,
                40,
            )
        } else {
            t.accent_color()
        };
        let aw = art_inner.width as usize;
        let ah = art_inner.height as usize;
        // Vertically center the art within the available height.
        let v_pad = ah.saturating_sub(art_lines.len()) / 2;
        let mut art_text: Vec<Line> = Vec::with_capacity(ah);
        for _ in 0..v_pad {
            art_text.push(Line::from(""));
        }
        for l in &art_lines {
            let pad = aw.saturating_sub(l.len());
            let left = pad / 2;
            let right = pad - left;
            let padded = format!("{}{}{}", " ".repeat(left), l, " ".repeat(right));
            art_text.push(Line::from(Span::styled(
                padded,
                Style::default().fg(art_color),
            )));
        }
        f.render_widget(Paragraph::new(art_text), art_inner);
    }

    // --- Info (left side) ---
    // Slot the metadata marquee in just under the time row when there's
    // both content to show and a row of vertical headroom. Falls back to
    // the original 5-row layout otherwise so cramped windows degrade
    // gracefully.
    let show_marquee = !np.metadata_marquee.is_empty() && info_area.height >= 6;
    let rows = if show_marquee {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // track name
                Constraint::Length(1), // artist — album (year)
                Constraint::Length(1), // controls
                Constraint::Length(1), // progress bar
                Constraint::Length(1), // time + hints
                Constraint::Length(1), // metadata marquee
                Constraint::Min(0),    // absorb extra
            ])
            .split(info_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // track name
                Constraint::Length(1), // artist — album (year)
                Constraint::Length(1), // controls
                Constraint::Length(1), // progress bar
                Constraint::Length(1), // time + hints
                Constraint::Min(0),    // absorb extra
            ])
            .split(info_area)
    };

    // Track name
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", &np.track_name),
            Style::default()
                .fg(t.sidebar_text)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    // Artist — Album (year)
    let album_line = match np.year {
        Some(y) => format!(" {} — {} ({})", &np.artist, &np.album, y),
        None => format!(" {} — {}", &np.artist, &np.album),
    };
    f.render_widget(
        Paragraph::new(album_line).style(Style::default().fg(t.header_text)),
        rows[1],
    );

    // Controls
    let controls = format!("{} {} {}", skin.prev, state_icon, skin.next);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            controls,
            Style::default()
                .fg(t.sidebar_text)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        rows[2],
    );

    // Progress bar
    let bar_width = rows[3].width.saturating_sub(4) as usize;
    let progress_ratio = if np.duration_ms > 0 {
        (np.elapsed_ms as f64 / np.duration_ms as f64).min(1.0)
    } else {
        0.0
    };
    let filled = (bar_width as f64 * progress_ratio) as usize;
    let empty = bar_width.saturating_sub(filled);

    let mut bar_spans = vec![Span::raw("  ")];
    if np.state == PlaybackState::Playing {
        let shine = anim::shine_offset(app.anim_frame, bar_width);
        for i in 0..filled {
            let is_shine = shine.is_some_and(|sp| i >= sp.saturating_sub(1) && i <= sp + 1);
            let color = if is_shine {
                anim::pulse_color(t.progress_bar, app.anim_frame, 30)
            } else {
                t.progress_bar
            };
            bar_spans.push(Span::styled(
                skin.bar_filled.to_string(),
                Style::default().fg(color),
            ));
        }
    } else {
        bar_spans.push(Span::styled(
            std::iter::repeat_n(skin.bar_filled, filled).collect::<String>(),
            Style::default().fg(t.progress_bar),
        ));
    }
    bar_spans.push(Span::styled(
        std::iter::repeat_n(skin.bar_empty, empty).collect::<String>(),
        Style::default().fg(t.progress_bg),
    ));
    f.render_widget(Paragraph::new(Line::from(bar_spans)), rows[3]);

    // Time + hints
    let elapsed_str = format_duration(np.elapsed_ms);
    let total_str = format_duration(np.duration_ms);
    let time_line = format!("  {} / {}  </>:scrub  n/p:skip", elapsed_str, total_str);
    f.render_widget(
        Paragraph::new(time_line).style(Style::default().fg(t.header_text)),
        rows[4],
    );

    // Metadata marquee — extended tag info (genre, BPM, key, bitrate, …)
    // joined by ` | `, scrolling on the same 12-frame-pause / 4-frame-step
    // cadence as the rest of the TUI's marqueed text. Pre-built once at
    // play time on `NowPlaying::metadata_marquee`, so per-frame work here
    // is just the windowing slice from `marquee()`.
    if show_marquee {
        let marquee_w = (rows[5].width as usize).saturating_sub(2);
        let scrolled = marquee(&np.metadata_marquee, marquee_w, app.anim_frame);
        f.render_widget(
            Paragraph::new(format!("  {}", scrolled)).style(Style::default().fg(t.dim_text)),
            rows[5],
        );
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, footer_left_width: u16) {
    let t = app.theme();
    let block = t.block().style(t.footer());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let left = match app.browse_mode {
        BrowseMode::Library => format!(" {} tracks", app.track_count()),
        BrowseMode::Device => {
            let rm_count = app.removal_queue.len();
            if rm_count > 0 {
                format!(
                    " {} on device | {} queued for removal",
                    app.device.tracks.len(),
                    rm_count
                )
            } else {
                format!(" {} on device", app.device.tracks.len())
            }
        }
    };
    let right = if app.browse_mode == BrowseMode::Device {
        "v:library | a:queue rm | D:delete | C:clr | ?:help".to_string()
    } else if app.device.status == DeviceStatus::Connected {
        "✓=synced ◐=partial | v:device | a:add | S:sync | q:quit | ?:help".to_string()
    } else {
        "v:device | a:add | S:sync | q:quit | ?:help".to_string()
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(footer_left_width), Constraint::Min(20)])
        .split(inner);

    f.render_widget(Paragraph::new(left).style(t.footer()), chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{} ", right))
            .alignment(Alignment::Right)
            .style(t.footer()),
        chunks[1],
    );
}

fn draw_confirm_removal(f: &mut Frame, app: &App, count: usize) {
    let t = app.theme();
    let area = f.area();
    let w = 36u16.min(area.width.saturating_sub(4));
    let h = 5u16.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .border_style(t.error())
        .title(" Confirm Delete ")
        .title_alignment(Alignment::Center);

    let lines = vec![
        Line::from(""),
        Line::from(format!(" Delete {} track(s)?", count)),
        Line::from(Span::styled(" Enter/y:yes  Esc/n:no", t.dim())),
    ];
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, rect);
}

fn draw_confirm_cache_clear(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();
    let w = 36u16.min(area.width.saturating_sub(4));
    let h = 5u16.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .border_style(t.error())
        .title(" Clear Cache ")
        .title_alignment(Alignment::Center);

    let lines = vec![
        Line::from(""),
        Line::from(" Clear playback cache?"),
        Line::from(Span::styled(" Enter/y:yes  Esc/n:no", t.dim())),
    ];
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, rect);
}

fn draw_toast(f: &mut Frame, app: &App, msg: &str, is_error: bool) {
    let t = app.theme();
    let area = f.area();
    if area.width < 8 || area.height < 3 {
        return;
    }
    let width = (msg.len() as u16 + 4).min(area.width - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(3)) / 2;
    let rect = Rect::new(x, y, width, 3);

    f.render_widget(Clear, rect);

    let style = if is_error { t.error() } else { t.success() };
    let block = t.block().border_style(style);
    let p = Paragraph::new(format!(" {} ", msg))
        .block(block)
        .style(style);
    f.render_widget(p, rect);
}

fn draw_help_overlay(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();
    let width = 50u16.min(area.width - 4);
    let height = 30u16.min(area.height - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let help_text = vec![
        "",
        "  Navigation",
        "  Tab         Cycle panels",
        "  Up/Down     Navigate items",
        "  Enter       Select / expand",
        "  1/2         Artists / Albums",
        "  v           Toggle Library / Device view",
        "  t           Theme picker",
        "  T           Toggle album art style (halfblock/ASCII)",
        "  P           Cycle player panel (auto / hidden / always)",
        "",
        "  Playback",
        "  Space       Play / pause selected track",
        "  n / p       Next / previous track in playlist",
        "  < / >       Seek -/+ 5 seconds",
        "",
        "  Logs",
        "  PgUp/PgDn   Scroll sync log",
        "  L           Dump log to /tmp (copies path to clipboard)",
        "",
        "  Library",
        "  /           Search sidebar",
        "  s           Cycle sort column",
        "  I           Show track info (TrackList panel)",
        "",
        "  Sync",
        "  a           Add track to queue",
        "  A           Add all visible tracks",
        "  4           Jump to sync queue",
        "  S / Enter   Execute sync",
        "  d           Remove from queue",
        "  C           Clear queue",
        "",
        "  Device view",
        "  a           Remove track from device",
        "  A           Remove all visible tracks",
        "  U           Dedupe (remove duplicate copies, keep newest)",
        "",
        "  Device",
        "  c           Connect to Zune",
        "  r           Refresh device tracks",
        "  X           Clear playback cache",
        "  Esc         Close / cancel",
        "  q           Quit",
    ];

    let lines: Vec<Line> = help_text.iter().map(|l| Line::from(*l)).collect();

    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(" Help — press Esc to close ");
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(p, rect);
}

fn draw_theme_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();
    let themes = theme::all_themes();
    let theme_count = themes.len();
    let width = 30u16.min(area.width.saturating_sub(4));
    let height = (theme_count as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(" Theme [t] ")
        .style(Style::default().bg(t.sidebar_bg));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = themes
        .iter()
        .enumerate()
        .map(|(i, theme_entry)| {
            let style = if i == app.theme_picker_index {
                t.selected()
            } else {
                Style::default().fg(t.sidebar_text).bg(t.sidebar_bg)
            };
            ListItem::new(format!("  {}", theme_entry.name)).style(style)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

/// One row inside the track-info popup body. `Section` renders as a dim
/// divider header; `Field` renders as a key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataRow {
    Section(&'static str),
    Field { key: &'static str, value: String },
}

/// Build the displayable rows for the track-info popup. The TUI's
/// `TrackInfo` carries the always-present identity fields plus the bits the
/// device cares about (duration, kind, on_device); when a matching library
/// `Track` is supplied (Library browse mode), the richer extended-metadata
/// fields (composer, ISRC, MusicBrainz IDs, audio properties, …) are
/// surfaced too.
///
/// Sections are emitted **only** if at least one field inside them is
/// non-empty — keeps the popup terse on lightly-tagged tracks.
pub fn format_metadata_pairs(track: &TrackInfo, lib: Option<&Track>) -> Vec<MetadataRow> {
    let mut out: Vec<MetadataRow> = Vec::new();

    // -- Identity (always shown) --
    out.push(MetadataRow::Section("Identity"));
    out.push(MetadataRow::Field {
        key: "Title",
        value: track.name.clone(),
    });
    out.push(MetadataRow::Field {
        key: "Artist",
        value: track.artist.clone(),
    });
    if let Some(aa) = lib.and_then(|t| t.album_artist.as_ref()) {
        out.push(MetadataRow::Field {
            key: "Album Artist",
            value: aa.clone(),
        });
    }
    out.push(MetadataRow::Field {
        key: "Album",
        value: track.album.clone(),
    });
    let track_total = lib.and_then(|t| t.track_total);
    if let Some(n) = track.track_number {
        let v = match track_total {
            Some(total) => format!("{n} / {total}"),
            None => format!("{n}"),
        };
        out.push(MetadataRow::Field {
            key: "Track #",
            value: v,
        });
    }
    let disc_total = lib.and_then(|t| t.disc_total);
    if let Some(n) = track.disc_number {
        let v = match disc_total {
            Some(total) => format!("{n} / {total}"),
            None => format!("{n}"),
        };
        out.push(MetadataRow::Field {
            key: "Disc #",
            value: v,
        });
    }

    // -- Classification --
    let mut class_rows: Vec<MetadataRow> = Vec::new();
    if let Some(g) = track.genre.as_ref() {
        class_rows.push(MetadataRow::Field {
            key: "Genre",
            value: g.clone(),
        });
    } else if let Some(g) = lib.and_then(|t| t.genre.as_ref()) {
        class_rows.push(MetadataRow::Field {
            key: "Genre",
            value: g.clone(),
        });
    }
    if let Some(y) = lib.and_then(|t| t.year) {
        class_rows.push(MetadataRow::Field {
            key: "Year",
            value: y.to_string(),
        });
    }
    if let Some(b) = lib.and_then(|t| t.bpm) {
        class_rows.push(MetadataRow::Field {
            key: "BPM",
            value: b.to_string(),
        });
    }
    push_lib_string(&mut class_rows, "Initial Key", lib, |t| {
        t.initial_key.as_deref()
    });
    push_lib_string(&mut class_rows, "Mood", lib, |t| t.mood.as_deref());
    push_lib_string(&mut class_rows, "Language", lib, |t| t.language.as_deref());
    if let Some(r) = lib.and_then(|t| t.rating) {
        class_rows.push(MetadataRow::Field {
            key: "Rating",
            value: format!("{r}/255"),
        });
    }
    if !class_rows.is_empty() {
        out.push(MetadataRow::Section("Classification"));
        out.append(&mut class_rows);
    }

    // -- Credits --
    let mut cred_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut cred_rows, "Composer", lib, |t| t.composer.as_deref());
    push_lib_string(&mut cred_rows, "Conductor", lib, |t| t.conductor.as_deref());
    push_lib_string(&mut cred_rows, "Lyricist", lib, |t| t.lyricist.as_deref());
    push_lib_string(&mut cred_rows, "Original Artist", lib, |t| {
        t.original_artist.as_deref()
    });
    push_lib_string(&mut cred_rows, "Original Album", lib, |t| {
        t.original_album.as_deref()
    });
    push_lib_string(&mut cred_rows, "Original Release Date", lib, |t| {
        t.original_release_date.as_deref()
    });
    if !cred_rows.is_empty() {
        out.push(MetadataRow::Section("Credits"));
        out.append(&mut cred_rows);
    }

    // -- Identifiers --
    let mut id_rows: Vec<MetadataRow> = Vec::new();
    if let Some(id) = lib.map(|t| t.id) {
        id_rows.push(MetadataRow::Field {
            key: "Library ID",
            value: id.to_string(),
        });
    }
    push_lib_string(&mut id_rows, "ISRC", lib, |t| t.isrc.as_deref());
    push_lib_string(&mut id_rows, "Barcode", lib, |t| t.barcode.as_deref());
    push_lib_string(&mut id_rows, "Catalog #", lib, |t| {
        t.catalog_number.as_deref()
    });
    push_lib_string(&mut id_rows, "Publisher", lib, |t| t.publisher.as_deref());
    push_lib_string(&mut id_rows, "Copyright", lib, |t| t.copyright.as_deref());
    if !id_rows.is_empty() {
        out.push(MetadataRow::Section("Identifiers"));
        out.append(&mut id_rows);
    }

    // -- MusicBrainz --
    let mut mb_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut mb_rows, "Recording ID", lib, |t| {
        t.mb_recording_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Track ID", lib, |t| t.mb_track_id.as_deref());
    push_lib_string(&mut mb_rows, "Release ID", lib, |t| {
        t.mb_release_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Release Group ID", lib, |t| {
        t.mb_release_group_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Work ID", lib, |t| t.mb_work_id.as_deref());
    push_lib_string(&mut mb_rows, "Artist ID", lib, |t| {
        t.mb_artist_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Release Artist ID", lib, |t| {
        t.mb_release_artist_id.as_deref()
    });
    if !mb_rows.is_empty() {
        out.push(MetadataRow::Section("MusicBrainz"));
        out.append(&mut mb_rows);
    }

    // -- ReplayGain --
    let mut rg_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut rg_rows, "Track Gain", lib, |t| {
        t.replaygain_track_gain.as_deref()
    });
    push_lib_string(&mut rg_rows, "Track Peak", lib, |t| {
        t.replaygain_track_peak.as_deref()
    });
    push_lib_string(&mut rg_rows, "Album Gain", lib, |t| {
        t.replaygain_album_gain.as_deref()
    });
    push_lib_string(&mut rg_rows, "Album Peak", lib, |t| {
        t.replaygain_album_peak.as_deref()
    });
    if !rg_rows.is_empty() {
        out.push(MetadataRow::Section("ReplayGain"));
        out.append(&mut rg_rows);
    }

    // -- Audio properties --
    let mut audio_rows: Vec<MetadataRow> = Vec::new();
    if let Some(k) = track.kind.as_ref() {
        audio_rows.push(MetadataRow::Field {
            key: "Format",
            value: k.clone(),
        });
    }
    if let Some(rate) = lib.and_then(|t| t.sample_rate) {
        audio_rows.push(MetadataRow::Field {
            key: "Sample Rate",
            value: format!("{:.1} kHz", rate as f64 / 1000.0),
        });
    }
    if let Some(c) = lib.and_then(|t| t.channels) {
        audio_rows.push(MetadataRow::Field {
            key: "Channels",
            value: c.to_string(),
        });
    }
    if let Some(bd) = lib.and_then(|t| t.bit_depth) {
        audio_rows.push(MetadataRow::Field {
            key: "Bit Depth",
            value: format!("{bd} bit"),
        });
    }
    if let Some(br) = lib.and_then(|t| t.audio_bitrate_kbps) {
        audio_rows.push(MetadataRow::Field {
            key: "Bitrate",
            value: format!("{br} kbps"),
        });
    }
    if let Some(d) = track.duration_ms {
        audio_rows.push(MetadataRow::Field {
            key: "Duration",
            value: format_duration(d),
        });
    }
    if !audio_rows.is_empty() {
        out.push(MetadataRow::Section("Audio"));
        out.append(&mut audio_rows);
    }

    // -- File --
    let mut file_rows: Vec<MetadataRow> = Vec::new();
    if let Some(loc) = track.location.as_ref() {
        file_rows.push(MetadataRow::Field {
            key: "Path",
            value: loc.clone(),
        });
    }
    if let Some(sz) = lib.and_then(|t| t.file_size_bytes) {
        file_rows.push(MetadataRow::Field {
            key: "File Size",
            value: format_bytes(sz),
        });
    }
    push_lib_string(&mut file_rows, "Encoder", lib, |t| t.encoder.as_deref());
    push_lib_string(&mut file_rows, "Encoder Settings", lib, |t| {
        t.encoder_settings.as_deref()
    });
    push_lib_string(&mut file_rows, "AcoustID", lib, |t| {
        t.acoustic_id.as_deref()
    });
    if !file_rows.is_empty() {
        out.push(MetadataRow::Section("File"));
        out.append(&mut file_rows);
    }

    // -- Notes --
    let mut note_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut note_rows, "Comment", lib, |t| t.comment.as_deref());
    push_lib_string(&mut note_rows, "Description", lib, |t| {
        t.description.as_deref()
    });
    if let Some(lyr) = lib.and_then(|t| t.lyrics.as_ref()) {
        // Lyrics can be many KB; show only the first line + a count, the
        // rest would dominate the popup.
        let line_count = lyr.lines().count().max(1);
        let preview = lyr.lines().next().unwrap_or("").to_string();
        let value = if line_count > 1 {
            format!("{preview}  … ({line_count} lines)")
        } else {
            preview
        };
        if !value.is_empty() {
            note_rows.push(MetadataRow::Field {
                key: "Lyrics",
                value,
            });
        }
    }
    if !note_rows.is_empty() {
        out.push(MetadataRow::Section("Notes"));
        out.append(&mut note_rows);
    }

    // -- Listening (aggregate plays/skips, valid whether on-device or not) --
    let mut listening_rows: Vec<MetadataRow> = Vec::new();
    if let Some(p) = track.play_count {
        listening_rows.push(MetadataRow::Field {
            key: "Plays",
            value: p.to_string(),
        });
    }
    if let Some(s) = track.skip_count {
        listening_rows.push(MetadataRow::Field {
            key: "Skips",
            value: s.to_string(),
        });
    }
    if let Some(t) = track.last_played_at_ms {
        listening_rows.push(MetadataRow::Field {
            key: "Last played",
            value: humanize_relative_ms(t, now_unix_ms_for_humanize()),
        });
    }
    if !listening_rows.is_empty() {
        out.push(MetadataRow::Section("Listening"));
        out.append(&mut listening_rows);
    }

    // -- Device-side bits (only when on-device) --
    if track.on_device {
        let mut dev_rows: Vec<MetadataRow> = Vec::new();
        if let Some(r) = track.rating {
            dev_rows.push(MetadataRow::Field {
                key: "Device Rating",
                value: format!("{}/100", r),
            });
        }
        if let Some(t) = track.last_synced_from_device_at_ms {
            dev_rows.push(MetadataRow::Field {
                key: "Last synced",
                value: humanize_relative_ms(t, now_unix_ms_for_humanize()),
            });
        }
        if !dev_rows.is_empty() {
            out.push(MetadataRow::Section("Device"));
            out.append(&mut dev_rows);
        }
    }

    out
}

/// Wall-clock now in unix ms. Wrapped here so the popup formatter has a
/// single point to swap for tests (the `humanize_relative_ms` helper takes
/// `now_ms` explicitly so unit tests pin the relative output).
fn now_unix_ms_for_humanize() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Render a unix-epoch-ms timestamp as a relative string against `now_ms`,
/// e.g. `"just now"`, `"5 minutes ago"`, `"yesterday"`, `"3 days ago"`,
/// or `"2026-04-12"` for older. Future timestamps (clock skew between
/// devices) clamp to "just now" so the popup never shows nonsense like
/// "in 5 minutes."
pub fn humanize_relative_ms(then_ms: u64, now_ms: u64) -> String {
    if then_ms >= now_ms {
        return "just now".to_string();
    }
    let secs = (now_ms - then_ms) / 1000;
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{mins} minutes ago")
        };
    }
    let hours = mins / 60;
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        };
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{days} days ago");
    }
    if days < 30 {
        let weeks = days / 7;
        return if weeks == 1 {
            "1 week ago".to_string()
        } else {
            format!("{weeks} weeks ago")
        };
    }
    // Older than a month — render as a calendar date in the user's local
    // sense of "year-month-day". We don't pull in chrono just for this;
    // do the math by hand against the unix epoch (1970-01-01 UTC).
    format_iso_date(then_ms / 1000)
}

/// Format a unix-second timestamp as `YYYY-MM-DD` (UTC). Standalone helper
/// instead of pulling in chrono — the popup only ever needs UTC date,
/// not full datetime formatting.
fn format_iso_date(unix_secs: u64) -> String {
    // Days since 1970-01-01.
    let mut days = (unix_secs / 86_400) as i64;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap_year(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months_leap = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = if is_leap_year(year) {
        &months_leap
    } else {
        &months_normal
    };
    let mut month: i64 = 1;
    for &dm in months {
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    let day = days + 1; // 1-based day
    format!("{year:04}-{month:02}-{day:02}")
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Helper: append a `Field` row from a library getter, only when the value
/// is non-empty after trimming.
fn push_lib_string(
    rows: &mut Vec<MetadataRow>,
    key: &'static str,
    lib: Option<&Track>,
    pick: impl Fn(&Track) -> Option<&str>,
) {
    if let Some(v) = lib.and_then(pick) {
        if !v.trim().is_empty() {
            rows.push(MetadataRow::Field {
                key,
                value: v.to_string(),
            });
        }
    }
}

/// Render byte counts in a compact human-readable form (KB / MB / GB).
fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

fn draw_track_info_overlay(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();

    // Responsive popup: percentage-based with a sane min/max so it stays
    // legible from 80x24 up to ultrawide terminals.
    let width = (area.width * 70 / 100)
        .clamp(40, 90)
        .min(area.width.saturating_sub(4));
    let height = (area.height * 70 / 100)
        .clamp(8, 30)
        .min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    // Resolve the current track. If somehow the popup is visible with no
    // selection, draw an empty block and bail — keeps render side defensive.
    let Some(track) = app.track_list.get(app.track_selected) else {
        let block = t
            .block()
            .border_style(Style::default().fg(t.selection_bg))
            .title(" Track info — Esc to close ")
            .style(Style::default().bg(t.main_bg));
        f.render_widget(block, rect);
        return;
    };

    // Resolved at popup-open time (`App::open_track_info`); reading the
    // cached value here keeps the render path O(1) instead of re-running
    // `tracks_by_name` on every ~50 ms tick while the popup is open.
    let lib_track = app.track_info_lib.as_ref();

    let title = format!(" Track info — {} (Esc to close) ", track.name);
    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(title)
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    // Column constraints below sum to `Length(20) + Min(10) = 30` cells, so
    // anything narrower will clip the value column to ~zero. Falling back to
    // the title-only block (the bare frame already drawn above) is a cleaner
    // failure mode than a half-rendered table.
    if inner.height < 2 || inner.width < 30 {
        return;
    }

    let pairs = format_metadata_pairs(track, lib_track);
    let total = pairs.len();
    let visible = inner.height.saturating_sub(1) as usize; // minus header
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.track_info_scroll.min(max_scroll);

    let header = Row::new([Cell::from("Field"), Cell::from("Value")])
        .style(t.header())
        .height(1);

    // Display width available for the value column. Both the marqueed string
    // and ratatui's column allocator must agree on this exact width — using
    // `Length(value_col_w)` (instead of `Min(10)`) makes the agreement
    // structural so the popup background never bleeds through truncated text.
    let value_col_w = (inner.width as usize).saturating_sub(21);
    let widths = [
        Constraint::Length(20),
        Constraint::Length(value_col_w as u16),
    ];

    let rows: Vec<Row> = pairs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, row)| match row {
            MetadataRow::Section(name) => {
                Row::new(vec![Cell::from(format!("── {} ──", name)), Cell::from("")])
                    .style(Style::default().fg(t.dim_text).bg(t.main_bg))
            }
            MetadataRow::Field { key, value } => {
                let bg = if i.is_multiple_of(2) {
                    t.main_bg
                } else {
                    t.alt_row_bg
                };
                // `marquee` is a no-op (returns the source / pad-truncated
                // form) when the value already fits, so short fields stay
                // perfectly stable; only overflowing values animate.
                let displayed = marquee(value, value_col_w, app.anim_frame);
                Row::new(vec![
                    Cell::from(*key).style(Style::default().fg(t.header_text)),
                    Cell::from(displayed).style(Style::default().fg(t.sidebar_text)),
                ])
                .style(Style::default().bg(bg))
            }
        })
        .collect();

    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, inner);
}

fn draw_search_overlay(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();
    let width = 40u16.min(area.width - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 3;
    let rect = Rect::new(x, y, width, 3);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(" Search ");
    let p = Paragraph::new(format!(" {}_", app.search_query)).block(block);
    f.render_widget(p, rect);
}

fn compute_scroll(selected: usize, visible: usize, total: usize) -> usize {
    if total <= visible {
        return 0;
    }
    if selected < visible / 2 {
        0
    } else if selected + visible / 2 >= total {
        total.saturating_sub(visible)
    } else {
        selected.saturating_sub(visible / 2)
    }
}

/// Build zip disk ASCII art with album info embedded.
/// Art by mga — <https://www.asciiart.eu/art/324546af3173c962>
fn build_zip_art<'a>(
    app: &App,
    album: &'a str,
    artist: &'a str,
    year: &'a str,
    track_count: usize,
    duration: &'a str,
) -> Vec<Line<'a>> {
    let t = app.theme();
    let dim = Style::default().fg(t.dim_text);
    let bold = Style::default()
        .fg(t.sidebar_text)
        .add_modifier(Modifier::BOLD);
    let info = Style::default().fg(t.header_text);

    // Pad or truncate a string to exactly `w` display columns. Uses
    // CJK-aware width so emoji / ambiguous-width chars don't overflow.
    let pad = |s: &str, w: usize| -> String {
        let t = truncate(s, w);
        let used = disp_width(&t);
        if used < w {
            format!("{}{}", t, " ".repeat(w - used))
        } else {
            t
        }
    };

    // Body lines (upper area) — artist and album name.
    let body_w = 18; // width inside `:  | ` and ` |  :`

    // Word-wrap helper: split text at a word boundary near `width` display
    // columns. Measures in cells, not chars, so wide glyphs don't push the
    // break point past the panel edge.
    let word_wrap = |text: &str, width: usize| -> (String, Option<String>) {
        if disp_width(text) <= width {
            return (pad(text, width), None);
        }
        // Walk chars accumulating display width, stopping at the first char
        // whose inclusion would exceed `width`.
        let mut used = 0usize;
        let mut byte_end = text.len();
        for (i, ch) in text.char_indices() {
            let cw = char_disp_width(ch);
            if used + cw > width {
                byte_end = i;
                break;
            }
            used += cw;
        }
        let break_at = text[..byte_end].rfind(' ').unwrap_or(byte_end);
        let first = pad(&text[..break_at], width);
        let rest = text[break_at..].trim_start().to_string();
        (first, Some(rest))
    };

    // Wrap artist across lines 1-2.
    let (artist_line1, artist_line2) = word_wrap(artist, body_w);

    // Wrap album — if artist used 2 lines, album only gets the mga line.
    let (album_line1, album_line2) = if artist_line2.is_some() {
        // Artist took 2 lines, album gets 1 line (line 3), no mga overflow.
        (pad(&truncate(album, body_w), body_w), None)
    } else {
        word_wrap(album, body_w)
    };

    // Line 2: artist overflow or album line 1.
    let line2_text = match &artist_line2 {
        Some(rest) => pad(&truncate(rest, body_w), body_w),
        None => album_line1.clone(),
    };
    let line2_style = if artist_line2.is_some() { bold } else { info };

    // Line 3 (mga line): album (if artist wrapped), album overflow, or just mga.
    let mga_line = if artist_line2.is_some() {
        // Artist wrapped — line 3 shows the album name.
        album_line1
    } else {
        match album_line2 {
            Some(ref rest) => {
                let rest_w = 13;
                format!("{} mga ", pad(rest, rest_w))
            }
            None => pad("              mga", body_w),
        }
    };
    let mga_style = if artist_line2.is_some() || album_line2.is_some() {
        info
    } else {
        dim
    };

    // Label lines (lower bracket area) — year, tracks, duration.
    let label_w = 18; // width inside `[` and `]`
    let year_line = if year.is_empty() {
        pad("", label_w)
    } else {
        pad(year, label_w)
    };
    let stats = format!("{} tracks", track_count);
    let stats_padded = pad(&stats, label_w);
    let dur_padded = pad(duration, label_w);

    vec![
        Line::from(Span::styled(r#"  .-|:"""":""""""'''"""":|-.  "#, dim)),
        Line::from(Span::styled(r#" :  |'----'-------------'|  : "#, dim)),
        // Artist name line 1
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(artist_line1, bold),
            Span::styled(" |  : ", dim),
        ]),
        // Line 2: artist overflow or album
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(line2_text, line2_style),
            Span::styled(" |  : ", dim),
        ]),
        // Line 3: album (if artist wrapped), album overflow, or mga
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(mga_line, mga_style),
            Span::styled(" |  : ", dim),
        ]),
        Line::from(Span::styled(r#" :  | .----------------. |  : "#, dim)),
        Line::from(Span::styled(r#" :  |[ zip:          [i]]|  : "#, dim)),
        Line::from(Span::styled(r#" :  |[------------------]|  : "#, dim)),
        // Year in label
        Line::from(vec![
            Span::styled(" :  |[", dim),
            Span::styled(year_line, info),
            Span::styled("]|  : ", dim),
        ]),
        // Track count in label
        Line::from(vec![
            Span::styled(" :  |[", dim),
            Span::styled(stats_padded, info),
            Span::styled("]|  : ", dim),
        ]),
        // Duration in label
        Line::from(vec![
            Span::styled(" :  |[", dim),
            Span::styled(dur_padded, info),
            Span::styled("]|  : ", dim),
        ]),
        Line::from(Span::styled(r#" :  |[::::::..    iomega]|  : "#, dim)),
        Line::from(Span::styled(r#" :  |""""""""""""""""""""|  : "#, dim)),
        Line::from(Span::styled(r#" '--'--------------------'--' "#, dim)),
    ]
}

/// Truncate a string to at most `max` terminal display columns, appending
/// an ellipsis if truncated. CJK characters occupy 2 columns, so char count
/// alone misrepresents rendered width.
fn truncate(s: &str, max: usize) -> String {
    if disp_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Reserve columns for the ellipsis glyph when there's room. `…` is an
    // ambiguous-width char — 2 cells under CJK-wide measurement — so reserve
    // its actual display width, not a hard-coded 1.
    let ellipsis = "\u{2026}";
    let ellipsis_w = disp_width(ellipsis);
    let (budget, suffix) = if max > ellipsis_w {
        (max - ellipsis_w, ellipsis)
    } else {
        (max, "")
    };
    let mut used = 0usize;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let w = char_disp_width(ch);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push_str(suffix);
    out
}

/// Right-pad `s` with spaces so its rendered display width is exactly `width`.
/// If `s` is already wider than `width`, returns it unchanged (the caller is
/// expected to have truncated first).
fn pad_right_to_width(s: &str, width: usize) -> String {
    let cur = disp_width(s);
    if cur >= width {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + (width - cur));
    out.push_str(s);
    for _ in 0..(width - cur) {
        out.push(' ');
    }
    out
}

/// Marquee-scroll a string that's wider than `width` display columns.
/// Scrolls through `text   text` seamlessly, advancing one character every
/// 4 frames with an initial pause. Respects grapheme display widths so wide
/// chars don't cause the visible window to drift.
fn marquee(text: &str, width: usize, frame: usize) -> String {
    if disp_width(text) <= width || width == 0 {
        return truncate(text, width);
    }
    let chars: Vec<char> = text.chars().collect();
    let gap = 3;
    let cycle_len = chars.len() + gap;
    // Pause at the start for 12 frames before scrolling.
    let scroll_frame = frame.saturating_sub(12);
    let offset = (scroll_frame / 4) % cycle_len;
    let padded: Vec<char> = chars
        .iter()
        .chain(std::iter::repeat_n(&' ', gap))
        .chain(chars.iter())
        .copied()
        .collect();
    // Take chars from `offset` onward until we fill `width` display columns.
    let mut used = 0usize;
    let mut out = String::new();
    for &ch in &padded[offset..] {
        let w = char_disp_width(ch);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    // Pad with spaces if the last char we couldn't fit left a half-column gap,
    // so the rendered width is stable across frames.
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

/// Pad/center a string to exactly `w` chars.
fn center_pad(s: &str, w: usize) -> String {
    let clipped = truncate_hard(s, w);
    let len = disp_width(&clipped);
    if len >= w {
        clipped
    } else {
        let left = (w - len) / 2;
        let right = w - len - left;
        format!("{}{}{}", " ".repeat(left), clipped, " ".repeat(right))
    }
}

/// Truncate to at most `max` display columns with no ellipsis. Used when
/// center/pad logic needs a hard cap on width.
fn truncate_hard(s: &str, max: usize) -> String {
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = char_disp_width(ch);
        if used + cw > max {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out
}

/// Build the Zune ASCII art lines with the given screen content.
fn build_zune_art(screen_line1: &str, screen_line2: &str) -> Vec<String> {
    let screen_w = 12;
    let line1 = center_pad(screen_line1, screen_w);
    let line2 = center_pad(screen_line2, screen_w);

    vec![
        " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ".to_string(),
        " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ".to_string(),
        format!(" \u{2502}  \u{2502}{}\u{2502}  \u{2502} ", line1),
        format!(" \u{2502}  \u{2502}{}\u{2502}  \u{2502} ", line2),
        " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ".to_string(),
        " \u{2502}                  \u{2502} ".to_string(),
        " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ".to_string(),
        " \u{2502}      \u{2502}    \u{2502}      \u{2502} ".to_string(),
        " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ".to_string(),
        " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TrackInfo;
    use zytunes::library::Track;

    fn empty_track_info() -> TrackInfo {
        TrackInfo::new(
            "Some Title".into(),
            "Some Artist".into(),
            "Some Album".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
    }

    #[test]
    fn format_metadata_pairs_skips_empty_fields() {
        // A track with only the always-present identity fields should not
        // emit em-dash rows for every optional metadata key — those just
        // pad the popup with noise.
        let ti = empty_track_info();
        let rows = format_metadata_pairs(&ti, None);
        let field_keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        // The always-present fields are Title/Artist/Album.
        assert!(field_keys.contains(&"Title"));
        assert!(field_keys.contains(&"Artist"));
        assert!(field_keys.contains(&"Album"));
        // No bonus rows for fields we never set.
        assert!(!field_keys.contains(&"Composer"));
        assert!(!field_keys.contains(&"ISRC"));
        assert!(!field_keys.contains(&"Sample Rate"));
        assert!(!field_keys.contains(&"BPM"));
    }

    #[test]
    fn format_metadata_pairs_groups_into_sections() {
        // Section dividers should appear as `MetadataRow::Section` between
        // groups of related fields.
        let mut ti = empty_track_info();
        ti.duration_ms = Some(120_000);
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            isrc: Some("USRC17607839".into()),
            sample_rate: Some(44_100),
            channels: Some(2),
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let sections: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Section(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert!(
            sections.contains(&"Identifiers"),
            "Identifiers section should appear when ISRC is present, got {:?}",
            sections
        );
        assert!(
            sections.contains(&"Audio"),
            "Audio section should appear when sample_rate is present, got {:?}",
            sections
        );
    }

    #[test]
    fn format_metadata_pairs_formats_audio_properties() {
        // Sample rate should render in kHz (e.g. "44.1 kHz") and channels
        // should be human-readable.
        let ti = empty_track_info();
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            sample_rate: Some(44_100),
            channels: Some(2),
            audio_bitrate_kbps: Some(320),
            file_size_bytes: Some(5_242_880), // 5 MiB
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let mut found_rate = false;
        let mut found_channels = false;
        let mut found_bitrate = false;
        let mut found_size = false;
        for row in &rows {
            if let MetadataRow::Field { key, value } = row {
                match *key {
                    "Sample Rate" => {
                        assert!(
                            value.contains("44.1") && value.contains("kHz"),
                            "expected '44.1 kHz', got {:?}",
                            value
                        );
                        found_rate = true;
                    }
                    "Channels" => {
                        assert_eq!(value, "2");
                        found_channels = true;
                    }
                    "Bitrate" => {
                        assert!(
                            value.contains("320") && value.contains("kbps"),
                            "expected '320 kbps', got {:?}",
                            value
                        );
                        found_bitrate = true;
                    }
                    "File Size" => {
                        assert!(
                            value.contains("MB") || value.contains("MiB"),
                            "expected human-readable bytes with MB suffix, got {:?}",
                            value
                        );
                        found_size = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(found_rate, "Sample Rate row missing");
        assert!(found_channels, "Channels row missing");
        assert!(found_bitrate, "Bitrate row missing");
        assert!(found_size, "File Size row missing");
    }

    #[test]
    fn format_metadata_pairs_enriches_with_library_track() {
        // Fields only available on the library `Track` (composer, mb_*,
        // replaygain) should appear in the popup when a library lookup
        // matched.
        let ti = empty_track_info();
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            composer: Some("Hans Zimmer".into()),
            mb_release_id: Some("aaaa-bbbb".into()),
            replaygain_track_gain: Some("-7.20 dB".into()),
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let by_key: std::collections::HashMap<&str, &str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, value } => Some((*key, value.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(by_key.get("Composer"), Some(&"Hans Zimmer"));
        assert_eq!(by_key.get("Release ID"), Some(&"aaaa-bbbb"));
        assert_eq!(by_key.get("Track Gain"), Some(&"-7.20 dB"));
    }

    #[test]
    fn sidebar_icon_accent_is_visible_on_selected_row() {
        // Regression: the sync-status glyph used `selection_bg` as its
        // foreground, which matched the selected row's background and hid
        // the glyph. Selected rows must use a color that contrasts with
        // `selection_bg`.
        for theme in theme::THEMES.iter() {
            let unselected = sidebar_icon_accent(theme, false);
            let selected = sidebar_icon_accent(theme, true);
            assert_eq!(
                unselected, theme.selection_bg,
                "{}: unselected accent should stay as the highlight color",
                theme.name
            );
            assert_ne!(
                selected, theme.selection_bg,
                "{}: selected accent must not collide with the row background",
                theme.name
            );
            assert_eq!(
                selected, theme.selection_text,
                "{}: selected accent should match the row text color",
                theme.name
            );
        }
    }

    #[test]
    fn junction_chars_matches_expected_glyphs() {
        use ratatui::widgets::BorderType;
        assert_eq!(
            junction_chars(BorderType::Plain),
            Some(("\u{2524}", "\u{2534}"))
        );
        assert_eq!(
            junction_chars(BorderType::Rounded),
            Some(("\u{2524}", "\u{2534}"))
        );
        assert_eq!(
            junction_chars(BorderType::Thick),
            Some(("\u{252B}", "\u{253B}"))
        );
        assert_eq!(
            junction_chars(BorderType::Double),
            Some(("\u{2563}", "\u{2569}"))
        );
        // Block-style borders (QuadrantOutside/QuadrantInside) have no clean
        // single-glyph T-junction; we intentionally fall through to None.
        assert_eq!(junction_chars(BorderType::QuadrantOutside), None);
        assert_eq!(junction_chars(BorderType::QuadrantInside), None);
    }

    #[test]
    fn vertical_divider_junctions_matches_expected_glyphs() {
        use ratatui::widgets::BorderType;
        assert_eq!(
            vertical_divider_junctions(BorderType::Plain),
            Some(("\u{252C}", "\u{2534}"))
        );
        assert_eq!(
            vertical_divider_junctions(BorderType::Rounded),
            Some(("\u{252C}", "\u{2534}"))
        );
        assert_eq!(
            vertical_divider_junctions(BorderType::Thick),
            Some(("\u{2533}", "\u{253B}"))
        );
        assert_eq!(
            vertical_divider_junctions(BorderType::Double),
            Some(("\u{2566}", "\u{2569}"))
        );
        assert_eq!(
            vertical_divider_junctions(BorderType::QuadrantOutside),
            None
        );
    }

    #[test]
    fn zune_art_connected_track_count() {
        let art = build_zune_art("4,455", "tracks");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}   4,455    \u{2502}  \u{2502} ",
            " \u{2502}  \u{2502}   tracks   \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_loading_state() {
        let art = build_zune_art("*", "Loading...");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}     *      \u{2502}  \u{2502} ",
            " \u{2502}  \u{2502} Loading... \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_zero_tracks() {
        let art = build_zune_art("0", "tracks");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}     0      \u{2502}  \u{2502} ",
            " \u{2502}  \u{2502}   tracks   \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_large_track_count() {
        let art = build_zune_art("12,345", "tracks");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}   12,345   \u{2502}  \u{2502} ",
            " \u{2502}  \u{2502}   tracks   \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_overflow_truncates() {
        let art = build_zune_art("1234567890ABC", "tracks");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}1234567890AB\u{2502}  \u{2502} ",
            " \u{2502}  \u{2502}   tracks   \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_syncing_state() {
        let art = build_zune_art("*", "Syncing...");
        let expected = vec![
            " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e} ",
            " \u{2502}  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}  \u{2502} ",
            " \u{2502}  \u{2502}     *      \u{2502}  \u{2502} ",
            " \u{2502}  \u{2502} Syncing... \u{2502}  \u{2502} ",
            " \u{2502}  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}  \u{2502} ",
            " \u{2502}                  \u{2502} ",
            " \u{2502}  |<  \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}  >|  \u{2502} ",
            " \u{2502}      \u{2502}    \u{2502}      \u{2502} ",
            " \u{2502}      \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}      \u{2502} ",
            " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f} ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn center_pad_cases() {
        assert_eq!(center_pad("hi", 6), "  hi  ");
        assert_eq!(center_pad("abc", 5), " abc ");
        assert_eq!(center_pad("ab", 5), " ab  "); // odd remainder: extra space on right
        assert_eq!(center_pad("toolong", 4), "tool"); // truncated
        assert_eq!(center_pad("exact", 5), "exact");
        assert_eq!(center_pad("", 4), "    ");
    }

    #[test]
    fn marquee_short_text_no_scroll() {
        assert_eq!(marquee("Hi", 10, 0), "Hi");
    }

    #[test]
    fn truncate_respects_cjk_display_width() {
        // Each CJK char is 2 display columns. 14 chars = 28 columns,
        // so at max=28 the string fits untouched; at max=27 it must be
        // truncated so rendered width is <= 27.
        let s = "自分は此処にいるへきてない"; // 13 CJK chars = 26 cols
        assert_eq!(s.width(), 26);
        assert_eq!(truncate(s, 26).width(), 26);
        assert!(truncate(s, 20).width() <= 20);
        assert!(truncate(s, 10).width() <= 10);
        // Ellipsis branch: result still fits budget.
        let out = truncate(s, 10);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_mixed_ascii_hiragana_budget() {
        // Mixes ASCII with Hiragana (Wide). Output width must stay <= max for
        // every budget so panel borders don't shift.
        let s = "(っ◔◡◔)っ ♥ Computer Class";
        for max in [10usize, 15, 19, 22] {
            let out = truncate(s, max);
            assert!(
                disp_width(&out) <= max,
                "max={max} produced width {} for {out:?}",
                disp_width(&out)
            );
        }
    }

    #[test]
    fn zip_art_lines_uniform_display_width() {
        // Every line of the zip-disk art must render at the same display
        // width so the `|` delimiters stay column-aligned. A long ASCII
        // album name forces the mga-line truncation path; CJK artist forces
        // the word-wrap path.
        use crate::app::App;
        let app = App::new();
        let tests = [
            (
                "GORE",
                "Mortality Salience (2022 Year End Mix) (single)",
                "2022",
                2usize,
                "1 hr 0 min",
            ),
            (
                "(っ◔◡◔)っ ♥ Computer Class",
                "Powered By Flash",
                "2021",
                9,
                "27 min",
            ),
            ("GORE", "耳をつんさくような沈黙 MIX", "2024", 12, "45 min"),
        ];
        for (artist, album, year, tracks, dur) in tests {
            let lines = build_zip_art(&app, album, artist, year, tracks, dur);
            let widths: Vec<usize> = lines
                .iter()
                .map(|l| l.iter().map(|s| disp_width(&s.content)).sum::<usize>())
                .collect();
            let first = widths[0];
            for (i, w) in widths.iter().enumerate() {
                assert_eq!(
                    *w, first,
                    "artist={artist:?} album={album:?}: line {i} width {w} != line 0 width {first}"
                );
            }
        }
    }

    #[test]
    fn center_pad_cjk_exact_width() {
        // "世界" is 4 display cells (2 Wide CJK chars).
        let out = center_pad("世界", 8);
        assert_eq!(disp_width(&out), 8);
    }

    #[test]
    fn truncate_mixed_ascii_cjk() {
        let s = "Hello 世界!"; // H=1,e=1,l=1,l=1,o=1,space=1,世=2,界=2,!=1 = 11
        assert_eq!(s.width(), 11);
        assert_eq!(truncate(s, 11), s);
        // At width 8, must stop before or at 8 columns.
        assert!(truncate(s, 8).width() <= 8);
    }

    #[test]
    fn marquee_cjk_frame_width_is_stable() {
        // A long CJK string that must scroll. The rendered width at every
        // frame should be exactly `width` — wide chars can't straddle the
        // visible window without the function compensating.
        let text = "あいうえおかきくけこ"; // 10 chars × 2 cols = 20 cols
        let width = 7;
        for frame in 0..40 {
            let out = marquee(text, width, frame);
            assert_eq!(
                out.width(),
                width,
                "frame {frame} produced width {} for {out:?}",
                out.width()
            );
        }
    }

    #[test]
    fn marquee_pauses_then_scrolls() {
        let text = "Hello World";
        let width = 5;
        // During pause (first 12 frames), shows start of text.
        assert_eq!(marquee(text, width, 0), "Hello");
        assert_eq!(marquee(text, width, 11), "Hello");
        // After pause, starts scrolling (every 4 frames).
        assert_eq!(marquee(text, width, 16), "ello ");
    }

    #[test]
    fn layout_metrics_compact_tier() {
        let area = Rect::new(0, 0, 80, 24);
        let m = LayoutMetrics::new(area, true, true, true);
        assert_eq!(m.device_width, 0);
        assert_eq!(m.sidebar_width, 20);
        assert_eq!(m.album_width, 22);
        assert_eq!(m.keys_width, 0); // force-hidden
        assert!(!m.show_zip_art);
        assert_eq!(m.footer_left_width, 12);
        assert_eq!(m.player_art_width, 0); // hidden at compact
    }

    #[test]
    fn layout_metrics_standard_tier() {
        let area = Rect::new(0, 0, 120, 30);
        let m = LayoutMetrics::new(area, true, true, true);
        assert_eq!(m.device_width, 28);
        assert_eq!(m.sidebar_width, 24);
        assert_eq!(m.album_width, 24);
        assert_eq!(m.keys_width, 0); // force-hidden
        assert!(m.show_zip_art);
        assert!(m.show_now_playing);
        assert_eq!(m.player_art_width, 14);
    }

    #[test]
    fn layout_metrics_full_tier() {
        let area = Rect::new(0, 0, 160, 40);
        let m = LayoutMetrics::new(area, true, true, true);
        assert_eq!(m.device_width, 36);
        assert_eq!(m.sidebar_width, 24);
        assert_eq!(m.album_width, 30);
        assert_eq!(m.keys_width, 24); // shown
        assert!(m.show_zip_art);
        assert_eq!(m.player_art_width, 16);
    }

    #[test]
    fn layout_metrics_full_no_keys() {
        let area = Rect::new(0, 0, 160, 40);
        let m = LayoutMetrics::new(area, false, true, true);
        assert_eq!(m.keys_width, 0);
    }

    #[test]
    fn layout_metrics_short_hides_now_playing() {
        // LayoutMetrics only enforces the absolute floor below which the panel
        // can't physically fit. Height-vs-auto semantics live in
        // `App::should_show_player`, which is the input here.
        let area = Rect::new(0, 0, 160, 11);
        let m = LayoutMetrics::new(area, false, false, true);
        assert!(!m.show_now_playing, "height 11 < 12-row floor");

        let area = Rect::new(0, 0, 160, 12);
        let m = LayoutMetrics::new(area, false, false, true);
        assert!(m.show_now_playing, "height 12 meets floor");

        // Force-hide wins regardless of height.
        let area = Rect::new(0, 0, 160, 40);
        let m = LayoutMetrics::new(area, false, false, false);
        assert!(!m.show_now_playing);
    }

    #[test]
    fn layout_metrics_no_album_browser() {
        let area = Rect::new(0, 0, 160, 40);
        let m = LayoutMetrics::new(area, false, false, false);
        assert_eq!(m.album_width, 0);
        assert_eq!(m.sidebar_width, 28); // wider without album browser
    }

    #[test]
    fn layout_panel_widths_matches_metrics() {
        // Verify the shared helper returns the same values as LayoutMetrics::new.
        for (w, show_keys, has_albums) in [
            (80u16, false, true),
            (120, true, true),
            (160, true, true),
            (160, false, false),
        ] {
            let area = Rect::new(0, 0, w, 30);
            let m = LayoutMetrics::new(area, show_keys, has_albums, false);
            let (dw, sw, aw, kw) = LayoutMetrics::panel_widths(w, show_keys, has_albums);
            assert_eq!(
                (dw, sw, aw, kw),
                (m.device_width, m.sidebar_width, m.album_width, m.keys_width)
            );
        }
    }

    // -- humanize_relative_ms / format_iso_date (Phase 3 commit 5) --

    #[test]
    fn humanize_just_now_for_under_one_minute() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now, now), "just now");
        assert_eq!(humanize_relative_ms(now - 30_000, now), "just now"); // 30s ago
        assert_eq!(humanize_relative_ms(now - 59_999, now), "just now");
    }

    #[test]
    fn humanize_future_clamps_to_just_now() {
        // Clock skew between TUI and device shouldn't show "in 5 minutes".
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now + 300_000, now), "just now");
    }

    #[test]
    fn humanize_minutes() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now - 60_000, now), "1 minute ago");
        assert_eq!(humanize_relative_ms(now - 600_000, now), "10 minutes ago");
        assert_eq!(
            humanize_relative_ms(now - 59 * 60_000, now),
            "59 minutes ago"
        );
    }

    #[test]
    fn humanize_hours() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now - 3_600_000, now), "1 hour ago");
        assert_eq!(
            humanize_relative_ms(now - 5 * 3_600_000, now),
            "5 hours ago"
        );
    }

    #[test]
    fn humanize_yesterday_then_days() {
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        assert_eq!(humanize_relative_ms(now - day, now), "yesterday");
        assert_eq!(humanize_relative_ms(now - 3 * day, now), "3 days ago");
        assert_eq!(humanize_relative_ms(now - 6 * day, now), "6 days ago");
    }

    #[test]
    fn humanize_weeks() {
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        assert_eq!(humanize_relative_ms(now - 7 * day, now), "1 week ago");
        assert_eq!(humanize_relative_ms(now - 14 * day, now), "2 weeks ago");
        assert_eq!(humanize_relative_ms(now - 28 * day, now), "4 weeks ago");
    }

    #[test]
    fn humanize_falls_through_to_iso_date_after_a_month() {
        // 1_700_000_000_000 ms = 2023-11-14 22:13:20 UTC.
        // Subtract 60 days → 2023-09-15.
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        let result = humanize_relative_ms(now - 60 * day, now);
        assert_eq!(result, "2023-09-15");
    }

    #[test]
    fn format_iso_date_unix_epoch() {
        assert_eq!(format_iso_date(0), "1970-01-01");
    }

    #[test]
    fn format_iso_date_known_dates() {
        // 2023-11-14 22:13:20 UTC.
        assert_eq!(format_iso_date(1_700_000_000), "2023-11-14");
        // 2024-02-29 00:00:00 UTC — leap day exercises the Feb-29 branch.
        assert_eq!(format_iso_date(1_709_164_800), "2024-02-29");
        // 2000-03-01 — year 2000 *is* a leap year (`y % 400 == 0`), so this
        // is the day after Feb 29; exercises `is_leap_year`'s `% 400` branch.
        assert_eq!(format_iso_date(951_868_800), "2000-03-01");
        // 2100-02-28 + 1 day → 2100-03-01: year 2100 is *not* a leap year
        // (`y % 100 == 0 && y % 400 != 0`); guards against the most common
        // leap-year miscalculation.
        let day = 86_400u64;
        let feb28_2100 = 4_107_456_000;
        assert_eq!(format_iso_date(feb28_2100), "2100-02-28");
        assert_eq!(format_iso_date(feb28_2100 + day), "2100-03-01");
    }

    // -- format_metadata_pairs: aggregate listening rows --

    #[test]
    fn format_metadata_pairs_renders_aggregate_plays_and_skips() {
        let mut ti = empty_track_info();
        ti.play_count = Some(9);
        ti.skip_count = Some(2);
        let rows = format_metadata_pairs(&ti, None);
        let listening_rows: Vec<(&str, &str)> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, value } => Some((*key, value.as_str())),
                _ => None,
            })
            .collect();
        assert!(listening_rows.contains(&("Plays", "9")));
        assert!(listening_rows.contains(&("Skips", "2")));
    }

    #[test]
    fn format_metadata_pairs_renders_last_played_when_set() {
        let mut ti = empty_track_info();
        ti.play_count = Some(1);
        ti.last_played_at_ms = Some(1_700_000_000_000);
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(
            keys.contains(&"Last played"),
            "Last played row must appear when last_played_at_ms is Some"
        );
    }

    #[test]
    fn format_metadata_pairs_omits_last_played_when_only_device_plays() {
        // Mirrors the case the user would see for a track that only has
        // device-merged plays (no TUI play): play_count is set but
        // last_played_at_ms stays None because we don't fabricate a
        // timestamp from sync time.
        let mut ti = empty_track_info();
        ti.play_count = Some(7);
        ti.last_played_at_ms = None;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Plays"));
        assert!(!keys.contains(&"Last played"));
    }

    #[test]
    fn format_metadata_pairs_omits_skips_row_when_unset() {
        // Skip count of None → no "Skips" row at all (the `tracks_to_info`
        // builder sets it to None when the sidecar value is 0).
        let mut ti = empty_track_info();
        ti.play_count = Some(5);
        ti.skip_count = None;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Plays"));
        assert!(!keys.contains(&"Skips"));
    }

    #[test]
    fn format_metadata_pairs_listening_section_appears_off_device() {
        // Aggregate stats are valid even when the track isn't on the
        // device — the user might have only ever played in the TUI.
        let mut ti = empty_track_info();
        ti.on_device = false;
        ti.play_count = Some(4);
        let rows = format_metadata_pairs(&ti, None);
        let sections: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Section(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert!(sections.contains(&"Listening"));
    }

    #[test]
    fn format_metadata_pairs_last_synced_only_when_on_device() {
        // The "Last synced" row needs both on_device == true AND a
        // last_synced_from_device_at_ms timestamp.
        let mut ti = empty_track_info();
        ti.on_device = false;
        ti.last_synced_from_device_at_ms = Some(1_700_000_000_000);
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(
            !keys.contains(&"Last synced"),
            "Last synced row must not appear off-device"
        );

        ti.on_device = true;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Last synced"));
    }
}
