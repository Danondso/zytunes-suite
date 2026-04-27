use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use image::DynamicImage;
use throbber_widgets_tui::ThrobberState;
use zytunes::dirlib::TrackSample;
use zytunes::library::{MusicLibrary, Track};
use zytunes::mtp::parse::DeviceEntry;

use std::path::PathBuf;

use crate::audio::{AudioCommand, AudioEvent};
use crate::background::{BgCommand, BgEvent, StorageInfo, SyncItem};
use crate::theme::{all_themes, Theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zytunes::listen_log::{ListenEvent, ListenLog};
use zytunes::local_plays::{self, LocalPlays};
use zytunes::playlist::{
    self as playlist_mod, GenerationParams, Playlist, PlaylistKind, SeedStrategy,
};
use zytunes::playlist_store::{self, PlaylistStore};
use zytunes::recommender::{BigramTable, Recommender, WeightedRecommender};

/// Result of dispatching a single keyboard event through `App::handle_key`.
/// `Quit` signals the run loop to break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    Continue,
    Quit,
}

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
    Playlists,
}

/// A single sidebar row, either a bare artist entry, a paired
/// (artist, album) entry, or a playlist row. Storing the parts structured
/// avoids the previous `format!("{} — {}")` / `split_once(" — ")` round-trip
/// that every lookup had to unpack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEntry {
    Artist(String),
    Album {
        artist: String,
        album: String,
    },
    /// Playlist entry: `id` is the stable `Playlist::id` for lookup, `name`
    /// is cached for display. Renamed playlists rebuild the sidebar source so
    /// `name` stays in sync without per-frame store reads.
    Playlist {
        id: u64,
        name: String,
    },
}

impl SidebarEntry {
    /// Human-readable label for rendering (also used as the queue-item label).
    pub fn display(&self) -> std::borrow::Cow<'_, str> {
        match self {
            SidebarEntry::Artist(a) => std::borrow::Cow::Borrowed(a.as_str()),
            SidebarEntry::Album { artist, album } => {
                std::borrow::Cow::Owned(format!("{} \u{2014} {}", artist, album))
            }
            SidebarEntry::Playlist { name, .. } => std::borrow::Cow::Borrowed(name.as_str()),
        }
    }

    /// Key used for first-letter jump navigation. Artist/album variants key
    /// off the artist name so navigation is uniform across sidebar modes;
    /// playlist rows key off the playlist name.
    pub fn nav_key(&self) -> &str {
        match self {
            SidebarEntry::Artist(a) => a.as_str(),
            SidebarEntry::Album { artist, .. } => artist.as_str(),
            SidebarEntry::Playlist { name, .. } => name.as_str(),
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
            SidebarEntry::Playlist { name, .. } => name.to_lowercase(),
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
            SidebarEntry::Playlist { name, .. } => f.write_str(name),
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
    /// How many times the device has played this track. `None` when the
    /// device family doesn't surface a playcount or the source is still
    /// pending implementation (Zune ZMDB extension awaits Phase 4b probe
    /// results). UI renders `—` for `None`, the digit for `Some`.
    pub play_count: Option<u32>,
    /// User-set rating from the device (MTP `0xDC8A`, range 0–100).
    /// `None` if the device doesn't surface ratings.
    pub rating: Option<u16>,
    /// How many times the device has skipped this track (MTP `0xDC92`).
    /// `None` when the device family doesn't surface a skip count. Carried
    /// here so device-mode rows that lack a library counterpart can still
    /// surface the device-side skip count (parity with `play_count`).
    pub skip_count: Option<u32>,
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

/// Per-track on-device metadata cached for fast Library-side lookup.
/// Mirrors the equivalent fields on `DeviceTrackInfo` and `DeviceEntry`.
pub type DeviceTrackMeta = (Option<u32>, Option<u16>);

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
    /// Library `Track::id` resolved at play time; `None` when the playing
    /// row has no library match (device-mode play, scratch file). Used to
    /// key the local-plays sidecar — when `None` no play/skip is recorded.
    pub track_id: Option<u64>,
    /// Set true once this playback session has been recorded as either a
    /// play or a skip. Idempotency guard so multiple Position events past
    /// the threshold count exactly once.
    pub counted: bool,
    /// Album year resolved from the library `Track` at play time. `None`
    /// when the row has no library counterpart or the tag is missing.
    pub year: Option<u32>,
    /// Pre-formatted marquee line of extended metadata (genre, BPM, key,
    /// bitrate, sample rate, file size, …) joined by ` | `. Built once at
    /// play time so per-frame rendering stays cheap. Empty string when no
    /// metadata is available.
    pub metadata_marquee: String,
}

/// Extract the year and assemble a `|`-separated metadata marquee from a
/// library `Track`. Empty inputs (or `None`) produce `(None, "".into())`.
/// Each segment is only added when the underlying tag is present, so a
/// lightly-tagged file produces a short line and a fully-tagged one runs
/// long enough to scroll. Order is roughly "audible / musical → technical
/// → credits → file" so the most relevant info passes first.
pub fn build_now_playing_metadata(track: Option<&Track>) -> (Option<u32>, String) {
    let Some(t) = track else {
        return (None, String::new());
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = &t.genre {
        parts.push(format!("Genre: {}", g));
    }
    if let Some(b) = t.bpm {
        parts.push(format!("BPM: {}", b));
    }
    if let Some(k) = &t.initial_key {
        parts.push(format!("Key: {}", k));
    }
    if let Some(m) = &t.mood {
        parts.push(format!("Mood: {}", m));
    }
    if let Some(br) = t.audio_bitrate_kbps {
        parts.push(format!("{} kbps", br));
    }
    if let Some(sr) = t.sample_rate {
        parts.push(format!("{:.1} kHz", sr as f64 / 1000.0));
    }
    if let Some(bd) = t.bit_depth {
        parts.push(format!("{}-bit", bd));
    }
    if let Some(ch) = t.channels {
        let label = match ch {
            1 => "Mono".to_string(),
            2 => "Stereo".to_string(),
            n => format!("{}ch", n),
        };
        parts.push(label);
    }
    if let Some(aa) = &t.album_artist {
        if !aa.eq_ignore_ascii_case(&t.artist) {
            parts.push(format!("Album Artist: {}", aa));
        }
    }
    if let Some(c) = &t.composer {
        parts.push(format!("Composer: {}", c));
    }
    if let Some(p) = &t.publisher {
        parts.push(format!("Publisher: {}", p));
    }
    if let Some(s) = t.file_size_bytes {
        parts.push(format_bytes_compact(s));
    }
    if let Some(e) = &t.encoder {
        parts.push(format!("Encoder: {}", e));
    }
    (t.year, parts.join(" | "))
}

/// Compact human-readable byte size used in the now-playing marquee.
/// Matches the rendering convention in `tui::ui::format_bytes`; kept local
/// here so `app.rs` does not need a back-edge into the renderer module.
fn format_bytes_compact(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
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
    /// Per-track metadata keyed identically to `track_set` so library-side
    /// rows can inherit on-device `play_count` and `rating` without walking
    /// `album_tracks`. Populated alongside `track_set` in
    /// `add_indexed_track`.
    pub track_metadata: HashMap<(String, String), DeviceTrackMeta>,
    /// Per-artist list of normalized device track names for substring fallback.
    pub artist_track_names: BTreeMap<String, Vec<String>>,
    /// Number of items the device acquired on its own (podcasts, Zune-to-Zune shares).
    pub acquired_items: u32,
    /// Sync progress status string from MTP vendor op 0x922f.
    pub sync_status: Option<String>,
}

impl DeviceState {
    #[allow(clippy::new_without_default)]
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
            track_metadata: HashMap::new(),
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
                play_count: entry.play_count,
                rating: entry.rating,
                skip_count: entry.skip_count,
            });

        let artist_key = normalize_for_match(&artist);
        let raw = normalize_for_match(&display_name);
        let stripped = normalize_for_match(zytunes::strip_track_number(display_name.trim()));
        self.track_set.insert((artist_key.clone(), raw.clone()));
        let meta = (entry.play_count, entry.rating);
        self.track_metadata
            .insert((artist_key.clone(), raw.clone()), meta);
        if stripped != raw {
            self.track_set
                .insert((artist_key.clone(), stripped.clone()));
            self.track_metadata
                .insert((artist_key.clone(), stripped), meta);
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

    /// Rebuild `track_set`, `track_metadata`, and `artist_track_names`
    /// entries for a single artist. Cheap because it only walks that
    /// artist's tracks, not the whole device.
    fn rebuild_lookup_for_artist(&mut self, artist: &str) {
        let artist_key = normalize_for_match(artist);
        self.track_set.retain(|(a, _)| a != &artist_key);
        self.track_metadata.retain(|(a, _), _| a != &artist_key);
        self.artist_track_names.remove(&artist_key);

        let mut names = Vec::new();
        // Collected as (key, metadata) so the post-walk insert phase can
        // populate both `track_set` and `track_metadata` consistently.
        let mut extra_entries: Vec<(String, DeviceTrackMeta)> = Vec::new();
        let mut raw_entries: Vec<(String, DeviceTrackMeta)> = Vec::new();
        for ((a, _), tracks) in &self.album_tracks {
            if normalize_for_match(a) != artist_key {
                continue;
            }
            for dt in tracks {
                let raw = normalize_for_match(&dt.name);
                let stripped = normalize_for_match(zytunes::strip_track_number(dt.name.trim()));
                let meta = (dt.play_count, dt.rating);
                if stripped != raw {
                    extra_entries.push((stripped, meta));
                }
                names.push(raw.clone());
                raw_entries.push((raw, meta));
            }
        }
        for (raw, meta) in raw_entries {
            self.track_set.insert((artist_key.clone(), raw.clone()));
            self.track_metadata.insert((artist_key.clone(), raw), meta);
        }
        for (stripped, meta) in extra_entries {
            self.track_set
                .insert((artist_key.clone(), stripped.clone()));
            self.track_metadata
                .insert((artist_key.clone(), stripped), meta);
        }
        if !names.is_empty() {
            self.artist_track_names.insert(artist_key, names);
        }
    }
}

/// Parse a device-relative track name (e.g. `"Artist/Album/01 track.mp3"`) into
/// (artist, album, display_name). Missing segments fall back to `Unknown Artist`
/// and `Unknown Album`. The display name has its file extension stripped.
/// Look up a library `Track::id` for a device-side `(artist, display_name)`
/// pair. Tries the raw name first, then the `strip_track_number`-stripped
/// form so device filenames like `"01 Smells Like Teen Spirit"` still resolve
/// to library titles like `"Smells Like Teen Spirit"`. Mirrors the matching
/// strategy used by `merge_device_plays_into_local` so the displayed
/// aggregate stays consistent with the merged sidecar.
fn resolve_library_id_for_device_track(
    lib: &dyn zytunes::library::MusicLibrary,
    artist: &str,
    display_name: &str,
) -> Option<u64> {
    let lookup = |name: &str| -> Option<u64> {
        lib.tracks_by_name(name)
            .find(|t| t.artist.eq_ignore_ascii_case(artist))
            .map(|t| t.id)
    };
    if let Some(id) = lookup(display_name) {
        return Some(id);
    }
    let stripped = zytunes::strip_track_number(display_name.trim());
    if stripped != display_name {
        return lookup(stripped);
    }
    None
}

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
    #[allow(clippy::new_without_default)]
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
    /// Track-info inspector popup. Shown only when the focused panel is
    /// `Panel::TrackList`; closed on any navigation that changes the
    /// selection (`move_up`/`move_down`, panel cycle, sidebar/album select).
    pub show_track_info: bool,
    /// Vertical scroll offset for the track-info popup body. Optimistic
    /// (incremented on key down without knowing visible height); render
    /// clamps to the actual `total - visible` upper bound.
    pub track_info_scroll: usize,
    /// Library `Track` resolved at popup-open time. Cached here so the
    /// renderer reads it directly each frame instead of re-running an O(N)
    /// `tracks_by_name` linear scan on every ~50 ms tick while the popup is
    /// visible. `None` when the popup is closed or no library row matched.
    pub track_info_lib: Option<Track>,
    /// Per-artist device presence for sidebar indicators (Library browse mode).
    pub artist_device_status: BTreeMap<String, DevicePresence>,
    /// Per-(artist, album) device presence for album-list indicators.
    pub album_device_status: BTreeMap<(String, String), DevicePresence>,
    /// Cached album art extracted from embedded tags (any lofty-supported
    /// format — MP3 / FLAC / ALAC / OGG / WMA).
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
    /// Aggregate play/skip counters across the TUI and any connected device.
    /// TUI plays bump it directly via `record_now_playing_play`/`_skip`;
    /// device-side counters merge in via `merge_device_observation` on each
    /// reconnect. Persisted to `local_plays_save_path` after every mutation.
    pub local_plays: LocalPlays,
    /// Sidecar path for `local_plays` persistence. Defaults to
    /// `~/.cache/zytunes/local-plays.json`; tests set `None` to disable disk
    /// writes or override with a temp path.
    pub local_plays_save_path: Option<PathBuf>,
    /// User-authored playlists (manual + generated). Loaded once at TUI
    /// launch via `load_playlists_from_disk`; `App::new()` leaves this empty
    /// so unit tests don't pick up developer-machine state.
    pub playlists: PlaylistStore,
    /// Sidecar path for `playlists` persistence. Defaults to
    /// `~/.config/zytunes/playlists.json`; tests set `None` to skip disk
    /// writes or pass a temp path.
    pub playlists_save_path: Option<PathBuf>,
    /// In-progress text input for the playlist-name modal (open via `N`).
    /// `Some(_)` means the modal is up and key events route to it.
    pub playlist_name_input: Option<String>,
    /// Set when the playlist-name modal is in rename mode; carries the ID
    /// being renamed. `None` means create-new.
    pub playlist_rename_target: Option<u64>,
    /// Pending playlist deletion awaiting confirmation. The confirmation
    /// overlay branch checks this before consulting `pending_removal`.
    pub pending_playlist_delete: Option<u64>,
    /// In-progress "add to playlist" picker. When `Some`, a popup lists
    /// existing playlists; up/down/enter pick one and the selected library
    /// track ID gets appended.
    pub add_to_playlist_picker: Option<AddToPlaylistPicker>,
    /// Playlist-generation form. `Some(_)` means the modal is open and key
    /// events route to it; the dispatcher gates on this exactly like
    /// `playlist_name_input`.
    pub generation_form: Option<GenerationFormState>,
    /// Playlists awaiting device-side creation, queued when the user
    /// enqueues a playlist for sync. Drained into `BgCommand::ImportPlaylist`
    /// commands once the file sync emits `BgEvent::SyncComplete` so the
    /// device-side resolver sees the freshly-uploaded tracks. Cleared on
    /// sync cancellation.
    pub pending_playlist_imports: Vec<PendingPlaylistImport>,
    /// Whether device-side playlist sync (Phase 3) is enabled. Defaults
    /// from `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1` at startup. Off after
    /// the 2026-04-26 iPod-iTunesDB-corruption incident; library-side
    /// playlists still work, only the device push is gated. Tests set
    /// this directly to avoid env-var races.
    pub experimental_playlist_sync: bool,
    /// Append-only log of every TUI play / skip. Drives the Phase 4
    /// sequence-aware recommender via `BigramTable`. Loaded from
    /// `~/.cache/zytunes/listen-log.jsonl` at startup; tests skip the
    /// disk path entirely (no `with_save_path`) so they don't pick up
    /// developer-machine state.
    pub listen_log: ListenLog,
}

/// One entry in `App::pending_playlist_imports`. Carries the playlist's
/// display name plus its ordered `(artist, album, title)` tuples so the
/// background worker can resolve them against the device's current track
/// set after the file sync lands.
#[derive(Debug, Clone)]
pub struct PendingPlaylistImport {
    pub name: String,
    pub track_keys: Vec<(String, String, String)>,
}

/// State for the "add this track to a playlist" overlay (opened with `+` on
/// a Library track row). Built when the user invokes the action; dropped on
/// confirm/cancel.
#[derive(Debug, Clone)]
pub struct AddToPlaylistPicker {
    /// Library `Track::id` being added.
    pub track_id: u64,
    /// Display name shown in the popup header.
    pub track_label: String,
    /// Snapshot of `(playlist_id, name)` at popup-open time so renames
    /// during the popup's lifetime don't shift selection.
    pub options: Vec<(u64, String)>,
    pub selected: usize,
}

/// In-progress state for the "Generate Playlist" modal.
///
/// Fields mirror [`GenerationParams`] one-to-one but live as primitives the
/// modal can edit incrementally. `commit()` (in the App impl) builds a
/// `GenerationParams` + name pair and hands it off to the recommender.
///
/// `selected_field` is the cursor — index into the field list rendered by
/// `draw_generation_form_overlay`. Tab/Shift-Tab move it. Field types know
/// what input keys mean for them (text accepts chars, radios respond to
/// up/down, sliders to `<` / `>`, checkboxes to space).
#[derive(Debug, Clone)]
pub struct GenerationFormState {
    pub name: String,
    pub seed_strategy_idx: usize, // 0=TopPlayed 1=Recent 2=Track 3=Artist 4=Genre
    pub window_days: u32,
    pub seed_count: usize,
    pub target_length: usize,
    pub max_per_artist: usize,
    pub max_per_album: usize,
    /// Slider 0..1, applied to `GenerationParams::diversity_lambda_bits`.
    pub diversity: f32,
    /// Slider 0..1, applied to `GenerationParams::novelty_bits`.
    pub novelty: f32,
    pub exclude_on_device: bool,
    /// Optional explicit context for `Track`/`Artist`/`Genre` strategies —
    /// captured at form-open time when the user invoked from a track row
    /// (`R` on a track) so the strategy can be applied without re-resolving.
    pub seed_context: SeedContext,
    /// `Some(playlist_id)` means we're regenerating that playlist in place;
    /// `None` means a brand-new generated playlist.
    pub regenerating: Option<u64>,
    pub selected_field: usize,
}

/// Captured context for non-behavioral seed strategies. Filled when the
/// form is opened with a specific seed (e.g. `R` on a track row); ignored
/// for the behavioral strategies.
#[derive(Debug, Clone, Default)]
pub struct SeedContext {
    pub track_id: Option<u64>,
    pub artist: Option<String>,
    pub genre: Option<String>,
}

impl GenerationFormState {
    /// Total form fields the cursor can land on. Kept in one place so the
    /// dispatcher and the renderer stay in sync.
    pub const FIELD_COUNT: usize = 10;
    pub const FIELD_NAME: usize = 0;
    pub const FIELD_SEED_STRATEGY: usize = 1;
    pub const FIELD_WINDOW_DAYS: usize = 2;
    pub const FIELD_SEED_COUNT: usize = 3;
    pub const FIELD_TARGET_LEN: usize = 4;
    pub const FIELD_MAX_PER_ARTIST: usize = 5;
    pub const FIELD_MAX_PER_ALBUM: usize = 6;
    pub const FIELD_DIVERSITY: usize = 7;
    pub const FIELD_NOVELTY: usize = 8;
    pub const FIELD_EXCLUDE_DEVICE: usize = 9;

    /// Build a fresh form pre-filled from the Discover Weekly defaults plus
    /// a date-stamped name. Use [`Self::for_track_seed`] when the user
    /// invoked from `R` on a track row.
    pub fn discover_weekly_default(now_ms: u64) -> Self {
        let p = GenerationParams::default_discover_weekly();
        GenerationFormState {
            name: format!("Discover Weekly · {}", short_date(now_ms)),
            seed_strategy_idx: match &p.seed_strategy {
                SeedStrategy::TopPlayed { .. } => 0,
                SeedStrategy::RecentlyPlayed { .. } => 1,
                SeedStrategy::Track(_) => 2,
                SeedStrategy::Artist(_) => 3,
                SeedStrategy::Genre(_) => 4,
            },
            window_days: 30,
            seed_count: 10,
            target_length: p.target_length,
            max_per_artist: p.max_per_artist,
            max_per_album: p.max_per_album,
            diversity: p.diversity_lambda(),
            novelty: p.novelty(),
            exclude_on_device: p.exclude_on_device,
            seed_context: SeedContext::default(),
            regenerating: None,
            selected_field: 0,
        }
    }

    /// Form pre-filled with the available seed contexts (track id, artist
    /// name, genre tag) so the user can flip between Track / Artist / Genre
    /// strategies in the form without having to re-invoke from a different
    /// row. `default_strategy_idx` picks which radio option lights up first
    /// — pass 2 for Track, 3 for Artist, 4 for Genre.
    ///
    /// `label` drives the default playlist name. Pass the track title for
    /// Track-seeded forms, the artist for Artist-seeded forms, etc.
    pub fn for_library_context(
        now_ms: u64,
        track_id: Option<u64>,
        artist: Option<String>,
        genre: Option<String>,
        default_strategy_idx: usize,
        label: &str,
    ) -> Self {
        let mut s = Self::discover_weekly_default(now_ms);
        s.seed_strategy_idx = default_strategy_idx.min(4);
        s.seed_context = SeedContext {
            track_id,
            artist,
            genre,
        };
        s.name = format!("More like {} · {}", label, short_date(now_ms));
        s
    }

    /// Convenience: the existing "more like this track" shortcut. Equivalent
    /// to `for_library_context(now, Some(id), None, None, 2, label)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn for_track_seed(now_ms: u64, track_id: u64, label: &str) -> Self {
        Self::for_library_context(now_ms, Some(track_id), None, None, 2, label)
    }

    /// Returns `Ok(())` when the currently-selected strategy has the
    /// context fields it needs to actually generate, or `Err(message)`
    /// describing what's missing. Surfaced as a toast at submit time.
    pub fn validate_strategy_context(&self) -> Result<(), &'static str> {
        match self.seed_strategy_idx {
            // Behavioural strategies — always usable.
            0 | 1 => Ok(()),
            // Track requires an explicit id.
            2 => {
                if self.seed_context.track_id.is_some() {
                    Ok(())
                } else {
                    Err("Track strategy needs a seed track — open the form from a Library track row")
                }
            }
            3 => {
                if self
                    .seed_context
                    .artist
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("Artist strategy needs an artist — open the form from a Library artist or album sidebar row")
                }
            }
            4 => {
                if self
                    .seed_context
                    .genre
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("Genre strategy needs a genre — open the form from a Library track row whose genre tag is populated")
                }
            }
            _ => Err("Unknown seed strategy"),
        }
    }

    /// Short display of the value backing the currently-selected strategy.
    /// Surfaced next to the radio in the form so the user can see what
    /// they'll generate from. `None` means "no context" — the form
    /// renderer flags this as `(none)`.
    pub fn current_context_label(&self) -> Option<String> {
        match self.seed_strategy_idx {
            0 | 1 => None,
            2 => self.seed_context.track_id.map(|id| format!("track #{id}")),
            3 => self.seed_context.artist.clone(),
            4 => self.seed_context.genre.clone(),
            _ => None,
        }
    }

    /// Form pre-filled to regenerate an existing generated playlist using
    /// its stored params.
    pub fn for_regenerate(playlist: &Playlist) -> Option<Self> {
        let (params, _gen_at) = match &playlist.kind {
            PlaylistKind::Generated {
                params,
                last_generated_ms,
            } => (params.clone(), *last_generated_ms),
            PlaylistKind::Manual => return None,
        };
        let (window_days, seed_count) = match &params.seed_strategy {
            SeedStrategy::TopPlayed { window_days, count } => (*window_days, *count),
            SeedStrategy::RecentlyPlayed { count } => (0, *count),
            _ => (0, 0),
        };
        let mut ctx = SeedContext::default();
        match &params.seed_strategy {
            SeedStrategy::Track(id) => ctx.track_id = Some(*id),
            SeedStrategy::Artist(name) => ctx.artist = Some(name.clone()),
            SeedStrategy::Genre(name) => ctx.genre = Some(name.clone()),
            _ => {}
        }
        Some(GenerationFormState {
            name: playlist.name.clone(),
            seed_strategy_idx: match &params.seed_strategy {
                SeedStrategy::TopPlayed { .. } => 0,
                SeedStrategy::RecentlyPlayed { .. } => 1,
                SeedStrategy::Track(_) => 2,
                SeedStrategy::Artist(_) => 3,
                SeedStrategy::Genre(_) => 4,
            },
            window_days,
            seed_count,
            target_length: params.target_length,
            max_per_artist: params.max_per_artist,
            max_per_album: params.max_per_album,
            diversity: params.diversity_lambda(),
            novelty: params.novelty(),
            exclude_on_device: params.exclude_on_device,
            seed_context: ctx,
            regenerating: Some(playlist.id),
            selected_field: 0,
        })
    }

    /// Compose [`GenerationParams`] from the current field values.
    pub fn to_params(&self) -> GenerationParams {
        let strategy = match self.seed_strategy_idx {
            0 => SeedStrategy::TopPlayed {
                window_days: self.window_days,
                count: self.seed_count.max(1),
            },
            1 => SeedStrategy::RecentlyPlayed {
                count: self.seed_count.max(1),
            },
            2 => SeedStrategy::Track(self.seed_context.track_id.unwrap_or(0)),
            3 => SeedStrategy::Artist(self.seed_context.artist.clone().unwrap_or_default()),
            _ => SeedStrategy::Genre(self.seed_context.genre.clone().unwrap_or_default()),
        };
        GenerationParams {
            seed_strategy: strategy,
            target_length: self.target_length.max(1),
            max_per_artist: self.max_per_artist.max(1),
            max_per_album: self.max_per_album.max(1),
            diversity_lambda_bits: self.diversity.clamp(0.0, 1.0).to_bits(),
            novelty_bits: self.novelty.clamp(0.0, 1.0).to_bits(),
            weights: zytunes::playlist::ScoringWeights::default(),
            exclude_artists: Vec::new(),
            exclude_track_ids: Vec::new(),
            exclude_on_device: self.exclude_on_device,
        }
    }
}

/// Whether the device-side playlist push (Phase 3) is enabled. Off by
/// default after the 2026-04-26 incident: writing a playlist to a real
/// iPod produced a corrupted iTunesDB. Until the write path round-trips
/// safely on hardware, this stays opt-in via
/// `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1`.
pub fn device_playlist_sync_enabled() -> bool {
    std::env::var("ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC")
        .ok()
        .as_deref()
        == Some("1")
}

/// Short YYYY-MM-DD-ish date for default playlist names. Cheap stand-alone
/// math beats pulling in `chrono` just for one stamp.
fn short_date(now_ms: u64) -> String {
    // Days since unix epoch.
    let total_days = (now_ms / 86_400_000) as i64;
    // Civil-from-days algorithm (Howard Hinnant) — accurate, no leap-year
    // edge cases, no allocations.
    let z = total_days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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
    /// Aggregate play count across the TUI and any connected devices,
    /// resolved from the local-plays sidecar via library `Track::id`. For
    /// device-mode rows that have a matching library track, also resolved
    /// from the sidecar; for device-mode rows without a library match it
    /// falls back to the raw device counter so the column never shows
    /// `None` when the device knows about plays.
    pub play_count: Option<u32>,
    /// Aggregate skip count, same provenance as `play_count`.
    pub skip_count: Option<u32>,
    /// User-set rating from the device (MTP `0xDC8A`, range 0–100, displayed
    /// as 0–5 stars by dividing by 20). Populated from `DeviceTrackInfo`.
    pub rating: Option<u16>,
    /// Unix epoch ms of the last TUI play. `None` until the user actually
    /// plays the track in this TUI — device-side plays don't fabricate a
    /// timestamp.
    pub last_played_at_ms: Option<u64>,
    /// Unix epoch ms of the most recent device-baseline sync for this
    /// track, sourced from `local_plays.device_baselines[<key>].
    /// last_synced_at_ms`. `None` for tracks that have never been observed
    /// on a device.
    pub last_synced_from_device_at_ms: Option<u64>,
    /// Library `Track::id` for rows that have a library counterpart. Cached
    /// here so the live-refresh path (`refresh_play_stats_for_id`) can locate
    /// affected rows in `track_list` after a sidecar mutation without
    /// re-running the lookup. `None` for device-only rows (sideloads,
    /// podcasts) — they don't appear in the library and don't get sidecar
    /// records, so there's nothing to refresh.
    pub library_id: Option<u64>,
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
    ///
    /// `play_count` is left at `None` here; callers building from a
    /// `DeviceTrackInfo` set it after construction. Library tracks never
    /// have a playcount (until Phase 3 wires local-play tracking).
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
            play_count: None,
            skip_count: None,
            rating: None,
            last_played_at_ms: None,
            last_synced_from_device_at_ms: None,
            library_id: None,
            artist_key,
            name_key,
        }
    }
}

impl App {
    #[allow(clippy::new_without_default)]
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
            show_track_info: false,
            track_info_scroll: 0,
            track_info_lib: None,
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
            // Disk state is loaded explicitly by `load_local_plays_from_disk`
            // so unit tests get a clean default `App` without inheriting
            // whatever sidecar exists on the developer's machine.
            local_plays_save_path: None,
            local_plays: LocalPlays::default(),
            playlists: PlaylistStore::default(),
            playlists_save_path: None,
            playlist_name_input: None,
            playlist_rename_target: None,
            pending_playlist_delete: None,
            add_to_playlist_picker: None,
            generation_form: None,
            pending_playlist_imports: Vec::new(),
            experimental_playlist_sync: device_playlist_sync_enabled(),
            listen_log: ListenLog::new(),
        }
    }

    /// Load the listen log from `~/.cache/zytunes/listen-log.jsonl` and
    /// bind future `append`s to the same path. Production code calls this
    /// once after `App::new()`; tests skip it so the in-memory log starts
    /// empty and never writes to the developer's home dir.
    pub fn load_listen_log_from_disk(&mut self) {
        self.listen_log = ListenLog::load();
    }

    /// Wire `playlists` to the on-disk file (`~/.config/zytunes/playlists.json`)
    /// and load any existing state. Production code calls this once after
    /// `App::new()`; tests skip it to avoid picking up developer-machine state.
    pub fn load_playlists_from_disk(&mut self) {
        let path = playlist_store::default_save_path();
        if let Some(p) = path.as_deref() {
            self.playlists = PlaylistStore::load_from(p);
        }
        self.playlists_save_path = path;
    }

    /// Persist the playlist store. Called after every mutation since the
    /// file is bounded by playlist count (small).
    pub fn persist_playlists(&self) {
        if let Some(p) = &self.playlists_save_path {
            self.playlists.save_to(p);
        }
    }

    /// Commit the in-progress playlist name input. Routes to either create
    /// (when `playlist_rename_target` is `None`) or rename (when set).
    /// Empty/whitespace-only names cancel the modal with an error toast.
    fn commit_playlist_name_input(&mut self) {
        let raw = match self.playlist_name_input.take() {
            Some(s) => s,
            None => return,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.playlist_rename_target = None;
            self.set_toast("Playlist name cannot be empty".into(), true);
            return;
        }
        let name = trimmed.to_string();

        match self.playlist_rename_target.take() {
            Some(id) => match self.playlists.rename(id, &name) {
                Ok(()) => {
                    self.persist_playlists();
                    self.refresh_sidebar();
                    self.set_toast(format!("Renamed to \"{}\"", name), false);
                }
                Err(e) => {
                    self.set_toast(format!("Rename failed: {e}"), true);
                }
            },
            None => {
                let new_id = self.playlists.add(Playlist::new_manual(name.clone()));
                self.persist_playlists();
                self.refresh_sidebar();
                // Move selection to the freshly-created playlist so the
                // user can immediately start adding tracks. The sidebar is
                // sorted Generated-first, then by `updated_at_ms` desc, so
                // a new manual playlist lands first within the manual block.
                if let Some(idx) = self
                    .sidebar_items
                    .iter()
                    .position(|e| matches!(e, SidebarEntry::Playlist { id, .. } if *id == new_id))
                {
                    self.sidebar_selected = idx;
                }
                self.set_toast(format!("Created \"{}\"", name), false);
            }
        }
    }

    /// Apply a confirmed playlist deletion. Adjusts sidebar selection so we
    /// don't leave the cursor pointing at a freed slot.
    fn commit_playlist_delete(&mut self, id: u64) {
        let name = self
            .playlists
            .get(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "playlist".to_string());
        if !self.playlists.remove(id) {
            return;
        }
        self.persist_playlists();
        let prior = self.sidebar_selected;
        self.refresh_sidebar();
        if !self.sidebar_items.is_empty() {
            self.sidebar_selected = prior.min(self.sidebar_items.len() - 1);
        }
        self.set_toast(format!("Deleted \"{}\"", name), false);
    }

    /// Drop the selected track from the currently-viewed playlist (used by
    /// `d` on a playlist's track row). No-op when the active playlist or
    /// selected track can't be resolved.
    fn remove_selected_track_from_playlist(&mut self) {
        let entry = match self.sidebar_items.get(self.sidebar_selected) {
            Some(e) => e.clone(),
            None => return,
        };
        let playlist_id = match entry {
            SidebarEntry::Playlist { id, .. } => id,
            _ => return,
        };
        let track = match self.track_list.get(self.track_selected) {
            Some(t) => t.clone(),
            None => return,
        };
        let track_id = match track.library_id {
            Some(id) => id,
            None => return,
        };
        if self.playlists.remove_track(playlist_id, track_id) {
            self.persist_playlists();
            // Re-render the playlist's tracks.
            self.select_sidebar_item();
            // Clamp track selection to the new bounds.
            if !self.track_list.is_empty() && self.track_selected >= self.track_list.len() {
                self.track_selected = self.track_list.len() - 1;
            }
            self.set_toast(format!("Removed \"{}\" from playlist", track.name), false);
        }
    }

    /// Open the "add this track to a playlist" picker. Snapshots the current
    /// list of playlists at open time so renames during the popup's lifetime
    /// don't shift selection. Toasts an error if there are no playlists yet.
    fn open_add_to_playlist_picker(&mut self) {
        let track = match self.track_list.get(self.track_selected) {
            Some(t) => t.clone(),
            None => return,
        };
        let track_id = match track.library_id {
            Some(id) => id,
            None => {
                self.set_toast("Track has no library ID".into(), true);
                return;
            }
        };
        if self.playlists.is_empty() {
            self.set_toast(
                "No playlists yet — switch to Playlists (v) and press N".into(),
                true,
            );
            return;
        }
        let options: Vec<(u64, String)> = self
            .playlists
            .playlists()
            .iter()
            .map(|p| (p.id, p.name.clone()))
            .collect();
        self.add_to_playlist_picker = Some(AddToPlaylistPicker {
            track_id,
            track_label: format!("{} — {}", track.artist, track.name),
            options,
            selected: 0,
        });
    }

    /// Apply the picker's selection, adding the track to the chosen playlist.
    fn confirm_add_to_playlist(&mut self) {
        let picker = match self.add_to_playlist_picker.take() {
            Some(p) => p,
            None => return,
        };
        let (playlist_id, playlist_name) = match picker.options.get(picker.selected) {
            Some(opt) => opt.clone(),
            None => return,
        };
        if self.playlists.add_track(playlist_id, picker.track_id) {
            self.persist_playlists();
            self.set_toast(
                format!("Added \"{}\" to \"{}\"", picker.track_label, playlist_name),
                false,
            );
        } else {
            self.set_toast(
                format!(
                    "\"{}\" already in \"{}\"",
                    picker.track_label, playlist_name
                ),
                false,
            );
        }
    }

    /// Open the Generation form. The source panel decides what context is
    /// stashed (and which seed strategy is pre-selected); the user can
    /// then toggle strategies in the form to use whatever they want from
    /// the same context.
    ///
    /// - **Library + TrackList panel**: track id, artist, and genre are
    ///   all captured from the focused row → defaults to Track strategy
    ///   but Artist and Genre are also unlocked.
    /// - **Library + Albums panel**: artist + album captured → defaults to
    ///   Artist strategy.
    /// - **Library + Library (sidebar) panel** with an Artist or Album
    ///   sidebar row: artist captured → defaults to Artist strategy.
    /// - **Anywhere else** (Playlists mode, no focused library row,
    ///   sidebar empty): Discover Weekly defaults — no context.
    pub fn open_generation_form(&mut self) {
        let now = playlist_mod::now_unix_ms();
        if self.browse_mode == BrowseMode::Library {
            // Track-row context: collect everything we can.
            if self.active_panel == Panel::TrackList {
                if let Some(t) = self.track_list.get(self.track_selected) {
                    let track_id = t.library_id;
                    let artist = if t.artist.is_empty() {
                        None
                    } else {
                        Some(t.artist.clone())
                    };
                    let genre = t.genre.clone().filter(|g| !g.trim().is_empty());
                    let label = if track_id.is_some() {
                        t.name.clone()
                    } else {
                        // Track has no library id (device-only row, etc).
                        // Default to artist seeding when we have one.
                        t.artist.clone()
                    };
                    let default_idx = if track_id.is_some() {
                        2 // Track
                    } else if artist.is_some() {
                        3 // Artist
                    } else {
                        0 // TopPlayed
                    };
                    self.generation_form = Some(GenerationFormState::for_library_context(
                        now,
                        track_id,
                        artist,
                        genre,
                        default_idx,
                        &label,
                    ));
                    return;
                }
            }
            // Album-row context (browsing artist's album list).
            if self.active_panel == Panel::Albums {
                if let Some(a) = self.album_list.get(self.album_selected) {
                    self.generation_form = Some(GenerationFormState::for_library_context(
                        now,
                        None,
                        Some(a.artist.clone()),
                        None,
                        3, // Artist
                        &a.artist,
                    ));
                    return;
                }
            }
            // Sidebar-row context (Artists or Albums sidebar mode).
            if self.active_panel == Panel::Library {
                if let Some(entry) = self.sidebar_items.get(self.sidebar_selected) {
                    let (artist, label) = match entry {
                        SidebarEntry::Artist(a) => (Some(a.clone()), a.clone()),
                        SidebarEntry::Album { artist, .. } => {
                            (Some(artist.clone()), artist.clone())
                        }
                        SidebarEntry::Playlist { .. } => (None, String::new()),
                    };
                    if let Some(a) = artist {
                        self.generation_form = Some(GenerationFormState::for_library_context(
                            now,
                            None,
                            Some(a),
                            None,
                            3, // Artist
                            &label,
                        ));
                        return;
                    }
                }
            }
        }
        self.generation_form = Some(GenerationFormState::discover_weekly_default(now));
    }

    /// Open the Generation form pre-filled to regenerate the currently
    /// selected generated playlist. No-op outside Playlists mode or for
    /// manual playlists.
    pub fn open_generation_form_for_regenerate(&mut self) {
        if self.browse_mode != BrowseMode::Playlists {
            return;
        }
        let entry = match self.sidebar_items.get(self.sidebar_selected) {
            Some(e) => e.clone(),
            None => return,
        };
        let id = match entry {
            SidebarEntry::Playlist { id, .. } => id,
            _ => return,
        };
        let playlist = match self.playlists.get(id) {
            Some(p) => p.clone(),
            None => return,
        };
        match GenerationFormState::for_regenerate(&playlist) {
            Some(form) => self.generation_form = Some(form),
            None => self.set_toast("Only generated playlists can be regenerated".into(), true),
        }
    }

    /// Route a non-Tab/Enter/Esc key to the active form field's editor.
    fn generation_form_dispatch_field_key(&mut self, key: KeyEvent) {
        let f = match &mut self.generation_form {
            Some(f) => f,
            None => return,
        };
        // Numeric/slider helpers — explicit `<`/`>` always works; `+`/`-`
        // works when the focused field is one of the numeric ones (avoids
        // conflicting with the name input which also accepts those chars).
        let is_dec = matches!(key.code, KeyCode::Char('<') | KeyCode::Left);
        let is_inc = matches!(key.code, KeyCode::Char('>') | KeyCode::Right);
        match f.selected_field {
            GenerationFormState::FIELD_NAME => match key.code {
                KeyCode::Backspace => {
                    f.name.pop();
                }
                KeyCode::Char(c) => {
                    f.name.push(c);
                }
                _ => {}
            },
            GenerationFormState::FIELD_SEED_STRATEGY => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    f.seed_strategy_idx = if f.seed_strategy_idx == 0 {
                        4
                    } else {
                        f.seed_strategy_idx - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char(' ') => {
                    f.seed_strategy_idx = (f.seed_strategy_idx + 1) % 5;
                }
                _ => {}
            },
            GenerationFormState::FIELD_WINDOW_DAYS => {
                if is_inc {
                    f.window_days = f.window_days.saturating_add(1);
                } else if is_dec {
                    f.window_days = f.window_days.saturating_sub(1);
                }
            }
            GenerationFormState::FIELD_SEED_COUNT => {
                if is_inc {
                    f.seed_count = (f.seed_count + 1).min(100);
                } else if is_dec {
                    f.seed_count = f.seed_count.saturating_sub(1).max(1);
                }
            }
            GenerationFormState::FIELD_TARGET_LEN => {
                if is_inc {
                    f.target_length = (f.target_length + 1).min(500);
                } else if is_dec {
                    f.target_length = f.target_length.saturating_sub(1).max(1);
                }
            }
            GenerationFormState::FIELD_MAX_PER_ARTIST => {
                if is_inc {
                    f.max_per_artist = (f.max_per_artist + 1).min(20);
                } else if is_dec {
                    f.max_per_artist = f.max_per_artist.saturating_sub(1).max(1);
                }
            }
            GenerationFormState::FIELD_MAX_PER_ALBUM => {
                if is_inc {
                    f.max_per_album = (f.max_per_album + 1).min(20);
                } else if is_dec {
                    f.max_per_album = f.max_per_album.saturating_sub(1).max(1);
                }
            }
            GenerationFormState::FIELD_DIVERSITY => {
                if is_inc {
                    f.diversity = (f.diversity + 0.05).min(1.0);
                } else if is_dec {
                    f.diversity = (f.diversity - 0.05).max(0.0);
                }
            }
            GenerationFormState::FIELD_NOVELTY => {
                if is_inc {
                    f.novelty = (f.novelty + 0.05).min(1.0);
                } else if is_dec {
                    f.novelty = (f.novelty - 0.05).max(0.0);
                }
            }
            GenerationFormState::FIELD_EXCLUDE_DEVICE => {
                if matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter) {
                    f.exclude_on_device = !f.exclude_on_device;
                }
            }
            _ => {}
        }
    }

    /// Submit the Generation form: build params, run the recommender, save
    /// the resulting playlist (replacing in place if `regenerating` is set).
    /// Routes the toast message based on success / empty result.
    fn commit_generation_form(&mut self) {
        let form = match self.generation_form.take() {
            Some(f) => f,
            None => return,
        };
        // Reject submits where the chosen strategy doesn't have its
        // backing context. Re-stash the form so the user can fix it
        // without retyping anything.
        if let Err(msg) = form.validate_strategy_context() {
            self.set_toast(msg.into(), true);
            self.generation_form = Some(form);
            return;
        }
        let params = form.to_params();
        let trimmed = form.name.trim();
        if trimmed.is_empty() {
            self.set_toast("Playlist name cannot be empty".into(), true);
            self.generation_form = Some(form);
            return;
        }
        let name = trimmed.to_string();
        let regen_target = form.regenerating;

        let lib = match &self.library {
            Some(l) => l.as_ref(),
            None => {
                self.set_toast("Library not loaded yet".into(), true);
                return;
            }
        };

        // Optional "exclude already on device" filter — only meaningful when
        // a device is connected. Build the exclusion set from the
        // device-side track-id set keyed by (artist, name) and resolved back
        // to library ids. Cheap because we already index by `(artist_key,
        // name_key)` on the library scan.
        let mut params_with_excludes = params;
        if params_with_excludes.exclude_on_device && self.device.status == DeviceStatus::Connected {
            let on_device_ids: Vec<u64> = lib
                .all_tracks()
                .filter(|t| {
                    let ak = normalize_for_match(&t.artist);
                    let nk = normalize_for_match(&t.name);
                    self.device.track_set.contains(&(ak, nk))
                })
                .map(|t| t.id)
                .collect();
            params_with_excludes.exclude_track_ids.extend(on_device_ids);
        }

        // Soft-penalty bag for regenerations.
        let prev: Vec<u64> = match regen_target.and_then(|id| self.playlists.get(id)) {
            Some(p) => {
                let mut bag = p.previously_recommended.clone();
                // Treat the prior selection itself as previously-recommended
                // so the next regeneration drifts off it.
                bag.extend(p.track_ids.iter().copied());
                bag.sort_unstable();
                bag.dedup();
                bag
            }
            None => Vec::new(),
        };

        // Phase 4: build the Markov bigram table from the listen log so
        // the sequence-aware term contributes when the user has enough
        // history. The recommender internally gates on `is_useful()` so
        // brand-new users aren't dragged around by spurious one-offs.
        let bigrams = BigramTable::from_log(&self.listen_log);
        let outcome = WeightedRecommender.generate(
            lib,
            &self.local_plays,
            &params_with_excludes,
            &prev,
            Some(&bigrams),
            playlist_mod::now_unix_ms(),
        );

        if outcome.track_ids.is_empty() {
            self.set_toast(
                format!(
                    "No tracks matched (eligible: {}, candidates: {})",
                    outcome.eligible_count, outcome.candidate_count
                ),
                true,
            );
            // Keep an empty record only if regenerating — the user
            // intentionally re-ran something they care about.
            if regen_target.is_none() {
                return;
            }
        }

        match regen_target {
            Some(id) => {
                if let Some(p) = self.playlists.get_mut(id) {
                    // Promote the prior pick into the soft-penalty bag for
                    // the *next* regen.
                    let mut bag = p.previously_recommended.clone();
                    bag.extend(p.track_ids.iter().copied());
                    bag.sort_unstable();
                    bag.dedup();
                    p.previously_recommended = bag;
                    p.track_ids = outcome.track_ids.clone();
                    p.name = name.clone();
                    p.kind = PlaylistKind::Generated {
                        params: params_with_excludes,
                        last_generated_ms: playlist_mod::now_unix_ms(),
                    };
                    p.updated_at_ms = playlist_mod::now_unix_ms();
                }
                self.persist_playlists();
                self.refresh_sidebar();
                self.set_toast(
                    format!(
                        "Regenerated \"{}\" — {} tracks (seeds: {}, eligible: {})",
                        name,
                        outcome.track_ids.len(),
                        outcome.seed_count,
                        outcome.eligible_count
                    ),
                    false,
                );
            }
            None => {
                let new_pl = Playlist::new_generated(
                    name.clone(),
                    params_with_excludes,
                    outcome.track_ids.clone(),
                );
                let new_id = self.playlists.add(new_pl);
                self.persist_playlists();
                self.refresh_sidebar();
                if let Some(idx) = self
                    .sidebar_items
                    .iter()
                    .position(|e| matches!(e, SidebarEntry::Playlist { id, .. } if *id == new_id))
                {
                    self.sidebar_selected = idx;
                }
                self.set_toast(
                    format!(
                        "Generated \"{}\" — {} tracks (seeds: {}, eligible: {})",
                        name,
                        outcome.track_ids.len(),
                        outcome.seed_count,
                        outcome.eligible_count
                    ),
                    false,
                );
            }
        }
    }

    /// Wire `local_plays` to the on-disk sidecar (`~/.cache/zytunes/local-plays.json`)
    /// and load any existing state. Production code calls this once after
    /// `App::new()`; tests skip it so they don't pick up developer-machine state.
    pub fn load_local_plays_from_disk(&mut self) {
        let path = local_plays::default_save_path();
        if let Some(p) = path.as_deref() {
            self.local_plays = LocalPlays::load_from(p);
        }
        self.local_plays_save_path = path;
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

    /// In-memory half of [`Self::cycle_show_player`]: advances the preference and
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

    /// In-memory half of [`Self::toggle_album_art_style`]: flips the style and
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

    // -- Track-info popup --

    /// Open the metadata-inspector popup for the currently selected track.
    /// No-op (and silently keeps the popup closed) when the track list is
    /// empty — there is no row to describe.
    ///
    /// Resolves the matching library `Track` once and stashes it in
    /// `track_info_lib` so the per-frame renderer does not re-walk the
    /// library on every ~50 ms tick while the popup is open.
    pub fn open_track_info(&mut self) {
        let Some(track) = self.track_list.get(self.track_selected) else {
            return;
        };
        let resolved = self.library.as_ref().and_then(|lib| {
            lib.tracks_by_name(&track.name)
                .find(|t| {
                    t.artist.eq_ignore_ascii_case(&track.artist)
                        && t.album.eq_ignore_ascii_case(&track.album)
                })
                .cloned()
        });
        self.track_info_lib = resolved;
        self.track_info_scroll = 0;
        self.show_track_info = true;
    }

    pub fn close_track_info(&mut self) {
        self.show_track_info = false;
        self.track_info_scroll = 0;
        self.track_info_lib = None;
    }

    /// Re-resolve the cached lib `Track` against the current library. Called
    /// from the `BgEvent::LibraryLoaded` handler so an open popup repaints
    /// from the freshly-scanned metadata instead of the stale clone captured
    /// when it was first opened. When the popup is closed, drop the cache —
    /// `open_track_info` resolves on demand the next time it runs.
    pub fn refresh_track_info_lib(&mut self) {
        if !self.show_track_info {
            self.track_info_lib = None;
            return;
        }
        let Some(track) = self.track_list.get(self.track_selected) else {
            self.track_info_lib = None;
            return;
        };
        self.track_info_lib = self.library.as_ref().and_then(|lib| {
            lib.tracks_by_name(&track.name)
                .find(|t| {
                    t.artist.eq_ignore_ascii_case(&track.artist)
                        && t.album.eq_ignore_ascii_case(&track.album)
                })
                .cloned()
        });
    }

    /// Move the popup scroll cursor by `delta` rows. Negative values clamp
    /// at zero; positive values are clamped against the actual content
    /// height in the renderer (the App layer doesn't know the popup's
    /// rendered visible height — same trick `compute_scroll` uses for the
    /// main track table).
    pub fn track_info_move(&mut self, delta: isize) {
        let next = self.track_info_scroll as isize + delta;
        self.track_info_scroll = next.max(0) as usize;
    }

    /// Page-sized jump (matches the log-pane page step).
    pub fn track_info_page(&mut self, delta: isize) {
        self.track_info_move(delta * 10);
    }

    pub fn track_info_home(&mut self) {
        self.track_info_scroll = 0;
    }

    pub fn track_info_end(&mut self) {
        self.track_info_scroll = usize::MAX;
    }

    // -- Playback controls --

    pub fn play_selected_track(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        if self.track_list.is_empty() {
            return;
        }
        // If the prior session never crossed the play threshold, transitioning
        // out counts as a skip — record before we lose the old `now_playing`.
        self.record_now_playing_skip();

        let index = self.track_selected.min(self.track_list.len() - 1);
        let track = &self.track_list[index];
        let path = match &track.location {
            Some(p) => p.clone(),
            None => {
                self.set_toast("No file path for this track".into(), true);
                return;
            }
        };
        let lib_track = self.lookup_library_track(track);
        let track_id = lib_track.as_ref().map(|t| t.id);
        let (year, metadata_marquee) = build_now_playing_metadata(lib_track.as_ref());
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
            track_id,
            counted: false,
            year,
            metadata_marquee,
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
        // Skip recorded before the transition since `play_from_playlist`
        // (or the end-of-playlist branch) overwrites `now_playing`. No-op
        // if the prior session was already counted as a play.
        self.record_now_playing_skip();
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
        // Restarting the current track (>3s elapsed) keeps the same session,
        // so don't record a skip there — only record when actually moving
        // to the previous row.
        let restart = self
            .now_playing
            .as_ref()
            .is_some_and(|np| np.elapsed_ms > 3000 || np.track_index == 0);
        if !restart {
            self.record_now_playing_skip();
        }
        if let Some(ref np) = self.now_playing {
            let playlist = Arc::clone(&np.playlist);
            let index = if restart {
                np.track_index
            } else {
                np.track_index - 1
            };
            self.play_from_playlist(index, playlist, audio_tx);
        }
    }

    pub fn stop_playback(&mut self, audio_tx: &mpsc::Sender<AudioCommand>) {
        self.record_now_playing_skip();
        let _ = audio_tx.send(AudioCommand::Stop);
        self.now_playing = None;
    }

    fn play_from_playlist(
        &mut self,
        index: usize,
        playlist: Arc<[TrackInfo]>,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) {
        let track = playlist[index].clone();
        let path = match &track.location {
            Some(p) => p.clone(),
            None => {
                self.set_toast("No file path for this track".into(), true);
                return;
            }
        };
        let lib_track = self.lookup_library_track(&track);
        let track_id = lib_track.as_ref().map(|t| t.id);
        let (year, metadata_marquee) = build_now_playing_metadata(lib_track.as_ref());
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
            track_id,
            counted: false,
            year,
            metadata_marquee,
        });
    }

    pub fn handle_audio_event(&mut self, event: AudioEvent, audio_tx: &mpsc::Sender<AudioCommand>) {
        match event {
            AudioEvent::Position { elapsed_ms } => {
                if let Some(ref mut np) = self.now_playing {
                    np.elapsed_ms = elapsed_ms;
                }
                self.maybe_record_play_for_threshold();
            }
            AudioEvent::TrackEnded => {
                // Fallback path for tracks that ended before any Position
                // event past the threshold (very short clips, race between
                // the end signal and the 250 ms position-poll). After this
                // call `next_track`'s skip-record is a no-op because
                // `counted` is already true.
                self.record_now_playing_play();
                self.next_track(audio_tx);
            }
            AudioEvent::PlaybackError(msg) => {
                // A playback failure isn't a user-initiated skip — drop
                // the session without recording either way.
                self.now_playing = None;
                self.set_toast(format!("Playback: {}", msg), true);
            }
        }
    }

    /// Look up the full library `Track` matching the given playable row.
    /// Same `(name, artist, album)` lookup as the popup-resolve path,
    /// including a `strip_track_number` fallback for device-mode rows
    /// whose names start with a track-number prefix (e.g. `"01 Song"`
    /// resolves to `"Song"`). Returns a cloned `Track` so callers can read
    /// extended metadata (year, genre, BPM, …)
    /// without holding a borrow on `self.library`. `None` for device-mode
    /// rows or scratch files with no library counterpart.
    fn lookup_library_track(&self, track: &TrackInfo) -> Option<Track> {
        let lib = self.library.as_deref()?;
        let lookup = |name: &str| -> Option<Track> {
            lib.tracks_by_name(name)
                .find(|t| {
                    t.artist.eq_ignore_ascii_case(&track.artist)
                        && t.album.eq_ignore_ascii_case(&track.album)
                })
                .cloned()
        };
        if let Some(t) = lookup(&track.name) {
            return Some(t);
        }
        let stripped = zytunes::strip_track_number(track.name.trim());
        if stripped != track.name {
            return lookup(stripped);
        }
        None
    }

    /// Persist `local_plays` to disk. Saves to `local_plays_save_path` if
    /// set; tests construct an `App` with `None` to skip disk writes.
    fn persist_local_plays(&self) {
        if let Some(p) = &self.local_plays_save_path {
            self.local_plays.save_to(p);
        }
    }

    /// If the current `now_playing` session has crossed the play threshold
    /// (50% of duration or 4 minutes, whichever first) and hasn't yet been
    /// counted, record it as a play. Idempotent within the session.
    fn maybe_record_play_for_threshold(&mut self) {
        let trip = match &self.now_playing {
            Some(np) => {
                !np.counted
                    && np.duration_ms > 0
                    && np.elapsed_ms >= local_plays::play_threshold_ms(np.duration_ms)
            }
            None => false,
        };
        if trip {
            self.record_now_playing_play();
        }
    }

    /// Record the current session as a play if not already counted. Bumps
    /// the aggregate count, refreshes `last_played_at_ms`, persists the
    /// sidecar, and flips the session's `counted` flag so subsequent
    /// triggers no-op.
    fn record_now_playing_play(&mut self) {
        let Some(np) = self.now_playing.as_mut() else {
            return;
        };
        if np.counted {
            return;
        }
        let recorded_id = np.track_id;
        if let Some(id) = recorded_id {
            let now = local_plays::now_unix_ms();
            self.local_plays.record_play(id, now);
            self.persist_local_plays();
            // Phase 4: append to the listen log so the sequence model
            // sees this play. `completed: true` because we crossed the
            // play threshold (50% / 4 min).
            self.listen_log.append(ListenEvent {
                ts: now,
                id,
                completed: true,
            });
        }
        // Always flip the flag — even if there's no library ID we don't
        // want to keep retrying the threshold check on every Position event.
        if let Some(np) = self.now_playing.as_mut() {
            np.counted = true;
        }
        // Push the new aggregate into any visible row + the popup snapshot
        // so the user sees the count update live without having to navigate
        // away and back to rebuild `track_list`.
        if let Some(id) = recorded_id {
            self.refresh_play_stats_for_id(id);
        }
    }

    /// Record the current session as a skip if not already counted. Same
    /// idempotency as `record_now_playing_play`; safe to call from any
    /// transition path (`next_track`, `prev_track`, `stop_playback`,
    /// `play_selected_track`).
    fn record_now_playing_skip(&mut self) {
        let Some(np) = self.now_playing.as_mut() else {
            return;
        };
        if np.counted {
            return;
        }
        let recorded_id = np.track_id;
        if let Some(id) = recorded_id {
            self.local_plays.record_skip(id);
            self.persist_local_plays();
            // Phase 4: log the skip so the sequence model can de-weight
            // transitions to tracks the user keeps abandoning. The bigram
            // builder treats `completed: false` as half-weight.
            self.listen_log.append(ListenEvent {
                ts: local_plays::now_unix_ms(),
                id,
                completed: false,
            });
        }
        if let Some(np) = self.now_playing.as_mut() {
            np.counted = true;
        }
        if let Some(id) = recorded_id {
            self.refresh_play_stats_for_id(id);
        }
    }

    /// Push the current sidecar values for `library_id` back into any
    /// `track_list` row that references it (and the popup snapshot, which
    /// the renderer reads independently). Without this the user records a
    /// play but the column / popup keeps showing the pre-bump number until
    /// they navigate away and back, which rebuilds `track_list`.
    fn refresh_play_stats_for_id(&mut self, library_id: u64) {
        let entry = match self.local_plays.get(library_id) {
            Some(p) => p.clone(),
            None => return,
        };
        let device_key = self.current_device_baseline_key();
        let last_synced = device_key
            .as_deref()
            .and_then(|k| entry.device_baselines.get(k))
            .map(|b| b.last_synced_at_ms);
        for row in self.track_list.iter_mut() {
            if row.library_id != Some(library_id) {
                continue;
            }
            row.play_count = Some(entry.play_count);
            row.skip_count = if entry.skip_count > 0 {
                Some(entry.skip_count)
            } else {
                None
            };
            row.last_played_at_ms = if entry.last_played_at_ms > 0 {
                Some(entry.last_played_at_ms)
            } else {
                None
            };
            // Only override the popup-visible "Last synced" timestamp when
            // we actually have a baseline for the current device — leaves
            // device-disconnected popups showing the row's last known value.
            if last_synced.is_some() {
                row.last_synced_from_device_at_ms = last_synced;
            }
        }
    }

    /// Build the per-device baseline key for the currently-connected device,
    /// or `None` when no device is connected. Cached on the `App` so the
    /// per-frame TrackInfo construction doesn't re-run the format on each
    /// row — but this helper is the single source of truth used by both
    /// the merge path and the display path.
    fn current_device_baseline_key(&self) -> Option<String> {
        self.device.family.map(|family| {
            local_plays::device_baseline_key(
                family,
                self.device.serial.as_deref(),
                self.device.firmware.as_deref(),
            )
        })
    }

    /// Fold the device's per-track `play_count` and `skip_count` into the
    /// aggregate `local_plays` sidecar via per-(track, device) baselines.
    /// First sight of a device adopts its full count; subsequent reconnects
    /// only contribute the delta. Idempotent — safe to call from both the
    /// `DeviceTracksLoaded` and `LibraryLoaded` handlers (whichever fires
    /// later wins, both leave the state correct).
    ///
    /// Match strategy mirrors `add_indexed_track`: parse each device entry
    /// path into `(artist, display_name)`, normalize, and look up the
    /// matching library track via `(artist_key, name_key)`. Both raw and
    /// `strip_track_number`-stripped name keys are indexed so library
    /// titles without a leading "01 " still match device filenames that
    /// have one.
    pub fn merge_device_plays_into_local(&mut self) {
        let Some(device_key) = self.current_device_baseline_key() else {
            return;
        };
        if self.device.tracks.is_empty() {
            return;
        }
        let Some(lib) = self.library.as_ref() else {
            return;
        };

        let now_ms = local_plays::now_unix_ms();

        // Index device tracks by normalized (artist_key, name_key) → (play, skip).
        // Building the index here (instead of reusing `device.track_metadata`)
        // keeps `DeviceTrackMeta`'s shape unchanged at the cost of one O(D)
        // pass — acceptable since this runs once per reconnect, not per render.
        let mut by_key: std::collections::HashMap<(String, String), (u32, u32)> =
            std::collections::HashMap::new();
        for dt in &self.device.tracks {
            if dt.is_dir() {
                continue;
            }
            let (artist, _, display_name) = parse_device_track_parts(&dt.name);
            let artist_key = normalize_for_match(&artist);
            let raw = normalize_for_match(&display_name);
            let stripped = normalize_for_match(zytunes::strip_track_number(display_name.trim()));
            let pair = (dt.play_count.unwrap_or(0), dt.skip_count.unwrap_or(0));
            by_key.insert((artist_key.clone(), raw.clone()), pair);
            if stripped != raw {
                by_key.insert((artist_key, stripped), pair);
            }
        }

        // Walk the library and collect matches into a Vec — defers the
        // mutable borrow of `self.local_plays` to a second pass so the
        // immutable borrow of `self.library` and `self.device.tracks`
        // (via `lib` and `by_key`) can outlive the lookup.
        let to_merge: Vec<(u64, u32, u32)> = lib
            .all_tracks()
            .filter_map(|t| {
                let artist_key = normalize_for_match(&t.artist);
                let name_key = normalize_for_match(&t.name);
                by_key
                    .get(&(artist_key, name_key))
                    .map(|(p, s)| (t.id, *p, *s))
            })
            .collect();

        if to_merge.is_empty() {
            return;
        }

        // Capture the affected library IDs before the move so we can push
        // the freshly-merged values into any visible `track_list` rows
        // (otherwise the popup / track table keeps showing pre-merge
        // numbers until the user navigates away and back).
        let affected_ids: Vec<u64> = to_merge.iter().map(|(id, _, _)| *id).collect();
        for (id, play, skip) in to_merge {
            self.local_plays
                .merge_device_observation(id, &device_key, play, skip, now_ms);
        }
        self.persist_local_plays();
        for id in affected_ids {
            self.refresh_play_stats_for_id(id);
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
        self.device.track_metadata.clear();
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
        // Drop any open track-info popup before wiping the index. Today the
        // popup is Library-mode-only and the browse-mode reset below makes
        // this a no-op, but if the gating is ever relaxed an open popup
        // could survive a disconnect and paint stale device state.
        self.close_track_info();
        self.device.artists.clear();
        self.device.albums.clear();
        self.device.album_tracks.clear();
        self.device.track_set.clear();
        self.device.track_metadata.clear();
        self.device.artist_track_names.clear();
        self.device.acquired_items = 0;
        self.artist_device_status.clear();
        self.album_device_status.clear();
        if self.browse_mode == BrowseMode::Device {
            self.browse_mode = BrowseMode::Library;
            self.refresh_sidebar();
        }
    }

    /// Cycle the browse mode: Library → Device → Playlists → Library.
    /// `Device` is skipped when no device is connected (parallels the
    /// existing `v` gate). Playlists mode is always reachable so users can
    /// still curate without a device hooked up.
    pub fn toggle_browse_mode(&mut self) {
        self.save_sidebar_pos();
        let device_connected = self.device.status == DeviceStatus::Connected;
        self.browse_mode = match self.browse_mode {
            BrowseMode::Library => {
                if device_connected {
                    BrowseMode::Device
                } else {
                    BrowseMode::Playlists
                }
            }
            BrowseMode::Device => BrowseMode::Playlists,
            BrowseMode::Playlists => BrowseMode::Library,
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
            // Playlist removal goes through `pending_playlist_delete`, not
            // the device-removal queue — playlists live library-side only.
            SidebarEntry::Playlist { .. } => Vec::new(),
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
            // Playlists never resolve to a device-side (artist, album) pair.
            // Callers are gated on `BrowseMode::Device` so this arm is
            // structurally unreachable in production but kept exhaustive.
            SidebarEntry::Playlist { .. } => (String::new(), String::new()),
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
            BrowseMode::Playlists => {
                // Sort: generated playlists first, then by `updated_at_ms`
                // descending. Most-recently-touched stays at the top.
                let mut entries: Vec<(&Playlist, SidebarEntry)> = self
                    .playlists
                    .playlists()
                    .iter()
                    .map(|p| {
                        (
                            p,
                            SidebarEntry::Playlist {
                                id: p.id,
                                name: p.name.clone(),
                            },
                        )
                    })
                    .collect();
                entries.sort_by(|(a, _), (b, _)| {
                    b.is_generated()
                        .cmp(&a.is_generated())
                        .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
                });
                entries.into_iter().map(|(_, e)| e).collect()
            }
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

        if self.browse_mode == BrowseMode::Playlists {
            self.select_sidebar_item_playlist(&entry);
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
                let device_key = self.current_device_baseline_key();
                self.track_list = tracks_to_info(
                    lib.album_tracks(album),
                    &self.device,
                    &self.local_plays,
                    device_key.as_deref(),
                );
                self.sort_tracks();
                self.track_selected = 0;
                self.track_scroll = 0;
                self.refresh_album_art();
            }
            // Library mode never produces Playlist entries; gated above.
            SidebarEntry::Playlist { .. } => {}
        }
    }

    /// Resolve a Playlist sidebar entry into a populated `track_list` of
    /// library-side `TrackInfo`s in playlist order. Tracks whose IDs no
    /// longer resolve in the library (file moved/deleted) are silently
    /// skipped — Phase 2 will add a load-time resolver that re-binds via
    /// `(artist, album, title)` lookup. For Phase 1 a quiet drop is fine.
    fn select_sidebar_item_playlist(&mut self, entry: &SidebarEntry) {
        let id = match entry {
            SidebarEntry::Playlist { id, .. } => *id,
            _ => return,
        };
        self.album_list.clear();
        self.album_art = None;
        self.album_art_key.clear();
        self.album_art_cache = None;
        self.album_art_size = (0, 0);

        let lib = match &self.library {
            Some(l) => l,
            None => {
                self.track_list.clear();
                return;
            }
        };
        let playlist = match self.playlists.get(id) {
            Some(p) => p,
            None => {
                self.track_list.clear();
                return;
            }
        };

        // Build a quick lookup from `Track::id` so we resolve playlist IDs
        // in O(N + M) rather than M * N. The library is iterated once.
        let mut by_id: HashMap<u64, &Track> = HashMap::new();
        for t in lib.all_tracks() {
            by_id.insert(t.id, t);
        }
        let device_key = self.current_device_baseline_key();
        let resolved: Vec<&Track> = playlist
            .track_ids
            .iter()
            .filter_map(|tid| by_id.get(tid).copied())
            .collect();
        self.track_list = tracks_to_info(
            resolved,
            &self.device,
            &self.local_plays,
            device_key.as_deref(),
        );
        // Don't sort — playlists carry user-meaningful order.
        self.track_selected = 0;
        self.track_scroll = 0;
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
                let device_key = self.current_device_baseline_key();
                let lib_ref = self.library.as_deref();
                self.track_list = match self.device.album_tracks.get(&key) {
                    Some(tracks) => device_tracks_to_info(
                        tracks,
                        lib_ref,
                        &self.local_plays,
                        device_key.as_deref(),
                    ),
                    None => Vec::new(),
                };
                self.track_selected = 0;
                self.track_scroll = 0;
            }
            // Device mode never produces Playlist entries.
            SidebarEntry::Playlist { .. } => {}
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
            let device_key = self.current_device_baseline_key();
            let lib_ref = self.library.as_deref();
            self.track_list = match self.device.album_tracks.get(&key) {
                Some(tracks) => {
                    device_tracks_to_info(tracks, lib_ref, &self.local_plays, device_key.as_deref())
                }
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

        let device_key = self.current_device_baseline_key();
        self.track_list = tracks_to_info(
            lib.album_tracks_by_artist(&album.artist, &album.name),
            &self.device,
            &self.local_plays,
            device_key.as_deref(),
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
        // Build a cache key from the current track list context. Artist and
        // album are passed separately so the persistent art cache can key on
        // structured fields rather than parsing the delimited form.
        let (key, artist, album) = if let Some(t) = self.track_list.first() {
            (
                format!("{}/{}", t.artist, t.album),
                t.artist.clone(),
                t.album.clone(),
            )
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
        self.pending_bg_commands.push(BgCommand::LoadAlbumArt {
            key,
            artist,
            album,
            paths,
        });
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
            SidebarEntry::Playlist { id, name } => {
                // Build SyncItems by resolving each track ID against the
                // library. Tracks the library can't resolve (file moved or
                // deleted) are silently skipped — Phase 2 adds the resolver
                // that re-binds via (artist, album, title).
                let lib = match &self.library {
                    Some(l) => l,
                    None => return,
                };
                let playlist = match self.playlists.get(*id) {
                    Some(p) => p.clone(),
                    None => return,
                };
                let by_id: HashMap<u64, &Track> = lib.all_tracks().map(|t| (t.id, t)).collect();
                let mut items = Vec::new();
                let mut missing = 0usize;
                for tid in &playlist.track_ids {
                    match by_id.get(tid) {
                        Some(t) => {
                            if let Some(loc) = &t.location {
                                items.push(SyncItem {
                                    artist: t.artist.clone(),
                                    album: t.album.clone(),
                                    name: t.name.clone(),
                                    location: loc.clone(),
                                    track_number: t.track_number,
                                    genre: t.genre.clone(),
                                    overwrite_targets: Vec::new(),
                                });
                            } else {
                                missing += 1;
                            }
                        }
                        None => missing += 1,
                    }
                }
                if items.is_empty() {
                    self.set_toast(
                        format!("Playlist \"{}\" has no playable tracks", name),
                        true,
                    );
                    return;
                }
                let count = items.len();
                // Build the (artist, album, title) tuple list before moving
                // `items` into the queue so the device-side resolver has the
                // exact same ordering the user sees in the playlist.
                let track_keys: Vec<(String, String, String)> = items
                    .iter()
                    .map(|i| (i.artist.clone(), i.album.clone(), i.name.clone()))
                    .collect();
                self.sync.queue.push(QueuedItem {
                    label: name.clone(),
                    tracks: items,
                });
                // Register a follow-up playlist creation for the next
                // `SyncComplete` event. The actual device push is gated
                // per-backend at drain time (see `handle_bg_event`'s
                // SyncComplete arm): Zune is always-on after the
                // 2026-04-26 hardware validation; iPod stays gated
                // behind `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1` until
                // the iTunesDB-corruption incident root-cause is found.
                self.pending_playlist_imports
                    .retain(|p| p.name.as_str() != name.as_str());
                self.pending_playlist_imports.push(PendingPlaylistImport {
                    name: name.clone(),
                    track_keys,
                });
                let toast = if missing > 0 {
                    format!("Added {count} tracks to queue ({missing} unresolved)")
                } else {
                    format!("Added {count} tracks to queue")
                };
                self.set_toast(toast, false);
                self.forward_last_queue_item_if_syncing();
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
        // Drop any pending playlist creations — they only make sense if
        // their tracks are about to land on-device, and clearing the queue
        // means the user changed their mind.
        self.pending_playlist_imports.clear();
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
                        // Re-resolve the popup's cached lib `Track` against
                        // the new library *before* `refresh_sidebar` clears
                        // `track_list` — closed popups drop the cache, open
                        // ones repaint from the freshly-scanned metadata.
                        self.refresh_track_info_lib();
                        self.rebuild_artist_device_status();
                        self.refresh_sidebar();
                        // If a device connected before the library finished
                        // scanning, the merge couldn't fire on
                        // `DeviceTracksLoaded`. Catch up now.
                        self.merge_device_plays_into_local();
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
                // Fold the device's per-track play/skip counters into the
                // aggregate local-plays sidecar via per-(track, device)
                // baselines. No-op when the library hasn't loaded yet —
                // the `LibraryLoaded` handler runs the merge in that order.
                self.merge_device_plays_into_local();
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
                // Drain queued playlist imports — the file sync completed,
                // so the device-side resolver will now see freshly-uploaded
                // tracks. Per-backend gating decides whether each spec
                // actually fires:
                //   - Zune: always fire (validated end-to-end on hw 2026-04-26)
                //   - iPod: only fire when ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1
                //     until the iTunesDB-corruption incident is root-caused
                //   - Unknown family: skip (defensive — no point sending to
                //     a backend whose Err semantics we don't trust yet)
                let family = self.device.family;
                let allow = match family {
                    Some(zytunes::device::DeviceFamily::Zune) => true,
                    Some(zytunes::device::DeviceFamily::Ipod) => self.experimental_playlist_sync,
                    None => false,
                };
                let drained: Vec<_> = self.pending_playlist_imports.drain(..).collect();
                for spec in drained {
                    if allow {
                        self.sync
                            .log
                            .push(format!("Importing playlist \"{}\" to device", spec.name));
                        self.pending_bg_commands.push(BgCommand::ImportPlaylist {
                            name: spec.name,
                            track_keys: spec.track_keys,
                        });
                    } else {
                        let reason = match family {
                            Some(zytunes::device::DeviceFamily::Ipod) => {
                                "iPod playlist sync gated off after 2026-04-26 \
                                 iTunesDB-corruption incident; set \
                                 ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1 to opt in"
                            }
                            _ => "no device session active",
                        };
                        self.sync.log.push(format!(
                            "Playlist \"{}\": device-side push skipped ({reason})",
                            spec.name
                        ));
                    }
                }
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
            BgEvent::PlaylistImported { name, summary } => match summary {
                Ok(s) => {
                    let verb = if s.replaced { "Replaced" } else { "Created" };
                    let msg = if s.skipped > 0 {
                        format!(
                            "{verb} playlist \"{}\" on device — {} tracks ({} unresolved)",
                            name, s.resolved, s.skipped
                        )
                    } else {
                        format!(
                            "{verb} playlist \"{}\" on device — {} tracks",
                            name, s.resolved
                        )
                    };
                    self.sync.log.push(msg.clone());
                    self.set_toast(msg, false);
                }
                Err(e) => {
                    let msg = format!("Playlist \"{}\" import failed: {}", name, e);
                    self.sync.log.push(msg.clone());
                    self.set_toast(msg, true);
                }
            },
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

    /// Single-source-of-truth keyboard dispatcher for the TUI.
    ///
    /// Handles the same modal-then-global cascade the run loop used to inline:
    /// search, theme picker, cache-clear confirm, removal confirm, help, and
    /// track-info popup are all gated before the global match. Returns
    /// `KeyOutcome::Quit` for `q` / `Ctrl+C` so the run loop can break.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        cmd_tx: &mpsc::Sender<BgCommand>,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) -> KeyOutcome {
        // Playlist name input modal — handles both create-new and rename.
        if self.playlist_name_input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.playlist_name_input = None;
                    self.playlist_rename_target = None;
                }
                KeyCode::Enter => {
                    self.commit_playlist_name_input();
                }
                KeyCode::Backspace => {
                    if let Some(buf) = &mut self.playlist_name_input {
                        buf.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(buf) = &mut self.playlist_name_input {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        // Playlist Generation form.
        if self.generation_form.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.generation_form = None;
                }
                KeyCode::Enter => {
                    self.commit_generation_form();
                }
                KeyCode::Tab => {
                    if let Some(f) = &mut self.generation_form {
                        f.selected_field =
                            (f.selected_field + 1) % GenerationFormState::FIELD_COUNT;
                    }
                }
                KeyCode::BackTab => {
                    if let Some(f) = &mut self.generation_form {
                        f.selected_field = if f.selected_field == 0 {
                            GenerationFormState::FIELD_COUNT - 1
                        } else {
                            f.selected_field - 1
                        };
                    }
                }
                _ => {
                    self.generation_form_dispatch_field_key(key);
                }
            }
            return KeyOutcome::Continue;
        }

        // "Add this track to a playlist" picker.
        if self.add_to_playlist_picker.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.add_to_playlist_picker = None;
                }
                KeyCode::Enter => {
                    self.confirm_add_to_playlist();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(p) = &mut self.add_to_playlist_picker {
                        if p.selected > 0 {
                            p.selected -= 1;
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(p) = &mut self.add_to_playlist_picker {
                        if p.selected + 1 < p.options.len() {
                            p.selected += 1;
                        }
                    }
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        // Playlist delete confirmation.
        if self.pending_playlist_delete.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(id) = self.pending_playlist_delete.take() {
                        self.commit_playlist_delete(id);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.pending_playlist_delete = None;
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.search_active {
            match key.code {
                KeyCode::Esc => {
                    self.search_active = false;
                    self.search_query.clear();
                    self.apply_sidebar_filter();
                }
                KeyCode::Enter => {
                    self.search_active = false;
                    self.active_panel = Panel::Library;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.apply_sidebar_filter();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.apply_sidebar_filter();
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.show_theme_picker {
            match key.code {
                KeyCode::Esc => self.theme_picker_cancel(),
                KeyCode::Enter => self.theme_picker_confirm(),
                KeyCode::Up => self.theme_picker_move(-1),
                KeyCode::Down => self.theme_picker_move(1),
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.pending_cache_clear {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.pending_cache_clear = false;
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
                        self.set_toast(format!("Cleared {:.1} MB of cached audio", mb), false);
                    } else {
                        self.set_toast("Cache is already empty".into(), false);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_cache_clear = false;
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.pending_removal.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(items) = self.pending_removal.take() {
                        let count = items.len();
                        self.set_toast(
                            format!("Removing {} track(s) from device...", count),
                            false,
                        );
                        let _ = cmd_tx.send(BgCommand::RemoveFromDevice(items));
                        self.removal_queue.clear();
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_removal = None;
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.show_help = false;
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        if self.show_track_info {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('I') => {
                    self.close_track_info();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.track_info_move(-1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.track_info_move(1);
                }
                KeyCode::PageUp => {
                    self.track_info_page(-1);
                }
                KeyCode::PageDown => {
                    self.track_info_page(1);
                }
                KeyCode::Char('g') => {
                    self.track_info_home();
                }
                KeyCode::Char('G') => {
                    self.track_info_end();
                }
                _ => {}
            }
            return KeyOutcome::Continue;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return KeyOutcome::Quit;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                return KeyOutcome::Quit;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('h') => {
                self.show_keys = !self.show_keys;
            }
            KeyCode::Tab => {
                self.cycle_panel();
            }
            KeyCode::BackTab => {
                self.cycle_panel_back();
            }
            KeyCode::Char('1') => {
                self.save_sidebar_pos();
                self.sidebar_mode = SidebarMode::Artists;
                self.refresh_sidebar();
                self.active_panel = Panel::Library;
            }
            KeyCode::Char('2') => {
                self.save_sidebar_pos();
                self.sidebar_mode = SidebarMode::Albums;
                self.refresh_sidebar();
                self.active_panel = Panel::Library;
            }
            KeyCode::Char('4') if !self.sync.queue.is_empty() => {
                self.active_panel = Panel::SyncQueue;
            }
            KeyCode::Char('t') => {
                self.open_theme_picker();
            }
            KeyCode::Char('T') => {
                self.toggle_album_art_style();
            }
            KeyCode::Char('I') if self.active_panel == Panel::TrackList => {
                self.open_track_info();
            }
            KeyCode::Char('P') => {
                let label = self.cycle_show_player();
                self.set_toast(label.to_string(), false);
            }
            KeyCode::Char('v') => {
                // `toggle_browse_mode` already skips `Device` when nothing's
                // connected, so cycling Library → Playlists → Library always
                // works and never requires a device. The old "Connect a
                // device first" gate was a holdover from when there were
                // only two modes — drop it so users can reach Playlists
                // without plugging anything in.
                self.toggle_browse_mode();
            }
            KeyCode::Char('c') if self.device.status == DeviceStatus::Disconnected => {
                self.device.status = DeviceStatus::Detecting;
                self.connection_anim_start = Some(self.anim_frame);
                let _ = cmd_tx.send(BgCommand::Connect);
            }
            KeyCode::Char('d') => match self.active_panel {
                Panel::SyncQueue => {
                    self.remove_queue_item();
                }
                Panel::Device => {
                    let _ = cmd_tx.send(BgCommand::Disconnect);
                    self.device.status = DeviceStatus::Disconnected;
                    self.device.name = None;
                    self.device.tracks.clear();
                    self.clear_device_index();
                    self.set_toast("Disconnected".into(), false);
                }
                Panel::Library if self.browse_mode == BrowseMode::Playlists => {
                    if let Some(SidebarEntry::Playlist { id, .. }) =
                        self.sidebar_items.get(self.sidebar_selected).cloned()
                    {
                        self.pending_playlist_delete = Some(id);
                    }
                }
                Panel::TrackList if self.browse_mode == BrowseMode::Playlists => {
                    self.remove_selected_track_from_playlist();
                }
                _ => {}
            },
            KeyCode::Char('N') if self.browse_mode == BrowseMode::Playlists => {
                self.playlist_name_input = Some(String::new());
                self.playlist_rename_target = None;
            }
            KeyCode::Char('e') if self.browse_mode == BrowseMode::Playlists => {
                if let Some(SidebarEntry::Playlist { id, name }) =
                    self.sidebar_items.get(self.sidebar_selected).cloned()
                {
                    self.playlist_name_input = Some(name);
                    self.playlist_rename_target = Some(id);
                }
            }
            KeyCode::Char('+')
                if self.browse_mode == BrowseMode::Library
                    && self.active_panel == Panel::TrackList =>
            {
                self.open_add_to_playlist_picker();
            }
            // Open Generation form. Pre-fills with the focused track's id
            // when invoked from a Library track row, otherwise the
            // Discover-Weekly defaults.
            KeyCode::Char('G') => {
                self.open_generation_form();
            }
            // Regenerate the selected generated playlist (Playlists mode).
            // Falls through to the device-refresh handler below otherwise.
            KeyCode::Char('R') if self.browse_mode == BrowseMode::Playlists => {
                self.open_generation_form_for_regenerate();
            }
            KeyCode::Char('r') if self.device.status == DeviceStatus::Connected => {
                let _ = cmd_tx.send(BgCommand::LoadDeviceTracks);
                self.set_toast("Refreshing device tracks...".into(), false);
            }
            KeyCode::Up => {
                self.move_up();
            }
            KeyCode::Down => {
                self.move_down();
            }
            KeyCode::PageUp => {
                self.sync.log_scroll_up(10);
            }
            KeyCode::PageDown => {
                self.sync.log_scroll_down(10);
            }
            KeyCode::Right => {
                self.skip_forward();
            }
            KeyCode::Left => {
                self.skip_back();
            }
            KeyCode::Char(' ') => {
                self.toggle_playback(audio_tx);
            }
            KeyCode::Char('<') | KeyCode::Char(',') if self.now_playing.is_some() => {
                let _ = audio_tx.send(AudioCommand::Scrub { delta_ms: -5000 });
            }
            KeyCode::Char('>') | KeyCode::Char('.') if self.now_playing.is_some() => {
                let _ = audio_tx.send(AudioCommand::Scrub { delta_ms: 5000 });
            }
            KeyCode::Char('n') => {
                self.next_track(audio_tx);
            }
            KeyCode::Char('p') => {
                self.prev_track(audio_tx);
            }
            KeyCode::Enter => match self.active_panel {
                Panel::Library => {
                    self.select_sidebar_item();
                    if self.has_album_browser() {
                        self.active_panel = Panel::Albums;
                    } else {
                        self.active_panel = Panel::TrackList;
                    }
                }
                Panel::Albums => {
                    self.active_panel = Panel::TrackList;
                }
                Panel::TrackList => {
                    self.play_selected_track(audio_tx);
                }
                Panel::SyncQueue => {
                    self.execute_sync(cmd_tx);
                }
                _ => {}
            },
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_query.clear();
            }
            KeyCode::Char('s') if self.active_panel == Panel::TrackList => {
                self.cycle_sort();
            }
            KeyCode::Char('L') => {
                let path = std::path::PathBuf::from("/tmp/zytunes-log.txt");
                let content = self.sync.log.join("\n");
                match std::fs::write(&path, &content) {
                    Ok(_) => {
                        let path_str = path.display().to_string();
                        let toast = match copy_to_clipboard(&path_str) {
                            Ok(()) => {
                                format!("Log dumped to {} (copied to clipboard)", path_str)
                            }
                            Err(e) => {
                                format!("Log dumped to {} (clipboard: {})", path_str, e)
                            }
                        };
                        self.set_toast(toast, false);
                    }
                    Err(e) => self.set_toast(format!("Log dump failed: {}", e), true),
                }
            }
            KeyCode::Char('S') if !self.sync.queue.is_empty() => {
                self.active_panel = Panel::SyncQueue;
                self.execute_sync(cmd_tx);
            }
            KeyCode::Char('a') => {
                if self.browse_mode == BrowseMode::Device {
                    self.queue_device_removal();
                } else {
                    match self.active_panel {
                        Panel::TrackList => {
                            self.add_selected_track_to_queue();
                        }
                        Panel::Library => {
                            self.add_sidebar_item_to_queue();
                        }
                        Panel::Albums => {
                            self.add_all_visible_to_queue();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('A') => {
                if self.browse_mode == BrowseMode::Device {
                    self.queue_device_removal();
                } else if self.active_panel == Panel::TrackList {
                    self.add_all_visible_to_queue();
                }
            }
            KeyCode::Char('D')
                if self.browse_mode == BrowseMode::Device && !self.removal_queue.is_empty() =>
            {
                self.pending_removal = Some(self.removal_queue.clone());
            }
            KeyCode::Char('C') => {
                if self.browse_mode == BrowseMode::Device {
                    let count = self.removal_queue.len();
                    self.clear_removal_queue();
                    if count > 0 {
                        self.set_toast(format!("Cleared {} queued removal(s)", count), false);
                    }
                } else if self.active_panel == Panel::SyncQueue {
                    self.clear_queue();
                }
            }
            KeyCode::Char('U') if self.browse_mode == BrowseMode::Device => {
                self.dedupe_device(cmd_tx);
            }
            KeyCode::Char('X') => {
                self.pending_cache_clear = true;
            }
            KeyCode::Esc => {
                if matches!(self.sync.status, SyncStatus::Running { .. }) {
                    let _ = cmd_tx.send(BgCommand::CancelSync);
                    // The user is bailing on the sync — drop any
                    // playlist-creation follow-ups so they don't fire on
                    // whatever partial sync did finish.
                    self.pending_playlist_imports.clear();
                }
                self.toast_message = None;
            }
            _ => {}
        }

        KeyOutcome::Continue
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
                    self.close_track_info();
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
                    self.close_track_info();
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
        self.close_track_info();
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
        self.close_track_info();
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

fn tracks_to_info<'a, I>(
    tracks: I,
    device: &DeviceState,
    local_plays: &LocalPlays,
    device_baseline_key: Option<&str>,
) -> Vec<TrackInfo>
where
    I: IntoIterator<Item = &'a Track>,
{
    tracks
        .into_iter()
        .map(|t| {
            let artist_key = normalize_for_match(&t.artist);
            let name_key = normalize_for_match(&t.name);
            let on_device = device.contains_track(&artist_key, &name_key);
            // Aggregate counters live in the local-plays sidecar; the device
            // delta has already been merged in via `merge_device_observation`
            // by the time we render. The popup row "Plays" shows this number,
            // and the track-table column reads `track.play_count` directly.
            let entry = local_plays.get(t.id);
            let (aggregate_plays, aggregate_skips, last_played) = match entry {
                Some(p) => (
                    Some(p.play_count),
                    if p.skip_count > 0 {
                        Some(p.skip_count)
                    } else {
                        None
                    },
                    if p.last_played_at_ms > 0 {
                        Some(p.last_played_at_ms)
                    } else {
                        None
                    },
                ),
                None => (None, None, None),
            };
            let last_synced_from_device_at_ms = device_baseline_key
                .filter(|_| on_device)
                .and_then(|k| entry?.device_baselines.get(k))
                .map(|b| b.last_synced_at_ms);
            // Rating remains a device-side concept; pull from track_metadata
            // when on-device, else None.
            let rating = if on_device {
                device
                    .track_metadata
                    .get(&(artist_key.clone(), name_key.clone()))
                    .copied()
                    .unwrap_or((None, None))
                    .1
            } else {
                None
            };
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
                play_count: aggregate_plays,
                skip_count: aggregate_skips,
                rating,
                last_played_at_ms: last_played,
                last_synced_from_device_at_ms,
                library_id: Some(t.id),
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

fn device_tracks_to_info(
    tracks: &[DeviceTrackInfo],
    library: Option<&dyn zytunes::library::MusicLibrary>,
    local_plays: &LocalPlays,
    device_baseline_key: Option<&str>,
) -> Vec<TrackInfo> {
    tracks
        .iter()
        .map(|dt| {
            let mut info = TrackInfo::new(
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
            );
            // Try to resolve the matching library track and pull the
            // aggregate count from the sidecar. Falls back to the raw
            // device counter for tracks that aren't in the library — those
            // are still meaningful to the user (a device-only podcast,
            // sideloaded sample) and dropping their plays would surprise.
            // The lookup tries both the raw and `strip_track_number`-stripped
            // form so the displayed aggregate matches what
            // `merge_device_plays_into_local` already folded into the sidecar
            // for filenames like `"01 Song"`.
            let lib_id = library
                .and_then(|lib| resolve_library_id_for_device_track(lib, &dt.artist, &dt.name));
            let entry = lib_id.and_then(|id| local_plays.get(id));
            info.play_count = match entry {
                Some(p) => Some(p.play_count),
                None => dt.play_count,
            };
            info.skip_count = match entry {
                Some(p) if p.skip_count > 0 => Some(p.skip_count),
                Some(_) => None,
                // Device-only tracks still surface their device-side skips —
                // mirrors the play-count fallback above so the two columns
                // don't show inconsistent provenance for sideloaded rows.
                None => dt.skip_count.filter(|&n| n > 0),
            };
            info.last_played_at_ms = match entry {
                Some(p) if p.last_played_at_ms > 0 => Some(p.last_played_at_ms),
                _ => None,
            };
            info.last_synced_from_device_at_ms = device_baseline_key
                .and_then(|k| entry?.device_baselines.get(k))
                .map(|b| b.last_synced_at_ms);
            info.rating = dt.rating;
            info.library_id = lib_id;
            info
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

/// Copy `text` to the system clipboard. Returns an error message on failure
/// (headless session, missing DISPLAY, etc.) so the caller can surface it.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clip = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clip.set_text(text).map_err(|e| e.to_string())
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
    fn format_bytes_compact_cases() {
        assert_eq!(format_bytes_compact(0), "0 B");
        assert_eq!(format_bytes_compact(512), "512 B");
        assert_eq!(format_bytes_compact(1500), "1.5 KB");
        assert_eq!(format_bytes_compact(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_bytes_compact(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn build_now_playing_metadata_none_track_yields_empty() {
        let (year, line) = build_now_playing_metadata(None);
        assert_eq!(year, None);
        assert!(line.is_empty());
    }

    #[test]
    fn build_now_playing_metadata_skips_missing_fields() {
        // A bare-bones Track with only the identity fields populated should
        // produce no marquee segments — every Option is `None` so nothing
        // makes it into the line.
        let t = Track {
            id: 1,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            ..Track::default()
        };
        let (year, line) = build_now_playing_metadata(Some(&t));
        assert_eq!(year, None);
        assert_eq!(line, "");
    }

    #[test]
    fn build_now_playing_metadata_assembles_segments_in_order() {
        let t = Track {
            id: 1,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            year: Some(2023),
            genre: Some("Rock".into()),
            bpm: Some(128),
            initial_key: Some("Am".into()),
            audio_bitrate_kbps: Some(320),
            sample_rate: Some(44_100),
            bit_depth: Some(16),
            channels: Some(2),
            file_size_bytes: Some(8 * 1024 * 1024),
            ..Track::default()
        };
        let (year, line) = build_now_playing_metadata(Some(&t));
        assert_eq!(year, Some(2023));
        // Order locks the user-visible reading flow: musical first, then
        // technical, then file. Renames here will surprise users mid-track.
        assert_eq!(
            line,
            "Genre: Rock | BPM: 128 | Key: Am | 320 kbps | 44.1 kHz | 16-bit | Stereo | 8.0 MB"
        );
    }

    #[test]
    fn build_now_playing_metadata_suppresses_redundant_album_artist() {
        // When album_artist matches artist (common for non-compilation
        // releases) it adds no signal, so we drop it from the marquee
        // rather than padding the line with "Album Artist: <same>".
        let t = Track {
            id: 1,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: Some("artist".into()),
            ..Track::default()
        };
        let (_, line) = build_now_playing_metadata(Some(&t));
        assert!(
            !line.contains("Album Artist"),
            "expected no album-artist segment, got `{}`",
            line
        );
    }

    #[test]
    fn build_now_playing_metadata_includes_distinct_album_artist() {
        // Compilations and split-credit releases still want the album-artist
        // visible — that's how a "Various Artists" marker shows up.
        let t = Track {
            id: 1,
            name: "Song".into(),
            artist: "Sufjan Stevens".into(),
            album: "Compilation".into(),
            album_artist: Some("Various Artists".into()),
            ..Track::default()
        };
        let (_, line) = build_now_playing_metadata(Some(&t));
        assert!(line.contains("Album Artist: Various Artists"), "{line}");
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
                },
                DeviceTrackInfo {
                    name: "Love of My Life".into(),
                    device_path: "/Music/Queen/A Night at the Opera/Love of My Life.mp3".into(),
                    object_id: 12,
                    artist: "Queen".into(),
                    album: "A Night at the Opera".into(),
                    track_number: None,
                    disc_number: None,
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
    fn device_tracks_to_info_propagates_play_count() {
        // End-to-end: a DeviceEntry with a non-zero play_count must reach
        // the rendered TrackInfo.play_count via the index → DeviceTrackInfo
        // → TrackInfo path. Regression for v1.4 enrichment plumbing.
        let mut app = App::new();
        app.device.tracks = vec![
            DeviceEntry {
                play_count: Some(7),
                ..make_device_entry("Artist/Album/played.mp3", 1000)
            },
            DeviceEntry {
                play_count: None,
                ..make_device_entry("Artist/Album/never.mp3", 2000)
            },
        ];
        app.build_device_index();

        let dt = app
            .device
            .album_tracks
            .get(&("Artist".into(), "Album".into()))
            .unwrap();
        let played = dt.iter().find(|t| t.name == "played").unwrap();
        assert_eq!(played.play_count, Some(7));

        let local_plays = LocalPlays::new();
        let infos = device_tracks_to_info(dt, None, &local_plays, None);
        let played = infos.iter().find(|t| t.name == "played").unwrap();
        let never = infos.iter().find(|t| t.name == "never").unwrap();
        // No library, no sidecar entry → falls back to the raw device count.
        assert_eq!(played.play_count, Some(7));
        assert_eq!(never.play_count, None);
    }

    #[test]
    fn device_tracks_to_info_resolves_track_number_prefixed_filenames() {
        // Regression: a device filename like "01 Smells Like Teen Spirit"
        // must resolve to the library title "Smells Like Teen Spirit" so the
        // displayed aggregate matches what `merge_device_plays_into_local`
        // already folded into the sidecar. Without strip_track_number
        // fallback, the lookup misses and the row falls through to the raw
        // device counter — the user sees "7" in device mode but "9" in
        // library mode for the same track.
        let mut app = App::new();
        app.device.tracks = vec![DeviceEntry {
            play_count: Some(7),
            skip_count: Some(2),
            ..make_device_entry(
                "Nirvana/Nevermind/01 Smells Like Teen Spirit.mp3",
                4_000_000,
            )
        }];
        app.build_device_index();

        let dt = app
            .device
            .album_tracks
            .get(&("Nirvana".into(), "Nevermind".into()))
            .unwrap();

        let lib: Box<dyn zytunes::library::MusicLibrary + Send> = Box::new(VecLibrary {
            tracks: vec![zytunes::library::Track {
                id: 42,
                name: "Smells Like Teen Spirit".into(),
                artist: "Nirvana".into(),
                album: "Nevermind".into(),
                ..Default::default()
            }],
        });

        // Sidecar has the post-merge aggregate: 2 prior TUI plays + 7 device.
        let mut local_plays = LocalPlays::new();
        local_plays.record_play(42, 100);
        local_plays.record_play(42, 200);
        local_plays.merge_device_observation(42, "Zune-ABC", 7, 2, 1_000);

        let infos = device_tracks_to_info(dt, Some(&*lib), &local_plays, Some("Zune-ABC"));
        let row = infos.iter().find(|t| t.name.contains("Smells")).unwrap();
        // The aggregate from the sidecar (9 plays, 2 skips), not the raw
        // device counters (7 plays, 2 skips).
        assert_eq!(row.play_count, Some(9));
        assert_eq!(row.skip_count, Some(2));
    }

    #[test]
    fn device_tracks_to_info_skip_count_falls_back_to_device_for_unmatched_rows() {
        // Mirror of the play_count fallback for sideloaded / podcast rows
        // that have no library counterpart. Before the fix, skip_count
        // dropped to None even when the device reported a non-zero value,
        // while play_count correctly fell back to dt.play_count.
        let mut app = App::new();
        app.device.tracks = vec![DeviceEntry {
            play_count: Some(4),
            skip_count: Some(3),
            ..make_device_entry("Podcast/Show/episode-12.mp3", 30_000_000)
        }];
        app.build_device_index();

        let dt = app
            .device
            .album_tracks
            .get(&("Podcast".into(), "Show".into()))
            .unwrap();
        let local_plays = LocalPlays::new();
        let infos = device_tracks_to_info(dt, None, &local_plays, None);
        let row = &infos[0];
        assert_eq!(row.play_count, Some(4), "play_count must fall back");
        assert_eq!(
            row.skip_count,
            Some(3),
            "skip_count fallback must mirror play_count's"
        );
    }

    #[test]
    fn device_tracks_to_info_omits_zero_skip_count() {
        // A device-side skip_count of 0 should suppress the column the same
        // way a sidecar `skip_count == 0` does — keeps the row terse.
        let mut app = App::new();
        app.device.tracks = vec![DeviceEntry {
            play_count: Some(4),
            skip_count: Some(0),
            ..make_device_entry("Artist/Album/never-skipped.mp3", 4_000_000)
        }];
        app.build_device_index();
        let dt = app
            .device
            .album_tracks
            .get(&("Artist".into(), "Album".into()))
            .unwrap();
        let local_plays = LocalPlays::new();
        let infos = device_tracks_to_info(dt, None, &local_plays, None);
        assert_eq!(infos[0].skip_count, None);
    }

    #[test]
    fn track_metadata_index_carries_play_count_and_rating() {
        // After build_device_index, a Library-side lookup keyed on
        // (artist_key, name_key) must surface the device's play_count and
        // rating. This is what `tracks_to_info` reads from to render
        // playcounts in Library mode without flipping into Device mode.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;
        app.device.tracks = vec![
            DeviceEntry {
                play_count: Some(5),
                rating: Some(80),
                ..make_device_entry("Radiohead/OK Computer/Karma Police.mp3", 4_000_000)
            },
            DeviceEntry {
                play_count: None,
                rating: None,
                ..make_device_entry("Radiohead/OK Computer/Airbag.mp3", 5_000_000)
            },
        ];
        app.build_device_index();

        let karma = app
            .device
            .track_metadata
            .get(&(
                normalize_for_match("Radiohead"),
                normalize_for_match("Karma Police"),
            ))
            .copied();
        assert_eq!(karma, Some((Some(5), Some(80))));

        let airbag = app
            .device
            .track_metadata
            .get(&(
                normalize_for_match("Radiohead"),
                normalize_for_match("Airbag"),
            ))
            .copied();
        assert_eq!(airbag, Some((None, None)));
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
            track_metadata: device.track_metadata.clone(),
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
    fn toggle_browse_mode_skips_device_when_disconnected() {
        // No device connected → cycle is Library → Playlists → Library;
        // Device is skipped because there's nothing to browse.
        let mut app = App::new();
        assert_eq!(app.browse_mode, BrowseMode::Library);
        assert_eq!(app.device.status, DeviceStatus::Disconnected);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Playlists);
        assert_eq!(app.active_panel, Panel::Library);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Library);
    }

    #[test]
    fn toggle_browse_mode_includes_device_when_connected() {
        // Connected → cycle is Library → Device → Playlists → Library.
        let mut app = App::new();
        app.device.status = DeviceStatus::Connected;

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Device);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Playlists);

        app.toggle_browse_mode();
        assert_eq!(app.browse_mode, BrowseMode::Library);
    }

    #[test]
    fn v_key_reaches_playlists_without_device_connected() {
        // Regression: an old gate on the `v` handler ("Connect a device
        // first") blocked users from reaching Playlists mode without
        // plugging in a device. `toggle_browse_mode` already skips Device
        // when nothing's connected, so `v` should always be live.
        use crate::audio::AudioCommand;
        use crate::background::BgCommand;
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<BgCommand>();
        let (audio_tx, _audio_rx) = std::sync::mpsc::channel::<AudioCommand>();
        let mut app = App::new();
        assert_eq!(app.browse_mode, BrowseMode::Library);
        assert_eq!(app.device.status, DeviceStatus::Disconnected);

        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('v'),
                crossterm::event::KeyModifiers::empty(),
            ),
            &cmd_tx,
            &audio_tx,
        );

        assert_eq!(app.browse_mode, BrowseMode::Playlists);
        // No "Connect a device first" toast must have fired.
        assert!(
            app.toast_message.is_none()
                || !app
                    .toast_message
                    .as_ref()
                    .unwrap()
                    .0
                    .contains("Connect a device"),
            "should not have shown the device-required toast"
        );
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
    fn clear_device_index_closes_track_info_popup() {
        let mut app = App::new();
        app.show_track_info = true;
        app.track_info_scroll = 7;
        app.track_info_lib = Some(zytunes::library::Track {
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            ..Default::default()
        });

        app.clear_device_index();

        assert!(!app.show_track_info);
        assert_eq!(app.track_info_scroll, 0);
        assert!(app.track_info_lib.is_none());
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

    /// In-memory `MusicLibrary` for tests that need real `Track` values
    /// (popup-refresh path exercises `tracks_by_name`).
    struct VecLibrary {
        tracks: Vec<zytunes::library::Track>,
    }
    impl zytunes::library::MusicLibrary for VecLibrary {
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
            name: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            let owned = name.to_string();
            Box::new(
                self.tracks
                    .iter()
                    .filter(move |t| t.name.eq_ignore_ascii_case(&owned)),
            )
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
    fn library_loaded_refreshes_open_track_info_popup() {
        let mut app = App::new();

        // Stage 1: initial library with bpm=120 for "Song".
        let initial_track = zytunes::library::Track {
            id: 1,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            bpm: Some(120),
            ..Default::default()
        };
        app.library = Some(Box::new(VecLibrary {
            tracks: vec![initial_track],
        }));
        app.track_list.push(TrackInfo::new(
            "Song".into(),
            "Artist".into(),
            "Album".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        ));
        app.track_selected = 0;
        app.open_track_info();
        assert!(app.show_track_info);
        assert_eq!(app.track_info_lib.as_ref().and_then(|t| t.bpm), Some(120));

        // Stage 2: deliver a new library with bpm=140 for the same track.
        let updated_track = zytunes::library::Track {
            id: 1,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            bpm: Some(140),
            ..Default::default()
        };
        let new_lib: Box<dyn zytunes::library::MusicLibrary + Send> = Box::new(VecLibrary {
            tracks: vec![updated_track],
        });
        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(new_lib)));

        // Popup should remain open and now reflect the freshly-scanned bpm.
        assert!(app.show_track_info);
        assert_eq!(app.track_info_lib.as_ref().and_then(|t| t.bpm), Some(140));
    }

    #[test]
    fn library_loaded_clears_track_info_lib_when_popup_closed() {
        let mut app = App::new();
        // Simulate a stale cached lib track left over from a prior popup.
        app.show_track_info = false;
        app.track_info_lib = Some(zytunes::library::Track {
            name: "Stale".into(),
            artist: "A".into(),
            album: "B".into(),
            bpm: Some(99),
            ..Default::default()
        });

        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(make_minimal_library())));

        assert!(app.track_info_lib.is_none());
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

    // -- Track-info popup tests --

    fn populate_track_list(app: &mut App, names: &[&str]) {
        for n in names {
            app.track_list.push(TrackInfo::new(
                (*n).to_string(),
                "Artist".into(),
                "Album".into(),
                None,
                None,
                None,
                None,
                None,
                None,
                false,
            ));
        }
    }

    #[test]
    fn track_info_open_noop_on_empty_list() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        app.open_track_info();
        assert!(
            !app.show_track_info,
            "open_track_info on an empty list must stay closed"
        );
    }

    #[test]
    fn track_info_open_sets_visible() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque"]);
        app.open_track_info();
        assert!(app.show_track_info);
        assert_eq!(app.track_info_scroll, 0);
    }

    #[test]
    fn track_info_close_resets_scroll() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque"]);
        app.open_track_info();
        app.track_info_move(5);
        app.close_track_info();
        assert!(!app.show_track_info);
        assert_eq!(app.track_info_scroll, 0);
    }

    #[test]
    fn track_info_scroll_does_not_underflow() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque"]);
        app.open_track_info();
        app.track_info_move(-1);
        app.track_info_move(-5);
        assert_eq!(app.track_info_scroll, 0);
    }

    #[test]
    fn track_info_home_resets_to_top() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque"]);
        app.open_track_info();
        app.track_info_move(8);
        app.track_info_home();
        assert_eq!(app.track_info_scroll, 0);
    }

    #[test]
    fn track_info_closes_on_panel_cycle() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque"]);
        app.open_track_info();
        app.cycle_panel();
        assert!(
            !app.show_track_info,
            "cycle_panel should dismiss the popup since selection context is changing"
        );
    }

    #[test]
    fn track_info_closes_on_track_selection_change() {
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        populate_track_list(&mut app, &["Idioteque", "Optimistic"]);
        app.track_selected = 0;
        app.open_track_info();
        app.move_down();
        assert!(
            !app.show_track_info,
            "moving the track-list selection should dismiss the popup"
        );
    }

    /// Bare-bones in-memory `MusicLibrary` for tests that need to exercise
    /// the popup's library-lookup cache. Mirrors the shape of `TestLibrary`
    /// in `src/lib.rs`, kept tiny — only the methods the tests touch.
    struct PopupTestLib {
        tracks: Vec<zytunes::library::Track>,
    }

    impl zytunes::library::MusicLibrary for PopupTestLib {
        fn artists(&self) -> Vec<&str> {
            Vec::new()
        }
        fn albums(&self) -> Vec<(&str, &str)> {
            Vec::new()
        }
        fn artist_tracks<'a>(
            &'a self,
            _artist: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn album_tracks<'a>(
            &'a self,
            _album: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn album_tracks_by_artist<'a>(
            &'a self,
            _artist: &str,
            _album: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            Box::new(std::iter::empty())
        }
        fn tracks_by_name<'a>(
            &'a self,
            name: &str,
        ) -> Box<dyn Iterator<Item = &'a zytunes::library::Track> + 'a> {
            let name = name.to_string();
            Box::new(
                self.tracks
                    .iter()
                    .filter(move |t| t.name.eq_ignore_ascii_case(&name)),
            )
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

    #[test]
    fn track_info_open_caches_library_track() {
        // The popup renderer reads `app.track_info_lib` rather than walking
        // the library on every frame. Verify the cached lookup is populated
        // on open (matching by case-insensitive artist+album+name) and
        // cleared on close.
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        let lib = PopupTestLib {
            tracks: vec![
                zytunes::library::Track {
                    id: 1,
                    name: "Idioteque".into(),
                    artist: "Radiohead".into(),
                    album: "Kid A".into(),
                    composer: Some("Thom Yorke".into()),
                    ..Default::default()
                },
                zytunes::library::Track {
                    id: 2,
                    name: "Decoy".into(),
                    artist: "Other".into(),
                    album: "Other".into(),
                    ..Default::default()
                },
            ],
        };
        app.library = Some(Box::new(lib));
        app.track_list.push(TrackInfo::new(
            "Idioteque".into(),
            "RADIOHEAD".into(), // case mismatch on purpose
            "Kid A".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        ));
        app.track_selected = 0;

        app.open_track_info();
        let cached = app
            .track_info_lib
            .as_ref()
            .expect("library lookup should resolve to a Track on open");
        assert_eq!(cached.id, 1);
        assert_eq!(cached.composer.as_deref(), Some("Thom Yorke"));

        app.close_track_info();
        assert!(
            app.track_info_lib.is_none(),
            "close_track_info must drop the cached library reference"
        );
    }

    #[test]
    fn track_info_open_caches_none_when_no_match() {
        // Track is in the TUI list but does not exist in the library — the
        // cache should be `None`, not panic, and the popup still opens with
        // just the TrackInfo identity fields.
        let mut app = App::new();
        app.active_panel = Panel::TrackList;
        app.library = Some(Box::new(PopupTestLib { tracks: Vec::new() }));
        populate_track_list(&mut app, &["Orphan"]);
        app.open_track_info();
        assert!(app.show_track_info);
        assert!(app.track_info_lib.is_none());
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
                ..Default::default()
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
                    play_count: None,
                    rating: None,
                    skip_count: None,
                },
                DeviceTrackInfo {
                    name: "Karma Police".into(),
                    device_path: "/Music/Radiohead/OK Computer/Karma Police.mp3".into(),
                    object_id: 2,
                    artist: "Radiohead".into(),
                    album: "OK Computer".into(),
                    track_number: None,
                    disc_number: None,
                    play_count: None,
                    rating: None,
                    skip_count: None,
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
                play_count: None,
                rating: None,
                skip_count: None,
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
            track_id: None,
            counted: false,
            year: None,
            metadata_marquee: String::new(),
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
            track_id: None,
            counted: false,
            year: None,
            metadata_marquee: String::new(),
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
            track_id: None,
            counted: false,
            year: None,
            metadata_marquee: String::new(),
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

    // -- local-plays wiring (Phase 3 commit 2) --

    /// Build an `App` with a one-track library and a `now_playing` session
    /// pointing at it. Disk persistence is disabled (`local_plays_save_path =
    /// None`) so tests don't pollute `~/.cache/zytunes/`. The playlist is
    /// populated with one matching `TrackInfo` so transition paths
    /// (`next_track`/`prev_track`) don't index into an empty slice.
    fn app_with_now_playing(track_id: u64, duration_ms: u64) -> App {
        let mut app = App::new();
        app.local_plays_save_path = None;
        let track = zytunes::library::Track {
            id: track_id,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            total_time_ms: Some(duration_ms),
            ..Default::default()
        };
        app.library = Some(Box::new(VecLibrary {
            tracks: vec![track],
        }));
        let info = TrackInfo::new(
            "Song".into(),
            "Artist".into(),
            "Album".into(),
            Some(duration_ms),
            None,
            Some("/tmp/song.mp3".into()),
            None,
            None,
            None,
            false,
        );
        let playlist: Arc<[TrackInfo]> = Arc::from(vec![info]);
        app.now_playing = Some(NowPlaying {
            track_name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist,
            paused_frame: None,
            track_id: Some(track_id),
            counted: false,
            year: None,
            metadata_marquee: String::new(),
        });
        app
    }

    fn dummy_audio_tx() -> mpsc::Sender<AudioCommand> {
        let (tx, _rx) = mpsc::channel::<AudioCommand>();
        tx
    }

    #[test]
    fn end_to_end_play_records_and_propagates_last_played() {
        // Mirror the production flow as closely as a unit test can: build a
        // library track, run it through `tracks_to_info` so library_id is
        // populated by the real builder, install as track_list, then play it
        // past the threshold via the audio event handler. Both `play_count`
        // and `last_played_at_ms` should be Some on the visible row, and
        // `format_metadata_pairs` should emit a "Last played" entry.
        let mut app = App::new();
        let track = zytunes::library::Track {
            id: 4242,
            name: "Track".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            total_time_ms: Some(240_000),
            location: Some("/tmp/track.mp3".into()),
            ..Default::default()
        };
        app.library = Some(Box::new(VecLibrary {
            tracks: vec![track.clone()],
        }));
        // Build track_list through the real builder so library_id wiring is
        // exercised end-to-end.
        let lib_ref = app.library.as_deref().unwrap();
        let device_key = app.current_device_baseline_key();
        app.track_list = tracks_to_info(
            std::iter::once(&track),
            &app.device,
            &app.local_plays,
            device_key.as_deref(),
        );
        assert_eq!(app.track_list.len(), 1);
        assert_eq!(app.track_list[0].library_id, Some(4242));
        assert!(app.track_list[0].last_played_at_ms.is_none());
        // Borrow check: drop the immutable lib_ref before we touch app
        // again.
        let _ = lib_ref;

        app.track_selected = 0;
        let tx = dummy_audio_tx();
        app.play_selected_track(&tx);
        assert_eq!(
            app.now_playing.as_ref().and_then(|np| np.track_id),
            Some(4242),
            "now_playing.track_id must resolve via resolve_library_track_id"
        );
        // Past threshold (50% of 240s = 120s).
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(4242).unwrap().play_count, 1);
        assert!(
            app.local_plays.get(4242).unwrap().last_played_at_ms > 0,
            "sidecar must record a non-zero last_played_at_ms"
        );
        assert_eq!(app.track_list[0].play_count, Some(1));
        assert!(
            app.track_list[0].last_played_at_ms.is_some(),
            "live refresh must push last_played into the visible row"
        );

        let pairs =
            crate::ui::format_metadata_pairs(&app.track_list[0], app.track_info_lib.as_ref());
        let keys: Vec<&str> = pairs
            .iter()
            .filter_map(|r| match r {
                crate::ui::MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(
            keys.contains(&"Last played"),
            "popup formatter must emit a Last played row after a TUI play"
        );
    }

    #[test]
    fn record_play_refreshes_visible_track_list_row() {
        // Regression: recording a play must push the new aggregate into any
        // visible `track_list` row tagged with the same `library_id`. Without
        // the live refresh, the popup / track table kept showing the
        // pre-bump number until the user navigated away to force a rebuild.
        let mut app = app_with_now_playing(42, 240_000);
        let mut info = TrackInfo::new(
            "Song".into(),
            "Artist".into(),
            "Album".into(),
            Some(240_000),
            None,
            Some("/tmp/song.mp3".into()),
            None,
            None,
            None,
            false,
        );
        info.library_id = Some(42);
        info.play_count = Some(0);
        app.track_list = vec![info];
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert_eq!(
            app.track_list[0].play_count,
            Some(1),
            "track_list row must reflect the just-recorded play live"
        );
        assert!(
            app.track_list[0].last_played_at_ms.is_some(),
            "last_played_at_ms must update live"
        );
    }

    #[test]
    fn record_skip_refreshes_visible_track_list_row() {
        let mut app = app_with_now_playing(99, 240_000);
        let mut info = TrackInfo::new(
            "Song".into(),
            "Artist".into(),
            "Album".into(),
            Some(240_000),
            None,
            Some("/tmp/song.mp3".into()),
            None,
            None,
            None,
            false,
        );
        info.library_id = Some(99);
        info.skip_count = None;
        app.track_list = vec![info];
        let tx = dummy_audio_tx();
        // Listen briefly then skip.
        app.handle_audio_event(AudioEvent::Position { elapsed_ms: 5_000 }, &tx);
        app.stop_playback(&tx);
        assert_eq!(
            app.track_list[0].skip_count,
            Some(1),
            "track_list row must reflect the just-recorded skip live"
        );
    }

    #[test]
    fn refresh_play_stats_for_id_only_touches_matching_rows() {
        // Multiple rows in track_list with different library_ids: refreshing
        // one must not stomp the others.
        let mut app = app_with_now_playing(1, 240_000);
        let mk = |id: u64, plays: u32| {
            let mut t = TrackInfo::new(
                format!("Song-{id}"),
                "Artist".into(),
                "Album".into(),
                Some(240_000),
                None,
                Some("/tmp/song.mp3".into()),
                None,
                None,
                None,
                false,
            );
            t.library_id = Some(id);
            t.play_count = Some(plays);
            t
        };
        app.track_list = vec![mk(1, 0), mk(2, 5), mk(3, 7)];
        app.local_plays.record_play(1, 1_000);
        app.refresh_play_stats_for_id(1);
        assert_eq!(app.track_list[0].play_count, Some(1), "row 1 refreshed");
        assert_eq!(
            app.track_list[1].play_count,
            Some(5),
            "row 2 untouched (library_id mismatch)"
        );
        assert_eq!(
            app.track_list[2].play_count,
            Some(7),
            "row 3 untouched (library_id mismatch)"
        );
    }

    #[test]
    fn position_at_threshold_records_play_once() {
        // 4-min track: threshold is 2 min. First Position past threshold
        // should bump play_count to 1; subsequent ones don't double-count.
        let mut app = app_with_now_playing(42, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 119_999,
            },
            &tx,
        );
        assert_eq!(
            app.local_plays.get(42).map(|p| p.play_count).unwrap_or(0),
            0,
            "below threshold must not count"
        );

        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 120_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 1);

        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 200_000,
            },
            &tx,
        );
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 230_000,
            },
            &tx,
        );
        assert_eq!(
            app.local_plays.get(42).unwrap().play_count,
            1,
            "post-threshold ticks must not double-count"
        );
    }

    #[test]
    fn position_threshold_caps_at_four_minutes() {
        // 20-min track: 50% would be 10 min, but cap is 4 min.
        let mut app = app_with_now_playing(7, 1_200_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 239_999,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(7).map(|p| p.play_count).unwrap_or(0), 0);
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 240_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(7).unwrap().play_count, 1);
    }

    #[test]
    fn track_ended_records_play_for_short_clips() {
        // 1.5s clip: threshold is 750ms but Position events poll at ~250ms,
        // so a tiny race could miss the threshold. TrackEnded is the safety
        // net.
        let mut app = app_with_now_playing(99, 1_500);
        let tx = dummy_audio_tx();
        app.handle_audio_event(AudioEvent::TrackEnded, &tx);
        assert_eq!(app.local_plays.get(99).unwrap().play_count, 1);
    }

    #[test]
    fn track_ended_after_play_recorded_is_idempotent() {
        let mut app = app_with_now_playing(1, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(1).unwrap().play_count, 1);
        // Track ends after threshold; play_count stays 1.
        app.handle_audio_event(AudioEvent::TrackEnded, &tx);
        assert_eq!(app.local_plays.get(1).unwrap().play_count, 1);
    }

    #[test]
    fn next_before_threshold_records_skip() {
        let mut app = app_with_now_playing(5, 240_000);
        let tx = dummy_audio_tx();
        // Listen for 30s, then skip.
        app.handle_audio_event(AudioEvent::Position { elapsed_ms: 30_000 }, &tx);
        app.next_track(&tx);
        let entry = app.local_plays.get(5).unwrap();
        assert_eq!(entry.play_count, 0);
        assert_eq!(entry.skip_count, 1);
    }

    #[test]
    fn next_after_threshold_does_not_record_skip() {
        let mut app = app_with_now_playing(5, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        // Skipping after the threshold is a play, not a skip.
        app.next_track(&tx);
        let entry = app.local_plays.get(5).unwrap();
        assert_eq!(entry.play_count, 1);
        assert_eq!(entry.skip_count, 0);
    }

    #[test]
    fn prev_before_3s_with_index_zero_restarts_no_skip() {
        // Pressing prev within the first 3s with track_index=0 restarts
        // the same track — same session, no skip recorded.
        let mut app = app_with_now_playing(8, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(AudioEvent::Position { elapsed_ms: 1_000 }, &tx);
        app.prev_track(&tx);
        let entry = app.local_plays.get(8);
        assert!(
            entry.is_none() || entry.unwrap().skip_count == 0,
            "restart-current must not record a skip"
        );
    }

    #[test]
    fn stop_before_threshold_records_skip() {
        let mut app = app_with_now_playing(11, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(AudioEvent::Position { elapsed_ms: 10_000 }, &tx);
        app.stop_playback(&tx);
        assert_eq!(app.local_plays.get(11).unwrap().skip_count, 1);
        assert!(app.now_playing.is_none());
    }

    #[test]
    fn playback_error_records_neither() {
        let mut app = app_with_now_playing(13, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(AudioEvent::Position { elapsed_ms: 30_000 }, &tx);
        app.handle_audio_event(AudioEvent::PlaybackError("boom".into()), &tx);
        let entry = app.local_plays.get(13);
        assert!(
            entry.is_none() || (entry.unwrap().play_count == 0 && entry.unwrap().skip_count == 0)
        );
        assert!(app.now_playing.is_none());
    }

    #[test]
    fn replay_after_stop_starts_fresh_session() {
        // After stop+replay, the second listen-through should bump
        // play_count again (not be blocked by the prior session's flag).
        let mut app = app_with_now_playing(17, 240_000);
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(17).unwrap().play_count, 1);

        // Simulate stop + manual replay (the production path goes through
        // play_selected_track; here we install a fresh NowPlaying directly).
        app.now_playing = Some(NowPlaying {
            track_name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 240_000,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist: Arc::from([] as [TrackInfo; 0]),
            paused_frame: None,
            track_id: Some(17),
            counted: false,
            year: None,
            metadata_marquee: String::new(),
        });
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert_eq!(app.local_plays.get(17).unwrap().play_count, 2);
    }

    #[test]
    fn no_track_id_skips_recording_silently() {
        // Device-mode rows don't resolve a library track_id. The threshold
        // must still flip `counted` (so we don't keep checking), but
        // local_plays stays empty.
        let mut app = app_with_now_playing(0, 240_000);
        if let Some(np) = app.now_playing.as_mut() {
            np.track_id = None;
        }
        let tx = dummy_audio_tx();
        app.handle_audio_event(
            AudioEvent::Position {
                elapsed_ms: 130_000,
            },
            &tx,
        );
        assert!(app.local_plays.is_empty());
        assert!(app.now_playing.as_ref().unwrap().counted);
    }

    #[test]
    fn play_selected_track_resolves_track_number_prefixed_name() {
        // Regression: a device-mode TrackInfo with name "01 Song" must
        // resolve to the library track titled "Song" so the play actually
        // gets recorded. Before the fix, `resolve_library_track_id` did an
        // exact name lookup and returned None for any track-number-prefixed
        // filename, silently dropping the play.
        let mut app = App::new();
        let track = zytunes::library::Track {
            id: 99,
            name: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            total_time_ms: Some(240_000),
            ..Default::default()
        };
        app.library = Some(Box::new(VecLibrary {
            tracks: vec![track],
        }));
        // Build a TrackInfo as device_tracks_to_info would: name carries
        // the leading track-number prefix from the device filename.
        let info = TrackInfo::new(
            "01 Song".into(),
            "Artist".into(),
            "Album".into(),
            Some(240_000),
            None,
            Some("/tmp/song.mp3".into()),
            None,
            None,
            None,
            false,
        );
        app.track_list = vec![info];
        app.track_selected = 0;
        let tx = dummy_audio_tx();
        app.play_selected_track(&tx);
        assert_eq!(
            app.now_playing.as_ref().and_then(|np| np.track_id),
            Some(99),
            "device-mode TrackInfo with track-number prefix must still resolve a library id"
        );
    }

    // -- merge_device_plays_into_local (Phase 3 commit 4) --

    fn app_with_library_for_merge() -> App {
        let mut app = App::new();
        app.local_plays_save_path = None;
        let track = zytunes::library::Track {
            id: 42,
            name: "Smells Like Teen Spirit".into(),
            artist: "Nirvana".into(),
            album: "Nevermind".into(),
            ..Default::default()
        };
        app.library = Some(Box::new(VecLibrary {
            tracks: vec![track],
        }));
        app.device.family = Some(zytunes::device::DeviceFamily::Zune);
        app.device.serial = Some("ABC123".into());
        app
    }

    fn matching_device_track(play: u32, skip: u32) -> DeviceEntry {
        DeviceEntry {
            object_id: 9001,
            storage_id: 65537,
            format: "MP3".to_string(),
            name: "Nirvana/Nevermind/01 Smells Like Teen Spirit.mp3".to_string(),
            play_count: Some(play),
            skip_count: Some(skip),
            ..Default::default()
        }
    }

    #[test]
    fn merge_walks_user_example() {
        // The user's spec: 2 TUI plays, then connect device with 7 plays
        // → aggregate 9. Reconnect with no change → still 9.
        let mut app = app_with_library_for_merge();
        app.local_plays.record_play(42, 100);
        app.local_plays.record_play(42, 200);
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 2);

        app.device.tracks = vec![matching_device_track(7, 0)];
        app.merge_device_plays_into_local();
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 9);

        // Reconnect, device still reports 7 — count holds at 9.
        app.merge_device_plays_into_local();
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 9);
    }

    #[test]
    fn merge_subsequent_observation_uses_delta_only() {
        let mut app = app_with_library_for_merge();
        app.device.tracks = vec![matching_device_track(5, 0)];
        app.merge_device_plays_into_local();
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 5);

        // Device went 5 → 8 between syncs.
        app.device.tracks = vec![matching_device_track(8, 0)];
        app.merge_device_plays_into_local();
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 8);
    }

    #[test]
    fn merge_handles_track_number_prefix_in_filename() {
        // Library Track.name is "Smells Like Teen Spirit" (from ID3 title);
        // device filename is "01 Smells Like Teen Spirit.mp3". They must
        // still match via `strip_track_number` normalization.
        let mut app = app_with_library_for_merge();
        app.device.tracks = vec![matching_device_track(3, 1)];
        app.merge_device_plays_into_local();
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 3);
        assert_eq!(app.local_plays.get(42).unwrap().skip_count, 1);
    }

    #[test]
    fn merge_no_op_without_library() {
        let mut app = App::new();
        app.local_plays_save_path = None;
        app.device.family = Some(zytunes::device::DeviceFamily::Zune);
        app.device.tracks = vec![matching_device_track(5, 0)];
        app.merge_device_plays_into_local();
        // No library yet — nothing should land in the sidecar.
        assert!(app.local_plays.is_empty());
    }

    #[test]
    fn merge_no_op_without_device_family() {
        let mut app = app_with_library_for_merge();
        app.device.family = None; // device hasn't connected
        app.device.tracks = vec![matching_device_track(5, 0)];
        app.merge_device_plays_into_local();
        assert!(app.local_plays.is_empty());
    }

    #[test]
    fn merge_no_op_when_no_library_match() {
        let mut app = app_with_library_for_merge();
        app.device.tracks = vec![DeviceEntry {
            object_id: 5000,
            storage_id: 65537,
            format: "MP3".into(),
            name: "Not An Artist/Some Album/track.mp3".into(),
            play_count: Some(10),
            skip_count: Some(2),
            ..Default::default()
        }];
        app.merge_device_plays_into_local();
        assert!(app.local_plays.is_empty());
    }

    #[test]
    fn library_loaded_after_device_runs_merge() {
        // Order-of-events test: device tracks land before library finishes
        // scanning. The DeviceTracksLoaded handler's merge no-ops (no
        // library yet); the LibraryLoaded handler's merge catches up.
        let mut app = App::new();
        app.local_plays_save_path = None;
        app.device.family = Some(zytunes::device::DeviceFamily::Zune);
        app.device.serial = Some("ABC123".into());
        app.handle_bg_event(BgEvent::DeviceTracksLoaded(vec![matching_device_track(
            7, 0,
        )]));
        assert!(app.local_plays.is_empty(), "merge before library = no-op");

        let lib: Box<dyn zytunes::library::MusicLibrary + Send> = Box::new(VecLibrary {
            tracks: vec![zytunes::library::Track {
                id: 42,
                name: "Smells Like Teen Spirit".into(),
                artist: "Nirvana".into(),
                album: "Nevermind".into(),
                ..Default::default()
            }],
        });
        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(lib)));
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 7);
    }

    #[test]
    fn device_tracks_after_library_runs_merge() {
        // Reverse order: library loads first, device follows. The
        // DeviceTracksLoaded handler's merge does the work.
        let mut app = App::new();
        app.local_plays_save_path = None;
        app.device.family = Some(zytunes::device::DeviceFamily::Zune);
        app.device.serial = Some("ABC123".into());

        let lib: Box<dyn zytunes::library::MusicLibrary + Send> = Box::new(VecLibrary {
            tracks: vec![zytunes::library::Track {
                id: 42,
                name: "Smells Like Teen Spirit".into(),
                artist: "Nirvana".into(),
                album: "Nevermind".into(),
                ..Default::default()
            }],
        });
        app.handle_bg_event(BgEvent::LibraryLoaded(Ok(lib)));
        assert!(app.local_plays.is_empty(), "no device tracks yet");

        app.handle_bg_event(BgEvent::DeviceTracksLoaded(vec![matching_device_track(
            7, 0,
        )]));
        assert_eq!(app.local_plays.get(42).unwrap().play_count, 7);
    }

    // ------------------------------------------------------------------
    // Playlist integration: sidebar build, selection, sync queue, modals.
    // The unit tests on `Playlist` and `PlaylistStore` verify the data
    // layer; these tests verify the TUI wiring sits on top of them
    // correctly without re-testing the data layer's invariants.
    // ------------------------------------------------------------------

    fn lib_with_tracks(rows: &[(u64, &str, &str, &str)]) -> Box<dyn MusicLibrary + Send> {
        let tracks: Vec<Track> = rows
            .iter()
            .map(|(id, artist, album, name)| Track {
                id: *id,
                name: (*name).to_string(),
                artist: (*artist).to_string(),
                album: (*album).to_string(),
                location: Some(format!("/tmp/{}.mp3", id)),
                ..Default::default()
            })
            .collect();
        Box::new(VecLibrary { tracks })
    }

    #[test]
    fn playlists_browse_mode_sidebar_lists_playlists() {
        let mut app = App::new();
        app.playlists.add(Playlist::new_manual("Faves"));
        app.playlists.add(Playlist::new_manual("Workout"));
        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();

        assert_eq!(app.sidebar_items.len(), 2);
        for entry in &app.sidebar_items {
            assert!(matches!(entry, SidebarEntry::Playlist { .. }));
        }
    }

    #[test]
    fn playlist_sidebar_sort_puts_generated_first() {
        let mut app = App::new();
        // Manual playlist created first → would otherwise lead by recency.
        app.playlists.add(Playlist::new_manual("Manual A"));
        // Sleep is overkill, but the second playlist may share `now_ms`;
        // bump its updated_at so the sort tie-break is unambiguous.
        let gen_id = app.playlists.add(Playlist::new_generated(
            "Gen B",
            zytunes::playlist::GenerationParams::default_discover_weekly(),
            vec![1, 2],
        ));
        if let Some(p) = app.playlists.get_mut(gen_id) {
            p.updated_at_ms = 100; // older than the manual one
        }
        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();

        // Generated must lead even though it was updated earlier than the
        // manual one — the sort prioritises kind first, then recency.
        match &app.sidebar_items[0] {
            SidebarEntry::Playlist { name, .. } => assert_eq!(name, "Gen B"),
            other => panic!("expected playlist first, got {other:?}"),
        }
    }

    #[test]
    fn select_playlist_populates_track_list_in_playlist_order() {
        let mut app = App::new();
        // Library has three tracks with IDs 10, 20, 30.
        app.library = Some(lib_with_tracks(&[
            (10, "A", "Album", "First"),
            (20, "A", "Album", "Second"),
            (30, "A", "Album", "Third"),
        ]));
        // Build a playlist that orders them backwards.
        let id = app.playlists.add(Playlist::new_manual("Backwards"));
        app.playlists.add_track(id, 30);
        app.playlists.add_track(id, 20);
        app.playlists.add_track(id, 10);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.select_sidebar_item();

        let names: Vec<&str> = app.track_list.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Third", "Second", "First"]);
    }

    #[test]
    fn select_playlist_skips_unresolvable_track_ids_silently() {
        // Library has 10 and 30; the playlist references 20 which is gone.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (10, "A", "Album", "First"),
            (30, "A", "Album", "Third"),
        ]));
        let id = app.playlists.add(Playlist::new_manual("MixedRefs"));
        app.playlists.add_track(id, 10);
        app.playlists.add_track(id, 20);
        app.playlists.add_track(id, 30);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.select_sidebar_item();

        // Only the resolvable IDs surface; the unresolvable one is dropped.
        assert_eq!(app.track_list.len(), 2);
        assert_eq!(app.track_list[0].name, "First");
        assert_eq!(app.track_list[1].name, "Third");
    }

    #[test]
    fn add_sidebar_playlist_to_queue_includes_all_resolvable_tracks() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (10, "A", "Alb", "T1"),
            (20, "B", "Alb", "T2"),
        ]));
        let id = app.playlists.add(Playlist::new_manual("Mix"));
        app.playlists.add_track(id, 10);
        app.playlists.add_track(id, 20);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.add_sidebar_item_to_queue();

        assert_eq!(app.sync.queue.len(), 1);
        let item = &app.sync.queue[0];
        assert_eq!(item.label, "Mix");
        assert_eq!(item.tracks.len(), 2);
        // Order is preserved.
        assert_eq!(item.tracks[0].name, "T1");
        assert_eq!(item.tracks[1].name, "T2");
    }

    #[test]
    fn commit_playlist_name_input_creates_then_renames() {
        let mut app = App::new();
        // Create.
        app.playlist_name_input = Some("First".to_string());
        app.playlist_rename_target = None;
        app.commit_playlist_name_input();
        assert_eq!(app.playlists.len(), 1);
        let id = app.playlists.playlists()[0].id;
        assert_eq!(app.playlists.get(id).unwrap().name, "First");

        // Rename.
        app.playlist_name_input = Some("Renamed".to_string());
        app.playlist_rename_target = Some(id);
        app.commit_playlist_name_input();
        assert_eq!(app.playlists.get(id).unwrap().name, "Renamed");
        assert_eq!(app.playlists.len(), 1, "rename must not duplicate");
    }

    #[test]
    fn commit_playlist_name_input_rejects_empty_name() {
        let mut app = App::new();
        app.playlist_name_input = Some("   ".to_string());
        app.commit_playlist_name_input();
        assert!(app.playlists.is_empty());
        assert!(
            app.toast_message.as_ref().unwrap().2,
            "should be error toast"
        );
    }

    #[test]
    fn commit_playlist_delete_removes_and_clamps_selection() {
        let mut app = App::new();
        let id_a = app.playlists.add(Playlist::new_manual("A"));
        let _id_b = app.playlists.add(Playlist::new_manual("B"));
        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 1; // pointing at the second row

        app.commit_playlist_delete(id_a);

        assert_eq!(app.playlists.len(), 1);
        // Selection must be clamped — only one row left.
        assert!(app.sidebar_selected < app.sidebar_items.len());
    }

    #[test]
    fn remove_selected_track_from_playlist_drops_it() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (10, "A", "Alb", "T1"),
            (20, "A", "Alb", "T2"),
        ]));
        let id = app.playlists.add(Playlist::new_manual("Mix"));
        app.playlists.add_track(id, 10);
        app.playlists.add_track(id, 20);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.select_sidebar_item();
        app.track_selected = 1; // pointing at T2

        app.remove_selected_track_from_playlist();

        assert_eq!(app.playlists.get(id).unwrap().track_ids, vec![10]);
        assert_eq!(app.track_list.len(), 1);
        assert_eq!(app.track_list[0].name, "T1");
    }

    #[test]
    fn add_to_playlist_picker_requires_existing_playlists() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(10, "A", "Alb", "T1")]));
        app.track_list = vec![TrackInfo {
            name: "T1".into(),
            artist: "A".into(),
            album: "Alb".into(),
            duration_ms: None,
            kind: None,
            location: Some("/tmp/10.mp3".into()),
            track_number: None,
            disc_number: None,
            genre: None,
            on_device: false,
            play_count: None,
            skip_count: None,
            rating: None,
            last_played_at_ms: None,
            last_synced_from_device_at_ms: None,
            library_id: Some(10),
            artist_key: "a".into(),
            name_key: "t1".into(),
        }];
        app.track_selected = 0;

        app.open_add_to_playlist_picker();
        assert!(
            app.add_to_playlist_picker.is_none(),
            "no playlists yet → picker must not open"
        );
        assert!(
            app.toast_message.is_some(),
            "should toast about missing playlists"
        );
    }

    #[test]
    fn add_to_playlist_picker_confirm_adds_track() {
        let mut app = App::new();
        let pid = app.playlists.add(Playlist::new_manual("Faves"));
        app.library = Some(lib_with_tracks(&[(10, "A", "Alb", "T1")]));
        app.track_list = vec![TrackInfo {
            name: "T1".into(),
            artist: "A".into(),
            album: "Alb".into(),
            duration_ms: None,
            kind: None,
            location: Some("/tmp/10.mp3".into()),
            track_number: None,
            disc_number: None,
            genre: None,
            on_device: false,
            play_count: None,
            skip_count: None,
            rating: None,
            last_played_at_ms: None,
            last_synced_from_device_at_ms: None,
            library_id: Some(10),
            artist_key: "a".into(),
            name_key: "t1".into(),
        }];
        app.track_selected = 0;

        app.open_add_to_playlist_picker();
        assert!(app.add_to_playlist_picker.is_some());
        app.confirm_add_to_playlist();

        assert!(app.add_to_playlist_picker.is_none());
        assert_eq!(app.playlists.get(pid).unwrap().track_ids, vec![10]);
    }

    // ------------------------------------------------------------------
    // Phase 2: Generation form + recommender integration tests.
    // ------------------------------------------------------------------

    #[test]
    fn generation_form_default_round_trips_to_params() {
        let f = GenerationFormState::discover_weekly_default(10_000);
        let p = f.to_params();
        assert_eq!(p.target_length, 25);
        assert!(p.exclude_on_device);
        // Strategy round-trips back to TopPlayed.
        assert!(matches!(p.seed_strategy, SeedStrategy::TopPlayed { .. }));
    }

    #[test]
    fn generation_form_for_track_seed_carries_id() {
        let f = GenerationFormState::for_track_seed(10_000, 42, "Roygbiv");
        let p = f.to_params();
        match p.seed_strategy {
            SeedStrategy::Track(id) => assert_eq!(id, 42),
            other => panic!("expected Track seed, got {other:?}"),
        }
    }

    #[test]
    fn generation_form_for_regenerate_returns_none_for_manual() {
        let p = Playlist::new_manual("Manual");
        assert!(GenerationFormState::for_regenerate(&p).is_none());
    }

    #[test]
    fn generation_form_for_regenerate_preserves_params() {
        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 7;
        let pl = Playlist::new_generated("DW", params.clone(), vec![1, 2, 3]);
        let f = GenerationFormState::for_regenerate(&pl).unwrap();
        assert_eq!(f.target_length, 7);
        assert_eq!(f.regenerating, Some(pl.id));
        // round-trip back through `to_params` matches the source.
        let round = f.to_params();
        assert_eq!(round.target_length, params.target_length);
    }

    #[test]
    fn commit_generation_form_creates_playlist_with_recommender_output() {
        let mut app = App::new();
        // Library: 6 tracks across 3 artists with one heavily-played seed.
        app.library = Some(lib_with_tracks(&[
            (1, "BoC", "Geogaddi", "Music Is Math"),
            (2, "BoC", "Geogaddi", "Gyroscope"),
            (3, "BoC", "Music Has The Right", "Roygbiv"),
            (4, "Aphex", "SAW2", "Stone in Focus"),
            (5, "Slayer", "Reign", "Raining Blood"),
            (6, "Mozart", "Requiem", "Lacrimosa"),
        ]));
        app.local_plays.record_play(1, 1_000);
        app.local_plays.record_play(1, 2_000);
        app.local_plays.record_play(2, 1_000);

        // Open with defaults (TopPlayed seed strategy → seeds = [1, 2]).
        app.open_generation_form();
        // Disable the on-device exclusion path (no device anyway, but
        // makes the assertion direct).
        if let Some(f) = &mut app.generation_form {
            f.exclude_on_device = false;
            f.target_length = 3;
        }
        app.commit_generation_form();

        assert_eq!(app.playlists.len(), 1);
        let p = &app.playlists.playlists()[0];
        assert!(p.is_generated());
        assert!(!p.track_ids.is_empty(), "recommender should produce tracks");
        // Seeds (1 and 2) must not appear in the generated set — the
        // recommender's `eligible` filter strips them.
        for tid in &p.track_ids {
            assert!(
                !matches!(*tid, 1 | 2),
                "seed track {tid} leaked into output"
            );
        }
    }

    #[test]
    fn commit_generation_form_regenerate_replaces_track_ids_in_place() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (1, "A", "X", "T1"),
            (2, "B", "X", "T2"),
            (3, "C", "Y", "T3"),
            (4, "D", "Z", "T4"),
        ]));
        app.local_plays.record_play(1, 1_000);

        // Seed an existing generated playlist.
        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 2;
        params.exclude_on_device = false;
        let pid = app
            .playlists
            .add(Playlist::new_generated("DW", params, vec![3, 4]));

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;

        app.open_generation_form_for_regenerate();
        assert!(app.generation_form.is_some());
        app.commit_generation_form();

        // Same playlist id, replaced track_ids, previously_recommended
        // now contains the prior selection so the next regen drifts.
        let p = app.playlists.get(pid).unwrap();
        assert!(p.previously_recommended.contains(&3));
        assert!(p.previously_recommended.contains(&4));
    }

    #[test]
    fn commit_generation_form_empty_name_rejects_with_toast() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(1, "A", "X", "T1")]));
        app.open_generation_form();
        if let Some(f) = &mut app.generation_form {
            f.name = "   ".into();
        }
        app.commit_generation_form();
        assert!(app.playlists.is_empty());
        assert!(app.toast_message.as_ref().unwrap().2);
    }

    #[test]
    fn commit_generation_form_no_library_rejects_with_toast() {
        let mut app = App::new();
        app.library = None;
        app.open_generation_form();
        app.commit_generation_form();
        assert!(app.playlists.is_empty());
        assert!(app.toast_message.as_ref().unwrap().2);
    }

    // ------------------------------------------------------------------
    // Phase 3: device-side playlist sync wiring.
    // ------------------------------------------------------------------

    #[test]
    fn enqueue_playlist_registers_pending_import() {
        // Enqueue always populates pending_playlist_imports — the
        // per-backend gate fires at drain time (handle_bg_event's
        // SyncComplete), not at enqueue time.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (10, "A", "Alb", "T1"),
            (20, "B", "Alb", "T2"),
        ]));
        let pid = app.playlists.add(Playlist::new_manual("Faves"));
        app.playlists.add_track(pid, 10);
        app.playlists.add_track(pid, 20);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.add_sidebar_item_to_queue();

        // Sync queue carries the file uploads as before.
        assert_eq!(app.sync.queue.len(), 1);
        // Pending import carries the (artist, album, title) tuples in the
        // exact order of the playlist.
        assert_eq!(app.pending_playlist_imports.len(), 1);
        let spec = &app.pending_playlist_imports[0];
        assert_eq!(spec.name, "Faves");
        assert_eq!(
            spec.track_keys,
            vec![
                ("A".into(), "Alb".into(), "T1".into()),
                ("B".into(), "Alb".into(), "T2".into()),
            ]
        );
    }

    #[test]
    fn enqueue_same_playlist_twice_replaces_pending_entry() {
        // The user might enqueue, change their mind and re-enqueue. The
        // second push should replace the first so the device only sees
        // one playlist creation per queue cycle.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(10, "A", "Alb", "T1")]));
        let pid = app.playlists.add(Playlist::new_manual("Faves"));
        app.playlists.add_track(pid, 10);

        app.browse_mode = BrowseMode::Playlists;
        app.refresh_sidebar();
        app.sidebar_selected = 0;
        app.add_sidebar_item_to_queue();
        app.add_sidebar_item_to_queue();

        assert_eq!(app.pending_playlist_imports.len(), 1);
    }

    #[test]
    fn sync_complete_drains_pending_imports_for_zune() {
        // Zune is always-on after the 2026-04-26 hardware validation.
        let mut app = App::new();
        app.device.family = Some(zytunes::device::DeviceFamily::Zune);
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Faves".into(),
            track_keys: vec![("A".into(), "X".into(), "T1".into())],
        });
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Workout".into(),
            track_keys: vec![("B".into(), "Y".into(), "T2".into())],
        });
        app.sync.status = SyncStatus::Running {
            current: 5,
            total: 5,
        };

        app.handle_bg_event(BgEvent::SyncComplete {
            success: 5,
            failed: 0,
            skipped: 0,
        });

        assert!(app.pending_playlist_imports.is_empty(), "drained");
        let import_count = app
            .pending_bg_commands
            .iter()
            .filter(|c| matches!(c, BgCommand::ImportPlaylist { .. }))
            .count();
        assert_eq!(import_count, 2);
    }

    #[test]
    fn sync_complete_skips_imports_for_ipod_when_gate_off() {
        // iPod stays gated until the 2026-04-26 iTunesDB-corruption
        // incident is root-caused. Pending import is dropped, file
        // uploads still happen, sync log explains why.
        let mut app = App::new();
        app.device.family = Some(zytunes::device::DeviceFamily::Ipod);
        assert!(!app.experimental_playlist_sync, "default off");
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Faves".into(),
            track_keys: vec![("A".into(), "X".into(), "T1".into())],
        });
        app.sync.status = SyncStatus::Running {
            current: 1,
            total: 1,
        };

        app.handle_bg_event(BgEvent::SyncComplete {
            success: 1,
            failed: 0,
            skipped: 0,
        });

        assert!(
            app.pending_playlist_imports.is_empty(),
            "drained either way"
        );
        let import_count = app
            .pending_bg_commands
            .iter()
            .filter(|c| matches!(c, BgCommand::ImportPlaylist { .. }))
            .count();
        assert_eq!(import_count, 0, "iPod gate must hold");
        assert!(
            app.sync
                .log
                .iter()
                .any(|l| l.contains("ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC")),
            "sync log should explain the gate; got {:?}",
            app.sync.log
        );
    }

    #[test]
    fn sync_complete_fires_imports_for_ipod_when_gate_on() {
        let mut app = App::new();
        app.device.family = Some(zytunes::device::DeviceFamily::Ipod);
        app.experimental_playlist_sync = true;
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Faves".into(),
            track_keys: vec![("A".into(), "X".into(), "T1".into())],
        });
        app.sync.status = SyncStatus::Running {
            current: 1,
            total: 1,
        };

        app.handle_bg_event(BgEvent::SyncComplete {
            success: 1,
            failed: 0,
            skipped: 0,
        });

        let import_count = app
            .pending_bg_commands
            .iter()
            .filter(|c| matches!(c, BgCommand::ImportPlaylist { .. }))
            .count();
        assert_eq!(import_count, 1);
    }

    #[test]
    fn sync_complete_skips_imports_when_no_device_family() {
        // Defensive: don't fire imports against an unknown backend.
        let mut app = App::new();
        app.device.family = None;
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Faves".into(),
            track_keys: vec![],
        });

        app.handle_bg_event(BgEvent::SyncComplete {
            success: 0,
            failed: 0,
            skipped: 0,
        });

        let import_count = app
            .pending_bg_commands
            .iter()
            .filter(|c| matches!(c, BgCommand::ImportPlaylist { .. }))
            .count();
        assert_eq!(import_count, 0);
    }

    #[test]
    fn clear_queue_drops_pending_imports() {
        let mut app = App::new();
        app.pending_playlist_imports.push(PendingPlaylistImport {
            name: "Faves".into(),
            track_keys: vec![],
        });
        app.sync.queue.push(QueuedItem {
            label: "X".into(),
            tracks: vec![],
        });
        app.clear_queue();
        assert!(app.sync.queue.is_empty());
        assert!(app.pending_playlist_imports.is_empty());
    }

    #[test]
    fn playlist_imported_event_routes_to_toast() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::PlaylistImported {
            name: "Faves".into(),
            summary: Ok(zytunes::mtp::PlaylistImportSummary {
                resolved: 12,
                skipped: 1,
                replaced: false,
            }),
        });
        let (msg, _, is_err) = app.toast_message.as_ref().unwrap();
        assert!(!is_err);
        assert!(msg.contains("Created"));
        assert!(msg.contains("12 tracks"));
        assert!(msg.contains("1 unresolved"));
    }

    #[test]
    fn playlist_imported_event_failure_routes_to_error_toast() {
        let mut app = App::new();
        app.handle_bg_event(BgEvent::PlaylistImported {
            name: "Faves".into(),
            summary: Err("device sync rejected".into()),
        });
        let (msg, _, is_err) = app.toast_message.as_ref().unwrap();
        assert!(is_err);
        assert!(msg.contains("Faves"));
        assert!(msg.contains("device sync rejected"));
    }

    // ------------------------------------------------------------------
    // Phase 4: listen log + sequence-aware recommender wiring.
    // ------------------------------------------------------------------

    /// Fixture: install a `NowPlaying` for the given library track ID so the
    /// `record_now_playing_*` paths have something to count.
    fn install_now_playing(app: &mut App, track_id: u64) {
        app.now_playing = Some(NowPlaying {
            track_name: format!("T{}", track_id),
            artist: "A".into(),
            album: "X".into(),
            duration_ms: 180_000,
            elapsed_ms: 0,
            state: PlaybackState::Playing,
            track_index: 0,
            playlist: Arc::from([] as [TrackInfo; 0]),
            paused_frame: None,
            track_id: Some(track_id),
            counted: false,
            year: None,
            metadata_marquee: String::new(),
        });
    }

    #[test]
    fn record_play_appends_completed_event_to_listen_log() {
        let mut app = App::new();
        install_now_playing(&mut app, 42);
        app.record_now_playing_play();
        assert_eq!(app.listen_log.len(), 1);
        assert_eq!(app.listen_log.events()[0].id, 42);
        assert!(app.listen_log.events()[0].completed);
    }

    #[test]
    fn record_skip_appends_uncompleted_event_to_listen_log() {
        let mut app = App::new();
        install_now_playing(&mut app, 7);
        app.record_now_playing_skip();
        assert_eq!(app.listen_log.len(), 1);
        assert_eq!(app.listen_log.events()[0].id, 7);
        assert!(!app.listen_log.events()[0].completed);
    }

    #[test]
    fn record_play_idempotent_within_session_only_logs_once() {
        let mut app = App::new();
        install_now_playing(&mut app, 5);
        app.record_now_playing_play();
        app.record_now_playing_play(); // second call is no-op (counted=true)
        assert_eq!(app.listen_log.len(), 1, "guard prevents duplicate logging");
    }

    #[test]
    fn commit_generation_form_passes_bigrams_when_useful() {
        // Build a library with a clear sequence-driven preference:
        // seed track id=1, two equally-content-similar candidates 2 & 3.
        // The listen log strongly biases 1→3.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[
            (1, "Same", "X", "T1"),
            (2, "Diff", "Y", "Cand A"),
            (3, "Other", "Z", "Cand B"),
        ]));
        // Heavy plays on track 1 so it dominates TopPlayed seed selection.
        for i in 0..5 {
            app.local_plays.record_play(1, 1_000 + i);
        }

        // Seed the listen log with 32 events / 2 sessions of 1→3 alternation
        // (clears the BigramTable::is_useful threshold).
        for i in 0..15 {
            app.listen_log.append(ListenEvent {
                ts: i * 1000,
                id: 1,
                completed: true,
            });
            app.listen_log.append(ListenEvent {
                ts: i * 1000 + 100,
                id: 3,
                completed: true,
            });
        }
        app.listen_log.append(ListenEvent {
            ts: 5_000_000,
            id: 1,
            completed: true,
        });
        app.listen_log.append(ListenEvent {
            ts: 5_001_000,
            id: 3,
            completed: true,
        });

        app.open_generation_form();
        if let Some(f) = &mut app.generation_form {
            f.exclude_on_device = false;
            f.target_length = 1;
        }
        app.commit_generation_form();

        let p = &app.playlists.playlists()[0];
        assert_eq!(
            p.track_ids,
            vec![3],
            "sequence-aware scorer should pick the bigram-followed track"
        );
    }

    #[test]
    fn commit_generation_form_works_when_listen_log_is_empty() {
        // No listen-log events → BigramTable not useful → recommender
        // skips the sequence term. Still must produce a result.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(1, "A", "X", "T1"), (2, "B", "Y", "T2")]));
        app.local_plays.record_play(1, 1_000);

        app.open_generation_form();
        if let Some(f) = &mut app.generation_form {
            f.exclude_on_device = false;
            f.target_length = 1;
        }
        app.commit_generation_form();

        assert_eq!(app.playlists.len(), 1);
        assert!(!app.playlists.playlists()[0].track_ids.is_empty());
    }

    // ------------------------------------------------------------------
    // Phase 2 follow-up: Artist/Genre seed strategy context discovery.
    // ------------------------------------------------------------------

    #[test]
    fn validate_strategy_context_passes_for_behavioural_strategies() {
        let mut f = GenerationFormState::discover_weekly_default(0);
        f.seed_strategy_idx = 0; // TopPlayed
        assert!(f.validate_strategy_context().is_ok());
        f.seed_strategy_idx = 1; // RecentlyPlayed
        assert!(f.validate_strategy_context().is_ok());
    }

    #[test]
    fn validate_strategy_context_rejects_track_without_id() {
        let mut f = GenerationFormState::discover_weekly_default(0);
        f.seed_strategy_idx = 2;
        assert!(f.validate_strategy_context().is_err());
        f.seed_context.track_id = Some(42);
        assert!(f.validate_strategy_context().is_ok());
    }

    #[test]
    fn validate_strategy_context_rejects_artist_without_name() {
        let mut f = GenerationFormState::discover_weekly_default(0);
        f.seed_strategy_idx = 3;
        assert!(f.validate_strategy_context().is_err());
        f.seed_context.artist = Some("   ".into());
        assert!(
            f.validate_strategy_context().is_err(),
            "whitespace-only must be rejected"
        );
        f.seed_context.artist = Some("Boards of Canada".into());
        assert!(f.validate_strategy_context().is_ok());
    }

    #[test]
    fn validate_strategy_context_rejects_genre_without_name() {
        let mut f = GenerationFormState::discover_weekly_default(0);
        f.seed_strategy_idx = 4;
        assert!(f.validate_strategy_context().is_err());
        f.seed_context.genre = Some("Electronic".into());
        assert!(f.validate_strategy_context().is_ok());
    }

    #[test]
    fn for_library_context_carries_all_three_fields() {
        let f = GenerationFormState::for_library_context(
            10_000,
            Some(42),
            Some("BoC".into()),
            Some("Electronic".into()),
            2,
            "Roygbiv",
        );
        assert_eq!(f.seed_context.track_id, Some(42));
        assert_eq!(f.seed_context.artist.as_deref(), Some("BoC"));
        assert_eq!(f.seed_context.genre.as_deref(), Some("Electronic"));
        // Switching strategies in the form picks up the corresponding ctx.
        let mut f = f;
        f.seed_strategy_idx = 3;
        assert_eq!(f.current_context_label().as_deref(), Some("BoC"));
        f.seed_strategy_idx = 4;
        assert_eq!(f.current_context_label().as_deref(), Some("Electronic"));
    }

    fn lib_with_genre_year_tracks(
        rows: &[(u64, &str, &str, &str, &str)],
    ) -> Box<dyn MusicLibrary + Send> {
        let tracks: Vec<Track> = rows
            .iter()
            .map(|(id, artist, album, name, genre)| Track {
                id: *id,
                name: (*name).to_string(),
                artist: (*artist).to_string(),
                album: (*album).to_string(),
                genre: Some((*genre).to_string()),
                location: Some(format!("/tmp/{}.mp3", id)),
                ..Default::default()
            })
            .collect();
        Box::new(VecLibrary { tracks })
    }

    #[test]
    fn open_generation_form_from_track_row_captures_artist_and_genre() {
        let mut app = App::new();
        app.library = Some(lib_with_genre_year_tracks(&[(
            10,
            "BoC",
            "Geogaddi",
            "Music Is Math",
            "Electronic",
        )]));
        app.browse_mode = BrowseMode::Library;
        app.active_panel = Panel::TrackList;
        // Build the track_list manually with the relevant fields set.
        app.track_list.push(TrackInfo {
            name: "Music Is Math".into(),
            artist: "BoC".into(),
            album: "Geogaddi".into(),
            duration_ms: None,
            kind: None,
            location: None,
            track_number: None,
            disc_number: None,
            genre: Some("Electronic".into()),
            on_device: false,
            play_count: None,
            skip_count: None,
            rating: None,
            last_played_at_ms: None,
            last_synced_from_device_at_ms: None,
            library_id: Some(10),
            artist_key: "boc".into(),
            name_key: "music is math".into(),
        });
        app.track_selected = 0;

        app.open_generation_form();

        let f = app.generation_form.as_ref().expect("form opens");
        assert_eq!(f.seed_strategy_idx, 2, "defaults to Track");
        assert_eq!(f.seed_context.track_id, Some(10));
        assert_eq!(f.seed_context.artist.as_deref(), Some("BoC"));
        assert_eq!(f.seed_context.genre.as_deref(), Some("Electronic"));
    }

    #[test]
    fn open_generation_form_from_artist_sidebar_captures_artist() {
        // VecLibrary's MusicLibrary impl returns empty for `artists()`, so
        // refresh_sidebar can't populate the sidebar from the library. Push
        // the entry directly — open_generation_form reads from
        // `sidebar_items` and doesn't care how it got there.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(1, "Beatles", "Album", "T1")]));
        app.browse_mode = BrowseMode::Library;
        app.active_panel = Panel::Library;
        app.sidebar_items = vec![SidebarEntry::Artist("Beatles".into())];
        app.sidebar_selected = 0;

        app.open_generation_form();

        let f = app.generation_form.as_ref().expect("form opens");
        assert_eq!(f.seed_strategy_idx, 3, "defaults to Artist");
        assert_eq!(f.seed_context.artist.as_deref(), Some("Beatles"));
    }

    #[test]
    fn open_generation_form_from_album_sidebar_captures_artist() {
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(1, "Beatles", "Revolver", "T1")]));
        app.browse_mode = BrowseMode::Library;
        app.active_panel = Panel::Library;
        app.sidebar_mode = SidebarMode::Albums;
        app.sidebar_items = vec![SidebarEntry::Album {
            artist: "Beatles".into(),
            album: "Revolver".into(),
        }];
        app.sidebar_selected = 0;

        app.open_generation_form();

        let f = app.generation_form.as_ref().expect("form opens");
        assert_eq!(f.seed_strategy_idx, 3, "defaults to Artist");
        assert_eq!(f.seed_context.artist.as_deref(), Some("Beatles"));
    }

    #[test]
    fn commit_form_with_artist_strategy_actually_generates() {
        // End-to-end: form populated with Artist context, recommender
        // resolves the strategy and produces a non-empty playlist.
        let mut app = App::new();
        app.library = Some(lib_with_genre_year_tracks(&[
            (1, "BoC", "Music Has The Right", "Roygbiv", "Electronic"),
            (2, "BoC", "Geogaddi", "Music Is Math", "Electronic"),
            (3, "Aphex Twin", "SAW2", "Stone in Focus", "Electronic"),
            (4, "Slayer", "Reign", "Raining Blood", "Metal"),
        ]));
        app.browse_mode = BrowseMode::Library;
        app.active_panel = Panel::Library;
        app.sidebar_items = vec![SidebarEntry::Artist("BoC".into())];
        app.sidebar_selected = 0;

        app.open_generation_form();
        if let Some(f) = &mut app.generation_form {
            f.exclude_on_device = false;
            f.target_length = 2;
        }
        app.commit_generation_form();

        assert_eq!(app.playlists.len(), 1);
        let p = &app.playlists.playlists()[0];
        assert!(!p.track_ids.is_empty(), "Artist strategy produces output");
    }

    #[test]
    fn commit_form_with_genre_strategy_uses_genre_context() {
        // Open from a track row → all three contexts captured. User then
        // switches to Genre in the form and submits. With several
        // electronic candidates available, the genre-similarity term
        // should rank them above the unrelated metal track.
        let mut app = App::new();
        app.library = Some(lib_with_genre_year_tracks(&[
            (1, "Aphex Twin", "SAW2", "Stone in Focus", "Electronic"),
            (2, "BoC", "Geogaddi", "Music Is Math", "Electronic"),
            (
                3,
                "Squarepusher",
                "Hard Normal Daddy",
                "Beep Street",
                "Electronic",
            ),
            (4, "Autechre", "Tri Repetae", "Clipper", "Electronic"),
            (5, "Slayer", "Reign", "Raining Blood", "Metal"),
        ]));
        app.browse_mode = BrowseMode::Library;
        app.active_panel = Panel::TrackList;
        app.track_list.push(TrackInfo {
            name: "Stone in Focus".into(),
            artist: "Aphex Twin".into(),
            album: "SAW2".into(),
            duration_ms: None,
            kind: None,
            location: None,
            track_number: None,
            disc_number: None,
            genre: Some("Electronic".into()),
            on_device: false,
            play_count: None,
            skip_count: None,
            rating: None,
            last_played_at_ms: None,
            last_synced_from_device_at_ms: None,
            library_id: Some(1),
            artist_key: "aphex twin".into(),
            name_key: "stone in focus".into(),
        });
        app.track_selected = 0;

        app.open_generation_form();
        // User switches the radio to Genre (idx 4).
        if let Some(f) = &mut app.generation_form {
            f.seed_strategy_idx = 4;
            f.exclude_on_device = false;
            // Genre seeding takes up to 8 matching tracks as seeds; with
            // four electronic tracks all becoming seeds, only one
            // (non-electronic) candidate remains in the pool. Asking for
            // 1 result asserts the wiring without picking a fight with
            // the seed-vs-candidate ratio.
            f.target_length = 1;
        }
        app.commit_generation_form();

        assert_eq!(app.playlists.len(), 1);
        let p = &app.playlists.playlists()[0];
        assert!(!p.track_ids.is_empty(), "Genre strategy produced output");
        // The form's seed strategy must have round-tripped to Genre.
        let stored = match &p.kind {
            PlaylistKind::Generated { params, .. } => &params.seed_strategy,
            _ => panic!("expected generated playlist"),
        };
        assert!(
            matches!(stored, SeedStrategy::Genre(g) if g == "Electronic"),
            "stored strategy must be Genre(Electronic), got {stored:?}"
        );
    }

    #[test]
    fn commit_form_rejects_artist_strategy_without_context() {
        // Open with no library context → user cycles to Artist strategy →
        // submit must reject and re-stash the form.
        let mut app = App::new();
        app.library = Some(lib_with_tracks(&[(1, "A", "X", "T1")]));
        app.browse_mode = BrowseMode::Playlists; // no library context source
        app.refresh_sidebar();
        app.open_generation_form();
        if let Some(f) = &mut app.generation_form {
            f.seed_strategy_idx = 3; // Artist — but seed_context.artist is None
        }
        app.commit_generation_form();

        assert!(app.playlists.is_empty(), "submit was rejected");
        assert!(
            app.generation_form.is_some(),
            "form re-stashed so user can fix it"
        );
        assert!(app.toast_message.as_ref().unwrap().2);
    }
}
