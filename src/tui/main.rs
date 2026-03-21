mod app;
mod background;
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

use app::{App, DeviceStatus, Panel, SidebarMode, SyncStatus};
use background::BgCommand;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Parse --library flag.
    let library_path = args
        .iter()
        .position(|a| a == "--library")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or(zytunes::DEFAULT_LIBRARY_XML);

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state.
    let mut app = App::new();

    // Try to load library (non-fatal if it fails).
    match app.load_library(library_path) {
        Ok(()) => {}
        Err(e) => {
            app.set_toast(format!("Library: {}", e), true);
        }
    }

    // Set up background worker.
    let (event_tx, event_rx) = mpsc::channel();
    let cmd_tx = background::spawn(event_tx);

    // Main event loop.
    let result = run_loop(&mut terminal, &mut app, &cmd_tx, &event_rx);

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
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        // Process background events.
        while let Ok(ev) = event_rx.try_recv() {
            app.handle_bg_event(ev);
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
                            // Keep filtered results.
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
                        app.sidebar_mode = SidebarMode::Artists;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('2') => {
                        app.sidebar_mode = SidebarMode::Albums;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('3') => {
                        app.sidebar_mode = SidebarMode::Playlists;
                        app.refresh_sidebar();
                        app.active_panel = Panel::Library;
                    }
                    KeyCode::Char('4') => {
                        if !app.sync_queue.is_empty() {
                            app.active_panel = Panel::SyncQueue;
                        }
                    }
                    KeyCode::Char('c') => {
                        if app.device_status == DeviceStatus::Disconnected {
                            app.device_status = DeviceStatus::Detecting;
                            let _ = cmd_tx.send(BgCommand::Connect);
                        }
                    }
                    KeyCode::Char('d') => match app.active_panel {
                        Panel::SyncQueue => {
                            app.remove_queue_item();
                        }
                        Panel::Device => {
                            let _ = cmd_tx.send(BgCommand::Disconnect);
                            app.device_status = DeviceStatus::Disconnected;
                            app.device_name = None;
                            app.device_tracks.clear();
                            app.set_toast("Disconnected".into(), false);
                        }
                        _ => {}
                    },
                    KeyCode::Char('r') => {
                        if app.device_status == DeviceStatus::Connected {
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
                    KeyCode::Right => {
                        app.skip_forward();
                    }
                    KeyCode::Left => {
                        app.skip_back();
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
                    KeyCode::Char('S') => {
                        if !app.sync_queue.is_empty() {
                            app.active_panel = Panel::SyncQueue;
                            app.execute_sync(cmd_tx);
                        }
                    }
                    KeyCode::Char('a') => match app.active_panel {
                        Panel::TrackList => {
                            app.add_selected_track_to_queue();
                        }
                        Panel::Library => {
                            app.add_sidebar_item_to_queue();
                        }
                        Panel::Albums => {
                            // Add all tracks from selected album to queue.
                            app.add_all_visible_to_queue();
                        }
                        _ => {}
                    },
                    KeyCode::Char('A') => {
                        if app.active_panel == Panel::TrackList {
                            app.add_all_visible_to_queue();
                        }
                    }
                    KeyCode::Char('C') => {
                        if app.active_panel == Panel::SyncQueue {
                            app.clear_queue();
                        }
                    }
                    KeyCode::Esc => {
                        if matches!(app.sync_status, SyncStatus::Running { .. }) {
                            let _ = cmd_tx.send(BgCommand::CancelSync);
                        }
                        // Dismiss toast.
                        app.toast_message = None;
                        // Reset sync complete status.
                        if matches!(app.sync_status, SyncStatus::Complete { .. }) {
                            app.sync_status = SyncStatus::Idle;
                        }
                    }
                    _ => {}
                }
            }
        }

        app.tick();

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
