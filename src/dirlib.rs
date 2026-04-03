//! Directory-scanning music library backend.
//!
//! Recursively scans a folder for audio files and reads metadata from ID3
//! tags (MP3) or infers it from the directory structure (other formats).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use id3::TagLike;

use crate::library::{MusicLibrary, Playlist, Track};

/// Audio file extensions recognized by the scanner.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wma", "wav", "aiff",
];

/// A music library built by scanning a directory tree.
pub struct DirectoryLibrary {
    tracks: HashMap<u64, Track>,
    root: String,
}

impl DirectoryLibrary {
    /// Recursively scan a directory for audio files and build a library.
    pub fn scan(path: &str) -> Result<Self, String> {
        let root = Path::new(path);
        if !root.is_dir() {
            return Err(format!("Not a directory: {path}"));
        }
        let mut tracks = HashMap::new();
        scan_dir(root, &mut tracks);
        Ok(DirectoryLibrary {
            tracks,
            root: path.to_string(),
        })
    }
}

fn scan_dir(dir: &Path, tracks: &mut HashMap<u64, Track>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, tracks);
        } else if is_audio_file(&path) {
            let id = hash_path(&path);
            let track = build_track(&path, id);
            tracks.insert(id, track);
        }
    }
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn hash_path(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    hasher.finish()
}

fn build_track(path: &Path, id: u64) -> Track {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Try ID3 tags for MP3 files.
    if ext == "mp3" {
        if let Some(track) = track_from_id3(path, id) {
            return track;
        }
    }

    // Fallback: infer metadata from directory structure.
    track_from_path(path, id)
}

/// Read metadata from ID3 tags (MP3 files).
fn track_from_id3(path: &Path, id: u64) -> Option<Track> {
    let tag = id3::Tag::read_from_path(path).ok()?;
    let name = tag
        .title()
        .map(|s| s.to_string())
        .unwrap_or_else(|| stem(path));
    let artist = tag
        .artist()
        .map(|s| s.to_string())
        .unwrap_or_else(|| parent_name(path, 2));
    let album = tag
        .album()
        .map(|s| s.to_string())
        .unwrap_or_else(|| parent_name(path, 1));

    Some(Track {
        id,
        name,
        artist,
        album,
        album_artist: tag.album_artist().map(|s| s.to_string()),
        genre: tag.genre_parsed().map(|s| s.to_string()),
        year: tag.year().map(|y| y as u32),
        track_number: tag.track(),
        disc_number: tag.disc(),
        total_time_ms: tag.duration().map(|d| d as u64),
        location: Some(path.to_string_lossy().to_string()),
        kind: Some(format!(
            "{} audio file",
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_uppercase()
        )),
    })
}

/// Infer metadata from directory structure: Artist/Album/Track.ext
fn track_from_path(path: &Path, id: u64) -> Track {
    let raw_stem = stem(path);
    let name = crate::strip_track_number(&raw_stem).to_string();

    Track {
        id,
        name,
        artist: parent_name(path, 2),
        album: parent_name(path, 1),
        album_artist: None,
        genre: None,
        year: None,
        track_number: None,
        disc_number: None,
        total_time_ms: None,
        location: Some(path.to_string_lossy().to_string()),
        kind: Some(format!(
            "{} audio file",
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_uppercase()
        )),
    }
}

/// Get the file stem (name without extension).
fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// Get the name of an ancestor directory (1 = parent, 2 = grandparent).
fn parent_name(path: &Path, levels: usize) -> String {
    let mut current = path.parent();
    for _ in 1..levels {
        current = current.and_then(|p| p.parent());
    }
    current
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

impl MusicLibrary for DirectoryLibrary {
    fn artists(&self) -> Vec<&str> {
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

    fn albums(&self) -> Vec<(&str, &str)> {
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

    fn user_playlists(&self) -> Vec<&Playlist> {
        vec![]
    }

    fn artist_tracks(&self, artist: &str) -> Vec<&Track> {
        self.tracks
            .values()
            .filter(|t| t.artist.eq_ignore_ascii_case(artist))
            .collect()
    }

    fn playlist_tracks(&self, _name: &str) -> Vec<&Track> {
        vec![]
    }

    fn album_tracks(&self, album: &str) -> Vec<&Track> {
        self.tracks
            .values()
            .filter(|t| t.album.eq_ignore_ascii_case(album))
            .collect()
    }

    fn album_tracks_by_artist(&self, artist: &str, album: &str) -> Vec<&Track> {
        self.tracks
            .values()
            .filter(|t| {
                t.album.eq_ignore_ascii_case(album) && t.artist.eq_ignore_ascii_case(artist)
            })
            .collect()
    }

    fn tracks_by_name(&self, name: &str) -> Vec<&Track> {
        self.tracks
            .values()
            .filter(|t| t.name.eq_ignore_ascii_case(name))
            .collect()
    }

    fn track_count(&self) -> usize {
        self.tracks.len()
    }

    fn all_tracks(&self) -> Vec<&Track> {
        self.tracks.values().collect()
    }

    fn music_folder(&self) -> Option<&str> {
        Some(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_empty_directory() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib.track_count(), 0);
        assert!(lib.artists().is_empty());
        assert!(lib.user_playlists().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_nonexistent_directory() {
        assert!(DirectoryLibrary::scan("/tmp/nonexistent-zytunes-dir-12345").is_err());
    }

    #[test]
    fn track_from_path_extracts_metadata() {
        let path = Path::new("/music/Radiohead/OK Computer/01 Airbag.flac");
        let track = track_from_path(path, 42);
        assert_eq!(track.name, "Airbag");
        assert_eq!(track.album, "OK Computer");
        assert_eq!(track.artist, "Radiohead");
        assert_eq!(track.id, 42);
    }

    #[test]
    fn is_audio_file_filters_correctly() {
        assert!(is_audio_file(Path::new("song.mp3")));
        assert!(is_audio_file(Path::new("song.FLAC")));
        assert!(is_audio_file(Path::new("song.m4a")));
        assert!(!is_audio_file(Path::new("readme.txt")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
    }

    #[test]
    fn scan_finds_audio_files() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-scan");
        let _ = fs::remove_dir_all(&dir);
        let artist_dir = dir.join("Artist").join("Album");
        fs::create_dir_all(&artist_dir).unwrap();
        // Create dummy files (not valid audio, but the scanner only checks extensions).
        fs::write(artist_dir.join("01 Song.mp3"), b"fake").unwrap();
        fs::write(artist_dir.join("02 Track.flac"), b"fake").unwrap();
        fs::write(artist_dir.join("cover.jpg"), b"fake").unwrap();

        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib.track_count(), 2);
        assert_eq!(lib.artists(), vec!["Artist"]);
        assert_eq!(lib.albums(), vec![("Artist", "Album")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_name_handles_shallow_paths() {
        let path = Path::new("/song.mp3");
        // Root "/" has no file_name(), so both levels return "Unknown".
        assert_eq!(parent_name(path, 1), "Unknown");
        assert_eq!(parent_name(path, 2), "Unknown");

        let path = Path::new("/Music/song.mp3");
        assert_eq!(parent_name(path, 1), "Music");
        assert_eq!(parent_name(path, 2), "Unknown");
    }
}
