//! Native IOKit MTP session implementing DeviceSession.
//!
//! Uses the zune-mtp crate for direct USB communication, bypassing aft-mtp-cli.

use crate::mtp::parse::DeviceEntry;
use crate::mtp::DeviceSession;

use zune_mtp::container::MTP_ROOT;
use zune_mtp::proplist::*;
use zune_mtp::session::ObjectInfo;
use zune_mtp::{MtpSession, MtpzKeys};

use id3::TagLike;
use std::collections::HashMap;
use std::path::PathBuf;

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

/// Native MTP session using IOKit.
pub struct NativeSession {
    session: MtpSession,
    storage_id: u32,
    log: Option<std::sync::mpsc::Sender<String>>,
    library: Option<DeviceLibrary>,
}

impl NativeSession {
    /// Set a log channel for progress messages.
    pub fn set_log_sender(&mut self, tx: std::sync::mpsc::Sender<String>) {
        self.log = Some(tx);
    }

    fn log_msg(&self, msg: &str) {
        if let Some(ref tx) = self.log {
            let _ = tx.send(msg.to_string());
        }
    }

    /// Query storage info. Returns (total_bytes, free_bytes).
    pub fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
        self.session.get_storage_info(self.storage_id)
    }

    /// Open a native IOKit MTP session to the Zune.
    /// Performs device detection, MTP session open, and MTPZ authentication.
    /// The `log` callback receives diagnostic messages for each step.
    pub fn open(
        product_id: u16,
        log: &dyn Fn(&str),
    ) -> Result<Self, String> {
        log("IOKit: Opening USB device...");
        let mut session = MtpSession::open(MICROSOFT_VENDOR_ID, product_id)
            .map_err(|e| format!("IOKit USB open failed: {e}"))?;
        log("IOKit: USB device opened, MTP session started");

        log("IOKit: Loading MTPZ keys from ~/.mtpz-data...");
        let keys = MtpzKeys::load_default()
            .map_err(|e| format!("IOKit MTPZ keys failed: {e}"))?;
        log("IOKit: Keys loaded, starting MTPZ handshake...");

        zune_mtp::mtpz::authenticate(&mut session, &keys, log)
            .map_err(|e| format!("IOKit MTPZ handshake failed: {e}"))?;
        log("IOKit: MTPZ handshake complete");

        log("IOKit: Querying storage...");
        let storage_ids = session
            .get_storage_ids()
            .map_err(|e| format!("IOKit get storage failed: {e}"))?;
        let storage_id = storage_ids
            .first()
            .copied()
            .ok_or("IOKit: No storage found on device")?;
        log(&format!("IOKit: Using storage {}", storage_id));

        Ok(NativeSession {
            session,
            storage_id,
            log: None,
            library: None,
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
            .get_object_handles(self.storage_id, parent)?;

        for handle in handles {
            let info = match self.session.get_object_info(handle) {
                Ok(info) => info,
                Err(_) => continue,
            };

            let full_name = if prefix.is_empty() {
                // Top-level = artist names. Log progress.
                self.log_msg(&format!("Scanning: {}", info.filename));
                info.filename.clone()
            } else {
                format!("{}/{}", prefix, info.filename)
            };

            let format_str = if info.object_format == ASSOCIATION_FORMAT {
                "Association".to_string()
            } else {
                format_name(info.object_format)
            };

            entries.push(DeviceEntry {
                object_id: handle as u64,
                storage_id: info.storage_id as u64,
                format: format_str,
                size: info.compressed_size as u64,
                name: full_name.clone(),
            });

            // Recurse into directories.
            if info.object_format == ASSOCIATION_FORMAT {
                self.list_recursive(handle, &full_name, entries)?;
            }
        }
        Ok(())
    }

    /// Find an object handle by name under a parent.
    fn find_object(&mut self, parent: u32, name: &str) -> Result<Option<u32>, String> {
        let handles = self
            .session
            .get_object_handles(self.storage_id, parent)?;
        for handle in handles {
            if let Ok(info) = self.session.get_object_info(handle) {
                if info.filename == name {
                    return Ok(Some(handle));
                }
            }
        }
        Ok(None)
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

    /// Initialize the device library — find/create Music, Artists, Albums folders
    /// and scan existing artists+albums.
    fn ensure_library(&mut self) -> Result<(), String> {
        if self.library.is_some() {
            return Ok(());
        }

        self.log_msg("Initializing device library...");

        // Detect device capabilities.
        let artist_supported = self
            .session
            .get_object_props_supported(FORMAT_ARTIST)
            .map(|props| !props.is_empty())
            .unwrap_or(false);

        let album_props = self
            .session
            .get_object_props_supported(FORMAT_ABSTRACT_AUDIO_ALBUM)
            .unwrap_or_default();
        let album_date_supported = album_props.contains(&PROP_DATE_AUTHORED);
        let album_cover_supported = album_props.contains(&PROP_REPRESENTATIVE_SAMPLE_DATA);

        self.log_msg(&format!(
            "Caps: artist={} date={} cover={}",
            artist_supported, album_date_supported, album_cover_supported
        ));

        // Find root folders.
        let root_handles = self
            .session
            .get_object_handles(self.storage_id, MTP_ROOT)?;

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

        let music_folder = music_folder.ok_or("Music folder not found on device")?;
        let artists_folder = artists_folder.unwrap_or(music_folder);
        let albums_folder = albums_folder.unwrap_or(music_folder);

        self.library = Some(DeviceLibrary {
            music_folder,
            artists_folder,
            albums_folder,
            caps: DeviceCaps {
                artist_supported,
                album_date_supported,
                album_cover_supported,
            },
            artists: HashMap::new(),
            albums: HashMap::new(),
        });

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

        let music_folder = self.library.as_ref().unwrap().music_folder;
        let artists_folder = self.library.as_ref().unwrap().artists_folder;
        let artist_supported = self.library.as_ref().unwrap().caps.artist_supported;

        // Create music subfolder: /Music/{Artist}/
        let folder_id = match self.find_object(music_folder, name)? {
            Some(h) => h,
            None => {
                self.log_msg(&format!("Creating folder: Music/{}", name));
                let props = PropListBuilder::new()
                    .add_string(PROP_OBJECT_FILENAME, name)
                    .build();
                let (_, _, id) = self.session.send_object_prop_list(
                    self.storage_id, music_folder, ASSOCIATION_FORMAT, 0, &props,
                )?;
                id
            }
        };

        // Create Artist MTP object if supported.
        let artist_id = if artist_supported {
            let props = PropListBuilder::new()
                .add_string(PROP_NAME, name)
                .add_string(PROP_OBJECT_FILENAME, &format!("{}.art", name))
                .build();
            match self.session.send_object_prop_list(
                self.storage_id, artists_folder, FORMAT_ARTIST, 0, &props,
            ) {
                Ok((_, _, id)) => id,
                Err(_) => folder_id, // Fallback to folder ID if artist creation fails.
            }
        } else {
            folder_id
        };

        if let Some(lib) = &mut self.library {
            lib.artists.insert(name.to_string(), ArtistInfo {
                id: artist_id,
                music_folder_id: folder_id,
            });
        }

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

        let albums_folder = self.library.as_ref().unwrap().albums_folder;
        let artist_folder = self.library.as_ref()
            .and_then(|l| l.artists.get(artist_name))
            .map(|a| a.music_folder_id)
            .unwrap_or(self.library.as_ref().unwrap().music_folder);

        // Create music subfolder: /Music/{Artist}/{Album}/
        let folder_id = match self.find_object(artist_folder, album_name)? {
            Some(h) => h,
            None => {
                self.log_msg(&format!("Creating folder: {}/{}", artist_name, album_name));
                let props = PropListBuilder::new()
                    .add_string(PROP_OBJECT_FILENAME, album_name)
                    .build();
                let (_, _, id) = self.session.send_object_prop_list(
                    self.storage_id, artist_folder, ASSOCIATION_FORMAT, 0, &props,
                )?;
                id
            }
        };

        // Create AbstractAudioAlbum MTP object.
        let mut props = PropListBuilder::new();
        if artist_supported {
            props.add_u32(PROP_ARTIST_ID, artist_id);
        } else {
            props.add_string(PROP_ARTIST, artist_name);
        }
        props.add_string(PROP_NAME, album_name);
        props.add_string(PROP_OBJECT_FILENAME, &format!("{}--{}.alb", artist_name, album_name));
        let album_data = props.build();

        let album_id = match self.session.send_object_prop_list(
            self.storage_id, albums_folder, FORMAT_ABSTRACT_AUDIO_ALBUM, 0, &album_data,
        ) {
            Ok((_, _, id)) => id,
            Err(_) => folder_id, // Fallback if album creation fails.
        };

        if let Some(lib) = &mut self.library {
            lib.albums.insert(key, AlbumInfo {
                id: album_id,
                music_folder_id: folder_id,
            });
        }

        Ok((album_id, folder_id))
    }
}

impl DeviceSession for NativeSession {
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
        let parent = self.resolve_path(path)?;
        let handles = self
            .session
            .get_object_handles(self.storage_id, parent)?;

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
        let file_data = std::fs::read(local_path)
            .map_err(|e| format!("Cannot read {}: {}", local_path, e))?;

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
        let (_, _, track_id) = self.session.send_object_prop_list(
            self.storage_id,
            album_folder,
            format,
            file_data.len() as u64,
            &prop_data,
        )?;

        // Upload the actual audio file.
        self.session.send_object(&file_data)?;

        // Link track to album via object references.
        if let Ok(mut refs) = self.session.get_object_references(album_obj_id) {
            refs.push(track_id);
            let _ = self.session.set_object_references(album_obj_id, &refs);
        } else {
            let _ = self.session.set_object_references(album_obj_id, &[track_id]);
        }

        // Set album art if available and supported.
        if self.library.as_ref().map(|l| l.caps.album_cover_supported).unwrap_or(false) {
            if let Ok(tag) = id3::Tag::read_from_path(local_path) {
                if let Some(pic) = tag.pictures().next() {
                    let mut art_data = Vec::with_capacity(4 + pic.data.len());
                    art_data.extend_from_slice(&(pic.data.len() as u32).to_le_bytes());
                    art_data.extend_from_slice(&pic.data);
                    let _ = self.session.set_object_prop_value(
                        album_obj_id,
                        PROP_REPRESENTATIVE_SAMPLE_DATA,
                        &art_data,
                    );
                }
            }
        }

        // Update the track cache incrementally.
        let new_entry = DeviceEntry {
            object_id: track_id as u64,
            storage_id: self.storage_id as u64,
            format: format_name(format),
            size: file_data.len() as u64,
            name: format!("{}/{}/{}", artist, album, filename),
        };
        Self::append_to_cache(&new_entry);

        Ok(track_id as u64)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), String> {
        let handle = self.resolve_path(device_path)?;
        self.session.delete_object(handle)?;
        Self::remove_from_cache(device_path);
        Ok(())
    }

    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
        // Try loading from cache first.
        if let Some(cached) = Self::load_cache() {
            self.log_msg(&format!("Loaded {} tracks from cache", cached.len()));
            return Ok(cached);
        }

        let parent = match self.resolve_path(path) {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };

        self.log_msg("Scanning device (first time may take a minute)...");
        let mut entries = Vec::new();
        self.list_recursive(parent, "", &mut entries)?;
        let tracks: Vec<DeviceEntry> =
            entries.into_iter().filter(|e| !e.is_dir()).collect();

        // Save to cache for next time.
        Self::save_cache(&tracks);
        self.log_msg(&format!("Cached {} tracks", tracks.len()));

        Ok(tracks)
    }

    fn create_playlist(
        &mut self,
        name: &str,
        track_ids: &[u64],
    ) -> Result<(), String> {
        self.ensure_library()?;
        let music_folder = self.library.as_ref().unwrap().music_folder;

        // Step 1: Create playlist object via SendObjectPropList.
        let props = PropListBuilder::new()
            .add_string(PROP_OBJECT_FILENAME, &format!("{}.pla", name))
            .build();
        let (_, _, playlist_id) = self.session.send_object_prop_list(
            self.storage_id,
            music_folder,
            FORMAT_ABSTRACT_AV_PLAYLIST,
            0,
            &props,
        )?;

        // Step 2: Send empty object data.
        self.session.send_object(&[])?;

        // Step 3: Set display name via SetObjectPropValue.
        let mut name_data = Vec::new();
        let chars: Vec<u16> = name.encode_utf16().collect();
        name_data.push((chars.len() + 1) as u8);
        for ch in &chars {
            name_data.extend_from_slice(&ch.to_le_bytes());
        }
        name_data.extend_from_slice(&0u16.to_le_bytes());
        let _ = self.session.set_object_prop_value(playlist_id, 0xDC44, &name_data);

        // Step 4: Link tracks via SetObjectReferences.
        let refs: Vec<u32> = track_ids.iter().map(|&id| id as u32).collect();
        self.session.set_object_references(playlist_id, &refs)?;

        Ok(())
    }
}

impl NativeSession {
    fn cache_path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".zytunes-track-cache"))
    }

    fn load_cache() -> Option<Vec<DeviceEntry>> {
        let path = Self::cache_path()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let mut entries = Vec::new();
        for line in content.lines() {
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
            Some(entries)
        }
    }

    fn save_cache(tracks: &[DeviceEntry]) {
        let path = match Self::cache_path() {
            Some(p) => p,
            None => return,
        };
        let mut content = String::new();
        for t in tracks {
            // Strip tabs from name to preserve the tab-delimited cache format.
            let safe_name = t.name.replace('\t', " ");
            content.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                t.object_id, t.storage_id, t.format, t.size, safe_name
            ));
        }
        let _ = std::fs::write(path, content);
    }

    /// Clear the track cache entirely.
    pub fn clear_cache() {
        if let Some(path) = Self::cache_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Append a single track to the cache file.
    fn append_to_cache(entry: &DeviceEntry) {
        let path = match Self::cache_path() {
            Some(p) => p,
            None => return,
        };
        use std::io::Write;
        // Strip tabs from name to preserve the tab-delimited cache format.
        let safe_name = entry.name.replace('\t', " ");
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\n",
            entry.object_id, entry.storage_id, entry.format, entry.size, safe_name
        );
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Remove a track from the cache by device path.
    fn remove_from_cache(device_path: &str) {
        let path = match Self::cache_path() {
            Some(p) => p,
            None => return,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        // Filter out lines whose name (5th field) matches the path suffix.
        let path_suffix = device_path.trim_start_matches("/Music/");
        let filtered: String = content
            .lines()
            .filter(|line| {
                line.splitn(5, '\t')
                    .nth(4)
                    .map(|name| name != path_suffix)
                    .unwrap_or(true)
            })
            .map(|line| format!("{}\n", line))
            .collect();
        let _ = std::fs::write(path, filtered);
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

/// Read metadata from an audio file (ID3 tags for MP3).
/// Returns (artist, album, title, track_number, genre).
fn read_metadata(path: &str, filename: &str) -> (String, String, String, u16, String) {
    // Try ID3 tags first.
    if let Ok(tag) = id3::Tag::read_from_path(path) {
        let artist = tag
            .artist()
            .unwrap_or("Unknown Artist")
            .to_string();
        let album = tag
            .album()
            .unwrap_or("Unknown Album")
            .to_string();
        let title = tag
            .title()
            .unwrap_or_else(|| stem(filename))
            .to_string();
        let track_num = tag.track().unwrap_or(0) as u16;
        let genre = tag
            .genre_parsed()
            .map(|g| g.to_string())
            .unwrap_or_default();
        return (artist, album, title, track_num, genre);
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
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
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
}
