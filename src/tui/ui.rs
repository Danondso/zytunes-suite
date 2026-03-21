use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse, BRAILLE_ONE};

use crate::app::{format_duration, format_with_commas, App, DeviceStatus, Panel, SidebarMode, SortColumn, SyncStatus};
use crate::theme;

fn throbber_symbol(state: &ThrobberState) -> String {
    Throbber::default()
        .throbber_set(BRAILLE_ONE)
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

    // Outer horizontal: device left | middle content | keys right
    let keys_width = if app.show_keys { 24 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(26),
            Constraint::Min(40),
            Constraint::Length(keys_width),
        ])
        .split(size);

    // Left column: device info + sync queue + log
    draw_device_left_panel(f, app, outer[0]);

    // Middle: browser area + footer
    let middle = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(outer[1]);

    // Browser columns: sidebar | optional albums | tracks
    let browser = if app.has_album_browser() {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(24),
                Constraint::Length(30),
                Constraint::Min(20),
            ])
            .split(middle[0])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(30)])
            .split(middle[0])
    };

    draw_sidebar(f, app, browser[0]);
    if app.has_album_browser() {
        draw_album_browser(f, app, browser[1]);
        draw_track_list(f, app, browser[2]);
    } else {
        draw_track_list(f, app, browser[1]);
    }

    draw_footer(f, app, middle[1]);

    if app.show_keys {
        draw_keys_panel(f, app, outer[2]);
    }

    // Toast overlay.
    if let Some((ref msg, _, is_error)) = app.toast_message {
        draw_toast(f, msg, is_error);
    }

    // Help overlay.
    if app.show_help {
        draw_help_overlay(f);
    }

    // Search overlay.
    if app.search_active {
        draw_search_overlay(f, app);
    }
}

fn draw_startup(f: &mut Frame, app: &App, area: Rect) {
    let symbol = throbber_symbol(&app.throbber_state);

    let path_display = app
        .library_path
        .as_deref()
        .unwrap_or("Library.xml");

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("zytunes", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", symbol), theme::dim()),
            Span::raw("Parsing library..."),
        ]),
        Line::from(""),
        Line::from(Span::styled(path_display, theme::dim())),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
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
    let is_active = app.active_panel == Panel::Library;
    let mode_label = match app.sidebar_mode {
        SidebarMode::Artists => "Artists",
        SidebarMode::Albums => "Albums",
        SidebarMode::Playlists => "Playlists",
    };

    let title = format!(" {} ", mode_label);
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(theme::SIDEBAR_BG));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.sidebar_items.is_empty() {
        let msg = if app.library.is_none() {
            "No library loaded"
        } else {
            "(empty)"
        };
        let p = Paragraph::new(msg).style(theme::dim());
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
                theme::sidebar_item_selected()
            } else {
                theme::sidebar_item()
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
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(theme::SIDEBAR_BG));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.album_list.is_empty() {
        let p = Paragraph::new("No albums").style(theme::dim());
        f.render_widget(p, inner);
        return;
    }

    let visible_height = inner.height as usize;
    let scroll = compute_scroll(app.album_selected, visible_height, app.album_list.len());

    let items: Vec<ListItem> = app
        .album_list
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, album)| {
            let is_selected = i == app.album_selected;
            let style = if is_selected {
                theme::sidebar_item_selected()
            } else {
                theme::sidebar_item()
            };
            let prefix = if is_selected { "> " } else { "  " };
            let meta = match album.year {
                Some(y) => format!(" ({}, {}t)", y, album.track_count),
                None => format!(" ({}t)", album.track_count),
            };
            let label = format!(
                "{}{}{}",
                prefix,
                truncate(
                    &album.name,
                    inner.width.saturating_sub(meta.len() as u16 + 3) as usize
                ),
                meta
            );
            ListItem::new(label).style(style)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn draw_track_list(f: &mut Frame, app: &App, area: Rect) {
    if app.has_album_browser() {
        draw_album_detail(f, app, area);
    } else {
        draw_track_table(f, app, area);
    }
}

fn draw_album_detail(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.active_panel == Panel::TrackList;
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };

    let album = app.album_list.get(app.album_selected);
    let album_name = album.map(|a| a.name.as_str()).unwrap_or("No Album");
    let album_artist = album.map(|a| a.artist.as_str()).unwrap_or("");
    let title = format!(
        " {} ",
        truncate(album_name, area.width.saturating_sub(4) as usize)
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(theme::MAIN_BG));
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

    // Zip disk ASCII art by mga — https://www.asciiart.eu/art/324546af3173c962
    // Album/artist/track info embedded into the disk body and label.
    let art_lines = build_zip_art(album_name, album_artist, &year_str, track_count, &dur_str);

    // Side-by-side: zip art on the left, track listing on the right.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(31), Constraint::Min(12)])
        .split(inner);

    let art = Paragraph::new(art_lines);
    f.render_widget(art, cols[0]);

    draw_album_track_list(f, app, cols[1]);
}

fn draw_album_track_list(f: &mut Frame, app: &App, area: Rect) {
    if app.track_list.is_empty() {
        let p = Paragraph::new("  No tracks").style(theme::dim());
        f.render_widget(p, area);
        return;
    }

    let is_active = app.active_panel == Panel::TrackList;
    let visible_height = area.height as usize;
    let scroll = compute_scroll(app.track_selected, visible_height, app.track_list.len());

    let rows: Vec<Row> = app
        .track_list
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, track)| {
            let is_selected = i == app.track_selected && is_active;
            let bg = if is_selected {
                theme::SELECTION_BG
            } else if i % 2 == 0 {
                theme::MAIN_BG
            } else {
                theme::ALT_ROW_BG
            };
            let fg = if is_selected {
                theme::SELECTION_TEXT
            } else {
                theme::SIDEBAR_TEXT
            };

            let num = track
                .track_number
                .map(|n| format!("{}.", n))
                .unwrap_or_default();
            let dur = track.duration_ms.map(format_duration).unwrap_or_default();

            Row::new(vec![
                Cell::from(num),
                Cell::from(track.name.clone()),
                Cell::from(dur),
            ])
            .style(Style::default().bg(bg).fg(fg))
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(12),
        Constraint::Length(6),
    ];

    let table = Table::new(rows, widths);
    f.render_widget(table, area);
}

fn draw_track_table(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.active_panel == Panel::TrackList;
    let title = format!(" Tracks ({}) ", app.track_list.len());
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .style(Style::default().bg(theme::MAIN_BG));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.track_list.is_empty() {
        let msg = if app.library.is_none() {
            "Load a library to browse tracks"
        } else {
            "Select an item from the sidebar"
        };
        let p = Paragraph::new(msg).style(theme::dim());
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
    let header = Row::new(header_cells).style(theme::header()).height(1);

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
                theme::SELECTION_BG
            } else if i % 2 == 0 {
                theme::MAIN_BG
            } else {
                theme::ALT_ROW_BG
            };
            let fg = if i == app.track_selected {
                theme::SELECTION_TEXT
            } else {
                theme::SIDEBAR_TEXT
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
    let is_active = app.active_panel == Panel::Device;
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };

    let title = match app.device_status {
        DeviceStatus::Disconnected => " Device [c] ".to_string(),
        DeviceStatus::Detecting | DeviceStatus::Connecting => " Device ".to_string(),
        DeviceStatus::Connected => {
            let name = app.device_name.as_deref().unwrap_or("Zune");
            format!(" {} ", name)
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
        .title_alignment(Alignment::Center);

    let inner = block.inner(area);
    f.render_widget(block, area);

    match app.device_status {
        DeviceStatus::Disconnected => {
            let p = Paragraph::new("No device.\nPress 'c' to connect.").style(theme::dim());
            f.render_widget(p, inner);
        }
        DeviceStatus::Detecting | DeviceStatus::Connecting => {
            let symbol = throbber_symbol(&app.throbber_state);
            let p = Paragraph::new(format!(" {} Please wait...", symbol)).style(theme::dim());
            f.render_widget(p, inner);
        }
        DeviceStatus::Connected => {
            draw_device_info_connected(f, app, inner);
        }
    }
}

fn draw_device_info_connected(f: &mut Frame, app: &App, area: Rect) {
    let is_syncing = matches!(app.sync_status, SyncStatus::Running { .. });
    let is_busy = app.device_loading_tracks;

    // Screen content: two lines inside the screen area.
    let (screen_line1, screen_line2) = if is_syncing {
        let symbol = throbber_symbol(&app.throbber_state);
        (symbol, "Syncing...".to_string())
    } else if is_busy {
        let symbol = throbber_symbol(&app.throbber_state);
        (symbol, "Loading...".to_string())
    } else {
        let count = app.device_tracks.len();
        let formatted = format_with_commas(count);
        (formatted, "tracks".to_string())
    };

    let zune_art = build_zune_art(&screen_line1, &screen_line2);

    let mut lines: Vec<Line> = Vec::new();

    // Zune ASCII art (centered).
    for l in &zune_art {
        lines.push(
            Line::from(Span::styled(l.as_str(), theme::dim())).alignment(Alignment::Center),
        );
    }

    // Device info lines below art.
    if let Some(ref fw) = app.device_firmware {
        lines.push(Line::from(vec![
            Span::styled(" FW: ", theme::dim()),
            Span::raw(fw.as_str()),
        ]));
    }
    if let Some(ref mfr) = app.device_manufacturer {
        lines.push(Line::from(vec![
            Span::styled(" Mfr: ", theme::dim()),
            Span::raw(mfr.as_str()),
        ]));
    }
    if let Some(ref mode) = app.device_usb_mode {
        lines.push(Line::from(vec![
            Span::styled(" USB: ", theme::dim()),
            Span::raw(mode.as_str()),
        ]));
    }
    if let Some(ref serial) = app.device_serial {
        let display = if serial.chars().count() > 12 {
            format!("{}...", serial.chars().take(12).collect::<String>())
        } else {
            serial.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(" S/N: ", theme::dim()),
            Span::raw(display),
        ]));
    }

    // Storage info.
    if let Some(ref storage) = app.device_storage {
        let total_gb = storage.total_bytes as f64 / 1_073_741_824.0;
        let free_gb = storage.free_bytes as f64 / 1_073_741_824.0;
        let used_gb = storage.used_bytes as f64 / 1_073_741_824.0;
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            " {:.1}/{:.1} GB ({:.1} free)",
            used_gb, total_gb, free_gb
        )));
        let bar_width = 16usize;
        let filled = (bar_width as f64 * storage.used_percent as f64 / 100.0) as usize;
        let empty = bar_width.saturating_sub(filled);
        lines.push(Line::from(vec![
            Span::raw(" ["),
            Span::styled("=".repeat(filled), Style::default().fg(theme::PROGRESS_BAR)),
            Span::styled(" ".repeat(empty), Style::default().fg(theme::PROGRESS_BG)),
            Span::raw(format!("] {}%", storage.used_percent)),
        ]));
    }

    if !app.device_loading_tracks {
        lines.push(Line::from(format!(
            " {} tracks on device",
            app.device_tracks.len()
        )));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, area);
}

fn draw_sync_queue(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.active_panel == Panel::SyncQueue;
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };

    match app.sync_status {
        SyncStatus::Running { current, total } => {
            let pct = if total > 0 {
                (current as f64 / total as f64 * 100.0) as u16
            } else {
                0
            };
            let title = format!(" Syncing [{}/{}] ", current, total);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title);
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(inner);

            let status_line = format!(
                "[{}/{}] Uploading: {}",
                current, total, app.sync_current_track
            );
            f.render_widget(Paragraph::new(status_line), chunks[0]);

            let gauge = Gauge::default()
                .gauge_style(
                    Style::default()
                        .fg(theme::PROGRESS_BAR)
                        .bg(theme::PROGRESS_BG),
                )
                .percent(pct)
                .label(format!("{}%", pct));
            f.render_widget(gauge, chunks[1]);

            f.render_widget(Paragraph::new("Esc: cancel").style(theme::dim()), chunks[2]);
        }
        SyncStatus::Complete {
            success, failed, ..
        } => {
            let title = format!(" Sync Complete — {} done, {} failed ", success, failed);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title);
            f.render_widget(block, area);
        }
        SyncStatus::Idle => {
            let total_tracks = app.total_queue_tracks();
            let title = format!(
                " Sync Queue — {} item(s), {} track(s) ",
                app.sync_queue.len(),
                total_tracks
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title)
                .style(Style::default().bg(theme::MAIN_BG));
            let inner = block.inner(area);
            f.render_widget(block, area);

            if app.sync_queue.is_empty() {
                let p =
                    Paragraph::new("Queue is empty. Press 'a' to add tracks.").style(theme::dim());
                f.render_widget(p, inner);
            } else {
                let items: Vec<ListItem> = app
                    .sync_queue
                    .iter()
                    .enumerate()
                    .map(|(i, q)| {
                        let style = if i == app.queue_selected && is_active {
                            theme::sidebar_item_selected()
                        } else {
                            Style::default().fg(theme::SIDEBAR_TEXT).bg(theme::MAIN_BG)
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
                f.render_widget(Paragraph::new(hints).style(theme::dim()), chunks[1]);
            }
        }
    }
}

fn draw_sync_log(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(" Log ")
        .style(Style::default().bg(theme::MAIN_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.sync_log.is_empty() {
        let p = Paragraph::new("No messages yet.").style(theme::dim());
        f.render_widget(p, inner);
        return;
    }

    // Show the most recent messages that fit, scrolled to the bottom.
    let visible = inner.height as usize;
    let start = app.sync_log.len().saturating_sub(visible);
    let lines: Vec<Line> = app.sync_log[start..]
        .iter()
        .map(|msg| {
            let style = if msg.contains("FAILED") {
                Style::default().fg(theme::ERROR_TEXT)
            } else if msg.starts_with("  OK") || msg.contains("Done:") {
                Style::default().fg(theme::SUCCESS_TEXT)
            } else {
                Style::default().fg(theme::SIDEBAR_TEXT)
            };
            Line::from(Span::styled(msg.as_str(), style))
        })
        .collect();

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn draw_keys_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(" Keys [h] ")
        .style(Style::default().bg(theme::SIDEBAR_BG));
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
        ("/", "Search"),
        ("c", "Connect"),
    ];
    lines.push(Line::from(Span::styled(
        " Global",
        Style::default()
            .fg(theme::HEADER_TEXT)
            .add_modifier(Modifier::BOLD),
    )));
    for (key, desc) in &global {
        lines.push(key_line(key, desc));
    }

    lines.push(Line::from(""));

    // Context-sensitive keys.
    let (section, keys): (&str, Vec<(&str, &str)>) = match app.active_panel {
        Panel::Library => (
            " Library",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("\u{2190}\u{2192}", "Skip A→B→C"),
                ("Enter", "Select"),
                ("a", "Add to queue"),
            ],
        ),
        Panel::Albums => (
            " Albums",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("Enter", "View tracks"),
                ("a", "Add album"),
            ],
        ),
        Panel::TrackList => (
            " Tracks",
            vec![
                ("\u{2191}\u{2193}", "Navigate"),
                ("s", "Cycle sort"),
                ("a", "Add track"),
                ("A", "Add all"),
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
            .fg(theme::HEADER_TEXT)
            .add_modifier(Modifier::BOLD),
    )));
    for (key, desc) in &keys {
        lines.push(key_line(key, desc));
    }

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn key_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {:>6} ", key),
            Style::default()
                .fg(theme::SELECTION_BG)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(theme::SIDEBAR_TEXT)),
    ])
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .style(theme::footer());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let left = format!(" {} tracks", app.track_count());
    let right = "Tab:panels | a:add | S:sync | q:quit | ?:help";

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(16), Constraint::Min(20)])
        .split(inner);

    f.render_widget(Paragraph::new(left).style(theme::footer()), chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{} ", right))
            .alignment(Alignment::Right)
            .style(theme::footer()),
        chunks[1],
    );
}

fn draw_toast(f: &mut Frame, msg: &str, is_error: bool) {
    let area = f.area();
    if area.width < 8 || area.height < 3 {
        return;
    }
    let width = (msg.len() as u16 + 4).min(area.width - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(3)) / 2;
    let rect = Rect::new(x, y, width, 3);

    f.render_widget(Clear, rect);

    let style = if is_error {
        theme::error()
    } else {
        theme::success()
    };
    let block = Block::default().borders(Borders::ALL).border_style(style);
    let p = Paragraph::new(format!(" {} ", msg))
        .block(block)
        .style(style);
    f.render_widget(p, rect);
}

fn draw_help_overlay(f: &mut Frame) {
    let area = f.area();
    let width = 50u16.min(area.width - 4);
    let height = 22u16.min(area.height - 4);
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
        "  Device",
        "  c           Connect to Zune",
        "  r           Refresh device tracks",
        "  Esc         Close / cancel",
        "  q           Quit",
    ];

    let lines: Vec<Line> = help_text.iter().map(|l| Line::from(*l)).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::SELECTION_BG))
        .title(" Help — press Esc to close ");
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(p, rect);
}

fn draw_search_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    let width = 40u16.min(area.width - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 3;
    let rect = Rect::new(x, y, width, 3);

    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::SELECTION_BG))
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
/// Art by mga — https://www.asciiart.eu/art/324546af3173c962
fn build_zip_art<'a>(
    album: &'a str,
    artist: &'a str,
    year: &'a str,
    track_count: usize,
    duration: &'a str,
) -> Vec<Line<'a>> {
    let dim = Style::default().fg(theme::DIM_TEXT);
    let bold = Style::default()
        .fg(theme::SIDEBAR_TEXT)
        .add_modifier(Modifier::BOLD);
    let info = Style::default().fg(theme::HEADER_TEXT);

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
    let artist_padded = pad(artist, body_w);

    // Word-wrap album name across two lines (line 2 + mga line).
    let album_chars: Vec<char> = album.chars().collect();
    let (album_line1, album_line2) = if album_chars.len() <= body_w {
        (pad(album, body_w), None)
    } else {
        // Find a word break point near body_w.
        let break_at = album[..album.char_indices()
            .take(body_w)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(body_w)]
            .rfind(' ')
            .unwrap_or(body_w.min(album.len()));
        let first = pad(&album[..break_at], body_w);
        let rest = album[break_at..].trim_start();
        // Second line shares space with "mga" — 14 chars for text, then " mga "
        let rest_w = 13;
        let second = format!("{} mga ", pad(rest, rest_w));
        (first, Some(second))
    };

    // mga line: either shows album overflow or just the attribution.
    let mga_line = match album_line2 {
        Some(ref wrapped) => wrapped.clone(),
        None => "              mga  ".to_string(),
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
        // Artist name in disk body
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(artist_padded, bold),
            Span::styled(" |  : ", dim),
        ]),
        // Album name line 1
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(album_line1, info),
            Span::styled(" |  : ", dim),
        ]),
        // Album name overflow / mga attribution
        Line::from(vec![
            Span::styled(" :  | ", dim),
            Span::styled(mga_line, if album_line2.is_some() { info } else { dim }),
            Span::styled("|  : ", dim),
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
        chars[..max - 1].iter().collect::<String>() + "…"
    } else {
        chars[..max].iter().collect()
    }
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
        " ╭──────────────────╮ ".to_string(),
        " │  ┌────────────┐  │ ".to_string(),
        format!(" │  │{}│  │ ", line1),
        format!(" │  │{}│  │ ", line2),
        " │  └────────────┘  │ ".to_string(),
        " │                  │ ".to_string(),
        " │  |<  ╭────╮  >|  │ ".to_string(),
        " │      │    │      │ ".to_string(),
        " │      ╰────╯      │ ".to_string(),
        " ╰──────────────────╯ ".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zune_art_connected_track_count() {
        let art = build_zune_art("4,455", "tracks");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │   4,455    │  │ ",
            " │  │   tracks   │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_loading_state() {
        let art = build_zune_art("*", "Loading...");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │     *      │  │ ",
            " │  │ Loading... │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_zero_tracks() {
        let art = build_zune_art("0", "tracks");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │     0      │  │ ",
            " │  │   tracks   │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_large_track_count() {
        let art = build_zune_art("12,345", "tracks");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │   12,345   │  │ ",
            " │  │   tracks   │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_overflow_truncates() {
        let art = build_zune_art("1234567890ABC", "tracks");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │1234567890AB│  │ ",
            " │  │   tracks   │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
        ];
        assert_eq!(art, expected);
    }

    #[test]
    fn zune_art_syncing_state() {
        let art = build_zune_art("*", "Syncing...");
        let expected = vec![
            " ╭──────────────────╮ ",
            " │  ┌────────────┐  │ ",
            " │  │     *      │  │ ",
            " │  │ Syncing... │  │ ",
            " │  └────────────┘  │ ",
            " │                  │ ",
            " │  |<  ╭────╮  >|  │ ",
            " │      │    │      │ ",
            " │      ╰────╯      │ ",
            " ╰──────────────────╯ ",
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
}
