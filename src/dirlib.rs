//! Directory-scanning music library backend.
//!
//! Recursively scans a folder for audio files and reads metadata from ID3
//! tags (MP3) or infers it from the directory structure (other formats).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::library::{MusicLibrary, Track};

/// Audio file extensions recognized by the scanner.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wma", "wav", "aiff", "alac",
];

/// A single metadata sample emitted during a scan, for progress UI.
#[derive(Clone, Debug)]
pub struct TrackSample {
    pub artist: String,
    pub album: String,
    pub name: String,
}

/// Progress update emitted from `scan_with_progress`.
#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub completed: u64,
    pub total: u64,
    pub sample: Option<TrackSample>,
}

/// A music library built by scanning a directory tree.
pub struct DirectoryLibrary {
    tracks: HashMap<u64, Track>,
    root: String,
}

impl DirectoryLibrary {
    /// Recursively scan a directory for audio files and build a library.
    ///
    /// Uses a disk cache (`~/.cache/zytunes/`) to skip metadata reads when no
    /// files have been added or modified since the last run. When the cache is
    /// stale, metadata reads are parallelized with rayon.
    pub fn scan(path: &str) -> Result<Self, String> {
        Self::scan_with_progress(path, |_| {})
    }

    /// Same as `scan`, but invokes `on_progress` for each track built.
    ///
    /// The callback is called from rayon worker threads (so it must be `Sync`)
    /// and is invoked once per track, plus once at the start with
    /// `completed = 0` and `sample = None` to report the total.
    pub fn scan_with_progress<F>(path: &str, on_progress: F) -> Result<Self, String>
    where
        F: Fn(ScanProgress) + Sync,
    {
        let root = Path::new(path);
        if !root.is_dir() {
            return Err(format!("Not a directory: {path}"));
        }

        // Load the previous per-file cache. Missing / unreadable = empty map,
        // so first launches just fall through to a full parse.
        let cached = crate::cache::load_dirlib_cache(path);

        // Parallel directory walk.
        let paths = collect_audio_paths(root);
        let total = paths.len() as u64;
        on_progress(ScanProgress {
            completed: 0,
            total,
            sample: None,
        });

        // For each file: stat → fingerprint → reuse cached Track if the
        // fingerprint matches, otherwise run lofty to parse tags + duration.
        // Stats are cheap; lofty is the expensive step, so the cache makes
        // repeat launches nearly instant even with `read_properties` on.
        let completed = AtomicU64::new(0);
        let entries: Vec<(String, crate::cache::CachedFile)> = paths
            .par_iter()
            .filter_map(|p| {
                let key = p.to_string_lossy().to_string();
                let fingerprint = crate::cache::FileFingerprint::from_path(p)?;

                let track = match cached.get(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.track.clone(),
                    _ => build_track(p, hash_path(p)),
                };

                let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(ScanProgress {
                    completed: n,
                    total,
                    sample: Some(TrackSample {
                        artist: track.artist.clone(),
                        album: track.album.clone(),
                        name: track.name.clone(),
                    }),
                });

                Some((key, crate::cache::CachedFile { fingerprint, track }))
            })
            .collect();

        let tracks: HashMap<u64, Track> = entries
            .iter()
            .map(|(_, cf)| (cf.track.id, cf.track.clone()))
            .collect();

        // Persist the fresh cache — implicitly drops entries for files that
        // disappeared from the tree since the last scan.
        let new_cache: HashMap<String, crate::cache::CachedFile> = entries.into_iter().collect();
        crate::cache::save_dirlib_cache(path, new_cache);

        Ok(DirectoryLibrary {
            tracks,
            root: path.to_string(),
        })
    }
}

/// Recursively collect audio file paths, walking each subdirectory in parallel.
///
/// For large libraries the serial `read_dir` walk alone can take several seconds;
/// rayon's recursive parallelism turns it into a few hundred milliseconds on SSD.
fn collect_audio_paths(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e.flatten().collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };
    entries
        .par_iter()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                collect_audio_paths(&path)
            } else if is_audio_file(&path) {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect()
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
    // Try lofty for all formats (FLAC, M4A, OGG, WAV, MP3, etc.)
    if let Some(track) = track_from_lofty(path, id) {
        return track;
    }

    // Fallback: infer metadata from directory structure.
    track_from_path(path, id)
}

/// Read metadata (tags + duration) from any audio file using lofty.
///
/// Reading properties is slow for MP3 VBR files because lofty has to sample
/// frames across the whole file to compute duration, but the per-file cache
/// makes sure we only pay that cost once per file per change.
fn track_from_lofty(path: &Path, id: u64) -> Option<Track> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Accessor;

    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;

    // Require at least a title or artist to consider the tag useful
    let has_useful_data = tag.title().is_some() || tag.artist().is_some();
    if !has_useful_data {
        return None;
    }

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
        genre: tag.genre().map(|s| s.to_string()),
        year: tag.year(),
        track_number: tag.track(),
        disc_number: tag.disk(),
        total_time_ms: {
            let dur = tagged.properties().duration();
            if dur.is_zero() {
                None
            } else {
                Some(dur.as_millis() as u64)
            }
        },
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

    fn artist_tracks<'a>(&'a self, artist: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let artist = artist.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.artist.eq_ignore_ascii_case(&artist)),
        )
    }

    fn album_tracks<'a>(&'a self, album: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let album = album.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.album.eq_ignore_ascii_case(&album)),
        )
    }

    fn album_tracks_by_artist<'a>(
        &'a self,
        artist: &str,
        album: &str,
    ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let artist = artist.to_string();
        let album = album.to_string();
        Box::new(self.tracks.values().filter(move |t| {
            t.album.eq_ignore_ascii_case(&album) && t.artist.eq_ignore_ascii_case(&artist)
        }))
    }

    fn tracks_by_name<'a>(&'a self, name: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let name = name.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.name.eq_ignore_ascii_case(&name)),
        )
    }

    fn track_count(&self) -> usize {
        self.tracks.len()
    }

    fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
        Box::new(self.tracks.values())
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
    fn scan_with_progress_reports_total_and_per_track() {
        use std::sync::Mutex;

        let dir = std::env::temp_dir().join("zytunes-dirlib-progress");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("ArtistP").join("AlbumP");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Alpha.mp3"), b"fake").unwrap();
        fs::write(album.join("02 Beta.mp3"), b"fake").unwrap();
        fs::write(album.join("03 Gamma.mp3"), b"fake").unwrap();

        let updates: Mutex<Vec<ScanProgress>> = Mutex::new(Vec::new());
        let lib = DirectoryLibrary::scan_with_progress(dir.to_str().unwrap(), |p| {
            updates.lock().unwrap().push(p);
        })
        .unwrap();
        assert_eq!(lib.track_count(), 3);

        let updates = updates.into_inner().unwrap();
        // First emission is the total announcement with no sample.
        assert_eq!(updates.first().unwrap().total, 3);
        assert_eq!(updates.first().unwrap().completed, 0);
        assert!(updates.first().unwrap().sample.is_none());

        // One per-track update plus the opener = 4.
        assert_eq!(updates.len(), 4);
        let with_samples: Vec<_> = updates.iter().filter(|u| u.sample.is_some()).collect();
        assert_eq!(with_samples.len(), 3);
        for u in &with_samples {
            assert_eq!(u.total, 3);
            let s = u.sample.as_ref().unwrap();
            assert_eq!(s.artist, "ArtistP");
            assert_eq!(s.album, "AlbumP");
        }
        // Max completed count equals the total.
        let max_completed = updates.iter().map(|u| u.completed).max().unwrap();
        assert_eq!(max_completed, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reuses_cached_tracks_for_unchanged_files() {
        // Use a distinct dir name so we don't race with other tests sharing
        // the same cache slot.
        let dir = std::env::temp_dir().join("zytunes-dirlib-incremental");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("CacheArtist").join("CacheAlbum");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Original.mp3"), b"fake").unwrap();
        fs::write(album.join("02 Second.mp3"), b"fake").unwrap();

        // First scan: nothing cached, everything parsed fresh.
        let lib1 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib1.track_count(), 2);

        // Peek at the cache: both files should be recorded.
        let cached = crate::cache::load_dirlib_cache(dir.to_str().unwrap());
        assert_eq!(cached.len(), 2);
        let original_fp = cached
            .get(album.join("01 Original.mp3").to_str().unwrap())
            .expect("first file should be cached")
            .fingerprint;

        // Add a third file. Second scan should reuse the first two from cache
        // and parse only the new one.
        fs::write(album.join("03 Added.mp3"), b"fake").unwrap();
        let lib2 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib2.track_count(), 3);

        let cached2 = crate::cache::load_dirlib_cache(dir.to_str().unwrap());
        assert_eq!(cached2.len(), 3);
        // Unchanged file's fingerprint must survive across scans.
        assert_eq!(
            cached2
                .get(album.join("01 Original.mp3").to_str().unwrap())
                .unwrap()
                .fingerprint,
            original_fp,
        );
        // Deleted file must be dropped from the cache.
        fs::remove_file(album.join("01 Original.mp3")).unwrap();
        let lib3 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib3.track_count(), 2);
        let cached3 = crate::cache::load_dirlib_cache(dir.to_str().unwrap());
        assert_eq!(cached3.len(), 2);
        assert!(!cached3.contains_key(album.join("01 Original.mp3").to_str().unwrap()));

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

    /// Generate a minimal valid WAV file.
    fn make_test_wav(path: &Path) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let num_samples: usize = 100;
        let data_size =
            (num_samples * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let file_size = 36 + data_size;

        let mut f = fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_size as usize]).unwrap();
    }

    #[test]
    fn build_track_reads_wav_tags_via_lofty() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-lofty");
        let _ = fs::remove_dir_all(&dir);
        let artist_dir = dir.join("FallbackArtist").join("FallbackAlbum");
        fs::create_dir_all(&artist_dir).unwrap();

        let wav_path = artist_dir.join("song.wav");
        make_test_wav(&wav_path);

        // Write tags with lofty (not id3)
        {
            use lofty::file::TaggedFileExt;
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Bjork".to_string());
            tag.set_album("Post".to_string());
            tag.set_title("Army of Me".to_string());
            tag.set_track(1);
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let track = build_track(&wav_path, 99);
        assert_eq!(track.artist, "Bjork");
        assert_eq!(track.album, "Post");
        assert_eq!(track.name, "Army of Me");
        assert_eq!(track.track_number, Some(1));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_track_path_fallback_for_untagged() {
        // File with no tags should infer from Artist/Album/Track.ext path structure
        let path = Path::new("/music/Radiohead/OK Computer/03 Subterranean Homesick Alien.flac");
        let track = build_track(path, 42);
        assert_eq!(track.artist, "Radiohead");
        assert_eq!(track.album, "OK Computer");
        assert_eq!(track.name, "Subterranean Homesick Alien");
    }
}
