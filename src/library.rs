//! Music library trait and shared `Track` type.

/// A track in the music library.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub total_time_ms: Option<u64>,
    pub location: Option<String>,
    pub kind: Option<String>,
    /// Chromaprint fingerprint, URL-safe base64 of the compressed form — same
    /// wire format as MusicBrainz Picard's `ACOUSTID_FINGERPRINT` tag, so
    /// computed and embedded fingerprints compare byte-for-byte. `None` until
    /// the background scan produces one.
    #[serde(default)]
    pub acoustic_id: Option<String>,
}

/// Trait abstracting a music library backend.
///
/// The track-returning methods yield `Box<dyn Iterator>` so streaming callers
/// (sidebar population, stats passes) don't force an intermediate `Vec` —
/// callers that want materialized state `.collect()` themselves.
pub trait MusicLibrary {
    /// All unique artist names, sorted.
    fn artists(&self) -> Vec<&str>;
    /// All unique (artist, album) pairs, sorted.
    fn albums(&self) -> Vec<(&str, &str)>;
    /// All tracks by a given artist (case-insensitive).
    fn artist_tracks<'a>(&'a self, artist: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a>;
    /// All tracks matching an album name (case-insensitive).
    fn album_tracks<'a>(&'a self, album: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a>;
    /// All tracks matching artist AND album (case-insensitive).
    fn album_tracks_by_artist<'a>(
        &'a self,
        artist: &str,
        album: &str,
    ) -> Box<dyn Iterator<Item = &'a Track> + 'a>;
    /// All tracks matching a track name (case-insensitive).
    fn tracks_by_name<'a>(&'a self, name: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a>;
    /// Total number of tracks.
    fn track_count(&self) -> usize;
    /// All tracks in the library.
    fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_>;
    /// The music folder path, if known.
    fn music_folder(&self) -> Option<&str>;
}
