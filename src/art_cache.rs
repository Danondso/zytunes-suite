//! Persistent cache for album art extracted from audio files.
//!
//! Art extraction via lofty is expensive (especially across a cold library),
//! so we cache the decoded JPEG bytes on disk keyed by (artist, album). Cache
//! entries are invalidated when the source file's `(mtime, size)` changes, so
//! re-tagging or replacing a track refreshes the cached art automatically.
//!
//! Layout under `root/`:
//!   - `{hash}.jpg`       — the cached JPEG payload
//!   - `{hash}.meta.json` — fingerprint of the source file the art came from

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct ArtMeta {
    source_path: String,
    source_mtime_secs: u64,
    source_size: u64,
}

/// On-disk album-art cache rooted at `dir`.
pub struct ArtCache {
    dir: PathBuf,
}

impl ArtCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Default cache location under `$HOME/.cache/zytunes/art`.
    ///
    /// Shares the same root as the dirlib metadata cache so worktrees pointed
    /// at the same `~/Music` reuse one cache. Returns `None` when `HOME` is
    /// unset (the tests rely on the explicit-root constructor instead).
    pub fn default_location() -> Option<Self> {
        let home = std::env::var("HOME").ok()?;
        Some(Self::new(
            Path::new(&home).join(".cache").join("zytunes").join("art"),
        ))
    }

    fn key(artist: &str, album: &str) -> String {
        let mut hasher = DefaultHasher::new();
        artist.hash(&mut hasher);
        0x1Fu8.hash(&mut hasher); // unit separator so "Ab/Cd" vs "A/b/Cd" don't collide
        album.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn jpg_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.jpg"))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.meta.json"))
    }

    /// Look up cached art for `(artist, album)`. Returns `None` if absent, if
    /// the meta sidecar is unreadable, or if the source file's fingerprint no
    /// longer matches what was cached.
    pub fn lookup(&self, artist: &str, album: &str) -> Option<Vec<u8>> {
        let key = Self::key(artist, album);
        let meta_bytes = std::fs::read(self.meta_path(&key)).ok()?;
        let meta: ArtMeta = serde_json::from_slice(&meta_bytes).ok()?;

        let fp = fingerprint(Path::new(&meta.source_path))?;
        if fp != (meta.source_mtime_secs, meta.source_size) {
            return None;
        }

        std::fs::read(self.jpg_path(&key)).ok()
    }

    /// Store `jpeg` for `(artist, album)`, fingerprinted from `source`. The
    /// cache directory is created on demand. Writes are best-effort: IO
    /// failures bubble up so callers can log them, but the cache is a
    /// performance layer, not a correctness boundary.
    pub fn store(&self, artist: &str, album: &str, source: &Path, jpeg: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;

        let (mtime_secs, size) = fingerprint(source)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "source file fingerprint"))?;
        let meta = ArtMeta {
            source_path: source.to_string_lossy().into_owned(),
            source_mtime_secs: mtime_secs,
            source_size: size,
        };

        let key = Self::key(artist, album);

        // Write the JPEG first so a crash mid-store leaves no meta pointing
        // at a missing payload. Worst case: a successful jpg write with no
        // meta, which `lookup` treats as a miss and overwrites on next store.
        std::fs::write(self.jpg_path(&key), jpeg)?;
        let meta_bytes = serde_json::to_vec(&meta).map_err(io::Error::other)?;
        std::fs::write(self.meta_path(&key), meta_bytes)?;
        Ok(())
    }
}

fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "zytunes-art-cache-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_source(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn fake_jpeg(tag: u8) -> Vec<u8> {
        // Minimal JPEG SOI + EOI with a tagging byte so tests can distinguish
        // cached payloads.
        vec![0xFF, 0xD8, tag, 0xFF, 0xD9]
    }

    #[test]
    fn lookup_empty_cache_returns_none() {
        let dir = scratch_dir("empty");
        let cache = ArtCache::new(dir.join("cache"));
        assert!(cache.lookup("Artist", "Album").is_none());
    }

    #[test]
    fn roundtrip_store_and_lookup() {
        let dir = scratch_dir("roundtrip");
        let src = write_source(&dir, "track.mp3", b"fake audio bytes");
        let cache = ArtCache::new(dir.join("cache"));
        let jpeg = fake_jpeg(0xAB);

        cache
            .store("Radiohead", "OK Computer", &src, &jpeg)
            .unwrap();
        let got = cache.lookup("Radiohead", "OK Computer").expect("hit");
        assert_eq!(got, jpeg);
    }

    #[test]
    fn lookup_misses_after_source_fingerprint_changes() {
        let dir = scratch_dir("fingerprint");
        let src = write_source(&dir, "track.mp3", b"v1");
        let cache = ArtCache::new(dir.join("cache"));
        cache.store("A", "B", &src, &fake_jpeg(1)).unwrap();
        assert!(cache.lookup("A", "B").is_some());

        // Rewrite with a different length so `size` differs — avoids the 1 s
        // mtime granularity that bites some filesystems.
        std::fs::write(&src, b"v2-longer-content").unwrap();

        assert!(
            cache.lookup("A", "B").is_none(),
            "lookup must invalidate once source fingerprint changes"
        );
    }

    #[test]
    fn lookup_misses_when_source_file_disappears() {
        let dir = scratch_dir("gone");
        let src = write_source(&dir, "track.mp3", b"bytes");
        let cache = ArtCache::new(dir.join("cache"));
        cache.store("A", "B", &src, &fake_jpeg(2)).unwrap();
        std::fs::remove_file(&src).unwrap();

        assert!(cache.lookup("A", "B").is_none());
    }

    #[test]
    fn different_albums_do_not_collide() {
        let dir = scratch_dir("nocollide");
        let src = write_source(&dir, "track.mp3", b"x");
        let cache = ArtCache::new(dir.join("cache"));
        cache
            .store("Artist", "Album One", &src, &fake_jpeg(0x01))
            .unwrap();
        cache
            .store("Artist", "Album Two", &src, &fake_jpeg(0x02))
            .unwrap();

        assert_eq!(
            cache.lookup("Artist", "Album One").unwrap(),
            fake_jpeg(0x01)
        );
        assert_eq!(
            cache.lookup("Artist", "Album Two").unwrap(),
            fake_jpeg(0x02)
        );
    }

    #[test]
    fn corrupt_meta_returns_none_not_panic() {
        let dir = scratch_dir("corrupt");
        let src = write_source(&dir, "track.mp3", b"x");
        let cache = ArtCache::new(dir.join("cache"));
        cache.store("A", "B", &src, &fake_jpeg(9)).unwrap();

        let key = ArtCache::key("A", "B");
        std::fs::write(cache.meta_path(&key), b"not json").unwrap();
        assert!(cache.lookup("A", "B").is_none());
    }

    #[test]
    fn key_disambiguates_delimiter_collisions() {
        // Guard against the "Ab / Cd" vs "A / bCd" flattening bug the unit
        // separator byte in the hasher is meant to prevent.
        let k1 = ArtCache::key("Ab", "Cd");
        let k2 = ArtCache::key("A", "bCd");
        assert_ne!(k1, k2);
    }
}
