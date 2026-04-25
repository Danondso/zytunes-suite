//! Directory-scanning music library backend.
//!
//! Recursively scans a folder for audio files and reads metadata from ID3
//! tags (MP3) or infers it from the directory structure (other formats).

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;

/// How many files the scan processes between cache snapshot decisions.
const SAVE_CHUNK_SIZE: usize = 2_000;

/// Minimum number of *changed* entries since the last save to trigger an
/// in-progress snapshot. A pure reload (every file is a cache hit with
/// matching `acoustic_id`) accumulates zero changes and never saves —
/// previously we re-serialised the entire ~36 MB cache after every chunk
/// even when nothing had changed, which dominated reload time.
const SAVE_DIRTY_THRESHOLD: u64 = 500;

/// Knobs for `scan_with_options`.
#[derive(Clone)]
pub struct ScanOptions {
    /// Compute acoustic fingerprints for files that lack one. Disabling
    /// makes the scan much faster but skips the foundation for cross-device
    /// playcount merging (Phase 2+ of the playcount work).
    pub fingerprint: bool,

    /// Receives diagnostic log messages (cache miss/hit summaries, save
    /// errors, etc.).  Defaults to writing to stderr; the TUI background
    /// worker replaces this with a channel-routing closure so messages appear
    /// in the sync log panel instead of corrupting the ratatui frame buffer.
    pub log: crate::cache::Logger,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            fingerprint: true,
            log: crate::cache::default_logger(),
        }
    }
}

// Manual Debug impl because function pointers aren't Debug.
impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

use crate::library::{MusicLibrary, Track};

/// Audio file extensions recognized by the scanner.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wma", "wav", "aiff", "alac",
];

/// A single metadata sample emitted during a scan, for progress UI.
#[derive(Clone, Debug)]
pub struct TrackSample {
    pub artist: String,
    pub album: String,
    pub name: String,
}

/// Progress update emitted from `scan_with_progress`.
#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub completed: u64,
    pub total: u64,
    pub sample: Option<TrackSample>,
}

/// A music library built by scanning a directory tree.
pub struct DirectoryLibrary {
    tracks: HashMap<u64, Track>,
    root: String,
}

impl DirectoryLibrary {
    /// Recursively scan a directory for audio files and build a library.
    ///
    /// Uses a disk cache (`~/.cache/zytunes/`) to skip metadata reads when no
    /// files have been added or modified since the last run. When the cache is
    /// stale, metadata reads are parallelized with rayon.
    pub fn scan(path: &str) -> Result<Self, String> {
        Self::scan_with_options(path, ScanOptions::default(), |_| {})
    }

    /// Same as `scan`, but invokes `on_progress` for each track built.
    ///
    /// The callback is called from rayon worker threads (so it must be `Sync`)
    /// and is invoked once per track, plus once at the start with
    /// `completed = 0` and `sample = None` to report the total.
    pub fn scan_with_progress<F>(path: &str, on_progress: F) -> Result<Self, String>
    where
        F: Fn(ScanProgress) + Sync,
    {
        Self::scan_with_options(path, ScanOptions::default(), on_progress)
    }

    /// Full scan API: caller supplies options (e.g. to disable fingerprinting)
    /// alongside a progress callback. The simpler `scan` and
    /// `scan_with_progress` entry points wrap this with `ScanOptions::default()`.
    pub fn scan_with_options<F>(
        path: &str,
        options: ScanOptions,
        on_progress: F,
    ) -> Result<Self, String>
    where
        F: Fn(ScanProgress) + Sync,
    {
        let root = Path::new(path);
        if !root.is_dir() {
            return Err(format!("Not a directory: {path}"));
        }

        let scan_start = Instant::now();

        // Load the previous per-file cache. Missing / unreadable = empty map,
        // so first launches just fall through to a full parse.
        let cached = crate::cache::load_dirlib_cache(path, &options.log);

        // Parallel directory walk.
        let paths = collect_audio_paths(root);
        let total = paths.len() as u64;
        on_progress(ScanProgress {
            completed: 0,
            total,
            sample: None,
        });

        // For each file: stat → fingerprint → reuse cached Track if the
        // (mtime,size) fingerprint matches, otherwise run lofty to parse tags
        // + duration. Stats are cheap; lofty is the expensive step, so the
        // cache makes repeat launches nearly instant even with
        // `read_properties` on.
        //
        // Acoustic fingerprint is computed if (a) the file is fresh, or (b)
        // the cached track has `acoustic_id == None`. Case (b) covers two
        // distinct situations:
        //   - cache predates the field (post-upgrade backfill), and
        //   - earlier scan tried to fingerprint and got `None` (corrupt /
        //     unsupported codec / too-short clip).
        //
        // The retry on case-2 is INTENTIONAL: not caching negative results
        // means a file that becomes fingerprintable later (codec support
        // improves, broken file is repaired or re-downloaded) automatically
        // picks up its fingerprint without a cache wipe. The cost is one
        // extra decode per launch per pathological file — bounded by the
        // (mtime,size) outer cache to actually-pathological files only,
        // and small relative to the value of self-healing on case 2.
        //
        // Cached tracks that already have an `acoustic_id` are reused as-is.
        let completed = AtomicU64::new(0);
        // Counters for the post-scan summary. Plain atomics — no lock
        // contention because they're only read once at the end.
        let cache_hit_with_fp = AtomicU64::new(0);
        let cache_hit_no_fp = AtomicU64::new(0);
        let cache_miss = AtomicU64::new(0);
        let fp_computed = AtomicU64::new(0);
        let fp_failed = AtomicU64::new(0);

        // Process in chunks of `SAVE_CHUNK_SIZE` and snapshot the cache after
        // each one. The working map starts as a clone of the previous cache,
        // gets per-file entries inserted as chunks complete, and is what we
        // serialise on every snapshot. Files we haven't visited yet survive
        // in the working map (still pointing at the previous-scan entry), so
        // a Ctrl+C mid-scan only loses the current chunk's work — never the
        // previous run's entries for files we just haven't reached yet.
        let extant_paths: HashSet<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let working: Mutex<HashMap<String, crate::cache::CachedFile>> = Mutex::new(cached.clone());

        // Tracks how many entries genuinely changed since the last on-disk
        // save. Increments only when (a) the file wasn't in the cache, or
        // (b) the file was cached but its `acoustic_id` is now different.
        // A pure reload (every file is a cache hit with the same fingerprint)
        // accumulates zero — and skips both per-chunk and final save.
        let pending_changes = AtomicU64::new(0);
        let total_changes = AtomicU64::new(0);

        for chunk in paths.chunks(SAVE_CHUNK_SIZE) {
            let chunk_entries: Vec<(String, crate::cache::CachedFile)> = chunk
                .par_iter()
                .filter_map(|p| {
                    let key = p.to_string_lossy().to_string();
                    let fingerprint = crate::cache::FileFingerprint::from_path(p)?;

                    let (mut track, came_from_cache, prev_acoustic) = match cached.get(&key) {
                        Some(entry) if entry.fingerprint == fingerprint => {
                            (entry.track.clone(), true, entry.track.acoustic_id.clone())
                        }
                        _ => {
                            cache_miss.fetch_add(1, Ordering::Relaxed);
                            (build_track(p, hash_path(p)), false, None)
                        }
                    };

                    if came_from_cache {
                        if track.acoustic_id.is_some() {
                            cache_hit_with_fp.fetch_add(1, Ordering::Relaxed);
                        } else {
                            cache_hit_no_fp.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    if options.fingerprint && track.acoustic_id.is_none() {
                        // Fresh-parse already tried `read_embedded_fingerprint`
                        // inside `track_from_lofty`; if that turned up nothing,
                        // re-trying it would just re-open + re-parse the tag
                        // for a guaranteed second `None`. Skip straight to
                        // compute.
                        //
                        // Cache-hit paths take the full pipeline: a pre-existing
                        // cache from before the field was added has acoustic_id
                        // = None despite the file possibly carrying a tag, so
                        // the embedded read is worth attempting once.
                        track.acoustic_id = if came_from_cache {
                            crate::fingerprint::fingerprint_for(p)
                        } else {
                            crate::fingerprint::compute_fingerprint(p)
                        };
                        if track.acoustic_id.is_some() {
                            fp_computed.fetch_add(1, Ordering::Relaxed);
                        } else {
                            fp_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    if !came_from_cache || track.acoustic_id != prev_acoustic {
                        pending_changes.fetch_add(1, Ordering::Relaxed);
                        total_changes.fetch_add(1, Ordering::Relaxed);
                    }

                    let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    on_progress(ScanProgress {
                        completed: n,
                        total,
                        sample: Some(TrackSample {
                            artist: track.artist.clone(),
                            album: track.album.clone(),
                            name: track.name.clone(),
                        }),
                    });

                    Some((key, crate::cache::CachedFile { fingerprint, track }))
                })
                .collect();

            // Merge chunk results into the working cache. Snapshot only if
            // enough genuine changes have accumulated since the last save —
            // a reload with no new fingerprints accumulates zero and never
            // hits the `>= threshold` branch, making reloads near-instant.
            let snapshot = {
                let mut w = working.lock().expect("working cache lock");
                for (k, v) in chunk_entries {
                    w.insert(k, v);
                }
                if pending_changes.load(Ordering::Relaxed) >= SAVE_DIRTY_THRESHOLD {
                    Some(w.clone())
                } else {
                    None
                }
            };
            if let Some(snap) = snapshot {
                crate::cache::save_dirlib_cache(path, snap, &options.log);
                pending_changes.store(0, Ordering::Relaxed);
            }
        }

        // Final pass: drop entries whose files disappeared since the previous
        // scan. Only at the end — partial saves can't prune, because
        // unprocessed paths aren't yet known to be missing-vs-pending.
        let mut final_cache = working.into_inner().expect("working cache lock");
        let pruned = {
            let before = final_cache.len();
            final_cache.retain(|k, _| extant_paths.contains(k));
            before - final_cache.len()
        };

        let tracks: HashMap<u64, Track> = final_cache
            .values()
            .map(|cf| (cf.track.id, cf.track.clone()))
            .collect();

        // Save only when there's actually new state to persist:
        //   - entries changed since last save (`pending_changes > 0`), or
        //   - files were pruned, or
        //   - in the unusual case where total_changes is 0 but the cache
        //     file doesn't exist yet (caught implicitly via pending_changes
        //     after first chunk if any insertion happened — pure-empty
        //     library handled via `saved_total == 0` skip).
        let pending = pending_changes.load(Ordering::Relaxed);
        let total_dirty = total_changes.load(Ordering::Relaxed);
        let needs_save = pending > 0 || pruned > 0;
        if needs_save {
            crate::cache::save_dirlib_cache(path, final_cache, &options.log);
        }

        // One-line summary only when work happened. Pure reloads stay silent.
        if total_dirty > 0 || pruned > 0 {
            (options.log)(&format!(
                "zytunes: scan: {:.1}s | new/changed: {total_dirty} | pruned: {pruned} | \
                 cache hits: {hit_fp} with-fp + {hit_no_fp} backfilled | misses: {miss} | \
                 fingerprints: {comp} computed, {fail} failed",
                scan_start.elapsed().as_secs_f64(),
                hit_fp = cache_hit_with_fp.load(Ordering::Relaxed),
                hit_no_fp = cache_hit_no_fp.load(Ordering::Relaxed),
                miss = cache_miss.load(Ordering::Relaxed),
                comp = fp_computed.load(Ordering::Relaxed),
                fail = fp_failed.load(Ordering::Relaxed),
            ));
        }

        Ok(DirectoryLibrary {
            tracks,
            root: path.to_string(),
        })
    }
}

/// Recursively collect audio file paths, walking each subdirectory in parallel.
///
/// For large libraries the serial `read_dir` walk alone can take several seconds;
/// rayon's recursive parallelism turns it into a few hundred milliseconds on SSD.
fn collect_audio_paths(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e.flatten().collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };
    entries
        .par_iter()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                collect_audio_paths(&path)
            } else if is_audio_file(&path) {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn hash_path(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    hasher.finish()
}

fn build_track(path: &Path, id: u64) -> Track {
    // Try lofty for all formats (FLAC, M4A, OGG, WAV, MP3, etc.)
    if let Some(track) = track_from_lofty(path, id) {
        return track;
    }

    // Fallback: infer metadata from directory structure.
    track_from_path(path, id)
}

/// Read metadata (tags + duration) from any audio file using lofty.
///
/// Reading properties is slow for MP3 VBR files because lofty has to sample
/// frames across the whole file to compute duration, but the per-file cache
/// makes sure we only pay that cost once per file per change.
fn track_from_lofty(path: &Path, id: u64) -> Option<Track> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::prelude::ItemKey;
    use lofty::tag::Accessor;

    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;

    // Require at least a title or artist to consider the tag useful
    let has_useful_data = tag.title().is_some() || tag.artist().is_some();
    if !has_useful_data {
        return None;
    }

    let name = tag
        .title()
        .map(|s| s.to_string())
        .unwrap_or_else(|| stem(path));
    let artist = tag
        .artist()
        .map(|s| s.to_string())
        .unwrap_or_else(|| parent_name(path, 2));
    let album = tag
        .album()
        .map(|s| s.to_string())
        .unwrap_or_else(|| parent_name(path, 1));

    // Prefer an already-embedded ACOUSTID_FINGERPRINT tag (written by Picard
    // / fpcalc) over computing one — we've already parsed the tagged file,
    // so reading the tag is effectively free; computing means a full decode.
    // The scan loop backfills via `fingerprint_for` when this returns None.
    let acoustic_id = crate::fingerprint::read_embedded_fingerprint(path);

    let s = |key: ItemKey| tag.get_string(&key).map(|v| v.to_string());
    let parsed = |key: ItemKey| s(key).and_then(|v| v.parse().ok());

    let props = tagged.properties();
    let file_size_bytes = std::fs::metadata(path).ok().map(|m| m.len());

    Some(Track {
        id,
        name,
        artist,
        album,
        genre: tag.genre().map(|s| s.to_string()),
        year: tag.year(),
        track_number: tag.track(),
        disc_number: tag.disk(),
        total_time_ms: {
            let dur = props.duration();
            if dur.is_zero() {
                None
            } else {
                Some(dur.as_millis() as u64)
            }
        },
        location: Some(path.to_string_lossy().to_string()),
        kind: Some(format!(
            "{} audio file",
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_uppercase()
        )),
        acoustic_id,

        album_artist: s(ItemKey::AlbumArtist),
        composer: s(ItemKey::Composer),
        conductor: s(ItemKey::Conductor),
        lyricist: s(ItemKey::Lyricist),
        comment: s(ItemKey::Comment),
        description: s(ItemKey::Description),
        // ID3v2 stores BPM as `IntegerBpm` (TBPM frame); VorbisComment/MP4
        // use the decimal `Bpm`. Try the decimal form first, then fall back.
        bpm: s(ItemKey::Bpm)
            .or_else(|| s(ItemKey::IntegerBpm))
            .and_then(|v| v.parse::<f64>().ok().map(|f| f.round() as u32)),
        initial_key: s(ItemKey::InitialKey),
        mood: s(ItemKey::Mood),
        language: s(ItemKey::Language),
        isrc: s(ItemKey::Isrc),
        barcode: s(ItemKey::Barcode),
        catalog_number: s(ItemKey::CatalogNumber),
        // ID3v2 stores TPUB as `Label` internally even though it represents
        // the publisher; VorbisComment uses both keys distinctly. Try the
        // canonical key first, then fall through to the alias.
        publisher: s(ItemKey::Publisher).or_else(|| s(ItemKey::Label)),
        copyright: s(ItemKey::CopyrightMessage),
        encoder: s(ItemKey::EncoderSoftware),
        encoder_settings: s(ItemKey::EncoderSettings),
        original_artist: s(ItemKey::OriginalArtist),
        original_album: s(ItemKey::OriginalAlbumTitle),
        original_release_date: s(ItemKey::OriginalReleaseDate),
        track_total: parsed(ItemKey::TrackTotal),
        disc_total: parsed(ItemKey::DiscTotal),
        lyrics: s(ItemKey::Lyrics),
        rating: s(ItemKey::Popularimeter).and_then(|v| v.parse::<u8>().ok()),

        mb_track_id: s(ItemKey::MusicBrainzTrackId),
        mb_recording_id: s(ItemKey::MusicBrainzRecordingId),
        mb_release_id: s(ItemKey::MusicBrainzReleaseId),
        mb_release_group_id: s(ItemKey::MusicBrainzReleaseGroupId),
        mb_artist_id: s(ItemKey::MusicBrainzArtistId),
        mb_release_artist_id: s(ItemKey::MusicBrainzReleaseArtistId),
        mb_work_id: s(ItemKey::MusicBrainzWorkId),

        replaygain_track_gain: s(ItemKey::ReplayGainTrackGain),
        replaygain_track_peak: s(ItemKey::ReplayGainTrackPeak),
        replaygain_album_gain: s(ItemKey::ReplayGainAlbumGain),
        replaygain_album_peak: s(ItemKey::ReplayGainAlbumPeak),

        sample_rate: props.sample_rate(),
        channels: props.channels(),
        bit_depth: props.bit_depth(),
        audio_bitrate_kbps: props.audio_bitrate(),
        overall_bitrate_kbps: props.overall_bitrate(),
        file_size_bytes,
    })
}

/// Infer metadata from directory structure: Artist/Album/Track.ext
fn track_from_path(path: &Path, id: u64) -> Track {
    let raw_stem = stem(path);
    let name = crate::strip_track_number(&raw_stem).to_string();

    Track {
        id,
        name,
        artist: parent_name(path, 2),
        album: parent_name(path, 1),
        location: Some(path.to_string_lossy().to_string()),
        kind: Some(format!(
            "{} audio file",
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_uppercase()
        )),
        ..Default::default()
    }
}

/// Get the file stem (name without extension).
fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// Get the name of an ancestor directory (1 = parent, 2 = grandparent).
fn parent_name(path: &Path, levels: usize) -> String {
    let mut current = path.parent();
    for _ in 1..levels {
        current = current.and_then(|p| p.parent());
    }
    current
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

impl MusicLibrary for DirectoryLibrary {
    fn artists(&self) -> Vec<&str> {
        let mut artists: Vec<&str> = self
            .tracks
            .values()
            .map(|t| t.artist.as_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        artists.sort_unstable();
        artists
    }

    fn albums(&self) -> Vec<(&str, &str)> {
        let mut albums: Vec<(&str, &str)> = self
            .tracks
            .values()
            .map(|t| (t.artist.as_str(), t.album.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        albums.sort_unstable();
        albums
    }

    fn artist_tracks<'a>(&'a self, artist: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let artist = artist.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.artist.eq_ignore_ascii_case(&artist)),
        )
    }

    fn album_tracks<'a>(&'a self, album: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let album = album.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.album.eq_ignore_ascii_case(&album)),
        )
    }

    fn album_tracks_by_artist<'a>(
        &'a self,
        artist: &str,
        album: &str,
    ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let artist = artist.to_string();
        let album = album.to_string();
        Box::new(self.tracks.values().filter(move |t| {
            t.album.eq_ignore_ascii_case(&album) && t.artist.eq_ignore_ascii_case(&artist)
        }))
    }

    fn tracks_by_name<'a>(&'a self, name: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let name = name.to_string();
        Box::new(
            self.tracks
                .values()
                .filter(move |t| t.name.eq_ignore_ascii_case(&name)),
        )
    }

    fn track_count(&self) -> usize {
        self.tracks.len()
    }

    fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
        Box::new(self.tracks.values())
    }

    fn music_folder(&self) -> Option<&str> {
        Some(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_empty_directory() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib.track_count(), 0);
        assert!(lib.artists().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_nonexistent_directory() {
        assert!(DirectoryLibrary::scan("/tmp/nonexistent-zytunes-dir-12345").is_err());
    }

    #[test]
    fn track_from_path_extracts_metadata() {
        let path = Path::new("/music/Radiohead/OK Computer/01 Airbag.flac");
        let track = track_from_path(path, 42);
        assert_eq!(track.name, "Airbag");
        assert_eq!(track.album, "OK Computer");
        assert_eq!(track.artist, "Radiohead");
        assert_eq!(track.id, 42);
    }

    #[test]
    fn is_audio_file_filters_correctly() {
        assert!(is_audio_file(Path::new("song.mp3")));
        assert!(is_audio_file(Path::new("song.FLAC")));
        assert!(is_audio_file(Path::new("song.m4a")));
        assert!(!is_audio_file(Path::new("readme.txt")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
    }

    #[test]
    fn scan_finds_audio_files() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-scan");
        let _ = fs::remove_dir_all(&dir);
        let artist_dir = dir.join("Artist").join("Album");
        fs::create_dir_all(&artist_dir).unwrap();
        // Create dummy files (not valid audio, but the scanner only checks extensions).
        fs::write(artist_dir.join("01 Song.mp3"), b"fake").unwrap();
        fs::write(artist_dir.join("02 Track.flac"), b"fake").unwrap();
        fs::write(artist_dir.join("cover.jpg"), b"fake").unwrap();

        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib.track_count(), 2);
        assert_eq!(lib.artists(), vec!["Artist"]);
        assert_eq!(lib.albums(), vec![("Artist", "Album")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_with_progress_reports_total_and_per_track() {
        use std::sync::Mutex;

        let dir = std::env::temp_dir().join("zytunes-dirlib-progress");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("ArtistP").join("AlbumP");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Alpha.mp3"), b"fake").unwrap();
        fs::write(album.join("02 Beta.mp3"), b"fake").unwrap();
        fs::write(album.join("03 Gamma.mp3"), b"fake").unwrap();

        let updates: Mutex<Vec<ScanProgress>> = Mutex::new(Vec::new());
        let lib = DirectoryLibrary::scan_with_progress(dir.to_str().unwrap(), |p| {
            updates.lock().unwrap().push(p);
        })
        .unwrap();
        assert_eq!(lib.track_count(), 3);

        let updates = updates.into_inner().unwrap();
        // First emission is the total announcement with no sample.
        assert_eq!(updates.first().unwrap().total, 3);
        assert_eq!(updates.first().unwrap().completed, 0);
        assert!(updates.first().unwrap().sample.is_none());

        // One per-track update plus the opener = 4.
        assert_eq!(updates.len(), 4);
        let with_samples: Vec<_> = updates.iter().filter(|u| u.sample.is_some()).collect();
        assert_eq!(with_samples.len(), 3);
        for u in &with_samples {
            assert_eq!(u.total, 3);
            let s = u.sample.as_ref().unwrap();
            assert_eq!(s.artist, "ArtistP");
            assert_eq!(s.album, "AlbumP");
        }
        // Max completed count equals the total.
        let max_completed = updates.iter().map(|u| u.completed).max().unwrap();
        assert_eq!(max_completed, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_reuses_cached_tracks_for_unchanged_files() {
        // Use a distinct dir name so we don't race with other tests sharing
        // the same cache slot.
        let dir = std::env::temp_dir().join("zytunes-dirlib-incremental");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("CacheArtist").join("CacheAlbum");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Original.mp3"), b"fake").unwrap();
        fs::write(album.join("02 Second.mp3"), b"fake").unwrap();

        // First scan: nothing cached, everything parsed fresh.
        let lib1 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib1.track_count(), 2);

        // Peek at the cache: both files should be recorded.
        let log = crate::cache::default_logger();
        let cached = crate::cache::load_dirlib_cache(dir.to_str().unwrap(), &log);
        assert_eq!(cached.len(), 2);
        let original_fp = cached
            .get(album.join("01 Original.mp3").to_str().unwrap())
            .expect("first file should be cached")
            .fingerprint;

        // Add a third file. Second scan should reuse the first two from cache
        // and parse only the new one.
        fs::write(album.join("03 Added.mp3"), b"fake").unwrap();
        let lib2 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib2.track_count(), 3);

        let cached2 = crate::cache::load_dirlib_cache(dir.to_str().unwrap(), &log);
        assert_eq!(cached2.len(), 3);
        // Unchanged file's fingerprint must survive across scans.
        assert_eq!(
            cached2
                .get(album.join("01 Original.mp3").to_str().unwrap())
                .unwrap()
                .fingerprint,
            original_fp,
        );
        // Deleted file must be dropped from the cache.
        fs::remove_file(album.join("01 Original.mp3")).unwrap();
        let lib3 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib3.track_count(), 2);
        let cached3 = crate::cache::load_dirlib_cache(dir.to_str().unwrap(), &log);
        assert_eq!(cached3.len(), 2);
        assert!(!cached3.contains_key(album.join("01 Original.mp3").to_str().unwrap()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_with_options_disabled_skips_fingerprinting() {
        // When fingerprinting is disabled, the scan must NOT compute or
        // backfill an `acoustic_id`. Used by the `fingerprinting = false`
        // config option to keep reloads fast at the cost of cross-device
        // playcount merging.
        let dir = std::env::temp_dir().join("zytunes-dirlib-fp-disabled");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("OffArtist").join("OffAlbum");
        fs::create_dir_all(&album).unwrap();
        write_sine_wav(&album.join("01 OffSong.wav"), 10);

        let opts = ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        };
        let lib = DirectoryLibrary::scan_with_options(dir.to_str().unwrap(), opts, |_| {}).unwrap();
        let track = lib.all_tracks().next().unwrap();
        assert!(
            track.acoustic_id.is_none(),
            "fingerprint:false must leave acoustic_id None even for decodable files; got Some"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pure_reload_does_not_rewrite_cache() {
        // After a first scan populates the cache, a second scan with no
        // file changes must NOT touch the cache file. Previously every
        // chunked save re-serialised the entire (potentially 36 MB) cache
        // even when nothing had changed, which made reloads scale with
        // library size instead of staying constant.
        let dir = std::env::temp_dir().join("zytunes-dirlib-pure-reload");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("ReloadArtist").join("ReloadAlbum");
        fs::create_dir_all(&album).unwrap();
        write_sine_wav(&album.join("01 R.wav"), 5);
        write_sine_wav(&album.join("02 R.wav"), 5);

        // First scan: populates cache.
        let opts = ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        };
        DirectoryLibrary::scan_with_options(dir.to_str().unwrap(), opts.clone(), |_| {}).unwrap();

        // Capture cache file mtime.
        let cache_path = std::path::Path::new(&std::env::var("HOME").unwrap())
            .join(".cache")
            .join("zytunes")
            .join(format!("dirlib-library-{:016x}.json", {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                dir.to_str().unwrap().hash(&mut h);
                h.finish()
            }));
        let mtime_before = std::fs::metadata(&cache_path).unwrap().modified().unwrap();

        // Sleep just enough for filesystem mtime granularity to advance had
        // we written. 1.1s covers ext4 (1s granularity) and HFS+ (1s).
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Second scan with no file changes — must not rewrite the cache.
        DirectoryLibrary::scan_with_options(dir.to_str().unwrap(), opts, |_| {}).unwrap();
        let mtime_after = std::fs::metadata(&cache_path).unwrap().modified().unwrap();

        assert_eq!(
            mtime_before, mtime_after,
            "pure reload must not rewrite the cache file"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_name_handles_shallow_paths() {
        let path = Path::new("/song.mp3");
        // Root "/" has no file_name(), so both levels return "Unknown".
        assert_eq!(parent_name(path, 1), "Unknown");
        assert_eq!(parent_name(path, 2), "Unknown");

        let path = Path::new("/Music/song.mp3");
        assert_eq!(parent_name(path, 1), "Music");
        assert_eq!(parent_name(path, 2), "Unknown");
    }

    /// Generate a minimal valid WAV file.
    fn make_test_wav(path: &Path) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let num_samples: usize = 100;
        let data_size =
            (num_samples * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let file_size = 36 + data_size;

        let mut f = fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_size as usize]).unwrap();
    }

    #[test]
    fn build_track_reads_wav_tags_via_lofty() {
        let dir = std::env::temp_dir().join("zytunes-dirlib-lofty");
        let _ = fs::remove_dir_all(&dir);
        let artist_dir = dir.join("FallbackArtist").join("FallbackAlbum");
        fs::create_dir_all(&artist_dir).unwrap();

        let wav_path = artist_dir.join("song.wav");
        make_test_wav(&wav_path);

        // Write tags with lofty (not id3)
        {
            use lofty::file::TaggedFileExt;
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Bjork".to_string());
            tag.set_album("Post".to_string());
            tag.set_title("Army of Me".to_string());
            tag.set_track(1);
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let track = build_track(&wav_path, 99);
        assert_eq!(track.artist, "Bjork");
        assert_eq!(track.album, "Post");
        assert_eq!(track.name, "Army of Me");
        assert_eq!(track.track_number, Some(1));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_track_reads_extended_lofty_metadata() {
        // Tag a WAV with the full kitchen sink of metadata that
        // `track_from_lofty` is now expected to surface — keys that previously
        // got dropped on the floor (composer, ISRC, BPM, MusicBrainz IDs,
        // ReplayGain values, comment) plus the audio-properties fields read
        // from `tagged.properties()` (sample rate, channels, bit depth,
        // bitrate, file size).
        let dir = std::env::temp_dir().join("zytunes-dirlib-lofty-extended");
        let _ = fs::remove_dir_all(&dir);
        let album_dir = dir.join("Artist").join("Album");
        fs::create_dir_all(&album_dir).unwrap();

        let wav_path = album_dir.join("track.wav");
        make_test_wav(&wav_path);

        {
            // Use ID3v2 explicitly — RIFF INFO (the WAV default) supports
            // only a tiny subset of the keys we want to round-trip, and lofty
            // silently drops unsupported keys when writing.
            use lofty::file::TaggedFileExt;
            use lofty::prelude::ItemKey;
            use lofty::tag::{Accessor, Tag, TagExt, TagType};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            tagged.insert_tag(Tag::new(TagType::Id3v2));
            let tag = tagged
                .tag_mut(TagType::Id3v2)
                .expect("Id3v2 tag was just inserted");
            tag.set_artist("Test Artist".to_string());
            tag.set_album("Test Album".to_string());
            tag.set_title("Test Title".to_string());
            tag.insert_text(ItemKey::AlbumArtist, "Test Album Artist".to_string());
            tag.insert_text(ItemKey::Composer, "Test Composer".to_string());
            tag.insert_text(ItemKey::Conductor, "Test Conductor".to_string());
            tag.insert_text(ItemKey::Lyricist, "Test Lyricist".to_string());
            tag.insert_text(ItemKey::Comment, "Test comment line".to_string());
            tag.insert_text(ItemKey::IntegerBpm, "128".to_string());
            tag.insert_text(ItemKey::InitialKey, "Cmaj".to_string());
            tag.insert_text(ItemKey::Mood, "Pensive".to_string());
            tag.insert_text(ItemKey::Language, "eng".to_string());
            tag.insert_text(ItemKey::Isrc, "USRC17607839".to_string());
            tag.insert_text(ItemKey::Barcode, "012345678905".to_string());
            tag.insert_text(ItemKey::CatalogNumber, "CAT-001".to_string());
            tag.insert_text(ItemKey::Publisher, "Test Records".to_string());
            tag.insert_text(ItemKey::CopyrightMessage, "© 2026".to_string());
            tag.insert_text(ItemKey::TrackTotal, "12".to_string());
            tag.insert_text(ItemKey::DiscTotal, "2".to_string());
            // ID3v2's UFID frame requires a registered owner identifier;
            // round-tripping `MusicBrainzRecordingId` through ID3v2 in this
            // generic-tag form is not reliable, so we exercise
            // `MusicBrainzReleaseId` (TXXX-based) for the MB code path.
            tag.insert_text(
                ItemKey::MusicBrainzReleaseId,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            );
            tag.insert_text(ItemKey::ReplayGainTrackGain, "-7.20 dB".to_string());
            tag.insert_text(ItemKey::ReplayGainAlbumGain, "-6.50 dB".to_string());
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let track = build_track(&wav_path, 7);

        // Tag-derived fields.
        assert_eq!(track.album_artist.as_deref(), Some("Test Album Artist"));
        assert_eq!(track.composer.as_deref(), Some("Test Composer"));
        assert_eq!(track.conductor.as_deref(), Some("Test Conductor"));
        assert_eq!(track.lyricist.as_deref(), Some("Test Lyricist"));
        assert_eq!(track.comment.as_deref(), Some("Test comment line"));
        assert_eq!(track.bpm, Some(128));
        assert_eq!(track.initial_key.as_deref(), Some("Cmaj"));
        assert_eq!(track.mood.as_deref(), Some("Pensive"));
        assert_eq!(track.language.as_deref(), Some("eng"));
        assert_eq!(track.isrc.as_deref(), Some("USRC17607839"));
        assert_eq!(track.barcode.as_deref(), Some("012345678905"));
        assert_eq!(track.catalog_number.as_deref(), Some("CAT-001"));
        assert_eq!(track.publisher.as_deref(), Some("Test Records"));
        assert_eq!(track.copyright.as_deref(), Some("© 2026"));
        assert_eq!(track.track_total, Some(12));
        assert_eq!(track.disc_total, Some(2));
        assert_eq!(
            track.mb_release_id.as_deref(),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        );
        assert_eq!(track.replaygain_track_gain.as_deref(), Some("-7.20 dB"));
        assert_eq!(track.replaygain_album_gain.as_deref(), Some("-6.50 dB"));

        // Audio properties — should always populate on a real WAV.
        assert_eq!(track.sample_rate, Some(44_100));
        assert_eq!(track.channels, Some(2));
        assert_eq!(track.bit_depth, Some(16));
        assert!(
            track.file_size_bytes.is_some_and(|n| n > 0),
            "file_size_bytes should be populated from the on-disk file"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn track_from_path_leaves_extended_fields_none() {
        // The fallback path (no readable tags) should not invent metadata for
        // the new fields — they all default to None.
        let dir = std::env::temp_dir().join("zytunes-dirlib-pathfallback");
        let _ = fs::remove_dir_all(&dir);
        let album_dir = dir.join("Artist").join("Album");
        fs::create_dir_all(&album_dir).unwrap();
        let path = album_dir.join("01 Untagged.mp3");
        fs::write(&path, b"not a real mp3").unwrap();

        let track = track_from_path(&path, 1);
        assert!(track.composer.is_none());
        assert!(track.isrc.is_none());
        assert!(track.bpm.is_none());
        assert!(track.mb_recording_id.is_none());
        assert!(track.replaygain_track_gain.is_none());
        assert!(track.sample_rate.is_none());
        assert!(track.channels.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    use crate::test_audio::write_sine_wav;

    #[test]
    fn scan_populates_acoustic_id_for_real_audio() {
        // Uses a real sine WAV so the symphonia decode → chromaprint path
        // actually runs end-to-end via the scan pipeline.
        let dir = std::env::temp_dir().join("zytunes-dirlib-acousticid");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("FpArtist").join("FpAlbum");
        fs::create_dir_all(&album).unwrap();
        write_sine_wav(&album.join("01 FpSong.wav"), 10);

        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        assert_eq!(lib.track_count(), 1);
        let track = lib.all_tracks().next().unwrap();
        assert!(
            track.acoustic_id.is_some(),
            "scan must populate acoustic_id for decodable files; got None"
        );

        // Second scan hits the cache but keeps the stored fingerprint.
        let original_fp = track.acoustic_id.clone();
        let lib2 = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        let track2 = lib2.all_tracks().next().unwrap();
        assert_eq!(
            track2.acoustic_id, original_fp,
            "cached scan must preserve the acoustic_id"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_backfills_acoustic_id_on_cached_track_with_none() {
        // Simulates an older cache from before acoustic_id existed: we
        // manually seed the cache with acoustic_id=None, then scan. The new
        // scan should reuse the cached metadata (mtime/size still matches)
        // but compute a fresh fingerprint, demonstrating the backfill path.
        let dir = std::env::temp_dir().join("zytunes-dirlib-fp-backfill");
        let _ = fs::remove_dir_all(&dir);
        let album = dir.join("BackArtist").join("BackAlbum");
        fs::create_dir_all(&album).unwrap();
        let file = album.join("01 BackSong.wav");
        write_sine_wav(&file, 10);

        let path_key = file.to_string_lossy().to_string();
        let fp = crate::cache::FileFingerprint::from_path(&file).unwrap();
        let seed = crate::cache::CachedFile {
            fingerprint: fp,
            track: Track {
                id: 42,
                name: "Back Song".into(),
                artist: "Back Artist".into(),
                album: "Back Album".into(),
                location: Some(path_key.clone()),
                acoustic_id: None, // simulated pre-feature cache
                ..Default::default()
            },
        };
        let mut seeded = HashMap::new();
        seeded.insert(path_key, seed);
        let log = crate::cache::default_logger();
        crate::cache::save_dirlib_cache(dir.to_str().unwrap(), seeded, &log);

        let lib = DirectoryLibrary::scan(dir.to_str().unwrap()).unwrap();
        let track = lib.all_tracks().next().unwrap();
        assert!(
            track.acoustic_id.is_some(),
            "scan must backfill acoustic_id for cached tracks that lack it"
        );
        // Metadata from the seeded cache must survive — proof we took the
        // cache-hit path rather than re-parsing tags from the file.
        assert_eq!(track.name, "Back Song");
        assert_eq!(track.id, 42);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_track_path_fallback_for_untagged() {
        // File with no tags should infer from Artist/Album/Track.ext path structure
        let path = Path::new("/music/Radiohead/OK Computer/03 Subterranean Homesick Alien.flac");
        let track = build_track(path, 42);
        assert_eq!(track.artist, "Radiohead");
        assert_eq!(track.album, "OK Computer");
        assert_eq!(track.name, "Subterranean Homesick Alien");
    }
}
