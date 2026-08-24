//! Native MTP session implementing DeviceSession.
//!
//! Uses the zune-mtp crate for direct USB communication, bypassing aft-mtp-cli.

mod album_art;
mod playcount;
mod track_cache;

use album_art::extract_album_art;
use playcount::{apply_playcounts, apply_ratings, apply_skip_counts};
use track_cache::TrackCache;

use crate::mtp::parse::DeviceEntry;
use crate::mtp::{DeviceError, DeviceSession};

use zune_mtp::container::MTP_ROOT;
use zune_mtp::proplist::*;
use zune_mtp::session::{parse_object_prop_list, ObjectInfo, PropListElement};
use zune_mtp::{MtpError, MtpSession, MtpzKeys};

use std::collections::HashMap;
use std::path::PathBuf;

/// Extension trait to convert MtpError results to [`DeviceError`] results at
/// the DeviceSession boundary. Classification happens here: the transport
/// already marked fatal USB failures (`MtpError::UsbFatal`) and the device
/// reported unsupported operations (`0x2005`), so no string sniffing is
/// needed.
trait MtpResultExt<T> {
    fn mtp_err(self) -> Result<T, DeviceError>;
}

impl<T> MtpResultExt<T> for Result<T, MtpError> {
    fn mtp_err(self) -> Result<T, DeviceError> {
        self.map_err(|e| {
            if e.is_device_gone() {
                DeviceError::DeviceGone(e.to_string())
            } else if e.is_operation_not_supported() {
                DeviceError::Unsupported(e.to_string())
            } else {
                DeviceError::Other(e.to_string())
            }
        })
    }
}

/// Microsoft vendor ID.
const MICROSOFT_VENDOR_ID: u16 = 0x045e;

/// MTP object format codes.
const ASSOCIATION_FORMAT: u16 = 0x3001;
const FORMAT_MP3: u16 = 0x3009;
const FORMAT_WMA: u16 = 0xB901;
const FORMAT_AAC: u16 = 0xB903;
const FORMAT_ARTIST: u16 = 0xB218;
const FORMAT_ABSTRACT_AUDIO_ALBUM: u16 = 0xBA03;
/// MTP format code for `AbstractAudioVideoPlaylist` — the Zune's playlist
/// object format. See `docs/zune-playlist-research.md` §1.
const FORMAT_ABSTRACT_AUDIO_VIDEO_PLAYLIST: u16 = 0xBA05;
const FORMAT_EXIF_JPEG: u16 = 0x3801;
const FORMAT_WMV: u16 = 0xB981;

/// Cached artist info.
struct ArtistInfo {
    id: u32,
    music_folder_id: u32,
}

/// Cached album info.
struct AlbumInfo {
    id: u32,
    music_folder_id: u32,
}

/// Device capability flags.
struct DeviceCaps {
    artist_supported: bool,
    #[allow(dead_code)]
    album_date_supported: bool,
    album_cover_supported: bool,
}

/// Drop `device_path`'s entries from a [`DeviceLibrary`]. `/Music/{artist}`
/// drops the artist and every album under it; `/Music/{artist}/{album}` (or
/// deeper) drops just that album. Returns whether anything was removed so
/// callers can skip a pointless cache re-save. Pure so it can be tested
/// without a live device session.
fn invalidate_device_library(lib: &mut DeviceLibrary, device_path: &str) -> bool {
    let parts: Vec<&str> = device_path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.first().copied() != Some("Music") || parts.len() < 2 {
        return false;
    }
    let artist = parts[1].to_string();
    match parts.len() {
        2 => {
            let albums_before = lib.albums.len();
            lib.albums.retain(|(a, _), _| a != &artist);
            let artist_removed = lib.artists.remove(&artist).is_some();
            artist_removed || lib.albums.len() != albums_before
        }
        _ => {
            let album = parts[2].to_string();
            lib.albums.remove(&(artist, album)).is_some()
        }
    }
}

/// Device library state — folders and cached artist/album mappings.
struct DeviceLibrary {
    music_folder: u32,
    artists_folder: u32,
    albums_folder: u32,
    caps: DeviceCaps,
    artists: HashMap<String, ArtistInfo>,
    albums: HashMap<(String, String), AlbumInfo>,
}

impl DeviceLibrary {
    /// Serialize to a simple text format for disk caching.
    fn serialize(&self) -> String {
        let mut s = String::new();
        // Header: folder handles and caps
        s.push_str(&format!(
            "HDR\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.music_folder,
            self.artists_folder,
            self.albums_folder,
            self.caps.artist_supported as u8,
            self.caps.album_date_supported as u8,
            self.caps.album_cover_supported as u8,
        ));
        for (name, info) in &self.artists {
            s.push_str(&format!(
                "ART\t{}\t{}\t{}\n",
                name.replace('\t', " "),
                info.id,
                info.music_folder_id
            ));
        }
        for ((artist, album), info) in &self.albums {
            s.push_str(&format!(
                "ALB\t{}\t{}\t{}\t{}\n",
                artist.replace('\t', " "),
                album.replace('\t', " "),
                info.id,
                info.music_folder_id
            ));
        }
        s
    }

    /// Deserialize from the text format. Returns None on any parse failure.
    fn deserialize(data: &str) -> Option<Self> {
        let mut music_folder = 0u32;
        let mut artists_folder = 0u32;
        let mut albums_folder = 0u32;
        let mut caps = DeviceCaps {
            artist_supported: false,
            album_date_supported: true,
            album_cover_supported: true,
        };
        let mut artists = HashMap::new();
        let mut albums = HashMap::new();
        let mut has_header = false;

        for line in data.lines() {
            let parts: Vec<&str> = line.splitn(7, '\t').collect();
            match parts.first() {
                Some(&"HDR") if parts.len() >= 7 => {
                    music_folder = parts[1].parse().ok()?;
                    artists_folder = parts[2].parse().ok()?;
                    albums_folder = parts[3].parse().ok()?;
                    caps.artist_supported = parts[4] == "1";
                    caps.album_date_supported = parts[5] == "1";
                    caps.album_cover_supported = parts[6] == "1";
                    has_header = true;
                }
                Some(&"ART") if parts.len() >= 4 => {
                    artists.insert(
                        parts[1].to_string(),
                        ArtistInfo {
                            id: parts[2].parse().ok()?,
                            music_folder_id: parts[3].parse().ok()?,
                        },
                    );
                }
                Some(&"ALB") if parts.len() >= 5 => {
                    albums.insert(
                        (parts[1].to_string(), parts[2].to_string()),
                        AlbumInfo {
                            id: parts[3].parse().ok()?,
                            music_folder_id: parts[4].parse().ok()?,
                        },
                    );
                }
                _ => continue,
            }
        }

        if !has_header {
            return None;
        }

        Some(DeviceLibrary {
            music_folder,
            artists_folder,
            albums_folder,
            caps,
            artists,
            albums,
        })
    }
}

/// Native MTP session using platform-specific USB transport.
pub struct NativeSession {
    session: MtpSession,
    storage_id: u32,
    product_id: u16,
    log: Option<std::sync::mpsc::Sender<String>>,
    library: Option<DeviceLibrary>,
    cache: TrackCache,
    sync_cache_serial: Option<String>,
    sync_restored: bool,
    pub firmware_version: Option<String>,
    /// Cached video entries from the last ZMDB parse.
    zmdb_video_cache: Option<Vec<DeviceEntry>>,
    /// True once an album-art write has failed in this session. When set,
    /// subsequent tracks skip art entirely — one bad JPEG can leave the MTP
    /// session too fragile to keep calling SetObjectPropValue, and each
    /// retry costs a ~45s ReadPipe timeout.
    art_disabled: bool,
    /// Session-scoped `(artist, album, title) → object_handle` map populated
    /// by `import_track` at upload time. `import_playlist` consults this
    /// first so freshly-synced tracks resolve to real handles without
    /// waiting for ZMDB to surface them. Lowercase keys for case-insensitive
    /// match against the playlist's track tuples.
    ///
    /// Why session-scoped and not persisted: ZMDB and the on-disk track
    /// cache normalise to "Artist/Album/Title", but the on-disk cache's
    /// post-`import_track` append uses the audio file's *filename* as the
    /// third segment. That mismatch means cache restoration via name
    /// matching can't repair freshly-uploaded handles after a reconnect.
    /// Caching directly on the import tuple sidesteps the whole name-format
    /// dance for the lifetime of the connection.
    recent_imports: HashMap<(String, String, String), u32>,
}

impl NativeSession {
    /// Access the device library state, returning an error if not yet initialized.
    fn lib(&self) -> Result<&DeviceLibrary, DeviceError> {
        self.library
            .as_ref()
            .ok_or_else(|| DeviceError::Other("Device library not initialized".into()))
    }

    /// `(major, minor)` version parsed from the firmware string, if readable.
    /// The Zune reports firmware like `"01.04.00485.00-00425"`; we only care
    /// about the first two segments.
    fn firmware_version_tuple(&self) -> Option<(u16, u16)> {
        parse_firmware_version(self.firmware_version.as_deref()?)
    }

    /// True if the device firmware is recent enough to implement the Zune
    /// metadata DB, acquired-items count, and sync-progress vendor ops.
    /// These were added in firmware 3.0 (Zune software 3.0, Sep 2008).
    /// Returns `true` when the version can't be parsed so we still attempt
    /// the ops — some code paths rely on the device's `DeviceRejected`
    /// reply to tell us to fall back.
    fn supports_modern_vendor_ops(&self) -> bool {
        match self.firmware_version_tuple() {
            Some((major, _)) => major >= 3,
            None => true,
        }
    }

    /// Set a log channel for progress messages.
    pub fn set_log_sender(&mut self, tx: std::sync::mpsc::Sender<String>) {
        self.log = Some(tx);
    }

    /// Set the device serial number (used to key caches per-device).
    pub fn set_serial(&mut self, serial: Option<String>) {
        self.cache = TrackCache::new(serial.clone());
        self.sync_cache_serial = serial;
    }

    fn log_msg(&self, msg: &str) {
        if let Some(ref tx) = self.log {
            let _ = tx.send(msg.to_string());
        }
    }

    /// Query storage info. Returns (total_bytes, free_bytes).
    pub fn get_storage_info(&mut self) -> Result<(u64, u64), DeviceError> {
        self.session.get_storage_info(self.storage_id).mtp_err()
    }

    /// Query the number of items the device acquired on its own
    /// (podcast downloads, Zune-to-Zune sharing).
    ///
    /// Returns `Ok(None)` when the device reports the vendor op as unsupported
    /// (older firmware). Callers should treat that as "feature unavailable"
    /// rather than a hard error.
    pub fn get_acquired_items_count(&mut self) -> Result<Option<u32>, DeviceError> {
        if !self.supports_modern_vendor_ops() {
            return Ok(None);
        }
        match self.session.get_acquired_items_count() {
            Ok(count) => Ok(Some(count)),
            Err(e) if e.is_operation_not_supported() => Ok(None),
            Err(e) => Err(e).mtp_err(),
        }
    }

    /// Query device sync progress (vendor op 0x922f). Returns raw payload bytes,
    /// or `Ok(None)` if the device doesn't support the query.
    pub fn get_sync_progress(&mut self) -> Result<Option<Vec<u8>>, DeviceError> {
        if !self.supports_modern_vendor_ops() {
            return Ok(None);
        }
        match self.session.get_sync_progress() {
            Ok(raw) => Ok(Some(raw)),
            Err(e) if e.is_operation_not_supported() => Ok(None),
            Err(e) => Err(e).mtp_err(),
        }
    }

    /// Open a native MTP session to the Zune.
    /// Performs device detection, MTP session open, and MTPZ authentication.
    /// The `log` callback receives diagnostic messages for each step.
    pub fn open(product_id: u16, log: &dyn Fn(&str)) -> Result<Self, DeviceError> {
        log("MTP: Opening USB device...");
        let mut session = MtpSession::open(MICROSOFT_VENDOR_ID, product_id)
            .map_err(|e| format!("USB open failed: {e}"))?;
        log("MTP: USB device opened, MTP session started");

        log("MTP: Loading MTPZ keys from ~/.mtpz-data...");
        let keys = MtpzKeys::load_default().map_err(|e| format!("MTPZ keys failed: {e}"))?;
        log("MTP: Keys loaded, starting MTPZ handshake...");

        zune_mtp::mtpz::authenticate(&mut session, &keys, log)
            .map_err(|e| format!("MTPZ handshake failed: {e}"))?;
        log("MTP: MTPZ handshake complete");

        // Try to read firmware version via GetDeviceInfo (post-handshake),
        // then fall back to MTP property 0xD404.
        let firmware_version = session
            .get_device_version()
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                session
                    .get_device_prop_string(0xD404)
                    .ok()
                    .filter(|v| !v.is_empty())
            });
        if let Some(ref v) = firmware_version {
            log(&format!("MTP: Firmware version: {}", v));
            if let Some((major, _)) = parse_firmware_version(v) {
                if major < 3 {
                    log(&format!(
                        "MTP: Firmware {v} predates Zune software 3.0 (Sep 2008); \
                         ZMDB bulk-query, acquired-items, and sync-progress ops \
                         will be skipped. A firmware upgrade to 3.x restores them."
                    ));
                }
            }
        }

        log("MTP: Querying storage...");
        let storage_ids = session
            .get_storage_ids()
            .map_err(|e| format!("MTP get storage failed: {e}"))?;
        let storage_id = storage_ids
            .first()
            .copied()
            .ok_or("MTP: No storage found on device")?;
        log(&format!("MTP: Using storage {}", storage_id));

        Ok(NativeSession {
            session,
            storage_id,
            product_id,
            log: None,
            library: None,
            cache: TrackCache::new(None),
            sync_cache_serial: None,
            sync_restored: false,
            firmware_version,
            zmdb_video_cache: None,
            art_disabled: false,
            recent_imports: HashMap::new(),
        })
    }

    /// Recursively list all objects under a given parent handle, building
    /// the relative path prefix for each entry.
    fn list_recursive(
        &mut self,
        parent: u32,
        prefix: &str,
        entries: &mut Vec<DeviceEntry>,
    ) -> Result<(), DeviceError> {
        let handles = self
            .session
            .get_object_handles(self.storage_id, parent)
            .mtp_err()?;

        for handle in handles {
            let info = match self.session.get_object_info(handle) {
                Ok(info) => info,
                Err(_) => continue,
            };

            let full_name = if prefix.is_empty() {
                info.filename.clone()
            } else {
                format!("{}/{}", prefix, info.filename)
            };

            let format_str = if info.object_format == ASSOCIATION_FORMAT {
                "Association".to_string()
            } else {
                format_name(info.object_format)
            };

            if info.object_format == ASSOCIATION_FORMAT {
                // Recurse into directories; only add the directory entry
                // if it contains children (skip empty leftover folders).
                let before = entries.len();
                self.list_recursive(handle, &full_name, entries)?;
                let has_children = entries.len() > before;
                if has_children && prefix.is_empty() {
                    self.log_msg(&format!("Scanning: {}", info.filename));
                }
                if has_children {
                    entries.push(DeviceEntry {
                        object_id: handle as u64,
                        storage_id: info.storage_id as u64,
                        format: format_str,
                        size: info.compressed_size as u64,
                        name: full_name.clone(),
                        ..Default::default()
                    });
                }
            } else {
                entries.push(DeviceEntry {
                    object_id: handle as u64,
                    storage_id: info.storage_id as u64,
                    format: format_str,
                    size: info.compressed_size as u64,
                    name: full_name.clone(),
                    ..Default::default()
                });
            }
        }
        Ok(())
    }

    /// Find an object handle by name under a parent.
    fn find_object(&mut self, parent: u32, name: &str) -> Result<Option<u32>, DeviceError> {
        let handles = self
            .session
            .get_object_handles(self.storage_id, parent)
            .mtp_err()?;
        let mut stem_match = None;
        for handle in handles {
            if let Ok(info) = self.session.get_object_info(handle) {
                if info.filename == name {
                    return Ok(Some(handle));
                }
                // Fall back to fuzzy match for ZMDB titles which lack
                // file extensions and track number prefixes.
                if stem_match.is_none() {
                    let stem = info
                        .filename
                        .rfind('.')
                        .map(|pos| &info.filename[..pos])
                        .unwrap_or(&info.filename);
                    if stem == name || stem.ends_with(&format!(" {}", name)) {
                        stem_match = Some(handle);
                    }
                }
            }
        }
        Ok(stem_match)
    }

    /// Find an existing folder by name under `parent`, or create it.
    fn find_or_create_folder(&mut self, parent: u32, name: &str) -> Result<u32, DeviceError> {
        if let Some(h) = self.find_object(parent, name)? {
            return Ok(h);
        }
        self.log_msg(&format!("Creating folder: {name}"));
        let props = PropListBuilder::new()
            .add_string(PROP_OBJECT_FILENAME, name)
            .build();
        let (_, _, id) = self
            .session
            .send_object_prop_list(self.storage_id, parent, ASSOCIATION_FORMAT, 0, &props)
            .mtp_err()?;
        let _ = self.session.send_object(&[]);
        Ok(id)
    }

    /// Resolve a path like "/Music/Artist/Album" to an object handle.
    fn resolve_path(&mut self, path: &str) -> Result<u32, DeviceError> {
        let parts: Vec<&str> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current = MTP_ROOT;
        for part in parts {
            match self.find_object(current, part)? {
                Some(handle) => current = handle,
                None => return Err(format!("Path not found: {}", path).into()),
            }
        }
        Ok(current)
    }

    /// Enrich `tracks` with `play_count` (from `0xDC91 UseCount`),
    /// `rating` (from `0xDC8A Rating`), and `skip_count` (from `0xDC92
    /// SkipCount`) using bulk `GetObjectPropList` queries — one round-trip
    /// per property for the whole library, not one per track. Tracks whose
    /// `object_id` is `0` (ZMDB entries before any cache merge has restored
    /// the handle) are skipped silently.
    ///
    /// All three queries are best-effort and independent: if the device
    /// rejects one, we log it and proceed with the others. UseCount and
    /// Rating are confirmed to work on Zune v1.4 firmware
    /// `01.04.00485.00-00425` even though they're not listed in
    /// `GetObjectPropsSupported(0x3009)`; SkipCount is in the same
    /// neighbourhood and may behave the same way, but if v1.4 rejects it
    /// the merge logic at the App layer just sees `delta = 0` for the skip
    /// dimension — no harm done.
    ///
    /// Called from each track-collect path (ZMDB fast path, cache-hit
    /// refresh, slow fallback); callers may invoke it multiple times per
    /// session and `enrich_one_prop`'s "unavailable" log fires per call —
    /// not rate-limited.
    fn enrich_with_playcounts(&mut self, tracks: &mut [DeviceEntry]) {
        // Avoid needless round-trips when nothing in the list could be
        // matched — e.g. ZMDB on a fresh connect with no prior cache.
        if !tracks.iter().any(|t| t.object_id != 0) {
            return;
        }
        self.enrich_one_prop(tracks, PROP_USE_COUNT, "playcounts", apply_playcounts);
        self.enrich_one_prop(tracks, PROP_RATING, "ratings", apply_ratings);
        self.enrich_one_prop(tracks, PROP_SKIP_COUNT, "skip counts", apply_skip_counts);
    }

    /// Issue a single bulk `GetObjectPropList` for `prop` across all MP3
    /// objects, parse the response, and apply the results onto `tracks`
    /// using `apply`. Logs how many entries were populated, or a one-liner
    /// if the device rejected the op. Generic over the apply function so
    /// the playcount and rating paths share the wire/log machinery.
    fn enrich_one_prop<F>(&mut self, tracks: &mut [DeviceEntry], prop: u16, label: &str, apply: F)
    where
        F: FnOnce(&mut [DeviceEntry], &[PropListElement]),
    {
        let raw = match self.session.get_object_prop_list(
            0xFFFFFFFF,
            FORMAT_MP3 as u32,
            prop as u32,
            0,
            0,
        ) {
            Ok(r) => r,
            Err(e) => {
                self.log_msg(&format!("{label} enrichment unavailable: {e}"));
                return;
            }
        };
        let elements = parse_object_prop_list(&raw);
        if elements.is_empty() {
            return;
        }
        // Counter callback handles the "before/after diff" so we don't need
        // to know which field on DeviceEntry the apply touched.
        let count_set = |ts: &[DeviceEntry], p: u16| -> usize {
            ts.iter()
                .filter(|t| match p {
                    PROP_USE_COUNT => t.play_count.is_some(),
                    PROP_RATING => t.rating.is_some(),
                    PROP_SKIP_COUNT => t.skip_count.is_some(),
                    _ => false,
                })
                .count()
        };
        let before = count_set(tracks, prop);
        apply(tracks, &elements);
        let added = count_set(tracks, prop) - before;
        if added > 0 {
            self.log_msg(&format!("Loaded {label} for {added} tracks"));
        }
    }

    /// Try to load the device library via the ZMDB vendor operation.
    /// Returns DeviceEntry values with synthesized paths matching the device filesystem.
    ///
    /// Short-circuits on firmware older than 3.0, where ZMDB was not yet
    /// implemented, and converts `OperationNotSupported (0x2005)` from newer
    /// devices into a concise error string so the caller's fallback log reads
    /// cleanly.
    fn try_zmdb(&mut self) -> Result<Vec<DeviceEntry>, DeviceError> {
        if !self.supports_modern_vendor_ops() {
            let v = self.firmware_version.as_deref().unwrap_or("unknown");
            return Err(DeviceError::Unsupported(format!(
                "firmware {v} predates ZMDB (added in firmware 3.0)"
            )));
        }
        let raw = self.session.get_zmdb(1).mtp_err().map_err(|e| match e {
            DeviceError::Unsupported(_) => {
                DeviceError::Unsupported("device does not support ZMDB bulk query".to_string())
            }
            other => other,
        })?;
        let zmdb = crate::mtp::zmdb::Zmdb::parse(&raw)?;
        self.log_msg(&format!("ZMDB: {}", zmdb.summary()));
        self.zmdb_video_cache = Some(zmdb.to_video_entries());
        Ok(zmdb.to_device_entries())
    }

    /// Initialize the device library — find/create Music, Artists, Albums folders
    /// and scan existing artists+albums.
    fn ensure_library(&mut self) -> Result<(), DeviceError> {
        if self.library.is_some() {
            return Ok(());
        }

        // Try loading from disk cache first.
        if let Some(cached_lib) = self.load_valid_library_cache() {
            self.library = Some(cached_lib);
            return Ok(());
        }

        self.log_msg("Initializing device library...");

        // GetObjectPropsSupported (0x9806) wedges both the Zune 30 and the
        // Zune HD (returns 0x2002 on HD; leaves the session broken on 30),
        // so we don't probe — per-model knowns are baked in here instead.
        //
        // Zune HD (pid=0x063e) requires tracks to reference an AbstractAudio
        // Artist object via PROP_ARTIST_ID (0xDAB9); inline PROP_ARTIST
        // strings are overwritten by the device's auto-created "Unknown
        // Artist" object. Earlier Zunes happily display the inline string
        // and the Artist-object creation path was never exercised there.
        let artist_supported = self.product_id == 0x063e;
        let (album_date_supported, album_cover_supported) = (true, true);

        self.log_msg(&format!(
            "Caps: artist={} date={} cover={}",
            artist_supported, album_date_supported, album_cover_supported
        ));

        let (music_folder, artists_folder, albums_folder) = self.find_library_roots()?;

        let artists = self.scan_existing_artists(artist_supported, music_folder, artists_folder);
        self.log_msg(&format!("Found {} existing artists", artists.len()));

        let albums = self.scan_existing_albums(&artists, music_folder, albums_folder);
        self.log_msg(&format!("Found {} existing albums", albums.len()));

        self.library = Some(DeviceLibrary {
            music_folder,
            artists_folder,
            albums_folder,
            caps: DeviceCaps {
                artist_supported,
                album_date_supported,
                album_cover_supported,
            },
            artists,
            albums,
        });

        self.save_library_cache();
        self.log_msg("Device library ready");

        Ok(())
    }

    /// Load the on-disk library cache, rejecting entries written before
    /// the HD-specific `artist_supported` default existed — HD
    /// (pid=0x063e) requires artist_supported=true or tracks file as
    /// "Unknown Artist".
    fn load_valid_library_cache(&mut self) -> Option<DeviceLibrary> {
        let cached_lib = self.load_library_cache()?;
        let expected_artist_supported = self.product_id == 0x063e;
        if cached_lib.caps.artist_supported != expected_artist_supported {
            self.log_msg(&format!(
                "Library cache has artist_supported={}, expected {} for pid=0x{:04x} — rebuilding",
                cached_lib.caps.artist_supported, expected_artist_supported, self.product_id
            ));
            return None;
        }
        self.log_msg(&format!(
            "Loaded library cache ({} artists, {} albums)",
            cached_lib.artists.len(),
            cached_lib.albums.len()
        ));
        Some(cached_lib)
    }

    /// Find (or, for Music, create) the three library root folders.
    /// Returns `(music, artists, albums)`; Artists/Albums fall back to
    /// the Music folder on devices that don't have them.
    fn find_library_roots(&mut self) -> Result<(u32, u32, u32), DeviceError> {
        let root_handles = self
            .session
            .get_object_handles(self.storage_id, MTP_ROOT)
            .mtp_err()?;

        let mut music_folder = None;
        let mut artists_folder = None;
        let mut albums_folder = None;

        for h in &root_handles {
            if let Ok(info) = self.session.get_object_info(*h) {
                match info.filename.as_str() {
                    "Music" => music_folder = Some(*h),
                    "Artists" => artists_folder = Some(*h),
                    "Albums" => albums_folder = Some(*h),
                    _ => {}
                }
            }
        }

        let music_folder = match music_folder {
            Some(h) => h,
            None => self.find_or_create_folder(MTP_ROOT, "Music")?,
        };
        Ok((
            music_folder,
            artists_folder.unwrap_or(music_folder),
            albums_folder.unwrap_or(music_folder),
        ))
    }

    /// Scan pre-existing artists: `.art` objects under Artists/ when the
    /// device supports artist objects, otherwise Music/ subfolders.
    fn scan_existing_artists(
        &mut self,
        artist_supported: bool,
        music_folder: u32,
        artists_folder: u32,
    ) -> HashMap<String, ArtistInfo> {
        let mut artists = HashMap::new();
        if artist_supported {
            if let Ok(handles) = self
                .session
                .get_object_handles(self.storage_id, artists_folder)
            {
                for h in handles {
                    if let Ok(info) = self.session.get_object_info(h) {
                        if info.object_format == FORMAT_ARTIST {
                            let name = info.filename.trim_end_matches(".art").to_string();
                            // Find the corresponding Music/{Artist}/ folder.
                            let music_folder_id = self
                                .find_object(music_folder, &name)
                                .ok()
                                .flatten()
                                .unwrap_or(music_folder);
                            artists.insert(
                                name,
                                ArtistInfo {
                                    id: h,
                                    music_folder_id,
                                },
                            );
                        }
                    }
                }
            }
        } else {
            // No artist objects — scan Music/ subfolders as artist folders.
            if let Ok(handles) = self
                .session
                .get_object_handles(self.storage_id, music_folder)
            {
                for h in handles {
                    if let Ok(info) = self.session.get_object_info(h) {
                        if info.object_format == ASSOCIATION_FORMAT {
                            artists.insert(
                                info.filename.clone(),
                                ArtistInfo {
                                    id: h,
                                    music_folder_id: h,
                                },
                            );
                        }
                    }
                }
            }
        }
        artists
    }

    /// Scan pre-existing albums: `Artist--Album.alb` objects under Albums/.
    fn scan_existing_albums(
        &mut self,
        artists: &HashMap<String, ArtistInfo>,
        music_folder: u32,
        albums_folder: u32,
    ) -> HashMap<(String, String), AlbumInfo> {
        let mut albums = HashMap::new();
        if let Ok(handles) = self
            .session
            .get_object_handles(self.storage_id, albums_folder)
        {
            for h in handles {
                if let Ok(info) = self.session.get_object_info(h) {
                    if info.object_format == FORMAT_ABSTRACT_AUDIO_ALBUM {
                        // Filename format: "Artist--Album.alb"
                        let base = info.filename.trim_end_matches(".alb");
                        if let Some((artist_name, album_name)) = base.split_once("--") {
                            // Find the Music/{Artist}/{Album}/ folder.
                            let artist_folder = artists
                                .get(artist_name)
                                .map(|a| a.music_folder_id)
                                .unwrap_or(music_folder);
                            let album_folder_id = self
                                .find_object(artist_folder, album_name)
                                .ok()
                                .flatten()
                                .unwrap_or(artist_folder);
                            albums.insert(
                                (artist_name.to_string(), album_name.to_string()),
                                AlbumInfo {
                                    id: h,
                                    music_folder_id: album_folder_id,
                                },
                            );
                        }
                    }
                }
            }
        }
        albums
    }

    /// Find or create an artist object + music folder. Returns (artist_id, music_folder_id).
    fn find_or_create_artist_native(&mut self, name: &str) -> Result<(u32, u32), DeviceError> {
        // Check cached.
        if let Some(lib) = &self.library {
            if let Some(a) = lib.artists.get(name) {
                return Ok((a.id, a.music_folder_id));
            }
        }

        let lib = self.lib()?;
        let music_folder = lib.music_folder;
        let artists_folder = lib.artists_folder;
        let artist_supported = lib.caps.artist_supported;

        // Create music subfolder: /Music/{Artist}/
        let folder_id = self.find_or_create_folder(music_folder, name)?;

        // Create Artist MTP object if supported.
        let artist_id = if artist_supported {
            let props = PropListBuilder::new()
                .add_string(PROP_NAME, name)
                .add_string(PROP_OBJECT_FILENAME, &format!("{}.art", name))
                .build();
            match self.session.send_object_prop_list(
                self.storage_id,
                artists_folder,
                FORMAT_ARTIST,
                0,
                &props,
            ) {
                Ok((_, _, id)) => {
                    // Complete the two-phase MTP operation with empty data.
                    let _ = self.session.send_object(&[]);
                    self.log_msg(&format!("Created artist object {id} for {name:?}"));
                    id
                }
                Err(e) => {
                    // Surface the failure — silently using the folder ID as
                    // artist_id leaves tracks tied to a non-artist handle and
                    // the HD will file them under "Unknown Artist".
                    self.log_msg(&format!(
                        "Artist object create failed for {name:?}: {e} — tracks may show as Unknown Artist"
                    ));
                    folder_id
                }
            }
        } else {
            folder_id
        };

        if let Some(lib) = &mut self.library {
            lib.artists.insert(
                name.to_string(),
                ArtistInfo {
                    id: artist_id,
                    music_folder_id: folder_id,
                },
            );
        }
        self.save_library_cache();

        Ok((artist_id, folder_id))
    }

    /// Find or create an album object + music folder. Returns (album_id, music_folder_id).
    fn find_or_create_album_native(
        &mut self,
        artist_name: &str,
        album_name: &str,
        artist_id: u32,
        artist_supported: bool,
    ) -> Result<(u32, u32), DeviceError> {
        let key = (artist_name.to_string(), album_name.to_string());
        if let Some(lib) = &self.library {
            if let Some(a) = lib.albums.get(&key) {
                return Ok((a.id, a.music_folder_id));
            }
        }

        let lib = self.lib()?;
        let albums_folder = lib.albums_folder;
        let artist_folder = lib
            .artists
            .get(artist_name)
            .map(|a| a.music_folder_id)
            .unwrap_or(lib.music_folder);

        // Create music subfolder: /Music/{Artist}/{Album}/
        let folder_id = self.find_or_create_folder(artist_folder, album_name)?;

        // Create AbstractAudioAlbum MTP object.
        let mut props = PropListBuilder::new();
        if artist_supported {
            props.add_u32(PROP_ARTIST_ID, artist_id);
        } else {
            props.add_string(PROP_ARTIST, artist_name);
        }
        props.add_string(PROP_NAME, album_name);
        props.add_string(
            PROP_OBJECT_FILENAME,
            &format!("{}--{}.alb", artist_name, album_name),
        );
        let album_data = props.build();

        let album_id = match self.session.send_object_prop_list(
            self.storage_id,
            albums_folder,
            FORMAT_ABSTRACT_AUDIO_ALBUM,
            0,
            &album_data,
        ) {
            Ok((_, _, id)) => {
                // Complete the two-phase MTP operation with empty data.
                let _ = self.session.send_object(&[]);
                id
            }
            Err(_) => folder_id, // Fallback if album creation fails.
        };

        if let Some(lib) = &mut self.library {
            lib.albums.insert(
                key,
                AlbumInfo {
                    id: album_id,
                    music_folder_id: folder_id,
                },
            );
        }
        self.save_library_cache();

        Ok((album_id, folder_id))
    }
}

impl DeviceSession for NativeSession {
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, DeviceError> {
        let parent = self.resolve_path(path)?;
        let handles = self
            .session
            .get_object_handles(self.storage_id, parent)
            .mtp_err()?;

        let mut entries = Vec::new();
        for handle in handles {
            let info = match self.session.get_object_info(handle) {
                Ok(info) => info,
                Err(_) => continue,
            };
            entries.push(object_info_to_entry(handle, &info));
        }
        Ok(entries)
    }

    fn import_track(
        &mut self,
        local_path: &str,
        meta: Option<&super::TrackMeta>,
    ) -> Result<u64, DeviceError> {
        let file_data =
            std::fs::read(local_path).map_err(|e| format!("Cannot read {}: {}", local_path, e))?;

        let filename = std::path::Path::new(local_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("track.mp3");

        // Prefer caller-supplied metadata (from the library scan); fall back to
        // a local lofty read for CLI `push` paths that have no Track backing.
        let (artist, album, title, track_num, genre) = match meta {
            Some(m) => (
                m.artist.clone(),
                m.album.clone(),
                m.title.clone(),
                m.track_number.unwrap_or(0).min(u16::MAX as u32) as u16,
                m.genre.clone().unwrap_or_default(),
            ),
            None => read_metadata(local_path, filename),
        };

        // Ensure library is loaded.
        self.ensure_library()?;

        // Get library caps.
        let artist_supported = self
            .library
            .as_ref()
            .map(|l| l.caps.artist_supported)
            .unwrap_or(false);
        // Find or create Artist object + music folder.
        let (artist_id, _artist_folder) = self.find_or_create_artist_native(&artist)?;

        // Find or create Album object + music folder.
        let (album_obj_id, album_folder) =
            self.find_or_create_album_native(&artist, &album, artist_id, artist_supported)?;

        // Determine audio format from extension.
        let format = detect_format(filename);

        // Create track via SendObjectPropList.
        let mut props = PropListBuilder::new();
        if artist_supported {
            props.add_u32(PROP_ARTIST_ID, artist_id);
        } else {
            props.add_string(PROP_ARTIST, &artist);
        }
        props
            .add_string(PROP_NAME, &title)
            .add_string(PROP_OBJECT_FILENAME, filename);
        if track_num > 0 {
            props.add_u16(PROP_TRACK, track_num);
        }
        if !genre.is_empty() {
            props.add_string(PROP_GENRE, &genre);
        }
        let prop_data = props.build();

        self.log_msg(&format!("Importing: {}", title));
        let (_, _, track_id) = self
            .session
            .send_object_prop_list(
                self.storage_id,
                album_folder,
                format,
                file_data.len() as u64,
                &prop_data,
            )
            .mtp_err()?;

        // Upload the actual audio file.
        self.session.send_object(&file_data).mtp_err()?;

        // Link track to album via object references.
        if let Ok(mut refs) = self.session.get_object_references(album_obj_id) {
            refs.push(track_id);
            let _ = self.session.set_object_references(album_obj_id, &refs);
        } else {
            let _ = self
                .session
                .set_object_references(album_obj_id, &[track_id]);
        }

        // Set album art if available and supported.
        self.try_set_album_art(local_path, album_obj_id);

        // Update the track cache incrementally.
        let new_entry = DeviceEntry {
            object_id: track_id as u64,
            storage_id: self.storage_id as u64,
            format: format_name(format),
            size: file_data.len() as u64,
            name: format!("{}/{}/{}", artist, album, filename),
            ..Default::default()
        };
        self.cache.append(&new_entry);

        // Record the (artist, album, title) → handle mapping so a same-session
        // playlist push can resolve freshly-uploaded tracks without going
        // through ZMDB (which returns object_id=0 for them).
        self.recent_imports.insert(
            (
                artist.to_lowercase(),
                album.to_lowercase(),
                title.to_lowercase(),
            ),
            track_id,
        );
        self.log_msg(&format!(
            "  recent_imports: cached handle 0x{track_id:08x} for ({}/{}/{}) — total {} entries",
            artist,
            album,
            title,
            self.recent_imports.len(),
        ));

        Ok(track_id as u64)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), DeviceError> {
        let handle = self.resolve_path(device_path)?;
        self.delete_recursive(handle)?;
        self.cache.remove(device_path);
        self.invalidate_library_for_path(device_path);
        Ok(())
    }

    fn rm_by_id(&mut self, object_id: u32) -> Result<(), DeviceError> {
        self.delete_recursive(object_id)?;
        self.cache.remove_by_id(object_id);
        Ok(())
    }

    fn cleanup_empty_folders(&mut self) -> Result<usize, DeviceError> {
        let music_folder = match self.resolve_path("/Music") {
            Ok(h) => h,
            Err(_) => return Ok(0),
        };
        let mut removed = 0usize;
        let artist_handles = self
            .session
            .get_object_handles(self.storage_id, music_folder)
            .mtp_err()?;

        for artist_h in artist_handles {
            let artist_info = match self.session.get_object_info(artist_h) {
                Ok(i) => i,
                Err(_) => continue,
            };
            if artist_info.object_format != ASSOCIATION_FORMAT {
                continue;
            }

            // Check album subfolders inside this artist folder.
            let album_handles = self
                .session
                .get_object_handles(self.storage_id, artist_h)
                .unwrap_or_default();
            for album_h in &album_handles {
                let album_info = match self.session.get_object_info(*album_h) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                if album_info.object_format != ASSOCIATION_FORMAT {
                    continue;
                }
                // If album folder is empty, delete it.
                let children = self
                    .session
                    .get_object_handles(self.storage_id, *album_h)
                    .unwrap_or_default();
                if children.is_empty() && self.session.delete_object(*album_h).is_ok() {
                    self.log_msg(&format!(
                        "Cleaned up empty folder: {}/{}",
                        artist_info.filename, album_info.filename
                    ));
                    // Deleting the folder orphans the cached abstract-album
                    // handle — drop it or the next sync of this album feeds a
                    // dead handle to send_object_prop_list (findings.md).
                    self.invalidate_library_for_path(&format!(
                        "/Music/{}/{}",
                        artist_info.filename, album_info.filename
                    ));
                    removed += 1;
                }
            }

            // Re-check artist folder — it may now be empty after album cleanup.
            let remaining = self
                .session
                .get_object_handles(self.storage_id, artist_h)
                .unwrap_or_default();
            if remaining.is_empty() && self.session.delete_object(artist_h).is_ok() {
                self.log_msg(&format!(
                    "Cleaned up empty folder: {}",
                    artist_info.filename
                ));
                // Same staleness hazard as the album case, for the artist
                // handle (and any album entries still keyed under it).
                self.invalidate_library_for_path(&format!("/Music/{}", artist_info.filename));
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, DeviceError> {
        // Query current storage to validate cache freshness.
        let current_free = self
            .session
            .get_storage_info(self.storage_id)
            .ok()
            .map(|(_, free)| free)
            .unwrap_or(0);

        // Try disk cache first — avoids slow MTP enumeration on reconnect.
        self.log_msg("Checking track cache...");
        // Keep old cache entries so we can preserve real object IDs after ZMDB reload.
        let old_cached = self.cache.load();
        if let Some((cached_free, ref cached)) = old_cached {
            let diff = current_free.abs_diff(cached_free);
            if diff > 1_000_000 {
                self.log_msg(&format!(
                    "Storage changed (free: {} → {}), invalidating caches",
                    cached_free, current_free
                ));
                self.cache.clear();
                self.clear_library_cache();
                self.library = None;
            } else {
                // Cache holds the last-known playcount/rating snapshot, but
                // those change without storage changing — re-run the bulk
                // enrichment so device-only plays surface in the TUI without
                // waiting for the next sync. Single round-trip per prop;
                // gracefully no-ops on firmware that rejects the bulk path.
                // Persist the refreshed values so a fast reconnect keeps them.
                let mut tracks = cached.clone();
                self.enrich_with_playcounts(&mut tracks);
                if let Err(e) = self.cache.save(&tracks, current_free) {
                    self.log_msg(&e);
                }
                let with_pc = tracks.iter().filter(|t| t.play_count.is_some()).count();
                self.log_msg(&format!(
                    "Loaded {} tracks from cache ({with_pc} with playcount)",
                    tracks.len()
                ));
                return Ok(tracks);
            }
        }
        self.log_msg("No cache, querying device...");

        // Restore sync progress before device queries (only on cache miss).
        self.restore_sync_progress();

        // Build a map of known object IDs from the old cache (before invalidation).
        let known_ids: std::collections::HashMap<String, u64> = old_cached
            .map(|(_, entries)| {
                entries
                    .into_iter()
                    .filter(|e| e.object_id > 0)
                    .map(|e| (e.name, e.object_id))
                    .collect()
            })
            .unwrap_or_default();

        // Try ZMDB — single MTP call for the entire device library.
        match self.try_zmdb() {
            Ok(mut tracks) => {
                // Merge in real object IDs from the old cache where names match.
                if !known_ids.is_empty() {
                    let mut restored = 0usize;
                    for t in &mut tracks {
                        if t.object_id == 0 {
                            if let Some(&id) = known_ids.get(&t.name) {
                                t.object_id = id;
                                restored += 1;
                            }
                        }
                    }
                    if restored > 0 {
                        self.log_msg(&format!(
                            "Restored {} object IDs from previous cache",
                            restored
                        ));
                    }
                }
                self.enrich_with_playcounts(&mut tracks);
                if let Err(e) = self.cache.save(&tracks, current_free) {
                    self.log_msg(&e);
                }
                return Ok(tracks);
            }
            // A fatal USB failure would just cascade across hundreds of
            // GetObjectInfo calls on a dead pipe — propagate it. Anything
            // else (unsupported vendor op on old firmware, ZMDB parse
            // failure) falls back to the slow handle walk.
            Err(e @ DeviceError::DeviceGone(_)) => return Err(e),
            Err(e) => {
                self.log_msg(&format!("ZMDB unavailable ({}), falling back to scan", e));
            }
        }

        // Fallback: recursive MTP object handle walk.
        let parent = match self.resolve_path(path) {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };

        self.log_msg("Scanning device (first time may take a minute)...");
        let mut entries = Vec::new();
        self.list_recursive(parent, "", &mut entries)?;
        let mut tracks: Vec<DeviceEntry> = entries.into_iter().filter(|e| !e.is_dir()).collect();

        self.enrich_with_playcounts(&mut tracks);
        if let Err(e) = self.cache.save(&tracks, current_free) {
            self.log_msg(&e);
        }
        self.log_msg(&format!("Cached {} tracks", tracks.len()));

        Ok(tracks)
    }

    fn get_storage_info(&mut self) -> Result<(u64, u64), DeviceError> {
        self.session.get_storage_info(self.storage_id).mtp_err()
    }

    fn prewarm_library(&mut self) -> Result<(), DeviceError> {
        self.ensure_library()
    }

    fn refresh_storage_cache(&mut self, free_bytes: u64) {
        self.cache.update_free_bytes(free_bytes);
    }

    fn save_sync_progress(&mut self) {
        // Route through the firmware-guarded helper: pre-3.0 Zunes (and any
        // device that explicitly rejects the vendor op) quietly return None
        // instead of producing a user-visible "device rejected operation"
        // warning at the end of every sync.
        let data = match self.get_sync_progress() {
            Ok(Some(d)) => d,
            Ok(None) => return,
            Err(e) => {
                self.log_msg(&format!("Could not read sync progress: {e}"));
                return;
            }
        };
        if let Some(path) = self.sync_cache_path() {
            if std::fs::write(&path, &data).is_ok() {
                self.log_msg(&format!("Saved sync progress ({} bytes)", data.len()));
            }
        }
    }

    fn import_photo(&mut self, filename: &str, jpeg_data: &[u8]) -> Result<u64, DeviceError> {
        let photos_folder = self.find_or_create_folder(MTP_ROOT, "Photos")?;

        let props = PropListBuilder::new()
            .add_string(PROP_OBJECT_FILENAME, filename)
            .add_string(PROP_NAME, stem(filename))
            .build();

        self.log_msg(&format!("Importing photo: {}", filename));
        let (_, _, obj_id) = self
            .session
            .send_object_prop_list(
                self.storage_id,
                photos_folder,
                FORMAT_EXIF_JPEG,
                jpeg_data.len() as u64,
                &props,
            )
            .mtp_err()?;

        self.session.send_object(jpeg_data).mtp_err()?;
        Ok(obj_id as u64)
    }

    fn import_video(&mut self, filename: &str, data: &[u8]) -> Result<u64, DeviceError> {
        let videos_folder = self.find_or_create_folder(MTP_ROOT, "Videos")?;

        let format = detect_video_format(filename);

        let props = PropListBuilder::new()
            .add_string(PROP_OBJECT_FILENAME, filename)
            .add_string(PROP_NAME, stem(filename))
            .build();

        self.log_msg(&format!("Importing video: {}", filename));
        let (_, _, obj_id) = self
            .session
            .send_object_prop_list(
                self.storage_id,
                videos_folder,
                format,
                data.len() as u64,
                &props,
            )
            .mtp_err()?;

        self.session.send_object(data).mtp_err()?;
        Ok(obj_id as u64)
    }

    fn collect_all_videos(&mut self) -> Result<Vec<DeviceEntry>, DeviceError> {
        // Return cached video entries from the last ZMDB parse if available.
        if let Some(ref cached) = self.zmdb_video_cache {
            self.log_msg(&format!("Returning {} cached video entries", cached.len()));
            return Ok(cached.clone());
        }
        // Try ZMDB to populate the cache.
        let _ = self.try_zmdb();
        if let Some(ref cached) = self.zmdb_video_cache {
            return Ok(cached.clone());
        }
        // Fallback: scan /Videos via MTP handle walk.
        self.log_msg("No ZMDB video data, scanning /Videos...");
        self.collect_all_tracks("/Videos")
    }

    /// Push a playlist to the Zune over MTP. See
    /// `docs/zune-playlist-research.md` for the protocol research; the
    /// flow follows libmtp's `create_new_abstract_list`.
    ///
    /// 1. Walk the device's track index to build a `(artist, album,
    ///    title) → object_handle` map. Tuples that don't resolve are
    ///    skipped (counted as `skipped` in the summary).
    /// 2. If a playlist with the same name already exists at the
    ///    storage root, capture its handle so we can replace in-place
    ///    via `SetObjectReferences` (libmtp's `update_abstract_list`
    ///    pattern). Otherwise:
    ///    - `SendObjectPropList(format=0xBA05, parent=0, size=0,
    ///      props=[ObjectFileName, Name])` — create a zero-byte
    ///      playlist object.
    ///    - `SendObject(&[])` — empty body. The device renders the
    ///      playlist from references, not the body.
    /// 3. `SetObjectReferences(playlist_handle, &member_handles)` —
    ///    attach the resolved track handles in playlist order.
    ///
    /// Capability-gates the prop set on
    /// `get_object_props_supported(0xBA05)` so firmware that doesn't
    /// advertise the standard props (Zune HD reports a richer set than
    /// the Zune 30 v1.4) doesn't trip on a write that fails at
    /// `Get_Object_Prop_Desc`.
    ///
    /// Falls back to `parent = music_folder` if `parent = 0` is
    /// rejected, mirroring libmtp's `default_music_folder` fallback.
    fn import_playlist(
        &mut self,
        name: &str,
        track_keys: &[(String, String, String)],
    ) -> Result<super::PlaylistImportSummary, DeviceError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("Playlist name cannot be empty".into());
        }

        // Step 0: capability check — log-only, firmware-gated.
        self.log_playlist_prop_support();

        // Step 1: resolve (artist, album, title) tuples to MTP object
        // handles.
        let (member_handles, skipped) = self.resolve_playlist_members(track_keys)?;

        // Step 2: locate or create the playlist object.
        let music_folder = self.lib()?.music_folder;

        // Compute the on-device filename once — used both for matching
        // existing playlists and (in the create branch) for the
        // ObjectFileName property. Match needs `.zpl`-suffixed because
        // that's what we store on the device; comparing the bare trimmed
        // name would never match an existing entry and we'd accumulate a
        // duplicate `<name>.zpl` per sync.
        let filename = if trimmed.to_lowercase().ends_with(".zpl") {
            trimmed.to_string()
        } else {
            format!("{trimmed}.zpl")
        };

        let (playlist_handle, replaced) = match self.find_existing_playlist(&filename) {
            Some(h) => {
                self.log_msg(&format!(
                    "Updating existing playlist \"{trimmed}\" at handle 0x{h:08x}"
                ));
                (h, true)
            }
            None => (
                self.create_playlist_object(trimmed, &filename, music_folder)?,
                false,
            ),
        };

        // Step 3: attach references. Order matters — that becomes the
        // on-device playlist order.
        self.session
            .set_object_references(playlist_handle, &member_handles)
            .map_err(|e| format!("SetObjectReferences failed: {e}"))?;

        Ok(super::PlaylistImportSummary {
            resolved: member_handles.len(),
            skipped,
            replaced,
        })
    }
}

impl NativeSession {
    /// Diagnostic-only probe of the playlist (0xBA05) props the device
    /// advertises. Never fails the import — it only logs.
    ///
    /// v1.4 firmware silently drops `GetObjectPropsSupported(0xBA05)`:
    /// the device never replies, our 30s ReadPipe timeout fires, and
    /// the bulk pipe is left in a half-response state that requires a
    /// physical replug. The `mtp-probe playlist-push` binary discovered
    /// this empirically (see its `--probe-caps` gate at
    /// tools/mtp-probe/src/playlist_push.rs and project memory entry
    /// `project_v14_playcount_via_getobjectproplist`). The probe's
    /// workaround: skip the query on v1.4 and just attempt the create —
    /// SendObjectPropList(0xBA05) with [ObjectFileName, Name] is known
    /// to work on v1.4 hw (validated 2026-04-26) regardless of what
    /// the cap query did or didn't say.
    ///
    /// Firmware 3.0+ handles the query fine, so we still run it there
    /// for the diagnostic log line.
    fn log_playlist_prop_support(&mut self) {
        if !self.supports_modern_vendor_ops() {
            self.log_msg("Skipping GetObjectPropsSupported(0xBA05) — wedges v1.4 firmware");
            return;
        }
        match self
            .session
            .get_object_props_supported(FORMAT_ABSTRACT_AUDIO_VIDEO_PLAYLIST)
        {
            Ok(supported) => {
                let has_filename = supported.contains(&PROP_OBJECT_FILENAME);
                let has_name = supported.contains(&PROP_NAME);
                if !has_filename || !has_name {
                    self.log_msg(&format!(
                        "warn: Zune doesn't advertise both required 0xBA05 props \
                         (filename: {has_filename}, name: {has_name}); attempting anyway",
                    ));
                }
            }
            Err(e) => {
                self.log_msg(&format!(
                    "warn: GetObjectPropsSupported(0xBA05) failed ({e}); attempting create anyway"
                ));
            }
        }
    }

    /// Resolve `(artist, album, title)` tuples to MTP object handles and
    /// log the outcome. Returns `(member_handles, skipped_count)`.
    ///
    /// Two-tier lookup:
    ///   1. `recent_imports` — same-session uploads, populated by
    ///      `import_track`. Authoritative because the handle came
    ///      straight from `SendObjectPropList`.
    ///   2. `collect_all_tracks` — ZMDB + on-disk track cache. Covers
    ///      tracks already on the device from a previous connection.
    ///
    /// Order matters: ZMDB returns `object_id = 0` for entries it
    /// synthesises from metadata, and the on-disk cache's name format
    /// ("Artist/Album/filename.ext") doesn't match ZMDB's
    /// ("Artist/Album/Title"), so cache-driven name restoration can
    /// miss freshly-uploaded tracks across a reconnect. The recent_imports
    /// tier sidesteps both issues for any track the user just synced.
    fn resolve_playlist_members(
        &mut self,
        track_keys: &[(String, String, String)],
    ) -> Result<(Vec<u32>, usize), DeviceError> {
        let device_tracks = self.collect_all_tracks("/Music")?;
        let resolved = resolve_playlist_handles(track_keys, &self.recent_imports, &device_tracks);
        let PlaylistResolveResult {
            member_handles,
            skipped,
            from_recent,
        } = resolved;
        // Always log the resolver outcome — the invisible-playlist failure
        // mode is "0 resolved, all skipped", which is silent without this.
        self.log_msg(&format!(
            "Playlist resolver: {} resolved ({} from same-session, {} from device), {} skipped of {} tuples",
            member_handles.len(),
            from_recent,
            member_handles.len().saturating_sub(from_recent),
            skipped,
            track_keys.len(),
        ));
        if member_handles.is_empty() && !track_keys.is_empty() {
            // Surface the first few unresolvable tuples so the user can see
            // *why* nothing matched (typo, missing-on-device, etc).
            let sample: Vec<String> = track_keys
                .iter()
                .take(3)
                .map(|(a, b, t)| format!("\"{a}\" / \"{b}\" / \"{t}\""))
                .collect();
            self.log_msg(&format!(
                "Playlist resolver: no handles matched. recent_imports has {} entries, device has {} tracks. \
                 Sample unresolved keys: {}",
                self.recent_imports.len(),
                device_tracks.len(),
                sample.join(" | "),
            ));
        }
        Ok((member_handles, skipped))
    }

    /// Find an existing 0xBA05 object with the given filename at the
    /// storage root — that's where `create_playlist_object` puts new
    /// ones. Scoping the search to the storage root keeps it cheap; the
    /// Zune tends to keep playlists there per libmtp conventions. Match
    /// is case-insensitive: the filesystem layer treats `Roadtrip.zpl`
    /// and `roadtrip.zpl` as the same file on the device.
    fn find_existing_playlist(&mut self, filename: &str) -> Option<u32> {
        self.session
            .get_object_handles(self.storage_id, MTP_ROOT)
            .ok()
            .and_then(|handles| {
                handles.into_iter().find(|h| {
                    self.session
                        .get_object_info(*h)
                        .ok()
                        .map(|info| {
                            info.object_format == FORMAT_ABSTRACT_AUDIO_VIDEO_PLAYLIST
                                && info.filename.eq_ignore_ascii_case(filename)
                        })
                        .unwrap_or(false)
                })
            })
    }

    /// Create a zero-byte playlist object and return its handle. libmtp
    /// ships parent=0 (let the device decide), with a documented fallback
    /// to `default_music_folder` on `InvalidParent` — mirror that.
    fn create_playlist_object(
        &mut self,
        name: &str,
        filename: &str,
        music_folder: u32,
    ) -> Result<u32, DeviceError> {
        let mut props = PropListBuilder::new();
        props
            .add_string(PROP_OBJECT_FILENAME, filename)
            .add_string(PROP_NAME, name);
        let prop_list = props.build();

        let create = self.session.send_object_prop_list(
            self.storage_id,
            0,
            FORMAT_ABSTRACT_AUDIO_VIDEO_PLAYLIST,
            0,
            &prop_list,
        );
        let (_, _, handle) = match create {
            Ok(triple) => triple,
            Err(_) => {
                self.log_msg("parent=0 rejected; retrying with music folder");
                self.session
                    .send_object_prop_list(
                        self.storage_id,
                        music_folder,
                        FORMAT_ABSTRACT_AUDIO_VIDEO_PLAYLIST,
                        0,
                        &prop_list,
                    )
                    .map_err(|e| format!("SendObjectPropList(0xBA05) failed: {e}"))?
            }
        };

        // Empty body. The Zune ignores the payload — references
        // are authoritative — but the create handshake requires
        // a SendObject to commit the proplist.
        self.session
            .send_object(&[])
            .map_err(|e| format!("SendObject(empty) failed: {e}"))?;

        Ok(handle)
    }

    /// Phase 4a probe: list MTP object properties the device advertises for
    /// the given object format. `format = 0x3009` is MP3 (the most common
    /// Zune content type). Returns the property codes in the same order the
    /// device emits them.
    pub fn probe_supported_props(&mut self, format: u16) -> Result<Vec<u16>, DeviceError> {
        self.session.get_object_props_supported(format).mtp_err()
    }

    /// Phase 4a probe: ask the device for ALL properties on `object_id`,
    /// regardless of what `GetObjectPropsSupported` advertises. Devices
    /// frequently serve unadvertised properties — particularly Microsoft-
    /// specific MTP-AAS extensions — and on Zune v1.4 this is the most
    /// likely path to surface a playcount the firmware doesn't list.
    ///
    /// Wraps `GetObjectPropList(handle, format=0, prop=0xFFFFFFFF, group=0,
    /// depth=0)` and decodes the response into typed elements.
    pub fn probe_all_props(
        &mut self,
        object_id: u32,
    ) -> Result<Vec<zune_mtp::session::PropListElement>, DeviceError> {
        let raw = self
            .session
            .get_object_prop_list(object_id, 0, 0xFFFF_FFFF, 0, 0)
            .map_err(|e| e.to_string())?;
        Ok(zune_mtp::session::parse_object_prop_list(&raw))
    }

    /// Phase 4a probe: invoke an arbitrary MTP operation by code with up to
    /// five u32 params, returning the response code, response params, and
    /// any data-in payload. Hard 10 s timeout so a hung device doesn't
    /// stall the whole session. Refuses to fire on the small allowlist of
    /// operation codes that have empirically wedged the device.
    ///
    /// Use ONLY for read-only data-in operations (op code in the 0x90xx /
    /// 0x91xx Microsoft vendor ranges, or unknown 0x9xxx). Calling a
    /// data-out operation with this will fail or wedge the session.
    pub fn probe_vendor_op(
        &mut self,
        op_code: u16,
        params: &[u32],
    ) -> Result<Vec<u8>, DeviceError> {
        // Empirical hard-block list. 0x9180 is documented in the v1.4
        // vendor-ops survey as causing a USB-resetting hang. Adding more
        // here as we learn them is cheap insurance.
        const BLOCKED: &[u16] = &[0x9180];
        if BLOCKED.contains(&op_code) {
            return Err(DeviceError::Other(format!(
                "op 0x{op_code:04X} is on the probe blocklist (known to wedge the device)"
            )));
        }
        self.session.execute_data_in_raw(op_code, params).mtp_err()
    }

    /// Phase 4a probe: dump the raw ZMDB binary (vendor op `0x9217`). Used
    /// for diff-based playcount investigation: dump before, play a track on
    /// the device, dump after, hex-diff the binaries to find changed bytes
    /// the parser currently ignores.
    ///
    /// Mirrors the firmware-version guard from `try_zmdb` so users on v1.4
    /// hardware get a friendly message instead of a bare
    /// `0x2005 OperationNotSupported` from the device.
    pub fn probe_zmdb_dump(&mut self) -> Result<Vec<u8>, DeviceError> {
        if !self.supports_modern_vendor_ops() {
            let v = self.firmware_version.as_deref().unwrap_or("unknown");
            return Err(DeviceError::Unsupported(format!(
                "firmware {v} predates ZMDB (added in firmware 3.0); ZMDB dump unavailable"
            )));
        }
        self.session.get_zmdb(1).mtp_err().map_err(|e| match e {
            DeviceError::Unsupported(_) => {
                DeviceError::Unsupported("device does not support ZMDB bulk query".to_string())
            }
            other => other,
        })
    }

    /// Phase 4a probe: BFS over the device's object tree from the active
    /// storage's root and return the first audio object's `(handle, format)`.
    /// Recursion is necessary because `get_object_handles(storage, MTP_ROOT)`
    /// returns top-level *folders* (`/Music`, `/Photos`, …) — all
    /// `ASSOCIATION_FORMAT` — and never the audio files which live nested
    /// under `/Music/Artist/Album/`.
    ///
    /// Audio formats whitelisted: MP3 (0x3009), WMA (0xB901), AAC (0xB903).
    /// MP4 container (0xB982) is intentionally excluded — that's the Zune's
    /// video format and would mislabel a video as audio.
    ///
    /// Used by the probe command to pick a target track without forcing the
    /// caller to know an object ID.
    pub fn probe_first_audio_handle(&mut self) -> Result<Option<(u32, u16)>, DeviceError> {
        let storage = self.storage_id;
        let mut queue: Vec<u32> = vec![MTP_ROOT];
        while let Some(parent) = queue.pop() {
            let handles = self
                .session
                .get_object_handles(storage, parent)
                .map_err(|e| e.to_string())?;
            for h in handles {
                let info = match self.session.get_object_info(h) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let fmt = info.object_format;
                if matches!(fmt, 0x3009 | 0xB901 | 0xB903) {
                    return Ok(Some((h, fmt)));
                }
                if fmt == ASSOCIATION_FORMAT {
                    queue.push(h);
                }
            }
        }
        Ok(None)
    }

    /// Path for the sync progress cache file.
    /// Path for the device library cache file.
    fn library_cache_path(&self) -> Option<PathBuf> {
        let base = crate::paths::device_cache_base()?;
        let filename = match &self.sync_cache_serial {
            Some(s) => format!(".zytunes-library-cache-{s}"),
            None => ".zytunes-library-cache".to_string(),
        };
        Some(base.join(filename))
    }

    /// Save the device library state to disk.
    fn save_library_cache(&self) {
        if let Some(ref lib) = self.library {
            if let Some(path) = self.library_cache_path() {
                let _ = std::fs::write(path, lib.serialize());
            }
        }
    }

    /// Delete a handle and, if it's an association (folder), its children first.
    /// Zune firmware does NOT cascade folder deletion for us: deleting a folder
    /// handle leaves the child objects orphaned on the device. Walk the
    /// hierarchy bottom-up so every descendant is gone before the parent.
    fn delete_recursive(&mut self, handle: u32) -> Result<(), DeviceError> {
        let is_folder = self
            .session
            .get_object_info(handle)
            .map(|info| info.object_format == ASSOCIATION_FORMAT)
            .unwrap_or(false);
        if is_folder {
            let children = self
                .session
                .get_object_handles(self.storage_id, handle)
                .unwrap_or_default();
            for child in children {
                self.delete_recursive(child)?;
            }
        }
        self.session.delete_object(handle).mtp_err()
    }

    /// Drop matching artist/album entries from the in-memory library and
    /// re-save the cache. Without this, a subsequent sync re-uses stale
    /// MTP handles for the deleted parent folder — on v1.4 firmware the
    /// resulting `send_object_prop_list` against a dead parent halts the
    /// OUT bulk pipe and cascades every queued track. See findings.md.
    fn invalidate_library_for_path(&mut self, device_path: &str) {
        if self.library.is_none() {
            self.library = self.load_library_cache();
        }
        let Some(lib) = self.library.as_mut() else {
            return;
        };
        if invalidate_device_library(lib, device_path) {
            self.save_library_cache();
        }
    }

    /// Delete the device library cache file.
    fn clear_library_cache(&self) {
        if let Some(path) = self.library_cache_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Load the device library state from disk cache.
    fn load_library_cache(&self) -> Option<DeviceLibrary> {
        let path = self.library_cache_path()?;
        let data = std::fs::read_to_string(path).ok()?;
        DeviceLibrary::deserialize(&data)
    }

    fn sync_cache_path(&self) -> Option<PathBuf> {
        let base = crate::paths::device_cache_base()?;
        let filename = match &self.sync_cache_serial {
            Some(s) => format!(".zytunes-sync-progress-{s}"),
            None => ".zytunes-sync-progress".to_string(),
        };
        Some(base.join(filename))
    }

    /// Restore cached sync progress to the device.
    /// Called once on connect, before loading tracks. Best-effort — failures are silent.
    fn restore_sync_progress(&mut self) {
        if self.sync_restored {
            return;
        }
        self.sync_restored = true;
        let path = match self.sync_cache_path() {
            Some(p) => p,
            None => return,
        };
        let cached = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => return, // No cache file — normal on first connect
        };
        // The SET payload is 530 bytes (first 530 of the 1036-byte GET response).
        let payload_len = 530.min(cached.len());
        if payload_len < 530 {
            return; // Corrupted cache — skip
        }
        if self.session.set_sync_progress(&cached[..530]).is_ok() {
            self.log_msg("Restored sync progress from cache");
        }
    }

    /// Try to extract and set album art on the device. Logs but doesn't fail on errors.
    fn try_set_album_art(&mut self, local_path: &str, album_obj_id: u32) {
        if self.art_disabled {
            return;
        }
        let supported = self
            .library
            .as_ref()
            .map(|l| l.caps.album_cover_supported)
            .unwrap_or(false);
        if !supported {
            self.log_msg("Album art not supported by device");
            return;
        }
        let jpeg_data = match extract_album_art(local_path) {
            Some(data) => data,
            None => {
                self.log_msg("No album art found in file");
                return;
            }
        };
        self.log_msg(&format!("Setting album art ({} bytes)", jpeg_data.len()));
        // Diagnostic: dump the exact bytes we're about to send so failures
        // can be inspected with `file`, `identify`, `exiftool`, etc.
        if std::env::var("ZYTUNES_DUMP_ART").is_ok() {
            let _ = std::fs::write("/tmp/zytunes-last-art.jpg", &jpeg_data);
        }
        let mut art_data = Vec::with_capacity(4 + jpeg_data.len());
        art_data.extend_from_slice(&(jpeg_data.len() as u32).to_le_bytes());
        art_data.extend_from_slice(&jpeg_data);
        match self.session.set_object_prop_value(
            album_obj_id,
            PROP_REPRESENTATIVE_SAMPLE_DATA,
            &art_data,
        ) {
            Ok(_) => self.log_msg("Album art set successfully"),
            Err(e) => {
                // One art failure typically means the Zune's JPEG decoder
                // wedged. Further SetObjectPropValue calls will each burn
                // a ~45s read timeout and keep the session degraded, so
                // skip art for the remainder of this session.
                self.art_disabled = true;
                self.log_msg(&format!(
                    "Album art failed ({}); skipping art for remaining tracks",
                    e
                ));
            }
        }
    }
}

/// Convert an MTP ObjectInfo to a DeviceEntry.
fn object_info_to_entry(handle: u32, info: &ObjectInfo) -> DeviceEntry {
    DeviceEntry {
        object_id: handle as u64,
        storage_id: info.storage_id as u64,
        format: if info.object_format == ASSOCIATION_FORMAT {
            "Association".to_string()
        } else {
            format_name(info.object_format)
        },
        size: info.compressed_size as u64,
        name: info.filename.clone(),
        ..Default::default()
    }
}

/// Map MTP object format codes to human-readable names.
fn format_name(format: u16) -> String {
    match format {
        0x3001 => "Association".to_string(),
        0x3009 => "MP3".to_string(),
        0x300A => "AVI".to_string(),
        0x300B => "MPEG".to_string(),
        0x300C => "ASF".to_string(),
        0x3801 => "JPEG".to_string(),
        0xB901 => "WMA".to_string(),
        0xB981 => "WMV".to_string(),
        0xB903 => "AAC".to_string(),
        0xBA05 => "AbstractAudioVideoPlaylist".to_string(),
        _ => format!("0x{:04x}", format),
    }
}

/// Read metadata from an audio file using lofty (supports all formats).
/// Returns (artist, album, title, track_number, genre).
fn read_metadata(path: &str, filename: &str) -> (String, String, String, u16, String) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    if let Ok(tagged) = lofty::probe::read_from_path(path) {
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            let artist = tag
                .artist()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown Artist".to_string());
            let album = tag
                .album()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown Album".to_string());
            let title = tag
                .title()
                .map(|s| s.to_string())
                .unwrap_or_else(|| stem(filename).to_string());
            let track_num = tag.track().unwrap_or(0) as u16;
            let genre = tag.genre().map(|g| g.to_string()).unwrap_or_default();
            return (artist, album, title, track_num, genre);
        }
    }

    // Fallback: parse from filename.
    let title = stem(filename).to_string();
    (
        "Unknown Artist".to_string(),
        "Unknown Album".to_string(),
        title,
        0,
        String::new(),
    )
}

/// Get filename stem (without extension).
fn stem(filename: &str) -> &str {
    filename
        .rfind('.')
        .map(|pos| &filename[..pos])
        .unwrap_or(filename)
}

/// Detect MTP audio format from filename extension.
fn detect_format(filename: &str) -> u16 {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "mp3" => FORMAT_MP3,
        "wma" => FORMAT_WMA,
        "aac" | "m4a" => FORMAT_AAC,
        _ => FORMAT_MP3, // Default to MP3.
    }
}

/// Detect MTP video format code from filename extension.
fn detect_video_format(filename: &str) -> u16 {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "wmv" => FORMAT_WMV,
        "avi" => 0x300A,          // AVI
        "mpeg" | "mpg" => 0x300B, // MPEG
        "mp4" => 0x300C,          // ASF (closest standard MTP format for MP4)
        _ => FORMAT_WMV,          // Default to WMV.
    }
}

/// Build an MTP ObjectInfo dataset for sending a file.
#[allow(dead_code)]
fn build_object_info_dataset(filename: &str, size: u32) -> Vec<u8> {
    let mut data = Vec::new();

    // StorageID (4) — 0 = let device choose
    data.extend_from_slice(&0u32.to_le_bytes());
    // ObjectFormat (2) — MP3
    data.extend_from_slice(&0x3009u16.to_le_bytes());
    // ProtectionStatus (2)
    data.extend_from_slice(&0u16.to_le_bytes());
    // ObjectCompressedSize (4)
    data.extend_from_slice(&size.to_le_bytes());
    // ThumbFormat (2)
    data.extend_from_slice(&0u16.to_le_bytes());
    // ThumbCompressedSize (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ThumbPixWidth (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ThumbPixHeight (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ImagePixWidth (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ImagePixHeight (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ImageBitDepth (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // ParentObject (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // AssociationType (2)
    data.extend_from_slice(&0u16.to_le_bytes());
    // AssociationDesc (4)
    data.extend_from_slice(&0u32.to_le_bytes());
    // SequenceNumber (4)
    data.extend_from_slice(&0u32.to_le_bytes());

    // Filename (MTP string)
    write_mtp_string(&mut data, filename);
    // CaptureDate (empty)
    data.push(0);
    // ModificationDate (empty)
    data.push(0);
    // Keywords (empty)
    data.push(0);

    data
}

/// Write a string in MTP format: [u8 num_chars] [u16 chars...] [u16 null]
#[allow(dead_code)]
fn write_mtp_string(buf: &mut Vec<u8>, s: &str) {
    let chars: Vec<u16> = s.encode_utf16().collect();
    buf.push((chars.len() + 1) as u8); // +1 for null terminator
    for ch in &chars {
        buf.extend_from_slice(&ch.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes()); // null terminator
}

/// Parse the `major.minor` version from a Zune firmware string.
///
/// The Zune reports firmware in the form `"01.04.00485.00-00425"` — four
/// dot-separated segments plus an optional trailing `-BUILD`. We only look
/// at the first two segments; leading zeros are stripped. Returns `None`
/// for anything we can't confidently parse (unknown devices).
/// Result of resolving a playlist's `(artist, album, title)` tuples to MTP
/// object handles. `from_recent` is informational — used for logging only.
struct PlaylistResolveResult {
    member_handles: Vec<u32>,
    skipped: usize,
    from_recent: usize,
}

/// Resolve playlist track tuples to MTP object handles.
///
/// Lookup order:
///   1. `recent_imports` — handles returned by `SendObjectPropList` during
///      same-session uploads. Authoritative.
///   2. `device_tracks` — entries from `collect_all_tracks` (ZMDB or
///      recursive walk). Filters out `object_id == 0` because ZMDB
///      synthesises entries without a real handle.
///
/// Same-session uploads have to win because ZMDB returns `object_id = 0`
/// for entries it builds from device metadata, and the cross-session cache
/// recovery path is name-format-sensitive in ways that break for fresh
/// uploads. Without this layering the resolver returns an empty handle
/// list immediately after a sync, and `SetObjectReferences` ships an
/// empty playlist that the device UI doesn't render.
///
/// De-duplicates handles by linear contains check — playlist sizes are in
/// the tens to low hundreds, so the O(n²) is fine and a HashSet would
/// lose insertion order.
fn resolve_playlist_handles(
    track_keys: &[(String, String, String)],
    recent_imports: &HashMap<(String, String, String), u32>,
    device_tracks: &[DeviceEntry],
) -> PlaylistResolveResult {
    let mut by_key: HashMap<(String, String, String), u32> =
        HashMap::with_capacity(device_tracks.len());
    for entry in device_tracks {
        if entry.object_id == 0 {
            continue;
        }
        let mut parts = entry.name.splitn(3, '/');
        let (Some(artist), Some(album), Some(title)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        by_key.insert(
            (
                artist.to_lowercase(),
                album.to_lowercase(),
                title.to_lowercase(),
            ),
            entry.object_id as u32,
        );
    }

    let mut member_handles: Vec<u32> = Vec::with_capacity(track_keys.len());
    let mut skipped = 0usize;
    let mut from_recent = 0usize;
    for (artist, album, title) in track_keys {
        let key = (
            artist.to_lowercase(),
            album.to_lowercase(),
            title.to_lowercase(),
        );
        let handle = recent_imports
            .get(&key)
            .inspect(|_| from_recent += 1)
            .or_else(|| by_key.get(&key));
        match handle {
            Some(h) => {
                if !member_handles.contains(h) {
                    member_handles.push(*h);
                }
            }
            None => skipped += 1,
        }
    }

    PlaylistResolveResult {
        member_handles,
        skipped,
        from_recent,
    }
}

fn parse_firmware_version(raw: &str) -> Option<(u16, u16)> {
    let head = raw.split('-').next().unwrap_or(raw);
    let mut parts = head.split('.');
    let major = parts.next()?.trim_start_matches('0');
    let minor = parts.next()?.trim_start_matches('0');
    // An all-zero segment like "00" becomes "" after trimming — treat as 0.
    let major: u16 = if major.is_empty() {
        0
    } else {
        major.parse().ok()?
    };
    let minor: u16 = if minor.is_empty() {
        0
    } else {
        minor.parse().ok()?
    };
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_library() -> DeviceLibrary {
        let mut artists = HashMap::new();
        artists.insert(
            "Rush".to_string(),
            ArtistInfo {
                id: 0x100,
                music_folder_id: 0x10,
            },
        );
        artists.insert(
            "Yes".to_string(),
            ArtistInfo {
                id: 0x101,
                music_folder_id: 0x11,
            },
        );
        let mut albums = HashMap::new();
        albums.insert(
            ("Rush".to_string(), "Moving Pictures".to_string()),
            AlbumInfo {
                id: 0x200,
                music_folder_id: 0x20,
            },
        );
        albums.insert(
            ("Rush".to_string(), "Hemispheres".to_string()),
            AlbumInfo {
                id: 0x201,
                music_folder_id: 0x21,
            },
        );
        albums.insert(
            ("Yes".to_string(), "Fragile".to_string()),
            AlbumInfo {
                id: 0x202,
                music_folder_id: 0x22,
            },
        );
        DeviceLibrary {
            music_folder: 1,
            artists_folder: 2,
            albums_folder: 3,
            caps: DeviceCaps {
                artist_supported: true,
                album_date_supported: true,
                album_cover_supported: true,
            },
            artists,
            albums,
        }
    }

    #[test]
    fn invalidate_album_path_drops_only_that_album() {
        let mut lib = sample_library();
        assert!(invalidate_device_library(
            &mut lib,
            "/Music/Rush/Moving Pictures"
        ));
        assert!(!lib
            .albums
            .contains_key(&("Rush".to_string(), "Moving Pictures".to_string())));
        // Sibling album and the artist itself survive.
        assert!(lib
            .albums
            .contains_key(&("Rush".to_string(), "Hemispheres".to_string())));
        assert!(lib.artists.contains_key("Rush"));
    }

    #[test]
    fn invalidate_artist_path_drops_artist_and_all_its_albums() {
        let mut lib = sample_library();
        assert!(invalidate_device_library(&mut lib, "/Music/Rush"));
        assert!(!lib.artists.contains_key("Rush"));
        assert!(!lib.albums.keys().any(|(a, _)| a == "Rush"));
        // Unrelated artist untouched.
        assert!(lib.artists.contains_key("Yes"));
        assert!(lib
            .albums
            .contains_key(&("Yes".to_string(), "Fragile".to_string())));
    }

    #[test]
    fn invalidate_reports_whether_anything_was_removed() {
        let mut lib = sample_library();
        // Non-Music path and unknown entries are no-ops.
        assert!(!invalidate_device_library(&mut lib, "/Videos/clip.wmv"));
        assert!(!invalidate_device_library(&mut lib, "/Music/Nobody"));
        assert!(!invalidate_device_library(
            &mut lib,
            "/Music/Rush/Not There"
        ));
        // A real removal reports true; repeating it reports false.
        assert!(invalidate_device_library(&mut lib, "/Music/Rush"));
        assert!(!invalidate_device_library(&mut lib, "/Music/Rush"));
    }

    #[test]
    fn parse_firmware_version_handles_zune_format() {
        // Real-world sample from a Zune 30 on early firmware.
        assert_eq!(parse_firmware_version("01.04.00485.00-00425"), Some((1, 4)));
        // Firmware 3.0 (Sep 2008) — first release with ZMDB/acquired-items.
        assert_eq!(parse_firmware_version("03.00.00123.00-00001"), Some((3, 0)));
        // No build suffix.
        assert_eq!(parse_firmware_version("03.02.01000.00"), Some((3, 2)));
        // Single leading segment.
        assert_eq!(parse_firmware_version("5.0"), Some((5, 0)));
    }

    #[test]
    fn parse_firmware_version_handles_all_zero_segments() {
        // A segment that trims to an empty string should round-trip to 0,
        // not propagate as a parse failure.
        assert_eq!(parse_firmware_version("00.00"), Some((0, 0)));
    }

    #[test]
    fn parse_firmware_version_rejects_garbage() {
        assert!(parse_firmware_version("").is_none());
        assert!(parse_firmware_version("no-dots").is_none());
        assert!(parse_firmware_version("bogus.nope").is_none());
        // Single segment — we need at least major.minor.
        assert!(parse_firmware_version("3").is_none());
    }

    #[test]
    fn write_mtp_string_ascii() {
        let mut buf = Vec::new();
        write_mtp_string(&mut buf, "Hi");
        // num_chars=3 (2 chars + null), 'H'=0x0048, 'i'=0x0069, null=0x0000
        assert_eq!(buf, [3, 0x48, 0x00, 0x69, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn write_mtp_string_empty() {
        let mut buf = Vec::new();
        write_mtp_string(&mut buf, "");
        // num_chars=1 (just null), null=0x0000
        assert_eq!(buf, [1, 0x00, 0x00]);
    }

    #[test]
    fn build_object_info_dataset_has_correct_size() {
        let data = build_object_info_dataset("test.mp3", 1024);
        // Fixed fields: 52 bytes, plus filename string, plus 3 empty strings
        assert!(data.len() > 52);
        // Check size field at offset 8
        let size = u32::from_le_bytes(data[8..12].try_into().unwrap());
        assert_eq!(size, 1024);
        // Check format field at offset 4 (MP3 = 0x3009)
        let format = u16::from_le_bytes(data[4..6].try_into().unwrap());
        assert_eq!(format, 0x3009);
    }

    #[test]
    fn format_name_known_formats() {
        assert_eq!(format_name(0x3001), "Association");
        assert_eq!(format_name(0x3009), "MP3");
        assert_eq!(format_name(0xB901), "WMA");
        assert_eq!(format_name(0xB903), "AAC");
    }

    #[test]
    fn format_name_unknown() {
        assert_eq!(format_name(0x1234), "0x1234");
    }

    #[test]
    fn detect_format_mp3() {
        assert_eq!(detect_format("song.mp3"), FORMAT_MP3);
        assert_eq!(detect_format("SONG.MP3"), FORMAT_MP3);
    }

    #[test]
    fn detect_format_wma() {
        assert_eq!(detect_format("track.wma"), FORMAT_WMA);
    }

    #[test]
    fn detect_format_aac() {
        assert_eq!(detect_format("track.aac"), FORMAT_AAC);
        assert_eq!(detect_format("track.m4a"), FORMAT_AAC);
    }

    #[test]
    fn detect_format_defaults_to_mp3() {
        assert_eq!(detect_format("track.flac"), FORMAT_MP3);
        assert_eq!(detect_format("noext"), FORMAT_MP3);
    }

    #[test]
    fn format_exif_jpeg_constant() {
        assert_eq!(FORMAT_EXIF_JPEG, 0x3801);
    }

    #[test]
    fn format_wmv_constant() {
        assert_eq!(FORMAT_WMV, 0xB981);
    }

    #[test]
    fn detect_video_format_wmv() {
        assert_eq!(detect_video_format("clip.wmv"), FORMAT_WMV);
        assert_eq!(detect_video_format("CLIP.WMV"), FORMAT_WMV);
    }

    #[test]
    fn detect_video_format_avi() {
        assert_eq!(detect_video_format("clip.avi"), 0x300A);
    }

    #[test]
    fn detect_video_format_mpeg() {
        assert_eq!(detect_video_format("clip.mpeg"), 0x300B);
        assert_eq!(detect_video_format("clip.mpg"), 0x300B);
    }

    #[test]
    fn detect_video_format_mp4() {
        assert_eq!(detect_video_format("clip.mp4"), 0x300C);
    }

    #[test]
    fn detect_video_format_defaults_to_wmv() {
        assert_eq!(detect_video_format("clip.unknown"), FORMAT_WMV);
        assert_eq!(detect_video_format("noext"), FORMAT_WMV);
    }

    #[test]
    fn format_name_includes_jpeg_and_wmv() {
        assert_eq!(format_name(0x3801), "JPEG");
        assert_eq!(format_name(0xB981), "WMV");
    }

    #[test]
    fn stem_basic() {
        assert_eq!(stem("song.mp3"), "song");
        assert_eq!(stem("my.song.mp3"), "my.song");
        assert_eq!(stem("noext"), "noext");
        assert_eq!(stem(".hidden"), "");
    }

    #[test]
    fn read_metadata_fallback_for_nonexistent_file() {
        let (artist, album, title, track_num, genre) =
            read_metadata("/nonexistent/path.mp3", "path.mp3");
        assert_eq!(artist, "Unknown Artist");
        assert_eq!(album, "Unknown Album");
        assert_eq!(title, "path");
        assert_eq!(track_num, 0);
        assert_eq!(genre, "");
    }

    // -- extract_album_art tests --

    /// Generate a minimal valid WAV file for testing.
    fn make_test_wav(path: &std::path::Path) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let num_samples: usize = 1000;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let data_size =
            (num_samples * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
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

    /// Generate a minimal JPEG image of given dimensions.
    fn make_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    /// Create a WAV file with embedded album art via lofty.
    fn make_wav_with_art(path: &std::path::Path, art_width: u32, art_height: u32) {
        use lofty::file::TaggedFileExt;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::tag::{Accessor, Tag, TagExt};

        make_test_wav(path);
        let art_data = make_test_jpeg(art_width, art_height);
        let mut tagged = lofty::probe::read_from_path(path).unwrap();
        let tag_type = tagged.primary_tag_type();
        if tagged.primary_tag().is_none() {
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().unwrap();
        tag.set_artist("Test Artist".to_string());
        let pic = Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            art_data,
        );
        tag.push_picture(pic);
        tag.save_to_path(path, lofty::config::WriteOptions::default())
            .unwrap();
    }

    #[test]
    fn extract_art_returns_jpeg_200x200() {
        let dir = std::env::temp_dir().join("zytunes-test-extract-art");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("art.wav");
        make_wav_with_art(&wav_path, 400, 400);

        let result = extract_album_art(wav_path.to_str().unwrap());
        assert!(result.is_some(), "expected album art");

        let jpeg_data = result.unwrap();
        let img = image::load_from_memory(&jpeg_data).unwrap();
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_art_no_art_returns_none() {
        let dir = std::env::temp_dir().join("zytunes-test-extract-noart");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("noart.wav");
        make_test_wav(&wav_path);

        let result = extract_album_art(wav_path.to_str().unwrap());
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_art_bad_path_returns_none() {
        let result = extract_album_art("/nonexistent/path/song.mp3");
        assert!(result.is_none());
    }

    // -- read_metadata tests --

    #[test]
    fn read_metadata_from_tagged_file() {
        let dir = std::env::temp_dir().join("zytunes-test-read-meta");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("tagged.wav");
        make_test_wav(&wav_path);

        // Write tags with lofty
        {
            use lofty::file::TaggedFileExt;
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Radiohead".to_string());
            tag.set_album("OK Computer".to_string());
            tag.set_title("Karma Police".to_string());
            tag.set_track(5);
            tag.set_genre("Alternative".to_string());
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let (artist, album, title, track_num, genre) =
            read_metadata(wav_path.to_str().unwrap(), "tagged.wav");
        assert_eq!(artist, "Radiohead");
        assert_eq!(album, "OK Computer");
        assert_eq!(title, "Karma Police");
        assert_eq!(track_num, 5);
        assert_eq!(genre, "Alternative");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn key(artist: &str, album: &str, title: &str) -> (String, String, String) {
        (
            artist.to_lowercase(),
            album.to_lowercase(),
            title.to_lowercase(),
        )
    }

    #[test]
    fn resolve_playlist_handles_prefers_recent_imports() {
        // Regression test for the invisible-playlist bug — when a track was
        // just uploaded in the same session, ZMDB has no real handle for it
        // (object_id = 0). recent_imports must win.
        let mut recent = HashMap::new();
        recent.insert(key("Artist", "Album", "Fresh Track"), 0xDEAD_BEEF);
        let device_tracks = vec![DeviceEntry {
            object_id: 0,
            name: "Artist/Album/Fresh Track".to_string(),
            ..Default::default()
        }];
        let track_keys = vec![(
            "Artist".to_string(),
            "Album".to_string(),
            "Fresh Track".to_string(),
        )];

        let r = resolve_playlist_handles(&track_keys, &recent, &device_tracks);
        assert_eq!(r.member_handles, vec![0xDEAD_BEEF]);
        assert_eq!(r.skipped, 0);
        assert_eq!(r.from_recent, 1);
    }

    #[test]
    fn resolve_playlist_handles_falls_back_to_device_tracks() {
        // Already-on-device tracks: recent_imports is empty, but the device
        // track list (from a prior cache) carries a real object_id.
        let recent = HashMap::new();
        let device_tracks = vec![DeviceEntry {
            object_id: 42,
            name: "Artist/Album/Old Track".to_string(),
            ..Default::default()
        }];
        let track_keys = vec![(
            "ARTIST".to_string(), // case-insensitive lookup
            "album".to_string(),
            "Old Track".to_string(),
        )];

        let r = resolve_playlist_handles(&track_keys, &recent, &device_tracks);
        assert_eq!(r.member_handles, vec![42]);
        assert_eq!(r.skipped, 0);
        assert_eq!(r.from_recent, 0);
    }

    #[test]
    fn resolve_playlist_handles_skips_unresolvable() {
        // Tracks that aren't in either tier count as skipped — playlist
        // still creates with the resolved members, summary surfaces the
        // gap to the user.
        let recent = HashMap::new();
        let device_tracks = vec![];
        let track_keys = vec![(
            "Ghost".to_string(),
            "Phantom".to_string(),
            "Missing".to_string(),
        )];

        let r = resolve_playlist_handles(&track_keys, &recent, &device_tracks);
        assert!(r.member_handles.is_empty());
        assert_eq!(r.skipped, 1);
        assert_eq!(r.from_recent, 0);
    }

    #[test]
    fn resolve_playlist_handles_filters_zmdb_object_id_zero() {
        // ZMDB returns object_id=0 for entries it synthesised from device
        // metadata without a real handle. Those must not pollute by_key —
        // otherwise we'd ship handle=0 to SetObjectReferences.
        let recent = HashMap::new();
        let device_tracks = vec![DeviceEntry {
            object_id: 0,
            name: "Artist/Album/Title".to_string(),
            ..Default::default()
        }];
        let track_keys = vec![(
            "Artist".to_string(),
            "Album".to_string(),
            "Title".to_string(),
        )];

        let r = resolve_playlist_handles(&track_keys, &recent, &device_tracks);
        assert!(r.member_handles.is_empty());
        assert_eq!(r.skipped, 1);
    }

    #[test]
    fn resolve_playlist_handles_dedupes_repeated_tracks() {
        // A user adding the same track twice to a playlist should produce a
        // single handle in member_handles. Order is preserved.
        let mut recent = HashMap::new();
        recent.insert(key("A", "Alb", "T1"), 1);
        recent.insert(key("A", "Alb", "T2"), 2);
        let track_keys = vec![
            ("A".to_string(), "Alb".to_string(), "T1".to_string()),
            ("A".to_string(), "Alb".to_string(), "T2".to_string()),
            ("A".to_string(), "Alb".to_string(), "T1".to_string()),
        ];

        let r = resolve_playlist_handles(&track_keys, &recent, &[]);
        assert_eq!(r.member_handles, vec![1, 2]);
    }

    #[test]
    fn resolve_playlist_handles_recent_wins_over_device() {
        // When both tiers have the same key, recent_imports wins because
        // it carries the authoritative handle from SendObjectPropList. The
        // device-tracks map could be stale (e.g. an old cache mismatch).
        let mut recent = HashMap::new();
        recent.insert(key("A", "Alb", "T"), 100);
        let device_tracks = vec![DeviceEntry {
            object_id: 200,
            name: "A/Alb/T".to_string(),
            ..Default::default()
        }];
        let track_keys = vec![("A".to_string(), "Alb".to_string(), "T".to_string())];

        let r = resolve_playlist_handles(&track_keys, &recent, &device_tracks);
        assert_eq!(r.member_handles, vec![100]);
        assert_eq!(r.from_recent, 1);
    }
}
