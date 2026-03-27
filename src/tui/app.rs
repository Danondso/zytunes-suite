use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Instant;

use throbber_widgets_tui::ThrobberState;
use zytunes::library::{ItunesLibrary, Track};
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
    #[allow(dead_code)]
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
    Running {
        current: usize,
        total: usize,
    },
    Complete {
        success: usize,
        failed: usize,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackState {
    #[allow(dead_code)]
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

pub struct App {
    pub active_panel: Panel,
    pub library: Option<ItunesLibrary>,
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
    pub browse_mode: BrowseMode,
    pub device_artists: Vec<String>,
    pub device_albums: BTreeMap<String, Vec<String>>,
    pub device_album_tracks: BTreeMap<(String, String), Vec<DeviceTrackInfo>>,
    pub sync_queue: Vec<QueuedItem>,
    pub queue_selected: usize,
    pub sync_status: SyncStatus,
    pub sync_current_track: String,
    pub sync_log: Vec<String>,
    pub throbber_state: ThrobberState,
    pub anim_frame: usize,
    pub connection_anim_start: Option<usize>,
    pub now_playing: Option<NowPlaying>,
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
            saved_sidebar_pos: [[0; 3]; 2],
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
            browse_mode: BrowseMode::Library,
            device_artists: Vec::new(),
            device_albums: BTreeMap::new(),
            device_album_tracks: BTreeMap::new(),
            sync_queue: Vec::new(),
            queue_selected: 0,
            sync_status: SyncStatus::Idle,
            sync_current_track: String::new(),
            sync_log: Vec::new(),
            throbber_state: ThrobberState::default(),
            anim_frame: 0,
            connection_anim_start: None,
            now_playing: None,
            should_quit: false,
            show_help: false,
            show_keys: true,
            search_active: false,
            search_query: String::new(),
            toast_message: None,
            library_path: None,
            loading_library: false,
            theme_index: 0,
            show_theme_picker: false,
            theme_picker_index: 0,
            theme_before_picker: 0,
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
        self.theme_picker_index = (self.theme_picker_index as isize + delta).rem_euclid(len as isize) as usize;
        self.theme_index = self.theme_picker_index;
    }

    pub fn theme_picker_confirm(&mut self) {
        self.show_theme_picker = false;
        // Save to config.
        let config = crate::config::Config {
            theme: Some(self.theme().name.to_string()),
        };
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

    pub fn handle_audio_event(
        &mut self,
        event: AudioEvent,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) {
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

    pub fn build_device_index(&mut self) {
        self.device_artists.clear();
        self.device_albums.clear();
        self.device_album_tracks.clear();

        let mut artist_set = std::collections::BTreeSet::new();

        for entry in &self.device_tracks {
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

            self.device_albums
                .entry(artist.clone())
                .or_default()
                .push(album.clone());

            let key = (artist.clone(), album.clone());
            self.device_album_tracks
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

        self.device_artists = artist_set.into_iter().collect();

        // Deduplicate album lists per artist.
        for albums in self.device_albums.values_mut() {
            albums.sort();
            albums.dedup();
        }
    }

    pub fn clear_device_index(&mut self) {
        self.device_artists.clear();
        self.device_albums.clear();
        self.device_album_tracks.clear();
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
    pub fn collect_device_removal_paths(&self) -> Vec<String> {
        if self.browse_mode != BrowseMode::Device {
            return Vec::new();
        }
        match self.active_panel {
            Panel::TrackList => {
                if let Some(track) = self.track_list.get(self.track_selected) {
                    // Find the DeviceTrackInfo with matching name in current context.
                    if let Some(item) = self.sidebar_items.get(self.sidebar_selected) {
                        let (artist, album) = self.resolve_device_artist_album(item);
                        if let Some(tracks) = self.device_album_tracks.get(&(artist, album)) {
                            if let Some(dt) = tracks.iter().find(|dt| dt.name == track.name) {
                                return vec![dt.device_path.clone()];
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
                    if let Some(tracks) = self.device_album_tracks.get(&key) {
                        return tracks.iter().map(|t| t.device_path.clone()).collect();
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

    fn collect_sidebar_removal_paths(&self, item: &str) -> Vec<String> {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                let Some(albums) = self.device_albums.get(item) else {
                    return Vec::new();
                };
                let mut paths = Vec::new();
                for album in albums {
                    let key = (item.to_string(), album.clone());
                    if let Some(tracks) = self.device_album_tracks.get(&key) {
                        paths.extend(tracks.iter().map(|t| t.device_path.clone()));
                    }
                }
                paths
            }
            SidebarMode::Albums => {
                let (artist, album) = self.resolve_device_artist_album(item);
                self.device_album_tracks
                    .get(&(artist, album))
                    .map(|tracks| tracks.iter().map(|t| t.device_path.clone()).collect())
                    .unwrap_or_default()
            }
            SidebarMode::Playlists => Vec::new(),
        }
    }

    fn resolve_device_artist_album(&self, item: &str) -> (String, String) {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                let album = self.album_list.get(self.album_selected)
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
                    SidebarMode::Artists => self.device_artists.clone(),
                    SidebarMode::Albums => {
                        let mut items = Vec::new();
                        for (artist, albums) in &self.device_albums {
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
                self.album_selected = 0;
                self.select_album();
            }
            SidebarMode::Albums => {
                self.album_list.clear();
                self.track_list =
                    if let Some((_, album)) = item.split_once(" \u{2014} ") {
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

    fn select_sidebar_item_device(&mut self, item: &str) {
        match self.sidebar_mode {
            SidebarMode::Artists => {
                if let Some(albums) = self.device_albums.get(item) {
                    self.album_list = albums
                        .iter()
                        .map(|album_name| {
                            let count = self
                                .device_album_tracks
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
                    self.track_list = match self.device_album_tracks.get(&key) {
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
            self.track_list = match self.device_album_tracks.get(&key) {
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
        if self.sync_queue.is_empty() {
            return;
        }
        if self.device_status != DeviceStatus::Connected {
            self.set_toast("Connect a device before syncing".into(), true);
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
                self.connection_anim_start = None;
                self.device_storage = storage;
                self.set_toast("Device connected".into(), false);
            }
            BgEvent::SessionFailed(e) => {
                self.device_status = DeviceStatus::Disconnected;
                self.connection_anim_start = None;
                self.set_toast(format!("Connection failed: {}", e), true);
            }
            BgEvent::LoadingDeviceTracks => {
                self.device_loading_tracks = true;
                self.set_toast("Loading device tracks...".into(), false);
            }
            BgEvent::DeviceTracksLoaded(tracks) => {
                self.device_loading_tracks = false;
                self.device_tracks = tracks;
                self.build_device_index();
                self.set_toast(
                    format!("Loaded {} device tracks", self.device_tracks.len()),
                    false,
                );
                if self.browse_mode == BrowseMode::Device {
                    self.refresh_sidebar();
                }
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
            } => {
                self.sync_status = SyncStatus::Complete {
                    success,
                    failed,
                };
                self.sync_queue.clear();
                self.queue_selected = 0;
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
                self.sync_status = SyncStatus::Running { current, total };
                self.sync_current_track = format!("Removing: {}", name);
            }
            BgEvent::RemoveComplete { success, failed } => {
                self.sync_status = SyncStatus::Idle;
                self.set_toast(
                    format!("Removed {} tracks, {} failed", success, failed),
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
        self.anim_frame = self.anim_frame.wrapping_add(1);

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
                    self.save_sidebar_pos();
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
                    self.save_sidebar_pos();
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
                self.save_sidebar_pos();
                return;
            }
        }
        // Wrap to top if at the end.
        self.sidebar_selected = 0;
        self.save_sidebar_pos();
    }

    /// Jump backward to the previous letter group in the sidebar.
    pub fn skip_back(&mut self) {
        if self.active_panel != Panel::Library || self.sidebar_items.is_empty() {
            return;
        }
        if self.sidebar_selected == 0 {
            self.sidebar_selected = self.sidebar_items.len() - 1;
            self.save_sidebar_pos();
            return;
        }
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
        self.save_sidebar_pos();
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
        app.device_tracks = vec![
            make_device_entry("Radiohead/OK Computer/Paranoid Android.mp3", 5_000_000),
            make_device_entry("Radiohead/OK Computer/Karma Police.mp3", 4_000_000),
            make_device_entry("Radiohead/The Bends/Fake Plastic Trees.mp3", 3_000_000),
        ];
        app.build_device_index();

        assert_eq!(app.device_artists, vec!["Radiohead"]);
        assert_eq!(
            app.device_albums.get("Radiohead").unwrap(),
            &vec!["OK Computer".to_string(), "The Bends".to_string()]
        );

        let ok_tracks = app
            .device_album_tracks
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
        app.device_tracks = vec![make_device_entry("Artist/track.flac", 1000)];
        app.build_device_index();

        assert_eq!(app.device_artists, vec!["Artist"]);
        let tracks = app
            .device_album_tracks
            .get(&("Artist".into(), "Unknown Album".into()))
            .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "track");
    }

    #[test]
    fn build_device_index_single_segment() {
        let mut app = App::new();
        app.device_tracks = vec![make_device_entry("loose_track.mp3", 500)];
        app.build_device_index();

        assert_eq!(app.device_artists, vec!["Unknown Artist"]);
        let tracks = app
            .device_album_tracks
            .get(&("Unknown Artist".into(), "Unknown Album".into()))
            .unwrap();
        assert_eq!(tracks[0].name, "loose_track");
    }

    #[test]
    fn build_device_index_deduplicates_albums() {
        let mut app = App::new();
        app.device_tracks = vec![
            make_device_entry("Artist/Album/track1.mp3", 100),
            make_device_entry("Artist/Album/track2.mp3", 200),
            make_device_entry("Artist/Album/track3.mp3", 300),
        ];
        app.build_device_index();

        let albums = app.device_albums.get("Artist").unwrap();
        assert_eq!(albums, &vec!["Album".to_string()]);

        let tracks = app
            .device_album_tracks
            .get(&("Artist".into(), "Album".into()))
            .unwrap();
        assert_eq!(tracks.len(), 3);
    }

    #[test]
    fn build_device_index_skips_directories() {
        let mut app = App::new();
        app.device_tracks = vec![
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

        assert_eq!(app.device_artists.len(), 1);
        assert_eq!(app.device_artists[0], "Artist");
    }

    #[test]
    fn build_device_index_empty() {
        let mut app = App::new();
        app.build_device_index();

        assert!(app.device_artists.is_empty());
        assert!(app.device_albums.is_empty());
        assert!(app.device_album_tracks.is_empty());
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
        app.device_tracks = vec![
            make_device_entry("Art/Alb/song.mp3", 1000),
        ];
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
        assert_eq!(paths, vec!["/Music/Art/Alb/song.mp3"]);
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
        app.device_artists = vec!["Test".into()];
        app.clear_device_index();

        assert_eq!(app.browse_mode, BrowseMode::Library);
        assert!(app.device_artists.is_empty());
        assert!(app.device_albums.is_empty());
        assert!(app.device_album_tracks.is_empty());
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
