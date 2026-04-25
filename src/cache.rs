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

/// A callable that receives diagnostic messages from cache operations.
///
/// Using `Arc<dyn Fn>` lets callers forward messages to a channel (TUI) or
/// write to stderr (CLI) without lifetime constraints on the closure.
pub type Logger = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// A logger that writes to stderr — the default for CLI code paths.
pub fn stderr_logger(msg: &str) {
    eprintln!("{msg}");
}

/// Convenience: wrap `stderr_logger` in an `Arc` for use as the default.
pub fn default_logger() -> Logger {
    std::sync::Arc::new(stderr_logger)
}

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

/// Bumped whenever the on-disk shape of a cached `Track` changes such that
/// stale cache entries would silently carry `None` for new fields. On
/// mismatch we discard the cache and force a re-scan so the new fields get
/// populated rather than sitting empty until each file is re-tagged.
///
/// Bump history:
///   1 → 2 (2026-04): added extended lofty metadata + audio properties
///   (composer, isrc, mb_*, replaygain_*, sample_rate, channels, …).
const CACHE_SCHEMA_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, Default)]
struct CachedLibrary {
    root: String,
    /// Default of `0` for caches written before the field existed — treated
    /// as a stale schema and discarded by `load_dirlib_cache`.
    #[serde(default)]
    schema_version: u32,
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

fn load_raw(dir_path: &str, log: &Logger) -> Option<CachedLibrary> {
    let path = cache_path(&dirlib_cache_name(dir_path))?;
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            // ENOENT on first launch is normal; only log if it's something
            // else (permission denied, bad symlink, etc.).
            if e.kind() != std::io::ErrorKind::NotFound {
                log(&format!(
                    "zytunes: cache: read {} failed: {e}",
                    path.display()
                ));
            } else {
                log(&format!(
                    "zytunes: cache: no cache file at {} (first scan or previous run failed to save)",
                    path.display()
                ));
            }
            return None;
        }
    };
    match serde_json::from_slice::<CachedLibrary>(&data) {
        Ok(c) => Some(c),
        Err(e) => {
            log(&format!(
                "zytunes: cache: parse {} ({} bytes) failed: {e} — treating as empty",
                path.display(),
                data.len()
            ));
            None
        }
    }
}

fn save_raw(dir_path: &str, cached: &CachedLibrary, log: &Logger) {
    let Some(path) = cache_path(&dirlib_cache_name(dir_path)) else {
        log("zytunes: cache: HOME unset, cannot persist library cache");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log(&format!(
                "zytunes: cache: mkdir {} failed: {e}",
                parent.display()
            ));
            return;
        }
    }
    let data = match serde_json::to_vec(cached) {
        Ok(d) => d,
        Err(e) => {
            log(&format!(
                "zytunes: cache: serialize {} files failed: {e}",
                cached.files.len()
            ));
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
        log(&format!(
            "zytunes: cache: write {} ({} bytes) failed: {e}",
            tmp.display(),
            data.len()
        ));
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log(&format!(
            "zytunes: cache: rename {} -> {} failed: {e}",
            tmp.display(),
            path.display()
        ));
        // Best-effort cleanup of the orphan; nothing we can do if this fails.
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Load the per-file cache for `dir_path`. Returns an empty map if the cache
/// is missing, unreadable, for a different root, or for an older schema.
///
/// `log` receives any diagnostic messages (cache miss, parse error, etc.).
/// Pass [`default_logger`] for the CLI, or a channel-routing closure for the TUI.
pub fn load_dirlib_cache(dir_path: &str, log: &Logger) -> HashMap<String, CachedFile> {
    match load_raw(dir_path, log) {
        Some(c) if c.root != dir_path => {
            // Path normalisation skew is the silent killer here — a single
            // trailing slash difference between launches (e.g. `~/Music`
            // vs `~/Music/`) silently invalidates the entire cache.
            log(&format!(
                "zytunes: cache: stored root {:?} != requested root {:?} — discarding {} cached entries",
                c.root,
                dir_path,
                c.files.len()
            ));
            HashMap::new()
        }
        Some(c) if c.schema_version != CACHE_SCHEMA_VERSION => {
            // A stale schema means newer `Track` fields would silently sit
            // at `None` for every cached entry until each file's mtime
            // changed. One forced re-scan is the smaller wart.
            log(&format!(
                "zytunes: cache: stored schema {} != current {} — discarding {} cached entries to force re-scan",
                c.schema_version,
                CACHE_SCHEMA_VERSION,
                c.files.len()
            ));
            HashMap::new()
        }
        Some(c) => c.files,
        None => HashMap::new(),
    }
}

/// Save the per-file cache for `dir_path`.
///
/// `log` receives any diagnostic messages (write errors, serialisation failures, etc.).
pub fn save_dirlib_cache(dir_path: &str, files: HashMap<String, CachedFile>, log: &Logger) {
    save_raw(
        dir_path,
        &CachedLibrary {
            root: dir_path.to_string(),
            schema_version: CACHE_SCHEMA_VERSION,
            files,
        },
        log,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Capturing logger for tests — collects all messages into a `Vec<String>`.
    fn capturing_logger() -> (Logger, std::sync::Arc<Mutex<Vec<String>>>) {
        let msgs: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
        let msgs_clone = msgs.clone();
        let log: Logger =
            std::sync::Arc::new(move |msg: &str| msgs_clone.lock().unwrap().push(msg.to_string()));
        (log, msgs)
    }

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
    fn cached_library_with_old_schema_is_discarded() {
        // A cache file from before `CACHE_SCHEMA_VERSION` existed — or from
        // any older schema — must be discarded so the next scan repopulates
        // newly-added `Track` fields instead of leaving them all None.
        //
        // Uses a unique scan-root key so this test's cache file is distinct
        // from any other test's, then writes/reads against the real `HOME`.
        // Mutating `HOME` mid-test would race with parallel tests that read
        // `cache_dir()` (the test runner does not serialize HOME accesses).
        let dir_path = "/tmp/zytunes-cache-schema-test-unique-root";
        let cache_name = dirlib_cache_name(dir_path);
        let Some(path) = cache_path(&cache_name) else {
            return; // HOME not set — skip (CI environments can be funny).
        };

        let stale = CachedLibrary {
            root: dir_path.to_string(),
            schema_version: 1,
            files: HashMap::from([(
                format!("{dir_path}/song.mp3"),
                CachedFile {
                    fingerprint: FileFingerprint {
                        mtime_secs: 0,
                        size: 0,
                    },
                    track: crate::library::Track {
                        id: 1,
                        name: "Stale".into(),
                        artist: "Stale".into(),
                        album: "Stale".into(),
                        ..Default::default()
                    },
                },
            )]),
        };
        let log = default_logger();
        save_raw(dir_path, &stale, &log);

        let loaded = load_dirlib_cache(dir_path, &log);
        assert!(
            loaded.is_empty(),
            "load_dirlib_cache must discard caches with a stale schema_version"
        );

        // Sanity check: a fresh save round-trips with the current schema.
        save_dirlib_cache(dir_path, HashMap::new(), &log);
        let raw = load_raw(dir_path, &log).expect("just wrote a cache");
        assert_eq!(raw.schema_version, CACHE_SCHEMA_VERSION);

        let _ = std::fs::remove_file(&path);
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

    #[test]
    fn load_dirlib_cache_emits_parse_error_via_logger() {
        // Write a corrupted JSON file directly into the cache location, then
        // call `load_dirlib_cache` and assert the logger captured the parse
        // error message rather than writing to stderr.
        let dir_path = "/tmp/zytunes-cache-test-parse-error-seam";

        // Put garbage bytes at the expected cache path.
        let cache_name = dirlib_cache_name(dir_path);
        let Some(path) = cache_path(&cache_name) else {
            return; // HOME not set — skip
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, b"{ this is not valid json !!!").unwrap();

        let (log, captured) = capturing_logger();
        let result = load_dirlib_cache(dir_path, &log);

        // Should fall back to an empty map, not panic.
        assert!(result.is_empty(), "corrupted cache should yield empty map");

        // Logger must have received the parse-error message.
        let msgs = captured.lock().unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("parse") && m.contains("treating as empty")),
            "expected a parse-error log message; got: {msgs:?}"
        );

        // Clean up.
        let _ = std::fs::remove_file(&path);
    }
}
