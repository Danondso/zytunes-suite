use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use image::DynamicImage;
use throbber_widgets_tui::ThrobberState;
use zytunes::dirlib::TrackSample;
use zytunes::library::{MusicLibrary, Track};
use zytunes::mtp::parse::DeviceEntry;

use crate::audio::{AudioCommand, AudioEvent};
use crate::background::{BgCommand, BgEvent, StorageInfo, SyncItem};
use crate::theme::{all_themes, Theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Library,
    Albums,
    TrackList,
    Device,
    SyncQueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarMode {
    Artists,
    Albums,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowseMode {
    Library,
    Device,
}

/// A single sidebar row, either a bare artist entry or a paired
/// (artist, album) entry. Storing the parts structured avoids the previous
/// `format!("{} — {}")` / `split_once(" — ")` round-trip that every lookup
/// had to unpack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEntry {
    Artist(String),
    Album { artist: String, album: String },
}

impl SidebarEntry {
    /// Human-readable label for rendering (also used as the queue-item label).
    pub fn display(&self) -> std::borrow::Cow<'_, str> {
        match self {
            SidebarEntry::Artist(a) => std::borrow::Cow::Borrowed(a.as_str()),
            SidebarEntry::Album { artist, album } => {
                std::borrow::Cow::Owned(format!("{} \u{2014} {}", artist, album))
            }
        }
    }

    /// Key used for first-letter jump navigation. Both variants key off the
    /// artist name so navigation is uniform across sidebar modes.
    pub fn nav_key(&self) -> &str {
        match self {
            SidebarEntry::Artist(a) => a.as_str(),
            SidebarEntry::Album { artist, .. } => artist.as_str(),
        }
    }

    /// Pre-lowercased search key cached alongside each sidebar row. Album
    /// entries join artist and album with `\n` so substring matches hit
    /// either side but can't span the separator. Built once by
    /// `rebuild_sidebar_source` so `/` keystrokes never re-lowercase.
    pub fn lowercase_key(&self) -> String {
        match self {
            SidebarEntry::Artist(a) => a.to_lowercase(),
            SidebarEntry::Album { artist, album } => {
                let mut out = artist.to_lowercase();
                out.push('\n');
                out.push_str(&album.to_lowercase());
                out
            }
        }
    }
}

impl std::fmt::Display for SidebarEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidebarEntry::Artist(a) => f.write_str(a),
            SidebarEntry::Album { artist, album } => {
                write!(f, "{} \u{2014} {}", artist, album)
            }
        }
    }
}

/// Which field of a track sample a loading phrase refers to.
#[derive(Copy, Clone)]
enum ScanField {
    Artist,
    Album,
    Track,
}

struct ScanPhrase {
    prefix: &'static str,
    field: ScanField,
}

/// Fun loading-phrase prefixes, cycled while the library is scanning.
const SCAN_PHRASES: &[ScanPhrase] = &[
    ScanPhrase {
        prefix: "Scoping",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Scanning",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Creepin' on",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Puttin' a spell on",
        field: ScanField::Track,
    },
    ScanPhrase {
        prefix: "Vibing with",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Peeking at",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Digging through",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Unpacking",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Snooping on",
        field: ScanField::Track,
    },
    ScanPhrase {
        prefix: "Cataloging",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Tipping hat to",
        field: ScanField::Track,
    },
];

/// Minimum time a scan phrase stays on screen before rotating (milliseconds).
const SCAN_PHRASE_MS: u128 = 900;

/// Cheap entropy source for picking phrases and samples; quality doesn't matter.
fn quick_random() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceTrackInfo {
    pub name: String,
    pub device_path: String,
    pub object_id: u64,
    pub artist: String,
    pub album: String,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceStatus {
    Disconnected,
    Detecting,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DevicePresence {
    None,
    Partial,
    Full,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumArtStyle {
    Halfblock,
    Ascii,
}

impl AlbumArtStyle {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlbumArtStyle::Halfblock => "halfblock",
            AlbumArtStyle::Ascii => "ascii",
        }
    }
}

impl std::str::FromStr for AlbumArtStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "halfblock" => Ok(AlbumArtStyle::Halfblock),
            "ascii" => Ok(AlbumArtStyle::Ascii),
            _ => Err(()),
        }
    }
}

/// Character ramp for ASCII art, ordered sparse → dense (10 chars).
/// High-luminance pixels map to denser glyphs (more ink coverage).
pub(crate) const ASCII_ART_RAMP: &[u8; 10] = b" .:-=+*%#@";

/// Rendered album-art buffer. The variant tracks which renderer produced the
/// buffer, so the cache can never drift out of sync with the active style.
#[allow(clippy::type_complexity)]
#[derive(Clone, Debug)]
pub enum AlbumArtCache {
    /// Halfblock cells: one terminal cell packs two vertical pixels as ▀ with
    /// fg = top pixel, bg = bottom pixel.
    Halfblock(Vec<Vec<(char, [u8; 3], [u8; 3])>>),
    /// ASCII cells: one pixel per cell, character picked from the luminance
    /// ramp, fg = pixel color.
    Ascii(Vec<Vec<(char, [u8; 3])>>),
}

impl AlbumArtCache {
    pub fn rows(&self) -> usize {
        match self {
            AlbumArtCache::Halfblock(v) => v.len(),
            AlbumArtCache::Ascii(v) => v.len(),
        }
    }

    pub fn width(&self) -> usize {
        match self {
            AlbumArtCache::Halfblock(v) => v.first().map(|r| r.len()).unwrap_or(0),
            AlbumArtCache::Ascii(v) => v.first().map(|r| r.len()).unwrap_or(0),
        }
    }

    pub fn matches_style(&self, style: AlbumArtStyle) -> bool {
        matches!(
            (self, style),
            (AlbumArtCache::Halfblock(_), AlbumArtStyle::Halfblock)
                | (AlbumArtCache::Ascii(_), AlbumArtStyle::Ascii)
        )
    }
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
    /// Shared immutable snapshot of the playlist at the moment playback
    /// started. `Arc` so skip/prev/next don't clone the whole track list on
    /// every hop.
    pub playlist: Arc<[TrackInfo]>,
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
    pub family: Option<zytunes::device::DeviceFamily>,
    pub storage: Option<StorageInfo>,
    pub tracks: Vec<DeviceEntry>,
    pub loading_tracks: bool,
    pub selected: usize,
    pub artists: Vec<String>,
    pub albums: BTreeMap<String, Vec<String>>,
    pub album_tracks: BTreeMap<(String, String), Vec<DeviceTrackInfo>>,
    /// Precomputed set of (normalized_artist, normalized_name) for on-device matching.
    pub track_set: HashSet<(String, String)>,
    /// Per-artist list of normalized device track names for substring fallback.
    pub artist_track_names: BTreeMap<String, Vec<String>>,
    /// Number of items the device acquired on its own (podcasts, Zune-to-Zune shares).
    pub acquired_items: u32,
    /// Sync progress status string from MTP vendor op 0x922f.
    pub sync_status: Option<String>,
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
            family: None,
            storage: None,
            tracks: Vec::new(),
            loading_tracks: false,
            selected: 0,
            artists: Vec::new(),
            albums: BTreeMap::new(),
            album_tracks: BTreeMap::new(),
            track_set: HashSet::new(),
            artist_track_names: BTreeMap::new(),
            acquired_items: 0,
            sync_status: None,
        }
    }

    /// Incrementally add a single device track to all index structures.
    /// Used during sync deltas to avoid O(N²) full rebuilds.
    pub fn add_indexed_track(&mut self, entry: &DeviceEntry) {
        if entry.is_dir() {
            return;
        }
        let (artist, album, display_name) = parse_device_track_parts(&entry.name);
        let device_path = format!("/Music/{}", entry.name);

        if let Some(existing) = self.album_tracks.get(&(artist.clone(), album.clone())) {
            if existing.iter().any(|t| t.device_path == device_path) {
                return;
            }
        }

        if let Err(pos) = self.artists.binary_search(&artist) {
            self.artists.insert(pos, artist.clone());
        }

        let albums = self.albums.entry(artist.clone()).or_default();
        if let Err(pos) = albums.binary_search(&album) {
            albums.insert(pos, album.clone());
        }

        self.album_tracks
            .entry((artist.clone(), album.clone()))
            .or_default()
            .push(DeviceTrackInfo {
                name: display_name.clone(),
                device_path,
                object_id: entry.object_id,
                artist: artist.clone(),
                album,
                track_number: entry.track_number,
                disc_number: entry.disc_number,
            });

        let artist_key = normalize_for_match(&artist);
        let raw = normalize_for_match(&display_name);
        let stripped = normalize_for_match(zytunes::strip_track_number(display_name.trim()));
        self.track_set.insert((artist_key.clone(), raw.clone()));
        if stripped != raw {
            self.track_set.insert((artist_key.clone(), stripped));
        }
        self.artist_track_names
            .entry(artist_key)
            .or_default()
            .push(raw);
    }

    /// Incrementally remove a single device track identified by its relative name
    /// (e.g. `"Artist/Album/track.mp3"`). Returns true if a track was found and removed.
    pub fn remove_indexed_track(&mut self, relative_name: &str) -> bool {
        let (artist, album, _display_name) = parse_device_track_parts(relative_name);
        let device_path = format!("/Music/{}", relative_name);
        let key = (artist.clone(), album.clone());

        let Some(tracks) = self.album_tracks.get_mut(&key) else {
            return false;
        };
        let before = tracks.len();
        tracks.retain(|t| t.device_path != device_path);
        if tracks.len() == before {
            return false;
        }
        let album_now_empty = tracks.is_empty();

        if album_now_empty {
            self.album_tracks.remove(&key);
            if let Some(albums) = self.albums.get_mut(&artist) {
                albums.retain(|a| a != &album);
                if albums.is_empty() {
                    self.albums.remove(&artist);
                    if let Ok(idx) = self.artists.binary_search(&artist) {
                        self.artists.remove(idx);
                    }
                }
            }
        }

        self.rebuild_lookup_for_artist(&artist);
        true
    }

    /// Check if a track identified by pre-normalized `(artist_key, name_key)`
    /// is on the device. Callers that already hold normalized keys (e.g.
    /// cached on `TrackInfo`) should use this directly to avoid re-allocating
    /// in hot paths.
    pub fn contains_track(&self, artist_key: &str, name_key: &str) -> bool {
        if self.status != DeviceStatus::Connected || self.track_set.is_empty() {
            return false;
        }
        // Fast path: direct (artist, name) hit in the precomputed set.
        if self
            .track_set
            .contains(&(artist_key.to_string(), name_key.to_string()))
        {
            return true;
        }
        // Fallback: bidirectional substring check against the artist's device
        // tracks (handles filenames with added/stripped track numbers or
        // incidental prefixes that the normalizer alone can't reconcile).
        if let Some(device_names) = self.artist_track_names.get(artist_key) {
            return device_names
                .iter()
                .any(|dn| dn.contains(name_key) || name_key.contains(dn.as_str()));
        }
        false
    }

    /// Rebuild `track_set` and `artist_track_names` entries for a single artist.
    /// Cheap because it only walks that artist's tracks, not the whole device.
    fn rebuild_lookup_for_artist(&mut self, artist: &str) {
        let artist_key = normalize_for_match(artist);
        self.track_set.retain(|(a, _)| a != &artist_key);
        self.artist_track_names.remove(&artist_key);

        let mut names = Vec::new();
        let mut extra_keys = Vec::new();
        for ((a, _), tracks) in &self.album_tracks {
            if normalize_for_match(a) != artist_key {
                continue;
            }
            for dt in tracks {
                let raw = normalize_for_match(&dt.name);
                let stripped = normalize_for_match(zytunes::strip_track_number(dt.name.trim()));
                if stripped != raw {
                    extra_keys.push(stripped);
                }
                names.push(raw);
            }
        }
        for raw in &names {
            self.track_set.insert((artist_key.clone(), raw.clone()));
        }
        for stripped in extra_keys {
            self.track_set.insert((artist_key.clone(), stripped));
        }
        if !names.is_empty() {
            self.artist_track_names.insert(artist_key, names);
        }
    }
}

/// Parse a device-relative track name (e.g. `"Artist/Album/01 track.mp3"`) into
/// (artist, album, display_name). Missing segments fall back to `Unknown Artist`
/// and `Unknown Album`. The display name has its file extension stripped.
fn parse_device_track_parts(name: &str) -> (String, String, String) {
    let parts: Vec<&str> = name.splitn(3, '/').collect();
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
            name.to_string(),
        ),
    };
    let display_name = filename
        .rfind('.')
        .map(|pos| &filename[..pos])
        .unwrap_or(&filename)
        .to_string();
    (artist, album, display_name)
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
    /// Filtered view the UI reads. Derived from `sidebar_items_full` by
    /// applying the `/` search filter (when active).
    pub sidebar_items: Vec<SidebarEntry>,
    /// Unfiltered source list rebuilt only when the underlying library,
    /// device contents, or sidebar mode change. Search keystrokes filter
    /// this into `sidebar_items` without re-allocating per row.
    sidebar_items_full: Vec<SidebarEntry>,
    /// Pre-lowercased form of each `sidebar_items_full` entry, parallel-
    /// indexed. For `Album` entries this joins artist and album with `\n`
    /// so queries match either side without spanning the boundary.
    sidebar_lowercase_full: Vec<String>,
    pub sidebar_selected: usize,
    pub sidebar_scroll: usize,
    /// Per-mode saved selection positions: [Artists, Albums] x [Library, Device]
    saved_sidebar_pos: HashMap<(BrowseMode, SidebarMode), usize>,
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
    pub loading_library: bool,
    /// (completed, total) track counts emitted by the library scanner.
    pub scan_progress: Option<(u64, u64)>,
    /// The current loading-screen message like "Scoping OK Computer".
    pub scan_phrase: Option<String>,
    /// When `scan_phrase` was last rotated.
    scan_phrase_rotated_at: Option<Instant>,
    /// Rolling buffer of recent track samples used to pick phrase targets.
    scan_samples: Vec<TrackSample>,
    pub theme: &'static Theme,
    pub show_theme_picker: bool,
    pub theme_picker_index: usize,
    pub theme_before_picker: &'static Theme,
    /// Per-artist device presence for sidebar indicators (Library browse mode).
    pub artist_device_status: BTreeMap<String, DevicePresence>,
    /// Per-(artist, album) device presence for album-list indicators.
    pub album_device_status: BTreeMap<(String, String), DevicePresence>,
    /// Cached album art extracted from ID3 tags.
    pub album_art: Option<DynamicImage>,
    /// Key used to avoid re-extracting art (e.g. "artist/album").
    album_art_key: String,
    /// Rendered album-art buffer for the active style, or `None` if no art is
    /// cached (either never built or invalidated by a style/size/key change).
    pub album_art_cache: Option<AlbumArtCache>,
    /// Dimensions (w, h) the cached album art was rendered for.
    album_art_size: (u16, u16),
    /// Renderer style for album art: halfblock (Unicode half-block) or ascii (character ramp).
    pub album_art_style: AlbumArtStyle,
    /// User preference for the now-playing panel. `None` means "auto" (show
    /// whenever there's a track and the terminal is tall enough); `Some(false)`
    /// force-hides the panel regardless. Persisted to `config.toml`.
    pub show_player: Option<bool>,
    /// Background commands to send after event handling (main loop flushes these).
    pub pending_bg_commands: Vec<BgCommand>,
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
    pub disc_number: Option<u32>,
    pub genre: Option<String>,
    pub on_device: bool,
    /// Pre-normalized (lowercased, trimmed, edge-stripped) artist key.
    /// Computed once at construction so hot paths (`retag_on_device`, per-frame
    /// filter passes) do not re-allocate on every render.
    pub artist_key: String,
    /// Pre-normalized track-name key, paired with `artist_key` for device lookup.
    pub name_key: String,
}

impl TrackInfo {
    /// Construct a `TrackInfo`, precomputing match keys so on-device lookups
    /// avoid re-normalizing per render.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        artist: String,
        album: String,
        duration_ms: Option<u64>,
        kind: Option<String>,
        location: Option<String>,
        track_number: Option<u32>,
        disc_number: Option<u32>,
        genre: Option<String>,
        on_device: bool,
    ) -> Self {
        let artist_key = normalize_for_match(&artist);
        let name_key = normalize_for_match(&name);
        TrackInfo {
            name,
            artist,
            album,
            duration_ms,
            kind,
            location,
            track_number,
            disc_number,
            genre,
            on_device,
            artist_key,
            name_key,
        }
    }
}

impl App {
    pub fn new() -> Self {
        App {
            active_panel: Panel::Library,
            library: None,
            sidebar_mode: SidebarMode::Artists,
            sidebar_items: Vec::new(),
            sidebar_items_full: Vec::new(),
            sidebar_lowercase_full: Vec::new(),
            sidebar_selected: 0,
            sidebar_scroll: 0,
            saved_sidebar_pos: HashMap::new(),
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
            loading_library: false,
            scan_progress: None,
            scan_phrase: None,
            scan_phrase_rotated_at: None,
            scan_samples: Vec::new(),
            theme: all_themes()[0],
            show_theme_picker: false,
            theme_picker_index: 0,
            theme_before_picker: all_themes()[0],
            artist_device_status: BTreeMap::new(),
            album_device_status: BTreeMap::new(),
            album_art: None,
            album_art_key: String::new(),
            album_art_cache: None,
            album_art_size: (0, 0),
            album_art_style: {
                let cfg = crate::config::load();
                cfg.album_art_style
                    .as_deref()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(AlbumArtStyle::Halfblock)
            },
            show_player: crate::config::load().show_player,
            pending_bg_commands: Vec::new(),
        }
    }

    /// Cycle through the three player-panel preferences:
    /// `None` (auto) → `Some(false)` (force hide) → `Some(true)` (force show) → `None`.
    /// Returns a user-facing label describing the new state. Persists the
    /// choice to `config.toml`.
    pub fn cycle_show_player(&mut self) -> &'static str {
        let label = self.cycle_show_player_in_memory();
        let pref = self.show_player;
        crate::config::update(|c| c.show_player = pref);
        label
    }

    /// In-memory half of [`cycle_show_player`]: advances the preference and
    /// returns the label, without touching the on-disk config. Split out for
    /// unit tests.
    fn cycle_show_player_in_memory(&mut self) -> &'static str {
        let (next, label) = match self.show_player {
            None => (Some(false), "Player: hidden"),
            Some(false) => (Some(true), "Player: always on"),
            Some(true) => (None, "Player: auto"),
        };
        self.show_player = next;
        label
    }

    /// Whether the now-playing panel should render at `height`, given the
    /// user's preference and current playback state. This is the single
    /// source of truth consumed by both `LayoutMetrics` and the pre-render
    /// album-art pass.
    pub fn should_show_player(&self, height: u16) -> bool {
        if self.now_playing.is_none() {
            return false;
        }
        match self.show_player {
            Some(true) => true,
            Some(false) => false,
            None => height >= 20,
        }
    }

    /// Toggle between halfblock and ASCII album art renderers, invalidating caches
    /// so the next render rebuilds at the current panel size. Persists choice to config.
    pub fn toggle_album_art_style(&mut self) {
        self.flip_art_style_in_memory();
        let style = self.album_art_style.as_str().to_string();
        crate::config::update(|c| c.album_art_style = Some(style));
    }

    /// In-memory half of [`toggle_album_art_style`]: flips the style and
    /// invalidates the cached art for the old style. Split out so unit tests
    /// can exercise the state transition without touching the on-disk config.
    fn flip_art_style_in_memory(&mut self) {
        self.album_art_style = match self.album_art_style {
            AlbumArtStyle::Halfblock => AlbumArtStyle::Ascii,
            AlbumArtStyle::Ascii => AlbumArtStyle::Halfblock,
        };
        self.album_art_cache = None;
        self.album_art_size = (0, 0);
    }

    pub fn theme(&self) -> &'static Theme {
        self.theme
    }

    pub fn open_theme_picker(&mut self) {
        self.theme_before_picker = self.theme;
        self.theme_picker_index = crate::theme::theme_position(self.theme);
        self.show_theme_picker = true;
    }

    pub fn theme_picker_move(&mut self, delta: isize) {
        let themes = all_themes();
        let len = themes.len();
        self.theme_picker_index =
            (self.theme_picker_index as isize + delta).rem_euclid(len as isize) as usize;
        self.theme = themes[self.theme_picker_index];
    }

    pub fn theme_picker_confirm(&mut self) {
        self.show_theme_picker = false;
        let theme_name = self.theme.name.to_string();
        crate::config::update(|c| c.theme = Some(theme_name));
    }

    pub fn theme_picker_cancel(&mut self) {
        self.theme = self.theme_before_picker;
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
            playlist: Arc::from(self.track_list.as_slice()),
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
            let playlist = Arc::clone(&np.playlist);
            if next_idx < playlist.len() {
                self.play_from_playlist(next_idx, playlist, audio_tx);
            } else {
                // End of playlist
                let _ = audio_tx.send(AudioCommand::Stop);
                self.now_playing = None;
            }
        }
    }

    pub fn prev_track(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        if let Some(ref np) = self.now_playing {
            let playlist = Arc::clone(&np.playlist);
            let index = if np.elapsed_ms > 3000 || np.track_index == 0 {
                // Restart current track
                np.track_index
            } else {
                np.track_index - 1
            };
            self.play_from_playlist(index, playlist, audio_tx);
        }
    }

    pub fn stop_playback(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        let _ = audio_tx.send(AudioCommand::Stop);
        self.now_playing = None;
    }

    fn play_from_playlist(
        &mut self,
        index: usize,
        playlist: Arc<[TrackInfo]>,
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
            playlist,
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

    /// Re-run UI-level derivations after incremental index changes.
    pub fn flush_device_index(&mut self) {
        if self.device_index_dirty {
            self.device_index_dirty = false;
            self.rebuild_artist_device_status();
            if self.browse_mode == BrowseMode::Device {
                self.refresh_sidebar();
            } else {
                self.retag_on_device();
            }
        }
    }

    /// Full rebuild of the device index from `device.tracks`. Used for initial
    /// load; individual add/remove events now update the index incrementally.
    pub fn build_device_index(&mut self) {
        self.device.artists.clear();
        self.device.albums.clear();
        self.device.album_tracks.clear();
        self.device.track_set.clear();
        self.device.artist_track_names.clear();

        let tracks = std::mem::take(&mut self.device.tracks);
        for entry in &tracks {
            self.device.add_indexed_track(entry);
        }
        self.device.tracks = tracks;
    }

    /// Re-tag the currently displayed track list using precomputed device sets.
    /// Uses the `TrackInfo`-cached `(artist_key, name_key)` so this runs with
    /// zero allocations even during live sync cascades.
    fn retag_on_device(&mut self) {
        for t in &mut self.track_list {
            t.on_device = self.device.contains_track(&t.artist_key, &t.name_key);
        }
    }

    /// Compute per-artist and per-album device presence (None/Partial/Full)
    /// by checking every track in the library against the device.
    pub fn rebuild_artist_device_status(&mut self) {
        self.artist_device_status.clear();
        self.album_device_status.clear();
        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };
        if self.device.status != DeviceStatus::Connected || self.device.track_set.is_empty() {
            return;
        }
        let mut artist_counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let mut album_counts: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
        for t in lib.all_tracks() {
            let artist_entry = artist_counts.entry(t.artist.clone()).or_default();
            artist_entry.1 += 1;
            let album_entry = album_counts
                .entry((t.artist.clone(), t.album.clone()))
                .or_default();
            album_entry.1 += 1;
            if is_on_device(&t.artist, &t.name, &self.device) {
                artist_entry.0 += 1;
                album_entry.0 += 1;
            }
        }
        for (artist, (on_device, total)) in artist_counts {
            let status = presence_from_counts(on_device, total);
            if status != DevicePresence::None {
                self.artist_device_status.insert(artist, status);
            }
        }
        for (key, (on_device, total)) in album_counts {
            let status = presence_from_counts(on_device, total);
            if status != DevicePresence::None {
                self.album_device_status.insert(key, status);
            }
        }
    }

    pub fn clear_device_index(&mut self) {
        self.device.artists.clear();
        self.device.albums.clear();
        self.device.album_tracks.clear();
        self.device.track_set.clear();
        self.device.artist_track_names.clear();
        self.device.acquired_items = 0;
        self.artist_device_status.clear();
        self.album_device_status.clear();
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
                    if let Some(entry) = self.sidebar_items.get(self.sidebar_selected) {
                        let (artist, album) = self.resolve_device_artist_album(entry);
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
                if let Some(entry) = self.sidebar_items.get(self.sidebar_selected) {
                    self.collect_sidebar_removal_paths(entry)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_sidebar_removal_paths(&self, entry: &SidebarEntry) -> Vec<(String, u64)> {
        match entry {
            SidebarEntry::Artist(artist) => {
                let Some(albums) = self.device.albums.get(artist) else {
                    return Vec::new();
                };
                let mut items = Vec::new();
                for album in albums {
                    let key = (artist.clone(), album.clone());
                    if let Some(tracks) = self.device.album_tracks.get(&key) {
                        items.extend(tracks.iter().map(|t| (t.device_path.clone(), t.object_id)));
                    }
                }
                items
            }
            SidebarEntry::Album { artist, album } => self
                .device
                .album_tracks
                .get(&(artist.clone(), album.clone()))
                .map(|tracks| {
                    tracks
                        .iter()
                        .map(|t| (t.device_path.clone(), t.object_id))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Resolve a sidebar entry to an `(artist, album)` pair using the current
    /// album-browser selection when the entry is Artist-scoped.
    fn resolve_device_artist_album(&self, entry: &SidebarEntry) -> (String, String) {
        match entry {
            SidebarEntry::Artist(artist) => {
                let album = self
                    .album_list
                    .get(self.album_selected)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                (artist.clone(), album)
            }
            SidebarEntry::Album { artist, album } => (artist.clone(), album.clone()),
        }
    }

    /// Save the current sidebar selection for the active mode.
    pub fn save_sidebar_pos(&mut self) {
        self.saved_sidebar_pos
            .insert((self.browse_mode, self.sidebar_mode), self.sidebar_selected);
    }

    /// Restore the saved sidebar selection for the active mode, clamped to list bounds.
    fn restore_sidebar_pos(&mut self) {
        let saved = self
            .saved_sidebar_pos
            .get(&(self.browse_mode, self.sidebar_mode))
            .copied()
            .unwrap_or(0);
        if self.sidebar_items.is_empty() {
            self.sidebar_selected = 0;
        } else {
            self.sidebar_selected = saved.min(self.sidebar_items.len() - 1);
        }
    }

    /// Rebuild the full sidebar source (expensive: scans the library or device
    /// index). Call when the underlying data changes, not on every keystroke.
    fn rebuild_sidebar_source(&mut self) {
        self.sidebar_items_full = match self.browse_mode {
            BrowseMode::Library => {
                let lib = match &self.library {
                    Some(l) => l,
                    None => {
                        self.sidebar_items_full.clear();
                        self.sidebar_lowercase_full.clear();
                        self.sidebar_items.clear();
                        return;
                    }
                };

                match self.sidebar_mode {
                    SidebarMode::Artists => lib
                        .artists()
                        .into_iter()
                        .map(|s| SidebarEntry::Artist(s.to_string()))
                        .collect(),
                    SidebarMode::Albums => lib
                        .albums()
                        .into_iter()
                        .map(|(artist, album)| SidebarEntry::Album {
                            artist: artist.to_string(),
                            album: album.to_string(),
                        })
                        .collect(),
                }
            }
            BrowseMode::Device => match self.sidebar_mode {
                SidebarMode::Artists => self
                    .device
                    .artists
                    .iter()
                    .map(|a| SidebarEntry::Artist(a.clone()))
                    .collect(),
                SidebarMode::Albums => {
                    let mut items: Vec<SidebarEntry> = Vec::new();
                    for (artist, albums) in &self.device.albums {
                        for album in albums {
                            items.push(SidebarEntry::Album {
                                artist: artist.clone(),
                                album: album.clone(),
                            });
                        }
                    }
                    // Sort by (artist, album) — matches the previous
                    // string-sort of "Artist — Album" for pairs that do
                    // not include an em-dash in the artist name.
                    items.sort_by(|a, b| match (a, b) {
                        (
                            SidebarEntry::Album {
                                artist: aa,
                                album: ab,
                            },
                            SidebarEntry::Album {
                                artist: ba,
                                album: bb,
                            },
                        ) => aa.cmp(ba).then_with(|| ab.cmp(bb)),
                        _ => std::cmp::Ordering::Equal,
                    });
                    items
                }
            },
        };
        self.sidebar_lowercase_full = self
            .sidebar_items_full
            .iter()
            .map(SidebarEntry::lowercase_key)
            .collect();
    }

    /// Apply the `/` search filter to the cached source, producing the
    /// visible `sidebar_items`. Cheap: just walks `sidebar_lowercase_full`
    /// and clones matching entries — no new `to_lowercase` allocations.
    pub fn apply_sidebar_filter(&mut self) {
        if self.search_active && !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            self.sidebar_items = self
                .sidebar_items_full
                .iter()
                .zip(self.sidebar_lowercase_full.iter())
                .filter(|(_, lc)| lc.contains(&q))
                .map(|(entry, _)| entry.clone())
                .collect();
        } else {
            self.sidebar_items = self.sidebar_items_full.clone();
        }

        self.restore_sidebar_pos();
        self.sidebar_scroll = 0;
    }

    pub fn refresh_sidebar(&mut self) {
        self.rebuild_sidebar_source();
        self.apply_sidebar_filter();
        self.album_list.clear();
        self.track_list.clear();
        self.track_selected = 0;
        self.track_scroll = 0;
    }

    pub fn select_sidebar_item(&mut self) {
        let entry = match self.sidebar_items.get(self.sidebar_selected) {
            Some(e) => e.clone(),
            None => return,
        };

        if self.browse_mode == BrowseMode::Device {
            self.select_sidebar_item_device(&entry);
            return;
        }

        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        match &entry {
            SidebarEntry::Artist(artist) => {
                let mut album_map: std::collections::BTreeMap<String, (Option<u32>, usize)> =
                    std::collections::BTreeMap::new();
                for t in lib.artist_tracks(artist) {
                    let e = album_map.entry(t.album.clone()).or_insert((t.year, 0));
                    e.1 += 1;
                    if e.0.is_none() && t.year.is_some() {
                        e.0 = t.year;
                    }
                }
                self.album_list = album_map
                    .into_iter()
                    .map(|(name, (year, count))| AlbumInfo {
                        name,
                        artist: artist.clone(),
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
            SidebarEntry::Album { album, .. } => {
                self.album_list.clear();
                self.track_list = tracks_to_info(lib.album_tracks(album), &self.device);
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
                self.refresh_album_art();
            }
        }
    }

    fn select_sidebar_item_device(&mut self, entry: &SidebarEntry) {
        match entry {
            SidebarEntry::Artist(artist) => {
                if let Some(albums) = self.device.albums.get(artist) {
                    self.album_list = albums
                        .iter()
                        .map(|album_name| {
                            let count = self
                                .device
                                .album_tracks
                                .get(&(artist.clone(), album_name.clone()))
                                .map(|t| t.len())
                                .unwrap_or(0);
                            AlbumInfo {
                                name: album_name.clone(),
                                artist: artist.clone(),
                                year: None,
                                track_count: count,
                            }
                        })
                        .collect();
                    self.album_selected = 0;
                    self.select_album();
                }
            }
            SidebarEntry::Album { artist, album } => {
                self.album_list.clear();
                let key = (artist.clone(), album.clone());
                self.track_list = match self.device.album_tracks.get(&key) {
                    Some(tracks) => device_tracks_to_info(tracks),
                    None => Vec::new(),
                };
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
            sort_album_tracks(&mut self.track_list);
            self.track_selected = 0;
            self.track_scroll = 0;
            return;
        }

        let lib = match &self.library {
            Some(l) => l,
            None => return,
        };

        self.track_list = tracks_to_info(
            lib.album_tracks_by_artist(&album.artist, &album.name),
            &self.device,
        );
        sort_album_tracks(&mut self.track_list);
        self.track_selected = 0;
        self.track_scroll = 0;
        self.refresh_album_art();
    }

    pub fn has_album_browser(&self) -> bool {
        !self.album_list.is_empty()
    }

    /// Request album art extraction in the background thread.
    pub fn refresh_album_art(&mut self) {
        // Build a cache key from the current track list context.
        let key = if let Some(t) = self.track_list.first() {
            format!("{}/{}", t.artist, t.album)
        } else {
            self.album_art = None;
            self.album_art_key.clear();
            self.album_art_cache = None;
            self.album_art_size = (0, 0);
            return;
        };

        if key == self.album_art_key {
            return; // already cached
        }
        self.album_art_key = key.clone();
        self.album_art = None;
        self.album_art_cache = None;
        self.album_art_size = (0, 0);

        // Collect file paths and dispatch to the background thread.
        let paths: Vec<String> = self
            .track_list
            .iter()
            .filter_map(|t| t.location.clone())
            .collect();
        self.pending_bg_commands
            .push(BgCommand::LoadAlbumArt { key, paths });
    }

    /// Rebuild the album-art cache sized to fit within the given terminal area,
    /// using the renderer selected by `album_art_style`:
    ///
    /// - `Halfblock`: each cell packs two vertical pixels as ▀ with fg=top,
    ///   bg=bottom — doubling vertical resolution.
    /// - `Ascii`: one pixel per cell, mapped to a character ramp by luminance
    ///   with fg=pixel color, bg=theme background.
    ///
    /// Only the cache for the active style is populated.
    pub fn render_album_art(&mut self, width: u16, height: u16) {
        let cache_valid = (width, height) == self.album_art_size
            && self
                .album_art_cache
                .as_ref()
                .is_some_and(|c| c.matches_style(self.album_art_style));
        if cache_valid {
            return;
        }
        self.album_art_size = (width, height);
        self.album_art_cache = None;

        let img = match &self.album_art {
            Some(img) => img,
            None => return,
        };

        // Terminal chars are roughly 1:2 (w:h), so 1 cell spans 1 pixel wide × 2 pixels tall.
        // Fit the image within the available area preserving aspect ratio.
        let (iw, ih) = (img.width(), img.height());
        let max_px_w = width as u32;
        let max_px_h = height as u32 * 2; // 2 pixel rows per terminal row
        let scale = (max_px_w as f64 / iw as f64).min(max_px_h as f64 / ih as f64);
        let cols = ((iw as f64 * scale).round() as u32).max(1);
        let px_h = ((ih as f64 * scale).round() as u32).max(2);
        let rows = px_h / 2;

        self.album_art_cache = Some(match self.album_art_style {
            AlbumArtStyle::Halfblock => {
                let resized =
                    img.resize_exact(cols, rows * 2, image::imageops::FilterType::Lanczos3);
                let rgba = resized.to_rgba8();
                let mut lines = Vec::with_capacity(rows as usize);
                for row in 0..rows {
                    let mut line = Vec::with_capacity(cols as usize);
                    for col in 0..cols {
                        let top = rgba.get_pixel(col, row * 2);
                        let bot = rgba.get_pixel(col, row * 2 + 1);
                        line.push(('▀', [top[0], top[1], top[2]], [bot[0], bot[1], bot[2]]));
                    }
                    lines.push(line);
                }
                AlbumArtCache::Halfblock(lines)
            }
            AlbumArtStyle::Ascii => {
                // One char per cell: sample one pixel per cell and map luminance to the ramp.
                let resized = img.resize_exact(cols, rows, image::imageops::FilterType::Lanczos3);
                let rgba = resized.to_rgba8();
                let ramp_len = ASCII_ART_RAMP.len() as u32;
                let mut lines = Vec::with_capacity(rows as usize);
                for row in 0..rows {
                    let mut line = Vec::with_capacity(cols as usize);
                    for col in 0..cols {
                        let p = rgba.get_pixel(col, row);
                        // Rec. 709 luma.
                        let lum = (0.2126 * p[0] as f32
                            + 0.7152 * p[1] as f32
                            + 0.0722 * p[2] as f32) as u32;
                        let idx = (lum * ramp_len / 256).min(ramp_len - 1) as usize;
                        let ch = ASCII_ART_RAMP[idx] as char;
                        line.push((ch, [p[0], p[1], p[2]]));
                    }
                    lines.push(line);
                }
                AlbumArtCache::Ascii(lines)
            }
        });
    }

    fn sort_tracks(&mut self) {
        // `sort_by_cached_key` computes each key once per element, not once per
        // comparison — so the string columns allocate `n` lowercased Strings
        // instead of `2·n·log n` worth of them.
        match self.sort_column {
            SortColumn::Number => self.track_list.sort_by_cached_key(|t| t.track_number),
            SortColumn::Name => self
                .track_list
                .sort_by_cached_key(|t| t.name.to_lowercase()),
            SortColumn::Artist => self
                .track_list
                .sort_by_cached_key(|t| t.artist.to_lowercase()),
            SortColumn::Album => self
                .track_list
                .sort_by_cached_key(|t| t.album.to_lowercase()),
            SortColumn::Duration => self.track_list.sort_by_cached_key(|t| t.duration_ms),
            SortColumn::Format => self.track_list.sort_by_cached_key(|t| t.kind.clone()),
        }
        if !self.sort_ascending {
            self.track_list.reverse();
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
                    track_number: track.track_number,
                    genre: track.genre.clone(),
                    overwrite_targets: Vec::new(),
                };
                self.sync.queue.push(QueuedItem {
                    label: format!("{} - {} - {}", track.artist, track.album, track.name),
                    tracks: vec![item],
                });
                self.set_toast(format!("Added \"{}\" to queue", track.name), false);
                self.forward_last_queue_item_if_syncing();
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
                    track_number: track.track_number,
                    genre: track.genre.clone(),
                    overwrite_targets: Vec::new(),
                });
            }
        }
        if items.is_empty() {
            self.set_toast("No tracks with file locations".into(), true);
            return;
        }
        let count = items.len();
        // Label holds the canonical name only. The queue renderer appends the
        // live track count so it stays accurate as tracks drain during sync.
        let label = match self.sidebar_items.get(self.sidebar_selected) {
            Some(entry) => entry.to_string(),
            None => "Tracks".to_string(),
        };
        self.sync.queue.push(QueuedItem {
            label,
            tracks: items,
        });
        self.set_toast(format!("Added {} tracks to queue", count), false);
        self.forward_last_queue_item_if_syncing();
    }

    pub fn add_sidebar_item_to_queue(&mut self) {
        let entry = match self.sidebar_items.get(self.sidebar_selected) {
            Some(e) => e.clone(),
            None => return,
        };

        match &entry {
            SidebarEntry::Artist(artist) => {
                // For artists, gather ALL tracks across all albums.
                let lib = match &self.library {
                    Some(l) => l,
                    None => return,
                };
                let mut items = Vec::new();
                for t in lib.artist_tracks(artist) {
                    if let Some(ref loc) = t.location {
                        items.push(SyncItem {
                            artist: t.artist.clone(),
                            album: t.album.clone(),
                            name: t.name.clone(),
                            location: loc.clone(),
                            track_number: t.track_number,
                            genre: t.genre.clone(),
                            overwrite_targets: Vec::new(),
                        });
                    }
                }
                if items.is_empty() {
                    self.set_toast("No tracks with file locations".into(), true);
                    return;
                }
                let count = items.len();
                // Label holds the artist name only; the queue renderer appends
                // the live count so it updates as tracks drain during sync.
                self.sync.queue.push(QueuedItem {
                    label: artist.clone(),
                    tracks: items,
                });
                self.set_toast(format!("Added {} tracks to queue", count), false);
                self.forward_last_queue_item_if_syncing();
            }
            SidebarEntry::Album { .. } => {
                // For albums, select to populate track list, then add all.
                // (add_all_visible_to_queue handles the forward itself.)
                self.select_sidebar_item();
                self.add_all_visible_to_queue();
            }
        }
    }

    /// If a sync is currently in flight, forward the tracks of the most
    /// recently pushed queue entry to the background worker so they get
    /// picked up by the running sync instead of being orphaned when the
    /// queue is cleared on completion. Mirrors `execute_sync`: every track
    /// is forwarded and any existing on-device copies are attached as
    /// `overwrite_targets` for the worker to remove before upload.
    fn forward_last_queue_item_if_syncing(&mut self) {
        if !matches!(self.sync.status, SyncStatus::Running { .. }) {
            return;
        }
        let Some(item) = self.sync.queue.last() else {
            return;
        };
        let mut forward: Vec<SyncItem> = item.tracks.clone();
        let mut overwrite_total = 0usize;
        for it in &mut forward {
            it.overwrite_targets = find_device_copies(&self.device, &it.artist, &it.name);
            overwrite_total += it.overwrite_targets.len();
        }
        if forward.is_empty() {
            return;
        }
        let n = forward.len();
        self.sync.log.push(if overwrite_total > 0 {
            format!(
                "Appended mid-sync: {n} {trk}; overwriting {overwrite_total} existing {copies}",
                trk = if n == 1 { "track" } else { "tracks" },
                copies = if overwrite_total == 1 {
                    "copy"
                } else {
                    "copies"
                },
            )
        } else {
            format!(
                "Appended mid-sync: {n} {trk}",
                trk = if n == 1 { "track" } else { "tracks" }
            )
        });
        self.pending_bg_commands
            .push(BgCommand::AppendSyncQueue(forward));
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

    /// Collect (device_path, object_id) pairs for every older copy of a
    /// duplicate on the device — the "keep newest, remove the rest" set.
    /// Groups are keyed on normalized (artist, album, name) so case and
    /// whitespace variants collapse together. Returns empty when the
    /// device has no duplicates. Pairs sweep across artists/albums so one
    /// BgCommand::RemoveFromDevice clears the entire backlog.
    pub fn collect_device_duplicates(&self) -> Vec<(String, u64)> {
        use std::collections::HashMap;
        type Key = (String, String, String);
        let mut groups: HashMap<Key, Vec<(String, u64)>> = HashMap::new();
        for tracks in self.device.album_tracks.values() {
            for t in tracks {
                let key = (
                    normalize_for_match(&t.artist),
                    normalize_for_match(&t.album),
                    normalize_for_match(&t.name),
                );
                groups
                    .entry(key)
                    .or_default()
                    .push((t.device_path.clone(), t.object_id));
            }
        }
        let mut out = Vec::new();
        for (_, mut copies) in groups {
            if copies.len() < 2 {
                continue;
            }
            // Newest object_id wins — MTP assigns monotonically, so the
            // highest ID corresponds to the most recent upload (which is
            // what the user just synced or most recently re-tagged).
            copies.sort_by_key(|(_, oid)| *oid);
            copies.pop(); // keep newest
            out.extend(copies);
        }
        out
    }

    pub fn dedupe_device(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        if self.device.status != DeviceStatus::Connected {
            self.set_toast("Connect a device first".into(), true);
            return;
        }
        let targets = self.collect_device_duplicates();
        if targets.is_empty() {
            self.set_toast("No duplicates found".into(), false);
            return;
        }
        let count = targets.len();
        self.set_toast(
            format!("Removing {} duplicate copy(ies) from device...", count),
            false,
        );
        let _ = cmd_tx.send(BgCommand::RemoveFromDevice(targets));
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
        // Queue every track — no pre-sync skip. Tracks that match existing
        // on-device copies get those copies' (device_path, object_id) stamped
        // into `overwrite_targets`; the worker removes them before uploading
        // the new file. Picks up pre-existing duplicates as a side effect.
        let mut items: Vec<SyncItem> = self
            .sync
            .queue
            .iter()
            .flat_map(|q| q.tracks.clone())
            .collect();
        let mut overwrite_total = 0usize;
        for it in &mut items {
            it.overwrite_targets = find_device_copies(&self.device, &it.artist, &it.name);
            overwrite_total += it.overwrite_targets.len();
        }
        let n = items.len();
        let plan = if overwrite_total > 0 {
            format!(
                "Syncing {n} {trk}; overwriting {overwrite_total} existing {copies}",
                trk = if n == 1 { "track" } else { "tracks" },
                copies = if overwrite_total == 1 {
                    "copy"
                } else {
                    "copies"
                }
            )
        } else {
            format!(
                "Syncing {n} {trk}",
                trk = if n == 1 { "track" } else { "tracks" }
            )
        };
        self.sync.log.push(plan);
        let _ = cmd_tx.send(BgCommand::ExecuteSyncQueue(items));
    }

    pub fn handle_bg_event(&mut self, event: BgEvent) {
        match event {
            BgEvent::LibraryLoaded(result) => {
                self.loading_library = false;
                self.scan_progress = None;
                self.scan_phrase = None;
                self.scan_phrase_rotated_at = None;
                self.scan_samples.clear();
                match result {
                    Ok(lib) => {
                        self.library = Some(lib);
                        self.rebuild_artist_device_status();
                        self.refresh_sidebar();
                    }
                    Err(e) => {
                        self.set_toast(format!("Library: {}", e), true);
                    }
                }
            }
            BgEvent::LibraryScanProgress(p) => {
                self.scan_progress = Some((p.completed, p.total));
                if let Some(sample) = p.sample {
                    // Keep a small rolling buffer (~128 most recent).
                    if self.scan_samples.len() >= 128 {
                        self.scan_samples.remove(0);
                    }
                    self.scan_samples.push(sample);
                }
                self.maybe_rotate_scan_phrase();
            }
            BgEvent::DeviceDetected(info) => {
                self.device.name = Some(info.name);
                self.device.firmware = info.firmware_version;
                self.device.serial = info.serial_number;
                self.device.manufacturer = info.manufacturer;
                self.device.model = info.model;
                self.device.usb_mode = info.usb_mode;
                self.device.family = Some(info.family);
                self.device.status = DeviceStatus::Connecting;
            }
            BgEvent::SessionReady(storage) => {
                self.device.status = DeviceStatus::Connected;
                self.connection_anim_start = None;
                self.device.storage = storage;
                self.set_toast("Device connected".into(), false);

                // Auto-sync photos/videos if configured.
                let cfg = crate::config::load();
                if let Some(photo_dir) = std::env::var("ZYTUNES_PHOTOS_DIR").ok().or(cfg.photo_dir)
                {
                    self.pending_bg_commands
                        .push(BgCommand::SyncPhotos { dir: photo_dir });
                }
                if let Some(video_dir) = std::env::var("ZYTUNES_VIDEOS_DIR").ok().or(cfg.video_dir)
                {
                    self.pending_bg_commands
                        .push(BgCommand::SyncVideos { dir: video_dir });
                }
            }
            BgEvent::SessionFailed(e) => {
                self.device.status = DeviceStatus::Disconnected;
                self.device.sync_status = None;
                self.connection_anim_start = None;
                self.set_toast(format!("Connection failed: {}", e), true);
                if self.browse_mode == BrowseMode::Library {
                    self.retag_on_device();
                }
            }
            BgEvent::DeviceSyncStatus(status) => {
                self.device.sync_status = status;
            }
            BgEvent::AlbumArtLoaded { key, image } => {
                // Only apply if the key still matches (user hasn't navigated away).
                if key == self.album_art_key {
                    self.album_art = image;
                    self.album_art_cache = None;
                    self.album_art_size = (0, 0);
                }
            }
            BgEvent::LoadingDeviceTracks => {
                self.device.loading_tracks = true;
                self.set_toast("Loading device tracks...".into(), false);
            }
            BgEvent::DeviceTracksLoaded(tracks) => {
                self.device.loading_tracks = false;
                self.device.tracks = tracks;
                self.build_device_index();
                self.rebuild_artist_device_status();
                self.set_toast(
                    format!("Loaded {} device tracks", self.device.tracks.len()),
                    false,
                );
                if self.browse_mode == BrowseMode::Device {
                    self.refresh_sidebar();
                }
                if self.browse_mode == BrowseMode::Library {
                    self.retag_on_device();
                }
            }
            BgEvent::DeviceTrackAdded(entry) => {
                self.device.add_indexed_track(&entry);
                self.device.tracks.push(entry);
                self.device_index_dirty = true;
            }
            BgEvent::DeviceTrackRemoved(path) => {
                // path is a full device path like "/Music/Artist/Album/track.mp3"
                // but DeviceEntry.name is relative like "Artist/Album/track.mp3"
                let relative = path.strip_prefix("/Music/").unwrap_or(&path).to_string();
                self.device.tracks.retain(|t| t.name != relative);
                self.device.remove_indexed_track(&relative);
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
            BgEvent::SyncComplete {
                success,
                failed,
                skipped,
            } => {
                self.sync.status = SyncStatus::Idle;
                self.sync.queue.clear();
                self.sync.queue_selected = 0;
                let msg = if skipped > 0 {
                    format!(
                        "Sync complete: {} done, {} skipped, {} failed",
                        success, skipped, failed
                    )
                } else {
                    format!("Sync complete: {} done, {} failed", success, failed)
                };
                self.set_toast(msg, failed > 0 || skipped > 0);
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
            BgEvent::PhotoSyncComplete { success, failed } => {
                if success > 0 || failed > 0 {
                    self.set_toast(
                        format!("Photo sync: {} done, {} failed", success, failed),
                        failed > 0,
                    );
                }
            }
            BgEvent::VideoSyncComplete { success, failed } => {
                if success > 0 || failed > 0 {
                    self.set_toast(
                        format!("Video sync: {} done, {} failed", success, failed),
                        failed > 0,
                    );
                }
            }
            BgEvent::AcquiredItemsCount(count) => {
                self.device.acquired_items = count;
            }
        }
    }

    pub fn set_toast(&mut self, msg: String, is_error: bool) {
        self.toast_message = Some((msg, Instant::now(), is_error));
    }

    /// Pick a new loading-screen phrase if enough time has passed (or none is set).
    fn maybe_rotate_scan_phrase(&mut self) {
        let should_rotate = self
            .scan_phrase_rotated_at
            .map(|t| t.elapsed().as_millis() >= SCAN_PHRASE_MS)
            .unwrap_or(true);
        if !should_rotate || self.scan_samples.is_empty() {
            return;
        }

        let seed = quick_random();
        let phrase = &SCAN_PHRASES[seed % SCAN_PHRASES.len()];
        let sample = &self.scan_samples[(seed / 7) % self.scan_samples.len()];
        let target: &str = match phrase.field {
            ScanField::Artist => &sample.artist,
            ScanField::Album => &sample.album,
            ScanField::Track => &sample.name,
        };
        self.scan_phrase = Some(format!("{} {}", phrase.prefix, target));
        self.scan_phrase_rotated_at = Some(Instant::now());
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

        // Keep the scan phrase rotating even if no new samples arrive (e.g.
        // the scan has stalled on a slow file).
        if self.loading_library {
            self.maybe_rotate_scan_phrase();
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
                let current_char =
                    first_char_upper(self.sidebar_items[self.sidebar_selected].nav_key());
                for i in (self.sidebar_selected + 1)..self.sidebar_items.len() {
                    if first_char_upper(self.sidebar_items[i].nav_key()) != current_char {
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
                    let current_char =
                        first_char_upper(self.sidebar_items[self.sidebar_selected].nav_key());
                    let mut i = self.sidebar_selected;
                    while i > 0
                        && first_char_upper(self.sidebar_items[i - 1].nav_key()) == current_char
                    {
                        i -= 1;
                    }
                    if i > 0 {
                        let prev_char = first_char_upper(self.sidebar_items[i - 1].nav_key());
                        while i > 0
                            && first_char_upper(self.sidebar_items[i - 1].nav_key()) == prev_char
                        {
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

fn tracks_to_info<'a, I>(tracks: I, device: &DeviceState) -> Vec<TrackInfo>
where
    I: IntoIterator<Item = &'a Track>,
{
    tracks
        .into_iter()
        .map(|t| {
            let artist_key = normalize_for_match(&t.artist);
            let name_key = normalize_for_match(&t.name);
            let on_device = device.contains_track(&artist_key, &name_key);
            TrackInfo {
                name: t.name.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                duration_ms: t.total_time_ms,
                kind: t.kind.clone(),
                location: t.location.clone(),
                track_number: t.track_number,
                disc_number: t.disc_number,
                genre: t.genre.clone(),
                on_device,
                artist_key,
                name_key,
            }
        })
        .collect()
}

/// Map a `(matched, total)` track count to a tri-state presence marker.
fn presence_from_counts(matched: usize, total: usize) -> DevicePresence {
    if total == 0 || matched == 0 {
        DevicePresence::None
    } else if matched >= total {
        DevicePresence::Full
    } else {
        DevicePresence::Partial
    }
}

/// Check if a single track is on device. Normalizes the inputs on the fly —
/// callers that already have pre-normalized keys (e.g. on `TrackInfo`) should
/// call `DeviceState::contains_track` directly to avoid the allocations.
fn is_on_device(artist: &str, name: &str, device: &DeviceState) -> bool {
    if device.status != DeviceStatus::Connected || device.track_set.is_empty() {
        return false;
    }
    let artist_key = normalize_for_match(artist);
    let name_key = normalize_for_match(name);
    device.contains_track(&artist_key, &name_key)
}

/// Find every on-device track whose normalized (artist, name) matches the
/// supplied pair. Returns `(device_path, object_id)` tuples — used by
/// `execute_sync` to populate `SyncItem::overwrite_targets` so re-queuing a
/// track wipes its existing on-device copies (including pre-existing
/// duplicates) before the new upload lands. Returns an empty vec if the
/// device isn't connected or the track isn't on it.
pub fn find_device_copies(device: &DeviceState, artist: &str, name: &str) -> Vec<(String, u64)> {
    if device.status != DeviceStatus::Connected {
        return Vec::new();
    }
    let artist_key = normalize_for_match(artist);
    let name_key = normalize_for_match(name);
    let stripped_key = normalize_for_match(zytunes::strip_track_number(name.trim()));

    let mut out = Vec::new();
    for tracks in device.album_tracks.values() {
        for t in tracks {
            if normalize_for_match(&t.artist) != artist_key {
                continue;
            }
            let t_name_key = normalize_for_match(&t.name);
            let t_stripped_key = normalize_for_match(zytunes::strip_track_number(t.name.trim()));
            // Bidirectional match — handles filenames with added/stripped
            // track-number prefixes, matching the same fallback
            // `DeviceState::contains_track` does for on/off-device tags.
            let matches = t_name_key == name_key
                || t_name_key == stripped_key
                || t_stripped_key == name_key
                || t_name_key.contains(&name_key)
                || name_key.contains(&t_name_key);
            if matches {
                out.push((t.device_path.clone(), t.object_id));
            }
        }
    }
    out
}

/// Sort a track list for album→track view. Disc number first, then track
/// number; tracks missing a number fall to the bottom so tagged tracks
/// stay in their intended album order. Shared between the Library and
/// Device paths so both views present tracks in the same album order.
fn sort_album_tracks(tracks: &mut [TrackInfo]) {
    tracks.sort_by(|a, b| {
        let disc = match (a.disc_number, b.disc_number) {
            (Some(da), Some(db)) => da.cmp(&db),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        if disc != std::cmp::Ordering::Equal {
            return disc;
        }
        match (a.track_number, b.track_number) {
            (Some(ta), Some(tb)) => ta.cmp(&tb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }
    });
}

fn device_tracks_to_info(tracks: &[DeviceTrackInfo]) -> Vec<TrackInfo> {
    tracks
        .iter()
        .map(|dt| {
            TrackInfo::new(
                dt.name.clone(),
                dt.artist.clone(),
                dt.album.clone(),
                None,
                None,
                None,
                dt.track_number,
                dt.disc_number,
                None,
                false,
            )
        })
        .collect()
}

/// Normalize a string for fuzzy matching: lowercase, trim, strip leading/trailing
/// non-alphanumeric characters (handles `*NSYNC` vs `NSYNC`, etc.).
fn normalize_for_match(s: &str) -> String {
    let trimmed = s.trim().to_lowercase();
    trimmed
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_string()
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
    fn adding_track_while_syncing_forwards_to_worker() {
        let mut app = App::new();
        // Simulate an in-flight sync.
        app.sync.status = SyncStatus::Running {
            current: 1,
            total: 5,
        };
        app.track_list.push(TrackInfo::new(
            "Idioteque".into(),
            "Radiohead".into(),
            "Kid A".into(),
            None,
            None,
            Some("/tmp/idioteque.mp3".into()),
            None,
            None,
            None,
            false,
        ));
        app.track_selected = 0;

        app.add_selected_track_to_queue();

        // Queue retains a record of the addition (for display on completion).
        assert_eq!(app.sync.queue.len(), 1);

        // And the worker was told to append to the running sync.
        let appended = app
            .pending_bg_commands
            .iter()
            .filter_map(|cmd| match cmd {
                BgCommand::AppendSyncQueue(items) => Some(items.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(appended.len(), 1, "expected exactly one AppendSyncQueue");
        assert_eq!(appended[0].len(), 1);
        assert_eq!(appended[0][0].name, "Idioteque");
    }

    #[test]
    fn mid_sync_append_populates_overwrite_targets() {
        // When a user queues a track mid-sync that's already on the device,
        // the append path forwards both tracks — the duplicate gets its
        // on-device copy stamped into overwrite_targets so the worker wipes
        // it before the new upload lands.
        let mut app = App::new();
        app.sync.status = SyncStatus::Running {
            current: 1,
            total: 5,
        };
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                object_id: 9,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        app.sync.queue.push(QueuedItem {
            label: "mix".into(),
            tracks: vec![
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Bohemian Rhapsody".into(),
                    location: "/music/a.mp3".into(),
                    ..Default::default()
                },
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Love of My Life".into(),
                    location: "/music/b.mp3".into(),
                    ..Default::default()
                },
            ],
        });

        app.forward_last_queue_item_if_syncing();

        let appended = app
            .pending_bg_commands
            .iter()
            .filter_map(|cmd| match cmd {
                BgCommand::AppendSyncQueue(items) => Some(items.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].len(), 2, "both tracks should forward");
        let bohemian = appended[0]
            .iter()
            .find(|i| i.name == "Bohemian Rhapsody")
            .expect("Bohemian Rhapsody forwarded");
        assert_eq!(bohemian.overwrite_targets.len(), 1);
        assert_eq!(bohemian.overwrite_targets[0].1, 9);
        let love = appended[0]
            .iter()
            .find(|i| i.name == "Love of My Life")
            .expect("Love of My Life forwarded");
        assert!(love.overwrite_targets.is_empty());

        assert!(
            app.sync
                .log
                .iter()
                .any(|m| m.contains("Appended mid-sync") && m.contains("overwriting 1")),
            "expected overwrite-summary log line, got {:?}",
            app.sync.log
        );
    }

    #[test]
    fn mid_sync_append_all_on_device_still_forwards_for_overwrite() {
        // Every track matching an on-device copy still forwards — the
        // worker receives the entry with overwrite_targets populated so
        // the refresh happens instead of a silent skip.
        let mut app = App::new();
        app.sync.status = SyncStatus::Running {
            current: 1,
            total: 5,
        };
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                object_id: 9,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);
        app.sync.queue.push(QueuedItem {
            label: "dup".into(),
            tracks: vec![SyncItem {
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                name: "Bohemian Rhapsody".into(),
                location: "/music/a.mp3".into(),
                ..Default::default()
            }],
        });

        app.forward_last_queue_item_if_syncing();

        let appended: Vec<_> = app
            .pending_bg_commands
            .iter()
            .filter_map(|cmd| match cmd {
                BgCommand::AppendSyncQueue(items) => Some(items.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].len(), 1);
        assert_eq!(appended[0][0].overwrite_targets.len(), 1);
        assert_eq!(appended[0][0].overwrite_targets[0].1, 9);
    }

    #[test]
    fn adding_track_when_idle_does_not_forward() {
        let mut app = App::new();
        assert!(matches!(app.sync.status, SyncStatus::Idle));
        app.track_list.push(TrackInfo::new(
            "Creep".into(),
            "Radiohead".into(),
            "Pablo Honey".into(),
            None,
            None,
            Some("/tmp/creep.mp3".into()),
            None,
            None,
            None,
            false,
        ));
        app.track_selected = 0;

        app.add_selected_track_to_queue();

        assert_eq!(app.sync.queue.len(), 1);
        assert!(
            !app.pending_bg_commands
                .iter()
                .any(|cmd| matches!(cmd, BgCommand::AppendSyncQueue(_))),
            "should not forward while sync is Idle",
        );
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
    fn collect_device_duplicates_keeps_newest_per_group() {
        // Explicit dedupe helper keeps the highest object_id per normalized
        // (artist, album, name) group and returns the rest for removal.
        // Groups of 1 produce no output; distinct albums-with-same-name
        // don't get grouped together.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                    object_id: 10,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody (1).mp3"
                        .into(),
                    object_id: 11,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody (2).mp3"
                        .into(),
                    object_id: 12,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                // Unique within the same album — must NOT appear in the
                // dedupe set.
                DeviceTrackInfo {
                    name: "Love of My Life".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Love of My Life.mp3".into(),
                    object_id: 13,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
            ],
        );
        // Same track name but different album — MUST NOT group with the
        // Night at the Opera rows.
        app.device.album_tracks.insert(
            ("Queen".into(), "Greatest Hits".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/Greatest Hits/Bohemian Rhapsody.mp3".into(),
                object_id: 20,
                artist: "Queen".into(),
                album: "Greatest Hits".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        let dupes = app.collect_device_duplicates();
        let mut oids: Vec<u64> = dupes.iter().map(|(_, oid)| *oid).collect();
        oids.sort();
        // Of the three Bohemian copies in Night at the Opera (10/11/12),
        // newest (12) is kept; 10 and 11 are targeted for removal. Nothing
        // else shows up.
        assert_eq!(oids, vec![10, 11]);
    }

    #[test]
    fn dedupe_device_dispatches_remove_when_duplicates_exist() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                    object_id: 10,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody (1).mp3"
                        .into(),
                    object_id: 11,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
            ],
        );
        build_match_sets(&mut app.device);

        let (tx, rx) = mpsc::channel();
        app.dedupe_device(&tx);

        match rx.try_recv() {
            Ok(BgCommand::RemoveFromDevice(items)) => {
                assert_eq!(items.len(), 1, "only the older copy gets removed");
                assert_eq!(items[0].1, 10);
            }
            Ok(other) => panic!(
                "expected RemoveFromDevice, got {:?}",
                std::mem::discriminant(&other)
            ),
            Err(_) => panic!("no command dispatched"),
        }
    }

    #[test]
    fn dedupe_device_toasts_when_no_duplicates() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                object_id: 10,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        let (tx, rx) = mpsc::channel();
        app.dedupe_device(&tx);

        assert!(rx.try_recv().is_err(), "no command should be dispatched");
        let toast = app.toast_message.as_ref().expect("toast should be set");
        assert!(toast.0.contains("No duplicates"));
    }

    #[test]
    fn find_device_copies_returns_every_matching_copy() {
        // If the device already has multiple copies of a track (say from
        // earlier sync mistakes), find_device_copies must return ALL of
        // them so overwrite-on-sync sweeps the pile instead of leaving
        // orphan duplicates behind.
        let mut device = DeviceState::new();
        device.status = DeviceStatus::Connected;
        device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                    object_id: 10,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Bohemian Rhapsody".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody (1).mp3"
                        .into(),
                    object_id: 11,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Love of My Life".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Love of My Life.mp3".into(),
                    object_id: 12,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                },
            ],
        );
        build_match_sets(&mut device);

        let copies = find_device_copies(&device, "Queen", "Bohemian Rhapsody");
        let mut oids: Vec<u64> = copies.iter().map(|(_, oid)| *oid).collect();
        oids.sort();
        assert_eq!(oids, vec![10, 11], "both duplicate copies must be returned");

        // Disconnected device returns no matches even if the index is populated.
        device.status = DeviceStatus::Disconnected;
        assert!(find_device_copies(&device, "Queen", "Bohemian Rhapsody").is_empty());
    }

    #[test]
    fn execute_sync_populates_overwrite_targets_for_duplicates() {
        // Tracks matching existing on-device copies must be dispatched with
        // their (device_path, object_id) stamped into `overwrite_targets`
        // so the worker removes the old copies before uploading the new —
        // no more silent "already on device" skip.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                object_id: 77,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        app.sync.queue.push(QueuedItem {
            label: "test".into(),
            tracks: vec![
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Bohemian Rhapsody".into(),
                    location: "/music/queen/bohemian.mp3".into(),
                    ..Default::default()
                },
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Love of My Life".into(),
                    location: "/music/queen/love.mp3".into(),
                    ..Default::default()
                },
            ],
        });

        let (tx, rx) = mpsc::channel();
        app.execute_sync(&tx);

        // Both tracks dispatched; the duplicate carries the overwrite target.
        match rx.try_recv() {
            Ok(BgCommand::ExecuteSyncQueue(items)) => {
                assert_eq!(items.len(), 2, "both tracks should be dispatched");
                let bohemian = items
                    .iter()
                    .find(|i| i.name == "Bohemian Rhapsody")
                    .expect("Bohemian Rhapsody in items");
                assert_eq!(
                    bohemian.overwrite_targets,
                    vec![(
                        "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".to_string(),
                        77u64
                    )]
                );
                let love = items
                    .iter()
                    .find(|i| i.name == "Love of My Life")
                    .expect("Love of My Life in items");
                assert!(
                    love.overwrite_targets.is_empty(),
                    "non-duplicate should have empty overwrite_targets"
                );
            }
            Ok(_) => panic!("expected ExecuteSyncQueue"),
            Err(_) => panic!("no command dispatched"),
        }
        // Plan summary mentions the overwrite count so the user sees what's happening.
        assert!(
            app.sync
                .log
                .iter()
                .any(|m| m.contains("Syncing 2") && m.contains("overwriting 1")),
            "expected overwrite summary, got {:?}",
            app.sync.log
        );
    }

    #[test]
    fn execute_sync_without_duplicates_logs_plain_plan() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.sync.queue.push(QueuedItem {
            label: "test".into(),
            tracks: vec![
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Bohemian Rhapsody".into(),
                    location: "/music/queen/bohemian.mp3".into(),
                    ..Default::default()
                },
                SyncItem {
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    name: "Love of My Life".into(),
                    location: "/music/queen/love.mp3".into(),
                    ..Default::default()
                },
            ],
        });

        let (tx, _rx) = mpsc::channel();
        app.execute_sync(&tx);

        // When nothing is skipped, the log should just state the count —
        // no trailing "; 0 already on device" noise.
        assert!(
            app.sync.log.iter().any(|m| m == "Syncing 2 tracks"),
            "expected plain plan, got {:?}",
            app.sync.log
        );
        assert!(
            !app.sync.log.iter().any(|m| m.contains("already on device")),
            "skip phrase must not appear when skip=0: {:?}",
            app.sync.log
        );
    }

    #[test]
    fn execute_sync_plan_is_singular_for_one_track() {
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.sync.queue.push(QueuedItem {
            label: "test".into(),
            tracks: vec![SyncItem {
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                name: "Bohemian Rhapsody".into(),
                location: "/music/queen/bohemian.mp3".into(),
                ..Default::default()
            }],
        });

        let (tx, _rx) = mpsc::channel();
        app.execute_sync(&tx);

        // Grammar check: "1 track" (singular), not "1 tracks".
        assert!(
            app.sync.log.iter().any(|m| m == "Syncing 1 track"),
            "expected singular form, got {:?}",
            app.sync.log
        );
    }

    #[test]
    fn queue_labels_do_not_bake_track_count() {
        // Queue-entry labels should hold the canonical name only — the UI is
        // responsible for rendering the live count, so baking "(N tracks)"
        // into the label produces the duplicated "(N tracks) (N tracks)"
        // we used to print.
        use zytunes::library::Track;
        struct OneArtist;
        impl zytunes::library::MusicLibrary for OneArtist {
            fn artists(&self) -> Vec<&str> {
                vec!["Queen"]
            }
            fn albums(&self) -> Vec<(&str, &str)> {
                vec![("Queen", "A Night at the Opera")]
            }
            fn artist_tracks<'a>(&'a self, _: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn album_tracks<'a>(&'a self, _: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn album_tracks_by_artist<'a>(
                &'a self,
                _: &str,
                _: &str,
            ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn tracks_by_name<'a>(&'a self, _: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn track_count(&self) -> usize {
                0
            }
            fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
                Box::new(std::iter::empty())
            }
            fn music_folder(&self) -> Option<&str> {
                None
            }
        }

        let mut app = App::new();
        app.library = Some(Box::new(OneArtist));
        // refresh_sidebar clears track_list, so seed it afterwards.
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.track_list = vec![TrackInfo::new(
            "Bohemian Rhapsody".into(),
            "Queen".into(),
            "A Night at the Opera".into(),
            None,
            None,
            Some("/music/queen/bohemian.mp3".into()),
            None,
            None,
            None,
            false,
        )];
        app.add_all_visible_to_queue();

        assert_eq!(app.sync.queue.len(), 1);
        let label = &app.sync.queue[0].label;
        assert!(
            !label.contains("tracks"),
            "label must not include track count, got {label:?}",
        );
        assert!(
            !label.contains("(1"),
            "label must not include parenthesised count, got {label:?}",
        );
    }

    #[test]
    fn execute_sync_all_on_device_dispatches_with_overwrite_targets() {
        // When every queued track matches an on-device copy, we still
        // dispatch — each item carries the target so the worker overwrites.
        // No more "nothing to do" toast; the user gets the refresh they
        // asked for by re-queuing.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/Bohemian Rhapsody.mp3".into(),
                object_id: 42,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);
        app.sync.queue.push(QueuedItem {
            label: "test".into(),
            tracks: vec![SyncItem {
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                name: "Bohemian Rhapsody".into(),
                location: "/music/queen/bohemian.mp3".into(),
                ..Default::default()
            }],
        });

        let (tx, rx) = mpsc::channel();
        app.execute_sync(&tx);

        match rx.try_recv() {
            Ok(BgCommand::ExecuteSyncQueue(items)) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].overwrite_targets.len(), 1);
                assert_eq!(items[0].overwrite_targets[0].1, 42);
            }
            Ok(_) => panic!("expected ExecuteSyncQueue"),
            Err(_) => panic!("no command dispatched — sync should always proceed now"),
        }
    }

    #[test]
    fn move_up_down_bounds() {
        let mut app = App::new();
        app.sidebar_items = vec![
            SidebarEntry::Artist("A".into()),
            SidebarEntry::Artist("B".into()),
            SidebarEntry::Artist("C".into()),
        ];
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
            ..Default::default()
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
                ..Default::default()
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
    fn add_indexed_track_updates_all_structures() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry(
            "Radiohead/OK Computer/01 Airbag.mp3",
            1234,
        ));

        assert_eq!(device.artists, vec!["Radiohead"]);
        assert_eq!(
            device.albums.get("Radiohead").unwrap(),
            &vec!["OK Computer".to_string()]
        );
        let tracks = device
            .album_tracks
            .get(&("Radiohead".into(), "OK Computer".into()))
            .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "01 Airbag");
        assert_eq!(
            tracks[0].device_path,
            "/Music/Radiohead/OK Computer/01 Airbag.mp3"
        );

        // Both the raw and track-number-stripped forms should be in track_set.
        assert!(is_on_device(
            "Radiohead",
            "01 Airbag",
            &with_connected(&device)
        ));
        assert!(is_on_device(
            "Radiohead",
            "Airbag",
            &with_connected(&device)
        ));
    }

    #[test]
    fn add_indexed_track_inserts_artists_sorted() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Zebra/A/t.mp3", 1));
        device.add_indexed_track(&make_device_entry("Alpha/A/t.mp3", 1));
        device.add_indexed_track(&make_device_entry("Mango/A/t.mp3", 1));

        assert_eq!(device.artists, vec!["Alpha", "Mango", "Zebra"]);
    }

    #[test]
    fn add_indexed_track_inserts_albums_sorted_and_deduped() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Artist/Zed/t1.mp3", 1));
        device.add_indexed_track(&make_device_entry("Artist/Alpha/t2.mp3", 1));
        device.add_indexed_track(&make_device_entry("Artist/Alpha/t3.mp3", 1));

        assert_eq!(
            device.albums.get("Artist").unwrap(),
            &vec!["Alpha".to_string(), "Zed".to_string()]
        );
    }

    #[test]
    fn add_indexed_track_is_idempotent_on_same_path() {
        let mut device = DeviceState::new();
        let entry = make_device_entry("Artist/Album/song.mp3", 100);
        device.add_indexed_track(&entry);
        device.add_indexed_track(&entry);

        assert_eq!(device.artists, vec!["Artist".to_string()]);
        assert_eq!(
            device.albums.get("Artist").unwrap(),
            &vec!["Album".to_string()]
        );
        let tracks = device
            .album_tracks
            .get(&("Artist".into(), "Album".into()))
            .unwrap();
        assert_eq!(tracks.len(), 1);
        let artist_key = normalize_for_match("Artist");
        let names = device.artist_track_names.get(&artist_key).unwrap();
        assert_eq!(names.len(), 1);
        // A single remove should fully clear the index — no duplicate ghost
        // entries left behind.
        assert!(device.remove_indexed_track("Artist/Album/song.mp3"));
        assert!(device.artists.is_empty());
        assert!(device.album_tracks.is_empty());
    }

    #[test]
    fn add_indexed_track_skips_directories() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&DeviceEntry {
            object_id: 1,
            storage_id: 65537,
            format: "Association".to_string(),
            size: 0,
            name: "Music".to_string(),
            ..Default::default()
        });
        assert!(device.artists.is_empty());
        assert!(device.album_tracks.is_empty());
    }

    #[test]
    fn remove_indexed_track_removes_album_and_artist_when_last() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Artist/Album/song.mp3", 100));
        device.add_indexed_track(&make_device_entry("Artist/Album/other.mp3", 200));

        assert!(device.remove_indexed_track("Artist/Album/song.mp3"));
        // Album still present — "other.mp3" remains.
        assert!(device
            .album_tracks
            .contains_key(&("Artist".into(), "Album".into())));
        assert_eq!(device.artists, vec!["Artist"]);

        assert!(device.remove_indexed_track("Artist/Album/other.mp3"));
        // Now album empty, artist pruned.
        assert!(device.album_tracks.is_empty());
        assert!(device.albums.is_empty());
        assert!(device.artists.is_empty());
    }

    #[test]
    fn remove_indexed_track_keeps_artist_when_other_album_remains() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Artist/AlbumA/a.mp3", 1));
        device.add_indexed_track(&make_device_entry("Artist/AlbumB/b.mp3", 1));

        assert!(device.remove_indexed_track("Artist/AlbumA/a.mp3"));

        assert_eq!(device.artists, vec!["Artist"]);
        assert_eq!(
            device.albums.get("Artist").unwrap(),
            &vec!["AlbumB".to_string()]
        );
        assert!(!device
            .album_tracks
            .contains_key(&("Artist".into(), "AlbumA".into())));
    }

    #[test]
    fn remove_indexed_track_clears_lookup_sets() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Artist/Album/song.mp3", 1));
        assert!(is_on_device("Artist", "song", &with_connected(&device)));

        device.remove_indexed_track("Artist/Album/song.mp3");
        assert!(!is_on_device("Artist", "song", &with_connected(&device)));
    }

    #[test]
    fn remove_indexed_track_returns_false_for_missing_path() {
        let mut device = DeviceState::new();
        device.add_indexed_track(&make_device_entry("Artist/Album/song.mp3", 1));

        assert!(!device.remove_indexed_track("Artist/Album/ghost.mp3"));
        assert!(!device.remove_indexed_track("Nobody/Nowhere/x.mp3"));
        // Existing track still there.
        assert_eq!(device.artists, vec!["Artist"]);
    }

    #[test]
    fn incremental_then_full_rebuild_produces_same_shape() {
        let entries = vec![
            make_device_entry("Beatles/Revolver/01 Taxman.mp3", 1),
            make_device_entry("Beatles/Revolver/02 Eleanor Rigby.mp3", 1),
            make_device_entry("Beatles/Abbey Road/01 Come Together.mp3", 1),
            make_device_entry("Radiohead/OK Computer/01 Airbag.mp3", 1),
        ];

        let mut incremental = DeviceState::new();
        for e in &entries {
            incremental.add_indexed_track(e);
        }

        let mut app = App::new();
        app.device.tracks = entries;
        app.build_device_index();

        assert_eq!(incremental.artists, app.device.artists);
        assert_eq!(incremental.albums, app.device.albums);
        assert_eq!(incremental.album_tracks, app.device.album_tracks);
        assert_eq!(incremental.track_set, app.device.track_set);
    }

    /// Helper: clone the device state and mark it Connected so is_on_device works.
    fn with_connected(device: &DeviceState) -> DeviceState {
        DeviceState {
            status: DeviceStatus::Connected,
            name: None,
            firmware: None,
            serial: None,
            manufacturer: None,
            model: None,
            usb_mode: None,
            family: None,
            storage: None,
            tracks: device.tracks.clone(),
            loading_tracks: false,
            selected: 0,
            artists: device.artists.clone(),
            albums: device.albums.clone(),
            album_tracks: device.album_tracks.clone(),
            track_set: device.track_set.clone(),
            artist_track_names: device.artist_track_names.clone(),
            acquired_items: 0,
            sync_status: None,
        }
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
        app.sidebar_items = vec![SidebarEntry::Artist("Art".into())];
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
            SidebarEntry::Artist("Apple".into()),
            SidebarEntry::Artist("Avocado".into()),
            SidebarEntry::Artist("Banana".into()),
            SidebarEntry::Artist("Cherry".into()),
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

    struct EmptyLibrary;
    impl zytunes::library::MusicLibrary for EmptyLibrary {
        fn artists(&self) -> Vec<&str> {
            Vec::new()
        }
        fn albums(&self) -> Vec<(&str, &str)> {
            Vec::new()
        }
        fn artist_tracks<'a>(
            &'a self,
            _: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn album_tracks<'a>(
            &'a self,
            _: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn album_tracks_by_artist<'a>(
            &'a self,
            _: &str,
            _: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn tracks_by_name<'a>(
            &'a self,
            _: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn track_count(&self) -> usize {
            0
        }
        fn all_tracks(&self) -> Box<dyn Iterator<Item = &zytunes::library::Track> + '_> {
            Box::new(std::iter::empty())
        }
        fn music_folder(&self) -> Option<&str> {
            None
        }
    }

    fn make_minimal_library() -> Box<dyn zytunes::library::MusicLibrary + Send> {
        Box::new(EmptyLibrary)
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
    fn scan_progress_event_sets_phrase_and_counts() {
        use zytunes::dirlib::{ScanProgress, TrackSample};
        let mut app = App::new();
        app.loading_library = true;

        app.handle_bg_event(BgEvent::LibraryScanProgress(ScanProgress {
            completed: 7,
            total: 42,
            sample: Some(TrackSample {
                artist: "Radiohead".into(),
                album: "OK Computer".into(),
                name: "Karma Police".into(),
            }),
        }));

        assert_eq!(app.scan_progress, Some((7, 42)));
        let phrase = app.scan_phrase.as_ref().expect("phrase should be set");
        // The phrase must reference one of the sample fields.
        assert!(
            phrase.contains("Radiohead")
                || phrase.contains("OK Computer")
                || phrase.contains("Karma Police"),
            "phrase {phrase:?} did not reference any sample field"
        );
    }

    #[test]
    fn library_loaded_clears_scan_state() {
        use zytunes::dirlib::{ScanProgress, TrackSample};
        let mut app = App::new();
        app.loading_library = true;
        app.handle_bg_event(BgEvent::LibraryScanProgress(ScanProgress {
            completed: 1,
            total: 1,
            sample: Some(TrackSample {
                artist: "A".into(),
                album: "B".into(),
                name: "C".into(),
            }),
        }));
        assert!(app.scan_phrase.is_some());
        assert!(app.scan_progress.is_some());

        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(make_minimal_library())));

        assert!(app.scan_phrase.is_none());
        assert!(app.scan_progress.is_none());
        assert!(app.scan_samples.is_empty());
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
            skipped: 0,
        });
        assert_eq!(app.sync.status, SyncStatus::Idle);
        assert!(app.sync.queue.is_empty());
        assert_eq!(app.sync.queue_selected, 0);
        // failed > 0 means toast is_error
        let (_, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(is_error);
    }

    #[test]
    fn handle_bg_event_sync_complete_with_skipped_shows_error_toast() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::SyncComplete {
            success: 23,
            failed: 1,
            skipped: 2,
        });
        let (msg, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(msg.contains("23 done"));
        assert!(msg.contains("2 skipped"));
        assert!(msg.contains("1 failed"));
        assert!(is_error, "skipped > 0 must flag as error");
    }

    #[test]
    fn handle_bg_event_sync_complete_no_skipped_hides_skipped_field() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::SyncComplete {
            success: 5,
            failed: 0,
            skipped: 0,
        });
        let (msg, _, is_error) = app.toast_message.as_ref().unwrap();
        assert!(!msg.contains("skipped"));
        assert!(!is_error);
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
        app.theme = &crate::theme::THEMES[3];
        app.open_theme_picker();
        app.theme_picker_move(2); // changes preview
        app.theme_picker_cancel();
        assert_eq!(app.theme.name, crate::theme::THEMES[3].name);
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
            SidebarEntry::Artist("Beatles".into()),
            SidebarEntry::Artist("Beach Boys".into()),
            SidebarEntry::Artist("Radiohead".into()),
            SidebarEntry::Artist("Rolling Stones".into()),
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
        assert!(app
            .sidebar_items
            .contains(&SidebarEntry::Artist("Beatles".into())));
        assert!(app
            .sidebar_items
            .contains(&SidebarEntry::Artist("Beach Boys".into())));
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

    /// Helper to populate the precomputed match sets on DeviceState from a
    /// pre-seeded `album_tracks` map.
    fn build_match_sets(device: &mut DeviceState) {
        device.track_set.clear();
        device.artist_track_names.clear();
        let keys: Vec<String> = device
            .album_tracks
            .values()
            .flatten()
            .map(|dt| dt.artist.clone())
            .collect();
        for artist in keys {
            device.rebuild_lookup_for_artist(&artist);
        }
    }

    #[test]
    fn is_on_device_marks_matching_tracks() {
        let mut device = DeviceState::new();
        device.status = DeviceStatus::Connected;
        device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "01 Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/01 Bohemian Rhapsody.mp3".into(),
                object_id: 1,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut device);

        assert!(
            is_on_device("Queen", "Bohemian Rhapsody", &device),
            "Bohemian Rhapsody should be on device"
        );
        assert!(
            !is_on_device("Queen", "Somebody to Love", &device),
            "Somebody to Love should not be on device"
        );
    }

    #[test]
    fn is_on_device_false_when_disconnected() {
        let device = DeviceState::new(); // status = Disconnected
        assert!(
            !is_on_device("Artist", "Test", &device),
            "should return false when disconnected"
        );
    }

    #[test]
    fn sidebar_entry_display_matches_legacy_stitched_form() {
        // Legacy string format used " — " (em-dash) between artist and album.
        // Existing persisted preferences and UI snapshots rely on exactly that
        // shape, so the structured display must reproduce it verbatim.
        let a = SidebarEntry::Artist("Queen".into());
        assert_eq!(a.display(), "Queen");
        assert_eq!(format!("{}", a), "Queen");
        let alb = SidebarEntry::Album {
            artist: "Queen".into(),
            album: "A Night at the Opera".into(),
        };
        assert_eq!(alb.display(), "Queen \u{2014} A Night at the Opera");
        assert_eq!(format!("{}", alb), "Queen \u{2014} A Night at the Opera");
    }

    #[test]
    fn sidebar_entry_lowercase_key_album_matches_either_side() {
        // The cached lowercase key powers the `/` search filter. Album entries
        // must let a substring query match either artist or album, but not
        // span the separator — otherwise "he\nok" would match "Radiohead\nOK"
        // by accident.
        let a = SidebarEntry::Album {
            artist: "Radiohead".into(),
            album: "OK Computer".into(),
        };
        let lc = a.lowercase_key();
        assert!(lc.contains("radio"), "matches on artist");
        assert!(lc.contains("computer"), "matches on album");
        assert!(!lc.contains("idioteque"));
        assert!(
            !lc.contains("headok"),
            "match must not span artist/album boundary"
        );
    }

    #[test]
    fn sort_album_tracks_orders_by_disc_then_track_number() {
        // Album view must render tracks in tagged order (disc, then track
        // number) regardless of whether they came from the library or the
        // device index. Missing numbers fall to the bottom so tagged tracks
        // keep their album order.
        fn ti(name: &str, disc: Option<u32>, track: Option<u32>) -> TrackInfo {
            TrackInfo::new(
                name.into(),
                "Artist".into(),
                "Album".into(),
                None,
                None,
                None,
                track,
                disc,
                None,
                false,
            )
        }
        let mut tracks = vec![
            ti("Disc2 Track2", Some(2), Some(2)),
            ti("No Number", None, None),
            ti("Disc1 Track3", Some(1), Some(3)),
            ti("Disc1 Track1", Some(1), Some(1)),
            ti("Disc2 Track1", Some(2), Some(1)),
        ];
        sort_album_tracks(&mut tracks);
        let order: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "Disc1 Track1",
                "Disc1 Track3",
                "Disc2 Track1",
                "Disc2 Track2",
                "No Number",
            ]
        );
    }

    #[test]
    fn search_filter_reuses_cached_lowercase() {
        // Verify the two-tier sidebar design: rebuild_sidebar_source populates
        // the full list once, apply_sidebar_filter builds the visible view
        // from the cached lowercase form without re-allocating per row.
        let mut app = App::new();
        app.browse_mode = BrowseMode::Device;
        app.device.artists = vec!["Beatles".into(), "Radiohead".into(), "Zero 7".into()];
        app.sidebar_mode = SidebarMode::Artists;
        app.refresh_sidebar();
        assert_eq!(app.sidebar_items.len(), 3);
        assert_eq!(app.sidebar_items_full.len(), 3);
        assert_eq!(app.sidebar_lowercase_full.len(), 3);

        app.search_active = true;
        app.search_query = "HEAD".into();
        app.apply_sidebar_filter();
        assert_eq!(app.sidebar_items.len(), 1);
        assert!(matches!(&app.sidebar_items[0], SidebarEntry::Artist(a) if a == "Radiohead"));
        // Full list is untouched — cheap re-filter is possible on next keystroke.
        assert_eq!(app.sidebar_items_full.len(), 3);

        app.search_query.clear();
        app.apply_sidebar_filter();
        assert_eq!(app.sidebar_items.len(), 3);
    }

    #[test]
    fn sidebar_entry_nav_key_is_artist_for_both_variants() {
        let a = SidebarEntry::Artist("Beatles".into());
        let alb = SidebarEntry::Album {
            artist: "Beatles".into(),
            album: "Revolver".into(),
        };
        // First-letter jump navigation must behave the same whether we're in
        // Artists or Albums sidebar mode — both key off the artist name.
        assert_eq!(a.nav_key(), "Beatles");
        assert_eq!(alb.nav_key(), "Beatles");
    }

    #[test]
    fn track_info_new_precomputes_match_keys() {
        let ti = TrackInfo::new(
            "  The Song  ".into(),
            "*NSYNC".into(),
            "Album".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );
        // Keys should be normalized (lowercased, edge-stripped) so device
        // lookup runs against the same form used in `track_set`.
        assert_eq!(ti.artist_key, normalize_for_match("*NSYNC"));
        assert_eq!(ti.name_key, normalize_for_match("  The Song  "));
    }

    #[test]
    fn retag_on_device_uses_precomputed_keys() {
        // Populate device state so "Queen / Bohemian Rhapsody" matches.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.album_tracks.insert(
            ("Queen".into(), "A Night at the Opera".into()),
            vec![DeviceTrackInfo {
                name: "01 Bohemian Rhapsody".into(),
                device_path: "/Music/Queen/A Night at the Opera/01 Bohemian Rhapsody.mp3".into(),
                object_id: 1,
                artist: "Queen".into(),
                album: "A Night at the Opera".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        // Two tracks: one should match, one should not.
        app.track_list = vec![
            TrackInfo::new(
                "Bohemian Rhapsody".into(),
                "Queen".into(),
                "A Night at the Opera".into(),
                None,
                None,
                None,
                None,
                None,
                None,
                false,
            ),
            TrackInfo::new(
                "Somebody to Love".into(),
                "Queen".into(),
                "A Day at the Races".into(),
                None,
                None,
                None,
                None,
                None,
                None,
                false,
            ),
        ];
        app.retag_on_device();

        assert!(
            app.track_list[0].on_device,
            "precomputed keys should match Bohemian Rhapsody"
        );
        assert!(
            !app.track_list[1].on_device,
            "precomputed keys should miss Somebody to Love"
        );
    }

    #[test]
    fn is_on_device_normalizes_special_chars() {
        let mut device = DeviceState::new();
        device.status = DeviceStatus::Connected;
        device.album_tracks.insert(
            ("NSYNC".into(), "No Strings Attached".into()),
            vec![DeviceTrackInfo {
                name: "Bye Bye Bye".into(),
                device_path: "/Music/NSYNC/No Strings Attached/Bye Bye Bye.mp3".into(),
                object_id: 10,
                artist: "NSYNC".into(),
                album: "No Strings Attached".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut device);

        assert!(
            is_on_device("*NSYNC", "Bye Bye Bye", &device),
            "*NSYNC should match NSYNC on device"
        );
    }

    #[test]
    fn rebuild_device_status_tags_artists_and_albums() {
        struct MockLib {
            tracks: Vec<zytunes::library::Track>,
        }
        impl zytunes::library::MusicLibrary for MockLib {
            fn artists(&self) -> Vec<&str> {
                Vec::new()
            }
            fn albums(&self) -> Vec<(&str, &str)> {
                Vec::new()
            }
            fn artist_tracks<'a>(
                &'a self,
                _: &str,
            ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn album_tracks<'a>(
                &'a self,
                _: &str,
            ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn album_tracks_by_artist<'a>(
                &'a self,
                _: &str,
                _: &str,
            ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn tracks_by_name<'a>(
                &'a self,
                _: &str,
            ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
                Box::new(std::iter::empty())
            }
            fn track_count(&self) -> usize {
                self.tracks.len()
            }
            fn all_tracks(&self) -> Box<dyn Iterator<Item = &zytunes::library::Track> + '_> {
                Box::new(self.tracks.iter())
            }
            fn music_folder(&self) -> Option<&str> {
                None
            }
        }

        fn track(id: u64, artist: &str, album: &str, name: &str) -> zytunes::library::Track {
            zytunes::library::Track {
                id,
                name: name.into(),
                artist: artist.into(),
                album: album.into(),
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            }
        }

        // Library: Radiohead has two albums, Bjork one; only one Radiohead
        // album is fully on device, the other is partial, Bjork's is absent.
        let lib = MockLib {
            tracks: vec![
                track(1, "Radiohead", "OK Computer", "Airbag"),
                track(2, "Radiohead", "OK Computer", "Karma Police"),
                track(3, "Radiohead", "Kid A", "Idioteque"),
                track(4, "Radiohead", "Kid A", "The National Anthem"),
                track(5, "Bjork", "Post", "Army of Me"),
            ],
        };

        let mut app = App::new();
        app.library = Some(Box::new(lib));
        app.device.status = DeviceStatus::Connected;
        // Put both OK Computer tracks + one Kid A track on device.
        app.device.album_tracks.insert(
            ("Radiohead".into(), "OK Computer".into()),
            vec![
                DeviceTrackInfo {
                    name: "Airbag".into(),
                    device_path: "/Music/Radiohead/OK Computer/Airbag.mp3".into(),
                    object_id: 1,
                    artist: "Radiohead".into(),
                    album: "OK Computer".into(),
                    track_number: None,
                    disc_number: None,
                },
                DeviceTrackInfo {
                    name: "Karma Police".into(),
                    device_path: "/Music/Radiohead/OK Computer/Karma Police.mp3".into(),
                    object_id: 2,
                    artist: "Radiohead".into(),
                    album: "OK Computer".into(),
                    track_number: None,
                    disc_number: None,
                },
            ],
        );
        app.device.album_tracks.insert(
            ("Radiohead".into(), "Kid A".into()),
            vec![DeviceTrackInfo {
                name: "Idioteque".into(),
                device_path: "/Music/Radiohead/Kid A/Idioteque.mp3".into(),
                object_id: 3,
                artist: "Radiohead".into(),
                album: "Kid A".into(),
                track_number: None,
                disc_number: None,
            }],
        );
        build_match_sets(&mut app.device);

        app.rebuild_artist_device_status();

        assert_eq!(
            app.artist_device_status.get("Radiohead"),
            Some(&DevicePresence::Partial),
            "Radiohead should be Partial (3 of 4 on device)",
        );
        assert!(
            !app.artist_device_status.contains_key("Bjork"),
            "Bjork should have no entry (nothing on device)",
        );
        assert_eq!(
            app.album_device_status
                .get(&("Radiohead".into(), "OK Computer".into())),
            Some(&DevicePresence::Full),
            "OK Computer should be Full",
        );
        assert_eq!(
            app.album_device_status
                .get(&("Radiohead".into(), "Kid A".into())),
            Some(&DevicePresence::Partial),
            "Kid A should be Partial",
        );
        assert!(
            !app.album_device_status
                .contains_key(&("Bjork".into(), "Post".into())),
            "Post should not be tracked (nothing on device)",
        );
    }

    #[test]
    fn album_art_style_from_str_round_trip() {
        for s in ["ascii", "halfblock"] {
            let style: AlbumArtStyle = s.parse().expect("valid style");
            assert_eq!(style.as_str(), s);
        }
        assert!("bogus".parse::<AlbumArtStyle>().is_err());
    }

    #[test]
    fn flip_art_style_in_memory_flips_and_clears_caches() {
        let mut app = App::new();
        app.album_art_style = AlbumArtStyle::Halfblock;
        app.album_art_cache = Some(AlbumArtCache::Halfblock(vec![vec![(
            '▀',
            [1, 2, 3],
            [4, 5, 6],
        )]]));
        app.album_art_size = (80, 24);

        app.flip_art_style_in_memory();
        assert_eq!(app.album_art_style, AlbumArtStyle::Ascii);
        assert!(app.album_art_cache.is_none());
        assert_eq!(app.album_art_size, (0, 0));

        // Seed the ascii variant and flip back; covers the other match arm.
        app.album_art_cache = Some(AlbumArtCache::Ascii(vec![vec![('@', [0, 0, 0])]]));
        app.flip_art_style_in_memory();
        assert_eq!(app.album_art_style, AlbumArtStyle::Halfblock);
        assert!(app.album_art_cache.is_none());
    }

    #[test]
    fn render_album_art_populates_only_active_cache() {
        use image::{DynamicImage, RgbaImage};

        let mut app = App::new();
        // Tiny gradient test image so the renderer has pixels to sample.
        let mut img = RgbaImage::new(8, 8);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x + y) * 16).min(255) as u8;
            *px = image::Rgba([v, v, v, 255]);
        }
        app.album_art = Some(DynamicImage::ImageRgba8(img));

        app.album_art_style = AlbumArtStyle::Halfblock;
        app.render_album_art(16, 8);
        assert!(matches!(
            app.album_art_cache,
            Some(AlbumArtCache::Halfblock(_))
        ));

        // Toggling should invalidate the cache so the other renderer fills its buffer.
        app.album_art_style = AlbumArtStyle::Ascii;
        app.album_art_cache = None;
        app.album_art_size = (0, 0);
        app.render_album_art(16, 8);
        let Some(AlbumArtCache::Ascii(rows)) = app.album_art_cache.as_ref() else {
            panic!("expected Ascii cache");
        };

        // Every glyph should belong to the ramp.
        let ramp: &[u8] = ASCII_ART_RAMP;
        for row in rows {
            for &(ch, _) in row {
                assert!(ramp.contains(&(ch as u8)), "unexpected glyph {:?}", ch);
            }
        }
    }

    #[test]
    fn should_show_player_respects_playback_state() {
        let mut app = App::new();
        app.show_player = None;
        // No now_playing — panel stays hidden regardless of pref or height.
        assert!(!app.should_show_player(100));
        app.show_player = Some(true);
        assert!(!app.should_show_player(100));
    }

    #[test]
    fn should_show_player_auto_uses_height_floor() {
        let mut app = App::new();
        app.now_playing = Some(NowPlaying {
            track_name: "t".into(),
            artist: "a".into(),
            album: "al".into(),
            duration_ms: 0,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist: Arc::from([] as [TrackInfo; 0]),
            paused_frame: None,
        });
        app.show_player = None;
        assert!(!app.should_show_player(19));
        assert!(app.should_show_player(20));
    }

    #[test]
    fn should_show_player_force_hide_always_wins() {
        let mut app = App::new();
        app.now_playing = Some(NowPlaying {
            track_name: "t".into(),
            artist: "a".into(),
            album: "al".into(),
            duration_ms: 0,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist: Arc::from([] as [TrackInfo; 0]),
            paused_frame: None,
        });
        app.show_player = Some(false);
        assert!(!app.should_show_player(100));
    }

    #[test]
    fn should_show_player_force_show_ignores_height() {
        // Force-show lets users see the panel on short terminals where auto
        // would hide it. LayoutMetrics enforces the absolute floor.
        let mut app = App::new();
        app.now_playing = Some(NowPlaying {
            track_name: "t".into(),
            artist: "a".into(),
            album: "al".into(),
            duration_ms: 0,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist: Arc::from([] as [TrackInfo; 0]),
            paused_frame: None,
        });
        app.show_player = Some(true);
        assert!(app.should_show_player(12));
    }

    #[test]
    fn cycle_show_player_in_memory_rotates() {
        let mut app = App::new();
        app.show_player = None;
        assert_eq!(app.cycle_show_player_in_memory(), "Player: hidden");
        assert_eq!(app.show_player, Some(false));
        assert_eq!(app.cycle_show_player_in_memory(), "Player: always on");
        assert_eq!(app.show_player, Some(true));
        assert_eq!(app.cycle_show_player_in_memory(), "Player: auto");
        assert_eq!(app.show_player, None);
    }
}
