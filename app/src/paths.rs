//! Resolve runtime cache locations with per-worktree isolation support.
//!
//! zytunes' device caches (track list, library handles, sync progress)
//! normally live under `$HOME` and are keyed only by device serial — not
//! by worktree — so branch-specific schema changes can clobber each
//! other when running multiple git worktrees (e.g., Conductor).
//!
//! `ZYTUNES_CACHE_DIR`, when set, redirects those device caches into one
//! directory. A workspace-level `.cargo/config.toml` points it at
//! `./.zytunes-cache` for any `cargo run`/`cargo test` from the tree,
//! giving each worktree its own device cache without requiring users to
//! manage env vars.
//!
//! The `dirlib` file-metadata cache deliberately does **not** honor this
//! override: it's keyed by a hash of the scan-root path, so worktrees
//! pointing at the same `~/Music` can reuse one cached scan and avoid
//! re-running the lofty pass on every new worktree. See `src/cache.rs`.

use std::path::{Path, PathBuf};

/// Returns `ZYTUNES_CACHE_DIR` (created if missing) when set to a non-empty
/// value, else `None`. Used as a shared override check by the two default
/// resolvers below.
pub fn override_cache_dir() -> Option<PathBuf> {
    let v = std::env::var("ZYTUNES_CACHE_DIR").ok()?;
    if v.is_empty() {
        return None;
    }
    let path = PathBuf::from(v);
    let _ = std::fs::create_dir_all(&path);
    Some(path)
}

/// Root of the shared (non-device-scoped) cache tree:
/// `$HOME/.cache/zytunes`. Deliberately does **not** consult
/// `ZYTUNES_CACHE_DIR` — everything under this root (dirlib scans, album
/// art, stems, model checkpoints, play history) is derived from source
/// data that sibling worktrees share, so isolating it per-worktree would
/// only multiply expensive regeneration. Device-scoped caches go through
/// [`device_cache_base`] instead. Returns `None` when `HOME` is unset.
pub fn zytunes_cache_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache").join("zytunes"))
}

/// Base directory for device-scoped cache dotfiles (track cache, library
/// cache, sync progress). Honors `ZYTUNES_CACHE_DIR`; falls back to `$HOME`
/// so existing installations keep reading `~/.zytunes-*-cache-{serial}`.
pub fn device_cache_base() -> Option<PathBuf> {
    if let Some(dir) = override_cache_dir() {
        return Some(dir);
    }
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Atomically write pre-serialized JSON to `path`.
///
/// Creates parent directories, stages the bytes to `path` with a
/// `.json.tmp` extension, fsyncs, then renames over the target — a crash
/// mid-write leaves the previous file intact rather than truncated JSON
/// the loader can't parse. The fsync-before-rename matters: without
/// `sync_all` a crash between the rename returning and the kernel
/// flushing the tmp's page cache can leave a renamed-but-empty file.
///
/// Returns a human-readable message naming the failed step; callers add
/// their own subsystem prefix when logging.
pub fn atomic_write_json(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {} failed: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(data)?;
            f.sync_all()
        })
        .map_err(|e| format!("write {} ({} bytes) failed: {e}", tmp.display(), data.len()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {} failed: {e}", tmp.display(), path.display())
    })?;
    // Fsync the parent directory so the rename itself survives power
    // loss — the file *data* was synced above, but on POSIX the new
    // directory entry isn't durable until the directory is. Best-effort:
    // the write already succeeded, and opening a directory read-only can
    // fail on exotic filesystems.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var` mutates process-global state. Tests that touch
    // `ZYTUNES_CACHE_DIR` must serialize to avoid stomping on each other
    // when cargo runs them across threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn override_returns_set_path_and_creates_it() {
        let _g = lock();
        let tmp = std::env::temp_dir().join(format!(
            "zytunes-paths-override-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("ZYTUNES_CACHE_DIR", &tmp);

        let got = override_cache_dir().expect("env set to non-empty");
        assert_eq!(got, tmp);
        assert!(tmp.is_dir(), "should create the directory if missing");

        std::env::remove_var("ZYTUNES_CACHE_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn override_treats_empty_as_unset() {
        let _g = lock();
        std::env::set_var("ZYTUNES_CACHE_DIR", "");
        assert!(override_cache_dir().is_none());
        std::env::remove_var("ZYTUNES_CACHE_DIR");
    }

    #[test]
    fn override_unset_returns_none() {
        let _g = lock();
        std::env::remove_var("ZYTUNES_CACHE_DIR");
        assert!(override_cache_dir().is_none());
    }

    #[test]
    fn device_cache_base_prefers_override_over_home() {
        let _g = lock();
        let tmp = std::env::temp_dir().join(format!(
            "zytunes-paths-device-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("ZYTUNES_CACHE_DIR", &tmp);

        assert_eq!(device_cache_base(), Some(tmp.clone()));

        std::env::remove_var("ZYTUNES_CACHE_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn device_cache_base_falls_back_to_home() {
        let _g = lock();
        std::env::remove_var("ZYTUNES_CACHE_DIR");
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        assert_eq!(device_cache_base(), home);
    }
}
