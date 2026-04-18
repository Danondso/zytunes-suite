//! Per-file metadata cache for the directory scanner.
//!
//! Caches each audio file's parsed `Track` keyed by its absolute path, with a
//! `(mtime, size)` fingerprint. On every scan we stat all files (cheap) and
//! only re-parse those whose fingerprint has changed — so repeat launches
//! only pay the lofty cost for tracks that were actually added or modified.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::library::Track;

/// `(mtime, size)` fingerprint for one audio file.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFingerprint {
    pub mtime_secs: u64,
    pub size: u64,
}

impl FileFingerprint {
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some(FileFingerprint {
            mtime_secs,
            size: meta.len(),
        })
    }
}

/// One cached audio file: its fingerprint plus the parsed track it produced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedFile {
    pub fingerprint: FileFingerprint,
    pub track: Track,
}

#[derive(Serialize, Deserialize, Default)]
struct CachedLibrary {
    root: String,
    /// Keyed by absolute file path.
    files: HashMap<String, CachedFile>,
}

fn cache_dir() -> Option<std::path::PathBuf> {
    // The dirlib cache is keyed by a hash of the scan root (see
    // `dirlib_cache_name`), so identical `~/Music` scans from different
    // worktrees can safely share one entry. Keeping this at `$HOME/.cache`
    // — and *not* consulting `ZYTUNES_CACHE_DIR` — avoids re-running the
    // lofty pass for large libraries every time a new worktree spins up.
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache").join("zytunes"))
}

fn cache_path(name: &str) -> Option<std::path::PathBuf> {
    cache_dir().map(|d| d.join(name))
}

/// Cache filename keyed by a hash of the source path so multiple scan roots
/// (and tests) don't clobber each other's caches.
fn dirlib_cache_name(dir_path: &str) -> String {
    let mut hasher = DefaultHasher::new();
    dir_path.hash(&mut hasher);
    format!("dirlib-library-{:016x}.json", hasher.finish())
}

fn load_raw(dir_path: &str) -> Option<CachedLibrary> {
    let path = cache_path(&dirlib_cache_name(dir_path))?;
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_raw(dir_path: &str, cached: &CachedLibrary) {
    let Some(path) = cache_path(&dirlib_cache_name(dir_path)) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_vec(cached) {
        let _ = std::fs::write(&path, data);
    }
}

/// Load the per-file cache for `dir_path`. Returns an empty map if the cache
/// is missing, unreadable, or for a different root.
pub fn load_dirlib_cache(dir_path: &str) -> HashMap<String, CachedFile> {
    match load_raw(dir_path) {
        Some(c) if c.root == dir_path => c.files,
        _ => HashMap::new(),
    }
}

/// Save the per-file cache for `dir_path`.
pub fn save_dirlib_cache(dir_path: &str, files: HashMap<String, CachedFile>) {
    save_raw(
        dir_path,
        &CachedLibrary {
            root: dir_path.to_string(),
            files,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_ignores_zytunes_cache_dir_override() {
        // The dirlib cache deliberately lives under `$HOME/.cache/zytunes`
        // regardless of `ZYTUNES_CACHE_DIR`, so big-library scans are
        // reused across worktrees. Guard that contract.
        let Some(home) = std::env::var("HOME").ok() else {
            return; // Can't assert relative to HOME if it isn't set.
        };
        let prev = std::env::var("ZYTUNES_CACHE_DIR").ok();
        std::env::set_var("ZYTUNES_CACHE_DIR", "/tmp/zytunes-should-be-ignored");

        let got = cache_dir().expect("HOME is set");
        assert_eq!(
            got,
            Path::new(&home).join(".cache").join("zytunes"),
            "dirlib cache must stay under $HOME/.cache/zytunes, not follow ZYTUNES_CACHE_DIR"
        );

        match prev {
            Some(v) => std::env::set_var("ZYTUNES_CACHE_DIR", v),
            None => std::env::remove_var("ZYTUNES_CACHE_DIR"),
        }
    }

    #[test]
    fn dirlib_cache_name_is_per_path() {
        let a = dirlib_cache_name("/Users/me/Music");
        let b = dirlib_cache_name("/tmp/zytunes-test-scan");
        assert_ne!(
            a, b,
            "distinct source paths should produce distinct cache filenames"
        );
        assert_eq!(
            dirlib_cache_name("/Users/me/Music"),
            a,
            "same source path should produce the same cache filename"
        );
        assert!(a.starts_with("dirlib-library-"));
        assert!(a.ends_with(".json"));
    }
}
