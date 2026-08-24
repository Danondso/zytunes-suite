//! In-process TUI test harness. Drives the real `App` plus `ui::draw` against
//! a `TestBackend` and synthetic `BgEvent` / `KeyEvent` streams so tests do
//! not need a PTY or the real background worker.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::{App, KeyOutcome};
use crate::audio::AudioCommand;
use crate::background::{BgCommand, BgEvent};
use crate::ui;

use zytunes::library::{MusicLibrary, Track};

/// Drives an `App` with a `TestBackend` for in-process e2e tests.
pub struct Harness {
    pub app: App,
    pub term: Terminal<TestBackend>,
    bg_tx: mpsc::Sender<BgEvent>,
    bg_rx: mpsc::Receiver<BgEvent>,
    cmd_tx: mpsc::Sender<BgCommand>,
    pub cmd_rx: mpsc::Receiver<BgCommand>,
    audio_tx: mpsc::Sender<AudioCommand>,
    pub audio_rx: mpsc::Receiver<AudioCommand>,
}

impl Harness {
    /// Build a harness sized to (`width`, `height`). The background-worker
    /// channels are wired to in-test endpoints so nothing escapes the test.
    pub fn new(width: u16, height: u16) -> Self {
        let backend = TestBackend::new(width, height);
        let term = Terminal::new(backend).expect("test backend always builds");
        let (bg_tx, bg_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (audio_tx, audio_rx) = mpsc::channel();

        let mut app = App::new();
        // Disable the local-plays sidecar so tests never touch the user's
        // `~/.cache/zytunes/local-plays.json` file.
        app.local_plays_save_path = None;

        Harness {
            app,
            term,
            bg_tx,
            bg_rx,
            cmd_tx,
            cmd_rx,
            audio_tx,
            audio_rx,
        }
    }

    /// Pump everything pending and render. Drains queued bg events, flushes
    /// queued bg commands, flushes the device index, and renders against the
    /// `TestBackend`. Intentionally does NOT call `App::tick()` so animation
    /// frames don't drift between runs and snapshot output stays
    /// deterministic — tests that want the spinner to advance can call
    /// `tick()` explicitly.
    pub fn step(&mut self) {
        // Mirror the run loop's height snapshot so visibility-gated key
        // handling (stem strip) sees the harness's real terminal size.
        if let Ok(size) = self.term.size() {
            self.app.last_term_height = size.height;
        }
        while let Ok(ev) = self.bg_rx.try_recv() {
            self.app.handle_bg_event(ev);
        }
        for cmd in self.app.pending_bg_commands.drain(..) {
            let _ = self.cmd_tx.send(cmd);
        }
        for cmd in self.app.pending_audio_commands.drain(..) {
            let _ = self.audio_tx.send(cmd);
        }
        self.app.flush_device_index();

        self.term
            .draw(|f| ui::draw(f, &self.app))
            .expect("draw against TestBackend");
    }

    /// Advance the App animation clock by one frame. Most tests don't need
    /// this; expose it for the rare cases that exercise animation logic.
    pub fn tick(&mut self) {
        self.app.tick();
    }

    /// Inject a synthetic background event. The next `step()` will drain it.
    pub fn push_event(&self, ev: BgEvent) {
        let _ = self.bg_tx.send(ev);
    }

    /// Send a key with no modifiers, then run one `step()`.
    pub fn key(&mut self, code: KeyCode) -> KeyOutcome {
        self.key_mods(code, KeyModifiers::NONE)
    }

    /// Send a key with explicit modifiers, then run one `step()` so any
    /// state change is visible in the next render.
    pub fn key_mods(&mut self, code: KeyCode, mods: KeyModifiers) -> KeyOutcome {
        let event = KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let outcome = self.app.handle_key(event, &self.cmd_tx, &self.audio_tx);
        self.step();
        outcome
    }

    /// Render the current `TestBackend` buffer as text, suitable for passing
    /// into `insta::assert_snapshot!`.
    pub fn render(&self) -> String {
        format!("{}", self.term.backend())
    }

    /// Drain commands the app has emitted to its bg-command channel.
    pub fn drain_commands(&self) -> Vec<BgCommand> {
        let mut out = Vec::new();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            out.push(cmd);
        }
        out
    }
}

/// Tiny in-memory `MusicLibrary` for harness tests. Holds a single fixed
/// roster so snapshots stay stable regardless of host filesystem state.
pub struct MockLibrary {
    tracks: Vec<Track>,
    artists: Vec<String>,
    albums: Vec<(String, String)>,
}

impl MockLibrary {
    /// Two artists, two albums, three tracks — enough to exercise sidebar +
    /// album browser + track list.
    pub fn small() -> Self {
        let tracks = vec![
            Track {
                id: 1,
                name: "Idioteque".into(),
                artist: "Radiohead".into(),
                album: "Kid A".into(),
                ..Default::default()
            },
            Track {
                id: 2,
                name: "Everything In Its Right Place".into(),
                artist: "Radiohead".into(),
                album: "Kid A".into(),
                ..Default::default()
            },
            Track {
                id: 3,
                name: "Lebanese Blonde".into(),
                artist: "Thievery Corporation".into(),
                album: "Mirror Conspiracy".into(),
                ..Default::default()
            },
        ];
        let mut artists: Vec<String> = tracks.iter().map(|t| t.artist.clone()).collect::<Vec<_>>();
        artists.sort();
        artists.dedup();
        let mut albums: Vec<(String, String)> = tracks
            .iter()
            .map(|t| (t.artist.clone(), t.album.clone()))
            .collect();
        albums.sort();
        albums.dedup();
        MockLibrary {
            tracks,
            artists,
            albums,
        }
    }
}

impl MusicLibrary for MockLibrary {
    fn artists(&self) -> Vec<&str> {
        self.artists.iter().map(|s| s.as_str()).collect()
    }

    fn albums(&self) -> Vec<(&str, &str)> {
        self.albums
            .iter()
            .map(|(a, al)| (a.as_str(), al.as_str()))
            .collect()
    }

    fn artist_tracks<'a>(&'a self, artist: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let q = artist.to_lowercase();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.artist.to_lowercase() == q),
        )
    }

    fn album_tracks<'a>(&'a self, album: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let q = album.to_lowercase();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.album.to_lowercase() == q),
        )
    }

    fn album_tracks_by_artist<'a>(
        &'a self,
        artist: &str,
        album: &str,
    ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let qa = artist.to_lowercase();
        let qb = album.to_lowercase();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.artist.to_lowercase() == qa && t.album.to_lowercase() == qb),
        )
    }

    fn tracks_by_name<'a>(&'a self, name: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let q = name.to_lowercase();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.name.to_lowercase() == q),
        )
    }

    fn track_count(&self) -> usize {
        self.tracks.len()
    }

    fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
        Box::new(self.tracks.iter())
    }

    fn music_folder(&self) -> Option<&str> {
        None
    }
}
