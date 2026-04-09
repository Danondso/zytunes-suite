use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Number of F-directories on iPod (F00..F49).
const NUM_F_DIRS: u32 = 50;

/// Get the iPod_Control/Music directory path.
pub fn music_dir(mount: &Path) -> PathBuf {
    mount.join("iPod_Control").join("Music")
}

/// Ensure all F00..F49 directories exist under iPod_Control/Music/.
pub fn ensure_f_dirs(mount: &Path) -> crate::Result<()> {
    let base = music_dir(mount);
    for i in 0..NUM_F_DIRS {
        let dir = base.join(format!("F{i:02}"));
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// Generate a hashed filename for a track file.
///
/// iPod stores files with obfuscated names like `ABCD.mp3` in F-directories.
/// We hash the original name + dbid to generate a deterministic but opaque name.
pub fn hash_filename(original_name: &str, dbid: u64) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    dbid.hash(&mut hasher);
    original_name.hash(&mut hasher);
    let hash = hasher.finish();

    let ext = Path::new(original_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp3");

    format!("{hash:016x}.{ext}")
}

/// Pick the F-directory with the fewest files for load balancing.
pub fn pick_f_dir(mount: &Path) -> crate::Result<u32> {
    let base = music_dir(mount);
    let mut min_count = u64::MAX;
    let mut best = 0u32;

    for i in 0..NUM_F_DIRS {
        let dir = base.join(format!("F{i:02}"));
        let count = match std::fs::read_dir(&dir) {
            Ok(entries) => entries.count() as u64,
            Err(_) => 0,
        };
        if count < min_count {
            min_count = count;
            best = i;
        }
    }

    Ok(best)
}

/// Build the iPod-style colon-separated path for a file.
///
/// e.g. `:iPod_Control:Music:F03:abcdef0123456789.mp3`
pub fn ipod_path(f_dir: u32, filename: &str) -> String {
    format!(":iPod_Control:Music:F{f_dir:02}:{filename}")
}

/// Convert an iPod colon-path to a real filesystem path relative to the mount.
pub fn real_path(mount: &Path, colon_path: &str) -> PathBuf {
    let stripped = colon_path.strip_prefix(':').unwrap_or(colon_path);
    let parts: Vec<&str> = stripped.split(':').collect();
    let mut path = mount.to_path_buf();
    for part in parts {
        path = path.join(part);
    }
    path
}

/// Audio file extensions recognized on iPod.
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "alac", "wav"];

/// Enumerate all audio files currently on the iPod under iPod_Control/Music/.
pub fn enumerate_audio_files(mount: &Path) -> crate::Result<Vec<PathBuf>> {
    let base = music_dir(mount);
    let mut files = Vec::new();
    let valid_ext: HashSet<&str> = AUDIO_EXTENSIONS.iter().copied().collect();

    for i in 0..NUM_F_DIRS {
        let dir = base.join(format!("F{i:02}"));
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if valid_ext.contains(ext.to_lowercase().as_str()) {
                        files.push(path);
                    }
                }
            }
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ensure_f_dirs() {
        let tmp = TempDir::new().unwrap();
        ensure_f_dirs(tmp.path()).unwrap();

        for i in 0..50 {
            assert!(music_dir(tmp.path()).join(format!("F{i:02}")).is_dir());
        }
    }

    #[test]
    fn test_hash_filename() {
        let name = hash_filename("song.mp3", 42);
        assert!(name.ends_with(".mp3"));
        assert_eq!(name.len(), 16 + 1 + 3); // 16 hex + dot + ext

        // Deterministic.
        assert_eq!(name, hash_filename("song.mp3", 42));

        // Different dbid => different name.
        assert_ne!(name, hash_filename("song.mp3", 43));
    }

    #[test]
    fn test_ipod_path_and_real_path() {
        let colon = ipod_path(3, "abcdef.mp3");
        assert_eq!(colon, ":iPod_Control:Music:F03:abcdef.mp3");

        let real = real_path(Path::new("/mnt/IPOD"), &colon);
        assert_eq!(
            real,
            PathBuf::from("/mnt/IPOD/iPod_Control/Music/F03/abcdef.mp3")
        );
    }

    #[test]
    fn test_pick_f_dir_empty() {
        let tmp = TempDir::new().unwrap();
        ensure_f_dirs(tmp.path()).unwrap();
        // All empty, should return 0.
        let dir = pick_f_dir(tmp.path()).unwrap();
        assert_eq!(dir, 0);
    }

    #[test]
    fn test_pick_f_dir_balancing() {
        let tmp = TempDir::new().unwrap();
        ensure_f_dirs(tmp.path()).unwrap();

        // Put files in F00.
        for i in 0..5 {
            std::fs::write(
                music_dir(tmp.path())
                    .join("F00")
                    .join(format!("file{i}.mp3")),
                b"data",
            )
            .unwrap();
        }

        let dir = pick_f_dir(tmp.path()).unwrap();
        // Should pick something other than F00.
        assert_ne!(dir, 0);
    }

    #[test]
    fn test_enumerate_audio_files() {
        let tmp = TempDir::new().unwrap();
        ensure_f_dirs(tmp.path()).unwrap();

        let f00 = music_dir(tmp.path()).join("F00");
        std::fs::write(f00.join("song.mp3"), b"data").unwrap();
        std::fs::write(f00.join("track.m4a"), b"data").unwrap();
        std::fs::write(f00.join("readme.txt"), b"data").unwrap();

        let files = enumerate_audio_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 2);
    }
}
