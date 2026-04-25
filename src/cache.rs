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
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            // ENOENT on first launch is normal; only log if it's something
            // else (permission denied, bad symlink, etc.).
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("zytunes: cache: read {} failed: {e}", path.display());
            } else {
                eprintln!(
                    "zytunes: cache: no cache file at {} (first scan or previous run failed to save)",
                    path.display()
                );
            }
            return None;
        }
    };
    match serde_json::from_slice::<CachedLibrary>(&data) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!(
                "zytunes: cache: parse {} ({} bytes) failed: {e} — treating as empty",
                path.display(),
                data.len()
            );
            None
        }
    }
}

fn save_raw(dir_path: &str, cached: &CachedLibrary) {
    let Some(path) = cache_path(&dirlib_cache_name(dir_path)) else {
        eprintln!("zytunes: cache: HOME unset, cannot persist library cache");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("zytunes: cache: mkdir {} failed: {e}", parent.display());
            return;
        }
    }
    let data = match serde_json::to_vec(cached) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "zytunes: cache: serialize {} files failed: {e}",
                cached.files.len()
            );
            return;
        }
    };
    // Atomic write: stage to a sibling .tmp, then rename. Without this, a
    // kill mid-write (Ctrl+C, OOM, kernel panic) leaves a truncated JSON on
    // disk; next launch fails to parse it and re-does the entire scan. The
    // rename is atomic on every POSIX filesystem we care about, so the cache
    // file is either the previous valid one or the new valid one — never a
    // half-written mix.
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &data) {
        eprintln!(
            "zytunes: cache: write {} ({} bytes) failed: {e}",
            tmp.display(),
            data.len()
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        eprintln!(
            "zytunes: cache: rename {} -> {} failed: {e}",
            tmp.display(),
            path.display()
        );
        // Best-effort cleanup of the orphan; nothing we can do if this fails.
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Load the per-file cache for `dir_path`. Returns an empty map if the cache
/// is missing, unreadable, or for a different root.
pub fn load_dirlib_cache(dir_path: &str) -> HashMap<String, CachedFile> {
    match load_raw(dir_path) {
        Some(c) if c.root == dir_path => c.files,
        Some(c) => {
            // Path normalisation skew is the silent killer here — a single
            // trailing slash difference between launches (e.g. `~/Music`
            // vs `~/Music/`) silently invalidates the entire cache.
            eprintln!(
                "zytunes: cache: stored root {:?} != requested root {:?} — discarding {} cached entries",
                c.root,
                dir_path,
                c.files.len()
            );
            HashMap::new()
        }
        None => HashMap::new(),
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
