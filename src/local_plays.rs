//! Local play tracking sidecar — aggregates plays across the TUI and any
//! connected devices, mirroring iTunes' approach: the library-side count is
//! authoritative, and each device's MTP-reported counter contributes via
//! observed deltas (per-device baselines) on every reconnect.
//!
//! ```text
//!   library.play_count = TUI plays + Σ (device.play_count - per-device baseline)
//! ```
//!
//! The sidecar lives at `~/.cache/zytunes/local-plays.json` and is
//! intentionally outside `ZYTUNES_CACHE_DIR`'s reach — plays are user state,
//! not per-device cache, and worktrees pointed at the same `~/Music` should
//! share one history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::device::DeviceFamily;

/// Bumped when the on-disk shape changes such that older entries would
/// silently carry default values for new fields. On mismatch the file is
/// discarded — a small one-time history gap beats wrong totals forever.
const SCHEMA_VERSION: u32 = 1;

/// iTunes-style play threshold: 50% of the track or 4 minutes, whichever
/// arrives first. Single source of truth so the App-side trigger and the
/// tests agree.
pub fn play_threshold_ms(duration_ms: u64) -> u64 {
    (duration_ms / 2).min(240_000)
}

/// Build the per-device baseline key. Namespacing by family keeps an iPod
/// and a Zune that happen to share a serial string from collapsing onto the
/// same baseline (vanishingly unlikely but free to defend against), and
/// makes the JSON file self-describing on inspection. Falls back to firmware
/// when the serial isn't known — collision-prone if two devices on the same
/// firmware ever connect, but better than dropping device plays entirely.
pub fn device_baseline_key(
    family: DeviceFamily,
    serial: Option<&str>,
    firmware: Option<&str>,
) -> String {
    let family_str = match family {
        DeviceFamily::Zune => "Zune",
        DeviceFamily::Ipod => "Ipod",
    };
    // Trim and reject empty/whitespace at each step so a whitespace-only
    // serial still falls through to firmware instead of collapsing onto the
    // "unknown" key with every other serial-less device.
    let key = serial
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| firmware.map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or("unknown");
    format!("{family_str}-{key}")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceBaseline {
    pub play_count_seen: u32,
    pub skip_count_seen: u32,
    pub last_synced_at_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackPlays {
    pub play_count: u32,
    pub skip_count: u32,
    /// Unix epoch ms of the last TUI play. Stays `0` when the track has only
    /// been played on a device — we don't fabricate a timestamp from sync
    /// time, since that could be hours or days after the actual play.
    pub last_played_at_ms: u64,
    pub device_baselines: HashMap<String, DeviceBaseline>,
}

#[derive(Serialize, Deserialize, Default)]
struct OnDisk {
    /// Default of `0` for files written before the field existed — treated
    /// as a stale schema and discarded.
    #[serde(default)]
    schema_version: u32,
    /// JSON object keys must be strings, so the `u64` library track ID is
    /// stringified on save and parsed back on load.
    tracks: HashMap<String, TrackPlays>,
}

/// In-memory sidecar. `App` holds one of these and persists changes via
/// `save_to` after each mutation.
#[derive(Clone, Debug, Default)]
pub struct LocalPlays {
    tracks: HashMap<u64, TrackPlays>,
}

impl LocalPlays {
    pub fn new() -> Self {
        LocalPlays::default()
    }

    pub fn get(&self, track_id: u64) -> Option<&TrackPlays> {
        self.tracks.get(&track_id)
    }

    /// Iterate over every `(track_id, plays)` pair. Used by the recommender
    /// to rank seeds by play count / recency without needing internal
    /// access to the storage map.
    pub fn entries(&self) -> impl Iterator<Item = (u64, &TrackPlays)> + '_ {
        self.tracks.iter().map(|(id, p)| (*id, p))
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Record a TUI play: bump the aggregate count and refresh the
    /// last-played timestamp. Idempotency within a playback session is the
    /// caller's responsibility (`NowPlaying.counted` flag at the App layer).
    pub fn record_play(&mut self, track_id: u64, now_ms: u64) {
        let entry = self.tracks.entry(track_id).or_default();
        entry.play_count = entry.play_count.saturating_add(1);
        entry.last_played_at_ms = now_ms;
    }

    /// Record a TUI skip: pre-threshold abandonment via next/prev/stop.
    /// Does not touch `last_played_at_ms` — a skip isn't a "play."
    pub fn record_skip(&mut self, track_id: u64) {
        let entry = self.tracks.entry(track_id).or_default();
        entry.skip_count = entry.skip_count.saturating_add(1);
    }

    /// Merge an observed device counter pair into the aggregate, using the
    /// per-device baseline to detect the delta since the last sync. First
    /// sight (no baseline yet) adopts the full device count — this matches
    /// iTunes: connecting an existing device merges its history into the
    /// library, it doesn't ignore it. Apparent negative deltas (factory
    /// reset, restore-from-backup, deleted-then-resynced) are clamped to
    /// zero and the baseline is reset to the new value, preserving the
    /// library count instead of subtracting from it.
    pub fn merge_device_observation(
        &mut self,
        track_id: u64,
        device_key: &str,
        observed_play: u32,
        observed_skip: u32,
        now_ms: u64,
    ) {
        let entry = self.tracks.entry(track_id).or_default();
        let prior = entry.device_baselines.get(device_key);
        let (delta_play, delta_skip) = match prior {
            None => (observed_play, observed_skip),
            Some(b) => (
                observed_play.saturating_sub(b.play_count_seen),
                observed_skip.saturating_sub(b.skip_count_seen),
            ),
        };
        entry.play_count = entry.play_count.saturating_add(delta_play);
        entry.skip_count = entry.skip_count.saturating_add(delta_skip);
        entry.device_baselines.insert(
            device_key.to_string(),
            DeviceBaseline {
                play_count_seen: observed_play,
                skip_count_seen: observed_skip,
                last_synced_at_ms: now_ms,
            },
        );
    }

    /// Read the sidecar from the well-known path. Missing, unparseable, or
    /// stale-schema files all degrade to an empty in-memory state.
    pub fn load() -> Self {
        sidecar_path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Read from an arbitrary path — the test seam.
    pub fn load_from(path: &Path) -> Self {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("zytunes: local-plays: read {} failed: {e}", path.display());
                }
                return LocalPlays::default();
            }
        };
        let parsed: OnDisk = match serde_json::from_slice(&data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "zytunes: local-plays: parse {} ({} bytes) failed: {e} — discarding",
                    path.display(),
                    data.len()
                );
                return LocalPlays::default();
            }
        };
        if parsed.schema_version != SCHEMA_VERSION {
            eprintln!(
                "zytunes: local-plays: stored schema {} != current {} — discarding {} entries",
                parsed.schema_version,
                SCHEMA_VERSION,
                parsed.tracks.len()
            );
            return LocalPlays::default();
        }
        let tracks = parsed
            .tracks
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u64>().ok().map(|id| (id, v)))
            .collect();
        LocalPlays { tracks }
    }

    /// Persist to the well-known path. Caller decides cadence — the App
    /// invokes this after each mutation since the file is bounded by
    /// library size (handful of bytes per entry).
    pub fn save(&self) {
        if let Some(p) = sidecar_path() {
            self.save_to(&p);
        }
    }

    /// Atomic write to an arbitrary path — staged via `.tmp` + rename so a
    /// crash mid-write leaves the previous file intact rather than a
    /// truncated JSON that `load_from` can't parse.
    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "zytunes: local-plays: mkdir {} failed: {e}",
                    parent.display()
                );
                return;
            }
        }
        let on_disk = OnDisk {
            schema_version: SCHEMA_VERSION,
            tracks: self
                .tracks
                .iter()
                .map(|(id, v)| (id.to_string(), v.clone()))
                .collect(),
        };
        let data = match serde_json::to_vec(&on_disk) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "zytunes: local-plays: serialize {} entries failed: {e}",
                    self.tracks.len()
                );
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &data) {
            eprintln!(
                "zytunes: local-plays: write {} ({} bytes) failed: {e}",
                tmp.display(),
                data.len()
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!(
                "zytunes: local-plays: rename {} -> {} failed: {e}",
                tmp.display(),
                path.display()
            );
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn sidecar_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache").join("zytunes"))
}

/// Default location of the sidecar (`~/.cache/zytunes/local-plays.json`).
/// Returns `None` when `HOME` is unset. Exposed so callers (the TUI App)
/// can pass it back into `save_to` and tests can override with a temp path.
pub fn default_save_path() -> Option<PathBuf> {
    sidecar_dir().map(|d| d.join("local-plays.json"))
}

fn sidecar_path() -> Option<PathBuf> {
    default_save_path()
}

/// Current unix time in milliseconds. Returns `0` if the system clock is
/// before the epoch (effectively impossible on real systems but keeps the
/// type simple — callers don't have to unwrap).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- play_threshold_ms --

    #[test]
    fn play_threshold_caps_at_four_minutes() {
        // 10-minute track: 50% would be 5 min, but cap is 4 min.
        assert_eq!(play_threshold_ms(600_000), 240_000);
    }

    #[test]
    fn play_threshold_uses_half_for_short_tracks() {
        // 3-minute track: 50% (1m30s) is below the 4-min cap.
        assert_eq!(play_threshold_ms(180_000), 90_000);
    }

    #[test]
    fn play_threshold_zero_duration_is_zero() {
        assert_eq!(play_threshold_ms(0), 0);
    }

    #[test]
    fn play_threshold_at_exactly_eight_minutes_caps() {
        // 8-minute track: 50% is exactly 4 min — both branches agree.
        assert_eq!(play_threshold_ms(480_000), 240_000);
    }

    // -- device_baseline_key --

    #[test]
    fn baseline_key_with_serial() {
        assert_eq!(
            device_baseline_key(DeviceFamily::Zune, Some("ABC123"), None),
            "Zune-ABC123"
        );
    }

    #[test]
    fn baseline_key_falls_back_to_firmware() {
        assert_eq!(
            device_baseline_key(DeviceFamily::Zune, None, Some("01.04.00485")),
            "Zune-01.04.00485"
        );
    }

    #[test]
    fn baseline_key_falls_back_to_unknown_when_neither_present() {
        assert_eq!(
            device_baseline_key(DeviceFamily::Ipod, None, None),
            "Ipod-unknown"
        );
    }

    #[test]
    fn baseline_key_treats_empty_serial_as_missing() {
        // A device that returns "" or whitespace from MTP shouldn't
        // collapse all such devices onto a single baseline.
        assert_eq!(
            device_baseline_key(DeviceFamily::Zune, Some("   "), Some("3.0")),
            "Zune-3.0"
        );
        assert_eq!(
            device_baseline_key(DeviceFamily::Zune, Some(""), None),
            "Zune-unknown"
        );
    }

    #[test]
    fn baseline_key_namespaces_by_family() {
        // Identical serial strings on different families stay distinct.
        let zune = device_baseline_key(DeviceFamily::Zune, Some("XYZ"), None);
        let ipod = device_baseline_key(DeviceFamily::Ipod, Some("XYZ"), None);
        assert_ne!(zune, ipod);
    }

    // -- record_play / record_skip --

    #[test]
    fn record_play_increments_and_stamps_time() {
        let mut p = LocalPlays::new();
        p.record_play(42, 1_700_000_000_000);
        let entry = p.get(42).unwrap();
        assert_eq!(entry.play_count, 1);
        assert_eq!(entry.last_played_at_ms, 1_700_000_000_000);

        p.record_play(42, 1_700_000_001_000);
        let entry = p.get(42).unwrap();
        assert_eq!(entry.play_count, 2);
        assert_eq!(entry.last_played_at_ms, 1_700_000_001_000);
    }

    #[test]
    fn record_skip_does_not_touch_last_played() {
        let mut p = LocalPlays::new();
        p.record_play(7, 1_700_000_000_000);
        p.record_skip(7);
        let entry = p.get(7).unwrap();
        assert_eq!(entry.skip_count, 1);
        // last_played_at_ms must still reflect the play, not the skip.
        assert_eq!(entry.last_played_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn record_skip_creates_entry_if_missing() {
        let mut p = LocalPlays::new();
        p.record_skip(99);
        let entry = p.get(99).unwrap();
        assert_eq!(entry.skip_count, 1);
        assert_eq!(entry.play_count, 0);
        assert_eq!(entry.last_played_at_ms, 0);
    }

    #[test]
    fn entries_yields_every_recorded_track() {
        // The recommender ranks seeds via this iterator (not by `get`),
        // so an empty store must yield empty and a populated store must
        // surface every (id, plays) pair regardless of insertion order.
        let mut p = LocalPlays::new();
        assert_eq!(p.entries().count(), 0, "empty store yields no entries");

        p.record_play(1, 1_000);
        p.record_play(2, 2_000);
        p.record_play(2, 3_000);
        p.record_skip(7);

        let mut collected: Vec<(u64, u32, u32, u64)> = p
            .entries()
            .map(|(id, t)| (id, t.play_count, t.skip_count, t.last_played_at_ms))
            .collect();
        collected.sort_by_key(|x| x.0);
        assert_eq!(
            collected,
            vec![(1, 1, 0, 1_000), (2, 2, 0, 3_000), (7, 0, 1, 0)],
        );
    }

    // -- merge_device_observation: the meat of the aggregation algorithm --

    #[test]
    fn merge_first_sight_adopts_full_device_count() {
        // The user's example: 0 TUI plays + first sight of device with 7 → 7.
        let mut p = LocalPlays::new();
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 1_000);
        assert_eq!(p.get(1).unwrap().play_count, 7);
        assert_eq!(
            p.get(1).unwrap().device_baselines["Zune-ABC"].play_count_seen,
            7
        );
    }

    #[test]
    fn merge_first_sight_adds_to_existing_tui_plays() {
        // The user's example: 2 TUI plays + first sight of device with 7 → 9.
        let mut p = LocalPlays::new();
        p.record_play(1, 100);
        p.record_play(1, 200);
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 1_000);
        assert_eq!(p.get(1).unwrap().play_count, 9);
    }

    #[test]
    fn merge_subsequent_uses_delta_only() {
        // First sight: 7. Then 7 → 8 on the device. Library should go up by 1.
        let mut p = LocalPlays::new();
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 1_000);
        p.merge_device_observation(1, "Zune-ABC", 8, 0, 2_000);
        assert_eq!(p.get(1).unwrap().play_count, 8);
    }

    #[test]
    fn merge_no_change_is_noop_for_count() {
        // The user's third step: reconnect with no change should not move
        // the library count.
        let mut p = LocalPlays::new();
        p.record_play(1, 100);
        p.record_play(1, 200);
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 1_000);
        assert_eq!(p.get(1).unwrap().play_count, 9);
        // Reconnect, device still reports 7.
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 2_000);
        assert_eq!(p.get(1).unwrap().play_count, 9);
        // But last_synced_at_ms moves forward.
        assert_eq!(
            p.get(1).unwrap().device_baselines["Zune-ABC"].last_synced_at_ms,
            2_000
        );
    }

    #[test]
    fn merge_device_count_went_down_clamps_and_resets_baseline() {
        // Device factory-reset: was 7, now 0. Library count is preserved
        // (delta clamped to 0), baseline is reset to 0 so the next legit
        // increment past 0 counts as a fresh delta.
        let mut p = LocalPlays::new();
        p.merge_device_observation(1, "Zune-ABC", 7, 0, 1_000);
        assert_eq!(p.get(1).unwrap().play_count, 7);

        p.merge_device_observation(1, "Zune-ABC", 0, 0, 2_000);
        assert_eq!(p.get(1).unwrap().play_count, 7);
        assert_eq!(
            p.get(1).unwrap().device_baselines["Zune-ABC"].play_count_seen,
            0
        );

        // After reset, two new device plays land cleanly.
        p.merge_device_observation(1, "Zune-ABC", 2, 0, 3_000);
        assert_eq!(p.get(1).unwrap().play_count, 9);
    }

    #[test]
    fn merge_multiple_devices_keep_separate_baselines() {
        // A second device contributes its own deltas without disturbing
        // the first device's baseline.
        let mut p = LocalPlays::new();
        p.merge_device_observation(1, "Zune-AAA", 5, 0, 1_000);
        p.merge_device_observation(1, "Zune-BBB", 3, 0, 2_000);
        assert_eq!(p.get(1).unwrap().play_count, 8);

        // Bump only Zune-AAA: library should track only that delta.
        p.merge_device_observation(1, "Zune-AAA", 6, 0, 3_000);
        assert_eq!(p.get(1).unwrap().play_count, 9);
        // Zune-BBB's baseline is untouched.
        assert_eq!(
            p.get(1).unwrap().device_baselines["Zune-BBB"].play_count_seen,
            3
        );
    }

    #[test]
    fn merge_skips_alongside_plays() {
        let mut p = LocalPlays::new();
        p.merge_device_observation(1, "Zune-ABC", 5, 2, 1_000);
        assert_eq!(p.get(1).unwrap().play_count, 5);
        assert_eq!(p.get(1).unwrap().skip_count, 2);

        p.merge_device_observation(1, "Zune-ABC", 5, 4, 2_000);
        // Plays unchanged, skips up by 2.
        assert_eq!(p.get(1).unwrap().play_count, 5);
        assert_eq!(p.get(1).unwrap().skip_count, 4);
    }

    #[test]
    fn merge_does_not_update_last_played_timestamp() {
        // We don't fabricate last_played_at_ms from sync time — the device
        // doesn't tell us when the play actually happened.
        let mut p = LocalPlays::new();
        p.record_play(1, 100); // TUI play sets last_played to 100
        p.merge_device_observation(1, "Zune-ABC", 5, 0, 999_999); // sync time 999_999
        assert_eq!(p.get(1).unwrap().last_played_at_ms, 100);
    }

    // -- save / load round-trip --

    fn temp_path(stem: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zytunes-local-plays-test-{}-{}.json",
            std::process::id(),
            stem
        ))
    }

    #[test]
    fn save_load_round_trip() {
        let path = temp_path("round-trip");
        let mut p = LocalPlays::new();
        p.record_play(1, 1_700_000_000_000);
        p.record_play(1, 1_700_000_010_000);
        p.record_skip(2);
        p.merge_device_observation(1, "Zune-ABC", 5, 1, 1_700_000_020_000);
        p.save_to(&path);

        let loaded = LocalPlays::load_from(&path);
        assert_eq!(loaded.len(), 2);

        let t1 = loaded.get(1).unwrap();
        assert_eq!(t1.play_count, 7); // 2 TUI + 5 first-sight
        assert_eq!(t1.skip_count, 1); // 1 first-sight
        assert_eq!(t1.last_played_at_ms, 1_700_000_010_000);
        assert_eq!(t1.device_baselines["Zune-ABC"].play_count_seen, 5);
        assert_eq!(t1.device_baselines["Zune-ABC"].skip_count_seen, 1);
        assert_eq!(
            t1.device_baselines["Zune-ABC"].last_synced_at_ms,
            1_700_000_020_000
        );

        let t2 = loaded.get(2).unwrap();
        assert_eq!(t2.skip_count, 1);
        assert_eq!(t2.play_count, 0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_uses_atomic_rename() {
        // Half-written .tmp must not be renamed unless write succeeds.
        // We assert the post-condition: after save, the .tmp does not exist
        // alongside the final file.
        let path = temp_path("atomic");
        let mut p = LocalPlays::new();
        p.record_play(1, 100);
        p.save_to(&path);

        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "stale .tmp left behind after save");
        assert!(path.exists());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let p = LocalPlays::load_from(&path);
        assert!(p.is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_empty() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"this is not json {{{").unwrap();
        let p = LocalPlays::load_from(&path);
        assert!(p.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_stale_schema_returns_empty() {
        let path = temp_path("stale-schema");
        // Mock an on-disk file with schema_version = 0 (pre-versioning).
        let body = r#"{"schema_version":0,"tracks":{"1":{"play_count":99,"skip_count":0,"last_played_at_ms":0,"device_baselines":{}}}}"#;
        std::fs::write(&path, body).unwrap();
        let p = LocalPlays::load_from(&path);
        assert!(p.is_empty(), "stale-schema file must be discarded");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_skips_entries_with_unparseable_track_id() {
        // A track-ID key that isn't a valid u64 silently drops to keep one
        // bad row from poisoning the whole file.
        let path = temp_path("bad-id");
        let body = r#"{"schema_version":1,"tracks":{"not-a-number":{"play_count":5,"skip_count":0,"last_played_at_ms":0,"device_baselines":{}},"42":{"play_count":3,"skip_count":0,"last_played_at_ms":0,"device_baselines":{}}}}"#;
        std::fs::write(&path, body).unwrap();
        let p = LocalPlays::load_from(&path);
        assert_eq!(p.len(), 1);
        assert_eq!(p.get(42).unwrap().play_count, 3);
        let _ = std::fs::remove_file(&path);
    }
}
