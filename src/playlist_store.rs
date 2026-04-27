//! On-disk persistence for [`Playlist`]s.
//!
//! Playlists are user-authored state, not a regenerable cache, so the file
//! lives next to `config.toml` under `~/.config/zytunes/`. On a schema-version
//! mismatch the existing file is *renamed* to `playlists.json.v{N}.bak`
//! rather than discarded — losing a hand-curated playlist silently would be
//! unacceptable. This is the deliberate divergence from `LocalPlays` and the
//! dirlib cache, both of which are regenerable and discard on mismatch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::playlist::{now_unix_ms, Playlist};

/// Bumped whenever the on-disk shape changes such that older entries would
/// silently sit at default values for new fields. On mismatch the file is
/// preserved as a `.bak` and an empty in-memory store is used.
const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    playlists: Vec<Playlist>,
}

/// In-memory store. `App` owns one of these and persists changes via
/// `save_to` after each mutation.
#[derive(Clone, Debug, Default)]
pub struct PlaylistStore {
    playlists: Vec<Playlist>,
}

/// Why a [`PlaylistStore::rename`] call failed. Distinguishing the two cases
/// lets the TUI surface a useful toast instead of "rename failed."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameError {
    /// The new name was empty after trimming whitespace.
    EmptyName,
    /// The target ID is not in the store.
    NotFound,
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameError::EmptyName => f.write_str("playlist name cannot be empty"),
            RenameError::NotFound => f.write_str("playlist not found"),
        }
    }
}

impl PlaylistStore {
    pub fn new() -> Self {
        PlaylistStore::default()
    }

    pub fn len(&self) -> usize {
        self.playlists.len()
    }

    pub fn is_empty(&self) -> bool {
        self.playlists.is_empty()
    }

    /// Borrow the underlying list. Sorted by the caller (the TUI sidebar
    /// applies its own ordering for display) — the store keeps insertion
    /// order, which is also chronological because IDs include `created_at_ms`
    /// and we never re-sort in place.
    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    pub fn get(&self, id: u64) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Playlist> {
        self.playlists.iter_mut().find(|p| p.id == id)
    }

    /// Insert a freshly-built playlist. If the ID collides with an existing
    /// entry (likely when two `new_manual` calls land in the same ms), bump
    /// `created_at_ms` by one until unique. This keeps every ID stable for
    /// the life of the playlist without requiring callers to handle the
    /// collision themselves.
    pub fn add(&mut self, mut playlist: Playlist) -> u64 {
        while self.playlists.iter().any(|p| p.id == playlist.id) {
            playlist.created_at_ms = playlist.created_at_ms.saturating_add(1);
            playlist.id = derive_id(playlist.created_at_ms, &playlist.name);
        }
        let id = playlist.id;
        self.playlists.push(playlist);
        id
    }

    /// Remove a playlist by ID. Returns `true` if found.
    pub fn remove(&mut self, id: u64) -> bool {
        match self.playlists.iter().position(|p| p.id == id) {
            Some(pos) => {
                self.playlists.remove(pos);
                true
            }
            None => false,
        }
    }

    /// Rename a playlist in place. Empty names are rejected. Returns
    /// `Err` describing the failure mode; the playlist's `id` does NOT change
    /// on rename — that would break sidecars and queue references.
    pub fn rename(&mut self, id: u64, new_name: impl Into<String>) -> Result<(), RenameError> {
        let new_name = new_name.into();
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(RenameError::EmptyName);
        }
        // Persist the trimmed form so a user typing "  Roadtrip  " doesn't
        // end up with whitespace baked into the sidebar label or the
        // playlist filename pushed to the device.
        let trimmed = trimmed.to_string();
        match self.get_mut(id) {
            Some(p) => {
                p.name = trimmed;
                p.updated_at_ms = now_unix_ms();
                Ok(())
            }
            None => Err(RenameError::NotFound),
        }
    }

    /// Add a track to a playlist by ID. Returns `true` if added.
    pub fn add_track(&mut self, playlist_id: u64, track_id: u64) -> bool {
        match self.get_mut(playlist_id) {
            Some(p) => p.add_track(track_id),
            None => false,
        }
    }

    /// Remove a track from a playlist. Returns `true` if removed.
    pub fn remove_track(&mut self, playlist_id: u64, track_id: u64) -> bool {
        match self.get_mut(playlist_id) {
            Some(p) => p.remove_track(track_id),
            None => false,
        }
    }

    /// Reorder a track within a playlist. Returns `true` on success.
    pub fn move_track(&mut self, playlist_id: u64, from: usize, to: usize) -> bool {
        match self.get_mut(playlist_id) {
            Some(p) => p.move_track(from, to),
            None => false,
        }
    }

    /// Read from the well-known path. Missing or unparseable files yield
    /// an empty store; on parse failure or schema mismatch the existing file
    /// is preserved (renamed) so it can be inspected/recovered manually.
    pub fn load() -> Self {
        match default_save_path() {
            Some(p) => Self::load_from(&p),
            None => PlaylistStore::default(),
        }
    }

    /// Read from an arbitrary path — the test seam.
    pub fn load_from(path: &Path) -> Self {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "zytunes: playlist-store: read {} failed: {e}",
                        path.display()
                    );
                }
                return PlaylistStore::default();
            }
        };
        let parsed: OnDisk = match serde_json::from_slice(&data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "zytunes: playlist-store: parse {} ({} bytes) failed: {e} — preserving as .corrupt.bak",
                    path.display(),
                    data.len()
                );
                back_up(path, "corrupt");
                return PlaylistStore::default();
            }
        };
        if parsed.schema_version != SCHEMA_VERSION {
            eprintln!(
                "zytunes: playlist-store: stored schema {} != current {} — preserving {} entries as .v{}.bak",
                parsed.schema_version,
                SCHEMA_VERSION,
                parsed.playlists.len(),
                parsed.schema_version,
            );
            back_up(path, &format!("v{}", parsed.schema_version));
            return PlaylistStore::default();
        }
        PlaylistStore {
            playlists: parsed.playlists,
        }
    }

    /// Persist to the well-known path.
    pub fn save(&self) {
        if let Some(p) = default_save_path() {
            self.save_to(&p);
        }
    }

    /// Atomic write to an arbitrary path — staged via `.tmp` + rename so a
    /// crash mid-write leaves the previous file intact.
    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "zytunes: playlist-store: mkdir {} failed: {e}",
                    parent.display()
                );
                return;
            }
        }
        let on_disk = OnDisk {
            schema_version: SCHEMA_VERSION,
            playlists: self.playlists.clone(),
        };
        let data = match serde_json::to_vec_pretty(&on_disk) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "zytunes: playlist-store: serialize {} entries failed: {e}",
                    self.playlists.len()
                );
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        // Write + fsync the tmp file *before* the rename. Without sync_all
        // a crash between rename returning and the kernel flushing the
        // tmp's page cache leaves a renamed-but-empty playlists.json. Cheap
        // on the small JSON the store produces; matches the durability
        // shape libgpod uses on the iPod side.
        let write_result = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(&data)?;
                f.sync_all()?;
                Ok(())
            });
        if let Err(e) = write_result {
            eprintln!(
                "zytunes: playlist-store: write {} ({} bytes) failed: {e}",
                tmp.display(),
                data.len()
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!(
                "zytunes: playlist-store: rename {} -> {} failed: {e}",
                tmp.display(),
                path.display()
            );
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Default location of the playlist file: `~/.config/zytunes/playlists.json`.
/// Returns `None` when `HOME` is unset.
pub fn default_save_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        Path::new(&home)
            .join(".config")
            .join("zytunes")
            .join("playlists.json"),
    )
}

/// Rename `path` to `path.{tag}.bak`, finding a unique suffix if a previous
/// backup already exists. Best-effort: log on failure but don't propagate.
fn back_up(path: &Path, tag: &str) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "playlists.json".to_string());
    let mut n = 0u32;
    loop {
        let suffix = if n == 0 {
            format!("{stem}.{tag}.bak")
        } else {
            format!("{stem}.{tag}.{n}.bak")
        };
        let candidate = parent.join(&suffix);
        if !candidate.exists() {
            if let Err(e) = std::fs::rename(path, &candidate) {
                eprintln!(
                    "zytunes: playlist-store: backup {} -> {} failed: {e}",
                    path.display(),
                    candidate.display()
                );
            } else {
                eprintln!(
                    "zytunes: playlist-store: backed up {} -> {}",
                    path.display(),
                    candidate.display()
                );
            }
            return;
        }
        n += 1;
        if n > 100 {
            eprintln!(
                "zytunes: playlist-store: gave up finding a unique backup name for {}",
                path.display()
            );
            return;
        }
    }
}

/// Re-derive a playlist ID by delegating to `playlist::hash_playlist_id`.
/// Used by the collision-resolution path when a generated ID collides
/// with an existing one. Single-source-of-truth for the hash so any
/// future change applies to both the create and recover paths.
fn derive_id(created_at_ms: u64, name: &str) -> u64 {
    crate::playlist::hash_playlist_id(created_at_ms, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::{GenerationParams, Playlist};

    fn temp_path(stem: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zytunes-playlist-store-test-{}-{}.json",
            std::process::id(),
            stem
        ))
    }

    #[test]
    fn add_and_get() {
        let mut s = PlaylistStore::new();
        let p = Playlist::new_manual("Faves");
        let id = s.add(p);
        assert_eq!(s.len(), 1);
        let got = s.get(id).unwrap();
        assert_eq!(got.name, "Faves");
    }

    #[test]
    fn add_resolves_id_collision() {
        // Same name + same created_at_ms collides — store bumps until unique.
        let mut s = PlaylistStore::new();
        let a = Playlist::new_manual("Same");
        let mut b = Playlist::new_manual("Same");
        // Force an exact ID collision by stamping b with a's created_at_ms.
        b.created_at_ms = a.created_at_ms;
        b.id = a.id;
        let id_a = s.add(a);
        let id_b = s.add(b);
        assert_ne!(id_a, id_b, "store must resolve collisions");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn remove_returns_false_for_missing() {
        let mut s = PlaylistStore::new();
        assert!(!s.remove(999));
        let id = s.add(Playlist::new_manual("Faves"));
        assert!(s.remove(id));
        assert!(s.is_empty());
    }

    #[test]
    fn rename_preserves_id_and_rejects_empty() {
        let mut s = PlaylistStore::new();
        let id = s.add(Playlist::new_manual("Old"));
        assert!(s.rename(id, "New").is_ok());
        assert_eq!(s.get(id).unwrap().name, "New");
        assert_eq!(s.rename(id, ""), Err(RenameError::EmptyName));
        assert_eq!(
            s.rename(id, "   "),
            Err(RenameError::EmptyName),
            "whitespace-only must be rejected"
        );
        assert_eq!(s.rename(999, "X"), Err(RenameError::NotFound));
    }

    #[test]
    fn rename_trims_surrounding_whitespace() {
        // Validation gates on the trimmed form, so the persisted name
        // must be trimmed too — otherwise "  Roadtrip  " survives the
        // empty-check and silently keeps its padding in the sidebar /
        // device filename.
        let mut s = PlaylistStore::new();
        let id = s.add(Playlist::new_manual("Old"));
        assert!(s.rename(id, "  Roadtrip  ").is_ok());
        assert_eq!(s.get(id).unwrap().name, "Roadtrip");
        assert!(s.rename(id, "\tTabbed\n").is_ok());
        assert_eq!(s.get(id).unwrap().name, "Tabbed");
    }

    #[test]
    fn add_track_through_store() {
        let mut s = PlaylistStore::new();
        let id = s.add(Playlist::new_manual("Faves"));
        assert!(s.add_track(id, 1));
        assert!(s.add_track(id, 2));
        assert!(!s.add_track(id, 1), "duplicate must be rejected");
        assert_eq!(s.get(id).unwrap().track_ids, vec![1, 2]);
    }

    #[test]
    fn save_load_round_trip_manual() {
        let path = temp_path("round-trip-manual");
        let mut s = PlaylistStore::new();
        let id = s.add(Playlist::new_manual("Faves"));
        s.add_track(id, 100);
        s.add_track(id, 200);
        s.save_to(&path);

        let loaded = PlaylistStore::load_from(&path);
        assert_eq!(loaded.len(), 1);
        let got = loaded.get(id).unwrap();
        assert_eq!(got.name, "Faves");
        assert_eq!(got.track_ids, vec![100, 200]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_load_round_trip_generated() {
        let path = temp_path("round-trip-generated");
        let mut s = PlaylistStore::new();
        let params = GenerationParams::default_discover_weekly();
        let id = s.add(Playlist::new_generated("DW", params.clone(), vec![1, 2, 3]));
        s.save_to(&path);

        let loaded = PlaylistStore::load_from(&path);
        let got = loaded.get(id).unwrap();
        assert!(got.is_generated());
        assert_eq!(got.track_ids, vec![1, 2, 3]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_uses_atomic_rename() {
        let path = temp_path("atomic");
        let s = PlaylistStore::new();
        s.save_to(&path);
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "stale .tmp left behind after save");
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let s = PlaylistStore::load_from(&path);
        assert!(s.is_empty());
    }

    #[test]
    fn stale_schema_is_backed_up_not_discarded() {
        let path = temp_path("stale-schema");
        // Clear any old backups from previous test runs.
        let _ = std::fs::remove_file(&path);
        let parent = path.parent().unwrap();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with(&format!(
                    "{}.v0",
                    path.file_name().unwrap().to_string_lossy()
                )) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        // Hand-craft a v0 file (pre-versioning).
        let body = r#"{"schema_version":0,"playlists":[]}"#;
        std::fs::write(&path, body).unwrap();

        let s = PlaylistStore::load_from(&path);
        assert!(s.is_empty(), "stale-schema file must yield empty store");
        assert!(!path.exists(), "original file must be renamed away");

        // The backup must exist somewhere alongside.
        let backup_present = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                s.starts_with(&format!(
                    "{}.v0",
                    path.file_name().unwrap().to_string_lossy()
                ))
            });
        assert!(backup_present, "stale-schema file must be backed up");

        // Cleanup the backup files.
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with(&format!(
                    "{}.v0",
                    path.file_name().unwrap().to_string_lossy()
                )) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"not json {{").unwrap();

        let s = PlaylistStore::load_from(&path);
        assert!(s.is_empty());
        assert!(!path.exists(), "corrupt file must be renamed away");

        // Cleanup any .corrupt.bak files.
        let parent = path.parent().unwrap();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with(&format!(
                    "{}.corrupt",
                    path.file_name().unwrap().to_string_lossy()
                )) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}
