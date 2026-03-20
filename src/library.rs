//! iTunes XML library parser.
//!
//! Parses Apple's iTunes/Music Library.xml plist format to extract
//! tracks and playlists. Uses a streaming parser for memory efficiency.

use std::collections::HashMap;
use std::io::BufRead;

/// A track from the iTunes library.
#[derive(Debug, Clone)]
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

/// A playlist from the iTunes library.
#[derive(Debug, Clone)]
pub struct Playlist {
    pub name: String,
    pub track_ids: Vec<u64>,
}

/// The parsed iTunes library.
pub struct ItunesLibrary {
    pub tracks: HashMap<u64, Track>,
    pub playlists: Vec<Playlist>,
    pub music_folder: Option<String>,
}

impl ItunesLibrary {
    /// Parse an iTunes Library.xml file.
    pub fn parse(path: &str) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Cannot open {path}: {e}"))?;
        let reader = std::io::BufReader::new(file);
        parse_itunes_xml(reader)
    }

    /// Get all tracks belonging to a playlist by name.
    pub fn playlist_tracks(&self, name: &str) -> Vec<&Track> {
        for pl in &self.playlists {
            if pl.name.eq_ignore_ascii_case(name) {
                return pl
                    .track_ids
                    .iter()
                    .filter_map(|id| self.tracks.get(id))
                    .collect();
            }
        }
        vec![]
    }

    /// Get all tracks by a given artist.
    pub fn artist_tracks(&self, artist: &str) -> Vec<&Track> {
        self.tracks
            .values()
            .filter(|t| t.artist.eq_ignore_ascii_case(artist))
            .collect()
    }

    /// Get all unique artist names, sorted.
    pub fn artists(&self) -> Vec<&str> {
        let mut artists: Vec<&str> = self
            .tracks
            .values()
            .map(|t| t.artist.as_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        artists.sort_unstable();
        artists
    }

    /// Get all unique (artist, album) pairs, sorted.
    pub fn albums(&self) -> Vec<(&str, &str)> {
        let mut albums: Vec<(&str, &str)> = self
            .tracks
            .values()
            .map(|t| (t.artist.as_str(), t.album.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        albums.sort_unstable();
        albums
    }

    /// Get user-visible playlists (exclude system playlists).
    pub fn user_playlists(&self) -> Vec<&Playlist> {
        self.playlists
            .iter()
            .filter(|p| !is_system_playlist(&p.name))
            .collect()
    }
}

fn is_system_playlist(name: &str) -> bool {
    matches!(
        name,
        "Library" | "Downloaded" | "Music" | "Podcasts" | "Audiobooks"
    )
}

fn decode_location(url: &str) -> String {
    let path = url.strip_prefix("file://").unwrap_or(url);
    percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .to_string()
}

/// Which section of the plist we're currently in.
#[derive(PartialEq, Clone, Copy)]
enum Section {
    Root,
    Tracks,
    TrackEntry,
    Playlists,
    PlaylistEntry,
    PlaylistItems,
}

fn parse_itunes_xml<R: BufRead>(reader: R) -> Result<ItunesLibrary, String> {
    use quick_xml::events::Event;
    let mut xml = quick_xml::Reader::from_reader(reader);
    xml.config_mut().trim_text(true);

    let mut library = ItunesLibrary {
        tracks: HashMap::new(),
        playlists: Vec::new(),
        music_folder: None,
    };

    let mut buf = Vec::with_capacity(4096);
    let mut section = Section::Root;
    let mut current_tag = String::new(); // The element tag we're currently inside (key, string, integer, etc.)
    let mut last_key = String::new(); // The last <key> value we saw
    let mut dict_depth = 0u32;

    let mut track_fields: HashMap<String, String> = HashMap::new();
    let mut pl_name = String::new();
    let mut pl_track_ids: Vec<u64> = Vec::new();
    let mut pl_item_key = String::new();
    let mut track_count = 0u32;

    loop {
        buf.clear();
        match xml.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,

            Ok(Event::Start(ref e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if current_tag == "dict" {
                    dict_depth += 1;
                    match section {
                        Section::Tracks if dict_depth == 3 => {
                            section = Section::TrackEntry;
                            track_fields.clear();
                        }
                        Section::Playlists => {
                            section = Section::PlaylistEntry;
                            pl_name.clear();
                            pl_track_ids.clear();
                        }
                        _ => {}
                    }
                } else if current_tag == "array"
                    && section == Section::PlaylistEntry
                    && last_key == "Playlist Items"
                {
                    section = Section::PlaylistItems;
                }
            }

            Ok(Event::End(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "dict" {
                    match section {
                        Section::TrackEntry if dict_depth == 3 => {
                            if let Some(track) = build_track(&track_fields) {
                                track_count += 1;
                                if track_count.is_multiple_of(10000) {
                                    eprint!("\r  Parsed {} tracks...    ", track_count);
                                }
                                library.tracks.insert(track.id, track);
                            }
                            section = Section::Tracks;
                        }
                        Section::PlaylistEntry => {
                            if !pl_name.is_empty() {
                                library.playlists.push(Playlist {
                                    name: pl_name.clone(),
                                    track_ids: pl_track_ids.clone(),
                                });
                            }
                            section = Section::Playlists;
                        }
                        Section::Tracks if dict_depth == 2 => {
                            section = Section::Root;
                        }
                        _ => {}
                    }
                    dict_depth -= 1;
                } else if tag == "array" {
                    if section == Section::PlaylistItems {
                        section = Section::PlaylistEntry;
                    } else if section == Section::Playlists {
                        section = Section::Root;
                    }
                }
                current_tag.clear();
            }

            Ok(Event::Text(ref e)) => {
                let text = e.unescape().unwrap_or_default().to_string();

                if current_tag == "key" {
                    last_key = text.clone();
                    // Detect section transitions.
                    if section == Section::Root && text == "Tracks" {
                        section = Section::Tracks;
                    } else if section == Section::Root && text == "Playlists" {
                        section = Section::Playlists;
                    }
                    if section == Section::PlaylistItems {
                        pl_item_key = text;
                    }
                } else if current_tag == "string"
                    || current_tag == "integer"
                    || current_tag == "date"
                {
                    match section {
                        Section::Root if last_key == "Music Folder" => {
                            library.music_folder = Some(decode_location(&text));
                        }
                        Section::TrackEntry => {
                            track_fields.insert(last_key.clone(), text);
                        }
                        Section::PlaylistEntry => {
                            if last_key == "Name" {
                                pl_name = text;
                            }
                        }
                        Section::PlaylistItems => {
                            if pl_item_key == "Track ID" {
                                if let Ok(id) = text.parse::<u64>() {
                                    pl_track_ids.push(id);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            Ok(Event::Empty(_)) => {
                // Self-closing tags like <true/>, <false/> — not needed.
            }

            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    xml.error_position()
                ))
            }
            _ => {}
        }
    }

    if track_count > 0 {
        eprint!("\r{}\r", " ".repeat(40));
    }

    Ok(library)
}

/// Parse an iTunes Library.xml from a string (convenience for testing).
#[cfg(test)]
fn parse_xml_str(xml: &str) -> Result<ItunesLibrary, String> {
    parse_itunes_xml(std::io::Cursor::new(xml))
}

fn build_track(fields: &HashMap<String, String>) -> Option<Track> {
    let name = fields.get("Name")?.clone();
    let id = fields.get("Track ID")?.parse::<u64>().ok()?;
    let artist = fields
        .get("Artist")
        .cloned()
        .unwrap_or_else(|| "Unknown Artist".into());
    let album = fields
        .get("Album")
        .cloned()
        .unwrap_or_else(|| "Unknown Album".into());
    let location = fields.get("Location").map(|s| decode_location(s));

    Some(Track {
        id,
        name,
        artist,
        album,
        album_artist: fields.get("Album Artist").cloned(),
        genre: fields.get("Genre").cloned(),
        year: fields.get("Year").and_then(|s| s.parse().ok()),
        track_number: fields.get("Track Number").and_then(|s| s.parse().ok()),
        disc_number: fields.get("Disc Number").and_then(|s| s.parse().ok()),
        total_time_ms: fields.get("Total Time").and_then(|s| s.parse().ok()),
        location,
        kind: fields.get("Kind").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_location_handles_prefixes_and_encoding() {
        assert_eq!(decode_location("file:///Music/song.mp3"), "/Music/song.mp3");
        assert_eq!(
            decode_location("file:///Music/My%20Song%20%232.mp3"),
            "/Music/My Song #2.mp3"
        );
        assert_eq!(decode_location("/Music/song.mp3"), "/Music/song.mp3");
    }

    #[test]
    fn is_system_playlist_classification() {
        for name in &["Library", "Downloaded", "Music", "Podcasts", "Audiobooks"] {
            assert!(is_system_playlist(name), "{name} should be system");
        }
        for name in &["Road Trip", "Classic Rock", ""] {
            assert!(!is_system_playlist(name), "{name} should not be system");
        }
    }

    #[test]
    fn build_track_with_all_fields() {
        let mut fields = HashMap::new();
        fields.insert("Track ID".into(), "42".into());
        fields.insert("Name".into(), "Test Song".into());
        fields.insert("Artist".into(), "Test Artist".into());
        fields.insert("Album".into(), "Test Album".into());
        fields.insert("Genre".into(), "Rock".into());
        fields.insert("Year".into(), "2024".into());
        fields.insert("Track Number".into(), "3".into());
        fields.insert("Location".into(), "file:///Music/test.mp3".into());
        fields.insert("Kind".into(), "MPEG audio file".into());

        let track = build_track(&fields).unwrap();
        assert_eq!(track.id, 42);
        assert_eq!(track.name, "Test Song");
        assert_eq!(track.genre.as_deref(), Some("Rock"));
        assert_eq!(track.year, Some(2024));
        assert_eq!(track.location.as_deref(), Some("/Music/test.mp3"));
    }

    #[test]
    fn build_track_rejects_invalid_input() {
        // Missing name
        let mut f = HashMap::new();
        f.insert("Track ID".into(), "1".into());
        assert!(build_track(&f).is_none());

        // Missing ID
        let mut f = HashMap::new();
        f.insert("Name".into(), "Song".into());
        assert!(build_track(&f).is_none());

        // Non-numeric ID
        let mut f = HashMap::new();
        f.insert("Track ID".into(), "abc".into());
        f.insert("Name".into(), "Song".into());
        assert!(build_track(&f).is_none());
    }

    #[test]
    fn build_track_defaults_artist_and_album() {
        let mut fields = HashMap::new();
        fields.insert("Track ID".into(), "1".into());
        fields.insert("Name".into(), "Song".into());
        let track = build_track(&fields).unwrap();
        assert_eq!(track.artist, "Unknown Artist");
        assert_eq!(track.album, "Unknown Album");
    }

    // -- ItunesLibrary query methods --

    fn make_lib() -> ItunesLibrary {
        let mut tracks = HashMap::new();
        tracks.insert(
            1,
            Track {
                id: 1,
                name: "Song A".into(),
                artist: "Radiohead".into(),
                album: "OK Computer".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );
        tracks.insert(
            2,
            Track {
                id: 2,
                name: "Song B".into(),
                artist: "Radiohead".into(),
                album: "Kid A".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );
        tracks.insert(
            3,
            Track {
                id: 3,
                name: "Song C".into(),
                artist: "Bjork".into(),
                album: "Homogenic".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );

        ItunesLibrary {
            tracks,
            playlists: vec![
                Playlist {
                    name: "Library".into(),
                    track_ids: vec![1, 2, 3],
                },
                Playlist {
                    name: "My Favs".into(),
                    track_ids: vec![1, 3],
                },
            ],
            music_folder: None,
        }
    }

    #[test]
    fn artist_tracks_case_insensitive() {
        let lib = make_lib();
        assert_eq!(lib.artist_tracks("radiohead").len(), 2);
        assert!(lib.artist_tracks("Nobody").is_empty());
    }

    #[test]
    fn artists_sorted_and_unique() {
        let lib = make_lib();
        assert_eq!(lib.artists(), vec!["Bjork", "Radiohead"]);
    }

    #[test]
    fn albums_sorted_and_unique() {
        let lib = make_lib();
        let albums = lib.albums();
        assert_eq!(albums.len(), 3);
        assert_eq!(albums[0], ("Bjork", "Homogenic"));
    }

    #[test]
    fn playlist_tracks_case_insensitive() {
        let lib = make_lib();
        let tracks = lib.playlist_tracks("my favs");
        assert_eq!(tracks.len(), 2);
        assert!(lib.playlist_tracks("Nonexistent").is_empty());
    }

    #[test]
    fn user_playlists_excludes_system() {
        let lib = make_lib();
        let names: Vec<&str> = lib
            .user_playlists()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert!(!names.contains(&"Library"));
        assert!(names.contains(&"My Favs"));
    }

    // -- XML parsing --

    #[test]
    fn parse_minimal_library_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Tracks</key>
    <dict>
        <key>100</key>
        <dict>
            <key>Track ID</key><integer>100</integer>
            <key>Name</key><string>Test Track</string>
            <key>Artist</key><string>Test Artist</string>
            <key>Album</key><string>Test Album</string>
            <key>Location</key><string>file:///Music/test.mp3</string>
        </dict>
    </dict>
    <key>Playlists</key>
    <array>
        <dict>
            <key>Name</key><string>My Playlist</string>
            <key>Playlist Items</key>
            <array>
                <dict>
                    <key>Track ID</key><integer>100</integer>
                </dict>
            </array>
        </dict>
    </array>
</dict>
</plist>"#;

        let lib = parse_xml_str(xml).unwrap();
        assert_eq!(lib.tracks.len(), 1);
        let track = lib.tracks.get(&100).unwrap();
        assert_eq!(track.name, "Test Track");
        assert_eq!(track.artist, "Test Artist");
        assert_eq!(track.location.as_deref(), Some("/Music/test.mp3"));

        assert_eq!(lib.playlists.len(), 1);
        assert_eq!(lib.playlists[0].name, "My Playlist");
        assert_eq!(lib.playlists[0].track_ids, vec![100]);
    }

    #[test]
    fn parse_empty_library_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict></dict></plist>"#;

        let lib = parse_xml_str(xml).unwrap();
        assert!(lib.tracks.is_empty());
        assert!(lib.playlists.is_empty());
    }

    #[test]
    fn parse_library_with_music_folder() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Music Folder</key><string>file:///Users/me/Music/</string>
    <key>Tracks</key><dict></dict>
</dict>
</plist>"#;

        let lib = parse_xml_str(xml).unwrap();
        assert_eq!(lib.music_folder.as_deref(), Some("/Users/me/Music/"));
    }

    #[test]
    fn parse_track_with_missing_optional_fields() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Tracks</key>
    <dict>
        <key>1</key>
        <dict>
            <key>Track ID</key><integer>1</integer>
            <key>Name</key><string>Minimal</string>
        </dict>
    </dict>
</dict>
</plist>"#;

        let lib = parse_xml_str(xml).unwrap();
        assert_eq!(lib.tracks.len(), 1);
        let track = lib.tracks.get(&1).unwrap();
        assert_eq!(track.name, "Minimal");
        assert_eq!(track.artist, "Unknown Artist");
        assert_eq!(track.album, "Unknown Album");
        assert!(track.location.is_none());
        assert!(track.genre.is_none());
    }

    #[test]
    fn parse_multiple_tracks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Tracks</key>
    <dict>
        <key>1</key>
        <dict>
            <key>Track ID</key><integer>1</integer>
            <key>Name</key><string>Song 1</string>
        </dict>
        <key>2</key>
        <dict>
            <key>Track ID</key><integer>2</integer>
            <key>Name</key><string>Song 2</string>
        </dict>
        <key>3</key>
        <dict>
            <key>Track ID</key><integer>3</integer>
            <key>Name</key><string>Song 3</string>
        </dict>
    </dict>
</dict>
</plist>"#;

        let lib = parse_xml_str(xml).unwrap();
        assert_eq!(lib.tracks.len(), 3);
    }
}
