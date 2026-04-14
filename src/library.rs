//! Music library trait and shared `Track` type.

/// A track in the music library.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub artist: String,
    pub album: String,
    #[allow(dead_code)] // parsed for future sync
    pub album_artist: Option<String>,
    #[allow(dead_code)] // parsed for future sync
    pub genre: Option<String>,
    #[allow(dead_code)] // parsed for future sync
    pub year: Option<u32>,
    #[allow(dead_code)] // parsed for future sync
    pub track_number: Option<u32>,
    #[allow(dead_code)] // parsed for future sync
    pub disc_number: Option<u32>,
    #[allow(dead_code)] // parsed for future sync
    pub total_time_ms: Option<u64>,
    pub location: Option<String>,
    pub kind: Option<String>,
}

/// Trait abstracting a music library backend.
pub trait MusicLibrary {
    /// All unique artist names, sorted.
    fn artists(&self) -> Vec<&str>;
    /// All unique (artist, album) pairs, sorted.
    fn albums(&self) -> Vec<(&str, &str)>;
    /// All tracks by a given artist (case-insensitive).
    fn artist_tracks(&self, artist: &str) -> Vec<&Track>;
    /// All tracks matching an album name (case-insensitive).
    fn album_tracks(&self, album: &str) -> Vec<&Track>;
    /// All tracks matching artist AND album (case-insensitive).
    fn album_tracks_by_artist(&self, artist: &str, album: &str) -> Vec<&Track>;
    /// All tracks matching a track name (case-insensitive).
    fn tracks_by_name(&self, name: &str) -> Vec<&Track>;
    /// Total number of tracks.
    fn track_count(&self) -> usize;
    /// All tracks in the library.
    fn all_tracks(&self) -> Vec<&Track>;
    /// The music folder path, if known.
    fn music_folder(&self) -> Option<&str>;
}
