//! Stable track IDs for the directory library.
//!
//! A track's ID used to be the hash of its path, so every move gave the
//! file a new identity and each store keyed on the ID (playlists, play
//! stats, the listen log) had to be re-keyed by hand. The registry here
//! records `path → ID` once and moves the entry with the file: the ID a
//! file was first seen under is the ID it keeps, and nothing keyed on an
//! ID is ever re-keyed.
//!
//! A new file's ID is still [`hash_path`] of where it was first seen, so
//! every ID written before the registry existed stays valid with no
//! migration. Only a file landing on a path whose hash a moved track still
//! holds gets a salted ID.
//!
//! The registry is its own file under the shared cache root, beside the
//! play stats, and is NOT part of the dirlib cache: a schema bump there
//! discards entries, and a discarded entry must not cost a track its
//! identity.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::cache::Logger;
use crate::dirlib::hash_path;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
struct OnDisk {
    schema_version: u32,
    root: String,
    /// Keyed by absolute file path, like the dirlib cache.
    ids: HashMap<String, u64>,
}

/// `path → ID` for every file of one library root.
#[derive(Debug, Default)]
pub struct TrackIds {
    by_path: HashMap<String, u64>,
    used: HashSet<u64>,
    dirty: bool,
}

impl TrackIds {
    /// The ID registered for `path`, if any.
    pub fn get(&self, path: &Path) -> Option<u64> {
        self.by_path.get(path.to_string_lossy().as_ref()).copied()
    }

    /// The ID of the file at `path`, assigning one on first sight.
    pub fn id_for(&mut self, path: &Path) -> u64 {
        if let Some(id) = self.get(path) {
            return id;
        }
        let id = self.unused_id(path);
        self.insert(path.to_string_lossy().into_owned(), id);
        id
    }

    /// [`hash_path`] of `path` unless a moved track still holds that
    /// value, then the first free salted hash.
    fn unused_id(&self, path: &Path) -> u64 {
        let base = hash_path(path);
        if !self.used.contains(&base) {
            return base;
        }
        let key = path.to_string_lossy();
        (1u64..)
            .map(|salt| {
                let mut hasher = DefaultHasher::new();
                key.hash(&mut hasher);
                salt.hash(&mut hasher);
                hasher.finish()
            })
            .find(|id| !self.used.contains(id))
            .expect("a free u64 exists")
    }

    fn insert(&mut self, path: String, id: u64) {
        self.by_path.insert(path, id);
        self.used.insert(id);
        self.dirty = true;
    }

    /// Move every entry whose path `moved` changes, IDs riding along.
    ///
    /// `moved` maps a path from before an apply to where that file is now
    /// (itself when untouched). All moves are taken from one snapshot, so a
    /// chain (`a → b` while `b → c`) keeps both IDs apart.
    ///
    /// An apply never lands a file on another (it files it beside), so an
    /// entry already at a mover's new path is for a file that is gone. The
    /// mover keeps its own ID and that entry's ID is freed: no two tracks
    /// are ever folded into one, and no store keyed on IDs is ever re-keyed.
    pub fn apply_moves(&mut self, moved: impl Fn(&Path) -> PathBuf) {
        let movers: Vec<(String, String)> = self
            .by_path
            .keys()
            .filter_map(|old| {
                let new = moved(Path::new(old));
                (new != Path::new(old)).then(|| (old.clone(), new.to_string_lossy().into_owned()))
            })
            .collect();
        let lifted: Vec<(String, u64)> = movers
            .into_iter()
            .filter_map(|(old, new)| self.by_path.remove(&old).map(|id| (new, id)))
            .collect();
        for (new, id) in lifted {
            self.dirty = true;
            if let Some(stale) = self.by_path.insert(new, id) {
                self.used.remove(&stale);
            }
        }
    }

    /// Drop entries whose path `keep` rejects (files a full scan no longer
    /// finds), freeing their IDs.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.by_path.len();
        self.by_path.retain(|path, id| {
            let kept = keep(path);
            if !kept {
                self.used.remove(id);
            }
            kept
        });
        if self.by_path.len() != before {
            self.dirty = true;
        }
    }

    /// Missing, unreadable, for another root, or from another schema all
    /// read as empty: IDs then fall back to path hashes, which is what
    /// they were before the registry existed.
    fn load_from(file: &Path, root: &str, log: &Logger) -> Self {
        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log(&format!(
                        "zytunes: track-ids: read {} failed: {e}",
                        file.display()
                    ));
                }
                return Self::default();
            }
        };
        let parsed: OnDisk = match serde_json::from_slice(&data) {
            Ok(p) => p,
            Err(e) => {
                log(&format!(
                    "zytunes: track-ids: parse {} ({} bytes) failed: {e} — starting empty",
                    file.display(),
                    data.len()
                ));
                return Self::default();
            }
        };
        if parsed.schema_version != SCHEMA_VERSION || parsed.root != root {
            log(&format!(
                "zytunes: track-ids: {} is for root {:?} schema {}, not {root:?} schema {SCHEMA_VERSION} — starting empty",
                file.display(),
                parsed.root,
                parsed.schema_version,
            ));
            return Self::default();
        }
        let used = parsed.ids.values().copied().collect();
        Self {
            by_path: parsed.ids,
            used,
            dirty: false,
        }
    }

    fn save_to(&self, file: &Path, root: &str, log: &Logger) {
        let on_disk = OnDisk {
            schema_version: SCHEMA_VERSION,
            root: root.to_string(),
            ids: self.by_path.clone(),
        };
        let result = serde_json::to_vec(&on_disk)
            .map_err(|e| format!("serialize {} entries failed: {e}", self.by_path.len()))
            .and_then(|data| crate::paths::atomic_write_json(file, &data));
        if let Err(e) = result {
            log(&format!("zytunes: track-ids: {e}"));
        }
    }
}

/// Registry filename, keyed by a hash of the library root like the dirlib
/// cache so several roots (and tests) do not share one file.
fn registry_name(root: &str) -> String {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    format!("track-ids-{:016x}.json", hasher.finish())
}

fn registry_path(root: &str) -> Option<PathBuf> {
    crate::paths::zytunes_cache_root().map(|d| d.join(registry_name(root)))
}

/// Serialises every read-modify-write of a registry file.
fn registry_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` against the registry of `root`, saving it afterwards when `f`
/// changed anything.
pub fn with_registry<R>(root: &str, log: &Logger, f: impl FnOnce(&mut TrackIds) -> R) -> R {
    let _guard = registry_lock();
    let file = registry_path(root);
    let mut ids = match &file {
        Some(file) => TrackIds::load_from(file, root, log),
        None => TrackIds::default(),
    };
    let out = f(&mut ids);
    if ids.dirty {
        match &file {
            Some(file) => ids.save_to(file, root, log),
            None => log("zytunes: track-ids: HOME unset, cannot persist track IDs"),
        }
    }
    out
}

/// Record the moves an apply performed, before the library is re-read.
pub fn record_moves(root: &str, log: &Logger, moved: impl Fn(&Path) -> PathBuf) {
    with_registry(root, log, |ids| ids.apply_moves(moved))
}

/// Delete the registry of `root`. Tests reuse temp-dir names across runs,
/// and a registry left by an earlier run would hand out that run's IDs.
#[cfg(test)]
pub(crate) fn forget(root: &str) {
    let _guard = registry_lock();
    if let Some(file) = registry_path(root) {
        let _ = std::fs::remove_file(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::default_logger;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// `moved` closure for a set of exact file renames.
    fn renames(pairs: &[(&str, &str)]) -> impl Fn(&Path) -> PathBuf {
        let map: HashMap<PathBuf, PathBuf> = pairs.iter().map(|(a, b)| (p(a), p(b))).collect();
        move |old| map.get(old).cloned().unwrap_or_else(|| old.to_path_buf())
    }

    #[test]
    fn a_new_file_gets_the_hash_of_its_path() {
        // IDs written before the registry existed are path hashes. A file
        // seen for the first time has to get that same value, or every
        // existing playlist and play count would point at nothing.
        let mut ids = TrackIds::default();
        let path = p("/music/A/B/01.mp3");
        assert_eq!(ids.id_for(&path), hash_path(&path));
        assert_eq!(ids.get(&path), Some(hash_path(&path)));
    }

    #[test]
    fn a_moved_file_keeps_its_id() {
        let mut ids = TrackIds::default();
        let id = ids.id_for(&p("/music/a/b/01.mp3"));
        ids.apply_moves(renames(&[("/music/a/b/01.mp3", "/music/A/B/01.mp3")]));
        assert_eq!(ids.id_for(&p("/music/A/B/01.mp3")), id);
        assert_eq!(ids.get(&p("/music/a/b/01.mp3")), None);
    }

    #[test]
    fn a_new_file_at_a_vacated_path_gets_its_own_id() {
        // The moved track still holds the hash of its first path. A new
        // rip dropped at that path must not share it.
        let mut ids = TrackIds::default();
        let moved_id = ids.id_for(&p("/music/old.mp3"));
        ids.apply_moves(renames(&[("/music/old.mp3", "/music/new.mp3")]));
        let newcomer = ids.id_for(&p("/music/old.mp3"));
        assert_ne!(newcomer, moved_id);
        assert_eq!(
            ids.id_for(&p("/music/old.mp3")),
            newcomer,
            "and it is stable"
        );
    }

    #[test]
    fn a_directory_rename_carries_every_id_under_it() {
        let mut ids = TrackIds::default();
        let one = ids.id_for(&p("/music/alice in chains/Dirt/01.mp3"));
        let two = ids.id_for(&p("/music/alice in chains/Facelift/01.mp3"));
        let other = ids.id_for(&p("/music/Tool/Lateralus/01.mp3"));
        let dirs = vec![(p("/music/alice in chains"), p("/music/Alice In Chains"))];
        ids.apply_moves(|old| crate::tag_ops::remap_through_dir_renames(old, &dirs));
        assert_eq!(ids.get(&p("/music/Alice In Chains/Dirt/01.mp3")), Some(one));
        assert_eq!(
            ids.get(&p("/music/Alice In Chains/Facelift/01.mp3")),
            Some(two)
        );
        assert_eq!(ids.get(&p("/music/Tool/Lateralus/01.mp3")), Some(other));
        assert_eq!(ids.get(&p("/music/alice in chains/Dirt/01.mp3")), None);
    }

    #[test]
    fn a_move_onto_a_registered_path_takes_it_over() {
        // An apply never lands a file on one that is there (it files it
        // beside). So an entry at the new path is for a file that has
        // since been deleted by hand, and the mover keeps its own ID.
        let mut ids = TrackIds::default();
        let mover = ids.id_for(&p("/music/feat/01.mp3"));
        let stale = ids.id_for(&p("/music/Artist/Album/01.mp3"));
        ids.apply_moves(renames(&[(
            "/music/feat/01.mp3",
            "/music/Artist/Album/01.mp3",
        )]));
        assert_eq!(ids.get(&p("/music/Artist/Album/01.mp3")), Some(mover));
        assert_eq!(ids.get(&p("/music/feat/01.mp3")), None);
        // The stale ID is free again.
        assert_eq!(
            ids.id_for(&p("/music/gone.mp3")),
            hash_path(&p("/music/gone.mp3"))
        );
        ids.retain(|path| path != "/music/Artist/Album/01.mp3");
        assert_eq!(ids.id_for(&p("/music/Artist/Album/01.mp3")), stale);
    }

    #[test]
    fn a_rename_chain_keeps_both_ids_apart() {
        // `a → b` while `b → c`: b's track is moving out, so a's arrival
        // must not cost it its ID.
        let mut ids = TrackIds::default();
        let a = ids.id_for(&p("/music/a.mp3"));
        let b = ids.id_for(&p("/music/b.mp3"));
        ids.apply_moves(renames(&[
            ("/music/a.mp3", "/music/b.mp3"),
            ("/music/b.mp3", "/music/c.mp3"),
        ]));
        assert_eq!(ids.get(&p("/music/b.mp3")), Some(a));
        assert_eq!(ids.get(&p("/music/c.mp3")), Some(b));
        assert_eq!(ids.get(&p("/music/a.mp3")), None);
    }

    #[test]
    fn retain_frees_the_ids_of_files_that_are_gone() {
        let mut ids = TrackIds::default();
        ids.id_for(&p("/music/keep.mp3"));
        let gone = ids.id_for(&p("/music/gone.mp3"));
        ids.retain(|path| path != "/music/gone.mp3");
        assert_eq!(ids.get(&p("/music/gone.mp3")), None);
        assert!(ids.get(&p("/music/keep.mp3")).is_some());
        // The file coming back at the same path gets its old ID again.
        assert_eq!(ids.id_for(&p("/music/gone.mp3")), gone);
    }

    #[test]
    fn the_registry_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("zytunes-track-ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("track-ids.json");
        let log = default_logger();

        let mut ids = TrackIds::default();
        let id = ids.id_for(&p("/music/old.mp3"));
        ids.apply_moves(renames(&[("/music/old.mp3", "/music/new.mp3")]));
        let newcomer = ids.id_for(&p("/music/old.mp3"));
        assert!(ids.dirty);
        ids.save_to(&file, "/music", &log);

        let mut loaded = TrackIds::load_from(&file, "/music", &log);
        assert!(!loaded.dirty, "a fresh load has nothing to save");
        assert_eq!(loaded.get(&p("/music/new.mp3")), Some(id));
        assert_eq!(loaded.get(&p("/music/old.mp3")), Some(newcomer));
        // IDs in use are rebuilt on load: a third file cannot take either.
        let third = loaded.id_for(&p("/music/third.mp3"));
        assert!(third != id && third != newcomer);

        // A registry written for another root is not this library's.
        let other = TrackIds::load_from(&file, "/elsewhere", &log);
        assert_eq!(other.get(&p("/music/new.mp3")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_registry_is_not_dirty() {
        let mut ids = TrackIds::default();
        ids.id_for(&p("/music/a.mp3"));
        ids.dirty = false;
        ids.id_for(&p("/music/a.mp3"));
        ids.apply_moves(|old| old.to_path_buf());
        ids.retain(|_| true);
        assert!(
            !ids.dirty,
            "lookups and no-op moves must not force a rewrite"
        );
    }
}
