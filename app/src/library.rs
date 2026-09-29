//! Music library trait and shared `Track` type.

use std::collections::HashMap;

/// A track in the music library.
///
/// All `Option` fields use `#[serde(default, skip_serializing_if =
/// "Option::is_none")]` so the JSON cache stays compact (no `"genre":null`
/// noise) and forward-compatible — old caches missing newly-added fields
/// still load.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub artist: String,
    pub album: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Chromaprint fingerprint, URL-safe base64 of the compressed form — same
    /// wire format as MusicBrainz Picard's `ACOUSTID_FINGERPRINT` tag, so
    /// computed and embedded fingerprints compare byte-for-byte. `None` until
    /// the background scan produces one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acoustic_id: Option<String>,

    // ---- Extended tag metadata (lofty `ItemKey`-derived) ----
    //
    // Every field below carries `#[serde(default, skip_serializing_if =
    // "Option::is_none")]` for the same reason: keeps the cache compact and
    // forward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyricist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barcode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_settings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    /// POPM/rating byte (0–255) from the file tag. Distinct from device-side
    /// rating which lives on `TrackInfo` in the TUI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,

    // ---- MusicBrainz identifiers ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_track_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_recording_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_release_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_release_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_release_artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mb_work_id: Option<String>,

    // ---- ReplayGain ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_track_gain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_track_peak: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_album_gain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_album_peak: Option<String>,

    // ---- Audio properties (from `tagged.properties()`) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
}

impl Track {
    /// Artist key used for sidebar grouping: prefers a non-empty `album_artist`
    /// (so "*NSYNC feat. Lisa Lopes" tracks sit under "*NSYNC" alongside the
    /// rest of the album), falling back to the credited `artist` when no
    /// album_artist tag is set.
    pub fn grouping_artist(&self) -> &str {
        self.album_artist
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.artist)
    }
}

/// Collapse strings that differ only by ASCII case, keeping the most
/// common original spelling (ties: lexicographic). Sorted case-insensitively.
///
/// Lookups (`artist_tracks`, …) already use `eq_ignore_ascii_case`, so a
/// case-sensitive unique list produces duplicate sidebar rows that show
/// the same tracks — `Alice in Chains` vs `Alice In Chains`.
pub fn collapse_ascii_case<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut groups: HashMap<String, HashMap<&'a str, usize>> = HashMap::new();
    for name in names {
        *groups
            .entry(name.to_ascii_lowercase())
            .or_default()
            .entry(name)
            .or_insert(0) += 1;
    }
    let mut out: Vec<&str> = groups.into_values().map(preferred_ascii_spelling).collect();
    out.sort_by(|a, b| {
        a.to_ascii_lowercase()
            .cmp(&b.to_ascii_lowercase())
            .then_with(|| (*a).cmp(*b))
    });
    out
}

fn preferred_ascii_spelling(spellings: HashMap<&str, usize>) -> &str {
    spellings
        .into_iter()
        .max_by(|(a, na), (b, nb)| na.cmp(nb).then_with(|| (*a).cmp(*b)))
        .map(|(s, _)| s)
        .expect("non-empty spelling group")
}

/// Trait abstracting a music library backend.
///
/// The track-returning methods yield `Box<dyn Iterator>` so streaming callers
/// (sidebar population, stats passes) don't force an intermediate `Vec` —
/// callers that want materialized state `.collect()` themselves.
pub trait MusicLibrary {
    /// Unique artist names, sorted. ASCII-case variants collapse to the
    /// most common original spelling.
    fn artists(&self) -> Vec<&str>;
    /// Unique `(artist, album)` pairs, sorted. ASCII-case variants of either
    /// half collapse the same way as [`Self::artists`].
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
    /// Look up a track by its stable path-hash id. Default walks `all_tracks`;
    /// backends keyed by id should override for O(1).
    fn track_by_id(&self, id: u64) -> Option<&Track> {
        self.all_tracks().find(|t| t.id == id)
    }

    /// Fill in Chromaprint ids computed after the library was already shown.
    ///
    /// Existing ids are left alone. The default is a no-op so backends that
    /// do not participate in background fingerprinting stay unchanged.
    fn apply_acoustic_ids(&mut self, _updates: &[(u64, String)]) {}
}

#[cfg(test)]
mod tests {
    use super::collapse_ascii_case;

    #[test]
    fn collapse_ascii_case_keeps_most_common_spelling() {
        let names = [
            "Alice In Chains",
            "Alice in Chains",
            "Alice in Chains",
            "Alice in Chains",
        ];
        assert_eq!(collapse_ascii_case(names), vec!["Alice in Chains"]);
    }

    #[test]
    fn collapse_ascii_case_keeps_distinct_artists() {
        let names = ["Radiohead", "Radiohead", "radiohead", "Alice in Chains"];
        assert_eq!(
            collapse_ascii_case(names),
            vec!["Alice in Chains", "Radiohead"]
        );
    }
}
