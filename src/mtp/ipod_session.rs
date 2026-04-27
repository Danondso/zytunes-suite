//! iPod device session implementing [`DeviceSession`] via the `ipod-db` crate.
//!
//! Unlike the Zune's MTP transport, the iPod is plain mass storage. All
//! operations are filesystem reads/writes plus iTunesDB binary serialization.
//! The database is written atomically with hash58 signing after every mutation.

use std::path::PathBuf;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::tag::Tag;

use super::parse::DeviceEntry;
use super::DeviceSession;

/// An active session with a connected iPod.
///
/// Holds the in-memory database and writes it back to disk (with hash58 signing)
/// after track imports and removals.
pub struct IpodSession {
    db: ipod_db::IpodDatabase,
    /// FirewireGuid for hash58 signing (iPod Classic). None for older models.
    firewire_id: Option<[u8; 20]>,
}

impl IpodSession {
    pub fn new(db: ipod_db::IpodDatabase, firewire_id: Option<[u8; 20]>) -> Self {
        Self { db, firewire_id }
    }

    /// Write the database to disk (atomically, with hash58 signing).
    ///
    /// Performs the libgpod-style end-of-sync bookkeeping:
    /// 1. Reassigns all track IDs sequentially from 52 (matches libgpod's
    ///    `prepare_itdb_for_write`)
    /// 2. Writes iTunesDB + ArtworkDB via the ipod-db serializer (signs hash58)
    /// 3. Deletes stale Play Counts / iTunesStats so the firmware regenerates
    fn flush(&mut self) -> Result<(), String> {
        self.db.reassign_track_ids();
        ipod_db::itunesdb_write::write_to_disk(&self.db, self.firewire_id.as_ref())
            .map_err(|e| format!("Failed to write iTunesDB: {}", e))?;

        let itunes_dir = self.mount().join("iPod_Control").join("iTunes");
        for stale in &["Play Counts", "iTunesStats", "PlayCounts.plist"] {
            let p = itunes_dir.join(stale);
            if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
        Ok(())
    }

    /// Mount point path.
    fn mount(&self) -> &std::path::Path {
        &self.db.mount_point
    }

    /// Delete the track with the given dbid: remove its file from disk,
    /// drop it from the database, and flush. Used by `rm` and `rm_by_id`.
    fn remove_track_by_dbid(&mut self, dbid: u64) -> Result<(), String> {
        let track = self
            .db
            .find_track(dbid)
            .ok_or_else(|| format!("Track with dbid 0x{:016x} not in database", dbid))?
            .clone();
        let real = ipod_db::fs::real_path(self.mount(), &track.ipod_path);
        if real.exists() {
            let _ = std::fs::remove_file(&real);
        }
        self.db.remove_track(dbid);
        self.flush()
    }
}

impl DeviceSession for IpodSession {
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
        // Convert iPod colon-path or slash-path to real filesystem path.
        let real = if path.starts_with(':') {
            ipod_db::fs::real_path(self.mount(), path)
        } else {
            // Slash-delimited path (e.g. "/Music") — map to iPod_Control/Music.
            let stripped = path.strip_prefix('/').unwrap_or(path);
            self.mount().join("iPod_Control").join(stripped)
        };

        let entries = std::fs::read_dir(&real)
            .map_err(|e| format!("Cannot list {}: {}", real.display(), e))?;

        let mut result = Vec::new();
        for entry in entries.flatten() {
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let name = entry.file_name().to_string_lossy().into_owned();

            result.push(DeviceEntry {
                object_id: 0, // No MTP object IDs for filesystem
                storage_id: 0,
                format: if is_dir {
                    "Association".to_string()
                } else {
                    name.rsplit('.').next().unwrap_or("unknown").to_uppercase()
                },
                size,
                name,
                ..Default::default()
            });
        }

        Ok(result)
    }

    fn import_track(
        &mut self,
        local_path: &str,
        _meta: Option<&super::TrackMeta>,
    ) -> Result<u64, String> {
        let src = PathBuf::from(local_path);
        if !src.exists() {
            return Err(format!("File not found: {}", local_path));
        }

        let file_size = std::fs::metadata(&src)
            .map_err(|e| format!("Cannot stat {}: {}", local_path, e))?
            .len() as u32;

        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp3")
            .to_lowercase();

        // Read metadata via lofty.
        let tag_file = lofty::read_from_path(&src).ok();
        let tag: Option<&Tag> = tag_file.as_ref().and_then(|f| f.primary_tag());

        let fallback_title: String = src.file_stem().unwrap_or_default().to_string_lossy().into();
        let title = tag
            .and_then(|t| t.title().map(|s| s.to_string()))
            .unwrap_or(fallback_title);
        let artist = tag
            .and_then(|t| t.artist().map(|s| s.to_string()))
            .unwrap_or_else(|| "Unknown Artist".into());
        let album = tag
            .and_then(|t| t.album().map(|s| s.to_string()))
            .unwrap_or_else(|| "Unknown Album".into());
        let genre = tag.and_then(|t| t.genre().map(|s| s.to_string()));
        let track_number = tag.and_then(|t| t.track().map(|n| n as u16));
        let total_tracks = tag.and_then(|t| t.track_total().map(|n| n as u16));
        let disc_number = tag.and_then(|t| t.disk().map(|n| n as u16));
        let total_discs = tag.and_then(|t| t.disk_total().map(|n| n as u16));
        let year = tag.and_then(|t| t.year().map(|y| y as u16));
        let album_artist =
            tag.and_then(|t| t.get_string(&ItemKey::AlbumArtist).map(|s| s.to_string()));

        let duration_ms = tag_file
            .as_ref()
            .map(|f| f.properties().duration().as_millis() as u32)
            .unwrap_or(0);
        let bitrate = tag_file
            .as_ref()
            .and_then(|f| f.properties().audio_bitrate())
            .map(|b| b as u16);
        let sample_rate = tag_file
            .as_ref()
            .and_then(|f| f.properties().sample_rate())
            .map(|sr| sr as u16);

        let filetype: u32 = match ext.as_str() {
            "mp3" => 0x4d503320,         // "MP3 "
            "m4a" | "aac" => 0x4d344120, // "M4A "
            "wav" => 0x57415620,         // "WAV "
            _ => 0x4d503320,             // default MP3
        };

        // Detect ALAC vs AAC for M4A files — the iPod firmware uses the filetype
        // string (mhod type 6) to select the decoder.
        let filetype_string: Option<String> = if ext == "m4a" || ext == "aac" {
            // Check the actual codec via lofty's FileType.
            let is_alac = tag_file
                .as_ref()
                .and_then(|f| {
                    // lofty reports codec via the file properties. For ALAC in M4A,
                    // the audio_bitrate is typically > 500 kbps (lossless), while
                    // AAC is typically < 400 kbps. But this is fragile.
                    // More reliable: check if the file contains ALAC codec via ffprobe
                    // fallback, or just check the bitrate heuristic for now.
                    f.properties().audio_bitrate().map(|br| br > 500)
                })
                .unwrap_or(false);
            if is_alac {
                Some("Apple Lossless audio file".into())
            } else {
                Some("AAC audio file".into())
            }
        } else {
            None
        };

        // Copy file to iPod F-dir.
        ipod_db::fs::ensure_f_dirs(self.mount())
            .map_err(|e| format!("Failed to create F-dirs: {}", e))?;
        let f_dir = ipod_db::fs::pick_f_dir(self.mount())
            .map_err(|e| format!("Failed to pick F-dir: {}", e))?;

        // Generate a unique filename using a hash. We don't have a dbid yet
        // (it's assigned by add_track), so use a timestamp-based seed.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let hash_name = ipod_db::fs::hash_filename(local_path, seed);
        let ipod_path = ipod_db::fs::ipod_path(f_dir, &hash_name);
        let real_dest = ipod_db::fs::real_path(self.mount(), &ipod_path);

        std::fs::copy(&src, &real_dest).map_err(|e| format!("Failed to copy to iPod: {}", e))?;

        // Build track and add to database.
        let mut track = ipod_db::IpodTrack::default();
        track.title = title;
        track.artist = artist;
        track.album = album;
        track.album_artist = album_artist;
        track.genre = genre;
        track.track_number = track_number;
        track.total_tracks = total_tracks;
        track.disc_number = disc_number;
        track.total_discs = total_discs;
        track.total_time_ms = if duration_ms > 0 {
            Some(duration_ms)
        } else {
            None
        };
        track.year = year;
        track.file_size = file_size;
        track.bitrate = bitrate;
        track.sample_rate = sample_rate;
        track.ipod_path = ipod_path;
        track.filetype = filetype;
        track.filetype_string = filetype_string;

        let dbid = self.db.add_track(track);

        // Extract album art and set on the database.
        if let Some(ref tf) = tag_file {
            if let Some(tag) = tf.primary_tag() {
                let pictures = tag.pictures();
                if let Some(pic) = pictures.first() {
                    // Initialize artwork store if not already set.
                    if self.db.artwork_store.is_none() {
                        self.db
                            .init_artwork(ipod_db::artwork::model_specs_classic());
                    }
                    // Best-effort — don't fail the import if artwork fails.
                    let _ = self.db.set_track_artwork(dbid, pic.data());
                }
            }
        }

        // Flush database to disk.
        self.flush()?;

        Ok(dbid)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), String> {
        // The path can arrive in several shapes:
        //   * colon-separated iPod path: `:iPod_Control:Music:F00:hash.mp3`
        //   * slash-prefixed filesystem path: `/iPod_Control/Music/F00/hash.mp3`
        //   * display path from collect_all_tracks: `Artist/Album/Title.ext`
        //   * `/Music/Artist/Album/Title.ext` (TUI prefixes with `/Music/`)
        let needle = device_path.trim_start_matches('/');
        let needle = needle.strip_prefix("Music/").unwrap_or(needle);
        let dbid = self
            .db
            .tracks
            .iter()
            .find(|t| {
                if t.ipod_path == device_path {
                    return true;
                }
                let colon = t.ipod_path.trim_start_matches(':');
                let slash = colon.replace(':', "/");
                if slash == device_path.trim_start_matches('/') {
                    return true;
                }
                let ext = t.ipod_path.rsplit('.').next().unwrap_or("mp3");
                let safe_title = t.title.replace('/', "_");
                let display = format!("{}/{}/{}.{}", t.artist, t.album, safe_title, ext);
                display == needle
            })
            .map(|t| t.dbid);

        let Some(dbid) = dbid else {
            return Err(format!("Track not found: {}", device_path));
        };
        self.remove_track_by_dbid(dbid)
    }

    fn rm_by_id(&mut self, object_id: u32) -> Result<(), String> {
        // The TUI truncates our u64 dbid to u32 when calling rm_by_id.
        // Match on the low 32 bits.
        let dbid = self
            .db
            .tracks
            .iter()
            .find(|t| (t.dbid as u32) == object_id)
            .map(|t| t.dbid)
            .ok_or_else(|| format!("No track with low-32 dbid = 0x{:x}", object_id))?;
        self.remove_track_by_dbid(dbid)
    }

    fn cleanup_empty_folders(&mut self) -> Result<usize, String> {
        let music_dir = self.mount().join("iPod_Control").join("Music");
        let mut removed = 0;

        for i in 0..50 {
            let f_dir = music_dir.join(format!("F{i:02}"));
            if !f_dir.is_dir() {
                continue;
            }
            let is_empty = std::fs::read_dir(&f_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true);
            if is_empty {
                let _ = std::fs::remove_dir(&f_dir);
                removed += 1;
            }
        }

        Ok(removed)
    }

    fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
        let mount = self.mount().to_path_buf();

        // Use statvfs-style info. On Unix, we can get this via std::fs metadata
        // of the mount point, but Rust stdlib doesn't expose free space directly.
        // Use the `fs2` approach or shell out to `df`.
        let output = std::process::Command::new("df")
            .args(["-k", &mount.to_string_lossy()])
            .output()
            .map_err(|e| format!("Failed to run df: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse df output: second line has columns
        // Filesystem 1024-blocks Used Available Capacity ...
        let line = stdout.lines().nth(1).ok_or("df returned no data")?;
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            return Err("Unexpected df output format".into());
        }

        let total_kb: u64 = cols[1].parse().unwrap_or(0);
        let available_kb: u64 = cols[3].parse().unwrap_or(0);
        let total = total_kb * 1024;
        let free = available_kb * 1024;

        Ok((total, free))
    }

    fn collect_all_tracks(&mut self, _path: &str) -> Result<Vec<DeviceEntry>, String> {
        // Build "Artist/Album/Title.ext" — the TUI's build_device_index splits
        // on '/' into (artist, album, filename) and strips the final extension
        // for display. Using the track title (not the hashed on-device filename)
        // gives the browse UI readable labels.
        let entries: Vec<DeviceEntry> = self
            .db
            .tracks
            .iter()
            .map(|t| {
                let ext = t.ipod_path.rsplit('.').next().unwrap_or("mp3");
                let safe_title = t.title.replace('/', "_"); // '/' breaks the splitn
                let display_path = format!("{}/{}/{}.{}", t.artist, t.album, safe_title, ext);

                DeviceEntry {
                    object_id: t.dbid,
                    storage_id: 0,
                    format: match t.filetype {
                        0x4d503320 => "MP3".to_string(),
                        0x4d344120 => "M4A".to_string(),
                        0x57415620 => "WAV".to_string(),
                        0x574d4120 => "WMA".to_string(),
                        _ => "MP3".to_string(),
                    },
                    size: t.file_size as u64,
                    name: display_path,
                    track_number: t.track_number.map(|n| n as u32),
                    disc_number: t.disc_number.map(|n| n as u32),
                    // The iPod stores these as plain `u32` and uses `0` as the
                    // sentinel for "never played" / "never skipped"; lift `0`
                    // to `None` so the UI can render an em-dash instead of a
                    // misleading-looking literal zero.
                    play_count: if t.play_count > 0 {
                        Some(t.play_count)
                    } else {
                        None
                    },
                    last_played: if t.last_played > 0 {
                        Some(u64::from(t.last_played))
                    } else {
                        None
                    },
                    skip_count: if t.skip_count > 0 {
                        Some(t.skip_count)
                    } else {
                        None
                    },
                    // mhit +30 is a 0..=100 byte (5-star × 20). `0` is the
                    // "unrated" sentinel — lift to `None` so the UI shows
                    // an em-dash for unrated tracks rather than a literal
                    // zero.
                    rating: if t.rating > 0 {
                        Some(t.rating as u16)
                    } else {
                        None
                    },
                }
            })
            .collect();

        Ok(entries)
    }

    fn save_sync_progress(&mut self) {
        // Flush the database as the "save" operation.
        // Errors are silently ignored (same pattern as Zune backend).
        let _ = self.flush();
    }

    fn import_playlist(
        &mut self,
        name: &str,
        track_keys: &[(String, String, String)],
    ) -> Result<super::PlaylistImportSummary, String> {
        let summary = upsert_playlist(&mut self.db, name, track_keys)?;
        self.flush()?;
        Ok(summary)
    }
}

/// Resolve `(artist, album, title)` tuples to iTunesDB dbids and either
/// replace the existing playlist with the same name or append a new one.
/// Pure DB mutation — no I/O — so the unit tests can exercise it without a
/// writable iPod mount. Tuples that don't match are skipped (counted in the
/// returned summary). The master playlist (always at `db.playlists[0]` per
/// iPod firmware invariants) is left untouched — user playlists live at
/// indices >= 1.
fn upsert_playlist(
    db: &mut ipod_db::IpodDatabase,
    name: &str,
    track_keys: &[(String, String, String)],
) -> Result<super::PlaylistImportSummary, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Playlist name cannot be empty".into());
    }
    if trimmed.eq_ignore_ascii_case("Library") || trimmed.eq_ignore_ascii_case("Master") {
        // The master playlist owns these names by convention; reject so
        // callers can't accidentally clobber it.
        return Err(format!("\"{trimmed}\" is reserved for the master playlist"));
    }

    // Resolve every (artist, album, title) tuple to an iPod dbid. Match
    // case-insensitively because library tag casing varies (e.g.
    // "the beatles" vs. "The Beatles") and the iPod is case-sensitive on
    // disk paths but not on display fields.
    let mut resolved_dbids: Vec<u64> = Vec::with_capacity(track_keys.len());
    let mut skipped = 0usize;
    for (artist, album, title) in track_keys {
        let found = db.tracks.iter().find(|t| {
            t.title.eq_ignore_ascii_case(title)
                && t.artist.eq_ignore_ascii_case(artist)
                && t.album.eq_ignore_ascii_case(album)
        });
        match found {
            Some(t) => {
                if !resolved_dbids.contains(&t.dbid) {
                    resolved_dbids.push(t.dbid);
                }
            }
            None => skipped += 1,
        }
    }

    // Replace by-name (case-insensitive — track resolution above is also
    // case-insensitive, and the user shouldn't end up with two playlists
    // that differ only by capitalisation). Skip index 0 (the master playlist).
    let position = db
        .playlists
        .iter()
        .skip(1)
        .position(|p| p.name.eq_ignore_ascii_case(trimmed))
        .map(|i| i + 1);

    let new_playlist = ipod_db::IpodPlaylist {
        name: trimmed.to_string(),
        is_master: false,
        track_ids: resolved_dbids.clone(),
    };
    let replaced = match position {
        Some(idx) => {
            db.playlists[idx] = new_playlist;
            true
        }
        None => {
            db.playlists.push(new_playlist);
            false
        }
    };

    Ok(super::PlaylistImportSummary {
        resolved: resolved_dbids.len(),
        skipped,
        replaced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(rating: u8, play_count: u32, last_played: u32, skip_count: u32) -> ipod_db::IpodTrack {
        // `raw_mhit_header` is `pub(crate)` to ipod-db, so `..Default::default()`
        // doesn't work from outside the crate. Build via Default + mutate.
        let mut t = ipod_db::IpodTrack::default();
        t.title = "T".into();
        t.artist = "A".into();
        t.album = "B".into();
        t.ipod_path = ":iPod_Control:Music:F00:t.mp3".into();
        t.filetype = 0x4d503320;
        t.file_size = 100;
        t.rating = rating;
        t.play_count = play_count;
        t.last_played = last_played;
        t.skip_count = skip_count;
        t
    }

    /// `collect_all_tracks` lifts the mhit play/skip/rating bytes into
    /// `DeviceEntry` fields, mapping `0` to `None` so the UI can render
    /// "never played" / "unrated" as an em-dash rather than a literal zero.
    #[test]
    fn collect_all_tracks_maps_zero_to_none() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(track(0, 0, 0, 0));
        let mut session = IpodSession::new(db, None);
        let entries = session.collect_all_tracks("").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].play_count, None);
        assert_eq!(entries[0].last_played, None);
        assert_eq!(entries[0].skip_count, None);
        assert_eq!(entries[0].rating, None);
    }

    #[test]
    fn collect_all_tracks_surfaces_nonzero_play_state() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(track(80, 7, 3_900_000_000, 2));
        let mut session = IpodSession::new(db, None);
        let entries = session.collect_all_tracks("").unwrap();
        assert_eq!(entries[0].play_count, Some(7));
        assert_eq!(entries[0].last_played, Some(3_900_000_000));
        assert_eq!(entries[0].skip_count, Some(2));
        // Rating passes through as raw 0..=100 (Zune's MTP rating uses the
        // same scale, so DeviceEntry.rating is unit-consistent across both
        // backends).
        assert_eq!(entries[0].rating, Some(80));
    }

    fn track_named(artist: &str, album: &str, title: &str) -> ipod_db::IpodTrack {
        let mut t = ipod_db::IpodTrack::default();
        t.title = title.into();
        t.artist = artist.into();
        t.album = album.into();
        t.ipod_path = format!(":iPod_Control:Music:F00:{}.mp3", title);
        t.filetype = 0x4d503320;
        t.file_size = 100;
        t
    }

    #[test]
    fn upsert_playlist_appends_new_with_resolved_dbids() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));
        let dbid_b = db.add_track(track_named("B", "Y", "T2"));

        let summary = upsert_playlist(
            &mut db,
            "Faves",
            &[
                ("a".into(), "x".into(), "t1".into()),
                ("B".into(), "Y".into(), "T2".into()),
            ],
        )
        .unwrap();

        assert_eq!(summary.resolved, 2);
        assert_eq!(summary.skipped, 0);
        assert!(!summary.replaced);

        // Master at index 0 is intact, user playlist at index 1.
        assert!(db.playlists[0].is_master);
        assert_eq!(db.playlists.len(), 2);
        let pl = &db.playlists[1];
        assert_eq!(pl.name, "Faves");
        assert!(!pl.is_master);
        assert_eq!(pl.track_ids, vec![dbid_a, dbid_b]);
    }

    #[test]
    fn upsert_playlist_replaces_existing_by_name() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));
        let dbid_b = db.add_track(track_named("B", "Y", "T2"));

        upsert_playlist(&mut db, "Faves", &[("A".into(), "X".into(), "T1".into())]).unwrap();
        let summary = upsert_playlist(
            &mut db,
            "Faves",
            &[
                ("B".into(), "Y".into(), "T2".into()),
                ("A".into(), "X".into(), "T1".into()),
            ],
        )
        .unwrap();

        assert!(summary.replaced);
        assert_eq!(db.playlists.len(), 2, "no second playlist created");
        let pl = &db.playlists[1];
        assert_eq!(pl.name, "Faves");
        assert_eq!(pl.track_ids, vec![dbid_b, dbid_a], "order honored");
    }

    #[test]
    fn upsert_playlist_replaces_case_insensitively() {
        // Track resolution is case-insensitive, so the by-name replace
        // lookup must match. Otherwise the user creates a duplicate
        // playlist by varying capitalisation.
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));

        upsert_playlist(&mut db, "Workout", &[("A".into(), "X".into(), "T1".into())]).unwrap();
        let summary =
            upsert_playlist(&mut db, "workout", &[("A".into(), "X".into(), "T1".into())]).unwrap();

        assert!(
            summary.replaced,
            "case-only difference must replace, not append"
        );
        assert_eq!(
            db.playlists.len(),
            2,
            "still only master + one user playlist"
        );
        // The new name supplied by the latest call is the one stored — the
        // user's intent was the more recent capitalisation.
        assert_eq!(db.playlists[1].name, "workout");
        assert_eq!(db.playlists[1].track_ids, vec![dbid_a]);
    }

    #[test]
    fn upsert_playlist_skips_unresolvable_tuples() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));

        let summary = upsert_playlist(
            &mut db,
            "Mixed",
            &[
                ("A".into(), "X".into(), "T1".into()),
                ("Ghost".into(), "Album".into(), "Missing".into()),
            ],
        )
        .unwrap();

        assert_eq!(summary.resolved, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(db.playlists[1].track_ids, vec![dbid_a]);
    }

    #[test]
    fn upsert_playlist_dedupes_repeated_tuples() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));

        let summary = upsert_playlist(
            &mut db,
            "Dup",
            &[
                ("A".into(), "X".into(), "T1".into()),
                ("A".into(), "X".into(), "T1".into()),
            ],
        )
        .unwrap();

        assert_eq!(summary.resolved, 1, "deduped to one resolved");
        assert_eq!(db.playlists[1].track_ids, vec![dbid_a]);
    }

    #[test]
    fn upsert_playlist_rejects_empty_name() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        assert!(upsert_playlist(&mut db, "  ", &[]).is_err());
    }

    #[test]
    fn upsert_playlist_rejects_master_alias() {
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        assert!(upsert_playlist(&mut db, "Library", &[]).is_err());
        assert!(upsert_playlist(&mut db, "MASTER", &[]).is_err());
    }

    #[test]
    fn upsert_playlist_master_remains_at_index_zero_with_all_tracks() {
        // The iPod firmware requires the master playlist (index 0, is_master)
        // to enumerate every track in the library. add_track maintains this
        // automatically; this test pins the invariant against future
        // refactors of the playlist insertion path.
        let mut db = ipod_db::IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid_a = db.add_track(track_named("A", "X", "T1"));
        let dbid_b = db.add_track(track_named("B", "Y", "T2"));
        upsert_playlist(&mut db, "Faves", &[("A".into(), "X".into(), "T1".into())]).unwrap();

        let master = &db.playlists[0];
        assert!(master.is_master);
        assert!(master.track_ids.contains(&dbid_a));
        assert!(master.track_ids.contains(&dbid_b));
    }
}
