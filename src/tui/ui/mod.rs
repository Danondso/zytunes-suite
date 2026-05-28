mod metadata;
mod util;

pub use metadata::{format_metadata_pairs, MetadataRow};
use util::{
    center_pad, char_disp_width, compute_scroll, disp_width, marquee, pad_right_to_width, truncate,
};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Padding, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse};

use crate::anim;
use crate::app::{
    format_duration, format_with_commas, AlbumArtCache, App, BrowseMode, DevicePresence,
    DeviceStatus, GenerationFormState, NowPlaying, Panel, PlaybackState, SidebarEntry, SidebarMode,
    SortColumn, SyncStatus,
};
use crate::background::CdStatusEvent;
use crate::theme;
use zytunes::musicbrainz::render_artist_credit;

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
    let full = f.area();

    if app.loading_library {
        draw_startup(f, app, full);
        return;
    }

    // Slice a 1-row CD status bar off the top when there's something CD-related
    // to report. `NoDrive` (and `None`) collapse the row so the existing layout
    // is undisturbed on machines without an optical drive.
    let (cd_bar_area, size) = if show_cd_status_bar(app) {
        let s = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(full);
        (Some(s[0]), s[1])
    } else {
        (None, full)
    };

    let m = LayoutMetrics::new(
        size,
        app.show_keys,
        app.has_album_browser(),
        app.should_show_player(size.height),
    );

    if let Some(area) = cd_bar_area {
        draw_cd_status_bar(f, app, area);
    }

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

    // Tag-manager overlay. Drawn on top of the track-info popup because the
    // `m` key dispatcher routes around the track-info guard.
    if app.tag_manager.is_some() {
        draw_tag_manager_overlay(f, app);
    }

    // CD import overlay — drawn last so it sits on top of everything.
    if app.import_overlay.is_some() {
        draw_import_overlay(f, app);
    }

    // Search overlay.
    if app.search_active {
        draw_search_overlay(f, app);
    }

    // Playlist name input modal (create or rename).
    if app.playlist_name_input.is_some() {
        draw_playlist_name_input(f, app);
    }

    // Add-to-playlist picker.
    if app.add_to_playlist_picker.is_some() {
        draw_add_to_playlist_picker(f, app);
    }

    // Playlist delete confirmation.
    if app.pending_playlist_delete.is_some() {
        draw_confirm_playlist_delete(f, app);
    }

    // Playlist generation form. Drawn last so it stacks above other
    // transient UI (toasts can still appear over it via subsequent ticks).
    if app.generation_form.is_some() {
        draw_generation_form_overlay(f, app);
    }
}

/// Predicate: should the CD status bar render at the top of the screen?
///
/// `NoDrive` and `None` both collapse the bar so users without an optical
/// drive see the unaltered layout. Any other state — disc loaded, disc
/// identified, lookup failed, OR an active rip — surfaces a one-line bar
/// so the user always sees CD-related activity.
pub(crate) fn show_cd_status_bar(app: &App) -> bool {
    if app.cd.rip.is_some() {
        return true;
    }
    matches!(
        app.cd.last_status,
        Some(
            CdStatusEvent::NoMedia { .. }
                | CdStatusEvent::UnknownDisc { .. }
                | CdStatusEvent::Identified { .. }
        )
    )
}

/// Render the one-row CD status bar at the top of the screen.
///
/// Format per state. Prefix is the literal ASCII `"CD "` (not an emoji)
/// so the bar renders consistently across terminals without depending on
/// emoji-font fallbacks:
/// - `NoMedia` — "CD {drive name} — no disc"
/// - `UnknownDisc` — "CD {drive name} — Unknown disc: {reason}"
/// - `Identified` — "CD {drive name} — {artist} — {album}   \[i\] import"
pub(crate) fn draw_cd_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let style = Style::default().bg(theme.footer_bg).fg(theme.footer_text);

    // Active rip takes precedence over any other CD status.
    if let Some(rip) = &app.cd.rip {
        let progress = format_rip_progress(rip.elapsed_ms, rip.track_length_ms);
        let line = Line::from(vec![
            Span::styled(
                "CD ripping ",
                Style::default()
                    .fg(theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{}/{}: {}   {progress}",
                rip.current, rip.total, rip.track_title
            )),
            Span::styled("   [c] cancel", Style::default().fg(theme.dim_text)),
        ]);
        f.render_widget(Paragraph::new(line).style(style), area);
        return;
    }

    let Some(status) = &app.cd.last_status else {
        return;
    };

    let line = match status {
        // `NoDrive` is filtered out by `show_cd_status_bar`, so this arm
        // shouldn't run — return early rather than render an empty row.
        CdStatusEvent::NoDrive => return,
        CdStatusEvent::NoMedia { drive } => Line::from(vec![
            Span::styled("CD ", Style::default().fg(theme.accent_secondary)),
            Span::raw(format!("{} — no disc", drive.name)),
        ]),
        CdStatusEvent::UnknownDisc { drive, reason, .. } => Line::from(vec![
            Span::styled("CD ", Style::default().fg(theme.accent_secondary)),
            Span::raw(format!("{} — Unknown disc: {reason}", drive.name)),
        ]),
        CdStatusEvent::Identified { drive, primary, .. } => {
            let artist = render_artist_credit(&primary.artist_credit);
            Line::from(vec![
                Span::styled("CD ", Style::default().fg(theme.accent_secondary)),
                Span::raw(format!("{} — {} — {}", drive.name, artist, primary.title)),
                Span::raw("   "),
                Span::styled(
                    "[i] import",
                    Style::default()
                        .fg(theme.header_text)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        }
    };

    let para = Paragraph::new(line).style(style);
    f.render_widget(para, area);
}

/// Render the elapsed / total progress fragment for the rip status bar.
///
/// `M:SS / M:SS — NN%` when both fields are known; `M:SS` alone when MB
/// didn't surface a track length. Floor-divides on the percent so it
/// never displays 100% while ffmpeg is still encoding the tail.
fn format_rip_progress(elapsed_ms: u64, total_ms: Option<u64>) -> String {
    let format_ms = |ms: u64| {
        let total_secs = ms / 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}:{secs:02}")
    };
    match total_ms {
        Some(total) if total > 0 => {
            let percent = ((elapsed_ms.saturating_mul(100) / total).min(99)) as u8;
            format!(
                "[{} / {} — {percent}%]",
                format_ms(elapsed_ms),
                format_ms(total),
            )
        }
        _ => format!("[{}]", format_ms(elapsed_ms)),
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
    // In Playlists mode the sidebar mode label is fixed — it lists
    // playlists, not artist/album buckets.
    let mode_label = if app.browse_mode == BrowseMode::Playlists {
        "Playlists"
    } else {
        match app.sidebar_mode {
            SidebarMode::Artists => "Artists",
            SidebarMode::Albums => "Albums",
        }
    };
    let browse_prefix = match app.browse_mode {
        BrowseMode::Library | BrowseMode::Playlists => "",
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
            BrowseMode::Playlists => "No playlists yet — press N to create one",
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
                    // Playlists never show device-presence indicators —
                    // they're library-side state. (`show_device_status` is
                    // also gated on `BrowseMode::Library` above.)
                    SidebarEntry::Playlist { .. } => None,
                };
                let accent_fg = sidebar_icon_accent(t, i == app.sidebar_selected);
                match presence {
                    Some(DevicePresence::Full) => ("✓ ", style.fg(accent_fg)),
                    Some(DevicePresence::Partial) => ("◐ ", style.fg(accent_fg)),
                    _ => ("  ", style),
                }
            } else if let SidebarEntry::Playlist { id, .. } = entry {
                // Visual distinction: generated playlists get a "~ " prefix,
                // manual playlists get the same two-space pad as device-mode
                // rows so name alignment is uniform across the column.
                let is_generated = app
                    .playlists
                    .get(*id)
                    .map(|p| p.is_generated())
                    .unwrap_or(false);
                let accent_fg = sidebar_icon_accent(t, i == app.sidebar_selected);
                if is_generated {
                    ("~ ", style.fg(accent_fg))
                } else {
                    ("  ", style)
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
        BrowseMode::Playlists => format!(" {} playlists", app.playlists.len()),
    };
    let right = match app.browse_mode {
        BrowseMode::Device => "v:cycle | a:queue rm | D:delete | C:clr | ?:help".to_string(),
        BrowseMode::Playlists => {
            "v:cycle | N:new | G:generate | R:regen | e:rename | d:delete | a:sync".to_string()
        }
        BrowseMode::Library => {
            if app.device.status == DeviceStatus::Connected {
                "✓=synced ◐=partial | v:cycle | a:add | +:playlist | S:sync | ?:help".to_string()
            } else {
                "v:cycle | a:add | +:playlist | S:sync | q:quit | ?:help".to_string()
            }
        }
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
        .style(t.modal())
        .border_style(t.error())
        .title(" Confirm Delete ")
        .title_alignment(Alignment::Center);

    let lines = vec![
        Line::from(""),
        Line::from(format!(" Delete {} track(s)?", count)),
        Line::from(Span::styled(" Enter/y:yes  Esc/n:no", t.modal_dim())),
    ];
    let p = Paragraph::new(lines).block(block).style(t.modal());
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
        .style(t.modal())
        .border_style(t.error())
        .title(" Clear Cache ")
        .title_alignment(Alignment::Center);

    let lines = vec![
        Line::from(""),
        Line::from(" Clear playback cache?"),
        Line::from(Span::styled(" Enter/y:yes  Esc/n:no", t.modal_dim())),
    ];
    let p = Paragraph::new(lines).block(block).style(t.modal());
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
    let width = 56u16.min(area.width - 4);
    let height = 38u16.min(area.height - 4);
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
        "  Playlists view",
        "  N           New manual playlist",
        "  G           Generate (Discover Weekly form)",
        "  R           Regenerate selected generated playlist",
        "  e           Rename selected playlist",
        "  d           Delete playlist (sidebar) / drop track (track list)",
        "  a           Add playlist to sync queue",
        "",
        "  Library track list",
        "  +           Add selected track to a playlist",
        "  G           Generate playlist seeded by this track",
        "",
        "  Library sidebar / album list",
        "  G           Generate playlist seeded by selected artist/album",
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
        .style(t.modal())
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(" Help — press Esc to close ");
    let p = Paragraph::new(lines)
        .block(block)
        .style(t.modal())
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
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(" Theme [t] ")
        .style(t.modal());

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

/// Render the CD import overlay — a centred modal with track-toggle list,
/// alternate-match picker, fidelity picker, and auto-eject toggle. Focus is
/// tracked on `App::import_overlay.focus`; the focused section gets a
/// `▶ ` prefix on its header.
fn draw_import_overlay(f: &mut Frame, app: &App) {
    let Some(overlay) = app.import_overlay.as_ref() else {
        return;
    };
    let t = app.theme();
    let area = f.area();

    let width = (area.width * 80 / 100)
        .clamp(50, 100)
        .min(area.width.saturating_sub(4));
    let height = (area.height * 80 / 100)
        .clamp(14, 36)
        .min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(format!(
            " Import disc — {} (Esc to cancel) ",
            overlay.current_release_label()
        ))
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.height < 9 {
        // Too cramped for the 6-row layout (match, tracks, fidelity, tags,
        // eject, footer) — bail with just the frame so we don't render
        // garbage.
        return;
    }

    // Vertical layout: match-line (1), separator+tracks (Min 4), fidelity (1),
    // tags-info (1), eject (1), footer hint (1). Separators are absorbed
    // into the section titles to keep the constraint list short.
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // match picker
            Constraint::Min(4),    // track list
            Constraint::Length(1), // fidelity
            Constraint::Length(1), // tags info
            Constraint::Length(1), // eject
            Constraint::Length(1), // footer hints
        ])
        .split(inner);

    draw_import_match_row(f, app, overlay, sections[0]);
    draw_import_track_list(f, app, overlay, sections[1]);
    draw_import_fidelity_row(f, app, overlay, sections[2]);
    draw_import_tags_row(f, app, overlay, sections[3]);
    draw_import_eject_row(f, app, overlay, sections[4]);
    draw_import_footer(f, app, overlay, sections[5]);
}

fn import_focus_prefix(focused: bool) -> &'static str {
    if focused {
        "▶ "
    } else {
        "  "
    }
}

fn draw_import_match_row(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::ImportOverlay,
    area: Rect,
) {
    let t = app.theme();
    let total = overlay.release_count();
    let prefix = import_focus_prefix(overlay.focus == crate::app::ImportField::AlternateMatch);
    let nav = if total > 1 {
        format!(
            "  ([) prev  next (])  {}/{}",
            overlay.release_idx + 1,
            total
        )
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(t.accent_secondary)),
        Span::styled(
            "Match: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(overlay.current_release_label()),
        Span::styled(nav, Style::default().fg(t.dim_text)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_import_track_list(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::ImportOverlay,
    area: Rect,
) {
    let t = app.theme();
    let focused = overlay.focus == crate::app::ImportField::Tracks;
    let title_prefix = import_focus_prefix(focused);
    let selected = overlay.selected_count();
    let total = overlay.current_tracks().len();
    let title = format!(
        " {title_prefix}Tracks — {selected}/{total} selected   [Space] toggle  [a] all  [n] none "
    );
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(t.border))
        .title(title)
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let visible = inner.height as usize;
    let cursor = overlay.track_cursor;
    // Window the cursor so it's always in view, biased toward top.
    let scroll = if cursor >= visible {
        cursor + 1 - visible
    } else {
        0
    };

    let rows: Vec<ListItem> = overlay
        .current_tracks()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, track)| {
            // Share the position fallback with the cursor handler so
            // both render and click-target agree on what "the track at
            // row N" is. See `app::import::effective_position`.
            let position = crate::app::effective_position(track, i);
            let selected_box = if overlay
                .track_selection
                .get(&position)
                .copied()
                .unwrap_or(false)
            {
                "[x]"
            } else {
                "[ ]"
            };
            let duration = track
                .length
                .map(|ms| format_duration_ms(ms as u64))
                .unwrap_or_else(|| "    ".into());
            // Title column uses `pad_right_to_width` (display-width-aware)
            // instead of `{:<40}` (Rust char count) so CJK titles —
            // each glyph counts as 2 cells — don't overflow the column.
            let title_col = pad_right_to_width(&truncate(&track.title, 40), 40);
            let row_text = format!("{selected_box} {position:>2}. {title_col}  {duration}");
            let style = if i == cursor && focused {
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.sidebar_text)
            };
            ListItem::new(Line::from(Span::styled(row_text, style)))
        })
        .collect();

    let list = List::new(rows).style(Style::default().bg(t.main_bg));
    f.render_widget(list, inner);
}

fn draw_import_fidelity_row(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::ImportOverlay,
    area: Rect,
) {
    let t = app.theme();
    let focused = overlay.focus == crate::app::ImportField::Fidelity;
    let prefix = import_focus_prefix(focused);
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(t.accent_secondary)),
        Span::styled(
            "Fidelity: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("◀ {} ▶", overlay.current_fidelity().label())),
        Span::styled("  (f / F)", Style::default().fg(t.dim_text)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Render the "Tags written" info line. The summary is fetched from the
/// currently-selected fidelity via `RipFidelity::tag_summary` so it
/// updates live as the user cycles through formats with `f` / `F`.
/// Today every variant returns the same string (the tag set is uniform
/// across containers — `tag_ripped_file` writes the same `ItemKey`
/// items regardless), so this is cosmetic motion that sets the stage
/// for per-format variation if a future container drops some fields.
fn draw_import_tags_row(f: &mut Frame, app: &App, overlay: &crate::app::ImportOverlay, area: Rect) {
    let t = app.theme();
    let line = Line::from(vec![
        Span::raw("  "), // align with focused-row content (no ▶ prefix; line isn't focusable)
        Span::styled(
            "Tags: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            overlay.current_fidelity().tag_summary(),
            Style::default().fg(t.dim_text),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_import_eject_row(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::ImportOverlay,
    area: Rect,
) {
    let t = app.theme();
    let focused = overlay.focus == crate::app::ImportField::AutoEject;
    let prefix = import_focus_prefix(focused);
    let checkbox = if overlay.auto_eject { "[x]" } else { "[ ]" };
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(t.accent_secondary)),
        Span::styled(
            "Eject after import: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(checkbox),
        Span::styled("  (e)", Style::default().fg(t.dim_text)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_import_footer(f: &mut Frame, app: &App, overlay: &crate::app::ImportOverlay, area: Rect) {
    let t = app.theme();
    // When the overlay is "armed" (Enter pressed against an existing-file
    // conflict), replace the standard footer with a conspicuous overwrite
    // prompt. Coloured red so it doesn't blend with the dim hint row.
    let line = if overlay.overwrite_armed {
        Line::from(vec![
            Span::styled(
                format!("⚠ {} track(s) already exist  ", overlay.conflict_count),
                Style::default()
                    .fg(t.error_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[Enter]",
                Style::default()
                    .fg(t.header_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" overwrite  "),
            Span::styled(
                "[Esc]",
                Style::default()
                    .fg(t.header_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "[Tab]",
                Style::default()
                    .fg(t.header_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" focus  "),
            Span::styled(
                "[Enter]",
                Style::default()
                    .fg(t.header_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" import  "),
            Span::styled(
                "[Esc]",
                Style::default()
                    .fg(t.header_text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ])
    };
    f.render_widget(
        Paragraph::new(line).style(Style::default().fg(t.dim_text).bg(t.main_bg)),
        area,
    );
}

/// `mm:ss` formatter for the import overlay's track-length column.
/// Lives here (not in `util.rs`) because it's currently the only caller —
/// extract on the second use.
fn format_duration_ms(ms: u64) -> String {
    let total = ms / 1000;
    format!("{:>2}:{:02}", total / 60, total % 60)
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
        .style(t.modal())
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(" Search ");
    let p = Paragraph::new(format!(" {}_", app.search_query))
        .block(block)
        .style(t.modal());
    f.render_widget(p, rect);
}

fn draw_playlist_name_input(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = 4u16.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 3;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let title = if app.playlist_rename_target.is_some() {
        " Rename Playlist "
    } else {
        " New Playlist "
    };
    let block = t
        .block()
        .style(t.modal())
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(title);
    let buf = app.playlist_name_input.as_deref().unwrap_or("");
    let lines = vec![
        Line::from(format!(" {}_", buf)),
        Line::from(Span::styled(" Enter:save  Esc:cancel", t.modal_dim())),
    ];
    let p = Paragraph::new(lines).block(block).style(t.modal());
    f.render_widget(p, rect);
}

fn draw_add_to_playlist_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let picker = match &app.add_to_playlist_picker {
        Some(p) => p,
        None => return,
    };
    let area = f.area();
    // Size: roughly 60% of width, capped; height grows with options up to 12.
    let width = (area.width * 6 / 10)
        .clamp(40, 80)
        .min(area.width.saturating_sub(4));
    let visible_options = picker.options.len().min(10) as u16;
    let height = (visible_options + 4).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .style(t.modal())
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(" Add to Playlist ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.height < 3 {
        return;
    }

    // Header line: which track is being added.
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" Track: ", t.modal_dim()),
        Span::styled(picker.track_label.clone(), t.modal()),
    ]))
    .style(t.modal());
    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    f.render_widget(header, header_rect);

    // Options list, vertical-scrolled around the selection.
    let list_rect = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );
    let visible = list_rect.height as usize;
    let scroll = compute_scroll(picker.selected, visible, picker.options.len());
    let items: Vec<ListItem> = picker
        .options
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, (_, name))| {
            let style = if i == picker.selected {
                t.sidebar_item_selected()
            } else {
                // Use modal() rather than sidebar_item() so the row's
                // background matches the surrounding modal — sidebar_item
                // omits a bg, which on themes like Newport Lights leaves
                // the unselected rows looking transparent.
                t.modal()
            };
            let cursor = if i == picker.selected { "> " } else { "  " };
            ListItem::new(format!("{}{}", cursor, name)).style(style)
        })
        .collect();
    f.render_widget(List::new(items).style(t.modal()), list_rect);

    // Footer hint.
    let hint_rect = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    let hint = Paragraph::new(Span::styled(
        " ↑/↓:select  Enter:add  Esc:cancel",
        t.modal_dim(),
    ))
    .style(t.modal());
    f.render_widget(hint, hint_rect);
}

fn draw_confirm_playlist_delete(f: &mut Frame, app: &App) {
    let t = app.theme();
    let id = match app.pending_playlist_delete {
        Some(id) => id,
        None => return,
    };
    let name = app
        .playlists
        .get(id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "playlist".to_string());

    let area = f.area();
    let title = format!(" Delete \"{}\"? ", name);
    let label_len = title.len() as u16 + 4;
    let w = label_len.max(36).min(area.width.saturating_sub(4));
    let h = 5u16.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let block = t
        .block()
        .style(t.modal())
        .border_style(t.error().bg(t.sidebar_bg))
        .title(title)
        .title_alignment(Alignment::Center);
    let lines = vec![
        Line::from(""),
        Line::from(" Delete this playlist? Tracks remain in the library."),
        Line::from(Span::styled(" Enter/y:yes  Esc/n:no", t.modal_dim())),
    ];
    let p = Paragraph::new(lines).block(block).style(t.modal());
    f.render_widget(p, rect);
}

fn draw_generation_form_overlay(f: &mut Frame, app: &App) {
    let t = app.theme();
    let form = match &app.generation_form {
        Some(f) => f,
        None => return,
    };
    let area = f.area();
    // Modal sized to ~70% of width × ~70% of height, clamped so it stays
    // legible on small terminals.
    let w = ((area.width * 7) / 10)
        .clamp(50, 80)
        .min(area.width.saturating_sub(4));
    let h = ((area.height * 7) / 10)
        .clamp(16, 22)
        .min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);
    let title = if form.regenerating.is_some() {
        " Regenerate Playlist "
    } else {
        " Generate Playlist "
    };
    let block = t
        .block()
        .style(t.modal())
        .border_style(Style::default().fg(t.selection_bg).bg(t.sidebar_bg))
        .title(title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.height < 6 {
        let p = Paragraph::new("Terminal too small to show form.").style(t.modal());
        f.render_widget(p, inner);
        return;
    }

    // Build the field rows.
    let mut lines: Vec<Line> = Vec::new();

    let row = |label: &str, value: String, focused: bool| -> Line<'static> {
        let label_style = if focused {
            t.sidebar_item_selected().add_modifier(Modifier::BOLD)
        } else {
            // Carry the modal background so the unfocused label is on the
            // same surface as the rest of the modal — bare `t.dim()` omits
            // a background and shows the underlying main panel through.
            t.modal_dim()
        };
        let cursor = if focused { "> " } else { "  " };
        Line::from(vec![
            Span::styled(format!("{cursor}{label:<16}"), label_style),
            Span::styled(value, t.modal()),
        ])
    };

    lines.push(row(
        "Name",
        if form.selected_field == GenerationFormState::FIELD_NAME {
            format!("{}_", form.name)
        } else {
            form.name.clone()
        },
        form.selected_field == GenerationFormState::FIELD_NAME,
    ));

    let strategy_labels = [
        "Top played",
        "Recently played",
        "More like a track",
        "By artist",
        "By genre",
    ];
    let label = strategy_labels
        .get(form.seed_strategy_idx)
        .copied()
        .unwrap_or("?");
    // Append the backing context next to the strategy so the user can see
    // what value will actually drive the recommendation. Behavioural
    // strategies (TopPlayed, RecentlyPlayed) have no context; Track,
    // Artist, and Genre strategies surface "(none)" when nothing was
    // captured at form-open time.
    let strategy = match form.current_context_label() {
        Some(ctx) => format!("( {} )  {}", label, ctx),
        None if form.seed_strategy_idx >= 2 => format!("( {} )  (none)", label),
        None => format!("( {} )", label),
    };
    lines.push(row(
        "Seed strategy",
        strategy,
        form.selected_field == GenerationFormState::FIELD_SEED_STRATEGY,
    ));
    let win_label = if form.window_days == 0 {
        "all-time".to_string()
    } else {
        format!("{} days", form.window_days)
    };
    lines.push(row(
        "Window",
        win_label,
        form.selected_field == GenerationFormState::FIELD_WINDOW_DAYS,
    ));
    lines.push(row(
        "Seed count",
        form.seed_count.to_string(),
        form.selected_field == GenerationFormState::FIELD_SEED_COUNT,
    ));
    lines.push(row(
        "Length",
        format!("{} tracks", form.target_length),
        form.selected_field == GenerationFormState::FIELD_TARGET_LEN,
    ));
    lines.push(row(
        "Max per artist",
        form.max_per_artist.to_string(),
        form.selected_field == GenerationFormState::FIELD_MAX_PER_ARTIST,
    ));
    lines.push(row(
        "Max per album",
        form.max_per_album.to_string(),
        form.selected_field == GenerationFormState::FIELD_MAX_PER_ALBUM,
    ));
    lines.push(row(
        "Diversity",
        slider_str(form.diversity),
        form.selected_field == GenerationFormState::FIELD_DIVERSITY,
    ));
    lines.push(row(
        "Novelty",
        slider_str(form.novelty),
        form.selected_field == GenerationFormState::FIELD_NOVELTY,
    ));
    let chk = if form.exclude_on_device { "[x]" } else { "[ ]" };
    lines.push(row(
        "Exclude on-device",
        chk.to_string(),
        form.selected_field == GenerationFormState::FIELD_EXCLUDE_DEVICE,
    ));

    // Spacer + footer hint.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Tab/Shift-Tab move • </> adjust • Space toggle • Enter generate • Esc cancel",
        t.modal_dim(),
    )));

    let p = Paragraph::new(lines)
        .style(t.modal())
        .wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

/// Render a 16-cell ASCII slider for a 0..1 value.
fn slider_str(v: f32) -> String {
    let cells = 16usize;
    let filled = (v.clamp(0.0, 1.0) * cells as f32).round() as usize;
    let bar: String = (0..cells)
        .map(|i| if i < filled { '=' } else { '-' })
        .collect();
    format!("[{bar}] {v:.2}")
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

fn draw_tag_manager_overlay(f: &mut Frame, app: &App) {
    use crate::app::TagManagerPhase;

    let Some(overlay) = app.tag_manager.as_ref() else {
        return;
    };
    let t = app.theme();
    let area = f.area();

    let width = (area.width * 70 / 100)
        .clamp(40, 100)
        .min(area.width.saturating_sub(4));
    let height = (area.height * 70 / 100)
        .clamp(8, 36)
        .min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);

    f.render_widget(Clear, rect);

    let title = match overlay.phase {
        TagManagerPhase::SearchInput => {
            " Tag manager — edit query (Enter to search, Esc to close) "
        }
        TagManagerPhase::SearchPending => " Tag manager — searching MusicBrainz… ",
        TagManagerPhase::SearchResults => {
            " Tag manager — ↑↓ select · Enter pick · s edit query · Esc back "
        }
        TagManagerPhase::LoadingRelease => " Tag manager — loading release… ",
        TagManagerPhase::DiffPreview => {
            " Tag manager — j/k · Space · a/n · c fold · Enter apply · s search · Esc "
        }
        TagManagerPhase::Applying => " Tag manager — applying… ",
        TagManagerPhase::Done => " Tag manager — done (any key to close) ",
        TagManagerPhase::Error => " Tag manager — error (any key to close) ",
    };
    let block = t
        .block()
        .border_style(Style::default().fg(t.selection_bg))
        .title(title)
        .style(Style::default().bg(t.main_bg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.height < 2 || inner.width < 30 {
        return;
    }

    match overlay.phase {
        TagManagerPhase::SearchInput => draw_tag_manager_search_input(f, app, overlay, inner),
        TagManagerPhase::SearchPending => {
            draw_centered_line(f, inner, "Searching MusicBrainz…", t.dim_text);
        }
        TagManagerPhase::SearchResults => draw_tag_manager_search_results(f, app, overlay, inner),
        TagManagerPhase::LoadingRelease => {
            draw_centered_line(f, inner, "Loading release details…", t.dim_text);
        }
        TagManagerPhase::DiffPreview | TagManagerPhase::Applying => {
            draw_tag_manager_diff(f, app, overlay, inner);
        }
        TagManagerPhase::Done => {
            draw_centered_line(f, inner, "Done. Press any key to close.", t.success_text);
        }
        TagManagerPhase::Error => {
            let msg = overlay
                .error
                .clone()
                .unwrap_or_else(|| "Unknown error".into());
            draw_centered_line(f, inner, &msg, t.error_text);
        }
    }
}

fn draw_centered_line(f: &mut Frame, area: Rect, msg: &str, color: Color) {
    // Wrap long messages so multi-sentence error strings (e.g. the
    // tag-manager AcoustID-no-MB-link error) don't get truncated at the
    // overlay's right edge. `trim: true` collapses leading whitespace on
    // wrapped lines so short messages still center cleanly.
    let para = Paragraph::new(msg)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(color));
    f.render_widget(para, area);
}

fn draw_tag_manager_search_input(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::TagManagerOverlay,
    inner: Rect,
) {
    use crate::app::SearchInputField;
    let t = app.theme();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    let artist_prefix = if overlay.search_input_field == SearchInputField::Artist {
        "▶ "
    } else {
        "  "
    };
    let album_prefix = if overlay.search_input_field == SearchInputField::Album {
        "▶ "
    } else {
        "  "
    };

    let artist_line = Line::from(vec![
        Span::styled(artist_prefix, Style::default().fg(t.accent_secondary)),
        Span::styled(
            "Artist: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(overlay.query_artist.clone()),
    ]);
    let album_line = Line::from(vec![
        Span::styled(album_prefix, Style::default().fg(t.accent_secondary)),
        Span::styled(
            "Album:  ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(overlay.query_album.clone()),
    ]);
    let hint = Line::from(vec![Span::styled(
        "Tab switch field · Enter search · Esc cancel",
        Style::default().fg(t.dim_text),
    )]);

    f.render_widget(Paragraph::new(artist_line), rows[0]);
    f.render_widget(Paragraph::new(album_line), rows[1]);
    f.render_widget(Paragraph::new(hint), rows[2]);
}

fn draw_tag_manager_search_results(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::TagManagerOverlay,
    inner: Rect,
) {
    let t = app.theme();
    if overlay.search_hits.is_empty() {
        draw_centered_line(
            f,
            inner,
            "No MusicBrainz matches. Esc to revise query.",
            t.dim_text,
        );
        return;
    }
    let rows: Vec<Row> = overlay
        .search_hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let artist = {
                let rendered = render_artist_credit(&hit.artist_credit);
                if rendered.is_empty() {
                    "(unknown artist)".to_string()
                } else {
                    rendered
                }
            };
            let year = hit
                .date
                .as_deref()
                .and_then(|d| d.get(..4))
                .unwrap_or("----");
            let country = hit.country.as_deref().unwrap_or("");
            let tracks = hit
                .track_count
                .or_else(|| hit.media.iter().filter_map(|m| m.track_count).max())
                .map(|n| n.to_string())
                .unwrap_or_default();
            let score = hit.score.to_string();
            let mut row = Row::new(vec![
                Cell::from(format!("{}.", i + 1)),
                Cell::from(score),
                Cell::from(hit.title.clone()),
                Cell::from(artist),
                Cell::from(year.to_string()),
                Cell::from(country.to_string()),
                Cell::from(tracks),
            ]);
            if i == overlay.hit_idx {
                row = row.style(Style::default().bg(t.selection_bg).fg(t.selection_text));
            }
            row
        })
        .collect();
    let header = Row::new(["#", "Score", "Title", "Artist", "Year", "Country", "Tracks"])
        .style(t.header())
        .height(1);
    let widths = [
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Min(20),
        Constraint::Min(15),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(7),
    ];
    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, inner);
}

fn draw_tag_manager_diff(
    f: &mut Frame,
    app: &App,
    overlay: &crate::app::TagManagerOverlay,
    inner: Rect,
) {
    let t = app.theme();
    let Some(diff) = overlay.diff.as_ref() else {
        return;
    };

    // Layout: 2-line summary header (release match + counts), then table.
    let total_tracks = diff.tracks.iter().filter(|t| !t.fields.is_empty()).count();
    let total_changes: usize = diff
        .tracks
        .iter()
        .flat_map(|t| t.fields.iter())
        .filter(|f| f.enabled)
        .count();
    let total_renames = diff
        .tracks
        .iter()
        .filter(|t| {
            t.dest_path.is_some()
                && t.fields
                    .iter()
                    .any(|f| f.kind == zytunes::tag_ops::FieldKind::Filename && f.enabled)
        })
        .count();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);
    let header_line = Line::from(vec![
        Span::styled(
            "Match: ",
            Style::default()
                .fg(t.header_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(diff.summary.clone()),
    ]);
    f.render_widget(Paragraph::new(header_line), rows[0]);
    let counts_line = Line::from(vec![
        Span::styled(
            format!(
                "{total_tracks} track{}",
                if total_tracks == 1 { "" } else { "s" }
            ),
            Style::default().fg(t.accent_secondary),
        ),
        Span::raw(" · "),
        Span::styled(
            format!(
                "{total_changes} change{}",
                if total_changes == 1 { "" } else { "s" }
            ),
            Style::default().fg(if total_changes == 0 {
                t.dim_text
            } else {
                t.success_text
            }),
        ),
        Span::raw(" · "),
        Span::styled(
            format!(
                "{total_renames} rename{}",
                if total_renames == 1 { "" } else { "s" }
            ),
            Style::default().fg(if total_renames == 0 {
                t.dim_text
            } else {
                t.success_text
            }),
        ),
    ]);
    f.render_widget(Paragraph::new(counts_line), rows[1]);

    // Flatten the per-track field list into one table, with per-track divider
    // rows so the user sees structure. Rows are sourced from the overlay's
    // `flattened_rows` so the renderer and the navigation/focus model can
    // never drift — if a row isn't in the navigable list (e.g. fields of a
    // collapsed track), it doesn't get rendered.
    struct DisplayRow<'a> {
        kind: DisplayKind<'a>,
        focused: bool,
    }
    enum DisplayKind<'a> {
        TrackHeader {
            track: &'a zytunes::tag_ops::TrackTagDiff,
            collapsed: bool,
        },
        Field(&'a zytunes::tag_ops::FieldDiff),
    }

    let display_rows: Vec<DisplayRow> = overlay
        .flattened_rows
        .iter()
        .enumerate()
        .filter_map(|(row_idx, row)| {
            let focused = row_idx == overlay.focused_row;
            match *row {
                crate::app::FocusRow::Header(ti) => {
                    let track = diff.tracks.get(ti)?;
                    Some(DisplayRow {
                        kind: DisplayKind::TrackHeader {
                            track,
                            collapsed: overlay.collapsed_tracks.contains(&ti),
                        },
                        focused,
                    })
                }
                crate::app::FocusRow::Field(ti, fi) => {
                    let field = diff.tracks.get(ti)?.fields.get(fi)?;
                    Some(DisplayRow {
                        kind: DisplayKind::Field(field),
                        focused,
                    })
                }
            }
        })
        .collect();

    if display_rows.is_empty() {
        draw_centered_line(
            f,
            rows[2],
            "No fields differ between library and release.",
            t.dim_text,
        );
        return;
    }

    let table_rows: Vec<Row> = display_rows
        .iter()
        .map(|dr| match &dr.kind {
            DisplayKind::TrackHeader { track, collapsed } => {
                // Build a rich header: "▼/▶ Track N · <title> · (k changes)".
                // The fold indicator tells the user whether to expect field
                // rows below — `▼` open, `▶` collapsed. The change count
                // counts ENABLED fields only so collapsed tracks still show
                // how much they hide.
                let label = render_track_header(track);
                let arrow = if *collapsed { "▶ " } else { "▼ " };
                let header_style = if dr.focused {
                    Style::default()
                        .bg(t.selection_bg)
                        .fg(t.selection_text)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(t.accent_secondary)
                        .bg(t.main_bg)
                        .add_modifier(Modifier::BOLD)
                };
                Row::new(vec![
                    Cell::from(arrow),
                    Cell::from(label),
                    Cell::from(""),
                    Cell::from(""),
                ])
                .style(header_style)
            }
            DisplayKind::Field(field) => {
                // The diff now carries every field, including unchanged
                // ones (the user wanted to see existing metadata even where
                // MB matches). Three-state marker so unchanged rows don't
                // look like a checkbox the user forgot to tick:
                //   [x]  changed + applying
                //   [ ]  changed + skipping
                //    =   unchanged (no-op, toggle is blocked)
                let changed = field.current != field.proposed;
                let mark = if !changed {
                    " = "
                } else if field.enabled {
                    "[x]"
                } else {
                    "[ ]"
                };
                // Filename diffs come in as full paths; collapse to basenames
                // so the column doesn't blow out and the actual delta (the
                // filename) is what the user sees.
                let (current, proposed) = if field.kind == zytunes::tag_ops::FieldKind::Filename {
                    (
                        basename_of(field.current.as_deref()),
                        basename_of(field.proposed.as_deref()),
                    )
                } else {
                    (
                        field.current.clone().unwrap_or_default(),
                        field.proposed.clone().unwrap_or_default(),
                    )
                };
                let style = if dr.focused {
                    Style::default().bg(t.selection_bg).fg(t.selection_text)
                } else if !changed {
                    Style::default().fg(t.dim_text)
                } else if field.enabled {
                    Style::default().fg(t.sidebar_text)
                } else {
                    Style::default().fg(t.dim_text)
                };
                Row::new(vec![
                    Cell::from(mark),
                    Cell::from(field.name),
                    Cell::from(current),
                    Cell::from(proposed),
                ])
                .style(style)
            }
        })
        .collect();
    let header = Row::new(["", "Field", "Current", "Proposed"])
        .style(t.header())
        .height(1);
    let value_w = (rows[2].width as usize).saturating_sub(4 + 22 + 4);
    let half = (value_w / 2) as u16;
    let widths = [
        Constraint::Length(3),
        Constraint::Length(22),
        Constraint::Length(half),
        Constraint::Min(10),
    ];
    // Render via TableState so ratatui auto-scrolls to keep the focused row
    // visible. `selected()` drives the offset adjustment internally; visual
    // highlight stays on the per-row `style` we already set (no
    // `highlight_style` here on purpose — we don't want to double-mark
    // the focused row).
    let focused_display_idx = display_rows.iter().position(|dr| dr.focused);
    let mut table_state = TableState::default();
    table_state.select(focused_display_idx);
    let table = Table::new(table_rows, widths).header(header);
    f.render_stateful_widget(table, rows[2], &mut table_state);
}

/// Render the per-track header label used in the diff table.
///
/// Pulls the Title and Track # fields out of the diff so a long album diff
/// is scannable without expanding every row. When the title/number don't
/// change, only the *current* value is shown to keep the line short.
fn render_track_header(track: &zytunes::tag_ops::TrackTagDiff) -> String {
    let title_field = track
        .fields
        .iter()
        .find(|f| f.kind == zytunes::tag_ops::FieldKind::Identity && f.name == "Title");
    let trknum_field = track
        .fields
        .iter()
        .find(|f| f.kind == zytunes::tag_ops::FieldKind::Numbering && f.name == "Track #");

    let trknum_str = trknum_field.and_then(|f| {
        let cur = f.current.as_deref();
        let prop = f.proposed.as_deref();
        match (cur, prop) {
            (Some(c), Some(p)) if c != p => Some(format!("Track {c} → {p}")),
            (_, Some(p)) => Some(format!("Track {p}")),
            (Some(c), None) => Some(format!("Track {c}")),
            (None, None) => None,
        }
    });

    let title_str: String = match title_field {
        Some(f) => {
            let cur = f.current.as_deref().unwrap_or("");
            let prop = f.proposed.as_deref().unwrap_or("");
            if cur == prop {
                format!("\"{cur}\"")
            } else {
                format!("\"{cur}\" → \"{prop}\"")
            }
        }
        None => track
            .src_path
            .file_stem()
            .map(|s| format!("\"{}\"", s.to_string_lossy()))
            .unwrap_or_default(),
    };

    let changes = track.fields.iter().filter(|f| f.enabled).count();
    let renaming = track
        .fields
        .iter()
        .any(|f| f.kind == zytunes::tag_ops::FieldKind::Filename && f.enabled);

    let mut suffix = format!("({} change{}", changes, if changes == 1 { "" } else { "s" });
    if renaming {
        suffix.push_str(", rename");
    }
    suffix.push(')');

    match trknum_str {
        Some(n) => format!("{n} · {title_str} · {suffix}"),
        None => format!("{title_str} · {suffix}"),
    }
}

fn basename_of(s: Option<&str>) -> String {
    match s {
        Some(p) => std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zytunes::tag_ops::{FieldDiff, FieldKind, TrackTagDiff};

    fn header_track(fields: Vec<FieldDiff>) -> TrackTagDiff {
        TrackTagDiff {
            src_path: std::path::PathBuf::from("/m/A/B/01.mp3"),
            dest_path: None,
            library_id: 1,
            fields,
        }
    }

    fn field(kind: FieldKind, name: &'static str, cur: &str, prop: &str) -> FieldDiff {
        FieldDiff {
            kind,
            name,
            current: Some(cur.into()),
            proposed: Some(prop.into()),
            enabled: true,
        }
    }

    #[test]
    fn render_track_header_shows_title_change_and_track_number() {
        let track = header_track(vec![
            field(FieldKind::Identity, "Title", "Old Title", "The Chain"),
            field(FieldKind::Numbering, "Track #", "1", "1"),
            field(FieldKind::Identity, "Album", "old album", "Rumours"),
        ]);
        let label = render_track_header(&track);
        assert!(label.contains("Track 1"));
        assert!(label.contains("\"Old Title\" → \"The Chain\""));
        assert!(label.contains("(3 changes"));
    }

    #[test]
    fn render_track_header_omits_arrow_when_title_unchanged() {
        let track = header_track(vec![
            FieldDiff {
                kind: FieldKind::Identity,
                name: "Title",
                current: Some("Dreams".into()),
                proposed: Some("Dreams".into()),
                enabled: false,
            },
            field(FieldKind::Numbering, "Track #", "1", "1"),
        ]);
        let label = render_track_header(&track);
        assert!(label.contains("\"Dreams\""));
        assert!(!label.contains("→"));
    }

    #[test]
    fn render_track_header_singular_change() {
        let track = header_track(vec![field(FieldKind::Identity, "Title", "old", "new")]);
        let label = render_track_header(&track);
        assert!(label.contains("(1 change)"), "got: {label}");
        assert!(!label.contains("changes"));
    }

    #[test]
    fn render_track_header_flags_rename() {
        let track = header_track(vec![
            field(FieldKind::Identity, "Title", "a", "b"),
            FieldDiff {
                kind: FieldKind::Filename,
                name: "Filename",
                current: Some("/m/A/B/01 old.mp3".into()),
                proposed: Some("/m/A/B/01 new.mp3".into()),
                enabled: true,
            },
        ]);
        let label = render_track_header(&track);
        assert!(label.contains("rename"));
    }

    #[test]
    fn render_track_header_skips_rename_when_filename_disabled() {
        let mut filename = FieldDiff {
            kind: FieldKind::Filename,
            name: "Filename",
            current: Some("/m/A/B/01 old.mp3".into()),
            proposed: Some("/m/A/B/01 new.mp3".into()),
            enabled: false,
        };
        let _ = &mut filename;
        let track = header_track(vec![filename]);
        let label = render_track_header(&track);
        assert!(!label.contains("rename"));
    }

    #[test]
    fn basename_of_strips_path() {
        assert_eq!(basename_of(Some("/m/A/B/01.mp3")), "01.mp3");
        assert_eq!(basename_of(Some("just.flac")), "just.flac");
        assert_eq!(basename_of(None), "");
    }

    #[test]
    fn rip_progress_format_with_known_total() {
        // 65s of 4-minute track → "1:05 / 4:00 — 27%"
        let s = format_rip_progress(65_000, Some(240_000));
        assert_eq!(s, "[1:05 / 4:00 — 27%]");
    }

    #[test]
    fn rip_progress_format_without_total_falls_back_to_elapsed_only() {
        // MB didn't supply a length — render just the elapsed time.
        let s = format_rip_progress(125_000, None);
        assert_eq!(s, "[2:05]");
    }

    #[test]
    fn rip_progress_format_caps_percent_at_99_until_complete() {
        // ffmpeg's `out_time_us` can briefly outrun the MB-reported
        // length on the tail end (rounding + encoder priming). Floor-
        // capping at 99% avoids a flashed "100%" before the encoder
        // finalises and the rip actually completes.
        let s = format_rip_progress(241_000, Some(240_000));
        assert!(s.contains("99%"), "got {s}");
        let s = format_rip_progress(240_000, Some(240_000));
        assert!(s.contains("99%"), "got {s}");
    }

    #[test]
    fn rip_progress_format_handles_zero_total() {
        // Edge case: a malformed MB record could surface length=0.
        // Should fall through to the elapsed-only branch.
        let s = format_rip_progress(5_000, Some(0));
        assert_eq!(s, "[0:05]");
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
}
