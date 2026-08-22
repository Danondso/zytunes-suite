//! JSON DTOs for the streaming API.

use std::collections::BTreeMap;

use serde::Serialize;
use zytunes::library::{MusicLibrary, Track};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AlbumPair {
    pub artist: String,
    pub album: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    pub track_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub art_url: Option<String>,
}

fn sort_key(t: &Track) -> (u32, u32, String) {
    (
        t.disc_number.unwrap_or(u32::MAX),
        t.track_number.unwrap_or(u32::MAX),
        t.name.to_lowercase(),
    )
}

impl AlbumPair {
    pub fn from_tracks(artist: String, album: String, tracks: &mut [&Track]) -> Self {
        tracks.sort_by_key(|t| sort_key(t));
        let art_url = tracks.first().map(|t| format!("/tracks/{}/art", t.id));
        let year = tracks.iter().find_map(|t| t.year);
        Self {
            artist,
            album,
            year,
            track_count: tracks.len() as u32,
            art_url,
        }
    }

    /// One in-memory pass over the library so the artist page does not N+1
    /// `/tracks` for covers.
    pub fn collect(library: &dyn MusicLibrary, artist_filter: Option<&str>) -> Vec<Self> {
        let mut groups: BTreeMap<(String, String), Vec<&Track>> = BTreeMap::new();
        let iter: Box<dyn Iterator<Item = &Track> + '_> = match artist_filter {
            Some(artist) => library.artist_tracks(artist),
            None => library.all_tracks(),
        };
        for track in iter {
            groups
                .entry((track.grouping_artist().to_string(), track.album.clone()))
                .or_default()
                .push(track);
        }
        groups
            .into_iter()
            .map(|((artist, album), mut tracks)| {
                Self::from_tracks(artist, album, tracks.as_mut_slice())
            })
            .collect()
    }
}

/// Ranked `GET /search` payload: matching artists and albums first, then tracks.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SearchResults {
    pub artists: Vec<String>,
    pub albums: Vec<AlbumPair>,
    pub tracks: Vec<TrackSummary>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TrackSummary {
    /// Path-hash track id as a decimal string so JSON clients (Dart/JS)
    /// do not lose bits above 2^53 / 2^63.
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl From<&Track> for TrackSummary {
    fn from(t: &Track) -> Self {
        Self {
            id: t.id.to_string(),
            name: t.name.clone(),
            artist: t.grouping_artist().to_string(),
            album: t.album.clone(),
            track_number: t.track_number,
            disc_number: t.disc_number,
            duration_ms: t.total_time_ms,
            kind: t.kind.clone(),
        }
    }
}

/// Full track payload for `GET /tracks/{id}`.
#[derive(Debug, Clone, Serialize)]
pub struct TrackDetail {
    #[serde(flatten)]
    pub summary: TrackSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_recording_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_release_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_track_gain: Option<String>,
    pub stream_url: String,
    pub file_url: String,
    pub art_url: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub play_count: u32,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub last_played_at_ms: u64,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// Response for `POST /tracks/{id}/play`.
#[derive(Debug, Clone, Serialize)]
pub struct PlayRecord {
    pub play_count: u32,
    pub last_played_at_ms: u64,
}

impl TrackDetail {
    pub fn from_track(t: &Track) -> Self {
        let id = t.id;
        Self {
            summary: TrackSummary::from(t),
            genre: t.genre.clone(),
            year: t.year,
            album_artist: t.album_artist.clone(),
            composer: t.composer.clone(),
            sample_rate: t.sample_rate,
            channels: t.channels,
            bit_depth: t.bit_depth,
            audio_bitrate_kbps: t.audio_bitrate_kbps,
            file_size_bytes: t.file_size_bytes,
            mb_recording_id: t.mb_recording_id.clone(),
            mb_release_id: t.mb_release_id.clone(),
            replaygain_track_gain: t.replaygain_track_gain.clone(),
            stream_url: format!("/tracks/{id}/stream"),
            file_url: format!("/tracks/{id}/file"),
            art_url: format!("/tracks/{id}/art"),
            play_count: 0,
            last_played_at_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_id_serializes_as_json_string() {
        let summary = TrackSummary {
            id: "18446744073709551615".into(),
            name: "Huge".into(),
            artist: "A".into(),
            album: "B".into(),
            track_number: None,
            disc_number: None,
            duration_ms: None,
            kind: None,
        };
        let value = serde_json::to_value(&summary).unwrap();
        assert!(
            value["id"].is_string(),
            "id must be a JSON string, got {}",
            value["id"]
        );
        assert_eq!(value["id"], "18446744073709551615");
        let raw = serde_json::to_string(&summary).unwrap();
        assert!(
            raw.contains("\"id\":\"18446744073709551615\""),
            "raw JSON must quote the id: {raw}"
        );
    }

    #[test]
    fn album_pair_uses_first_sorted_track_for_art_and_year() {
        let later = Track {
            id: 2,
            name: "B".into(),
            artist: "A".into(),
            album: "X".into(),
            track_number: Some(2),
            year: Some(1998),
            ..Default::default()
        };
        let earlier = Track {
            id: 1,
            name: "A".into(),
            artist: "A".into(),
            album: "X".into(),
            track_number: Some(1),
            year: Some(1997),
            ..Default::default()
        };
        let pair = AlbumPair::from_tracks("A".into(), "X".into(), &mut [&later, &earlier]);
        assert_eq!(pair.art_url.as_deref(), Some("/tracks/1/art"));
        assert_eq!(pair.year, Some(1997));
        assert_eq!(pair.track_count, 2);
    }
}
