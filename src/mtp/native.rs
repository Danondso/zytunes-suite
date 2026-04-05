//! Native MTP session implementing DeviceSession.
//!
//! Uses the zune-mtp crate for direct USB communication, bypassing aft-mtp-cli.

use crate::mtp::parse::DeviceEntry;
use crate::mtp::DeviceSession;

use zune_mtp::container::MTP_ROOT;
use zune_mtp::proplist::*;
use zune_mtp::session::ObjectInfo;
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
const FORMAT_ABSTRACT_AV_PLAYLIST: u16 = 0xBA05;

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
        let cache_dir = std::env::var("HOME").ok().map(PathBuf::from);
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
            let parts: Vec<&str> = line.splitn(5, '\t').collect();
            if parts.len() < 5 {
                continue;
            }
            entries.push(DeviceEntry {
                object_id: parts[0].parse().unwrap_or(0),
                storage_id: parts[1].parse().unwrap_or(0),
                format: parts[2].to_string(),
                size: parts[3].parse().unwrap_or(0),
                name: parts[4].to_string(),
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
            let safe_name = t.name.replace('\t', " ");
            content.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                t.object_id, t.storage_id, t.format, t.size, safe_name
            ));
        }
        let _ = std::fs::write(path, content);
    }

    /// Clear the track cache entirely.
    pub fn clear(&self) {
        if let Some(path) = self.cache_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn append(&self, entry: &DeviceEntry) {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return,
        };
        use std::io::Write;
        let safe_name = entry.name.replace('\t', " ");
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\n",
            entry.object_id, entry.storage_id, entry.format, entry.size, safe_name
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn remove(&self, device_path: &str) {
        let path_suffix = device_path.trim_start_matches("/Music/");
        self.filter_cache(|line| {
            line.splitn(5, '\t')
                .nth(4)
                .map(|name| name != path_suffix)
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
    log: Option<std::sync::mpsc::Sender<String>>,
    library: Option<DeviceLibrary>,
    cache: TrackCache,
    sync_cache_serial: Option<String>,
    sync_restored: bool,
    pub firmware_version: Option<String>,
}

impl NativeSession {
    /// Access the device library state, returning an error if not yet initialized.
    fn lib(&self) -> Result<&DeviceLibrary, String> {
        self.library
            .as_ref()
            .ok_or_else(|| "Device library not initialized".to_string())
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
    pub fn get_acquired_items_count(&mut self) -> Result<u32, String> {
        self.session.get_acquired_items_count().mtp_err()
    }

    /// Query device sync progress (vendor op 0x922f). Returns raw payload bytes.
    pub fn get_sync_progress(&mut self) -> Result<Vec<u8>, String> {
        self.session.get_sync_progress().mtp_err()
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
            log: None,
            library: None,
            cache: TrackCache::new(None),
            sync_cache_serial: None,
            sync_restored: false,
            firmware_version,
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
                    });
                }
            } else {
                entries.push(DeviceEntry {
                    object_id: handle as u64,
                    storage_id: info.storage_id as u64,
                    format: format_str,
                    size: info.compressed_size as u64,
                    name: full_name.clone(),
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
        for handle in handles {
            if let Ok(info) = self.session.get_object_info(handle) {
                if info.filename == name {
                    return Ok(Some(handle));
                }
            }
        }
        Ok(None)
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

    /// Try to load the device library via the ZMDB vendor operation.
    /// Returns DeviceEntry values with synthesized paths matching the device filesystem.
    fn try_zmdb(&mut self) -> Result<Vec<DeviceEntry>, String> {
        let raw = self.session.get_zmdb(1).mtp_err()?;
        let zmdb = crate::mtp::zmdb::Zmdb::parse(&raw)?;
        self.log_msg(&format!("ZMDB: {}", zmdb.summary()));
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
            self.log_msg(&format!(
                "Loaded library cache ({} artists, {} albums)",
                cached_lib.artists.len(),
                cached_lib.albums.len()
            ));
            self.library = Some(cached_lib);
            return Ok(());
        }

        self.log_msg("Initializing device library...");

        // Detect device capabilities.
        // On macOS, we can probe via GetObjectPropsSupported. On Linux, the
        // Zune rejects these queries (timeout or GeneralError 0x2002) even
        // though it supports the features. Since we only target the Zune 30,
        // use known defaults on Linux and probe on macOS.
        //
        // artist_supported defaults to false — the safe path uses Music/
        // subfolders directly. Album date and cover default to true since
        // the Zune 30 supports them but the probe fails on Linux.
        let (artist_supported, album_date_supported, album_cover_supported) =
            if cfg!(target_os = "macos") {
                self.probe_capabilities().unwrap_or((false, true, true))
            } else {
                self.log_msg("Using Zune defaults (artist=false, date=true, cover=true)");
                (false, true, true)
            };

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
                    id
                }
                Err(_) => folder_id, // Fallback to folder ID if artist creation fails.
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

    fn zune_import(&mut self, local_path: &str) -> Result<u64, String> {
        let file_data =
            std::fs::read(local_path).map_err(|e| format!("Cannot read {}: {}", local_path, e))?;

        let filename = std::path::Path::new(local_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("track.mp3");

        // Read metadata from ID3 tags.
        let (artist, album, title, track_num, genre) = read_metadata(local_path, filename);

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
        };
        self.cache.append(&new_entry);

        Ok(track_id as u64)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), String> {
        let handle = self.resolve_path(device_path)?;
        self.session.delete_object(handle).mtp_err()?;
        self.cache.remove(device_path);
        Ok(())
    }

    fn rm_by_id(&mut self, object_id: u32) -> Result<(), String> {
        self.session.delete_object(object_id).mtp_err()?;
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
        if let Some((cached_free, cached)) = self.cache.load() {
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
                self.log_msg(&format!("Loaded {} tracks from cache", cached.len()));
                return Ok(cached);
            }
        }
        self.log_msg("No cache, querying device...");

        // Restore sync progress before device queries (only on cache miss).
        self.restore_sync_progress();

        // Try ZMDB — single MTP call for the entire device library.
        match self.try_zmdb() {
            Ok(tracks) => {
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
        let tracks: Vec<DeviceEntry> = entries.into_iter().filter(|e| !e.is_dir()).collect();

        self.cache.save(&tracks, current_free);
        self.log_msg(&format!("Cached {} tracks", tracks.len()));

        Ok(tracks)
    }

    fn create_playlist(&mut self, name: &str, track_ids: &[u64]) -> Result<(), String> {
        self.ensure_library()?;
        let music_folder = self.lib()?.music_folder;

        // Step 1: Create playlist object via SendObjectPropList.
        let props = PropListBuilder::new()
            .add_string(PROP_OBJECT_FILENAME, &format!("{}.pla", name))
            .build();
        let (_, _, playlist_id) = self
            .session
            .send_object_prop_list(
                self.storage_id,
                music_folder,
                FORMAT_ABSTRACT_AV_PLAYLIST,
                0,
                &props,
            )
            .mtp_err()?;

        // Step 2: Send empty object data.
        self.session.send_object(&[]).mtp_err()?;

        // Step 3: Set display name via SetObjectPropValue.
        let mut name_data = Vec::new();
        let chars: Vec<u16> = name.encode_utf16().collect();
        name_data.push((chars.len() + 1) as u8);
        for ch in &chars {
            name_data.extend_from_slice(&ch.to_le_bytes());
        }
        name_data.extend_from_slice(&0u16.to_le_bytes());
        let _ = self
            .session
            .set_object_prop_value(playlist_id, PROP_NAME, &name_data);

        // Step 4: Link tracks via SetObjectReferences.
        let refs: Vec<u32> = track_ids.iter().map(|&id| id as u32).collect();
        self.session
            .set_object_references(playlist_id, &refs)
            .mtp_err()?;

        Ok(())
    }

    fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
        self.session.get_storage_info(self.storage_id).mtp_err()
    }

    fn save_sync_progress(&mut self) {
        let data = match self.session.get_sync_progress() {
            Ok(d) => d,
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
}

impl NativeSession {
    /// Path for the sync progress cache file.
    /// Path for the device library cache file.
    fn library_cache_path(&self) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let filename = match &self.sync_cache_serial {
            Some(s) => format!(".zytunes-library-cache-{s}"),
            None => ".zytunes-library-cache".to_string(),
        };
        Some(PathBuf::from(home).join(filename))
    }

    /// Save the device library state to disk.
    fn save_library_cache(&self) {
        if let Some(ref lib) = self.library {
            if let Some(path) = self.library_cache_path() {
                let _ = std::fs::write(path, lib.serialize());
            }
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
        let home = std::env::var("HOME").ok()?;
        let filename = match &self.sync_cache_serial {
            Some(s) => format!(".zytunes-sync-progress-{s}"),
            None => ".zytunes-sync-progress".to_string(),
        };
        Some(PathBuf::from(home).join(filename))
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

    /// Probe device capabilities via GetObjectPropsSupported.
    /// Returns None if any query fails (e.g., on Linux where the Zune may reject these).
    fn probe_capabilities(&mut self) -> Option<(bool, bool, bool)> {
        let artist_props = self
            .session
            .get_object_props_supported(FORMAT_ARTIST)
            .ok()?;
        let artist_supported = !artist_props.is_empty();

        let album_props = self
            .session
            .get_object_props_supported(FORMAT_ABSTRACT_AUDIO_ALBUM)
            .ok()?;
        let album_date_supported = album_props.contains(&PROP_DATE_AUTHORED);
        let album_cover_supported = album_props.contains(&PROP_REPRESENTATIVE_SAMPLE_DATA);

        self.log_msg(&format!(
            "Probed caps: artist={} date={} cover={}",
            artist_supported, album_date_supported, album_cover_supported
        ));
        Some((
            artist_supported,
            album_date_supported,
            album_cover_supported,
        ))
    }

    /// Clear the track cache entirely.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    /// Try to extract and set album art on the device. Logs but doesn't fail on errors.
    fn try_set_album_art(&mut self, local_path: &str, album_obj_id: u32) {
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
        let mut art_data = Vec::with_capacity(4 + jpeg_data.len());
        art_data.extend_from_slice(&(jpeg_data.len() as u32).to_le_bytes());
        art_data.extend_from_slice(&jpeg_data);
        match self.session.set_object_prop_value(
            album_obj_id,
            PROP_REPRESENTATIVE_SAMPLE_DATA,
            &art_data,
        ) {
            Ok(_) => self.log_msg("Album art set successfully"),
            Err(e) => self.log_msg(&format!("Album art failed: {}", e)),
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
        0xB901 => "WMA".to_string(),
        0xB903 => "AAC".to_string(),
        0xBA05 => "AbstractAudioVideoPlaylist".to_string(),
        _ => format!("0x{:04x}", format),
    }
}

/// Extract album art from an audio file, resized to 200x200 JPEG.
/// Uses lofty for extraction and image crate for resizing. Works entirely in-memory.
fn extract_album_art(path: &str) -> Option<Vec<u8>> {
    use lofty::file::TaggedFileExt;

    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pic = tag.pictures().first()?;
    let img = image::load_from_memory(pic.data()).ok()?;
    let resized = img.resize_exact(200, 200, image::imageops::FilterType::Lanczos3);
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
        .ok()?;
    let data = jpeg_buf.into_inner();
    if data.is_empty() {
        None
    } else {
        Some(data)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
