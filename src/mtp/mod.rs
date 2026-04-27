pub mod ipod_session;
pub mod native;
pub mod parse;
pub mod zmdb;

pub use ipod_session::IpodSession;
pub use native::NativeSession;
use parse::DeviceEntry;

/// Pre-parsed metadata for a track being imported, lifted from the library so
/// the device session doesn't have to re-read the audio file with lofty.
/// CLI paths without a library backing (e.g. `zytunes push <file>`) pass
/// `None` and let the session fall back to its own tag read.
#[derive(Debug, Clone)]
pub struct TrackMeta {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
}

impl TrackMeta {
    pub fn from_track(t: &crate::library::Track) -> Self {
        Self {
            artist: t.artist.clone(),
            album: t.album.clone(),
            title: t.name.clone(),
            track_number: t.track_number,
            genre: t.genre.clone(),
        }
    }
}

/// Trait abstracting device session operations for testability.
///
/// Implemented by [`NativeSession`] for real hardware and by `MockSession` in tests.
/// All methods return `Result<T, String>` — errors are human-readable messages.
pub trait DeviceSession {
    /// List files and directories at `path` on the device.
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    /// Import a local audio file to the device. Returns the new MTP object ID.
    /// `meta`, when supplied, short-circuits the session's own tag read.
    fn import_track(&mut self, local_path: &str, meta: Option<&TrackMeta>) -> Result<u64, String>;
    /// Remove a file or folder by device path (e.g., `/Music/Artist/Album/track.mp3`).
    fn rm(&mut self, device_path: &str) -> Result<(), String>;
    /// Remove an object by its MTP object ID.
    fn rm_by_id(&mut self, object_id: u32) -> Result<(), String>;
    /// Delete empty artist/album folders under `/Music`. Returns count of folders removed.
    fn cleanup_empty_folders(&mut self) -> Result<usize, String>;
    /// Query device storage. Returns `(total_bytes, free_bytes)`.
    fn get_storage_info(&mut self) -> Result<(u64, u64), String>;
    /// Recursively collect all track entries under `path` (e.g., `/Music`).
    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String>;
    /// Save the device's sync progress to a local cache file.
    /// Called after a successful sync session. Default is a no-op.
    fn save_sync_progress(&mut self) {}
    /// Pre-warm any internal write-side state (e.g. mapping artist/album folder
    /// handles) so the first `import_track` call doesn't pay a one-time setup
    /// cost. Called once during connect. Default is a no-op for backends that
    /// don't need a separate library scan.
    fn prewarm_library(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Refresh any locally-cached free-space metadata after operations that change
    /// device storage (sync, delete). Without this, the next reconnect sees a
    /// large diff between cached and current free bytes and invalidates the
    /// track/library caches, forcing a slow re-enumeration. Default is a no-op.
    fn refresh_storage_cache(&mut self, _free_bytes: u64) {}
    /// Import a photo to the device. Takes filename and pre-resized JPEG bytes.
    /// Returns the new MTP object ID.
    fn import_photo(&mut self, _filename: &str, _jpeg_data: &[u8]) -> Result<u64, String> {
        Err("Photo import not supported".into())
    }
    /// Import a video file to the device. Takes filename and raw file bytes.
    /// Returns the new MTP object ID.
    fn import_video(&mut self, _filename: &str, _data: &[u8]) -> Result<u64, String> {
        Err("Video import not supported".into())
    }
    /// Collect all video entries from the device. Uses ZMDB if available,
    /// otherwise falls back to scanning `/Videos` via MTP handle walk.
    fn collect_all_videos(&mut self) -> Result<Vec<DeviceEntry>, String> {
        Ok(Vec::new())
    }
    /// Create or replace a playlist on the device.
    ///
    /// `track_keys` is an ordered list of `(artist, album, title)` tuples
    /// taken from the library; each backend resolves them to its own
    /// device-side ID scheme (iPod dbid, Zune object handle). Tuples that
    /// don't resolve are skipped; the function only fails if the playlist
    /// itself cannot be written. A playlist with the same `name` is
    /// replaced atomically — the iPod's `master playlist` invariant is
    /// preserved by the backend.
    ///
    /// Default is `Err("playlist sync not supported on this device")` so
    /// backends opt in.
    fn import_playlist(
        &mut self,
        _name: &str,
        _track_keys: &[(String, String, String)],
    ) -> Result<PlaylistImportSummary, String> {
        Err("Playlist sync not supported on this device".into())
    }
}

/// Result of a successful `import_playlist` call. The TUI surfaces the
/// counts in the sync log so the user knows how many tracks made it onto
/// the device-side playlist vs. how many couldn't be matched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaylistImportSummary {
    /// Tuples that resolved to a device-side track and landed in the playlist.
    pub resolved: usize,
    /// Tuples skipped because the device doesn't have a matching track.
    pub skipped: usize,
    /// `true` if a playlist with the same name was overwritten,
    /// `false` if this was a fresh insert.
    pub replaced: bool,
}
