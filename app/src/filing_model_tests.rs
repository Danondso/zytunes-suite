//! Model test for filing: random sequences of applies against a real
//! folder tree, with the invariants a review would otherwise have to
//! re-derive checked after every step.
//!
//! Example tests pin one scenario each. This one generates the scenarios:
//! files bound for names that are free, held, numbered, in another case,
//! held by a track of the same diff that is leaving or staying, two bound
//! for one name, files arriving from the inbox, and folders that have been
//! made read-only so renames fail halfway through an album.
//!
//! What must hold whatever the sequence:
//!   - no file is lost, duplicated or truncated;
//!   - no temp artefact of a move or tag write is left behind;
//!   - the library the apply's reread produces lists exactly the files on
//!     disk, and equals what a full scan produces;
//!   - a file keeps the track ID it was first given, however often and
//!     however it moves, and no two files share one;
//!   - `rename_map` says where each file really went;
//!   - no folder is left empty.
//!
//! A file's identity here is its inode: a rename keeps it, and the diffs
//! only rename (a tag write swaps in a rewritten copy).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::dirlib::{DirectoryLibrary, ScanOptions};
use crate::library::MusicLibrary;
use crate::tag_ops::{apply_and_record_moves, FieldDiff, FieldKind, ReleaseTagDiff, TrackTagDiff};
use crate::test_audio::write_sine_wav;

const ARTISTS: &[&str] = &["alice in chains", "Alice In Chains", "Tool"];
const ALBUMS: &[&str] = &["dirt", "Dirt", "Lateralus"];
const NAMES: &[&str] = &[
    "01 - Them Bones.wav",
    "01 - Them Bones (2).wav",
    "01 them bones.wav",
    "02 - Dam That River.wav",
    "03 - Rain When I Die.wav",
];
/// Lengths in seconds. Two files within 2 s of each other count as copies
/// of one recording, so both "a copy" and "a different song" come up.
const LENGTHS: &[u32] = &[1, 1, 5];
const ARTEFACTS: &[&str] = &[".zytunes-case-", ".tagtmp", ".zytunes-part"];

/// xorshift64*: deterministic per seed, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
}

fn random_path(rng: &mut Rng, root: &Path) -> PathBuf {
    root.join(rng.pick(ARTISTS))
        .join(rng.pick(ALBUMS))
        .join(rng.pick(NAMES))
}

/// Every file and every folder under `dir`, sorted so a seed replays.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            dirs.push(path.clone());
            walk(&path, files, dirs);
        } else {
            files.push(path);
        }
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    walk(dir, &mut files, &mut dirs);
    files
}

fn inode(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .ino()
}

fn rename_row(src: &Path, dest: Option<&Path>) -> TrackTagDiff {
    TrackTagDiff {
        src_path: src.to_path_buf(),
        dest_path: dest.map(Path::to_path_buf),
        library_id: 0,
        fields: dest
            .map(|dest| FieldDiff {
                kind: FieldKind::Filename,
                name: "Filename",
                current: Some(src.display().to_string()),
                proposed: Some(dest.display().to_string()),
                enabled: true,
            })
            .into_iter()
            .collect(),
    }
}

/// A full scan, without fingerprinting (it decodes every file).
fn scan(root: &str) -> DirectoryLibrary {
    let options = ScanOptions {
        fingerprint: false,
        log: std::sync::Arc::new(|_: &str| {}),
    };
    DirectoryLibrary::scan_with_options(root, options, |_| {}).unwrap()
}

/// How often the generated applies reached each case worth reaching, so a
/// change to the vocabulary cannot quietly stop the model testing them.
#[derive(Default, Debug)]
struct Reached {
    moved: usize,
    filed_from_inbox: usize,
    landed_beside: usize,
    refused_as_wrong_pairing: usize,
    folders_retitled: usize,
    failed_on_a_locked_folder: usize,
}

fn library_view(lib: &DirectoryLibrary) -> BTreeMap<String, u64> {
    lib.all_tracks()
        .filter_map(|t| Some((t.location.clone()?, t.id)))
        .collect()
}

struct Model {
    root: PathBuf,
    inbox: PathBuf,
    /// Every file the test made: inode → its length.
    files: HashMap<u64, u64>,
    /// The track ID each file was given the first time the library saw it.
    ids: HashMap<u64, u64>,
    /// A folder was made read-only at some point, so an emptied folder
    /// under it could not be removed.
    faulted: bool,
}

impl Model {
    fn create(&mut self, path: &Path, secs: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(path, secs);
        let meta = std::fs::metadata(path).unwrap();
        self.files.insert(meta.ino(), meta.len());
    }

    /// Check everything that must hold between applies. `lib` is what the
    /// apply's own reread produced.
    fn check(&mut self, lib: &DirectoryLibrary, at: &str) {
        let (mut in_root, mut dirs) = (Vec::new(), Vec::new());
        walk(&self.root, &mut in_root, &mut dirs);
        let in_inbox = files_under(&self.inbox);

        for path in in_root.iter().chain(&in_inbox).chain(&dirs) {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                !ARTEFACTS.iter().any(|a| name.contains(a)),
                "{at}: temp artefact left behind: {}",
                path.display()
            );
        }

        // Nothing lost, duplicated, truncated, or made up.
        let mut seen: HashMap<u64, &PathBuf> = HashMap::new();
        for path in in_root.iter().chain(&in_inbox) {
            let meta = std::fs::metadata(path).unwrap();
            let Some(len) = self.files.get(&meta.ino()) else {
                panic!("{at}: a file the test never made: {}", path.display());
            };
            assert_eq!(meta.len(), *len, "{at}: {} changed size", path.display());
            if let Some(other) = seen.insert(meta.ino(), path) {
                panic!(
                    "{at}: one file under two names: {} and {}",
                    other.display(),
                    path.display()
                );
            }
        }
        assert_eq!(seen.len(), self.files.len(), "{at}: a file is gone");

        // The library lists exactly what is on disk under the root.
        let listed = library_view(lib);
        let on_disk: BTreeSet<String> = in_root
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            listed.keys().cloned().collect::<BTreeSet<_>>(),
            on_disk,
            "{at}: the library and the disk disagree"
        );

        // Each file has the ID it was first given, and no two share one.
        let mut owners: HashMap<u64, &String> = HashMap::new();
        for (location, id) in &listed {
            if let Some(other) = owners.insert(*id, location) {
                panic!("{at}: {other} and {location} share ID {id}");
            }
            let first = *self.ids.entry(inode(Path::new(location))).or_insert(*id);
            assert_eq!(*id, first, "{at}: {location} changed its track ID");
        }

        // A full scan agrees with the reread.
        let scanned = scan(self.root.to_str().unwrap());
        assert_eq!(
            library_view(&scanned),
            listed,
            "{at}: a full scan sees a different library"
        );

        if !self.faulted {
            for dir in &dirs {
                let empty = std::fs::read_dir(dir).unwrap().next().is_none();
                assert!(!empty, "{at}: empty folder left: {}", dir.display());
            }
        }
    }
}

fn run(seed: u64, reached: &mut Reached) {
    let mut rng = Rng::new(seed);
    let base = std::env::temp_dir()
        .join("zytunes-filing-model")
        .join(seed.to_string());
    // A folder an earlier, failed run left read-only would block the wipe.
    let (mut stale_files, mut stale_dirs) = (Vec::new(), Vec::new());
    walk(&base, &mut stale_files, &mut stale_dirs);
    for dir in stale_dirs {
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("Music");
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.to_str().unwrap().to_string();
    let inbox = crate::library_layout::default_inbox_dir(&root).unwrap();
    std::fs::create_dir_all(&inbox).unwrap();
    crate::track_ids::forget(&root_str);
    let log: crate::cache::Logger = std::sync::Arc::new(|_: &str| {});

    let mut model = Model {
        root: root.clone(),
        inbox: inbox.clone(),
        files: HashMap::new(),
        ids: HashMap::new(),
        faulted: false,
    };
    for _ in 0..(4 + rng.below(6)) {
        let path = random_path(&mut rng, &root);
        if !path.exists() {
            model.create(&path, *rng.pick(LENGTHS));
        }
    }
    let lib = scan(&root_str);
    model.check(&lib, &format!("seed {seed} setup"));

    for step in 0..8 {
        let at = format!("seed {seed} step {step}");
        let mut sources = files_under(&root);
        if rng.one_in(3) {
            let drop = inbox.join(format!("drop {step}.wav"));
            model.create(&drop, *rng.pick(LENGTHS));
            sources.push(drop);
        }
        // A random handful of files, each bound for a random name. One in
        // four is listed without a rename: a track of the album that is
        // staying where it is.
        let mut diff = ReleaseTagDiff {
            release_mbid: "rel".into(),
            summary: at.clone(),
            tracks: Vec::new(),
        };
        let wanted = 1 + rng.below(5);
        while diff.tracks.len() < wanted && !sources.is_empty() {
            let src = sources.swap_remove(rng.below(sources.len()));
            let dest = random_path(&mut rng, &root);
            let stays = rng.one_in(4) && src.starts_with(&root);
            diff.tracks
                .push(rename_row(&src, (!stays && dest != src).then_some(&*dest)));
        }
        let before: HashMap<PathBuf, u64> = diff
            .tracks
            .iter()
            .map(|t| (t.src_path.clone(), inode(&t.src_path)))
            .collect();

        // One apply in three runs with a folder read-only, so renames in
        // it fail while the rest of the album goes through.
        let mut locked: Option<PathBuf> = None;
        if rng.one_in(3) {
            let (mut files, mut dirs) = (Vec::new(), Vec::new());
            walk(&root, &mut files, &mut dirs);
            if !dirs.is_empty() {
                let dir = dirs.swap_remove(rng.below(dirs.len()));
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
                locked = Some(dir);
                model.faulted = true;
            }
        }

        let outcome = apply_and_record_moves(&diff, &root_str, &log);

        // The folder may have been carried along by a retitle of its
        // parent, or removed: an empty one is pruned when a move into it
        // fails, which only needs its parent to be writable.
        if let Some(dir) = locked {
            let now = crate::tag_ops::remap_through_dir_renames(&dir, &outcome.dir_renames);
            if now.exists() {
                std::fs::set_permissions(&now, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let detail = format!("{at}\n  diff: {:#?}\n  outcome: {outcome:#?}", diff.tracks);
        assert_eq!(outcome.results.len(), diff.tracks.len(), "{detail}");
        reached.moved += outcome.rename_map.len();
        reached.folders_retitled += outcome.dir_renames.len();
        for (track, result) in diff.tracks.iter().zip(&outcome.results) {
            let landed = outcome.rename_map.get(&track.src_path);
            if landed.is_some() && track.src_path.starts_with(&inbox) {
                reached.filed_from_inbox += 1;
            }
            if landed.is_some() && landed != track.dest_path.as_ref() {
                reached.landed_beside += 1;
            }
            match result {
                Err(e) if e.contains("Permission denied") => {
                    reached.failed_on_a_locked_folder += 1;
                }
                Err(e) if e.contains("collision") => reached.refused_as_wrong_pairing += 1,
                _ => {}
            }
        }
        for (src, landed) in &outcome.rename_map {
            assert!(
                landed.exists(),
                "{detail}\n  nothing at {}",
                landed.display()
            );
            assert_eq!(
                inode(landed),
                before[src],
                "{detail}\n  rename_map names another file for {}",
                src.display()
            );
        }
        let lib = DirectoryLibrary::reread_paths_after(
            &root_str,
            &outcome.reread_paths(&diff, &root),
            &outcome.vacated(),
            &outcome.dir_renames,
            false,
            &log,
        )
        .unwrap();
        model.check(&lib, &detail);
    }
    crate::track_ids::forget(&root_str);
    crate::cache::forget_dirlib_cache(&root_str);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn filing_keeps_every_file_and_its_identity_through_random_applies() {
    let mut reached = Reached::default();
    for seed in 0..150 {
        run(seed, &mut reached);
    }
    assert!(
        reached.moved > 0
            && reached.filed_from_inbox > 0
            && reached.landed_beside > 0
            && reached.refused_as_wrong_pairing > 0
            && reached.folders_retitled > 0,
        "the generator no longer reaches every case: {reached:?}"
    );
    // Root ignores directory permissions, so nothing fails on a read-only
    // folder there.
    let is_root = std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0);
    assert!(
        is_root || reached.failed_on_a_locked_folder > 0,
        "no apply failed on a read-only folder: {reached:?}"
    );
    eprintln!("filing model reached: {reached:?}");
}
