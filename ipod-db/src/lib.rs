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
//!
//! # iTunesDB format overview
//!
//! The database is a tree of length-prefixed chunks, each identified by a 4-byte magic:
//!
//! ```text
//! mhbd (database header)
//! ├── mhsd type=1 (track dataset)
//! │   └── mhlt (track list)
//! │       └── mhit (track) ×N
//! │           └── mhod (string: title, artist, album, path, ...) ×M
//! ├── mhsd type=2 (playlist dataset)
//! │   └── mhlp (playlist list)
//! │       └── mhyp (playlist) ×N
//! │           ├── mhod (string: playlist name) ×M
//! │           └── mhip (playlist item: track reference) ×K
//! └── mhsd type=3,4,5 (podcasts, albums, smart playlists — skipped)
//! ```
//!
//! All integers are little-endian. Strings are UTF-16LE. Each chunk's header_size
//! field allows skipping unknown fields for forward compatibility across iPod
//! generations (tested: iPod Video, Classic, Mini).
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
//! db.add_track(IpodTrack {
//!     dbid: 0, track_id: 0, // assigned by add_track
//!     title: "Song".into(), artist: "Artist".into(), album: "Album".into(),
//!     album_artist: None, genre: None, track_number: Some(1), disc_number: None,
//!     total_time_ms: Some(180000), year: Some(2024), file_size: 5_000_000,
//!     bitrate: Some(320), sample_rate: Some(44100),
//!     ipod_path: ":iPod_Control:Music:F00:ABCD.mp3".into(),
//!     filetype: 0x4d503320,
//! });
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
#[derive(Debug, Clone)]
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
    /// File type (1 = MP3, 2 = AAC/M4A, 4 = WAV).
    pub filetype: u32,
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
    /// All tracks.
    pub tracks: Vec<IpodTrack>,
    /// All playlists (first is always master).
    pub playlists: Vec<IpodPlaylist>,
    /// Path to the iPod mount point.
    pub mount_point: PathBuf,
    /// Artwork store (initialized when artwork is being managed).
    pub artwork_store: Option<artwork::ArtworkStore>,
    /// Next available track ID for new entries.
    next_track_id: u32,
    /// Next available dbid for new entries.
    next_dbid: u64,
}

impl IpodDatabase {
    /// Create a new empty database for an iPod at the given mount point.
    pub fn new(mount_point: PathBuf) -> Self {
        Self {
            db_version: 0x19,
            tracks: Vec::new(),
            playlists: vec![IpodPlaylist {
                name: String::new(),
                is_master: true,
                track_ids: Vec::new(),
            }],
            mount_point,
            artwork_store: None,
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
        tracks: Vec<IpodTrack>,
        playlists: Vec<IpodPlaylist>,
        mount_point: PathBuf,
    ) -> Self {
        let next_track_id = tracks.iter().map(|t| t.track_id).max().unwrap_or(0) + 1;
        let next_dbid = tracks.iter().map(|t| t.dbid).max().unwrap_or(0) + 1;
        Self {
            db_version,
            tracks,
            playlists,
            mount_point,
            artwork_store: None,
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
