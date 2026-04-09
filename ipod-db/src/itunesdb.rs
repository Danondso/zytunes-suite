use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read, Seek, SeekFrom};

use crate::{IpodDatabase, IpodDbError, IpodPlaylist, IpodTrack};

/// Mhod type constants for data objects.
///
/// String types (1-18) contain UTF-16LE encoded text with a 16-byte sub-header.
/// Higher-numbered types (22+) are binary data (smart playlists, sort keys, etc.)
/// and are skipped during parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MhodType {
    Title = 1,
    Location = 2,
    Album = 3,
    Artist = 4,
    Genre = 5,
    Filetype = 6,
    Comment = 7,
    Composer = 8,
    Grouping = 12,
    AlbumArtist = 14,
    SortArtist = 15,
    SortTitle = 16,
    SortAlbum = 17,
    SortAlbumArtist = 18,
}

impl MhodType {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Title),
            2 => Some(Self::Location),
            3 => Some(Self::Album),
            4 => Some(Self::Artist),
            5 => Some(Self::Genre),
            6 => Some(Self::Filetype),
            7 => Some(Self::Comment),
            8 => Some(Self::Composer),
            12 => Some(Self::Grouping),
            14 => Some(Self::AlbumArtist),
            15 => Some(Self::SortArtist),
            16 => Some(Self::SortTitle),
            17 => Some(Self::SortAlbum),
            18 => Some(Self::SortAlbumArtist),
            _ => None,
        }
    }
}

fn read_magic(cur: &mut Cursor<&[u8]>) -> crate::Result<[u8; 4]> {
    let mut buf = [0u8; 4];
    cur.read_exact(&mut buf)?;
    Ok(buf)
}

fn expect_magic(cur: &mut Cursor<&[u8]>, expected: &[u8; 4]) -> crate::Result<()> {
    let magic = read_magic(cur)?;
    if &magic != expected {
        return Err(IpodDbError::Parse(format!(
            "expected magic {:?}, got {:?} at offset {}",
            std::str::from_utf8(expected).unwrap_or("?"),
            std::str::from_utf8(&magic).unwrap_or("?"),
            cur.position() - 4
        )));
    }
    Ok(())
}

/// Read a UTF-16LE string from a byte slice.
fn decode_utf16le(data: &[u8]) -> String {
    let (decoded, _, _) = encoding_rs::UTF_16LE.decode(data);
    decoded.into_owned()
}

/// Parse a string mhod payload. Returns (mhod_type, string_value).
fn parse_string_mhod(
    cur: &mut Cursor<&[u8]>,
    start: u64,
) -> crate::Result<Option<(MhodType, String)>> {
    // We're positioned right after the magic was already consumed.
    let _header_size = cur.read_u32::<LittleEndian>()?;
    let total_size = cur.read_u32::<LittleEndian>()?;
    let mhod_type_raw = cur.read_u32::<LittleEndian>()?;

    let mhod_type = match MhodType::from_u32(mhod_type_raw) {
        Some(t) => t,
        None => {
            // Unknown mhod type — skip it entirely.
            cur.seek(SeekFrom::Start(start + total_size as u64))?;
            return Ok(None);
        }
    };

    // Skip to the string sub-header (at header offset 24, we've read 16 bytes after magic).
    // Position ourselves at offset 24 from chunk start.
    cur.seek(SeekFrom::Start(start + 24))?;

    let _string_position = cur.read_u32::<LittleEndian>()?;
    let string_byte_len = cur.read_u32::<LittleEndian>()?;
    let _encoding = cur.read_u32::<LittleEndian>()?; // 1 = UTF-16LE, 2 = UTF-8
    let _padding = cur.read_u32::<LittleEndian>()?;

    // Guard against corrupted DB claiming absurd string lengths.
    if string_byte_len > 10 * 1024 * 1024 {
        return Err(IpodDbError::Parse(format!(
            "mhod string length {} exceeds 10MB sanity limit at offset {}",
            string_byte_len, start
        )));
    }
    let mut string_data = vec![0u8; string_byte_len as usize];
    cur.read_exact(&mut string_data)?;

    let value = decode_utf16le(&string_data);

    // Advance to end of mhod.
    cur.seek(SeekFrom::Start(start + total_size as u64))?;

    Ok(Some((mhod_type, value)))
}

/// Parse an mhit (track item) and its child mhods.
///
/// mhit field layout (offsets from chunk start):
///   +0   magic "mhit"          +96  bookmark_time_ms
///   +4   header_size           +100 sort_order
///   +8   total_size            +104 date_added (Mac timestamp)
///   +12  num_mhods             +108 date_released (Mac timestamp)
///   +16  track_id              +112 dbid (persistent ID, u64)
///   +20  visible               +120 checked (0xffff0001 = yes)
///   +24  filetype              +124 app_rating
///   +28  type|compilation|     +128 bpm
///        rating|padding        +136 sample_rate (fixed-point, dup)
///   +32  date_modified         +144 explicit_flag
///   +36  file_size             +164 remember_playback_pos
///   +40  total_time_ms         +168 dbid2 (duplicate of +112)
///   +44  track_number          +176 lyrics_flag
///   +48  total_tracks          +184 mark_unplayed
///   +52  year                  +188 size_on_disk
///   +56  bitrate               +200 media_type
///   +60  sample_rate           +204 season_number|episode_number
///   +64  volume_adjust         +208 has_gapless_data
///   +68  start_time            +248 gapless_encoding_delay
///   +72  stop_time             +256 gapless_track_flag
///   +76  sound_check           +288 album_id
///   +80  play_count            +300 file_size2 (>4GB support)
///   +84  last_played (Mac timestamp)
///   +88  date_added_to_device (Mac timestamp)
///   +92  disc_number
///   +96  disc_total
fn parse_mhit(cur: &mut Cursor<&[u8]>, start: u64) -> crate::Result<IpodTrack> {
    let header_size = cur.read_u32::<LittleEndian>()?; // +4
    let total_size = cur.read_u32::<LittleEndian>()?; // +8
    let num_mhods = cur.read_u32::<LittleEndian>()?; // +12
    let track_id = cur.read_u32::<LittleEndian>()?; // +16
    let _visible = cur.read_u32::<LittleEndian>()?; // +20
    let filetype = cur.read_u32::<LittleEndian>()?; // +24
    let _type_byte = cur.read_u8()?; // +28
    let _compilation = cur.read_u8()?; // +29
    let _rating = cur.read_u8()?; // +30
    let _padding = cur.read_u8()?; // +31
    let _date_modified = cur.read_u32::<LittleEndian>()?; // +32
    let file_size = cur.read_u32::<LittleEndian>()?; // +36
    let total_time = cur.read_u32::<LittleEndian>()?; // +40
    let track_number = cur.read_u32::<LittleEndian>()?; // +44
    let _total_tracks = cur.read_u32::<LittleEndian>()?; // +48
    let year = cur.read_u32::<LittleEndian>()?; // +52
    let bitrate = cur.read_u32::<LittleEndian>()?; // +56
    let sample_rate_raw = cur.read_u32::<LittleEndian>()?; // +60
    let sample_rate = (sample_rate_raw >> 16) as u16;
    let _volume_adjust = cur.read_u32::<LittleEndian>()?; // +64
    let _start_time = cur.read_u32::<LittleEndian>()?; // +68
    let _stop_time = cur.read_u32::<LittleEndian>()?; // +72
    let _sound_check = cur.read_u32::<LittleEndian>()?; // +76
    let _play_count = cur.read_u32::<LittleEndian>()?; // +80
    let _last_played = cur.read_u32::<LittleEndian>()?; // +84  Mac timestamp
    let _date_added_to_device = cur.read_u32::<LittleEndian>()?; // +88  Mac timestamp
    let disc_number = cur.read_u32::<LittleEndian>()?; // +92
    let _disc_total = cur.read_u32::<LittleEndian>()?; // +96
    let _sort_order = cur.read_u32::<LittleEndian>()?; // +100
    let _date_added = cur.read_u32::<LittleEndian>()?; // +104
    let _date_released = cur.read_u32::<LittleEndian>()?; // +108

    // dbid (persistent ID) at offset 112.
    // Guard: only read if header is large enough to contain the field.
    let dbid = if header_size >= 120 {
        cur.read_u64::<LittleEndian>()? // +112
    } else {
        0
    };

    // Jump to end of mhit header to read child mhods.
    cur.seek(SeekFrom::Start(start + header_size as u64))?;

    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut album_artist = None;
    let mut genre = None;
    let mut ipod_path = String::new();

    for _ in 0..num_mhods {
        let mhod_start = cur.position();
        expect_magic(cur, b"mhod")?;
        if let Some((mhod_type, value)) = parse_string_mhod(cur, mhod_start)? {
            match mhod_type {
                MhodType::Title => title = value,
                MhodType::Location => ipod_path = value,
                MhodType::Album => album = value,
                MhodType::Artist => artist = value,
                MhodType::Genre => genre = Some(value),
                MhodType::AlbumArtist => album_artist = Some(value),
                _ => {}
            }
        }
    }

    // Ensure we're at end of mhit total size.
    cur.seek(SeekFrom::Start(start + total_size as u64))?;

    Ok(IpodTrack {
        dbid,
        track_id,
        title,
        artist,
        album,
        album_artist,
        genre,
        track_number: if track_number > 0 {
            Some(track_number as u16)
        } else {
            None
        },
        disc_number: if disc_number > 0 {
            Some(disc_number as u16)
        } else {
            None
        },
        total_time_ms: if total_time > 0 {
            Some(total_time)
        } else {
            None
        },
        year: if year > 0 { Some(year as u16) } else { None },
        file_size,
        bitrate: if bitrate > 0 {
            Some(bitrate as u16)
        } else {
            None
        },
        sample_rate: if sample_rate > 0 {
            Some(sample_rate)
        } else {
            None
        },
        ipod_path,
        filetype,
    })
}

/// Parse an mhyp (playlist) and its child mhods + mhips.
fn parse_mhyp(cur: &mut Cursor<&[u8]>, start: u64) -> crate::Result<IpodPlaylist> {
    let header_size = cur.read_u32::<LittleEndian>()?;
    let total_size = cur.read_u32::<LittleEndian>()?;
    let num_mhods = cur.read_u32::<LittleEndian>()?;
    let num_mhips = cur.read_u32::<LittleEndian>()?;
    let is_master = cur.read_u32::<LittleEndian>()? != 0;

    // Jump to end of header to read children.
    cur.seek(SeekFrom::Start(start + header_size as u64))?;

    let mut name = String::new();

    // Parse mhod children (playlist name, etc.)
    for _ in 0..num_mhods {
        let mhod_start = cur.position();
        expect_magic(cur, b"mhod")?;
        if let Some((mhod_type, value)) = parse_string_mhod(cur, mhod_start)? {
            if mhod_type == MhodType::Title {
                name = value;
            }
        }
    }

    // Parse mhip children (track references).
    let mut track_ids = Vec::with_capacity(num_mhips as usize);
    for _ in 0..num_mhips {
        let mhip_start = cur.position();
        expect_magic(cur, b"mhip")?;
        let mhip_header_size = cur.read_u32::<LittleEndian>()?;
        let mhip_total_size = cur.read_u32::<LittleEndian>()?;
        let _num_mhip_mhods = cur.read_u32::<LittleEndian>()?;
        let _podcast_grouping = cur.read_u32::<LittleEndian>()?;
        let _group_id = cur.read_u32::<LittleEndian>()?;
        let track_id = cur.read_u32::<LittleEndian>()?;
        let _ = mhip_header_size; // used for skipping

        track_ids.push(track_id as u64);

        // Skip to end of mhip.
        cur.seek(SeekFrom::Start(mhip_start + mhip_total_size as u64))?;
    }

    // Ensure we're at end of mhyp.
    cur.seek(SeekFrom::Start(start + total_size as u64))?;

    Ok(IpodPlaylist {
        name,
        is_master,
        track_ids,
    })
}

/// Parse an iTunesDB from raw bytes into an `IpodDatabase`.
pub fn parse(data: &[u8], mount_point: std::path::PathBuf) -> crate::Result<IpodDatabase> {
    let mut cur = Cursor::new(data);

    // mhbd
    expect_magic(&mut cur, b"mhbd")?;
    let mhbd_header_size = cur.read_u32::<LittleEndian>()?;
    let _mhbd_total_size = cur.read_u32::<LittleEndian>()?;
    let _db_type = cur.read_u32::<LittleEndian>()?; // 1 = iTunesDB, 2 = podcast DB
    let db_version = cur.read_u32::<LittleEndian>()?;
    let num_datasets = cur.read_u32::<LittleEndian>()?;

    cur.seek(SeekFrom::Start(mhbd_header_size as u64))?;

    let mut tracks = Vec::new();
    let mut playlists = Vec::new();

    for _ in 0..num_datasets {
        let mhsd_start = cur.position();
        expect_magic(&mut cur, b"mhsd")?;
        let mhsd_header_size = cur.read_u32::<LittleEndian>()?;
        let mhsd_total_size = cur.read_u32::<LittleEndian>()?;
        let dataset_type = cur.read_u32::<LittleEndian>()?;

        match dataset_type {
            1 => {
                // Track list dataset.
                cur.seek(SeekFrom::Start(mhsd_start + mhsd_header_size as u64))?;
                let mhlt_start = cur.position();
                expect_magic(&mut cur, b"mhlt")?;
                let mhlt_header_size = cur.read_u32::<LittleEndian>()?;
                let num_tracks = cur.read_u32::<LittleEndian>()?;

                cur.seek(SeekFrom::Start(mhlt_start + mhlt_header_size as u64))?;

                for _ in 0..num_tracks {
                    let mhit_start = cur.position();
                    expect_magic(&mut cur, b"mhit")?;
                    tracks.push(parse_mhit(&mut cur, mhit_start)?);
                }
            }
            2 => {
                // Playlist dataset.
                cur.seek(SeekFrom::Start(mhsd_start + mhsd_header_size as u64))?;
                let mhlp_start = cur.position();
                expect_magic(&mut cur, b"mhlp")?;
                let mhlp_header_size = cur.read_u32::<LittleEndian>()?;
                let num_playlists = cur.read_u32::<LittleEndian>()?;

                cur.seek(SeekFrom::Start(mhlp_start + mhlp_header_size as u64))?;

                for _ in 0..num_playlists {
                    let mhyp_start = cur.position();
                    expect_magic(&mut cur, b"mhyp")?;
                    playlists.push(parse_mhyp(&mut cur, mhyp_start)?);
                }
            }
            _ => {
                // Podcast or other dataset — skip.
            }
        }

        // Skip to end of mhsd.
        cur.seek(SeekFrom::Start(mhsd_start + mhsd_total_size as u64))?;
    }

    // Resolve playlist track_ids: mhip stores track_id (u32), but our playlists
    // use dbid (u64). Build a lookup and convert.
    let track_id_to_dbid: std::collections::HashMap<u32, u64> =
        tracks.iter().map(|t| (t.track_id, t.dbid)).collect();

    for pl in &mut playlists {
        pl.track_ids = pl
            .track_ids
            .iter()
            .filter_map(|&tid| track_id_to_dbid.get(&(tid as u32)).copied())
            .collect();
    }

    let mut db = IpodDatabase {
        db_version,
        tracks,
        playlists,
        mount_point,
        next_track_id: 0,
        next_dbid: 0,
    };
    db.recalculate_ids();

    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itunesdb_write;
    use std::path::PathBuf;

    #[test]
    fn test_decode_utf16le() {
        let data = b"H\x00e\x00l\x00l\x00o\x00";
        assert_eq!(decode_utf16le(data), "Hello");
    }

    #[test]
    fn test_mhod_type_from_u32() {
        assert_eq!(MhodType::from_u32(1), Some(MhodType::Title));
        assert_eq!(MhodType::from_u32(2), Some(MhodType::Location));
        assert_eq!(MhodType::from_u32(99), None);
    }

    #[test]
    fn test_round_trip_empty_db() {
        let db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let bytes = itunesdb_write::serialize(&db);
        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();
        assert_eq!(parsed.tracks.len(), 0);
        assert_eq!(parsed.playlists.len(), 1);
        assert!(parsed.playlists[0].is_master);
    }

    #[test]
    fn test_round_trip_with_tracks() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Test Song".into(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
            album_artist: Some("Album Artist".into()),
            genre: Some("Rock".into()),
            track_number: Some(3),
            disc_number: Some(1),
            total_time_ms: Some(240000),
            year: Some(2024),
            file_size: 5_000_000,
            bitrate: Some(320),
            sample_rate: Some(44100),
            ipod_path: ":iPod_Control:Music:F00:abcdef.mp3".into(),
            filetype: 0x4d503320, // "MP3 "
        });

        let bytes = itunesdb_write::serialize(&db);
        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();

        assert_eq!(parsed.tracks.len(), 1);
        let t = &parsed.tracks[0];
        assert_eq!(t.title, "Test Song");
        assert_eq!(t.artist, "Test Artist");
        assert_eq!(t.album, "Test Album");
        assert_eq!(t.album_artist.as_deref(), Some("Album Artist"));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        assert_eq!(t.track_number, Some(3));
        assert_eq!(t.disc_number, Some(1));
        assert_eq!(t.total_time_ms, Some(240000));
        assert_eq!(t.year, Some(2024));
        assert_eq!(t.file_size, 5_000_000);
        assert_eq!(t.bitrate, Some(320));
        assert_eq!(t.sample_rate, Some(44100));
        assert_eq!(t.ipod_path, ":iPod_Control:Music:F00:abcdef.mp3");
        assert_eq!(t.filetype, 0x4d503320);

        // Check master playlist has the track.
        assert_eq!(parsed.playlists[0].track_ids.len(), 1);
        assert_eq!(parsed.playlists[0].track_ids[0], t.dbid);
    }

    #[test]
    fn test_round_trip_with_playlist() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let dbid1 = db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Song A".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 1000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
        });
        let dbid2 = db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Song B".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 2000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F01:b.mp3".into(),
            filetype: 0x4d503320,
        });

        db.playlists.push(IpodPlaylist {
            name: "My Playlist".into(),
            is_master: false,
            track_ids: vec![dbid2, dbid1],
        });

        let bytes = itunesdb_write::serialize(&db);
        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();

        assert_eq!(parsed.playlists.len(), 2);
        assert!(parsed.playlists[0].is_master);
        assert_eq!(parsed.playlists[0].track_ids.len(), 2);

        assert_eq!(parsed.playlists[1].name, "My Playlist");
        assert!(!parsed.playlists[1].is_master);
        assert_eq!(parsed.playlists[1].track_ids.len(), 2);
        assert_eq!(parsed.playlists[1].track_ids[0], dbid2);
        assert_eq!(parsed.playlists[1].track_ids[1], dbid1);
    }
}
