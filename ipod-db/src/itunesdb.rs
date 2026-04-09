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
/// mhit field layout (offsets from chunk start, all little-endian):
///
/// Core fields (+4 to +108, always present):
///   +4   header_size     u32    +56  bitrate           u32
///   +8   total_size      u32    +60  sample_rate       u32  fixed-point (Hz<<16)
///   +12  num_mhods       u32    +64  volume_adjust     u32
///   +16  track_id        u32    +68  start_time        u32  ms
///   +20  visible         u32    +72  stop_time         u32  ms
///   +24  filetype        u32    +76  sound_check       u32
///   +28  type            u8     +80  play_count        u32
///   +29  compilation     u8     +84  last_played       u32  Mac timestamp
///   +30  rating          u8     +88  date_added_to_device u32 Mac timestamp
///   +31  padding         u8     +92  disc_number       u32
///   +32  date_modified   u32    +96  disc_total        u32
///   +36  file_size       u32    +100 sort_order        u32
///   +40  total_time_ms   u32    +104 date_added        u32  Mac timestamp
///   +44  track_number    u32    +108 date_released     u32  Mac timestamp
///   +48  total_tracks    u32
///   +52  year            u32
///
/// Extended fields (+112 to +208, require header_size >= 212):
///   +112 dbid            u64    +168 dbid2             u64  duplicate
///   +120 checked         u32    +176 lyrics_flag       u32
///   +124 app_rating      u32    +180 movie_flag        u32
///   +128 bpm             u32    +184 mark_unplayed     u32
///   +132 artwork_count   u32    +188 size_on_disk      u32
///   +136 sample_rate_dup u32    +192 date_modified2    u32
///   +140 date_released2  u32    +196 hash              u32
///   +144 explicit_flag   u32    +200 media_type        u32
///   +148 skip_count      u32    +204 season|episode    u32
///   +152 last_skipped    u32    +208 has_gapless_data  u32
///   +156 has_artwork     u32
///   +160 skip_shuffling  u32
///   +164 remember_pos    u32
///
/// Fields beyond +212 (gapless data, album IDs, etc.) vary by generation
/// and are skipped via header_size.
fn parse_mhit(cur: &mut Cursor<&[u8]>, start: u64) -> crate::Result<IpodTrack> {
    // Read the minimum header fields (offsets +4 through +108, 108 bytes).
    // Every field is read sequentially with no seek gaps.
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
    let _last_played = cur.read_u32::<LittleEndian>()?; // +84   Mac timestamp
    let _date_added_to_device = cur.read_u32::<LittleEndian>()?; // +88   Mac timestamp
    let disc_number = cur.read_u32::<LittleEndian>()?; // +92
    let _disc_total = cur.read_u32::<LittleEndian>()?; // +96
    let _sort_order = cur.read_u32::<LittleEndian>()?; // +100
    let _date_added = cur.read_u32::<LittleEndian>()?; // +104  Mac timestamp
    let _date_released = cur.read_u32::<LittleEndian>()?; // +108  Mac timestamp
                                                          // Sequential read ends at +112. Remaining fields need header_size guards.

    // Extended fields (offsets +112 through +320). Only present in larger headers.
    // Each block is guarded by header_size to handle older iPod generations.
    let dbid = if header_size >= 120 {
        cur.read_u64::<LittleEndian>()? // +112  persistent ID
    } else {
        0
    };

    if header_size >= 212 {
        let _checked = cur.read_u32::<LittleEndian>()?; // +120
        let _app_rating = cur.read_u32::<LittleEndian>()?; // +124
        let _bpm = cur.read_u32::<LittleEndian>()?; // +128
        let _artwork_count = cur.read_u32::<LittleEndian>()?; // +132
        let _sample_rate_dup = cur.read_u32::<LittleEndian>()?; // +136  fixed-point dup
        let _date_released2 = cur.read_u32::<LittleEndian>()?; // +140
        let _explicit_flag = cur.read_u32::<LittleEndian>()?; // +144
        let _skip_count = cur.read_u32::<LittleEndian>()?; // +148
        let _last_skipped = cur.read_u32::<LittleEndian>()?; // +152  Mac timestamp
        let _has_artwork = cur.read_u32::<LittleEndian>()?; // +156
        let _skip_shuffling = cur.read_u32::<LittleEndian>()?; // +160
        let _remember_pos = cur.read_u32::<LittleEndian>()?; // +164
        let _dbid2 = cur.read_u64::<LittleEndian>()?; // +168  persistent ID dup
        let _lyrics_flag = cur.read_u32::<LittleEndian>()?; // +176
        let _movie_flag = cur.read_u32::<LittleEndian>()?; // +180
        let _mark_unplayed = cur.read_u32::<LittleEndian>()?; // +184
        let _size_on_disk = cur.read_u32::<LittleEndian>()?; // +188
        let _date_modified2 = cur.read_u32::<LittleEndian>()?; // +192
        let _hash = cur.read_u32::<LittleEndian>()?; // +196
        let _media_type = cur.read_u32::<LittleEndian>()?; // +200
        let _season_episode = cur.read_u32::<LittleEndian>()?; // +204
        let _has_gapless = cur.read_u32::<LittleEndian>()?; // +208
    }
    // Sequential read ends at +212. Remaining header bytes (212..header_size)
    // contain gapless data, album IDs, and other fields that vary by generation.
    // We skip them via the seek below — header_size handles forward compat.

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

    let db = IpodDatabase::from_parsed(db_version, tracks, playlists, mount_point);

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

    #[test]
    fn test_round_trip_all_fields() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: Some("AA".into()),
            genre: Some("Rock".into()),
            track_number: Some(7),
            disc_number: Some(2),
            total_time_ms: Some(300000),
            year: Some(1999),
            file_size: 8_000_000,
            bitrate: Some(256),
            sample_rate: Some(48000),
            ipod_path: ":iPod_Control:Music:F05:XYZW.mp3".into(),
            filetype: 0x4d503320,
        });

        let bytes = itunesdb_write::serialize(&db);
        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();

        let t = &parsed.tracks[0];
        assert_eq!(t.title, "Song");
        assert_eq!(t.artist, "Artist");
        assert_eq!(t.album, "Album");
        assert_eq!(t.album_artist.as_deref(), Some("AA"));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        assert_eq!(t.track_number, Some(7));
        assert_eq!(t.disc_number, Some(2));
        assert_eq!(t.total_time_ms, Some(300000));
        assert_eq!(t.year, Some(1999));
        assert_eq!(t.file_size, 8_000_000);
        assert_eq!(t.bitrate, Some(256));
        assert_eq!(t.sample_rate, Some(48000));
        assert_eq!(t.ipod_path, ":iPod_Control:Music:F05:XYZW.mp3");
        assert_eq!(t.filetype, 0x4d503320);
        assert_eq!(t.dbid, db.tracks[0].dbid);
        assert_eq!(t.track_id, db.tracks[0].track_id);
    }

    #[test]
    fn test_corrupt_mhod_string_length() {
        // Build a minimal DB, then corrupt the string length in the first mhod.
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "X".into(),
            artist: "Y".into(),
            album: "Z".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 100,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
        });

        let mut bytes = itunesdb_write::serialize(&db);

        // Find the first mhod and corrupt its string_byte_len field (offset +28 from mhod start).
        let mhod_pos = bytes.windows(4).position(|w| w == b"mhod").unwrap();
        // String byte length is at mhod_pos + 28.
        let len_pos = mhod_pos + 28;
        // Write a huge length (0x01000000 = 16MB, exceeds 10MB limit).
        bytes[len_pos..len_pos + 4].copy_from_slice(&0x01000000u32.to_le_bytes());

        let result = parse(&bytes, PathBuf::from("/mnt/IPOD"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("sanity limit"),
            "expected sanity limit error, got: {err}"
        );
    }

    #[test]
    fn test_small_mhit_header_no_dbid() {
        // Build a DB, then shrink the mhit header_size to < 120 so dbid can't be read.
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "T".into(),
            artist: "A".into(),
            album: "B".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 50,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:b.mp3".into(),
            filetype: 0x4d503320,
        });

        let mut bytes = itunesdb_write::serialize(&db);

        // Find the mhit and restructure it with header_size=112.
        // Must move mhod data to start at offset 112 and fix all sizes.
        let mhit_pos = bytes.windows(4).position(|w| w == b"mhit").unwrap();
        let old_hdr =
            u32::from_le_bytes(bytes[mhit_pos + 4..mhit_pos + 8].try_into().unwrap()) as usize;
        let old_total =
            u32::from_le_bytes(bytes[mhit_pos + 8..mhit_pos + 12].try_into().unwrap()) as usize;
        let new_hdr: usize = 112;

        // Extract mhod data, rebuild mhit with smaller header.
        let mhod_data = bytes[mhit_pos + old_hdr..mhit_pos + old_total].to_vec();
        let new_total = new_hdr + mhod_data.len();
        let mut new_mhit = bytes[mhit_pos..mhit_pos + new_hdr].to_vec();
        new_mhit[4..8].copy_from_slice(&(new_hdr as u32).to_le_bytes());
        new_mhit[8..12].copy_from_slice(&(new_total as u32).to_le_bytes());
        new_mhit.extend_from_slice(&mhod_data);

        // Replace old mhit and fix parent container sizes.
        let size_diff = old_total as i64 - new_total as i64;
        bytes.splice(mhit_pos..mhit_pos + old_total, new_mhit);
        // Fix mhbd total_size.
        let mhbd_total = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as i64 - size_diff;
        bytes[8..12].copy_from_slice(&(mhbd_total as u32).to_le_bytes());
        // Find the tracks mhsd (type=1) and fix its total_size.
        let mhbd_hdr = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let num_ds = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut ds_pos = mhbd_hdr;
        for _ in 0..num_ds {
            let ds_type = u32::from_le_bytes(bytes[ds_pos + 12..ds_pos + 16].try_into().unwrap());
            let ds_total = u32::from_le_bytes(bytes[ds_pos + 8..ds_pos + 12].try_into().unwrap());
            if ds_type == 1 {
                let new_ds_total = ds_total as i64 - size_diff;
                bytes[ds_pos + 8..ds_pos + 12]
                    .copy_from_slice(&(new_ds_total as u32).to_le_bytes());
                break;
            }
            ds_pos += ds_total as usize;
        }

        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(
            parsed.tracks[0].dbid, 0,
            "dbid should default to 0 for small headers"
        );
        assert_eq!(parsed.tracks[0].title, "T");
    }

    #[test]
    fn test_unknown_mhsd_types_skipped() {
        // Build a normal DB, then insert a fake mhsd type=99 between the two real datasets.
        let db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let mut bytes = itunesdb_write::serialize(&db);

        // Increment num_datasets by 1 and insert a dummy mhsd.
        let num_ds = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        bytes[20..24].copy_from_slice(&(num_ds + 1).to_le_bytes());

        // Build a minimal fake mhsd (type=99, header=96, total=96).
        let mut fake_mhsd = vec![0u8; 96];
        fake_mhsd[0..4].copy_from_slice(b"mhsd");
        fake_mhsd[4..8].copy_from_slice(&96u32.to_le_bytes()); // header_size
        fake_mhsd[8..12].copy_from_slice(&96u32.to_le_bytes()); // total_size
        fake_mhsd[12..16].copy_from_slice(&99u32.to_le_bytes()); // type = unknown

        // Insert after the mhbd header (offset 104).
        let mhbd_hdr_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        bytes.splice(mhbd_hdr_size..mhbd_hdr_size, fake_mhsd);

        // Update mhbd total_size.
        let new_total = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&new_total.to_le_bytes());

        let parsed = parse(&bytes, PathBuf::from("/mnt/IPOD")).unwrap();
        assert_eq!(parsed.tracks.len(), 0);
        assert_eq!(parsed.playlists.len(), 1); // master playlist
    }
}
