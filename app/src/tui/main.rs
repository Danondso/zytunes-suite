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

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, KeyOutcome};
use background::BgCommand;
use zytunes::resolve_mp3_quality;

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

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Load config first so user-defined themes are registered before the App
    // samples the default theme.
    let cfg = config::load();
    theme::init_themes(&cfg.themes);

    let mut quality_warning = None;
    let mp3_quality = resolve_mp3_quality(
        None,
        std::env::var("ZYTUNES_TRANSCODE_QUALITY").ok().as_deref(),
        cfg.transcode_quality.as_deref(),
        |m| quality_warning = Some(m.to_string()),
    )
    .unwrap_or_default();

    // Set up background worker first so we can clone event_tx for the
    // TUI logger before the background thread takes ownership.
    let (event_tx, event_rx) = mpsc::channel();
    let tui_log_tx = event_tx.clone();
    let tui_logger: zytunes::cache::Logger = std::sync::Arc::new(move |msg: &str| {
        let _ = tui_log_tx.send(background::BgEvent::SyncMessage(msg.to_string()));
    });
    let cmd_tx = background::spawn(event_tx, mp3_quality);

    // Create app state.
    let mut app = App::new();
    if let Some(msg) = zytunes::paths::mtpz_file_missing_message() {
        app.sync.log.push(msg);
    }
    app.load_local_plays_from_disk(&tui_logger);
    app.load_playlists_from_disk(&tui_logger);
    app.load_listen_log_from_disk(&tui_logger);
    if let Some(w) = quality_warning {
        app.sync.log.push(w);
    }
    app.loading_library = true;

    if let Some(ref theme_name) = cfg.theme {
        app.theme = theme::theme_by_name(theme_name);
    }

    // Set up audio thread.
    let (audio_event_tx, audio_event_rx) = mpsc::channel();
    let audio_cmd_tx = audio::spawn(audio_event_tx, std::sync::Arc::clone(&app.waveform));

    // Kick off async library load. `fingerprinting` defaults to true when
    // unset; setting `fingerprinting = false` in config.toml skips the
    // expensive symphonia + chromaprint pass.
    let _ = cmd_tx.send(BgCommand::LoadLibrary {
        music_dir: cfg.music_dir.clone(),
        fingerprint: cfg.fingerprinting.unwrap_or(true),
    });

    // Main event loop.
    let result = run_loop(
        &mut terminal,
        &mut app,
        &cmd_tx,
        &event_rx,
        &audio_cmd_tx,
        &audio_event_rx,
    );

    // Take down any running stem engine/installer before this process
    // exits — detached job threads die with us, but their demucs/uv
    // children would survive as orphans (on macOS there's no PDEATHSIG),
    // and an orphaned demucs holding the HuggingFace download lock wedges
    // every future separation until someone finds and kills it.
    zytunes::stems::kill_active_stem_children();

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
        // Refresh the height snapshot key dispatch uses for visibility
        // questions (e.g. whether the stem strip may claim the digit keys).
        app.last_term_height = terminal.size()?.height;

        // Pre-render album art for the area below the track list in album detail view.
        if app.album_art.is_some() && app.has_album_browser() {
            let size = terminal.size()?;
            let w = size.width;
            let (device_w, sidebar_w, album_w, keys_w) =
                ui::LayoutMetrics::panel_widths(w, app.show_keys, app.has_album_browser());
            let middle_w = w.saturating_sub(device_w + keys_w);
            // Right column = middle - sidebar - albums - block border(2).
            let right_w = middle_w.saturating_sub(sidebar_w + album_w + 2);
            // Available height below tracks: total browser height minus
            // a minimum of 4 rows for the track list, borders, footer, player.
            let show_player = app.should_show_player(size.height);
            // borders + footer + player; the player height comes from the
            // same accessor draw() uses (11 while stems are engaged) so
            // the art is sized for the panel that will actually be drawn.
            let overhead: u16 = 2
                + 3
                + if show_player {
                    app.player_panel_height()
                } else {
                    0
                };
            let browser_h = size.height.saturating_sub(overhead);
            let track_min = 4u16.min(app.track_list.len() as u16);
            // Art panel total rows (including its own top/bottom border and padding).
            let art_panel_h = browser_h.saturating_sub(track_min).min(26);
            // Art cache fills the panel's inner area.
            // Width: -2 borders, -4 padding (2 left + 2 right).
            // Height: -2 borders, -1 padding (1 top, 0 bottom).
            let art_inner_w = right_w.saturating_sub(6);
            let art_inner_h = art_panel_h.saturating_sub(3);
            if art_inner_w >= 6 && art_inner_h >= 3 {
                app.render_album_art(art_inner_w, art_inner_h);
            }
        }

        terminal.draw(|f| ui::draw(f, app))?;

        // Process background events.
        while let Ok(ev) = event_rx.try_recv() {
            app.handle_bg_event(ev);
        }
        // Poll for CDs: throttled to ~5s by `maybe_request_cd_detect`.
        // Called every tick so a disc insert is noticed within one poll
        // interval without the worker needing its own timer thread.
        app.maybe_request_cd_detect();
        // Flush any pending background commands queued during event handling.
        for cmd in app.pending_bg_commands.drain(..) {
            let _ = cmd_tx.send(cmd);
        }
        // Same for audio commands (stem-mode swaps queue Play/Scrub pairs
        // from event handlers, which have no audio_tx). Order preserved.
        for cmd in app.pending_audio_commands.drain(..) {
            let _ = audio_tx.send(cmd);
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
                if app.handle_key(key, cmd_tx, audio_tx) == KeyOutcome::Quit {
                    app.tick();
                    app.stop_playback(audio_tx);
                    break;
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
