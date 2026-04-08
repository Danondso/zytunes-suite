pub mod detect;
pub mod fs;
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

    /// Recalculate next IDs from existing tracks (used after parsing).
    pub(crate) fn recalculate_ids(&mut self) {
        self.next_track_id = self.tracks.iter().map(|t| t.track_id).max().unwrap_or(0) + 1;
        self.next_dbid = self.tracks.iter().map(|t| t.dbid).max().unwrap_or(0) + 1;
    }
}
