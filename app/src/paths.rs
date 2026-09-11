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

/// Root of the shared (non-device-scoped) cache tree.
///
/// Precedence: `cache_dir` in `~/.config/zytunes/config.toml` (non-empty)
/// → `$HOME/.cache/zytunes`. Deliberately does **not** consult
/// `ZYTUNES_CACHE_DIR` — that env var is worktree isolation for
/// device-scoped caches only. Everything under this root (dirlib scans,
/// album art, stems, model checkpoints, play history) is derived from
/// source data that sibling worktrees share. Returns `None` when neither
/// an override nor `HOME` is available.
pub fn zytunes_cache_root() -> Option<PathBuf> {
    resolve_user_cache_root_from(
        config_string_field("cache_dir").as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn resolve_user_cache_root_from(config_value: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = config_value.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(Path::new(home?).join(".cache").join("zytunes"))
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

/// Default MTPZ handshake file: `$HOME/.mtpz-data`.
pub fn default_mtpz_data_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| Path::new(&home).join(".mtpz-data"))
}

/// Effective MTPZ handshake-file path.
///
/// Precedence: `ZYTUNES_MTPZ_DATA` (non-empty) → `mtpz_data` in
/// `~/.config/zytunes/config.toml` (non-empty) → `~/.mtpz-data`.
/// Does not require the file to exist — [`zune_mtp::MtpzKeys::load`]
/// reports a missing file with the resolved path in the error.
pub fn resolve_mtpz_data_path() -> Result<PathBuf, String> {
    resolve_mtpz_data_path_from(
        std::env::var("ZYTUNES_MTPZ_DATA").ok().as_deref(),
        config_string_field("mtpz_data").as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn resolve_mtpz_data_path_from(
    env_override: Option<&str>,
    config_value: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(trimmed) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(trimmed));
    }
    if let Some(trimmed) = config_value.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(trimmed));
    }
    match home.map(str::trim).filter(|s| !s.is_empty()) {
        Some(home) => Ok(Path::new(home).join(".mtpz-data")),
        None => Err(
            "HOME is unset; set ZYTUNES_MTPZ_DATA or mtpz_data in ~/.config/zytunes/config.toml"
                .into(),
        ),
    }
}

/// User-facing line when the handshake file is missing. `None` when a
/// readable file is at the resolved path. Shared by the TUI log and the
/// Zune session-open error so the wording stays one place.
pub fn mtpz_file_missing_message() -> Option<String> {
    match resolve_mtpz_data_path() {
        Ok(path) if path.is_file() => None,
        Ok(path) => Some(mtpz_file_missing_message_for(&path)),
        Err(_) => Some("MTPZ file not found. Zune music management is disabled.".into()),
    }
}

fn mtpz_file_missing_message_for(path: &Path) -> String {
    format!(
        "MTPZ file not found at {}. Zune music management is disabled.",
        path.display()
    )
}

fn config_string_field(field: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home)
        .join(".config")
        .join("zytunes")
        .join("config.toml");
    let contents = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = contents.parse().ok()?;
    table
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
    fn user_cache_root_honors_config_override() {
        assert_eq!(
            resolve_user_cache_root_from(Some("/mnt/big/zytunes-cache"), Some("/home/user")),
            Some(PathBuf::from("/mnt/big/zytunes-cache"))
        );
        assert_eq!(
            resolve_user_cache_root_from(Some("  "), Some("/home/user")),
            Some(PathBuf::from("/home/user/.cache/zytunes"))
        );
        assert_eq!(
            resolve_user_cache_root_from(None, Some("/home/user")),
            Some(PathBuf::from("/home/user/.cache/zytunes"))
        );
    }

    #[test]
    fn device_cache_base_falls_back_to_home() {
        let _g = lock();
        std::env::remove_var("ZYTUNES_CACHE_DIR");
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        assert_eq!(device_cache_base(), home);
    }

    #[test]
    fn mtpz_data_env_wins_over_config_and_home() {
        assert_eq!(
            resolve_mtpz_data_path_from(
                Some("/tmp/from-env"),
                Some("/tmp/from-config"),
                Some("/home/user"),
            )
            .unwrap(),
            PathBuf::from("/tmp/from-env")
        );
    }

    #[test]
    fn mtpz_data_config_wins_over_home() {
        assert_eq!(
            resolve_mtpz_data_path_from(None, Some("/opt/keys/.mtpz-data"), Some("/home/user"))
                .unwrap(),
            PathBuf::from("/opt/keys/.mtpz-data")
        );
    }

    #[test]
    fn mtpz_data_blank_env_and_config_fall_through_to_home() {
        assert_eq!(
            resolve_mtpz_data_path_from(Some("  "), Some(""), Some("/home/user")).unwrap(),
            PathBuf::from("/home/user/.mtpz-data")
        );
    }

    #[test]
    fn mtpz_data_errors_when_nothing_resolves() {
        let err = resolve_mtpz_data_path_from(None, None, None).unwrap_err();
        assert!(err.contains("mtpz_data"), "{err}");
    }

    #[test]
    fn mtpz_missing_message_names_the_path() {
        let msg = mtpz_file_missing_message_for(Path::new("/tmp/nope.mtpz-data"));
        assert!(msg.contains("/tmp/nope.mtpz-data"), "{msg}");
        assert!(msg.contains("Zune music management is disabled"), "{msg}");
    }

    #[test]
    fn mtpz_missing_message_is_none_when_file_exists() {
        let _g = lock();
        let tmp = std::env::temp_dir().join(format!(
            "zytunes-mtpz-present-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&tmp, "placeholder").unwrap();
        std::env::set_var("ZYTUNES_MTPZ_DATA", &tmp);
        assert!(
            mtpz_file_missing_message().is_none(),
            "present file should not warn"
        );
        std::env::remove_var("ZYTUNES_MTPZ_DATA");
        let _ = std::fs::remove_file(&tmp);
    }
}
