mod anim;
mod app;
mod audio;
mod background;
mod config;
mod theme;
mod ui;

use std::io;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, BrowseMode, DeviceStatus, Panel, SidebarMode, SyncStatus};
use background::BgCommand;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Suppress panics from background threads (rodio/symphonia can panic on
    // unsupported files). Only silence non-main threads to preserve useful panics.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_main = std::thread::current().name() == Some("main");
        if is_main {
            default_hook(info);
        }
        // Silently ignore panics from unnamed threads (rodio, symphonia)
    }));

    let args: Vec<String> = std::env::args().collect();

    // Parse --library flag.
    let default = zytunes::library_xml_path();
    let library_path = args
        .iter()
        .position(|a| a == "--library")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or(&default);

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state.
    let mut app = App::new();
    app.loading_library = true;
    app.library_path = Some(library_path.to_string());

    // Load config and apply theme.
    let cfg = config::load();
    if let Some(ref theme_name) = cfg.theme {
        app.theme_index = theme::find_theme_index(theme_name);
    }

    // Set up background worker.
    let (event_tx, event_rx) = mpsc::channel();
    let cmd_tx = background::spawn(event_tx);

    // Set up audio thread.
    let (audio_event_tx, audio_event_rx) = mpsc::channel();
    let audio_cmd_tx = audio::spawn(audio_event_tx);

    // Kick off async library load.
    let _ = cmd_tx.send(BgCommand::LoadLibrary(library_path.to_string()));

    // Main event loop.
    let result = run_loop(
        &mut terminal,
        &mut app,
        &cmd_tx,
        &event_rx,
        &audio_cmd_tx,
        &audio_event_rx,
    );

    // Restore terminal.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    Ok(())
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    cmd_tx: &mpsc::Sender<BgCommand>,
    event_rx: &mpsc::Receiver<background::BgEvent>,
    audio_tx: &mpsc::Sender<audio::AudioCommand>,
    audio_rx: &mpsc::Receiver<audio::AudioEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        // Pre-render album art for the area below the track list in album detail view.
        if app.album_art.is_some() && app.has_album_browser() {
            let size = terminal.size()?;
            let keys_w: u16 = if app.show_keys { 24 } else { 0 };
            let middle_w = size.width.saturating_sub(36 + keys_w);
            // Right column = middle - sidebar(24) - albums(30) - block border(2).
            let right_w = middle_w.saturating_sub(24 + 30 + 2);
            // Available height below tracks: total browser height minus
            // a minimum of 4 rows for the track list, borders, footer, player.
            let has_player = app.now_playing.is_some();
            let overhead = 2 + 3 + if has_player { 9 } else { 0 }; // borders + footer + player
            let browser_h = size.height.saturating_sub(overhead as u16);
            let track_min = 4u16.min(app.track_list.len() as u16);
            let art_h = browser_h.saturating_sub(track_min).min(26);
            if right_w >= 6 && art_h >= 3 {
                app.render_album_art(right_w, art_h);
            }
        }

        terminal.draw(|f| ui::draw(f, app))?;

        // Process background events.
        while let Ok(ev) = event_rx.try_recv() {
            app.handle_bg_event(ev);
        }
        app.flush_device_index();

        // Process audio events.
        while let Ok(ev) = audio_rx.try_recv() {
            app.handle_audio_event(ev, audio_tx);
        }

        // Query playback position periodically.
        if app.now_playing.is_some() && app.anim_frame.is_multiple_of(4) {
            let _ = audio_tx.send(audio::AudioCommand::QueryPosition);
        }

        // Poll for keyboard events with 50ms timeout.
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                // Handle search mode input first.
                if app.search_active {
                    match key.code {
                        KeyCode::Esc => {
                            app.search_active = false;
                            app.search_query.clear();
                            app.refresh_sidebar();
                        }
                        KeyCode::Enter => {
                            app.search_active = false;
                            app.active_panel = Panel::Library;
                        }
                        KeyCode::Backspace => {
                            app.search_query.pop();
                            app.refresh_sidebar();
                        }
                        KeyCode::Char(c) => {
                            app.search_query.push(c);
                            app.refresh_sidebar();
                        }
                        _ => {}
                    }
                    continue;
                }

                // Handle theme picker.
                if app.show_theme_picker {
                    match key.code {
                        KeyCode::Esc => app.theme_picker_cancel(),
                        KeyCode::Enter => app.theme_picker_confirm(),
                        KeyCode::Up => app.theme_picker_move(-1),
                        KeyCode::Down => app.theme_picker_move(1),
                        _ => {}
                    }
                    continue;
                }

                // Handle cache clear confirmation dialog.
                if app.pending_cache_clear {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.pending_cache_clear = false;
                            let playback_dir = std::env::temp_dir().join("zytunes-playback");
                            let mut total: u64 = 0;
                            if playback_dir.exists() {
                                if let Ok(entries) = std::fs::read_dir(&playback_dir) {
                                    for entry in entries.flatten() {
                                        total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                                    }
                                }
                                let _ = std::fs::remove_dir_all(&playback_dir);
                            }
                            if total > 0 {
                                let mb = total as f64 / (1024.0 * 1024.0);
                                app.set_toast(
                                    format!("Cleared {:.1} MB of cached audio", mb),
                                    false,
                                );
                            } else {
                                app.set_toast("Cache is already empty".into(), false);
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.pending_cache_clear = false;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Handle removal confirmation dialog.
                if app.pending_removal.is_some() {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                            confirm_device_removal(app, cmd_tx);
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.pending_removal = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Handle help overlay.
                if app.show_help {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                            app.show_help = false;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Global keys.
                match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('?') => {
                        app.show_help = true;
                    }
                    KeyCode::Char('h') => {
                        app.show_keys = !app.show_keys;
                    }
                    KeyCode::Tab => {
                        app.cycle_panel();
                    }
                    KeyCode::BackTab => {
                        app.cycle_panel_back();
                    }
                    KeyCode::Char('1') => {
                        app.save_sidebar_pos();
                        app.sidebar_mode = SidebarMode::Artists;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('2') => {
                        app.save_sidebar_pos();
                        app.sidebar_mode = SidebarMode::Albums;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('3') => {
                        app.save_sidebar_pos();
                        app.sidebar_mode = SidebarMode::Playlists;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('4') => {
                        if !app.sync.queue.is_empty() {
                            app.active_panel = Panel::SyncQueue;
                        }
                    }
                    KeyCode::Char('t') => {
                        app.open_theme_picker();
                    }
                    KeyCode::Char('v') => {
                        if app.browse_mode == BrowseMode::Device
                            || app.device.status == DeviceStatus::Connected
                        {
                            app.toggle_browse_mode();
                        } else {
                            app.set_toast("Connect a device first".into(), true);
                        }
                    }
                    KeyCode::Char('c') => {
                        if app.device.status == DeviceStatus::Disconnected {
                            app.device.status = DeviceStatus::Detecting;
                            app.connection_anim_start = Some(app.anim_frame);
                            let _ = cmd_tx.send(BgCommand::Connect);
                        }
                    }
                    KeyCode::Char('d') => match app.active_panel {
                        Panel::SyncQueue => {
                            app.remove_queue_item();
                        }
                        Panel::Device => {
                            let _ = cmd_tx.send(BgCommand::Disconnect);
                            app.device.status = DeviceStatus::Disconnected;
                            app.device.name = None;
                            app.device.tracks.clear();
                            app.clear_device_index();
                            app.set_toast("Disconnected".into(), false);
                        }
                        _ => {}
                    },
                    KeyCode::Char('r') => {
                        if app.device.status == DeviceStatus::Connected {
                            let _ = cmd_tx.send(BgCommand::LoadDeviceTracks);
                            app.set_toast("Refreshing device tracks...".into(), false);
                        }
                    }
                    KeyCode::Up => {
                        app.move_up();
                    }
                    KeyCode::Down => {
                        app.move_down();
                    }
                    KeyCode::PageUp => {
                        app.sync.log_scroll_up(10);
                    }
                    KeyCode::PageDown => {
                        app.sync.log_scroll_down(10);
                    }
                    KeyCode::Right => {
                        app.skip_forward();
                    }
                    KeyCode::Left => {
                        app.skip_back();
                    }
                    KeyCode::Char(' ') => {
                        app.toggle_playback(audio_tx);
                    }
                    KeyCode::Char('<') | KeyCode::Char(',') => {
                        if app.now_playing.is_some() {
                            let _ = audio_tx.send(audio::AudioCommand::Scrub { delta_ms: -5000 });
                        }
                    }
                    KeyCode::Char('>') | KeyCode::Char('.') => {
                        if app.now_playing.is_some() {
                            let _ = audio_tx.send(audio::AudioCommand::Scrub { delta_ms: 5000 });
                        }
                    }
                    KeyCode::Char('n') => {
                        app.next_track(audio_tx);
                    }
                    KeyCode::Char('p') => {
                        app.prev_track(audio_tx);
                    }
                    KeyCode::Enter => match app.active_panel {
                        Panel::Library => {
                            app.select_sidebar_item();
                            if app.has_album_browser() {
                                app.active_panel = Panel::Albums;
                            } else {
                                app.active_panel = Panel::TrackList;
                            }
                        }
                        Panel::Albums => {
                            app.active_panel = Panel::TrackList;
                        }
                        Panel::TrackList => {
                            app.play_selected_track(audio_tx);
                        }
                        Panel::SyncQueue => {
                            app.execute_sync(cmd_tx);
                        }
                        _ => {}
                    },
                    KeyCode::Char('/') => {
                        app.search_active = true;
                        app.search_query.clear();
                    }
                    KeyCode::Char('s') => {
                        if app.active_panel == Panel::TrackList {
                            app.cycle_sort();
                        }
                    }
                    KeyCode::Char('L') => {
                        let path = std::path::PathBuf::from("/tmp/zytunes-log.txt");
                        let content = app.sync.log.join("\n");
                        match std::fs::write(&path, &content) {
                            Ok(_) => {
                                app.set_toast(format!("Log dumped to {}", path.display()), false)
                            }
                            Err(e) => app.set_toast(format!("Log dump failed: {}", e), true),
                        }
                    }
                    KeyCode::Char('S') => {
                        if !app.sync.queue.is_empty() {
                            app.active_panel = Panel::SyncQueue;
                            app.execute_sync(cmd_tx);
                        }
                    }
                    KeyCode::Char('a') => {
                        if app.browse_mode == BrowseMode::Device {
                            app.queue_device_removal();
                        } else {
                            match app.active_panel {
                                Panel::TrackList => {
                                    app.add_selected_track_to_queue();
                                }
                                Panel::Library => {
                                    app.add_sidebar_item_to_queue();
                                }
                                Panel::Albums => {
                                    app.add_all_visible_to_queue();
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Char('A') => {
                        if app.browse_mode == BrowseMode::Device {
                            app.queue_device_removal();
                        } else if app.active_panel == Panel::TrackList {
                            app.add_all_visible_to_queue();
                        }
                    }
                    KeyCode::Char('D') => {
                        if app.browse_mode == BrowseMode::Device && !app.removal_queue.is_empty() {
                            app.pending_removal = Some(app.removal_queue.clone());
                        }
                    }
                    KeyCode::Char('C') => {
                        if app.browse_mode == BrowseMode::Device {
                            let count = app.removal_queue.len();
                            app.clear_removal_queue();
                            if count > 0 {
                                app.set_toast(
                                    format!("Cleared {} queued removal(s)", count),
                                    false,
                                );
                            }
                        } else if app.active_panel == Panel::SyncQueue {
                            app.clear_queue();
                        }
                    }
                    KeyCode::Char('X') => {
                        app.pending_cache_clear = true;
                    }
                    KeyCode::Esc => {
                        if matches!(app.sync.status, SyncStatus::Running { .. }) {
                            let _ = cmd_tx.send(BgCommand::CancelSync);
                        }
                        // Dismiss toast.
                        app.toast_message = None;
                    }
                    _ => {}
                }
            }
        }

        app.tick();

        if app.should_quit {
            app.stop_playback(audio_tx);
            break;
        }
    }

    Ok(())
}

fn confirm_device_removal(app: &mut App, cmd_tx: &mpsc::Sender<BgCommand>) {
    if let Some(items) = app.pending_removal.take() {
        let count = items.len();
        app.set_toast(format!("Removing {} track(s) from device...", count), false);
        let _ = cmd_tx.send(BgCommand::RemoveFromDevice(items));
        app.removal_queue.clear();
    }
}
