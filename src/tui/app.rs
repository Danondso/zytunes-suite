use std::sync::mpsc;
use std::time::Instant;

use throbber_widgets_tui::ThrobberState;
use zytunes::library::{ItunesLibrary, Track};
use zytunes::mtp::parse::DeviceEntry;

use crate::background::{BgCommand, BgEvent, StorageInfo, SyncItem};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Library,
    Albums,
    TrackList,
    Device,
    SyncQueue,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidebarMode {
    Artists,
    Albums,
    Playlists,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceStatus {
    Disconnected,
    Detecting,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncStatus {
    Idle,
    Running {
        current: usize,
        total: usize,
    },
    Complete {
        success: usize,
        failed: usize,
        skipped: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortColumn {
    Number,
    Name,
    Artist,
    Album,
    Duration,
    Format,
}

#[derive(Clone)]
pub struct QueuedItem {
    pub label: String,
    pub tracks: Vec<SyncItem>,
}

pub struct App {
    pub active_panel: Panel,
    pub library: Option<ItunesLibrary>,
    pub sidebar_mode: SidebarMode,
    pub sidebar_items: Vec<String>,
    pub sidebar_selected: usize,
    pub sidebar_scroll: usize,
    pub album_list: Vec<AlbumInfo>,
    pub album_selected: usize,
    pub track_list: Vec<TrackInfo>,
    pub track_selected: usize,
    pub track_scroll: usize,
    pub sort_column: SortColumn,
    pub sort_ascending: bool,
    pub device_status: DeviceStatus,
    pub device_name: Option<String>,
    pub device_firmware: Option<String>,
    pub device_serial: Option<String>,
    pub device_manufacturer: Option<String>,
    pub device_model: Option<String>,
    pub device_usb_mode: Option<String>,
    pub device_storage: Option<StorageInfo>,
    pub device_tracks: Vec<DeviceEntry>,
    pub device_loading_tracks: bool,
    pub device_selected: usize,
    pub sync_queue: Vec<QueuedItem>,
    pub queue_selected: usize,
    pub sync_status: SyncStatus,
    pub sync_current_track: String,
    pub sync_log: Vec<String>,
    pub throbber_state: ThrobberState,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_keys: bool,
    pub search_active: bool,
    pub search_query: String,
    pub toast_message: Option<(String, Instant, bool)>, // (msg, time, is_error)
    pub library_path: Option<String>,
    pub loading_library: bool,
}

#[derive(Clone)]
pub struct AlbumInfo {
    pub name: String,
    pub artist: String,
    pub year: Option<u32>,
    pub track_count: usize,
}

#[derive(Clone)]
pub struct TrackInfo {
    pub name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub kind: Option<String>,
    pub location: Option<String>,
    pub track_number: Option<u32>,
}

impl App {
    pub fn new() -> Self {
        App {
            active_panel: Panel::Library,
            library: None,
            sidebar_mode: SidebarMode::Artists,
            sidebar_items: Vec::new(),
            sidebar_selected: 0,
            sidebar_scroll: 0,
            album_list: Vec::new(),
            album_selected: 0,
            track_list: Vec::new(),
            track_selected: 0,
            track_scroll: 0,
            sort_column: SortColumn::Name,
            sort_ascending: true,
            device_status: DeviceStatus::Disconnected,
            device_name: None,
            device_firmware: None,
            device_serial: None,
            device_manufacturer: None,
            device_model: None,
            device_usb_mode: None,
            device_storage: None,
            device_tracks: Vec::new(),
            device_loading_tracks: false,
            device_selected: 0,
            sync_queue: Vec::new(),
            queue_selected: 0,
            sync_status: SyncStatus::Idle,
            sync_current_track: String::new(),
            sync_log: Vec::new(),
            throbber_state: ThrobberState::default(),
            should_quit: false,
            show_help: false,
            show_keys: true,
            search_active: false,
            search_query: String::new(),
            toast_message: None,
            library_path: None,
            loading_library: false,
        }
    }


    pub fn refresh_sidebar(&mut self) {
        let lib = match &self.library {
            Some(l) => l,
            None => {
                self.sidebar_items.clear();
                return;
            }
        };

        self.sidebar_items = match self.sidebar_mode {
            SidebarMode::Artists => lib.artists().into_iter().map(|s| s.to_string()).collect(),
            SidebarMode::Albums => lib
                .albums()
                .into_iter()
                .map(|(artist, album)| format!("{} — {}", artist, album))
                .collect(),
            SidebarMode::Playlists => lib
                .user_playlists()
                .into_iter()
                .map(|p| format!("{} ({} tracks)", p.name, p.track_ids.len()))
                .collect(),
        };

        if self.search_active && !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            self.sidebar_items
                .retain(|item| item.to_lowercase().contains(&q));
        }

        self.sidebar_selected = 0;
        self.sidebar_scroll = 0;
    }

    pub fn select_sidebar_item(&mut self) {
        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        let item = match self.sidebar_items.get(self.sidebar_selected) {
            Some(i) => i.clone(),
            None => return,
        };

        match self.sidebar_mode {
            SidebarMode::Artists => {
                // Populate album list for this artist.
                let tracks = lib.artist_tracks(&item);
                let mut album_map: std::collections::BTreeMap<String, (Option<u32>, usize)> =
                    std::collections::BTreeMap::new();
                for t in &tracks {
                    let entry = album_map.entry(t.album.clone()).or_insert((t.year, 0));
                    entry.1 += 1;
                    if entry.0.is_none() && t.year.is_some() {
                        entry.0 = t.year;
                    }
                }
                self.album_list = album_map
                    .into_iter()
                    .map(|(name, (year, count))| AlbumInfo {
                        name,
                        artist: item.clone(),
                        year,
                        track_count: count,
                    })
                    .collect();
                self.album_selected = 0;
                // Auto-select first album to show its tracks.
                self.select_album();
            }
            SidebarMode::Albums => {
                // item format: "Artist — Album"
                self.album_list.clear();
                self.track_list = if let Some((_, album)) = item.split_once(" — ") {
                    let tracks: Vec<&Track> = lib
                        .tracks
                        .values()
                        .filter(|t| t.album.eq_ignore_ascii_case(album))
                        .collect();
                    tracks_to_info(tracks)
                } else {
                    Vec::new()
                };
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
            }
            SidebarMode::Playlists => {
                // item format: "Name (N tracks)"
                self.album_list.clear();
                let name = item.rfind(" (").map(|pos| &item[..pos]).unwrap_or(&item);
                let tracks = lib.playlist_tracks(name);
                self.track_list = tracks_to_info(tracks);
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
            }
        }
    }

    pub fn select_album(&mut self) {
        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        let album = match self.album_list.get(self.album_selected) {
            Some(a) => a.clone(),
            None => {
                self.track_list.clear();
                return;
            }
        };

        let tracks: Vec<&Track> = lib
            .tracks
            .values()
            .filter(|t| {
                t.album.eq_ignore_ascii_case(&album.name)
                    && t.artist.eq_ignore_ascii_case(&album.artist)
            })
            .collect();
        self.track_list = tracks_to_info(tracks);
        // Sort by track number for album views.
        self.track_list
            .sort_by(|a, b| a.track_number.cmp(&b.track_number));
        self.track_selected = 0;
        self.track_scroll = 0;
    }

    pub fn has_album_browser(&self) -> bool {
        !self.album_list.is_empty()
    }

    fn sort_tracks(&mut self) {
        let asc = self.sort_ascending;
        match self.sort_column {
            SortColumn::Number => self.track_list.sort_by(|a, b| {
                let cmp = a.track_number.cmp(&b.track_number);
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
            SortColumn::Name => self.track_list.sort_by(|a, b| {
                let cmp = a.name.to_lowercase().cmp(&b.name.to_lowercase());
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
            SortColumn::Artist => self.track_list.sort_by(|a, b| {
                let cmp = a.artist.to_lowercase().cmp(&b.artist.to_lowercase());
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
            SortColumn::Album => self.track_list.sort_by(|a, b| {
                let cmp = a.album.to_lowercase().cmp(&b.album.to_lowercase());
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
            SortColumn::Duration => self.track_list.sort_by(|a, b| {
                let cmp = a.duration_ms.cmp(&b.duration_ms);
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
            SortColumn::Format => self.track_list.sort_by(|a, b| {
                let cmp = a.kind.cmp(&b.kind);
                if asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }),
        }
    }

    pub fn cycle_sort(&mut self) {
        use SortColumn::*;
        self.sort_column = match self.sort_column {
            Number => Name,
            Name => Artist,
            Artist => Album,
            Album => Duration,
            Duration => Format,
            Format => Number,
        };
        self.sort_ascending = true;
        self.sort_tracks();
    }

    pub fn add_selected_track_to_queue(&mut self) {
        if let Some(track) = self.track_list.get(self.track_selected) {
            if let Some(ref loc) = track.location {
                let item = SyncItem {
                    artist: track.artist.clone(),
                    album: track.album.clone(),
                    name: track.name.clone(),
                    location: loc.clone(),
                };
                self.sync_queue.push(QueuedItem {
                    label: format!("{} - {}", track.artist, track.name),
                    tracks: vec![item],
                });
                self.set_toast(format!("Added \"{}\" to queue", track.name), false);
            } else {
                self.set_toast("Track has no file location".into(), true);
            }
        }
    }

    pub fn add_all_visible_to_queue(&mut self) {
        let mut items = Vec::new();
        for track in &self.track_list {
            if let Some(ref loc) = track.location {
                items.push(SyncItem {
                    artist: track.artist.clone(),
                    album: track.album.clone(),
                    name: track.name.clone(),
                    location: loc.clone(),
                });
            }
        }
        if items.is_empty() {
            self.set_toast("No tracks with file locations".into(), true);
            return;
        }
        let count = items.len();
        let label = match self.sidebar_items.get(self.sidebar_selected) {
            Some(name) => format!("{} ({} tracks)", name, count),
            None => format!("{} tracks", count),
        };
        self.sync_queue.push(QueuedItem {
            label,
            tracks: items,
        });
        self.set_toast(format!("Added {} tracks to queue", count), false);
    }

    pub fn add_sidebar_item_to_queue(&mut self) {
        // First select to populate track list, then add all.
        self.select_sidebar_item();
        self.add_all_visible_to_queue();
    }

    pub fn remove_queue_item(&mut self) {
        if !self.sync_queue.is_empty() {
            self.sync_queue.remove(self.queue_selected);
            if self.queue_selected >= self.sync_queue.len() && self.queue_selected > 0 {
                self.queue_selected -= 1;
            }
        }
    }

    pub fn clear_queue(&mut self) {
        self.sync_queue.clear();
        self.queue_selected = 0;
    }

    pub fn execute_sync(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        if self.sync_queue.is_empty() || self.device_status != DeviceStatus::Connected {
            return;
        }
        self.sync_log.clear();
        let items: Vec<SyncItem> = self
            .sync_queue
            .iter()
            .flat_map(|q| q.tracks.clone())
            .collect();
        let _ = cmd_tx.send(BgCommand::ExecuteSyncQueue(items));
    }

    pub fn handle_bg_event(&mut self, event: BgEvent) {
        match event {
            BgEvent::LibraryLoaded(result) => {
                self.loading_library = false;
                match result {
                    Ok(lib) => {
                        self.library = Some(lib);
                        self.refresh_sidebar();
                    }
                    Err(e) => {
                        self.set_toast(format!("Library: {}", e), true);
                    }
                }
            }
            BgEvent::DeviceDetected(info) => {
                self.device_name = Some(info.name);
                self.device_firmware = info.firmware_version;
                self.device_serial = info.serial_number;
                self.device_manufacturer = info.manufacturer;
                self.device_model = info.model;
                self.device_usb_mode = info.usb_mode;
                self.device_status = DeviceStatus::Connecting;
            }
            BgEvent::SessionReady(storage) => {
                self.device_status = DeviceStatus::Connected;
                self.device_storage = storage;
                self.set_toast("Device connected".into(), false);
            }
            BgEvent::SessionFailed(e) => {
                self.device_status = DeviceStatus::Disconnected;
                self.set_toast(format!("Connection failed: {}", e), true);
            }
            BgEvent::LoadingDeviceTracks => {
                self.device_loading_tracks = true;
                self.set_toast("Loading device tracks...".into(), false);
            }
            BgEvent::DeviceTracksLoaded(tracks) => {
                self.device_loading_tracks = false;
                self.device_tracks = tracks;
                self.set_toast(
                    format!("Loaded {} device tracks", self.device_tracks.len()),
                    false,
                );
            }
            BgEvent::Error(e) => {
                self.device_loading_tracks = false;
                self.set_toast(e, true);
            }
            BgEvent::SyncMessage(msg) => {
                self.sync_log.push(msg);
            }
            BgEvent::SyncProgress {
                current,
                total,
                track_name,
            } => {
                self.sync_status = SyncStatus::Running { current, total };
                self.sync_current_track = track_name;
            }
            BgEvent::SyncTrackDone {
                track_name,
                success,
                error,
            } => {
                if !success {
                    self.set_toast(
                        format!("Failed: {} - {}", track_name, error.unwrap_or_default()),
                        true,
                    );
                }
            }
            BgEvent::SyncComplete {
                success,
                failed,
                skipped,
            } => {
                self.sync_status = SyncStatus::Complete {
                    success,
                    failed,
                    skipped,
                };
                self.sync_queue.clear();
                self.queue_selected = 0;
                self.set_toast(
                    format!("Sync complete: {} done, {} failed", success, failed),
                    failed > 0,
                );
            }
        }
    }

    pub fn set_toast(&mut self, msg: String, is_error: bool) {
        self.toast_message = Some((msg, Instant::now(), is_error));
    }

    pub fn tick(&mut self) {
        self.throbber_state.calc_next();

        // Auto-dismiss toast after 5 seconds.
        if let Some((_, time, _)) = &self.toast_message {
            if time.elapsed().as_secs() >= 5 {
                self.toast_message = None;
            }
        }
    }

    pub fn track_count(&self) -> usize {
        self.library.as_ref().map(|l| l.tracks.len()).unwrap_or(0)
    }

    pub fn total_queue_tracks(&self) -> usize {
        self.sync_queue.iter().map(|q| q.tracks.len()).sum()
    }

    // Navigation helpers

    pub fn move_up(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_selected > 0 {
                    self.sidebar_selected -= 1;
                }
            }
            Panel::Albums => {
                if self.album_selected > 0 {
                    self.album_selected -= 1;
                    self.select_album();
                }
            }
            Panel::TrackList => {
                if self.track_selected > 0 {
                    self.track_selected -= 1;
                }
            }
            Panel::Device => {
                if self.device_selected > 0 {
                    self.device_selected -= 1;
                }
            }
            Panel::SyncQueue => {
                if self.queue_selected > 0 {
                    self.queue_selected -= 1;
                }
            }
        }
    }

    pub fn move_down(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_selected + 1 < self.sidebar_items.len() {
                    self.sidebar_selected += 1;
                }
            }
            Panel::Albums => {
                if self.album_selected + 1 < self.album_list.len() {
                    self.album_selected += 1;
                    self.select_album();
                }
            }
            Panel::TrackList => {
                if self.track_selected + 1 < self.track_list.len() {
                    self.track_selected += 1;
                }
            }
            Panel::Device => {
                if self.device_selected + 1 < self.device_tracks.len() {
                    self.device_selected += 1;
                }
            }
            Panel::SyncQueue => {
                if self.queue_selected + 1 < self.sync_queue.len() {
                    self.queue_selected += 1;
                }
            }
        }
    }

    /// Jump forward to the next letter group in the sidebar.
    pub fn skip_forward(&mut self) {
        if self.active_panel != Panel::Library || self.sidebar_items.is_empty() {
            return;
        }
        let current_char = first_char_upper(&self.sidebar_items[self.sidebar_selected]);
        for i in (self.sidebar_selected + 1)..self.sidebar_items.len() {
            if first_char_upper(&self.sidebar_items[i]) != current_char {
                self.sidebar_selected = i;
                return;
            }
        }
        // Wrap to top if at the end.
        self.sidebar_selected = 0;
    }

    /// Jump backward to the previous letter group in the sidebar.
    pub fn skip_back(&mut self) {
        if self.active_panel != Panel::Library || self.sidebar_items.is_empty() {
            return;
        }
        if self.sidebar_selected == 0 {
            // Wrap to last item.
            self.sidebar_selected = self.sidebar_items.len() - 1;
            return;
        }
        let current_char = first_char_upper(&self.sidebar_items[self.sidebar_selected]);
        // First, go to the start of the current letter group.
        let mut i = self.sidebar_selected;
        while i > 0 && first_char_upper(&self.sidebar_items[i - 1]) == current_char {
            i -= 1;
        }
        if i > 0 {
            // Jump to start of previous letter group.
            let prev_char = first_char_upper(&self.sidebar_items[i - 1]);
            while i > 0 && first_char_upper(&self.sidebar_items[i - 1]) == prev_char {
                i -= 1;
            }
            self.sidebar_selected = i;
        } else {
            // Already at the first group, wrap to last.
            self.sidebar_selected = self.sidebar_items.len() - 1;
        }
    }

    pub fn cycle_panel(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Library => {
                if self.has_album_browser() {
                    Panel::Albums
                } else {
                    Panel::TrackList
                }
            }
            Panel::Albums => Panel::TrackList,
            Panel::TrackList => Panel::Device,
            Panel::Device => Panel::SyncQueue,
            Panel::SyncQueue => Panel::Library,
        };
    }

    pub fn cycle_panel_back(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Library => Panel::SyncQueue,
            Panel::Albums => Panel::Library,
            Panel::TrackList => {
                if self.has_album_browser() {
                    Panel::Albums
                } else {
                    Panel::Library
                }
            }
            Panel::Device => Panel::TrackList,
            Panel::SyncQueue => Panel::Device,
        };
    }
}

fn first_char_upper(s: &str) -> char {
    s.chars()
        .next()
        .unwrap_or(' ')
        .to_uppercase()
        .next()
        .unwrap_or(' ')
}

fn tracks_to_info(tracks: Vec<&Track>) -> Vec<TrackInfo> {
    tracks
        .into_iter()
        .map(|t| TrackInfo {
            name: t.name.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration_ms: t.total_time_ms,
            kind: t.kind.clone(),
            location: t.location.clone(),
            track_number: t.track_number,
        })
        .collect()
}

pub fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

pub fn format_duration(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_with_commas_cases() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(5), "5");
        assert_eq!(format_with_commas(999), "999");
        assert_eq!(format_with_commas(1000), "1,000");
        assert_eq!(format_with_commas(12345), "12,345");
        assert_eq!(format_with_commas(1_000_000), "1,000,000");
    }

    #[test]
    fn format_duration_cases() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(1000), "0:01");
        assert_eq!(format_duration(60_000), "1:00");
        assert_eq!(format_duration(61_000), "1:01");
        assert_eq!(format_duration(3_723_000), "62:03");
        assert_eq!(format_duration(500), "0:00"); // rounds down
    }

    #[test]
    fn first_char_upper_cases() {
        assert_eq!(first_char_upper("hello"), 'H');
        assert_eq!(first_char_upper("Hello"), 'H');
        assert_eq!(first_char_upper("123"), '1');
        assert_eq!(first_char_upper(""), ' ');
        assert_eq!(first_char_upper("über"), 'Ü');
    }

    #[test]
    fn throbber_state_advances() {
        let mut app = App::new();
        let first = app.throbber_state.index();
        app.tick();
        let second = app.throbber_state.index();
        assert_ne!(first, second);
    }

    #[test]
    fn new_app_defaults() {
        let app = App::new();
        assert_eq!(app.active_panel, Panel::Library);
        assert_eq!(app.sidebar_mode, SidebarMode::Artists);
        assert!(app.sync_queue.is_empty());
        assert!(app.sync_log.is_empty());
        assert_eq!(app.device_status, DeviceStatus::Disconnected);
        assert_eq!(app.sync_status, SyncStatus::Idle);
        assert!(!app.should_quit);
    }

    #[test]
    fn cycle_panel_wraps() {
        let mut app = App::new();
        assert_eq!(app.active_panel, Panel::Library);
        app.cycle_panel(); // Library -> TrackList (no albums)
        assert_eq!(app.active_panel, Panel::TrackList);
        app.cycle_panel(); // TrackList -> Device
        assert_eq!(app.active_panel, Panel::Device);
        app.cycle_panel(); // Device -> SyncQueue
        assert_eq!(app.active_panel, Panel::SyncQueue);
        app.cycle_panel(); // SyncQueue -> Library
        assert_eq!(app.active_panel, Panel::Library);
    }

    #[test]
    fn cycle_sort_advances_column() {
        let mut app = App::new();
        assert_eq!(app.sort_column, SortColumn::Name);
        app.cycle_sort();
        assert_eq!(app.sort_column, SortColumn::Artist);
        app.cycle_sort();
        assert_eq!(app.sort_column, SortColumn::Album);
    }

    #[test]
    fn queue_add_remove_clear() {
        let mut app = App::new();
        assert!(app.sync_queue.is_empty());
        assert_eq!(app.total_queue_tracks(), 0);

        app.sync_queue.push(QueuedItem {
            label: "Test".into(),
            tracks: vec![],
        });
        assert_eq!(app.sync_queue.len(), 1);

        app.sync_queue.push(QueuedItem {
            label: "Test2".into(),
            tracks: vec![],
        });
        app.queue_selected = 0;
        app.remove_queue_item();
        assert_eq!(app.sync_queue.len(), 1);
        assert_eq!(app.sync_queue[0].label, "Test2");

        app.clear_queue();
        assert!(app.sync_queue.is_empty());
        assert_eq!(app.queue_selected, 0);
    }

    #[test]
    fn remove_queue_item_empty_noop() {
        let mut app = App::new();
        app.remove_queue_item(); // should not panic
        assert!(app.sync_queue.is_empty());
    }

    #[test]
    fn move_up_down_bounds() {
        let mut app = App::new();
        app.sidebar_items = vec!["A".into(), "B".into(), "C".into()];
        app.active_panel = Panel::Library;

        app.move_up(); // already at 0
        assert_eq!(app.sidebar_selected, 0);

        app.move_down();
        assert_eq!(app.sidebar_selected, 1);
        app.move_down();
        assert_eq!(app.sidebar_selected, 2);
        app.move_down(); // at end
        assert_eq!(app.sidebar_selected, 2);
    }

    #[test]
    fn toast_auto_dismisses() {
        let mut app = App::new();
        app.set_toast("hello".into(), false);
        assert!(app.toast_message.is_some());

        // Manually set the timestamp to 6 seconds ago.
        if let Some((_, ref mut time, _)) = app.toast_message {
            *time = Instant::now() - std::time::Duration::from_secs(6);
        }
        app.tick();
        assert!(app.toast_message.is_none());
    }

    #[test]
    fn skip_forward_wraps() {
        let mut app = App::new();
        app.active_panel = Panel::Library;
        app.sidebar_items = vec![
            "Apple".into(),
            "Avocado".into(),
            "Banana".into(),
            "Cherry".into(),
        ];
        app.sidebar_selected = 0;
        app.skip_forward(); // A -> B
        assert_eq!(app.sidebar_selected, 2);
        app.skip_forward(); // B -> C
        assert_eq!(app.sidebar_selected, 3);
        app.skip_forward(); // C -> wrap to 0
        assert_eq!(app.sidebar_selected, 0);
    }
}
