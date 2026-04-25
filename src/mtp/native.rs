//! Native MTP session implementing DeviceSession.
//!
//! Uses the zune-mtp crate for direct USB communication, bypassing aft-mtp-cli.

use crate::mtp::parse::DeviceEntry;
use crate::mtp::DeviceSession;

use zune_mtp::container::MTP_ROOT;
use zune_mtp::proplist::*;
use zune_mtp::session::{parse_object_prop_list, ObjectInfo, PropListElement};
use zune_mtp::{MtpError, MtpSession, MtpzKeys};

use std::collections::HashMap;
use std::path::PathBuf;

/// Extension trait to convert MtpError results to String results at the DeviceSession boundary.
trait MtpResultExt<T> {
    fn mtp_err(self) -> Result<T, String>;
}

impl<T> MtpResultExt<T> for Result<T, MtpError> {
    fn mtp_err(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
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

/// Track cache for persisting device track lists across sessions.
pub struct TrackCache {
    serial: Option<String>,
    cache_dir: Option<PathBuf>,
}

impl TrackCache {
    fn new(serial: Option<String>) -> Self {
        let cache_dir = crate::paths::device_cache_base();
        TrackCache { serial, cache_dir }
    }

    fn cache_path(&self) -> Option<PathBuf> {
        let dir = self.cache_dir.as_ref()?;
        let filename = match &self.serial {
            Some(s) => format!(".zytunes-track-cache-{}", s),
            None => ".zytunes-track-cache".to_string(),
        };
        Some(dir.join(filename))
    }

    /// Load cached tracks. Returns `(cached_free_bytes, tracks)`.
    fn load(&self) -> Option<(u64, Vec<DeviceEntry>)> {
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
            // (column 5) was added in Phase 4b alongside rating (column 6).
            // Old caches still parse — missing fields stay `None`.
            let parts: Vec<&str> = line.splitn(7, '\t').collect();
            if parts.len() < 5 {
                continue;
            }
            let play_count = parts
                .get(5)
                .and_then(|s| s.trim_end_matches('\n').parse::<u32>().ok());
            let rating = parts
                .get(6)
                .and_then(|s| s.trim_end_matches('\n').parse::<u16>().ok());
            entries.push(DeviceEntry {
                object_id: parts[0].parse().unwrap_or(0),
                storage_id: parts[1].parse().unwrap_or(0),
                format: parts[2].to_string(),
                size: parts[3].parse().unwrap_or(0),
                name: parts[4].to_string(),
                play_count,
                rating,
                ..Default::default()
            });
        }
        if entries.is_empty() {
            None
        } else {
            Some((free_bytes, entries))
        }
    }

    fn save(&self, tracks: &[DeviceEntry], free_bytes: u64) {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return,
        };
        let mut content = format!("#free_bytes:{free_bytes}\n");
        for t in tracks {
            content.push_str(&serialize_entry(t));
        }
        let _ = std::fs::write(path, content);
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
    fn update_free_bytes(&self, free_bytes: u64) {
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

    fn append(&self, entry: &DeviceEntry) {
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

    fn remove(&self, device_path: &str) {
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

    fn remove_by_id(&self, object_id: u32) {
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
}

impl NativeSession {
    /// Access the device library state, returning an error if not yet initialized.
    fn lib(&self) -> Result<&DeviceLibrary, String> {
        self.library
            .as_ref()
            .ok_or_else(|| "Device library not initialized".to_string())
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
    pub fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
        self.session.get_storage_info(self.storage_id).mtp_err()
    }

    /// Query the number of items the device acquired on its own
    /// (podcast downloads, Zune-to-Zune sharing).
    ///
    /// Returns `Ok(None)` when the device reports the vendor op as unsupported
    /// (older firmware). Callers should treat that as "feature unavailable"
    /// rather than a hard error.
    pub fn get_acquired_items_count(&mut self) -> Result<Option<u32>, String> {
        if !self.supports_modern_vendor_ops() {
            return Ok(None);
        }
        match self.session.get_acquired_items_count() {
            Ok(count) => Ok(Some(count)),
            Err(e) if e.is_operation_not_supported() => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Query device sync progress (vendor op 0x922f). Returns raw payload bytes,
    /// or `Ok(None)` if the device doesn't support the query.
    pub fn get_sync_progress(&mut self) -> Result<Option<Vec<u8>>, String> {
        if !self.supports_modern_vendor_ops() {
            return Ok(None);
        }
        match self.session.get_sync_progress() {
            Ok(raw) => Ok(Some(raw)),
            Err(e) if e.is_operation_not_supported() => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Open a native MTP session to the Zune.
    /// Performs device detection, MTP session open, and MTPZ authentication.
    /// The `log` callback receives diagnostic messages for each step.
    pub fn open(product_id: u16, log: &dyn Fn(&str)) -> Result<Self, String> {
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
        })
    }

    /// Recursively list all objects under a given parent handle, building
    /// the relative path prefix for each entry.
    fn list_recursive(
        &mut self,
        parent: u32,
        prefix: &str,
        entries: &mut Vec<DeviceEntry>,
    ) -> Result<(), String> {
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
    fn find_object(&mut self, parent: u32, name: &str) -> Result<Option<u32>, String> {
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
    fn find_or_create_folder(&mut self, parent: u32, name: &str) -> Result<u32, String> {
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
    fn resolve_path(&mut self, path: &str) -> Result<u32, String> {
        let parts: Vec<&str> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current = MTP_ROOT;
        for part in parts {
            match self.find_object(current, part)? {
                Some(handle) => current = handle,
                None => return Err(format!("Path not found: {}", path)),
            }
        }
        Ok(current)
    }

    /// Enrich `tracks` with `play_count` (from `0xDC91 UseCount`) and
    /// `rating` (from `0xDC8A Rating`) using bulk `GetObjectPropList`
    /// queries — one round-trip per property for the whole library, not
    /// one per track. Tracks whose `object_id` is `0` (ZMDB entries before
    /// any cache merge has restored the handle) are skipped silently.
    ///
    /// Both queries are best-effort and independent: if the device rejects
    /// one, we log it and proceed with the other. Confirmed to work on Zune
    /// v1.4 firmware `01.04.00485.00-00425` even though these props are not
    /// listed in `GetObjectPropsSupported(0x3009)`.
    fn enrich_with_playcounts(&mut self, tracks: &mut [DeviceEntry]) {
        // Avoid needless round-trips when nothing in the list could be
        // matched — e.g. ZMDB on a fresh connect with no prior cache.
        if !tracks.iter().any(|t| t.object_id != 0) {
            return;
        }
        self.enrich_one_prop(tracks, PROP_USE_COUNT, "playcounts", apply_playcounts);
        self.enrich_one_prop(tracks, PROP_RATING, "ratings", apply_ratings);
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
    fn try_zmdb(&mut self) -> Result<Vec<DeviceEntry>, String> {
        if !self.supports_modern_vendor_ops() {
            let v = self.firmware_version.as_deref().unwrap_or("unknown");
            return Err(format!(
                "firmware {v} predates ZMDB (added in firmware 3.0)"
            ));
        }
        let raw = self.session.get_zmdb(1).map_err(|e| {
            if e.is_operation_not_supported() {
                "device does not support ZMDB bulk query".to_string()
            } else {
                e.to_string()
            }
        })?;
        let zmdb = crate::mtp::zmdb::Zmdb::parse(&raw)?;
        self.log_msg(&format!("ZMDB: {}", zmdb.summary()));
        self.zmdb_video_cache = Some(zmdb.to_video_entries());
        Ok(zmdb.to_device_entries())
    }

    /// Initialize the device library — find/create Music, Artists, Albums folders
    /// and scan existing artists+albums.
    fn ensure_library(&mut self) -> Result<(), String> {
        if self.library.is_some() {
            return Ok(());
        }

        // Try loading from disk cache first.
        if let Some(cached_lib) = self.load_library_cache() {
            // Self-correct caches written before the HD-specific
            // artist_supported default existed — HD (pid=0x063e) requires
            // artist_supported=true or tracks file as "Unknown Artist".
            let expected_artist_supported = self.product_id == 0x063e;
            if cached_lib.caps.artist_supported != expected_artist_supported {
                self.log_msg(&format!(
                    "Library cache has artist_supported={}, expected {} for pid=0x{:04x} — rebuilding",
                    cached_lib.caps.artist_supported, expected_artist_supported, self.product_id
                ));
            } else {
                self.log_msg(&format!(
                    "Loaded library cache ({} artists, {} albums)",
                    cached_lib.artists.len(),
                    cached_lib.albums.len()
                ));
                self.library = Some(cached_lib);
                return Ok(());
            }
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

        // Find root folders.
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
        let artists_folder = artists_folder.unwrap_or(music_folder);
        let albums_folder = albums_folder.unwrap_or(music_folder);

        // Scan existing artists from the Artists folder.
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
        self.log_msg(&format!("Found {} existing artists", artists.len()));

        // Scan existing albums from the Albums folder.
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

    /// Find or create an artist object + music folder. Returns (artist_id, music_folder_id).
    fn find_or_create_artist_native(&mut self, name: &str) -> Result<(u32, u32), String> {
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
    ) -> Result<(u32, u32), String> {
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
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
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
    ) -> Result<u64, String> {
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

        Ok(track_id as u64)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), String> {
        let handle = self.resolve_path(device_path)?;
        self.delete_recursive(handle)?;
        self.cache.remove(device_path);
        self.invalidate_library_for_path(device_path);
        Ok(())
    }

    fn rm_by_id(&mut self, object_id: u32) -> Result<(), String> {
        self.delete_recursive(object_id)?;
        self.cache.remove_by_id(object_id);
        Ok(())
    }

    fn cleanup_empty_folders(&mut self) -> Result<usize, String> {
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
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
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
                let with_pc = cached.iter().filter(|t| t.play_count.is_some()).count();
                self.log_msg(&format!(
                    "Loaded {} tracks from cache ({with_pc} with playcount)",
                    cached.len()
                ));
                return Ok(cached.clone());
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
                self.cache.save(&tracks, current_free);
                return Ok(tracks);
            }
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
        self.cache.save(&tracks, current_free);
        self.log_msg(&format!("Cached {} tracks", tracks.len()));

        Ok(tracks)
    }

    fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
        self.session.get_storage_info(self.storage_id).mtp_err()
    }

    fn prewarm_library(&mut self) -> Result<(), String> {
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

    fn import_photo(&mut self, filename: &str, jpeg_data: &[u8]) -> Result<u64, String> {
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

    fn import_video(&mut self, filename: &str, data: &[u8]) -> Result<u64, String> {
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

    fn collect_all_videos(&mut self) -> Result<Vec<DeviceEntry>, String> {
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
}

impl NativeSession {
    /// Phase 4a probe: list MTP object properties the device advertises for
    /// the given object format. `format = 0x3009` is MP3 (the most common
    /// Zune content type). Returns the property codes in the same order the
    /// device emits them.
    pub fn probe_supported_props(&mut self, format: u16) -> Result<Vec<u16>, String> {
        self.session
            .get_object_props_supported(format)
            .map_err(|e| e.to_string())
    }

    /// Phase 4a probe: read a single MTP object property as raw bytes.
    /// Caller decodes per the property's MTP data type.
    pub fn probe_prop_value(&mut self, object_id: u32, prop: u16) -> Result<Vec<u8>, String> {
        self.session
            .get_object_prop_value(object_id, prop)
            .map_err(|e| e.to_string())
    }

    /// Phase 4a probe: read a u32 MTP object property. `Ok(None)` when
    /// the device returns no value bytes (interpreted as "unset").
    pub fn probe_prop_u32(&mut self, object_id: u32, prop: u16) -> Result<Option<u32>, String> {
        self.session
            .get_object_prop_u32(object_id, prop)
            .map_err(|e| e.to_string())
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
    ) -> Result<Vec<zune_mtp::session::PropListElement>, String> {
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
    pub fn probe_vendor_op(&mut self, op_code: u16, params: &[u32]) -> Result<Vec<u8>, String> {
        // Empirical hard-block list. 0x9180 is documented in the v1.4
        // vendor-ops survey as causing a USB-resetting hang. Adding more
        // here as we learn them is cheap insurance.
        const BLOCKED: &[u16] = &[0x9180];
        if BLOCKED.contains(&op_code) {
            return Err(format!(
                "op 0x{op_code:04X} is on the probe blocklist (known to wedge the device)"
            ));
        }
        self.session
            .execute_data_in_raw(op_code, params)
            .map_err(|e| e.to_string())
    }

    /// Phase 4a probe: dump the raw ZMDB binary (vendor op `0x9217`). Used
    /// for diff-based playcount investigation: dump before, play a track on
    /// the device, dump after, hex-diff the binaries to find changed bytes
    /// the parser currently ignores.
    ///
    /// Mirrors the firmware-version guard from `try_zmdb` so users on v1.4
    /// hardware get a friendly message instead of a bare
    /// `0x2005 OperationNotSupported` from the device.
    pub fn probe_zmdb_dump(&mut self) -> Result<Vec<u8>, String> {
        if !self.supports_modern_vendor_ops() {
            let v = self.firmware_version.as_deref().unwrap_or("unknown");
            return Err(format!(
                "firmware {v} predates ZMDB (added in firmware 3.0); ZMDB dump unavailable"
            ));
        }
        self.session.get_zmdb(1).map_err(|e| {
            if e.is_operation_not_supported() {
                "device does not support ZMDB bulk query".to_string()
            } else {
                e.to_string()
            }
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
    pub fn probe_first_audio_handle(&mut self) -> Result<Option<(u32, u16)>, String> {
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
    fn delete_recursive(&mut self, handle: u32) -> Result<(), String> {
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
        let parts: Vec<&str> = device_path
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if parts.first().copied() != Some("Music") || parts.len() < 2 {
            return;
        }
        let artist = parts[1].to_string();
        if self.library.is_none() {
            self.library = self.load_library_cache();
        }
        let Some(lib) = self.library.as_mut() else {
            return;
        };
        match parts.len() {
            2 => {
                lib.albums.retain(|(a, _), _| a != &artist);
                lib.artists.remove(&artist);
            }
            _ => {
                let album = parts[2].to_string();
                lib.albums.remove(&(artist, album));
            }
        }
        self.save_library_cache();
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

    /// Clear the track cache entirely.
    pub fn clear_cache(&self) {
        self.cache.clear();
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
/// Serialize one `DeviceEntry` to its on-disk track-cache line. Tabs in the
/// name are flattened to spaces so the splitn(7, '\t') loader stays in sync.
/// `play_count` and `rating` are emitted as digits or empty for `None`.
fn serialize_entry(entry: &DeviceEntry) -> String {
    let safe_name = entry.name.replace('\t', " ");
    let pc = entry.play_count.map(|v| v.to_string()).unwrap_or_default();
    let rt = entry.rating.map(|v| v.to_string()).unwrap_or_default();
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        entry.object_id, entry.storage_id, entry.format, entry.size, safe_name, pc, rt
    )
}

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `play_count` for entries whose `object_id` matches a returned handle.
/// Tracks without an `object_id` (ZMDB entries on a fresh connect) keep
/// `play_count = None`. Pure helper so the join logic is unit-testable
/// without a live MTP session.
fn apply_playcounts(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_USE_COUNT {
            if let Some(v) = e.as_u32() {
                counts.insert(e.object_handle, v);
            }
        }
    }
    if counts.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&n) = counts.get(&(t.object_id as u32)) {
            t.play_count = Some(n);
        }
    }
}

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `rating` for entries whose `object_id` matches a returned handle. Same
/// shape as `apply_playcounts` but for `0xDC8A Rating` (UINT16). Tracks
/// without `object_id` are skipped silently.
fn apply_ratings(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut ratings: HashMap<u32, u16> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_RATING {
            if let Some(v) = e.as_u16() {
                ratings.insert(e.object_handle, v);
            }
        }
    }
    if ratings.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&r) = ratings.get(&(t.object_id as u32)) {
            t.rating = Some(r);
        }
    }
}

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

/// Extract album art from an audio file, resized to 200x200 JPEG.
/// Uses lofty for extraction and image crate for resizing. Works entirely in-memory.
/// Extract and re-encode album art as a conservative 200x200 baseline JPEG.
///
/// We go through explicit RGB8 and a concrete `JpegEncoder` (rather than
/// `DynamicImage::write_to(Jpeg)`) to keep the output defensive:
///   - Force RGB8: strips alpha / palette / CMYK / grayscale / 16-bit quirks.
///   - Explicit baseline quality: no progressive scan, no surprise defaults.
///   - No ICC / EXIF / XMP carried over from the source picture.
///   - Validate SOI/EOI markers so we never hand the Zune a truncated stream.
///
/// Context: the Zune 30 firmware occasionally wedges (ReadPipe timeout with
/// no response code) when `SetObjectPropValue` is called with certain JPEGs.
/// A single wedge poisons the whole MTP session. Normalizing the output
/// through a minimal, metadata-free encoder drops the probability of hitting
/// the decoder bug.
fn extract_album_art(path: &str) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ExtendedColorType;
    use lofty::file::TaggedFileExt;

    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pic = tag.pictures().first()?;
    let img = image::load_from_memory(pic.data()).ok()?;
    let resized = img.resize_exact(200, 200, image::imageops::FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut jpeg_buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, 85);
    encoder
        .encode(rgb.as_raw(), 200, 200, ExtendedColorType::Rgb8)
        .ok()?;

    // Round-trip pass: decode the freshly encoded JPEG and re-encode at the
    // same quality. mtp-probe album-art-check showed v1.4 firmware hangs on
    // certain first-pass byte patterns (e.g. a ~21 KB first-pass JPEG) but
    // accepts the round-tripped output (20938 bytes) from the same source.
    // Re-encoding from already-quantized DCT data smooths the high-frequency
    // content just enough to dodge the firmware's prop-handler bug.
    let final_buf = match image::load_from_memory(&jpeg_buf) {
        Ok(img2) => {
            let rgb2 = img2.to_rgb8();
            let mut buf2: Vec<u8> = Vec::with_capacity(jpeg_buf.len());
            let mut enc2 = JpegEncoder::new_with_quality(&mut buf2, 85);
            if enc2
                .encode(rgb2.as_raw(), 200, 200, ExtendedColorType::Rgb8)
                .is_ok()
            {
                buf2
            } else {
                jpeg_buf
            }
        }
        Err(_) => jpeg_buf,
    };

    // Sanity: baseline JPEG starts with FFD8 (SOI) and ends with FFD9 (EOI).
    // If the encoder produced something truncated, don't send it.
    if final_buf.len() < 4
        || final_buf[..2] != [0xFF, 0xD8]
        || final_buf[final_buf.len() - 2..] != [0xFF, 0xD9]
    {
        return None;
    }
    Some(final_buf)
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
        cache.save(&entries, 5_000_000);

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
        cache.save(&entries, 0);

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
        cache.save(&entries, 0);

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
        cache.save(&entries, 0);

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
        cache.save(&entries, 0);

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
        cache.save(&[sample_entry("x.mp3", 1)], 0);
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
        cache.save(&entries, 28_000_000_000);

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
        cache.save(&entries, 1_000_000);

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
        cache.save(&entries, 1_000_000);

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
        cache.save(&entries, 0);

        let (_, loaded) = cache.load().unwrap();
        // Tab should be replaced with space in saved format.
        assert_eq!(loaded[0].name, "Art ist/Album/track.mp3");

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
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

    fn use_count_element(handle: u32, count: u32) -> PropListElement {
        PropListElement {
            object_handle: handle,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: count.to_le_bytes().to_vec(),
        }
    }

    #[test]
    fn apply_playcounts_populates_matched_handles() {
        let mut tracks = vec![sample_entry("a.mp3", 100), sample_entry("b.mp3", 101)];
        let elements = vec![use_count_element(100, 7), use_count_element(101, 0)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, Some(7));
        assert_eq!(tracks[1].play_count, Some(0));
    }

    #[test]
    fn apply_playcounts_leaves_unmatched_handles_alone() {
        // Track 102 has no element in the prop-list response; it should
        // stay None, not be cleared, even if play_count was already set.
        let mut tracks = vec![
            sample_entry("a.mp3", 100),
            DeviceEntry {
                play_count: Some(99),
                ..sample_entry("b.mp3", 102)
            },
        ];
        let elements = vec![use_count_element(100, 5)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, Some(5));
        assert_eq!(tracks[1].play_count, Some(99));
    }

    #[test]
    fn apply_playcounts_skips_zero_object_ids() {
        // ZMDB tracks before any cache merge have object_id == 0; they must
        // not match an element whose handle was reported as 0 by the device.
        let mut tracks = vec![sample_entry("zmdb-only.mp3", 0)];
        let elements = vec![use_count_element(0, 12)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, None);
    }

    #[test]
    fn apply_playcounts_ignores_non_use_count_props() {
        let mut tracks = vec![sample_entry("a.mp3", 200)];
        let rating = PropListElement {
            object_handle: 200,
            prop_code: PROP_RATING,
            datatype: 0x0004,
            value: 4u16.to_le_bytes().to_vec(),
        };
        apply_playcounts(&mut tracks, &[rating]);
        assert_eq!(tracks[0].play_count, None);
    }

    #[test]
    fn apply_ratings_populates_matched_handles() {
        let mut tracks = vec![sample_entry("a.mp3", 300), sample_entry("b.mp3", 301)];
        let elements = vec![
            PropListElement {
                object_handle: 300,
                prop_code: PROP_RATING,
                datatype: 0x0004,
                value: 80u16.to_le_bytes().to_vec(),
            },
            PropListElement {
                object_handle: 301,
                prop_code: PROP_RATING,
                datatype: 0x0004,
                value: 0u16.to_le_bytes().to_vec(),
            },
        ];
        apply_ratings(&mut tracks, &elements);
        assert_eq!(tracks[0].rating, Some(80));
        assert_eq!(tracks[1].rating, Some(0));
    }

    #[test]
    fn apply_ratings_ignores_use_count_props() {
        // A UseCount element must not be misinterpreted as a rating.
        let mut tracks = vec![sample_entry("a.mp3", 400)];
        let use_count = PropListElement {
            object_handle: 400,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: 5u32.to_le_bytes().to_vec(),
        };
        apply_ratings(&mut tracks, &[use_count]);
        assert_eq!(tracks[0].rating, None);
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
        cache.save(&entries, 1_000);

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
        cache.save(&entries, 1_000);

        let (_, loaded) = cache.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].rating, Some(80));
        assert_eq!(loaded[1].rating, None);

        if let Some(p) = cache.cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }
}
