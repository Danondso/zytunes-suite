//! Library metadata cache for fast startup.
//!
//! Caches parsed track/playlist data to `~/.cache/zytunes/` so repeat
//! launches skip expensive XML parsing or directory scanning with lofty.

use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::library::{Playlist, Track};

/// Fingerprint of the library source for cache invalidation.
#[derive(Serialize, Deserialize, PartialEq)]
struct Fingerprint {
    /// For XML: the file path. For dir: the root path.
    source: String,
    /// For XML: single entry with mtime+size. For dir: count + newest mtime.
    mtime_secs: u64,
    size: u64,
}

#[derive(Serialize, Deserialize)]
struct CachedLibrary {
    fingerprint: Fingerprint,
    tracks: HashMap<u64, Track>,
    playlists: Vec<Playlist>,
    music_folder_path: Option<String>,
}

fn cache_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache").join("zytunes"))
}

fn cache_path(name: &str) -> Option<std::path::PathBuf> {
    cache_dir().map(|d| d.join(name))
}

fn file_fingerprint(path: &str) -> Option<Fingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(Fingerprint {
        source: path.to_string(),
        mtime_secs: mtime,
        size: meta.len(),
    })
}

/// Build a fingerprint for a directory by counting audio files and tracking
/// the newest modification time. This is much faster than hashing every file.
fn dir_fingerprint(path: &str) -> Option<Fingerprint> {
    let mut newest: u64 = 0;
    let mut count: u64 = 0;
    count_dir(Path::new(path), &mut newest, &mut count);
    Some(Fingerprint {
        source: path.to_string(),
        mtime_secs: newest,
        size: count,
    })
}

fn count_dir(dir: &Path, newest: &mut u64, count: &mut u64) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count_dir(&path, newest, count);
        } else if is_audio_ext(&path) {
            *count += 1;
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(SystemTime::UNIX_EPOCH) {
                        let secs = dur.as_secs();
                        if secs > *newest {
                            *newest = secs;
                        }
                    }
                }
            }
        }
    }
}

fn is_audio_ext(path: &Path) -> bool {
    const EXTS: &[&str] = &[
        "mp3", "flac", "m4a", "aac", "ogg", "opus", "wma", "wav", "aiff", "alac",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| EXTS.contains(&ext.to_lowercase().as_str()))
}

fn load_cache(name: &str) -> Option<CachedLibrary> {
    let path = cache_path(name)?;
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_cache(name: &str, cached: &CachedLibrary) {
    if let Some(path) = cache_path(name) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(data) = serde_json::to_vec(cached) {
            let _ = std::fs::write(&path, data);
        }
    }
}

/// Cached iTunes library data.
pub struct ItunesCacheData {
    pub tracks: HashMap<u64, Track>,
    pub playlists: Vec<Playlist>,
    pub music_folder_path: Option<String>,
}

/// Try to load an iTunes library from cache. Returns None if cache is stale/missing.
pub fn load_itunes_cached(xml_path: &str) -> Option<ItunesCacheData> {
    let fp = file_fingerprint(xml_path)?;
    let cached = load_cache("itunes-library.json")?;
    if cached.fingerprint == fp {
        Some(ItunesCacheData {
            tracks: cached.tracks,
            playlists: cached.playlists,
            music_folder_path: cached.music_folder_path,
        })
    } else {
        None
    }
}

/// Save a parsed iTunes library to cache.
pub fn save_itunes_cache(
    xml_path: &str,
    tracks: &HashMap<u64, Track>,
    playlists: &[Playlist],
    music_folder_path: Option<&str>,
) {
    if let Some(fp) = file_fingerprint(xml_path) {
        save_cache(
            "itunes-library.json",
            &CachedLibrary {
                fingerprint: fp,
                tracks: tracks.clone(),
                playlists: playlists.to_vec(),
                music_folder_path: music_folder_path.map(|s| s.to_string()),
            },
        );
    }
}

/// Try to load a directory library from cache. Returns None if cache is stale/missing.
pub fn load_dirlib_cached(dir_path: &str) -> Option<HashMap<u64, Track>> {
    let fp = dir_fingerprint(dir_path)?;
    let cached = load_cache("dirlib-library.json")?;
    if cached.fingerprint == fp {
        Some(cached.tracks)
    } else {
        None
    }
}

/// Save a parsed directory library to cache.
pub fn save_dirlib_cache(dir_path: &str, tracks: &HashMap<u64, Track>) {
    if let Some(fp) = dir_fingerprint(dir_path) {
        save_cache(
            "dirlib-library.json",
            &CachedLibrary {
                fingerprint: fp,
                tracks: tracks.clone(),
                playlists: Vec::new(),
                music_folder_path: Some(dir_path.to_string()),
            },
        );
    }
}
