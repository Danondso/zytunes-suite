//! Pure Rust library for reading and writing the iPod iTunesDB binary database format.
//!
//! Classic iPods store their music library in a proprietary binary file at
//! `iPod_Control/iTunes/iTunesDB`. This crate parses that format into an in-memory
//! [`IpodDatabase`] and can serialize it back to disk.
//!
//! # Modules
//!
//! - [`detect`] — Scan filesystem mount points for connected iPods
//! - [`fs`] — Manage the iPod's F00-F49 music directory structure
//! - [`itunesdb`] — Parse iTunesDB binary format into [`IpodDatabase`]
//! - [`itunesdb_write`] — Serialize [`IpodDatabase`] back to binary, with atomic writes
//! - [`artwork`] — ArtworkDB + ITHMB thumbnail encoding for album art
//! - [`hash`] — iPod Classic hash58 (HMAC-SHA1) database signing
//!
//! # iTunesDB format overview
//!
//! The database is a tree of length-prefixed chunks, each identified by a 4-byte magic:
//!
//! ```text
//! mhbd (database header, 244 bytes — includes db_id, db_version, hash58)
//! ├── mhsd type=4 (album dataset)
//! │   └── mhla (album list)
//! │       └── mhia (album) ×N
//! │           └── mhod (type 200=name, 201=artist, 202=sort artist)
//! ├── mhsd type=1 (track dataset)
//! │   └── mhlt (track list)
//! │       └── mhit (track, 624-byte header) ×N
//! │           └── mhod (1=title, 2=path, 3=album, 4=artist, 5=genre,
//! │                      6=filetype, 14=album_artist,
//! │                      22=sort_artist, 23=sort_album_artist,
//! │                      27=sort_title, 28=sort_album) ×M
//! ├── mhsd type=3 (podcast playlist — clone of type 2, required by firmware)
//! ├── mhsd type=2 (playlist dataset)
//! │   └── mhlp (playlist list)
//! │       └── mhyp (playlist, 184-byte header) ×N
//! │           ├── mhod type=1 (name)
//! │           ├── mhod type=100 (column layout)
//! │           ├── mhod type=102 (column prefs)
//! │           ├── mhod type=52 (sort index) + type=53 (letter index) ×K
//! │           └── mhip (playlist item + child mhod type=100) ×J
//! └── mhsd type=5 (smart playlists — preserved from parse, rules not editable)
//! ```
//!
//! All integers are little-endian. Strings are UTF-16LE. Each chunk's header_size
//! field allows skipping unknown fields for forward compatibility across iPod
//! generations (tested: iPod Video, Classic, Mini).
//!
//! # iPod Classic requirements
//!
//! The Classic firmware (db_version 0x75) is strict about what it accepts:
//!
//! - **`db_id`** must be nonzero — firmware rejects the database otherwise.
//! - **hash58** (HMAC-SHA1 at mhbd +0x58) must be valid for the device's
//!   FirewireGuid, or the firmware shows "No Music".
//! - **Sort indexes** (mhod type 52) and **letter indexes** (mhod type 53)
//!   must be present in the master playlist for the browse UI to work.
//! - **Album list** (mhsd type 4) must be populated — empty list may cause
//!   the firmware to reject the database.
//! - **Dataset order** must be: 4 (albums), 1 (tracks), 3 (podcasts),
//!   2 (playlists), 5 (smart playlists).
//!
//! # Round-trip fidelity
//!
//! The serializer produces output within ~0.5% of an iTunes-written reference
//! database. Fields that are NOT round-tripped (metadata we don't carry):
//!
//! - Composer (mhod type 8), grouping (type 12), sort composer (type 29)
//! - Per-track podcast/TV fields (types 22 as podcast URL, 23, 30)
//! - mhbd header fields beyond db_id (language, timestamps)
//!
//! Smart playlists (mhsd type 5) are preserved as raw bytes from the parsed
//! database since their rule format cannot be regenerated.
//!
//! # Example
//!
//! ```no_run
//! use ipod_db::{hash, itunesdb, itunesdb_write, IpodDatabase, IpodTrack};
//! use std::path::PathBuf;
//!
//! // Parse an existing database
//! let mount = PathBuf::from("/mnt/ipod");
//! let raw = std::fs::read(mount.join("iPod_Control/iTunes/iTunesDB")).unwrap();
//! let mut db = itunesdb::parse(&raw, mount).unwrap();
//!
//! // Add a track
//! let mut track = IpodTrack::default();
//! track.title = "Song".into();
//! track.artist = "Artist".into();
//! track.album = "Album".into();
//! track.track_number = Some(1);
//! track.total_time_ms = Some(180000);
//! track.year = Some(2024);
//! track.file_size = 5_000_000;
//! track.bitrate = Some(320);
//! track.sample_rate = Some(44100);
//! track.ipod_path = ":iPod_Control:Music:F00:ABCD.mp3".into();
//! track.filetype = 0x4d503320;
//! db.add_track(track);
//!
//! // Write back to disk (atomic, with .bak backup and hash58 signing)
//! let fwid = hash::parse_firewire_id("000A2700215CDB22").unwrap();
//! itunesdb_write::write_to_disk(&db, Some(&fwid)).unwrap();
//! ```

pub mod artwork;
pub mod detect;
pub(crate) mod encoding;
pub mod fs;
pub mod hash;
pub mod itunesdb;
pub mod itunesdb_write;

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpodDbError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid iTunesDB: {0}")]
    Parse(String),

    #[error("no iPod found at mount points")]
    NotFound,

    #[error("iPod filesystem error: {0}")]
    Filesystem(String),

    #[error("artwork error: {0}")]
    Artwork(String),
}

pub type Result<T> = std::result::Result<T, IpodDbError>;

/// A track entry in the iPod database.
#[derive(Debug, Clone, Default)]
pub struct IpodTrack {
    /// Unique track ID within the database (mhit dbid).
    pub dbid: u64,
    /// Track ID used for references (mhit track_id).
    pub track_id: u32,
    /// Title.
    pub title: String,
    /// Artist name.
    pub artist: String,
    /// Album name.
    pub album: String,
    /// Album artist, if different from track artist.
    pub album_artist: Option<String>,
    /// Genre.
    pub genre: Option<String>,
    /// Track number on the album.
    pub track_number: Option<u16>,
    /// Disc number.
    pub disc_number: Option<u16>,
    /// Total time in milliseconds.
    pub total_time_ms: Option<u32>,
    /// Year.
    pub year: Option<u16>,
    /// File size in bytes.
    pub file_size: u32,
    /// Bitrate in kbps.
    pub bitrate: Option<u16>,
    /// Sample rate in Hz.
    pub sample_rate: Option<u16>,
    /// iPod-style path (colon-separated, e.g. ":iPod_Control:Music:F00:ABCD.mp3").
    pub ipod_path: String,
    /// File type code (0x4d503320 = MP3, 0x4d344120 = M4A/AAC/ALAC, etc).
    pub filetype: u32,
    /// Filetype description string for mhod type 6 (e.g. "MPEG audio file",
    /// "AAC audio file", "Apple Lossless audio file"). The iPod firmware uses
    /// this to select the audio decoder. When `None`, derived from `filetype`.
    pub filetype_string: Option<String>,
    /// Raw mhit header bytes (full 624-byte header from parsed database).
    /// When present, the serializer replays this and patches only the fields
    /// it actively manages (artwork_count, has_artwork, total_size, num_mhods).
    pub(crate) raw_mhit_header: Option<Vec<u8>>,
}

impl IpodTrack {
    /// Clear the raw mhit header, forcing the serializer to rebuild it from
    /// scratch using the track's metadata fields. Use this when the cached
    /// header needs to be regenerated (e.g. after fixing field values).
    pub fn clear_raw_header(&mut self) {
        self.raw_mhit_header = None;
    }

    /// Mutable access to the raw mhit header bytes for field-level patching.
    /// Returns `None` if the track was created from scratch (no parsed header).
    pub fn raw_header_mut(&mut self) -> Option<&mut Vec<u8>> {
        self.raw_mhit_header.as_mut()
    }
}

/// A playlist in the iPod database.
#[derive(Debug, Clone)]
pub struct IpodPlaylist {
    /// Playlist name. The first playlist is always the master playlist (all tracks).
    pub name: String,
    /// Whether this is the master (library) playlist.
    pub is_master: bool,
    /// Track IDs (persistent IDs / dbids) belonging to this playlist, in order.
    pub track_ids: Vec<u64>,
}

/// The full in-memory representation of an iPod's iTunesDB.
#[derive(Debug, Clone)]
pub struct IpodDatabase {
    /// Database version (from mhbd header).
    pub db_version: u32,
    /// Database persistent ID (from mhbd +24). Required by iPod Classic firmware.
    pub db_id: u64,
    /// All tracks.
    pub tracks: Vec<IpodTrack>,
    /// All playlists (first is always master).
    pub playlists: Vec<IpodPlaylist>,
    /// Path to the iPod mount point.
    pub mount_point: PathBuf,
    /// Artwork store (initialized when artwork is being managed).
    pub artwork_store: Option<artwork::ArtworkStore>,
    /// Raw mhbd header (244 bytes). Preserved from parse so the serializer
    /// can replay firmware-critical fields (language, platform, persistent IDs,
    /// hash72, timezone, etc.) that we don't actively manage.
    pub(crate) raw_mhbd_header: Option<Vec<u8>>,
    /// Raw mhsd type=5 (smart playlists) blob. Preserved from parse for
    /// lossless round-trip since smart playlist rules can't be regenerated.
    pub(crate) raw_smart_playlists: Option<Vec<u8>>,
    /// Next available track ID for new entries.
    next_track_id: u32,
    /// Next available dbid for new entries.
    next_dbid: u64,
}

impl IpodDatabase {
    /// Create a new empty database for an iPod at the given mount point.
    pub fn new(mount_point: PathBuf) -> Self {
        // Generate a db_id from timestamp. Classic firmware requires a nonzero value.
        let db_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            db_version: 0x19,
            db_id,
            tracks: Vec::new(),
            playlists: vec![IpodPlaylist {
                name: String::new(),
                is_master: true,
                track_ids: Vec::new(),
            }],
            mount_point,
            artwork_store: None,
            raw_mhbd_header: None,
            raw_smart_playlists: None,
            next_track_id: 1,
            next_dbid: 1,
        }
    }

    /// Add a track and return the assigned dbid.
    pub fn add_track(&mut self, mut track: IpodTrack) -> u64 {
        track.track_id = self.next_track_id;
        track.dbid = self.next_dbid;
        self.next_track_id += 1;
        self.next_dbid += 1;

        let dbid = track.dbid;

        // Add to master playlist.
        if let Some(master) = self.playlists.first_mut() {
            master.track_ids.push(dbid);
        }

        self.tracks.push(track);
        dbid
    }

    /// Remove a track by dbid. Also removes from all playlists.
    pub fn remove_track(&mut self, dbid: u64) -> Option<IpodTrack> {
        let pos = self.tracks.iter().position(|t| t.dbid == dbid)?;
        let track = self.tracks.remove(pos);
        for pl in &mut self.playlists {
            pl.track_ids.retain(|&id| id != dbid);
        }
        Some(track)
    }

    /// Find a track by dbid.
    pub fn find_track(&self, dbid: u64) -> Option<&IpodTrack> {
        self.tracks.iter().find(|t| t.dbid == dbid)
    }

    /// Path to the iTunesDB file on disk.
    pub fn db_path(&self) -> PathBuf {
        self.mount_point
            .join("iPod_Control")
            .join("iTunes")
            .join("iTunesDB")
    }

    /// Construct from parsed data, auto-calculating next available IDs.
    pub(crate) fn from_parsed(
        db_version: u32,
        db_id: u64,
        tracks: Vec<IpodTrack>,
        playlists: Vec<IpodPlaylist>,
        raw_mhbd_header: Option<Vec<u8>>,
        raw_smart_playlists: Option<Vec<u8>>,
        mount_point: PathBuf,
    ) -> Self {
        let next_track_id = tracks.iter().map(|t| t.track_id).max().unwrap_or(0) + 1;
        let next_dbid = tracks.iter().map(|t| t.dbid).max().unwrap_or(0) + 1;
        Self {
            db_version,
            db_id,
            tracks,
            playlists,
            mount_point,
            artwork_store: None,
            raw_mhbd_header,
            raw_smart_playlists,
            next_track_id,
            next_dbid,
        }
    }

    /// Initialize the artwork store with thumbnail specs for the target iPod model.
    pub fn init_artwork(&mut self, specs: Vec<artwork::ThumbnailSpec>) {
        self.artwork_store = Some(artwork::ArtworkStore::new(specs));
    }

    /// Set artwork for a track by dbid. The artwork store must be initialized first.
    pub fn set_track_artwork(&mut self, dbid: u64, image_bytes: &[u8]) -> Result<()> {
        let store = self
            .artwork_store
            .as_mut()
            .ok_or_else(|| IpodDbError::Artwork("artwork store not initialized".into()))?;
        store.add_artwork(dbid, image_bytes)
    }

    /// Path to the ArtworkDB file on disk.
    pub fn artwork_db_path(&self) -> PathBuf {
        artwork::ArtworkStore::db_path(&self.mount_point)
    }
}
