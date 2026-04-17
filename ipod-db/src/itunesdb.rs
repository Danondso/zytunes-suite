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
    let total_tracks = cur.read_u32::<LittleEndian>()?; // +48
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
    let total_discs = cur.read_u32::<LittleEndian>()?; // +96
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
    let mut filetype_string = None;

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
                MhodType::Filetype => filetype_string = Some(value),
                MhodType::AlbumArtist => album_artist = Some(value),
                _ => {}
            }
        }
    }

    // Ensure we're at end of mhit total size.
    cur.seek(SeekFrom::Start(start + total_size as u64))?;

    // Capture the entire raw mhit (header + mhods) for lossless replay.
    // The iPod firmware is sensitive to the exact mhod set, order, and content
    // that iTunes wrote — rebuilding them from metadata loses information that
    // the firmware depends on for playback routing.
    let raw_mhit_header = {
        let s = start as usize;
        let end = s + total_size as usize;
        let inner = cur.get_ref();
        if end <= inner.len() {
            Some(inner[s..end].to_vec())
        } else {
            None
        }
    };

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
        total_tracks: if total_tracks > 0 {
            Some(total_tracks as u16)
        } else {
            None
        },
        disc_number: if disc_number > 0 {
            Some(disc_number as u16)
        } else {
            None
        },
        total_discs: if total_discs > 0 {
            Some(total_discs as u16)
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
        filetype_string,
        raw_mhit_header,
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
    let db_id = cur.read_u64::<LittleEndian>()?; // +24 persistent database ID

    // Preserve raw mhbd header for lossless replay of firmware-critical fields.
    let raw_mhbd_header = if (mhbd_header_size as usize) <= data.len() {
        Some(data[..mhbd_header_size as usize].to_vec())
    } else {
        None
    };

    cur.seek(SeekFrom::Start(mhbd_header_size as u64))?;

    let mut tracks = Vec::new();
    let mut playlists = Vec::new();
    let mut raw_smart_playlists: Option<Vec<u8>> = None;

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
            5 => {
                // Smart playlists — preserve raw blob for lossless round-trip.
                let start = mhsd_start as usize;
                let end = start + mhsd_total_size as usize;
                if end <= data.len() {
                    raw_smart_playlists = Some(data[start..end].to_vec());
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

    let db = IpodDatabase::from_parsed(
        db_version,
        db_id,
        tracks,
        playlists,
        raw_mhbd_header,
        raw_smart_playlists,
        mount_point,
    );

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
            total_tracks: None,
            disc_number: Some(1),
            total_discs: None,
            total_time_ms: Some(240000),
            year: Some(2024),
            file_size: 5_000_000,
            bitrate: Some(320),
            sample_rate: Some(44100),
            ipod_path: ":iPod_Control:Music:F00:abcdef.mp3".into(),
            filetype: 0x4d503320, // "MP3 "
            filetype_string: None,
            raw_mhit_header: None,
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
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 1000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
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
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 2000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F01:b.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
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
            total_tracks: None,
            disc_number: Some(2),
            total_discs: None,
            total_time_ms: Some(300000),
            year: Some(1999),
            file_size: 8_000_000,
            bitrate: Some(256),
            sample_rate: Some(48000),
            ipod_path: ":iPod_Control:Music:F05:XYZW.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
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
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 100,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
        });

        let mut bytes = itunesdb_write::serialize(&db);

        // Find the first mhod inside the track dataset (after mhit), not in album/playlist headers.
        let mhit_pos = bytes.windows(4).position(|w| w == b"mhit").unwrap();
        let mhod_pos = mhit_pos
            + bytes[mhit_pos..]
                .windows(4)
                .position(|w| w == b"mhod")
                .unwrap();
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
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 50,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:b.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
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

    #[test]
    fn test_serialized_structure() {
        use byteorder::{LittleEndian, ReadBytesExt};
        use std::io::Cursor;

        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "B Song".into(),
            artist: "A Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: Some("Rock".into()),
            track_number: None,
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 100,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
        });
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "A Song".into(),
            artist: "B Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: Some("Pop".into()),
            track_number: None,
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 200,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F01:b.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
        });

        let bytes = itunesdb_write::serialize(&db);
        let mut cur = Cursor::new(bytes.as_slice());

        // mhbd header is 244 bytes.
        let mut magic = [0u8; 4];
        cur.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"mhbd");
        let mhbd_hdr = cur.read_u32::<LittleEndian>().unwrap();
        assert_eq!(mhbd_hdr, 244, "mhbd header should be 244 bytes");
        let mhbd_total = cur.read_u32::<LittleEndian>().unwrap();
        assert_eq!(
            mhbd_total as usize,
            bytes.len(),
            "mhbd total_size should match file"
        );
        cur.read_u32::<LittleEndian>().unwrap(); // db_type
        cur.read_u32::<LittleEndian>().unwrap(); // db_version
        let num_datasets = cur.read_u32::<LittleEndian>().unwrap();
        assert_eq!(num_datasets, 8, "should have 8 datasets (libgpod standard)");

        // Walk datasets and check order matches libgpod: 1, 3, 2, 4, 8, 6, 10, 5.
        cur.set_position(mhbd_hdr as u64);
        let expected_types = [1, 3, 2, 4, 8, 6, 10, 5];
        for &expected in &expected_types {
            cur.read_exact(&mut magic).unwrap();
            assert_eq!(&magic, b"mhsd");
            let _ds_hdr = cur.read_u32::<LittleEndian>().unwrap();
            let ds_total = cur.read_u32::<LittleEndian>().unwrap();
            let ds_type = cur.read_u32::<LittleEndian>().unwrap();
            assert_eq!(ds_type, expected, "dataset type mismatch");
            cur.set_position(cur.position() - 16 + ds_total as u64);
        }

        // Find first mhit and check header size is 624.
        let mhit_pos = bytes.windows(4).position(|w| w == b"mhit").unwrap();
        let mhit_hdr = u32::from_le_bytes(bytes[mhit_pos + 4..mhit_pos + 8].try_into().unwrap());
        assert_eq!(
            mhit_hdr, 624,
            "mhit header should be 624 bytes (iTunes format)"
        );
    }

    #[test]
    fn test_sort_index_padding_before_entries() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Zebra".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 100,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:a.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
        });
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Apple".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: None,
            year: None,
            file_size: 200,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F01:b.mp3".into(),
            filetype: 0x4d503320,
            filetype_string: None,
            raw_mhit_header: None,
        });

        let bytes = itunesdb_write::serialize(&db);

        // Find the first mhod type=52 (sort index).
        let mut pos = 0;
        while pos + 16 < bytes.len() {
            if &bytes[pos..pos + 4] == b"mhod" {
                let mtype = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
                if mtype == 52 {
                    // sort_type at +24, count at +28, then 40 bytes padding, then entries.
                    let sort_type =
                        u32::from_le_bytes(bytes[pos + 24..pos + 28].try_into().unwrap());
                    let count = u32::from_le_bytes(bytes[pos + 28..pos + 32].try_into().unwrap());
                    assert_eq!(count, 2);

                    // 40 bytes of padding (offsets +32 to +72 from mhod start).
                    let padding = &bytes[pos + 32..pos + 72];
                    assert_eq!(
                        padding, &[0u8; 40],
                        "40-byte padding should be before entries"
                    );

                    // Entries start at +72.
                    let entry0 = u32::from_le_bytes(bytes[pos + 72..pos + 76].try_into().unwrap());
                    let entry1 = u32::from_le_bytes(bytes[pos + 76..pos + 80].try_into().unwrap());

                    // sort_type=3 is title sort. "Apple" (index 1) < "Zebra" (index 0).
                    if sort_type == 3 {
                        assert_eq!(entry0, 1, "Apple (index 1) should sort first");
                        assert_eq!(entry1, 0, "Zebra (index 0) should sort second");
                    }
                    break;
                }
            }
            pos += 1;
        }
    }

    #[test]
    fn test_sort_index_correct_order() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        let titles = ["Cherry", "Apple", "Banana"];
        for (i, title) in titles.iter().enumerate() {
            db.add_track(IpodTrack {
                dbid: 0,
                track_id: 0,
                title: title.to_string(),
                artist: "Artist".into(),
                album: "Album".into(),
                album_artist: None,
                genre: None,
                track_number: None,
                total_tracks: None,
                disc_number: None,
                total_discs: None,
                total_time_ms: None,
                year: None,
                file_size: 100,
                bitrate: None,
                sample_rate: None,
                ipod_path: format!(":iPod_Control:Music:F0{i}:x.mp3"),
                filetype: 0x4d503320,
                filetype_string: None,
                raw_mhit_header: None,
            });
        }

        let bytes = itunesdb_write::serialize(&db);

        // Find title sort index (sort_type=3).
        let mut pos = 0;
        while pos + 80 < bytes.len() {
            if &bytes[pos..pos + 4] == b"mhod" {
                let mtype = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
                let sort_type = u32::from_le_bytes(bytes[pos + 24..pos + 28].try_into().unwrap());
                if mtype == 52 && sort_type == 3 {
                    // Entries at +72 (after 40-byte padding).
                    let e0 = u32::from_le_bytes(bytes[pos + 72..pos + 76].try_into().unwrap());
                    let e1 = u32::from_le_bytes(bytes[pos + 76..pos + 80].try_into().unwrap());
                    let e2 = u32::from_le_bytes(bytes[pos + 80..pos + 84].try_into().unwrap());

                    // Alphabetical: Apple(1), Banana(2), Cherry(0).
                    assert_eq!(e0, 1, "Apple should be first");
                    assert_eq!(e1, 2, "Banana should be second");
                    assert_eq!(e2, 0, "Cherry should be third");
                    return;
                }
            }
            pos += 1;
        }
        panic!("sort_type=3 mhod not found");
    }

    /// Simulate the real-world scenario: parse an existing DB (with raw mhit
    /// headers), add a new track (from-scratch mhit), serialize, and re-parse.
    /// Both old and new tracks must survive.
    #[test]
    fn test_mixed_raw_and_scratch_tracks() {
        // Step 1: Create initial DB with one track, serialize to get raw headers.
        let mut db1 = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db1.add_track(IpodTrack {
            title: "Existing Song".into(),
            artist: "Old Artist".into(),
            album: "Old Album".into(),
            album_artist: None,
            genre: Some("Jazz".into()),
            track_number: Some(1),
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            total_time_ms: Some(200000),
            year: Some(2020),
            file_size: 4_000_000,
            bitrate: Some(256),
            sample_rate: Some(44100),
            ipod_path: ":iPod_Control:Music:F00:old.mp3".into(),
            filetype: 0x4d503320,
            ..Default::default()
        });
        let bytes1 = itunesdb_write::serialize(&db1);

        // Step 2: Parse it back — this track now has raw_mhit_header.
        let mut db2 = parse(&bytes1, PathBuf::from("/mnt/IPOD")).unwrap();
        assert_eq!(db2.tracks.len(), 1);
        assert!(
            db2.tracks[0].raw_mhit_header.is_some(),
            "parsed track should have raw header"
        );

        // Step 3: Add a NEW track (no raw header — will use from-scratch path).
        db2.add_track(IpodTrack {
            title: "New Song".into(),
            artist: "New Artist".into(),
            album: "New Album".into(),
            album_artist: Some("New AA".into()),
            genre: Some("Rock".into()),
            track_number: Some(5),
            total_tracks: None,
            disc_number: Some(2),
            total_discs: None,
            total_time_ms: Some(300000),
            year: Some(2025),
            file_size: 6_000_000,
            bitrate: Some(320),
            sample_rate: Some(48000),
            ipod_path: ":iPod_Control:Music:F01:new.mp3".into(),
            filetype: 0x4d503320,
            ..Default::default()
        });

        // Step 4: Serialize the mixed DB and parse again.
        let bytes2 = itunesdb_write::serialize(&db2);
        let db3 = parse(&bytes2, PathBuf::from("/mnt/IPOD")).unwrap();

        // Both tracks must be present and correct.
        assert_eq!(db3.tracks.len(), 2);

        let old = &db3.tracks[0];
        assert_eq!(old.title, "Existing Song");
        assert_eq!(old.artist, "Old Artist");
        assert_eq!(old.album, "Old Album");
        assert_eq!(old.genre.as_deref(), Some("Jazz"));
        assert_eq!(old.track_number, Some(1));
        assert_eq!(old.total_time_ms, Some(200000));
        assert_eq!(old.year, Some(2020));
        assert_eq!(old.file_size, 4_000_000);
        assert_eq!(old.bitrate, Some(256));
        assert_eq!(old.sample_rate, Some(44100));

        let new = &db3.tracks[1];
        assert_eq!(new.title, "New Song");
        assert_eq!(new.artist, "New Artist");
        assert_eq!(new.album, "New Album");
        assert_eq!(new.album_artist.as_deref(), Some("New AA"));
        assert_eq!(new.genre.as_deref(), Some("Rock"));
        assert_eq!(new.track_number, Some(5));
        assert_eq!(new.disc_number, Some(2));
        assert_eq!(new.total_time_ms, Some(300000));
        assert_eq!(new.year, Some(2025));
        assert_eq!(new.file_size, 6_000_000);
        assert_eq!(new.bitrate, Some(320));
        assert_eq!(new.sample_rate, Some(48000));

        // Master playlist should have both tracks.
        assert_eq!(db3.playlists[0].track_ids.len(), 2);

        // Verify mhit header size is 624 for both tracks (iTunes format).
        let mhit_positions: Vec<usize> = bytes2
            .windows(4)
            .enumerate()
            .filter(|(_, w)| w == b"mhit")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(mhit_positions.len(), 2, "should have 2 mhit records");
        for pos in &mhit_positions {
            let hs = u32::from_le_bytes(bytes2[pos + 4..pos + 8].try_into().unwrap());
            assert_eq!(hs, 624, "mhit header should be 624 bytes (iTunes format)");
        }
    }

    /// Verify that timestamps in from-scratch mhit headers are non-zero Mac timestamps.
    #[test]
    fn test_from_scratch_timestamps_populated() {
        let mut db = IpodDatabase::new(PathBuf::from("/mnt/IPOD"));
        db.add_track(IpodTrack {
            title: "T".into(),
            artist: "A".into(),
            album: "B".into(),
            ipod_path: ":iPod_Control:Music:F00:t.mp3".into(),
            filetype: 0x4d503320,
            file_size: 100,
            ..Default::default()
        });

        let bytes = itunesdb_write::serialize(&db);

        // Find the mhit.
        let mhit_pos = bytes.windows(4).position(|w| w == b"mhit").unwrap();

        // libgpod layout: +0x20 = time_modified, +0x68 = time_added
        let time_modified =
            u32::from_le_bytes(bytes[mhit_pos + 0x20..mhit_pos + 0x24].try_into().unwrap());
        let time_added =
            u32::from_le_bytes(bytes[mhit_pos + 0x68..mhit_pos + 0x6C].try_into().unwrap());

        // HFS epoch (1904-based): timestamps should be > 3_800_000_000 (roughly 2024+).
        assert!(
            time_modified > 3_800_000_000,
            "time_modified should be an HFS timestamp, got {time_modified}"
        );
        assert!(
            time_added > 3_800_000_000,
            "time_added should be an HFS timestamp, got {time_added}"
        );
    }
}
