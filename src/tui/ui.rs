use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse};

use crate::anim;
use crate::app::{
    format_duration, format_with_commas, App, BrowseMode, DeviceStatus, NowPlaying, Panel,
    PlaybackState, SidebarMode, SortColumn, SyncStatus,
};
use crate::theme;

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
    fn new(area: Rect, show_keys: bool, has_album_browser: bool, has_player: bool) -> Self {
        let w = area.width;
        let h = area.height;

        if w < 100 {
            // Compact: hide device panel, force-hide keys, narrow sidebar.
            LayoutMetrics {
                device_width: 0,
                sidebar_width: 20,
                album_width: if has_album_browser { 22 } else { 0 },
                keys_width: 0,
                show_now_playing: has_player && h >= 20,
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
                show_now_playing: has_player && h >= 20,
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
                show_now_playing: has_player && h >= 20,
                show_zip_art: true,
                footer_left_width: 16,
                player_art_width: 16,
            }
        }
    }

    /// Returns (device_w, sidebar_w, album_w, keys_w) for album art
    /// pre-render calculations in the event loop.
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

    /// Whether the now-playing panel should be shown at the given height.
    pub(crate) fn show_now_playing(height: u16) -> bool {
        height >= 20
    }
}

fn throbber_symbol(state: &ThrobberState, theme_index: usize) -> String {
    Throbber::default()
        .throbber_set(anim::spinner_set_for_theme(theme_index))
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
        app.now_playing.is_some(),
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

    // Search overlay.
    if app.search_active {
        draw_search_overlay(f, app);
    }
}

fn draw_startup(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let symbol = throbber_symbol(&app.throbber_state, app.theme_index);
    let pulse = anim::animated_accent(
        t.accent_color(),
        t.accent_secondary,
        t.accent_anim,
        app.anim_frame,
        40,
    );

    let path_display = app.library_path.as_deref().unwrap_or("Library.xml");

    let revealed = anim::typing_reveal("zytunes", app.anim_frame);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            revealed,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", symbol), Style::default().fg(pulse)),
            Span::raw("Parsing library..."),
        ]),
        Line::from(""),
        Line::from(Span::styled(path_display, t.dim())),
    ];

    let block = t
        .block()
        .border_style(t.dim())
        .title(" Starting ")
        .title_alignment(Alignment::Center);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);

    // Center the panel: 40 wide, 10 tall
    let w = 40u16.min(area.width);
    let h = 10u16.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let centered = Rect::new(x, y, w, h);

    f.render_widget(Clear, centered);
    f.render_widget(paragraph, centered);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::Library;
    let mode_label = match app.sidebar_mode {
        SidebarMode::Artists => "Artists",
        SidebarMode::Albums => "Albums",
        SidebarMode::Playlists => "Playlists",
    };
    let browse_prefix = match app.browse_mode {
        BrowseMode::Library => "",
        BrowseMode::Device => "Zune: ",
    };

    let title = format!(" {}{} ", browse_prefix, mode_label);
    let border_style = if is_active {
        Style::default().fg(t.selection_bg)
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
                } else if app.sidebar_mode == SidebarMode::Playlists {
                    "Playlists not available\nin device view"
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

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, name)| {
            let style = if i == app.sidebar_selected {
                t.sidebar_item_selected()
            } else {
                t.sidebar_item()
            };
            let prefix = if i == app.sidebar_selected {
                "> "
            } else {
                "  "
            };
            ListItem::new(format!(
                "{}{}",
                prefix,
                truncate(name, inner.width as usize - 3)
            ))
            .style(style)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn draw_album_browser(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::Albums;
    let artist = app
        .sidebar_items
        .get(app.sidebar_selected)
        .cloned()
        .unwrap_or_default();
    let title = format!(
        " {} ",
        truncate(&artist, area.width.saturating_sub(4) as usize)
    );
    let border_style = if is_active {
        Style::default().fg(t.selection_bg)
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
        let max_name = inner.width.saturating_sub(3) as usize;
        let display_name = if is_selected && album.name.len() > max_name && max_name > 0 {
            marquee(&album.name, max_name, app.anim_frame)
        } else {
            truncate(&album.name, max_name).to_string()
        };
        let label = format!("{}{}", prefix, display_name);
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
        Style::default().fg(t.selection_bg)
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

        // Split right column: tracks on top, album art below.
        let art_rows = app.album_art_lines.len() as u16;
        let right_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(art_rows)])
            .split(cols[1]);

        draw_album_track_list(f, app, right_split[0]);
        draw_album_art_inline(f, app, right_split[1]);
    } else {
        // Not enough width or compact tier: full-width track list, no zip art.
        draw_album_track_list(f, app, inner);
    }
}

fn draw_album_art_inline(f: &mut Frame, app: &App, area: Rect) {
    if app.album_art_lines.is_empty() || area.height == 0 {
        return;
    }

    let art_w = app.album_art_lines.first().map(|r| r.len()).unwrap_or(0) as u16;
    let x_offset = area.width.saturating_sub(art_w) / 2;

    let pad: String = " ".repeat(x_offset as usize);
    let pad_style = Style::default().bg(app.theme().main_bg);

    let lines: Vec<Line> = app
        .album_art_lines
        .iter()
        .take(area.height as usize)
        .map(|row| {
            let mut spans: Vec<Span> = Vec::with_capacity(row.len() + 1);
            if x_offset > 0 {
                spans.push(Span::styled(pad.as_str(), pad_style));
            }
            for &(_, fg, bg) in row
                .iter()
                .take(area.width.saturating_sub(x_offset) as usize)
            {
                spans.push(Span::styled(
                    "▀",
                    Style::default()
                        .fg(Color::Rgb(fg[0], fg[1], fg[2]))
                        .bg(Color::Rgb(bg[0], bg[1], bg[2])),
                ));
            }
            Line::from(spans)
        })
        .collect();

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
                Row::new(vec![Cell::from(""), Cell::from(disc_label), Cell::from("")])
                    .style(t.dim()),
            ));
            last_disc = Some(track.disc_number);
        }

        let is_selected = i == app.track_selected && is_active;
        let bg = if is_selected {
            t.selection_bg
        } else if i % 2 == 0 {
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

        flat_rows.push((
            Some(i),
            Row::new(vec![
                Cell::from(num),
                Cell::from(track.name.clone()),
                Cell::from(dur),
            ])
            .style(Style::default().bg(bg).fg(fg)),
        ));
    }

    // Scroll based on the selected track's position in the flat list.
    let selected_flat = flat_rows
        .iter()
        .position(|(idx, _)| *idx == Some(app.track_selected))
        .unwrap_or(0);
    let visible_height = area.height as usize;
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
    ];

    let table = Table::new(visible_rows, widths);
    f.render_widget(table, area);
}

fn draw_track_table(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let is_active = app.active_panel == Panel::TrackList;
    let title = format!(" Tracks ({}) ", app.track_list.len());
    let border_style = if is_active {
        Style::default().fg(t.selection_bg)
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
            } else if i % 2 == 0 {
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

            Row::new(vec![
                Cell::from(num),
                Cell::from(track.name.clone()),
                Cell::from(track.artist.clone()),
                Cell::from(track.album.clone()),
                Cell::from(dur),
                Cell::from(kind),
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
        Style::default().fg(t.selection_bg)
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
        .title_alignment(Alignment::Center);

    let inner = block.inner(area);
    f.render_widget(block, area);

    match app.device.status {
        DeviceStatus::Disconnected => {
            let p = Paragraph::new("No device.\nPress 'c' to connect.").style(t.dim());
            f.render_widget(p, inner);
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
        let symbol = throbber_symbol(&app.throbber_state, app.theme_index);
        (symbol, "Syncing...".to_string())
    } else if is_busy {
        let symbol = throbber_symbol(&app.throbber_state, app.theme_index);
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
        Style::default().fg(t.selection_bg)
    } else {
        t.border()
    };

    match app.sync.status {
        SyncStatus::Running { current, total } => {
            let symbol = throbber_symbol(&app.throbber_state, app.theme_index);
            let title = format!(" {} {}/{} ", symbol, current, total);
            let block = t.block().border_style(border_style).title(title);
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
                    let items: Vec<ListItem> = app
                        .sync
                        .queue
                        .iter()
                        .enumerate()
                        .map(|(i, q)| {
                            let style = if i == app.sync.queue_selected && is_active {
                                t.sidebar_item_selected()
                            } else {
                                Style::default().fg(t.sidebar_text).bg(t.main_bg)
                            };
                            ListItem::new(format!("  {} ({} tracks)", q.label, q.tracks.len()))
                                .style(style)
                        })
                        .collect();

                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(2), Constraint::Length(1)])
                        .split(inner);

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
        Panel::Device => (" Device", vec![("r", "Refresh"), ("d", "Disconnect")]),
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
    let skin = anim::player_skin(app.theme_index);

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

    // Split inner area: info (left) | art (right), art hidden at Compact.
    let show_art = art_width > 0 && inner.width > art_width + 20;
    let (info_area, art_area) = if show_art {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(art_width)])
            .split(inner);
        (cols[0], Some(cols[1]))
    } else {
        (inner, None)
    };

    // --- Art (right side, inside the shared block) ---
    if let Some(art_rect) = art_area {
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
        let aw = art_rect.width as usize;
        let ah = art_rect.height as usize;
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
        f.render_widget(Paragraph::new(art_text), art_rect);
    }

    // --- Info (left side) ---
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // track name
            Constraint::Length(1), // artist — album
            Constraint::Length(1), // controls
            Constraint::Length(1), // progress bar
            Constraint::Length(1), // time + hints
            Constraint::Min(0),    // absorb extra
        ])
        .split(info_area);

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

    // Artist — Album
    f.render_widget(
        Paragraph::new(format!(" {} — {}", &np.artist, &np.album)).style(t.dim()),
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
    f.render_widget(Paragraph::new(time_line).style(t.dim()), rows[4]);
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
        "v:library | a:queue rm | D:delete | C:clr | ?:help"
    } else {
        "v:device | a:add | S:sync | q:quit | ?:help"
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
        "  1/2/3       Artists / Albums / Playlists",
        "  v           Toggle Library / Device view",
        "  t           Theme picker",
        "",
        "  Library",
        "  /           Search sidebar",
        "  s           Cycle sort column",
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
    let theme_count = theme::THEMES.len();
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

    let items: Vec<ListItem> = theme::THEMES
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

    // Pad or truncate a string to exactly `w` chars.
    let pad = |s: &str, w: usize| -> String {
        let t = truncate(s, w);
        let chars: Vec<char> = t.chars().collect();
        if chars.len() < w {
            format!("{}{}", t, " ".repeat(w - chars.len()))
        } else {
            t
        }
    };

    // Body lines (upper area) — artist and album name.
    let body_w = 18; // width inside `:  | ` and ` |  :`

    // Word-wrap helper: split text at a word boundary near `width`.
    let word_wrap = |text: &str, width: usize| -> (String, Option<String>) {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= width {
            (pad(text, width), None)
        } else {
            let byte_end = text
                .char_indices()
                .take(width)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(width);
            let break_at = text[..byte_end].rfind(' ').unwrap_or(byte_end);
            let first = pad(&text[..break_at], width);
            let rest = text[break_at..].trim_start().to_string();
            (first, Some(rest))
        }
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
        Line::from(Span::styled(r#" .-|:"""":""""""'''"""":|-.  "#, dim)),
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

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max > 1 {
        chars[..max - 1].iter().collect::<String>() + "\u{2026}"
    } else {
        chars[..max].iter().collect()
    }
}

/// Marquee-scroll a string that's longer than `width`. Scrolls through
/// `text   text` seamlessly, advancing one character every 4 frames, with
/// a pause at the start.
fn marquee(text: &str, width: usize, frame: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width || width == 0 {
        return truncate(text, width);
    }
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
    padded[offset..offset + width].iter().collect()
}

/// Pad/center a string to exactly `w` chars.
fn center_pad(s: &str, w: usize) -> String {
    let len = s.chars().count();
    if len >= w {
        s.chars().take(w).collect()
    } else {
        let left = (w - len) / 2;
        let right = w - len - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
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
        let area = Rect::new(0, 0, 160, 19);
        let m = LayoutMetrics::new(area, false, false, true);
        assert!(!m.show_now_playing);

        let area = Rect::new(0, 0, 160, 20);
        let m = LayoutMetrics::new(area, false, false, true);
        assert!(m.show_now_playing);
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
}
