//! Ranked library search across artists, albums, and tracks.

use std::collections::{BTreeMap, BTreeSet};

use zytunes::library::{MusicLibrary, Track};

use crate::dto::{AlbumPair, SearchResults, TrackSummary};

/// Case-insensitive substring search. Empty query returns no hits.
///
/// Artists whose grouping name matches, then albums whose title or artist
/// matches (with cover/`track_count` from the grouped tracks), then tracks
/// ranked title → artist → album. Within a rank, results keep library
/// iteration order.
pub fn search_library(lib: &dyn MusicLibrary, query: &str) -> SearchResults {
    let q = query.trim();
    if q.is_empty() {
        return SearchResults::default();
    }
    let q = q.to_lowercase();

    let mut artist_names = BTreeSet::new();
    let mut album_groups: BTreeMap<(String, String), Vec<&Track>> = BTreeMap::new();
    let mut titled = Vec::new();
    let mut artist_tracks = Vec::new();
    let mut album_title_tracks = Vec::new();

    for t in lib.all_tracks() {
        let artist = t.grouping_artist();
        let artist_hit = artist.to_lowercase().contains(&q);
        let album_hit = t.album.to_lowercase().contains(&q);

        if artist_hit {
            artist_names.insert(artist.to_string());
        }
        if album_hit {
            artist_names.insert(artist.to_string());
        }
        if artist_hit || album_hit {
            album_groups
                .entry((artist.to_string(), t.album.clone()))
                .or_default()
                .push(t);
        }

        let name = t.name.to_lowercase();
        if name.contains(&q) {
            titled.push(t);
        } else if artist_hit {
            artist_tracks.push(t);
        } else if album_hit {
            album_title_tracks.push(t);
        }
    }

    titled.extend(artist_tracks);
    titled.extend(album_title_tracks);

    SearchResults {
        artists: artist_names.into_iter().collect(),
        albums: album_groups
            .into_iter()
            .map(|((artist, album), mut tracks)| {
                AlbumPair::from_tracks(artist, album, tracks.as_mut_slice())
            })
            .collect(),
        tracks: titled.into_iter().map(TrackSummary::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zytunes::library::Track;

    struct VecLib(Vec<Track>);

    impl MusicLibrary for VecLib {
        fn artists(&self) -> Vec<&str> {
            Vec::new()
        }
        fn albums(&self) -> Vec<(&str, &str)> {
            Vec::new()
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
            self.0.len()
        }
        fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
            Box::new(self.0.iter())
        }
        fn music_folder(&self) -> Option<&str> {
            None
        }
    }

    fn track(id: u64, name: &str, artist: &str, album: &str) -> Track {
        Track {
            id,
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            ..Default::default()
        }
    }

    fn fixture() -> VecLib {
        VecLib(vec![
            track(1, "Karma Police", "Radiohead", "OK Computer"),
            track(2, "Creep", "Radiohead", "Pablo Honey"),
            track(3, "Paranoid Android", "Radiohead", "OK Computer"),
            track(4, "Karma", "Some Artist", "Other Album"),
            track(5, "Song", "Karma Collective", "Debut"),
            track(6, "Tune", "Band", "Karma Sessions"),
        ])
    }

    fn track_ids(hits: &SearchResults) -> Vec<u64> {
        hits.tracks
            .iter()
            .map(|t| t.id.parse::<u64>().unwrap())
            .collect()
    }

    #[test]
    fn empty_query_returns_no_hits() {
        let lib = fixture();
        assert_eq!(search_library(&lib, ""), SearchResults::default());
        assert_eq!(search_library(&lib, "   "), SearchResults::default());
    }

    #[test]
    fn title_matches_rank_before_artist_and_album() {
        let lib = fixture();
        let hits = search_library(&lib, "karma");
        // Title: 1 (Karma Police), 4 (Karma)
        // Artist: 5 (Karma Collective)
        // Album: 6 (Karma Sessions)
        assert_eq!(track_ids(&hits), vec![1, 4, 5, 6]);
        assert_eq!(hits.artists, vec!["Band", "Karma Collective"]);
        let albums: Vec<&str> = hits.albums.iter().map(|a| a.album.as_str()).collect();
        assert_eq!(albums, vec!["Karma Sessions", "Debut"]);
    }

    #[test]
    fn artist_query_lists_the_artist_and_their_albums() {
        let lib = fixture();
        let hits = search_library(&lib, "radiohead");
        assert_eq!(hits.artists, vec!["Radiohead"]);
        let albums: Vec<&str> = hits.albums.iter().map(|a| a.album.as_str()).collect();
        assert_eq!(albums, vec!["OK Computer", "Pablo Honey"]);
        assert_eq!(track_ids(&hits), vec![1, 2, 3]);
    }

    #[test]
    fn album_query_includes_the_album_and_its_artist() {
        let lib = fixture();
        let hits = search_library(&lib, "ok computer");
        assert_eq!(hits.artists, vec!["Radiohead"]);
        assert_eq!(hits.albums.len(), 1);
        assert_eq!(hits.albums[0].album, "OK Computer");
        assert_eq!(hits.albums[0].track_count, 2);
        assert_eq!(track_ids(&hits), vec![1, 3]);
    }

    #[test]
    fn search_is_case_insensitive() {
        let lib = fixture();
        let lower = search_library(&lib, "radiohead");
        let upper = search_library(&lib, "RADIOHEAD");
        assert_eq!(lower, upper);
        assert_eq!(track_ids(&lower), vec![1, 2, 3]);
    }

    #[test]
    fn no_match_returns_empty() {
        let lib = fixture();
        assert_eq!(search_library(&lib, "zzzz-nope"), SearchResults::default());
    }
}
