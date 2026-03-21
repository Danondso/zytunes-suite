use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use crate::app::{format_duration, App, DeviceStatus, Panel, SidebarMode, SortColumn, SyncStatus};
use crate::theme;

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Optional keys panel on the right.
    let keys_width = if app.show_keys { 24 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(keys_width)])
        .split(size);

    let content_area = outer[0];

    // Main vertical layout: top area + bottom bar + footer
    let has_queue = !app.sync_queue.is_empty()
        || matches!(app.sync_status, SyncStatus::Running { .. })
        || matches!(app.sync_status, SyncStatus::Complete { .. })
        || !app.sync_log.is_empty();
    let bottom_height = if has_queue { 8 } else { 0 };

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),                      // top area (library + tracks + device)
            Constraint::Length(bottom_height as u16), // sync queue
            Constraint::Length(3),                    // footer
        ])
        .split(content_area);

    // Top area: split into sidebar + main content vertically,
    // then main content into tracks + device horizontally.
    let has_device = app.device_status != DeviceStatus::Disconnected;
    let device_height = if has_device { 8 } else { 3 };

    let top_vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(device_height)])
        .split(main_chunks[0]);

    let top_horizontal = if app.has_album_browser() {
        // 3-column layout: sidebar | albums | tracks
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(28),
                Constraint::Length(36),
                Constraint::Min(24),
            ])
            .split(top_vertical[0])
    } else {
        // 2-column layout: sidebar | tracks
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(30)])
            .split(top_vertical[0])
    };

    // Draw panels.
    draw_sidebar(f, app, top_horizontal[0]);
    if app.has_album_browser() {
        draw_album_browser(f, app, top_horizontal[1]);
        draw_track_list(f, app, top_horizontal[2]);
    } else {
        draw_track_list(f, app, top_horizontal[1]);
    }
    draw_device_panel(f, app, top_vertical[1]);

    if has_queue {
        let bottom_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
            .split(main_chunks[1]);
        draw_sync_queue(f, app, bottom_cols[0]);
        draw_sync_log(f, app, bottom_cols[1]);
    }

    draw_footer(f, app, main_chunks[2]);

    if app.show_keys {
        draw_keys_panel(f, app, outer[1]);
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

fn draw_device_panel(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.active_panel == Panel::Device;
    let border_style = if is_active {
        Style::default().fg(theme::SELECTION_BG)
    } else {
        theme::border()
    };

    let spinner = app.spinner_frame();
    let title = match app.device_status {
        DeviceStatus::Disconnected => " Device [c: connect] ".to_string(),
        DeviceStatus::Detecting => format!(" Device — {} Detecting... ", spinner),
        DeviceStatus::Connecting => format!(" Device — {} Connecting... ", spinner),
        DeviceStatus::Connected => {
            let name = app.device_name.as_deref().unwrap_or("Zune");
            if app.device_loading_tracks {
                format!(" {} — {} Loading tracks... ", spinner, name)
            } else {
                format!(" {} — {} tracks ", name, app.device_tracks.len())
            }
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    f.render_widget(block, area);

    match app.device_status {
        DeviceStatus::Disconnected => {
            let p =
                Paragraph::new("No device connected. Press 'c' to connect.").style(theme::dim());
            f.render_widget(p, inner);
        }
        DeviceStatus::Detecting | DeviceStatus::Connecting => {}
        DeviceStatus::Connected => {
            draw_device_connected(f, app, inner);
        }
    }
}

fn draw_device_connected(f: &mut Frame, app: &App, area: Rect) {
    let zune_art = [
        "  ┌─────────┐  ",
        "  │  ┌───┐  │  ",
        "  │  │ Z │  │  ",
        "  │  └───┘  │  ",
        "  │   (●)   │  ",
        "  └─────────┘  ",
    ];

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(18),
            Constraint::Min(20),
            Constraint::Min(24),
        ])
        .split(area);

    // ASCII art.
    let art_lines: Vec<Line> = zune_art
        .iter()
        .map(|l| Line::from(Span::styled(*l, theme::dim())))
        .collect();
    let art = Paragraph::new(art_lines);
    f.render_widget(art, chunks[0]);

    // Device info (left column).
    let name = app.device_name.as_deref().unwrap_or("Zune 30");
    let mut info: Vec<Line> = vec![Line::from(Span::styled(
        name,
        Style::default().add_modifier(Modifier::BOLD),
    ))];

    if let Some(ref fw) = app.device_firmware {
        info.push(Line::from(vec![
            Span::styled("FW: ", theme::dim()),
            Span::raw(fw.as_str()),
        ]));
    }

    if let Some(ref mfr) = app.device_manufacturer {
        info.push(Line::from(vec![
            Span::styled("Mfr: ", theme::dim()),
            Span::raw(mfr.as_str()),
        ]));
    }

    if let Some(ref mode) = app.device_usb_mode {
        info.push(Line::from(vec![
            Span::styled("USB: ", theme::dim()),
            Span::raw(mode.as_str()),
        ]));
    }

    if let Some(ref serial) = app.device_serial {
        let display = if serial.len() > 12 {
            format!("{}...", &serial[..12])
        } else {
            serial.clone()
        };
        info.push(Line::from(vec![
            Span::styled("S/N: ", theme::dim()),
            Span::raw(display),
        ]));
    }

    let info_p = Paragraph::new(info);
    f.render_widget(info_p, chunks[1]);

    // Storage + tracks info (right column).
    let track_count = app.device_tracks.len();
    let mut right_info: Vec<Line> = Vec::new();

    if app.device_loading_tracks {
        right_info.push(Line::from(Span::styled("Loading tracks...", theme::dim())));
    } else {
        right_info.push(Line::from(format!("{} tracks on device", track_count)));
    }

    if let Some(ref storage) = app.device_storage {
        let total_gb = storage.total_bytes as f64 / 1_073_741_824.0;
        let free_gb = storage.free_bytes as f64 / 1_073_741_824.0;
        let used_gb = storage.used_bytes as f64 / 1_073_741_824.0;
        right_info.push(Line::from(format!(
            "{:.1} GB / {:.1} GB ({:.1} GB free)",
            used_gb, total_gb, free_gb
        )));
        // Simple text gauge.
        let bar_width = 20usize;
        let filled = (bar_width as f64 * storage.used_percent as f64 / 100.0) as usize;
        let empty = bar_width.saturating_sub(filled);
        right_info.push(Line::from(vec![
            Span::raw("["),
            Span::styled("=".repeat(filled), Style::default().fg(theme::PROGRESS_BAR)),
            Span::styled(" ".repeat(empty), Style::default().fg(theme::PROGRESS_BG)),
            Span::raw(format!("] {}%", storage.used_percent)),
        ]));
    }

    right_info.push(Line::from(""));
    right_info.push(Line::from(Span::styled(
        "r: refresh | d: disconnect",
        theme::dim(),
    )));

    let right_p = Paragraph::new(right_info);
    f.render_widget(right_p, chunks[2]);
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

                let hints = "Enter/S: sync | d: remove | C: clear";
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
    let center = match app.device_status {
        DeviceStatus::Disconnected => "No device".to_string(),
        DeviceStatus::Detecting => "Detecting...".to_string(),
        DeviceStatus::Connecting => "Connecting...".to_string(),
        DeviceStatus::Connected => {
            let name = app.device_name.as_deref().unwrap_or("Zune");
            format!("Connected: {}", name)
        }
    };
    let right = "h:keys | Tab:panels | c:connect | a:add | S:sync | q:quit | ?:help";

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Min(20),
            Constraint::Length(right.len() as u16 + 1),
        ])
        .split(inner);

    f.render_widget(Paragraph::new(left).style(theme::footer()), chunks[0]);
    f.render_widget(
        Paragraph::new(center)
            .alignment(Alignment::Center)
            .style(theme::footer()),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(format!("{} ", right))
            .alignment(Alignment::Right)
            .style(theme::footer()),
        chunks[2],
    );
}

fn draw_toast(f: &mut Frame, msg: &str, is_error: bool) {
    let area = f.area();
    let width = (msg.len() as u16 + 4).min(area.width - 4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(5);
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
