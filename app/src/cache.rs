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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
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
    /// A Chromaprint attempt returned nothing. Skip further attempts until the
    /// file's `(mtime, size)` changes and this entry is rebuilt.
    #[serde(default)]
    pub fingerprint_failed: bool,
}

/// Bumped whenever the on-disk shape of a cached `Track` changes such that
/// stale cache entries would silently carry `None` for new fields. On
/// mismatch we discard the cache and force a re-scan so the new fields get
/// populated rather than sitting empty until each file is re-tagged.
///
/// Bump history:
///   1 → 2 (2026-04): added extended lofty metadata + audio properties
///   (composer, isrc, mb_*, replaygain_*, sample_rate, channels, …).
///   2 → 3 (2026-05): identity-like string fields now trim leading/trailing
///   whitespace at scan time (legacy taggers space-pad ID3v2 frames). Stale
///   v2 entries hold "311                           " which would still
///   poison MB searches and sidebar grouping until re-tagged — force a
///   re-scan instead.
///   3 → 4: untagged files gained a duration read from the audio stream.
///   A v3 entry with `total_time_ms: None` is dropped so the next scan
///   re-reads it; an entry that already has a duration stays.
const CACHE_SCHEMA_VERSION: u32 = 4;

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
    // worktrees can safely share one entry. `zytunes_cache_root` does not
    // consult `ZYTUNES_CACHE_DIR`, which avoids re-running the lofty pass
    // for large libraries every time a new worktree spins up.
    crate::paths::zytunes_cache_root()
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
            // Schema-version mismatch: prefer an in-memory migration over a
            // full re-scan whenever possible. Re-parsing tens of thousands
            // of audio files takes minutes — orders of magnitude worse than
            // walking the cached Tracks in RAM. The migration path runs
            // only the transformation the new version needs; if it can't
            // express the migration, we fall through to a discard.
            match migrate_cache(c.schema_version, c.files, log) {
                Some(files) => {
                    log(&format!(
                        "zytunes: cache: migrated {} entries from schema {} to {}",
                        files.len(),
                        c.schema_version,
                        CACHE_SCHEMA_VERSION,
                    ));
                    files
                }
                None => {
                    log(&format!(
                        "zytunes: cache: schema {} not migratable to {} — discarding entries to force re-scan",
                        c.schema_version, CACHE_SCHEMA_VERSION,
                    ));
                    HashMap::new()
                }
            }
        }
        Some(c) => c.files,
        None => HashMap::new(),
    }
}

/// Best-effort in-memory upgrade of cached entries between schema versions.
/// Avoids the multi-minute full re-scan that an outright discard would
/// trigger on a tens-of-thousands-of-files library.
///
/// Returns `Some(files)` when every step from `from_version` to
/// `CACHE_SCHEMA_VERSION` could be applied to the existing data; `None`
/// when the gap is too large or the version is unknown (caller discards).
///
/// Migration steps run in order so multi-version jumps compose. Each step
/// must be pure RAM work — no disk reads of the source audio. If a future
/// schema bump genuinely needs to re-read files, return `None` here and
/// fall back to the rescan path.
fn migrate_cache(
    from_version: u32,
    mut files: HashMap<String, CachedFile>,
    log: &Logger,
) -> Option<HashMap<String, CachedFile>> {
    let mut current = from_version;

    // Step 2 → 3: trim whitespace on identity-like string fields. v2
    // entries from legacy taggers hold values like Artist="311           "
    // that poison MB queries and sidebar grouping. Mirror the canonical
    // list in `dirlib::track_from_lofty` — free-form fields (comment,
    // description, lyrics) keep their whitespace.
    if current == 2 {
        fn trim_string(s: &mut String, dirty: &mut bool) {
            let stripped = s.trim();
            if stripped.len() != s.len() {
                *s = stripped.to_string();
                *dirty = true;
            }
        }
        fn trim_opt(o: &mut Option<String>, dirty: &mut bool) {
            if let Some(s) = o.as_mut() {
                trim_string(s, dirty);
                if s.is_empty() {
                    *o = None;
                    *dirty = true;
                }
            }
        }
        let mut trimmed = 0u64;
        for entry in files.values_mut() {
            let t = &mut entry.track;
            let mut dirty = false;
            trim_string(&mut t.name, &mut dirty);
            trim_string(&mut t.artist, &mut dirty);
            trim_string(&mut t.album, &mut dirty);
            for field in [
                &mut t.album_artist,
                &mut t.composer,
                &mut t.conductor,
                &mut t.lyricist,
                &mut t.genre,
                &mut t.initial_key,
                &mut t.mood,
                &mut t.language,
                &mut t.isrc,
                &mut t.barcode,
                &mut t.catalog_number,
                &mut t.publisher,
                &mut t.copyright,
                &mut t.encoder,
                &mut t.encoder_settings,
                &mut t.original_artist,
                &mut t.original_album,
                &mut t.original_release_date,
                &mut t.mb_track_id,
                &mut t.mb_recording_id,
                &mut t.mb_release_id,
                &mut t.mb_release_group_id,
                &mut t.mb_artist_id,
                &mut t.mb_release_artist_id,
                &mut t.mb_work_id,
                &mut t.replaygain_track_gain,
                &mut t.replaygain_track_peak,
                &mut t.replaygain_album_gain,
                &mut t.replaygain_album_peak,
            ] {
                trim_opt(field, &mut dirty);
            }
            if dirty {
                trimmed += 1;
            }
        }
        log(&format!(
            "zytunes: cache: v2→v3 trimmed whitespace on {trimmed} entries (no file reads needed)"
        ));
        current = 3;
    }

    // Step 3 → 4: drop entries that never recorded a duration. Filling
    // one in means reading the audio file, which this function does not
    // do. A missing entry is re-read on the next scan; one that already
    // has a duration is left alone.
    if current == 3 {
        let before = files.len();
        files.retain(|_, entry| entry.track.total_time_ms.is_some());
        log(&format!(
            "zytunes: cache: v3→v4 dropped {} entries with no duration so they are re-read",
            before - files.len()
        ));
        current = 4;
    }

    if current == CACHE_SCHEMA_VERSION {
        Some(files)
    } else {
        None
    }
}

/// Serialises writers of one dirlib cache. The file replace itself is atomic;
/// the lock keeps a scan snapshot from landing between a fingerprint
/// backfill's read and its write.
fn dirlib_write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bumped once per successful acoustic-id commit. A scan that started
/// earlier can tell that a backfill wrote ids its in-memory snapshot lacks.
static ACOUSTIC_EPOCH: AtomicU64 = AtomicU64::new(0);

pub(crate) fn acoustic_cache_epoch() -> u64 {
    ACOUSTIC_EPOCH.load(Ordering::Acquire)
}

/// Copy acoustic ids and remembered fingerprint failures from the on-disk
/// cache onto `files` entries that still share that file's `(mtime, size)`.
fn merge_disk_acoustic_ids(dir_path: &str, files: &mut HashMap<String, CachedFile>, log: &Logger) {
    let Some(existing) = load_raw(dir_path, log) else {
        return;
    };
    if existing.root != dir_path || existing.schema_version != CACHE_SCHEMA_VERSION {
        return;
    }
    for (path, entry) in files.iter_mut() {
        if entry.track.acoustic_id.is_some() {
            continue;
        }
        let Some(prev) = existing.files.get(path) else {
            continue;
        };
        if prev.fingerprint == entry.fingerprint {
            if let Some(id) = prev.track.acoustic_id.clone() {
                entry.track.acoustic_id = Some(id);
                entry.fingerprint_failed = false;
            } else if prev.fingerprint_failed {
                entry.fingerprint_failed = true;
            }
        }
    }
}

/// Save the per-file cache for `dir_path`.
///
/// `log` receives any diagnostic messages (write errors, serialisation failures, etc.).
pub fn save_dirlib_cache(dir_path: &str, files: HashMap<String, CachedFile>, log: &Logger) {
    let _guard = dirlib_write_lock();
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

/// Checkpoint a scan snapshot. When `epoch_at_start` is behind
/// [`acoustic_cache_epoch`], acoustic ids committed since the scan began
/// are copied onto matching entries before the snapshot replaces the file.
pub(crate) fn save_dirlib_cache_preserving_acoustic_ids(
    dir_path: &str,
    files: HashMap<String, CachedFile>,
    epoch_at_start: u64,
    log: &Logger,
) {
    let _ = finish_scan_cache(dir_path, files, epoch_at_start, true, log);
}

/// Merge any acoustic ids committed during a deferred scan, optionally
/// persist the snapshot, and return the map the caller builds its library from.
pub(crate) fn finish_scan_cache(
    dir_path: &str,
    mut files: HashMap<String, CachedFile>,
    epoch_at_start: u64,
    save: bool,
    log: &Logger,
) -> HashMap<String, CachedFile> {
    let _guard = dirlib_write_lock();
    if acoustic_cache_epoch() != epoch_at_start {
        merge_disk_acoustic_ids(dir_path, &mut files, log);
    }
    if save {
        let cached = CachedLibrary {
            root: dir_path.to_string(),
            schema_version: CACHE_SCHEMA_VERSION,
            files,
        };
        save_raw(dir_path, &cached, log);
        cached.files
    } else {
        files
    }
}

/// Write one chunk of fingerprint results in a single cache rewrite.
///
/// Hits and misses share one read/modify/write so a large library cache is
/// parsed and replaced once per chunk. `still_current` runs while the write
/// lock is held. A library reload that cancelled this backfill gets `false`
/// and the commit is dropped, so a scan snapshot cannot be overwritten by a
/// job it already superseded.
///
/// A stored id clears `fingerprint_failed`. A miss sets that flag and is
/// skipped on later launches until the file's `(mtime, size)` changes.
///
/// Returns the `(path, track id, acoustic id)` triples that landed. The path
/// is the cache key, so the caller can tell which inputs were stored.
pub(crate) fn commit_fingerprint_progress<F>(
    dir_path: &str,
    log: &Logger,
    hits: &[(String, FileFingerprint, String)],
    misses: &[(String, FileFingerprint)],
    still_current: F,
) -> Vec<(String, u64, String)>
where
    F: Fn() -> bool,
{
    let _guard = dirlib_write_lock();
    if !still_current() || (hits.is_empty() && misses.is_empty()) {
        return Vec::new();
    }
    let mut files = load_dirlib_cache(dir_path, log);
    let mut updates = Vec::new();
    let mut changed = false;
    for (path, fp, acoustic_id) in hits {
        let Some(entry) = files.get_mut(path) else {
            continue;
        };
        if entry.fingerprint != *fp || entry.track.acoustic_id.is_some() {
            continue;
        }
        entry.track.acoustic_id = Some(acoustic_id.clone());
        entry.fingerprint_failed = false;
        changed = true;
        updates.push((path.clone(), entry.track.id, acoustic_id.clone()));
    }
    for (path, fp) in misses {
        let Some(entry) = files.get_mut(path) else {
            continue;
        };
        if entry.fingerprint != *fp || entry.track.acoustic_id.is_some() || entry.fingerprint_failed
        {
            continue;
        }
        entry.fingerprint_failed = true;
        changed = true;
    }
    if !changed {
        return updates;
    }
    let cached = CachedLibrary {
        root: dir_path.to_string(),
        schema_version: CACHE_SCHEMA_VERSION,
        files,
    };
    save_raw(dir_path, &cached, log);
    ACOUSTIC_EPOCH.fetch_add(1, Ordering::Release);
    updates
}

/// Delete the dirlib cache of `dir_path`. For tests that scan many
/// throwaway roots and would otherwise leave a cache file for each.
#[cfg(test)]
pub(crate) fn forget_dirlib_cache(dir_path: &str) {
    let _guard = dirlib_write_lock();
    if let Some(path) = cache_path(&dirlib_cache_name(dir_path)) {
        let _ = std::fs::remove_file(path);
    }
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
    fn cached_library_v2_to_v3_migrates_in_memory_without_rescan() {
        // Regression: bumping the schema used to nuke the cache and force a
        // full lofty re-scan of every file, which takes minutes on a real
        // library. v2 → v3 only changed how identity strings are trimmed,
        // so the migration runs entirely in RAM — verify the migrated
        // cache survives the load and the padded values come out trimmed.
        let dir_path = "/tmp/zytunes-cache-v2-to-v3-test";
        let cache_name = dirlib_cache_name(dir_path);
        let Some(_) = cache_path(&cache_name) else {
            return; // HOME not set — skip.
        };

        let v2 = CachedLibrary {
            root: dir_path.to_string(),
            schema_version: 2,
            files: HashMap::from([(
                format!("{dir_path}/padded.m4a"),
                CachedFile {
                    fingerprint: FileFingerprint {
                        mtime_secs: 0,
                        size: 0,
                    },
                    fingerprint_failed: false,
                    track: crate::library::Track {
                        id: 1,
                        name: "Title  ".into(),
                        artist: "311                           ".into(),
                        album: "  Album Name  ".into(),
                        album_artist: Some("  311  ".into()),
                        isrc: Some("   USRC11111111   ".into()),
                        comment: Some("  intentional padding  ".into()),
                        total_time_ms: Some(180_000),
                        ..Default::default()
                    },
                },
            )]),
        };
        let log = default_logger();
        save_raw(dir_path, &v2, &log);

        let loaded = load_dirlib_cache(dir_path, &log);
        assert_eq!(
            loaded.len(),
            1,
            "v2→v3 must migrate in place, not discard the cache"
        );
        let track = &loaded
            .get(&format!("{dir_path}/padded.m4a"))
            .expect("migrated entry should be present")
            .track;
        assert_eq!(track.name, "Title");
        assert_eq!(track.artist, "311");
        assert_eq!(track.album, "Album Name");
        assert_eq!(track.album_artist.as_deref(), Some("311"));
        assert_eq!(track.isrc.as_deref(), Some("USRC11111111"));
        // Free-form fields keep their whitespace (mirrors track_from_lofty).
        assert_eq!(
            track.comment.as_deref(),
            Some("  intentional padding  "),
            "comment is free-form and must NOT be trimmed",
        );
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
                    fingerprint_failed: false,
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
    fn a_v3_cache_drops_entries_that_never_recorded_a_duration() {
        // Untagged files cached before the scan read their duration keep
        // total_time_ms: None, and filing then skips AcoustID. Those
        // entries have to leave the cache so the next scan re-reads them.
        // An entry that already has a duration stays.
        let dir_path = "/tmp/zytunes-cache-v3-duration-test";
        let cache_name = dirlib_cache_name(dir_path);
        let Some(path) = cache_path(&cache_name) else {
            return;
        };
        let file = |name: &str, duration: Option<u64>| {
            (
                format!("{dir_path}/{name}"),
                CachedFile {
                    fingerprint: FileFingerprint {
                        mtime_secs: 1,
                        size: 1,
                    },
                    fingerprint_failed: false,
                    track: crate::library::Track {
                        id: 1,
                        name: name.into(),
                        artist: "A".into(),
                        album: "B".into(),
                        total_time_ms: duration,
                        ..Default::default()
                    },
                },
            )
        };
        let stale = CachedLibrary {
            root: dir_path.to_string(),
            schema_version: 3,
            files: HashMap::from([file("untagged.wav", None), file("tagged.mp3", Some(1_000))]),
        };
        let log = default_logger();
        save_raw(dir_path, &stale, &log);
        let loaded = load_dirlib_cache(dir_path, &log);
        assert!(
            !loaded.contains_key(&format!("{dir_path}/untagged.wav")),
            "a cached track with no duration must be re-read"
        );
        assert!(
            loaded.contains_key(&format!("{dir_path}/tagged.mp3")),
            "a track that already has a duration stays"
        );
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

    #[test]
    fn commit_fingerprint_progress_writes_hits_and_misses_together() {
        // One chunk used to rewrite the cache twice, and a stored id left
        // `fingerprint_failed` set. Both outcomes have to land in one write,
        // and a success has to clear a previous failure.
        let dir_path = "/tmp/zytunes-cache-fp-progress";
        let cache_name = dirlib_cache_name(dir_path);
        let Some(path) = cache_path(&cache_name) else {
            return;
        };
        let fp = FileFingerprint {
            mtime_secs: 1,
            size: 10,
        };
        let hit_path = format!("{dir_path}/ok.wav");
        let miss_path = format!("{dir_path}/bad.mp3");
        let track = |id, name: &str| crate::library::Track {
            id,
            name: name.into(),
            artist: "A".into(),
            album: "B".into(),
            ..Default::default()
        };
        let mut files = HashMap::new();
        files.insert(
            hit_path.clone(),
            CachedFile {
                fingerprint: fp,
                fingerprint_failed: true,
                track: track(7, "Ok"),
            },
        );
        files.insert(
            miss_path.clone(),
            CachedFile {
                fingerprint: fp,
                fingerprint_failed: false,
                track: track(8, "Bad"),
            },
        );
        let log = default_logger();
        save_dirlib_cache(dir_path, files, &log);
        let epoch = acoustic_cache_epoch();
        let updates = commit_fingerprint_progress(
            dir_path,
            &log,
            &[(hit_path.clone(), fp, "fp-ok".into())],
            &[(miss_path.clone(), fp)],
            || true,
        );
        assert_eq!(updates, vec![(hit_path.clone(), 7, "fp-ok".into())]);
        assert!(acoustic_cache_epoch() > epoch);

        let cached = load_dirlib_cache(dir_path, &log);
        let hit = cached.get(&hit_path).expect("hit entry");
        assert_eq!(hit.track.acoustic_id.as_deref(), Some("fp-ok"));
        assert!(!hit.fingerprint_failed);
        let miss = cached.get(&miss_path).expect("miss entry");
        assert!(miss.track.acoustic_id.is_none());
        assert!(miss.fingerprint_failed);

        let _ = std::fs::remove_file(&path);
    }
}
