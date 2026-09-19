//! Per-device track-list cache backed by a tab-separated file under
//! `~/.zytunes-track-cache-{serial}` (or whichever directory
//! `ZYTUNES_CACHE_DIR` redirects to).
//!
//! The cache lets a reconnect skip a full MTP rescan: the only thing we
//! re-validate is the device's free-bytes header. Schema has grown over
//! time (`play_count`, `rating`, `skip_count`, `duration_ms` columns added in later
//! phases) and `load` accepts every historical shape so users don't lose
//! their cache when they upgrade.

use std::path::PathBuf;

use crate::mtp::parse::DeviceEntry;

/// Track cache for persisting device track lists across sessions.
pub struct TrackCache {
    pub(super) serial: Option<String>,
    pub(super) cache_dir: Option<PathBuf>,
}

impl TrackCache {
    pub(super) fn new(serial: Option<String>) -> Self {
        let cache_dir = crate::paths::device_cache_base();
        TrackCache { serial, cache_dir }
    }

    pub(super) fn cache_path(&self) -> Option<PathBuf> {
        let dir = self.cache_dir.as_ref()?;
        let filename = match &self.serial {
            Some(s) => format!(".zytunes-track-cache-{}", s),
            None => ".zytunes-track-cache".to_string(),
        };
        Some(dir.join(filename))
    }

    /// Load cached tracks. Returns `(cached_free_bytes, tracks)`.
    pub(super) fn load(&self) -> Option<(u64, Vec<DeviceEntry>)> {
        let path = self.cache_path()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut entries = Vec::new();
        let mut free_bytes = 0u64;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("#free_bytes:") {
                free_bytes = val.parse().unwrap_or(0);
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            // Format evolution: original cache had 5 fields; play_count
            // (column 5) and rating (column 6) were added in Phase 4b;
            // skip_count (column 7) was added in Phase 3 alongside the
            // local-plays sidecar; duration_ms (column 8) followed.
            // Old caches still parse — missing fields stay `None`.
            // `lines()` already strips `\n` and `\r\n`, so per-field
            // newline trimming would be redundant.
            let parts: Vec<&str> = line.splitn(9, '\t').collect();
            if parts.len() < 5 {
                continue;
            }
            let play_count = parts.get(5).and_then(|s| s.parse::<u32>().ok());
            let rating = parts.get(6).and_then(|s| s.parse::<u16>().ok());
            let skip_count = parts.get(7).and_then(|s| s.parse::<u32>().ok());
            let duration_ms = parts.get(8).and_then(|s| s.parse::<u32>().ok());
            entries.push(DeviceEntry {
                object_id: parts[0].parse().unwrap_or(0),
                storage_id: parts[1].parse().unwrap_or(0),
                format: parts[2].to_string(),
                size: parts[3].parse().unwrap_or(0),
                name: parts[4].to_string(),
                play_count,
                rating,
                skip_count,
                duration_ms,
                ..Default::default()
            });
        }
        if entries.is_empty() {
            None
        } else {
            Some((free_bytes, entries))
        }
    }

    /// Persist the track cache. Returns `Err` with a human-readable message
    /// when the write fails so callers can surface it through their log
    /// channel — silently dropping ENOSPC or a flipped permission would
    /// cost the next reconnect a full track scan with no diagnostic.
    pub(super) fn save(&self, tracks: &[DeviceEntry], free_bytes: u64) -> Result<(), String> {
        let Some(path) = self.cache_path() else {
            return Ok(());
        };
        let mut content = format!("#free_bytes:{free_bytes}\n");
        for t in tracks {
            content.push_str(&serialize_entry(t));
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("track cache: write {} failed: {e}", path.display()))
    }

    /// Clear the track cache entirely.
    pub fn clear(&self) {
        if let Some(path) = self.cache_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Update the `#free_bytes:` header in the existing cache file, preserving
    /// all track entries. No-op when the cache is empty, missing, or has no
    /// tracks — there's nothing to keep valid until the first full save.
    ///
    /// Rewrites only the header line and re-emits the rest of the file
    /// verbatim, avoiding the parse + re-serialize cost of `load()` + `save()`.
    pub(super) fn update_free_bytes(&self, free_bytes: u64) {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let Some(rest) = content
            .strip_prefix("#free_bytes:")
            .and_then(|s| s.split_once('\n').map(|(_, r)| r))
        else {
            return;
        };
        if rest.trim().is_empty() {
            return;
        }
        let _ = std::fs::write(path, format!("#free_bytes:{free_bytes}\n{rest}"));
    }

    pub(super) fn append(&self, entry: &DeviceEntry) {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return,
        };
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = file.write_all(serialize_entry(entry).as_bytes());
        }
    }

    pub(super) fn remove(&self, device_path: &str) {
        let path_suffix = device_path.trim_start_matches("/Music/");
        let prefix = format!("{path_suffix}/");
        // Name is field #5 (index 4) in the cache line; field #6 is the
        // optional play_count added in Phase 4b. Using `split` over `splitn`
        // because we only inspect one field by index.
        self.filter_cache(|line| {
            line.split('\t')
                .nth(4)
                .map(|name| name != path_suffix && !name.starts_with(&prefix))
                .unwrap_or(true)
        });
    }

    pub(super) fn remove_by_id(&self, object_id: u32) {
        let id_str = object_id.to_string();
        self.filter_cache(|line| {
            line.split('\t')
                .next()
                .map(|id| id != id_str)
                .unwrap_or(true)
        });
    }

    /// Read the cache file, keep only lines matching the predicate, write back.
    fn filter_cache<F: Fn(&str) -> bool>(&self, keep: F) {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let filtered: String = content
            .lines()
            .filter(|line| keep(line))
            .map(|line| format!("{}\n", line))
            .collect();
        let _ = std::fs::write(path, filtered);
    }
}

/// Serialize one `DeviceEntry` to its on-disk track-cache line. Tabs in
/// the name are flattened to spaces so the `splitn(9, '\t')` loader stays
/// in sync. `play_count`, `rating`, `skip_count`, and `duration_ms` are
/// emitted as digits or empty for `None`.
fn serialize_entry(entry: &DeviceEntry) -> String {
    let safe_name = entry.name.replace('\t', " ");
    let pc = entry.play_count.map(|v| v.to_string()).unwrap_or_default();
    let rt = entry.rating.map(|v| v.to_string()).unwrap_or_default();
    let sk = entry.skip_count.map(|v| v.to_string()).unwrap_or_default();
    let dur = entry.duration_ms.map(|v| v.to_string()).unwrap_or_default();
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        entry.object_id, entry.storage_id, entry.format, entry.size, safe_name, pc, rt, sk, dur
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache(serial: Option<&str>) -> TrackCache {
        let dir = std::env::temp_dir().join(format!("zytunes-test-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TrackCache {
            serial: serial.map(|s| s.to_string()),
            cache_dir: Some(dir),
        }
    }

    fn sample_entry(name: &str, id: u64) -> DeviceEntry {
        DeviceEntry {
            object_id: id,
            storage_id: 65537,
            format: "MP3".to_string(),
            size: 1024,
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn track_cache_path_uses_serial() {
        let cache = make_cache(Some("ABC123"));
        let path = cache.cache_path().unwrap();
        assert!(path
            .to_str()
            .unwrap()
            .contains("zytunes-track-cache-ABC123"));
    }

    #[test]
    fn track_cache_path_without_serial() {
        let cache = make_cache(None);
        let path = cache.cache_path().unwrap();
        assert!(path.to_str().unwrap().contains("zytunes-track-cache"));
        assert!(!path.to_str().unwrap().contains("zytunes-track-cache-"));
    }

    #[test]
    fn track_cache_save_and_load() {
        let cache = make_cache(Some("save-load"));
        let entries = vec![
            sample_entry("Artist/Album/track1.mp3", 100),
            sample_entry("Artist/Album/track2.mp3", 101),
        ];
        cache.save(&entries, 5_000_000).expect("save");

        let (free_bytes, loaded) = cache.load().unwrap();
        assert_eq!(free_bytes, 5_000_000);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Artist/Album/track1.mp3");
        assert_eq!(loaded[0].object_id, 100);
        assert_eq!(loaded[1].name, "Artist/Album/track2.mp3");

        // Cleanup.
        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_append() {
        let cache = make_cache(Some("append"));
        let entries = vec![sample_entry("first.mp3", 1)];
        cache.save(&entries, 0).expect("save");

        cache.append(&sample_entry("second.mp3", 2));

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].name, "second.mp3");

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_remove() {
        let cache = make_cache(Some("remove"));
        let entries = vec![
            sample_entry("Artist/Album/keep.mp3", 1),
            sample_entry("Artist/Album/delete.mp3", 2),
        ];
        cache.save(&entries, 0).expect("save");

        cache.remove("/Music/Artist/Album/delete.mp3");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Artist/Album/keep.mp3");

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_remove_folder_clears_children() {
        // Regression: rm of an album folder must clear every track entry
        // under it, not just an exact-name match. Without this, sync would
        // dedup the orphaned tracks as "already on device" and never
        // re-push them.
        let cache = make_cache(Some("remove-folder"));
        let entries = vec![
            sample_entry("Artist/AlbumA/song1.mp3", 1),
            sample_entry("Artist/AlbumA/song2.mp3", 2),
            sample_entry("Artist/AlbumB/song3.mp3", 3),
            sample_entry("OtherArtist/AlbumA/song4.mp3", 4),
        ];
        cache.save(&entries, 0).expect("save");

        cache.remove("/Music/Artist/AlbumA");

        let (_, loaded) = cache.load().unwrap();
        let names: Vec<&str> = loaded.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Artist/AlbumB/song3.mp3", "OtherArtist/AlbumA/song4.mp3"]
        );

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_remove_does_not_match_partial_prefix() {
        // "/Music/Album" must not also clobber "Album-Live/..." — the prefix
        // check has to use a path-segment boundary.
        let cache = make_cache(Some("remove-partial"));
        let entries = vec![
            sample_entry("Album/song.mp3", 1),
            sample_entry("Album-Live/song.mp3", 2),
        ];
        cache.save(&entries, 0).expect("save");

        cache.remove("/Music/Album");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Album-Live/song.mp3");

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_clear() {
        let cache = make_cache(Some("clear"));
        cache.save(&[sample_entry("x.mp3", 1)], 0).expect("save");
        assert!(cache.cache_path().unwrap().exists());

        cache.clear();
        assert!(!cache.cache_path().unwrap().exists());
    }

    #[test]
    fn track_cache_load_empty_returns_none() {
        let cache = make_cache(Some("empty"));
        assert!(cache.load().is_none());
    }

    #[test]
    fn track_cache_free_bytes_round_trips() {
        let cache = make_cache(Some("free-bytes"));
        let entries = vec![sample_entry("track.mp3", 1)];
        cache.save(&entries, 28_000_000_000).expect("save");

        let (free, loaded) = cache.load().unwrap();
        assert_eq!(free, 28_000_000_000);
        assert_eq!(loaded.len(), 1);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_header_only_returns_none() {
        let cache = make_cache(Some("header-only"));
        // A cache with just the header and no tracks should return None.
        if let Some(p) = cache.cache_path() {
            let _ = std::fs::write(&p, "#free_bytes:5000000\n");
            assert!(cache.load().is_none());
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_update_free_bytes_preserves_entries() {
        let cache = make_cache(Some("update-free"));
        let entries = vec![
            sample_entry("Artist/Album/a.mp3", 1),
            sample_entry("Artist/Album/b.mp3", 2),
        ];
        cache.save(&entries, 1_000_000).expect("save");

        cache.update_free_bytes(2_500_000);

        let (free, loaded) = cache.load().unwrap();
        assert_eq!(free, 2_500_000);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Artist/Album/a.mp3");
        assert_eq!(loaded[1].name, "Artist/Album/b.mp3");

        cache.clear();
    }

    #[test]
    fn track_cache_update_free_bytes_noop_when_empty() {
        let cache = make_cache(Some("update-free-empty"));
        // No prior save — file doesn't exist yet.
        cache.update_free_bytes(5_000_000);
        // Should not have created a file.
        assert!(cache.load().is_none());
        if let Some(p) = cache.cache_path() {
            assert!(!p.exists());
        }
    }

    #[test]
    fn track_cache_update_free_bytes_noop_when_header_only() {
        let cache = make_cache(Some("update-free-header-only"));
        let path = cache.cache_path().unwrap();
        std::fs::write(&path, "#free_bytes:1000\n").unwrap();
        cache.update_free_bytes(9_999);
        // Header-only caches carry no entries worth preserving, so the file
        // should be left untouched rather than having its header mutated.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#free_bytes:1000\n"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn track_cache_update_free_bytes_preserves_payload_bytes() {
        let cache = make_cache(Some("update-free-bytewise"));
        let entries = vec![
            sample_entry("Artist/Album/a.mp3", 1),
            sample_entry("Artist/Album/b.mp3", 2),
        ];
        cache.save(&entries, 1_000_000).expect("save");

        let path = cache.cache_path().unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let payload_before = before.split_once('\n').unwrap().1;

        cache.update_free_bytes(42);

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with("#free_bytes:42\n"));
        let payload_after = after.split_once('\n').unwrap().1;
        assert_eq!(payload_before, payload_after);

        cache.clear();
    }

    #[test]
    fn track_cache_legacy_format_defaults_free_bytes_zero() {
        let cache = make_cache(Some("legacy"));
        // Simulate an old cache file without the #free_bytes header.
        if let Some(p) = cache.cache_path() {
            let _ = std::fs::write(&p, "1\t1\tmp3\t1000\tArtist/Album/track.mp3\n");
            let (free, loaded) = cache.load().unwrap();
            assert_eq!(free, 0);
            assert_eq!(loaded.len(), 1);
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_tabs_in_name_handled() {
        let cache = make_cache(Some("tabs"));
        let entries = vec![sample_entry("Art\tist/Album/track.mp3", 1)];
        cache.save(&entries, 0).expect("save");

        let (_, loaded) = cache.load().unwrap();
        // Tab should be replaced with space in saved format.
        assert_eq!(loaded[0].name, "Art ist/Album/track.mp3");

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Locks down the schema-evolution claim in `TrackCache::load`'s comment:
    /// the parser must accept 5-field rows (pre-Phase-4b), 7-field rows with
    /// playcount/rating populated, mixed-format files, CRLF line endings, and
    /// 6-field rows where rating is missing but playcount is present.
    #[test]
    fn track_cache_load_handles_mixed_schema_rows() {
        let cache = make_cache(Some("mixed-schema"));
        let path = cache.cache_path().unwrap();

        // 5-field row (legacy), 6-field row (playcount only), 7-field row
        // (playcount + rating), and a CRLF-terminated 7-field row.
        let content = "#free_bytes:1000\n\
            1\t1\tmp3\t100\tA/B/legacy.mp3\n\
            2\t1\tmp3\t200\tA/B/playcount-only.mp3\t7\n\
            3\t1\tmp3\t300\tA/B/full.mp3\t12\t80\n\
            4\t1\tmp3\t400\tA/B/crlf.mp3\t3\t60\r\n";
        std::fs::write(&path, content).unwrap();

        let (free, loaded) = cache.load().expect("load");
        assert_eq!(free, 1000);
        assert_eq!(loaded.len(), 4);

        assert_eq!(loaded[0].name, "A/B/legacy.mp3");
        assert_eq!(loaded[0].play_count, None);
        assert_eq!(loaded[0].rating, None);

        assert_eq!(loaded[1].play_count, Some(7));
        assert_eq!(loaded[1].rating, None);

        assert_eq!(loaded[2].play_count, Some(12));
        assert_eq!(loaded[2].rating, Some(80));

        // CRLF row: lines() strips \r\n, so the rating parses cleanly.
        assert_eq!(loaded[3].name, "A/B/crlf.mp3");
        assert_eq!(loaded[3].play_count, Some(3));
        assert_eq!(loaded[3].rating, Some(60));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn track_cache_round_trips_skip_count() {
        let cache = make_cache(Some("skipcount-rt"));
        let entries = vec![
            DeviceEntry {
                skip_count: Some(2),
                ..sample_entry("Artist/Album/skipped.mp3", 100)
            },
            sample_entry("Artist/Album/never.mp3", 101),
        ];
        cache.save(&entries, 1_000).expect("save");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].skip_count, Some(2));
        assert_eq!(loaded[1].skip_count, None);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_loads_seven_field_format_without_skip_count() {
        // A pre-Phase-3 cache (7 fields: id, sid, fmt, size, name, pc, rt)
        // must still parse — `skip_count` defaults to None.
        let cache = make_cache(Some("seven-field-legacy"));
        if let Some(p) = cache.cache_path() {
            let _ = std::fs::write(
                &p,
                "#free_bytes:0\n1\t1\tmp3\t100\tA/B/legacy.mp3\t12\t80\n",
            );
            let (_, loaded) = cache.load().unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].play_count, Some(12));
            assert_eq!(loaded[0].rating, Some(80));
            assert_eq!(loaded[0].skip_count, None);
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_round_trips_play_count() {
        let cache = make_cache(Some("playcount-rt"));
        let entries = vec![
            DeviceEntry {
                play_count: Some(7),
                ..sample_entry("Artist/Album/played.mp3", 100)
            },
            sample_entry("Artist/Album/never.mp3", 101),
        ];
        cache.save(&entries, 1_000).expect("save");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].play_count, Some(7));
        assert_eq!(loaded[1].play_count, None);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_loads_legacy_5_field_format() {
        // Old cache files predate the play_count and rating columns; they
        // have only 5 tab-separated fields. The loader must accept them
        // and leave both Option fields as None rather than dropping the
        // entry.
        let cache = make_cache(Some("legacy-5col"));
        let path = cache.cache_path().unwrap();
        let legacy = "#free_bytes:0\n100\t65537\tMP3\t1024\tArtist/Album/old.mp3\n";
        std::fs::write(&path, legacy).unwrap();

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Artist/Album/old.mp3");
        assert_eq!(loaded[0].play_count, None);
        assert_eq!(loaded[0].rating, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn track_cache_loads_six_field_format_without_rating() {
        // Intermediate cache shape: play_count column added but rating
        // column not yet. Loader must keep play_count and leave rating None.
        let cache = make_cache(Some("legacy-6col"));
        let path = cache.cache_path().unwrap();
        let legacy = "#free_bytes:0\n100\t65537\tMP3\t1024\tArtist/Album/mid.mp3\t7\n";
        std::fs::write(&path, legacy).unwrap();

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].play_count, Some(7));
        assert_eq!(loaded[0].rating, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn track_cache_round_trips_rating() {
        let cache = make_cache(Some("rating-rt"));
        let entries = vec![
            DeviceEntry {
                rating: Some(80),
                ..sample_entry("Artist/Album/rated.mp3", 200)
            },
            sample_entry("Artist/Album/unrated.mp3", 201),
        ];
        cache.save(&entries, 1_000).expect("save");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].rating, Some(80));
        assert_eq!(loaded[1].rating, None);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_round_trips_duration() {
        let cache = make_cache(Some("duration-rt"));
        let entries = vec![
            DeviceEntry {
                duration_ms: Some(240_000),
                ..sample_entry("Artist/Album/timed.mp3", 100)
            },
            sample_entry("Artist/Album/untimed.mp3", 101),
        ];
        cache.save(&entries, 1_000).expect("save");

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].duration_ms, Some(240_000));
        assert_eq!(loaded[1].duration_ms, None);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn track_cache_loads_eight_field_format_without_duration() {
        // Pre-duration cache (8 fields: id, sid, fmt, size, name, pc, rt, sk)
        // must still parse — `duration_ms` defaults to None.
        let cache = make_cache(Some("eight-field-legacy"));
        if let Some(p) = cache.cache_path() {
            let _ = std::fs::write(
                &p,
                "#free_bytes:0\n1\t1\tmp3\t100\tA/B/legacy.mp3\t12\t80\t3\n",
            );
            let (_, loaded) = cache.load().unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].play_count, Some(12));
            assert_eq!(loaded[0].rating, Some(80));
            assert_eq!(loaded[0].skip_count, Some(3));
            assert_eq!(loaded[0].duration_ms, None);
            let _ = std::fs::remove_file(p);
        }
    }
}
