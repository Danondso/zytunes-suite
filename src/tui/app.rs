use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Instant;

use image::DynamicImage;
use throbber_widgets_tui::ThrobberState;
use zytunes::library::{MusicLibrary, Track};
use zytunes::mtp::parse::DeviceEntry;

use crate::audio::{AudioCommand, AudioEvent};
use crate::background::{BgCommand, BgEvent, StorageInfo, SyncItem};
use crate::theme::{Theme, THEMES};

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
pub enum BrowseMode {
    Library,
    Device,
}

#[derive(Clone, Debug)]
pub struct DeviceTrackInfo {
    pub name: String,
    pub device_path: String,
    #[allow(dead_code)]
    pub size: u64,
    pub object_id: u64,
    pub artist: String,
    pub album: String,
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
    Running { current: usize, total: usize },
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

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

pub struct NowPlaying {
    pub track_name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub elapsed_ms: u64,
    pub state: PlaybackState,
    pub track_index: usize,
    pub playlist: Vec<TrackInfo>,
    /// Frame at which playback was paused (freezes animation).
    pub paused_frame: Option<usize>,
}

#[derive(Clone)]
pub struct QueuedItem {
    pub label: String,
    pub tracks: Vec<SyncItem>,
}

pub struct DeviceState {
    pub status: DeviceStatus,
    pub name: Option<String>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub usb_mode: Option<String>,
    pub storage: Option<StorageInfo>,
    pub tracks: Vec<DeviceEntry>,
    pub loading_tracks: bool,
    pub selected: usize,
    pub artists: Vec<String>,
    pub albums: BTreeMap<String, Vec<String>>,
    pub album_tracks: BTreeMap<(String, String), Vec<DeviceTrackInfo>>,
}

impl DeviceState {
    pub fn new() -> Self {
        DeviceState {
            status: DeviceStatus::Disconnected,
            name: None,
            firmware: None,
            serial: None,
            manufacturer: None,
            model: None,
            usb_mode: None,
            storage: None,
            tracks: Vec::new(),
            loading_tracks: false,
            selected: 0,
            artists: Vec::new(),
            albums: BTreeMap::new(),
            album_tracks: BTreeMap::new(),
        }
    }
}

pub struct SyncState {
    pub queue: Vec<QueuedItem>,
    pub queue_selected: usize,
    pub status: SyncStatus,
    pub current_track: String,
    pub log: Vec<String>,
    /// Lines scrolled up from the bottom of the log. 0 = pinned to bottom.
    pub log_scroll: usize,
}

impl SyncState {
    pub fn new() -> Self {
        SyncState {
            queue: Vec::new(),
            queue_selected: 0,
            status: SyncStatus::Idle,
            current_track: String::new(),
            log: Vec::new(),
            log_scroll: 0,
        }
    }

    /// Scroll the log up by `n` lines.
    pub fn log_scroll_up(&mut self, n: usize) {
        let max = self.log.len().saturating_sub(1);
        self.log_scroll = (self.log_scroll + n).min(max);
    }

    /// Scroll the log down by `n` lines (toward the bottom).
    pub fn log_scroll_down(&mut self, n: usize) {
        self.log_scroll = self.log_scroll.saturating_sub(n);
    }
}

pub struct App {
    pub active_panel: Panel,
    pub library: Option<Box<dyn MusicLibrary>>,
    pub sidebar_mode: SidebarMode,
    pub sidebar_items: Vec<String>,
    pub sidebar_selected: usize,
    pub sidebar_scroll: usize,
    /// Per-mode saved selection positions: [Artists, Albums, Playlists] x [Library, Device]
    saved_sidebar_pos: [[usize; 3]; 2],
    pub album_list: Vec<AlbumInfo>,
    pub album_selected: usize,
    pub track_list: Vec<TrackInfo>,
    pub track_selected: usize,
    pub track_scroll: usize,
    pub sort_column: SortColumn,
    pub sort_ascending: bool,
    pub device: DeviceState,
    pub device_index_dirty: bool,
    pub browse_mode: BrowseMode,
    pub sync: SyncState,
    pub throbber_state: ThrobberState,
    pub anim_frame: usize,
    pub connection_anim_start: Option<usize>,
    pub now_playing: Option<NowPlaying>,
    pub removal_queue: Vec<(String, u64)>,
    pub pending_removal: Option<Vec<(String, u64)>>,
    pub pending_cache_clear: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_keys: bool,
    pub search_active: bool,
    pub search_query: String,
    pub toast_message: Option<(String, Instant, bool)>, // (msg, time, is_error)
    pub library_path: Option<String>,
    pub loading_library: bool,
    pub theme_index: usize,
    pub show_theme_picker: bool,
    pub theme_picker_index: usize,
    pub theme_before_picker: usize,
    /// Cached album art extracted from ID3 tags.
    pub album_art: Option<DynamicImage>,
    /// Key used to avoid re-extracting art (e.g. "artist/album").
    album_art_key: String,
    /// Cached halfblock art lines: (char, fg_rgb, bg_rgb) per cell.
    #[allow(clippy::type_complexity)]
    pub album_art_lines: Vec<Vec<(char, [u8; 3], [u8; 3])>>,
    /// Dimensions (w, h) the cached ASCII art was rendered for.
    album_art_size: (u16, u16),
}

#[derive(Clone)]
pub struct AlbumInfo {
    pub name: String,
    pub artist: String,
    pub year: Option<u32>,
    #[allow(dead_code)]
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
            saved_sidebar_pos: [[0; 3]; 2],
            album_list: Vec::new(),
            album_selected: 0,
            track_list: Vec::new(),
            track_selected: 0,
            track_scroll: 0,
            sort_column: SortColumn::Name,
            sort_ascending: true,
            device: DeviceState::new(),
            device_index_dirty: false,
            browse_mode: BrowseMode::Library,
            sync: SyncState::new(),
            throbber_state: ThrobberState::default(),
            anim_frame: 0,
            connection_anim_start: None,
            now_playing: None,
            removal_queue: Vec::new(),
            pending_removal: None,
            pending_cache_clear: false,
            should_quit: false,
            show_help: false,
            show_keys: false,
            search_active: false,
            search_query: String::new(),
            toast_message: None,
            library_path: None,
            loading_library: false,
            theme_index: 0,
            show_theme_picker: false,
            theme_picker_index: 0,
            theme_before_picker: 0,
            album_art: None,
            album_art_key: String::new(),
            album_art_lines: Vec::new(),
            album_art_size: (0, 0),
        }
    }

    pub fn theme(&self) -> &'static Theme {
        &THEMES[self.theme_index.min(THEMES.len() - 1)]
    }

    pub fn open_theme_picker(&mut self) {
        self.theme_before_picker = self.theme_index;
        self.theme_picker_index = self.theme_index;
        self.show_theme_picker = true;
    }

    pub fn theme_picker_move(&mut self, delta: isize) {
        let len = THEMES.len();
        self.theme_picker_index =
            (self.theme_picker_index as isize + delta).rem_euclid(len as isize) as usize;
        self.theme_index = self.theme_picker_index;
    }

    pub fn theme_picker_confirm(&mut self) {
        self.show_theme_picker = false;
        // Save to config.
        let mut config = crate::config::load();
        config.theme = Some(self.theme().name.to_string());
        crate::config::save(&config);
    }

    pub fn theme_picker_cancel(&mut self) {
        self.theme_index = self.theme_before_picker;
        self.show_theme_picker = false;
    }

    // -- Playback controls --

    pub fn play_selected_track(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        if self.track_list.is_empty() {
            return;
        }
        let index = self.track_selected.min(self.track_list.len() - 1);
        let track = &self.track_list[index];
        let path = match &track.location {
            Some(p) => p.clone(),
            None => {
                self.set_toast("No file path for this track".into(), true);
                return;
            }
        };
        let _ = audio_tx.send(AudioCommand::Play { path });
        self.now_playing = Some(NowPlaying {
            track_name: track.name.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms.unwrap_or(0),
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: index,
            playlist: self.track_list.clone(),
            paused_frame: None,
        });
    }

    pub fn toggle_playback(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        match &self.now_playing {
            Some(np) if np.state == PlaybackState::Playing => {
                let _ = audio_tx.send(AudioCommand::Pause);
                if let Some(ref mut np) = self.now_playing {
                    np.state = PlaybackState::Paused;
                    np.paused_frame = Some(self.anim_frame);
                }
            }
            Some(np) if np.state == PlaybackState::Paused => {
                let _ = audio_tx.send(AudioCommand::Resume);
                if let Some(ref mut np) = self.now_playing {
                    np.state = PlaybackState::Playing;
                    np.paused_frame = None;
                }
            }
            _ => {
                self.play_selected_track(audio_tx);
            }
        }
    }

    pub fn next_track(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        if let Some(ref np) = self.now_playing {
            let next_idx = np.track_index + 1;
            let playlist = np.playlist.clone();
            if next_idx < playlist.len() {
                self.play_from_playlist(next_idx, &playlist, audio_tx);
            } else {
                // End of playlist
                let _ = audio_tx.send(AudioCommand::Stop);
                self.now_playing = None;
            }
        }
    }

    pub fn prev_track(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        if let Some(ref np) = self.now_playing {
            let playlist = np.playlist.clone();
            if np.elapsed_ms > 3000 || np.track_index == 0 {
                // Restart current track
                self.play_from_playlist(np.track_index, &playlist, audio_tx);
            } else {
                self.play_from_playlist(np.track_index - 1, &playlist, audio_tx);
            }
        }
    }

    pub fn stop_playback(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        let _ = audio_tx.send(AudioCommand::Stop);
        self.now_playing = None;
    }

    fn play_from_playlist(
        &mut self,
        index: usize,
        playlist: &[TrackInfo],
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) {
        let track = &playlist[index];
        let path = match &track.location {
            Some(p) => p.clone(),
            None => {
                self.set_toast("No file path for this track".into(), true);
                return;
            }
        };
        let _ = audio_tx.send(AudioCommand::Play { path });
        self.now_playing = Some(NowPlaying {
            track_name: track.name.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms.unwrap_or(0),
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: index,
            playlist: playlist.to_vec(),
            paused_frame: None,
        });
    }

    pub fn handle_audio_event(&mut self, event: AudioEvent, audio_tx: &mpsc::Sender<AudioCommand>) {
        match event {
            AudioEvent::Position { elapsed_ms } => {
                if let Some(ref mut np) = self.now_playing {
                    np.elapsed_ms = elapsed_ms;
                }
            }
            AudioEvent::TrackEnded => {
                self.next_track(audio_tx);
            }
            AudioEvent::PlaybackError(msg) => {
                self.now_playing = None;
                self.set_toast(format!("Playback: {}", msg), true);
            }
        }
    }

    /// Rebuild device index if it was marked dirty by incremental updates.
    pub fn flush_device_index(&mut self) {
        if self.device_index_dirty {
            self.device_index_dirty = false;
            self.build_device_index();
            if self.browse_mode == BrowseMode::Device {
                self.refresh_sidebar();
            }
        }
    }

    pub fn build_device_index(&mut self) {
        self.device.artists.clear();
        self.device.albums.clear();
        self.device.album_tracks.clear();

        let mut artist_set = std::collections::BTreeSet::new();

        for entry in &self.device.tracks {
            if entry.is_dir() {
                continue;
            }
            // Name format from lsext-r: "Artist/Album/track.mp3"
            let parts: Vec<&str> = entry.name.splitn(3, '/').collect();
            let (artist, album, filename) = match parts.len() {
                3 => (
                    parts[0].to_string(),
                    parts[1].to_string(),
                    parts[2].to_string(),
                ),
                2 => (
                    parts[0].to_string(),
                    "Unknown Album".to_string(),
                    parts[1].to_string(),
                ),
                _ => (
                    "Unknown Artist".to_string(),
                    "Unknown Album".to_string(),
                    entry.name.clone(),
                ),
            };

            // Strip file extension for display name.
            let display_name = filename
                .rfind('.')
                .map(|pos| &filename[..pos])
                .unwrap_or(&filename)
                .to_string();

            artist_set.insert(artist.clone());

            self.device
                .albums
                .entry(artist.clone())
                .or_default()
                .push(album.clone());

            let key = (artist.clone(), album.clone());
            self.device
                .album_tracks
                .entry(key)
                .or_default()
                .push(DeviceTrackInfo {
                    name: display_name,
                    device_path: format!("/Music/{}", entry.name),
                    size: entry.size,
                    object_id: entry.object_id,
                    artist,
                    album,
                });
        }

        self.device.artists = artist_set.into_iter().collect();

        // Deduplicate album lists per artist.
        for albums in self.device.albums.values_mut() {
            albums.sort();
            albums.dedup();
        }
    }

    pub fn clear_device_index(&mut self) {
        self.device.artists.clear();
        self.device.albums.clear();
        self.device.album_tracks.clear();
        if self.browse_mode == BrowseMode::Device {
            self.browse_mode = BrowseMode::Library;
            self.refresh_sidebar();
        }
    }

    pub fn toggle_browse_mode(&mut self) {
        self.save_sidebar_pos();
        self.browse_mode = match self.browse_mode {
            BrowseMode::Library => BrowseMode::Device,
            BrowseMode::Device => BrowseMode::Library,
        };
        self.refresh_sidebar();
        self.active_panel = Panel::Library;
    }

    /// Collect device paths for the currently selected item(s) based on active panel.
    pub fn collect_device_removal_paths(&self) -> Vec<(String, u64)> {
        if self.browse_mode != BrowseMode::Device {
            return Vec::new();
        }
        match self.active_panel {
            Panel::TrackList => {
                if let Some(track) = self.track_list.get(self.track_selected) {
                    // Find the DeviceTrackInfo with matching name in current context.
                    if let Some(item) = self.sidebar_items.get(self.sidebar_selected) {
                        let (artist, album) = self.resolve_device_artist_album(item);
                        if let Some(tracks) = self.device.album_tracks.get(&(artist, album)) {
                            if let Some(dt) = tracks.iter().find(|dt| dt.name == track.name) {
                                return vec![(dt.device_path.clone(), dt.object_id)];
                            }
                        }
                    }
                }
                Vec::new()
            }
            Panel::Albums => {
                // All tracks in the selected album.
                if let Some(album_info) = self.album_list.get(self.album_selected) {
                    let key = (album_info.artist.clone(), album_info.name.clone());
                    if let Some(tracks) = self.device.album_tracks.get(&key) {
                        return tracks
                            .iter()
                            .map(|t| (t.device_path.clone(), t.object_id))
                            .collect();
                    }
                }
                Vec::new()
            }
            Panel::Library => {
                if let Some(item) = self.sidebar_items.get(self.sidebar_selected) {
                    self.collect_sidebar_removal_paths(item)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_sidebar_removal_paths(&self, item: &str) -> Vec<(String, u64)> {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                let Some(albums) = self.device.albums.get(item) else {
                    return Vec::new();
                };
                let mut items = Vec::new();
                for album in albums {
                    let key = (item.to_string(), album.clone());
                    if let Some(tracks) = self.device.album_tracks.get(&key) {
                        items.extend(tracks.iter().map(|t| (t.device_path.clone(), t.object_id)));
                    }
                }
                items
            }
            SidebarMode::Albums => {
                let (artist, album) = self.resolve_device_artist_album(item);
                self.device
                    .album_tracks
                    .get(&(artist, album))
                    .map(|tracks| {
                        tracks
                            .iter()
                            .map(|t| (t.device_path.clone(), t.object_id))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            SidebarMode::Playlists => Vec::new(),
        }
    }

    fn resolve_device_artist_album(&self, item: &str) -> (String, String) {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                let album = self
                    .album_list
                    .get(self.album_selected)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                (item.to_string(), album)
            }
            SidebarMode::Albums => {
                if let Some((artist, album)) = item.split_once(" \u{2014} ") {
                    (artist.to_string(), album.to_string())
                } else {
                    (item.to_string(), String::new())
                }
            }
            SidebarMode::Playlists => (String::new(), String::new()),
        }
    }

    fn sidebar_mode_index(&self) -> usize {
        match self.sidebar_mode {
            SidebarMode::Artists => 0,
            SidebarMode::Albums => 1,
            SidebarMode::Playlists => 2,
        }
    }

    fn browse_mode_index(&self) -> usize {
        match self.browse_mode {
            BrowseMode::Library => 0,
            BrowseMode::Device => 1,
        }
    }

    /// Save the current sidebar selection for the active mode.
    pub fn save_sidebar_pos(&mut self) {
        let b = self.browse_mode_index();
        let m = self.sidebar_mode_index();
        self.saved_sidebar_pos[b][m] = self.sidebar_selected;
    }

    /// Restore the saved sidebar selection for the active mode, clamped to list bounds.
    fn restore_sidebar_pos(&mut self) {
        let b = self.browse_mode_index();
        let m = self.sidebar_mode_index();
        let saved = self.saved_sidebar_pos[b][m];
        if self.sidebar_items.is_empty() {
            self.sidebar_selected = 0;
        } else {
            self.sidebar_selected = saved.min(self.sidebar_items.len() - 1);
        }
    }

    pub fn refresh_sidebar(&mut self) {
        match self.browse_mode {
            BrowseMode::Library => {
                let lib = match &self.library {
                    Some(l) => l,
                    None => {
                        self.sidebar_items.clear();
                        return;
                    }
                };

                self.sidebar_items = match self.sidebar_mode {
                    SidebarMode::Artists => {
                        lib.artists().into_iter().map(|s| s.to_string()).collect()
                    }
                    SidebarMode::Albums => lib
                        .albums()
                        .into_iter()
                        .map(|(artist, album)| format!("{} \u{2014} {}", artist, album))
                        .collect(),
                    SidebarMode::Playlists => lib
                        .user_playlists()
                        .into_iter()
                        .map(|p| format!("{} ({} tracks)", p.name, p.track_ids.len()))
                        .collect(),
                };
            }
            BrowseMode::Device => {
                self.sidebar_items = match self.sidebar_mode {
                    SidebarMode::Artists => self.device.artists.clone(),
                    SidebarMode::Albums => {
                        let mut items = Vec::new();
                        for (artist, albums) in &self.device.albums {
                            for album in albums {
                                items.push(format!("{} \u{2014} {}", artist, album));
                            }
                        }
                        items.sort();
                        items
                    }
                    SidebarMode::Playlists => Vec::new(),
                };
            }
        }

        if self.search_active && !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            self.sidebar_items
                .retain(|item| item.to_lowercase().contains(&q));
        }

        self.restore_sidebar_pos();
        self.sidebar_scroll = 0;
        self.album_list.clear();
        self.track_list.clear();
        self.track_selected = 0;
        self.track_scroll = 0;
    }

    pub fn select_sidebar_item(&mut self) {
        let item = match self.sidebar_items.get(self.sidebar_selected) {
            Some(i) => i.clone(),
            None => return,
        };

        if self.browse_mode == BrowseMode::Device {
            self.select_sidebar_item_device(&item);
            return;
        }

        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        match self.sidebar_mode {
            SidebarMode::Artists => {
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
                // Sort by year (oldest first), albums without a year go last.
                self.album_list.sort_by(|a, b| match (a.year, b.year) {
                    (Some(ya), Some(yb)) => ya.cmp(&yb).then_with(|| a.name.cmp(&b.name)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.name.cmp(&b.name),
                });
                self.album_selected = 0;
                self.select_album();
            }
            SidebarMode::Albums => {
                self.album_list.clear();
                self.track_list = if let Some((_, album)) = item.split_once(" \u{2014} ") {
                    tracks_to_info(lib.album_tracks(album))
                } else {
                    Vec::new()
                };
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
                self.refresh_album_art();
            }
            SidebarMode::Playlists => {
                self.album_list.clear();
                let name = item.rfind(" (").map(|pos| &item[..pos]).unwrap_or(&item);
                let tracks = lib.playlist_tracks(name);
                self.track_list = tracks_to_info(tracks);
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
                self.refresh_album_art();
            }
        }
    }

    fn select_sidebar_item_device(&mut self, item: &str) {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                if let Some(albums) = self.device.albums.get(item) {
                    self.album_list = albums
                        .iter()
                        .map(|album_name| {
                            let count = self
                                .device
                                .album_tracks
                                .get(&(item.to_string(), album_name.clone()))
                                .map(|t| t.len())
                                .unwrap_or(0);
                            AlbumInfo {
                                name: album_name.clone(),
                                artist: item.to_string(),
                                year: None,
                                track_count: count,
                            }
                        })
                        .collect();
                    self.album_selected = 0;
                    self.select_album();
                }
            }
            SidebarMode::Albums => {
                self.album_list.clear();
                if let Some((artist, album)) = item.split_once(" \u{2014} ") {
                    let key = (artist.to_string(), album.to_string());
                    self.track_list = match self.device.album_tracks.get(&key) {
                        Some(tracks) => device_tracks_to_info(tracks),
                        None => Vec::new(),
                    };
                } else {
                    self.track_list = Vec::new();
                }
                self.track_selected = 0;
                self.track_scroll = 0;
            }
            SidebarMode::Playlists => {
                self.album_list.clear();
                self.track_list.clear();
                self.track_selected = 0;
                self.track_scroll = 0;
            }
        }
    }

    pub fn select_album(&mut self) {
        let album = match self.album_list.get(self.album_selected) {
            Some(a) => a.clone(),
            None => {
                self.track_list.clear();
                return;
            }
        };

        if self.browse_mode == BrowseMode::Device {
            let key = (album.artist.clone(), album.name.clone());
            self.track_list = match self.device.album_tracks.get(&key) {
                Some(tracks) => device_tracks_to_info(tracks),
                None => Vec::new(),
            };
            self.track_selected = 0;
            self.track_scroll = 0;
            return;
        }

        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        self.track_list = tracks_to_info(lib.album_tracks_by_artist(&album.artist, &album.name));
        // Sort by track number for album views.
        self.track_list
            .sort_by(|a, b| a.track_number.cmp(&b.track_number));
        self.track_selected = 0;
        self.track_scroll = 0;
        self.refresh_album_art();
    }

    pub fn has_album_browser(&self) -> bool {
        !self.album_list.is_empty()
    }

    /// Extract embedded album art from the first track that has it.
    pub fn refresh_album_art(&mut self) {
        // Build a cache key from the current track list context.
        let key = if let Some(t) = self.track_list.first() {
            format!("{}/{}", t.artist, t.album)
        } else {
            self.album_art = None;
            self.album_art_key.clear();
            self.album_art_lines.clear();
            self.album_art_size = (0, 0);
            return;
        };

        if key == self.album_art_key {
            return; // already cached
        }
        self.album_art_key = key;
        self.album_art = None;
        self.album_art_lines.clear();
        self.album_art_size = (0, 0);

        for track in &self.track_list {
            if let Some(ref loc) = track.location {
                if let Ok(tag) = id3::Tag::read_from_path(loc) {
                    if let Some(pic) = tag.pictures().next() {
                        if let Ok(img) = image::load_from_memory(&pic.data) {
                            self.album_art = Some(img);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Render album art as halfblock characters sized to a square that fits
    /// within the given terminal area. Each cell packs two vertical pixels
    /// using ▀ with fg=top color, bg=bottom color — doubling vertical resolution.
    pub fn render_album_art(&mut self, width: u16, height: u16) {
        if (width, height) == self.album_art_size && !self.album_art_lines.is_empty() {
            return;
        }
        self.album_art_size = (width, height);
        self.album_art_lines.clear();

        let img = match &self.album_art {
            Some(img) => img,
            None => return,
        };

        // Terminal chars are roughly 1:2 (w:h), so 1 cell = 1 pixel wide, 2 pixels tall.
        // Fit the image within the available area preserving aspect ratio.
        let (iw, ih) = (img.width(), img.height());
        let max_px_w = width as u32;
        let max_px_h = height as u32 * 2; // 2 pixel rows per terminal row
        let scale = (max_px_w as f64 / iw as f64).min(max_px_h as f64 / ih as f64);
        let cols = ((iw as f64 * scale).round() as u32).max(1);
        let px_h = ((ih as f64 * scale).round() as u32).max(2);
        let rows = px_h / 2;

        let resized = img.resize_exact(cols, rows * 2, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();

        for row in 0..rows {
            let mut line = Vec::with_capacity(cols as usize);
            for col in 0..cols {
                let top = rgba.get_pixel(col, row * 2);
                let bot = rgba.get_pixel(col, row * 2 + 1);
                line.push(('▀', [top[0], top[1], top[2]], [bot[0], bot[1], bot[2]]));
            }
            self.album_art_lines.push(line);
        }
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
                self.sync.queue.push(QueuedItem {
                    label: format!("{} - {} - {}", track.artist, track.album, track.name),
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
        self.sync.queue.push(QueuedItem {
            label,
            tracks: items,
        });
        self.set_toast(format!("Added {} tracks to queue", count), false);
    }

    pub fn add_sidebar_item_to_queue(&mut self) {
        let item = match self.sidebar_items.get(self.sidebar_selected) {
            Some(i) => i.clone(),
            None => return,
        };

        if self.sidebar_mode == SidebarMode::Artists {
            // For artists, gather ALL tracks across all albums.
            let lib = match &self.library {
                Some(l) => l,
                None => return,
            };
            let tracks = lib.artist_tracks(&item);
            let mut items = Vec::new();
            for t in &tracks {
                if let Some(ref loc) = t.location {
                    items.push(SyncItem {
                        artist: t.artist.clone(),
                        album: t.album.clone(),
                        name: t.name.clone(),
                        location: loc.clone(),
                    });
                }
            }
            if items.is_empty() {
                self.set_toast("No tracks with file locations".into(), true);
                return;
            }
            let count = items.len();
            self.sync.queue.push(QueuedItem {
                label: format!("{} ({} tracks)", item, count),
                tracks: items,
            });
            self.set_toast(format!("Added {} tracks to queue", count), false);
        } else {
            // For albums/playlists, select to populate track list, then add all.
            self.select_sidebar_item();
            self.add_all_visible_to_queue();
        }
    }

    pub fn remove_queue_item(&mut self) {
        if !self.sync.queue.is_empty() {
            self.sync.queue.remove(self.sync.queue_selected);
            if self.sync.queue_selected >= self.sync.queue.len() && self.sync.queue_selected > 0 {
                self.sync.queue_selected -= 1;
            }
        }
    }

    pub fn clear_queue(&mut self) {
        self.sync.queue.clear();
        self.sync.queue_selected = 0;
    }

    pub fn queue_device_removal(&mut self) {
        let items = self.collect_device_removal_paths();
        if items.is_empty() {
            self.set_toast("Nothing to queue for removal".into(), true);
            return;
        }
        let count = items.len();
        // Deduplicate by object_id
        for item in items {
            if !self
                .removal_queue
                .iter()
                .any(|(_, id)| *id == item.1 && item.1 > 0)
            {
                self.removal_queue.push(item);
            }
        }
        self.set_toast(format!("Queued {} track(s) for removal", count), false);
    }

    pub fn clear_removal_queue(&mut self) {
        self.removal_queue.clear();
    }

    pub fn execute_sync(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        if self.sync.queue.is_empty() {
            return;
        }
        if self.device.status != DeviceStatus::Connected {
            self.set_toast("Connect a device before syncing".into(), true);
            return;
        }
        self.sync.log.clear();
        let items: Vec<SyncItem> = self
            .sync
            .queue
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
                self.device.name = Some(info.name);
                self.device.firmware = info.firmware_version;
                self.device.serial = info.serial_number;
                self.device.manufacturer = info.manufacturer;
                self.device.model = info.model;
                self.device.usb_mode = info.usb_mode;
                self.device.status = DeviceStatus::Connecting;
            }
            BgEvent::SessionReady(storage) => {
                self.device.status = DeviceStatus::Connected;
                self.connection_anim_start = None;
                self.device.storage = storage;
                self.set_toast("Device connected".into(), false);
            }
            BgEvent::SessionFailed(e) => {
                self.device.status = DeviceStatus::Disconnected;
                self.connection_anim_start = None;
                self.set_toast(format!("Connection failed: {}", e), true);
            }
            BgEvent::LoadingDeviceTracks => {
                self.device.loading_tracks = true;
                self.set_toast("Loading device tracks...".into(), false);
            }
            BgEvent::DeviceTracksLoaded(tracks) => {
                self.device.loading_tracks = false;
                self.device.tracks = tracks;
                self.build_device_index();
                self.set_toast(
                    format!("Loaded {} device tracks", self.device.tracks.len()),
                    false,
                );
                if self.browse_mode == BrowseMode::Device {
                    self.refresh_sidebar();
                }
            }
            BgEvent::DeviceTrackAdded(entry) => {
                self.device.tracks.push(entry);
                self.device_index_dirty = true;
            }
            BgEvent::DeviceTrackRemoved(path) => {
                // path is a full device path like "/Music/Artist/Album/track.mp3"
                // but DeviceEntry.name is relative like "Artist/Album/track.mp3"
                let relative = path.strip_prefix("/Music/").unwrap_or(&path);
                self.device.tracks.retain(|t| t.name != relative);
                self.device_index_dirty = true;
            }
            BgEvent::Error(e) => {
                self.device.loading_tracks = false;
                self.set_toast(e, true);
            }
            BgEvent::SyncMessage(msg) => {
                self.sync.log.push(msg);
            }
            BgEvent::SyncProgress {
                current,
                total,
                track_name,
            } => {
                self.sync.status = SyncStatus::Running { current, total };
                self.sync.current_track = track_name;
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
            BgEvent::SyncComplete { success, failed } => {
                self.sync.status = SyncStatus::Idle;
                self.sync.queue.clear();
                self.sync.queue_selected = 0;
                self.set_toast(
                    format!("Sync complete: {} done, {} failed", success, failed),
                    failed > 0,
                );
            }
            BgEvent::RemoveProgress {
                current,
                total,
                name,
            } => {
                self.sync.status = SyncStatus::Running { current, total };
                self.sync.current_track = format!("Removing: {}", name);
            }
            BgEvent::RemoveComplete { success, failed } => {
                self.sync.status = SyncStatus::Idle;
                self.set_toast(
                    format!("Removed {} tracks, {} failed", success, failed),
                    failed > 0,
                );
            }
            BgEvent::StorageUpdated(storage) => {
                self.device.storage = Some(storage);
            }
        }
    }

    pub fn set_toast(&mut self, msg: String, is_error: bool) {
        self.toast_message = Some((msg, Instant::now(), is_error));
    }

    pub fn tick(&mut self) {
        self.throbber_state.calc_next();
        self.anim_frame = self.anim_frame.wrapping_add(1);

        // Auto-dismiss toast after 5 seconds.
        if let Some((_, time, _)) = &self.toast_message {
            if time.elapsed().as_secs() >= 5 {
                self.toast_message = None;
            }
        }
    }

    pub fn track_count(&self) -> usize {
        self.library.as_ref().map(|l| l.track_count()).unwrap_or(0)
    }

    pub fn total_queue_tracks(&self) -> usize {
        self.sync.queue.iter().map(|q| q.tracks.len()).sum()
    }

    // Navigation helpers

    pub fn move_up(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_selected > 0 {
                    self.sidebar_selected -= 1;
                    self.save_sidebar_pos();
                    if self.sidebar_mode == SidebarMode::Artists {
                        self.select_sidebar_item();
                    }
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
                if self.device.selected > 0 {
                    self.device.selected -= 1;
                }
            }
            Panel::SyncQueue => {
                if self.sync.queue_selected > 0 {
                    self.sync.queue_selected -= 1;
                }
            }
        }
    }

    pub fn move_down(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_selected + 1 < self.sidebar_items.len() {
                    self.sidebar_selected += 1;
                    self.save_sidebar_pos();
                    if self.sidebar_mode == SidebarMode::Artists {
                        self.select_sidebar_item();
                    }
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
                if self.device.selected + 1 < self.device.tracks.len() {
                    self.device.selected += 1;
                }
            }
            Panel::SyncQueue => {
                if self.sync.queue_selected + 1 < self.sync.queue.len() {
                    self.sync.queue_selected += 1;
                }
            }
        }
    }

    /// Jump forward to the next letter group in the sidebar, or the next
    /// year group in the album browser.
    pub fn skip_forward(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_items.is_empty() {
                    return;
                }
                let current_char = first_char_upper(&self.sidebar_items[self.sidebar_selected]);
                for i in (self.sidebar_selected + 1)..self.sidebar_items.len() {
                    if first_char_upper(&self.sidebar_items[i]) != current_char {
                        self.sidebar_selected = i;
                        self.save_sidebar_pos();
                        if self.sidebar_mode == SidebarMode::Artists {
                            self.select_sidebar_item();
                        }
                        return;
                    }
                }
                // Wrap to top if at the end.
                self.sidebar_selected = 0;
                self.save_sidebar_pos();
                if self.sidebar_mode == SidebarMode::Artists {
                    self.select_sidebar_item();
                }
            }
            Panel::Albums => {
                if self.album_list.is_empty() {
                    return;
                }
                let current_year = self.album_list[self.album_selected].year;
                for i in (self.album_selected + 1)..self.album_list.len() {
                    if self.album_list[i].year != current_year {
                        self.album_selected = i;
                        self.select_album();
                        return;
                    }
                }
                // Wrap to top.
                self.album_selected = 0;
                self.select_album();
            }
            _ => {}
        }
    }

    /// Jump backward to the previous letter group in the sidebar, or the
    /// previous year group in the album browser.
    pub fn skip_back(&mut self) {
        match self.active_panel {
            Panel::Library => {
                if self.sidebar_items.is_empty() {
                    return;
                }
                if self.sidebar_selected == 0 {
                    self.sidebar_selected = self.sidebar_items.len() - 1;
                } else {
                    let current_char = first_char_upper(&self.sidebar_items[self.sidebar_selected]);
                    let mut i = self.sidebar_selected;
                    while i > 0 && first_char_upper(&self.sidebar_items[i - 1]) == current_char {
                        i -= 1;
                    }
                    if i > 0 {
                        let prev_char = first_char_upper(&self.sidebar_items[i - 1]);
                        while i > 0 && first_char_upper(&self.sidebar_items[i - 1]) == prev_char {
                            i -= 1;
                        }
                        self.sidebar_selected = i;
                    } else {
                        self.sidebar_selected = self.sidebar_items.len() - 1;
                    }
                }
                self.save_sidebar_pos();
                if self.sidebar_mode == SidebarMode::Artists {
                    self.select_sidebar_item();
                }
            }
            Panel::Albums => {
                if self.album_list.is_empty() {
                    return;
                }
                if self.album_selected == 0 {
                    self.album_selected = self.album_list.len() - 1;
                } else {
                    let current_year = self.album_list[self.album_selected].year;
                    let mut i = self.album_selected;
                    while i > 0 && self.album_list[i - 1].year == current_year {
                        i -= 1;
                    }
                    if i > 0 {
                        let prev_year = self.album_list[i - 1].year;
                        while i > 0 && self.album_list[i - 1].year == prev_year {
                            i -= 1;
                        }
                        self.album_selected = i;
                    } else {
                        self.album_selected = self.album_list.len() - 1;
                    }
                }
                self.select_album();
            }
            _ => {}
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

fn device_tracks_to_info(tracks: &[DeviceTrackInfo]) -> Vec<TrackInfo> {
    tracks
        .iter()
        .map(|dt| TrackInfo {
            name: dt.name.clone(),
            artist: dt.artist.clone(),
            album: dt.album.clone(),
            duration_ms: None,
            kind: None,
            location: None,
            track_number: None,
        })
        .collect()
}

pub fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
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
        assert!(app.sync.queue.is_empty());
        assert!(app.sync.log.is_empty());
        assert_eq!(app.device.status, DeviceStatus::Disconnected);
        assert_eq!(app.sync.status, SyncStatus::Idle);
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
        assert!(app.sync.queue.is_empty());
        assert_eq!(app.total_queue_tracks(), 0);

        app.sync.queue.push(QueuedItem {
            label: "Test".into(),
            tracks: vec![],
        });
        assert_eq!(app.sync.queue.len(), 1);

        app.sync.queue.push(QueuedItem {
            label: "Test2".into(),
            tracks: vec![],
        });
        app.sync.queue_selected = 0;
        app.remove_queue_item();
        assert_eq!(app.sync.queue.len(), 1);
        assert_eq!(app.sync.queue[0].label, "Test2");

        app.clear_queue();
        assert!(app.sync.queue.is_empty());
        assert_eq!(app.sync.queue_selected, 0);
    }

    #[test]
    fn remove_queue_item_empty_noop() {
        let mut app = App::new();
        app.remove_queue_item(); // should not panic
        assert!(app.sync.queue.is_empty());
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

    fn make_device_entry(name: &str, size: u64) -> DeviceEntry {
        DeviceEntry {
            object_id: 1000,
            storage_id: 65537,
            format: "MP3".to_string(),
            size,
            name: name.to_string(),
        }
    }

    #[test]
    fn build_device_index_three_segments() {
        let mut app = App::new();
        app.device.tracks = vec![
            make_device_entry("Radiohead/OK Computer/Paranoid Android.mp3", 5_000_000),
            make_device_entry("Radiohead/OK Computer/Karma Police.mp3", 4_000_000),
            make_device_entry("Radiohead/The Bends/Fake Plastic Trees.mp3", 3_000_000),
        ];
        app.build_device_index();

        assert_eq!(app.device.artists, vec!["Radiohead"]);
        assert_eq!(
            app.device.albums.get("Radiohead").unwrap(),
            &vec!["OK Computer".to_string(), "The Bends".to_string()]
        );

        let ok_tracks = app
            .device
            .album_tracks
            .get(&("Radiohead".into(), "OK Computer".into()))
            .unwrap();
        assert_eq!(ok_tracks.len(), 2);
        assert_eq!(ok_tracks[0].name, "Paranoid Android");
        assert_eq!(
            ok_tracks[0].device_path,
            "/Music/Radiohead/OK Computer/Paranoid Android.mp3"
        );
    }

    #[test]
    fn build_device_index_two_segments() {
        let mut app = App::new();
        app.device.tracks = vec![make_device_entry("Artist/track.flac", 1000)];
        app.build_device_index();

        assert_eq!(app.device.artists, vec!["Artist"]);
        let tracks = app
            .device
            .album_tracks
            .get(&("Artist".into(), "Unknown Album".into()))
            .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "track");
    }

    #[test]
    fn build_device_index_single_segment() {
        let mut app = App::new();
        app.device.tracks = vec![make_device_entry("loose_track.mp3", 500)];
        app.build_device_index();

        assert_eq!(app.device.artists, vec!["Unknown Artist"]);
        let tracks = app
            .device
            .album_tracks
            .get(&("Unknown Artist".into(), "Unknown Album".into()))
            .unwrap();
        assert_eq!(tracks[0].name, "loose_track");
    }

    #[test]
    fn build_device_index_deduplicates_albums() {
        let mut app = App::new();
        app.device.tracks = vec![
            make_device_entry("Artist/Album/track1.mp3", 100),
            make_device_entry("Artist/Album/track2.mp3", 200),
            make_device_entry("Artist/Album/track3.mp3", 300),
        ];
        app.build_device_index();

        let albums = app.device.albums.get("Artist").unwrap();
        assert_eq!(albums, &vec!["Album".to_string()]);

        let tracks = app
            .device
            .album_tracks
            .get(&("Artist".into(), "Album".into()))
            .unwrap();
        assert_eq!(tracks.len(), 3);
    }

    #[test]
    fn build_device_index_skips_directories() {
        let mut app = App::new();
        app.device.tracks = vec![
            DeviceEntry {
                object_id: 1,
                storage_id: 65537,
                format: "Association".to_string(),
                size: 0,
                name: "Albums".to_string(),
            },
            make_device_entry("Artist/Album/song.mp3", 1000),
        ];
        app.build_device_index();

        assert_eq!(app.device.artists.len(), 1);
        assert_eq!(app.device.artists[0], "Artist");
    }

    #[test]
    fn build_device_index_empty() {
        let mut app = App::new();
        app.build_device_index();

        assert!(app.device.artists.is_empty());
        assert!(app.device.albums.is_empty());
        assert!(app.device.album_tracks.is_empty());
    }

    #[test]
    fn collect_removal_paths_library_mode_returns_empty() {
        let mut app = App::new();
        app.browse_mode = BrowseMode::Library;
        assert!(app.collect_device_removal_paths().is_empty());
    }

    #[test]
    fn collect_removal_paths_device_mode_track() {
        let mut app = App::new();
        app.device.tracks = vec![make_device_entry("Art/Alb/song.mp3", 1000)];
        app.build_device_index();
        app.browse_mode = BrowseMode::Device;
        app.sidebar_mode = SidebarMode::Artists;
        app.sidebar_items = vec!["Art".into()];
        app.sidebar_selected = 0;
        app.select_sidebar_item();

        // Now move to TrackList panel and select the track.
        app.active_panel = Panel::TrackList;
        app.track_selected = 0;

        let paths = app.collect_device_removal_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].0, "/Music/Art/Alb/song.mp3");
    }

    #[test]
    fn toggle_browse_mode_switches_and_resets() {
        let mut app = App::new();
        assert_eq!(app.browse_mode, BrowseMode::Library);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Device);
        assert_eq!(app.active_panel, Panel::Library);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Library);
    }

    #[test]
    fn clear_device_index_resets_browse_mode() {
        let mut app = App::new();
        app.browse_mode = BrowseMode::Device;
        app.device.artists = vec!["Test".into()];
        app.clear_device_index();

        assert_eq!(app.browse_mode, BrowseMode::Library);
        assert!(app.device.artists.is_empty());
        assert!(app.device.albums.is_empty());
        assert!(app.device.album_tracks.is_empty());
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

    // -- handle_bg_event tests --

    use std::collections::HashMap;

    fn make_minimal_library() -> Box<dyn zytunes::library::MusicLibrary + Send> {
        Box::new(zytunes::library::ItunesLibrary {
            tracks: HashMap::new(),
            playlists: Vec::new(),
            music_folder_path: None,
        })
    }

    #[test]
    fn handle_bg_event_library_loaded_ok() {
        let mut app = App::new();
        let lib = make_minimal_library();
        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(lib)));
        assert!(app.library.is_some());
        assert!(!app.loading_library);
    }

    #[test]
    fn handle_bg_event_library_loaded_err() {
        let mut app = App::new();
        app.loading_library = true;
        app.handle_bg_event(BgEvent::LibraryLoaded(Err("bad path".into())));
        assert!(app.library.is_none());
        assert!(!app.loading_library);
        let (msg, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(is_error);
        assert!(msg.contains("bad path"));
    }

    #[test]
    fn handle_bg_event_session_ready() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connecting;
        let storage = StorageInfo {
            used_bytes: 1000,
            free_bytes: 9000,
            total_bytes: 10000,
            used_percent: 10,
        };
        app.handle_bg_event(BgEvent::SessionReady(Some(storage)));
        assert_eq!(app.device.status, DeviceStatus::Connected);
        assert!(app.device.storage.is_some());
        assert!(app.connection_anim_start.is_none());
    }

    #[test]
    fn handle_bg_event_session_failed() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connecting;
        app.connection_anim_start = Some(42);
        app.handle_bg_event(BgEvent::SessionFailed("timeout".into()));
        assert_eq!(app.device.status, DeviceStatus::Disconnected);
        assert!(app.connection_anim_start.is_none());
        let (msg, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(is_error);
        assert!(msg.contains("timeout"));
    }

    #[test]
    fn handle_bg_event_device_tracks_loaded() {
        let mut app = App::new();
        app.device.loading_tracks = true;
        let tracks = vec![
            make_device_entry("Artist/Album/song1.mp3", 1000),
            make_device_entry("Artist/Album/song2.mp3", 2000),
        ];
        app.handle_bg_event(BgEvent::DeviceTracksLoaded(tracks));
        assert!(!app.device.loading_tracks);
        assert_eq!(app.device.tracks.len(), 2);
        // build_device_index should have been called
        assert!(!app.device.artists.is_empty());
    }

    #[test]
    fn handle_bg_event_sync_progress() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::SyncProgress {
            current: 2,
            total: 5,
            track_name: "song".into(),
        });
        assert_eq!(
            app.sync.status,
            SyncStatus::Running {
                current: 2,
                total: 5
            }
        );
        assert_eq!(app.sync.current_track, "song");
    }

    #[test]
    fn handle_bg_event_sync_complete() {
        let mut app = App::new();
        app.sync.status = SyncStatus::Running {
            current: 3,
            total: 4,
        };
        app.sync.queue.push(QueuedItem {
            label: "test".into(),
            tracks: vec![],
        });
        app.handle_bg_event(BgEvent::SyncComplete {
            success: 3,
            failed: 1,
        });
        assert_eq!(app.sync.status, SyncStatus::Idle);
        assert!(app.sync.queue.is_empty());
        assert_eq!(app.sync.queue_selected, 0);
        // failed > 0 means toast is_error
        let (_, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(is_error);
    }

    #[test]
    fn handle_bg_event_error_sets_toast() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::Error("something went wrong".into()));
        let (msg, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(is_error);
        assert_eq!(msg, "something went wrong");
    }

    #[test]
    fn handle_bg_event_sync_message_appends_to_log() {
        let mut app = App::new();
        assert!(app.sync.log.is_empty());
        app.handle_bg_event(BgEvent::SyncMessage("first msg".into()));
        app.handle_bg_event(BgEvent::SyncMessage("second msg".into()));
        assert_eq!(app.sync.log.len(), 2);
        assert_eq!(app.sync.log[0], "first msg");
        assert_eq!(app.sync.log[1], "second msg");
    }

    // -- Theme picker tests --

    #[test]
    fn theme_picker_move_wraps() {
        let mut app = App::new();
        app.open_theme_picker();
        let count = crate::theme::THEMES.len();
        app.theme_picker_move(-1); // wraps to last
        assert_eq!(app.theme_picker_index, count - 1);
        app.theme_picker_move(1); // wraps back to 0
        assert_eq!(app.theme_picker_index, 0);
    }

    #[test]
    fn theme_picker_cancel_restores() {
        let mut app = App::new();
        app.theme_index = 3;
        app.open_theme_picker();
        app.theme_picker_move(2); // changes preview
        app.theme_picker_cancel();
        assert_eq!(app.theme_index, 3);
        assert!(!app.show_theme_picker);
    }

    // -- Playback tests --

    #[test]
    fn toggle_playback_without_now_playing_is_noop() {
        let mut app = App::new();
        // toggle_playback needs an audio_tx; when now_playing is None and track_list is empty,
        // play_selected_track returns early, so nothing happens.
        let (audio_tx, _audio_rx) = mpsc::channel();
        app.toggle_playback(&audio_tx);
        assert!(app.now_playing.is_none());
    }

    // -- Search filter test --

    #[test]
    fn search_filters_sidebar() {
        let mut app = App::new();
        // Populate sidebar items directly (simulating a loaded library sidebar)
        app.sidebar_items = vec![
            "Beatles".into(),
            "Beach Boys".into(),
            "Radiohead".into(),
            "Rolling Stones".into(),
        ];
        // Activate search with a query
        app.search_active = true;
        app.search_query = "bea".into();
        // refresh_sidebar in Library mode with no library clears items,
        // so instead we test the filtering logic directly on Device mode
        app.browse_mode = BrowseMode::Device;
        app.device.artists = vec![
            "Beatles".into(),
            "Beach Boys".into(),
            "Radiohead".into(),
            "Rolling Stones".into(),
        ];
        app.sidebar_mode = SidebarMode::Artists;
        app.refresh_sidebar();
        assert_eq!(app.sidebar_items.len(), 2);
        assert!(app.sidebar_items.contains(&"Beatles".to_string()));
        assert!(app.sidebar_items.contains(&"Beach Boys".to_string()));
    }

    #[test]
    fn album_year_sort_oldest_first() {
        let mut app = App::new();
        app.album_list = vec![
            AlbumInfo {
                name: "C".into(),
                artist: "X".into(),
                year: None,
                track_count: 1,
            },
            AlbumInfo {
                name: "A".into(),
                artist: "X".into(),
                year: Some(2000),
                track_count: 1,
            },
            AlbumInfo {
                name: "B".into(),
                artist: "X".into(),
                year: Some(1990),
                track_count: 1,
            },
        ];
        // Simulate the sort that select_sidebar_item does.
        app.album_list.sort_by(|a, b| match (a.year, b.year) {
            (Some(ya), Some(yb)) => ya.cmp(&yb).then_with(|| a.name.cmp(&b.name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });
        assert_eq!(app.album_list[0].name, "B"); // 1990
        assert_eq!(app.album_list[1].name, "A"); // 2000
        assert_eq!(app.album_list[2].name, "C"); // None (last)
    }

    #[test]
    fn skip_forward_albums_by_year() {
        let mut app = App::new();
        app.active_panel = Panel::Albums;
        app.album_list = vec![
            AlbumInfo {
                name: "A".into(),
                artist: "X".into(),
                year: Some(1990),
                track_count: 1,
            },
            AlbumInfo {
                name: "B".into(),
                artist: "X".into(),
                year: Some(1990),
                track_count: 1,
            },
            AlbumInfo {
                name: "C".into(),
                artist: "X".into(),
                year: Some(2000),
                track_count: 1,
            },
            AlbumInfo {
                name: "D".into(),
                artist: "X".into(),
                year: None,
                track_count: 1,
            },
        ];
        app.album_selected = 0;
        app.skip_forward(); // 1990 -> 2000
        assert_eq!(app.album_selected, 2);
        app.skip_forward(); // 2000 -> None
        assert_eq!(app.album_selected, 3);
        app.skip_forward(); // None -> wrap to 0
        assert_eq!(app.album_selected, 0);
    }

    #[test]
    fn skip_back_albums_by_year() {
        let mut app = App::new();
        app.active_panel = Panel::Albums;
        app.album_list = vec![
            AlbumInfo {
                name: "A".into(),
                artist: "X".into(),
                year: Some(1990),
                track_count: 1,
            },
            AlbumInfo {
                name: "B".into(),
                artist: "X".into(),
                year: Some(1990),
                track_count: 1,
            },
            AlbumInfo {
                name: "C".into(),
                artist: "X".into(),
                year: Some(2000),
                track_count: 1,
            },
            AlbumInfo {
                name: "D".into(),
                artist: "X".into(),
                year: None,
                track_count: 1,
            },
        ];
        app.album_selected = 3;
        app.skip_back(); // None -> start of 2000
        assert_eq!(app.album_selected, 2);
        app.skip_back(); // 2000 -> start of 1990
        assert_eq!(app.album_selected, 0);
        app.skip_back(); // 1990 at start -> wrap to end
        assert_eq!(app.album_selected, 3);
    }

    #[test]
    fn new_app_show_keys_defaults_false() {
        let app = App::new();
        assert!(!app.show_keys);
    }
}
