//! Serialize an [`IpodDatabase`] into the iTunesDB binary format.
//!
//! Produces a complete database with all five dataset types the iPod Classic
//! firmware expects:
//!
//! 1. **Albums** (mhsd type 4) — built from track metadata, one mhia per
//!    unique (album, artist) pair with mhod types 200/201/202.
//! 2. **Tracks** (mhsd type 1) — mhit headers (624 bytes each) with child
//!    mhods for title, path, album, artist, genre, filetype, album artist,
//!    and sort strings (types 22/23/27/28).
//! 3. **Podcasts** (mhsd type 3) — clone of the playlist dataset.
//! 4. **Playlists** (mhsd type 2) — master playlist with sort indexes
//!    (type 52, 10 sort types), letter indexes (type 53), column info
//!    (types 100/102), and mhip entries with album group references.
//! 5. **Smart playlists** (mhsd type 5) — replayed from raw bytes if parsed
//!    from an existing database, otherwise empty.
//!
//! The mhbd header is 244 bytes (hash58-compatible) and includes the database
//! persistent ID (`db_id`), version, and dataset count. After serialization,
//! call [`crate::hash::sign_hash58`] to compute and write the HMAC-SHA1 hash
//! required by iPod Classic.

use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;

use crate::encoding::encode_utf16le;
use crate::{IpodDatabase, IpodDbError, IpodTrack};

/// Seconds between the Mac HFS epoch (1904-01-01) and Unix epoch (1970-01-01).
/// The iPod uses the 1904-based HFS epoch for all timestamp fields, NOT the
/// 2001-based Cocoa epoch.
const HFS_EPOCH_OFFSET: u64 = 2_082_844_800;

/// Current time as a Mac HFS timestamp (seconds since 1904-01-01 00:00:00 UTC).
fn mac_timestamp_now() -> u32 {
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    unix_secs.wrapping_add(HFS_EPOCH_OFFSET) as u32
}

/// Write a little-endian u32 at an exact byte offset in a buffer.
fn put_u32_at(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Write a string mhod chunk. Returns the serialized bytes.
fn write_mhod(mhod_type: u32, value: &str) -> Vec<u8> {
    let string_bytes = encode_utf16le(value);
    let header_size: u32 = 24;
    let string_header_size: u32 = 16;
    let total_size = header_size + string_header_size + string_bytes.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhod").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(mhod_type).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    // String sub-header.
    buf.write_u32::<LittleEndian>(1).unwrap(); // unknown (always 1 in iTunes-written DBs)
    buf.write_u32::<LittleEndian>(string_bytes.len() as u32)
        .unwrap();
    buf.write_u32::<LittleEndian>(1).unwrap(); // encoding (1 = UTF-16LE)
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    buf.write_all(&string_bytes).unwrap();
    buf
}

/// Replay a parsed track's raw bytes (mhit header + mhods) verbatim.
///
/// The raw blob captured during parse includes the exact mhod set, order, and
/// content that iTunes wrote. The iPod firmware is sensitive to this — rebuilding
/// mhods from metadata loses information the firmware depends on for playback.
///
/// We only patch artwork fields in the mhit header since those may change when
/// artwork is added after parse.
fn write_mhit_from_raw(raw_blob: &[u8], artwork_count: u32) -> Vec<u8> {
    let mut buf = raw_blob.to_vec();

    // Patch artwork fields if they changed.
    // The offsets depend on the header size of the original blob:
    // - iTunes 624-byte (0x270): artwork_count=u32@+132, has_artwork=u32@+156
    // - libgpod 584-byte (0x248): artwork_count=u16@+0x7C, has_artwork=u8@+0xA4
    let header_size = if buf.len() >= 8 {
        u32::from_le_bytes(buf[4..8].try_into().unwrap_or([0; 4]))
    } else {
        0
    };
    if header_size == 0x270 && buf.len() >= 160 {
        // iTunes layout (624-byte headers).
        put_u32_at(&mut buf, 132, artwork_count);
        put_u32_at(&mut buf, 156, if artwork_count > 0 { 1 } else { 0 });
    } else if header_size == 0x248 && buf.len() >= 0xA5 {
        // libgpod layout (584-byte headers).
        buf[0x7C..0x7E].copy_from_slice(&(artwork_count as u16).to_le_bytes());
        buf[0xA4] = if artwork_count > 0 { 1 } else { 2 };
    }

    buf
}

/// Build the child mhods for a new track (from-scratch path only).
///
/// For parsed tracks, the raw blob (header + mhods) is replayed verbatim.
/// This function is only called for tracks added after parse.
fn build_track_mhods(track: &IpodTrack, _artwork_count: u32) -> Vec<Vec<u8>> {
    let mut mhods = Vec::new();

    // Title (type 1).
    mhods.push(write_mhod(1, &track.title));

    // Artist (type 4).
    if !track.artist.is_empty() {
        mhods.push(write_mhod(4, &track.artist));
    }

    // Album (type 3).
    if !track.album.is_empty() {
        mhods.push(write_mhod(3, &track.album));
    }

    // Genre (type 5).
    if let Some(ref genre) = track.genre {
        mhods.push(write_mhod(5, genre));
    }

    // Filetype string (type 6). The firmware uses this to select the decoder,
    // so ALAC must say "Apple Lossless audio file", not "AAC audio file".
    let filetype_str = track
        .filetype_string
        .as_deref()
        .unwrap_or(match track.filetype {
            0x4d503320 => "MPEG audio file",
            0x4d344120 => "AAC audio file",
            0x4d345020 => "Protected AAC audio file",
            0x57415620 => "WAV audio file",
            0x574d4120 => "WMA audio file",
            _ => "Audio file",
        });
    mhods.push(write_mhod(6, filetype_str));

    // Location (type 2) — firmware needs this to find the file.
    mhods.push(write_mhod(2, &track.ipod_path));

    // Album artist (type 14).
    if let Some(ref aa) = track.album_artist {
        mhods.push(write_mhod(14, aa));
    }

    mhods
}

/// Write an mhit chunk with its child mhods. Returns the serialized bytes.
///
/// `artwork_count` is the number of thumbnail entries (0 if no artwork).
/// `album_id` is the cross-reference into the mhsd type=4 album list.
/// Write an mhit for a new track, ported from libgpod's mk_mhit.
///
/// Field layout follows `references/libgpod/src/itdb_itunesdb.c` line 3956.
/// Header size is 0x248 (584 bytes) — libgpod's standard. The iPod Classic
/// firmware accepts this even though iTunes writes 0x270 (624).
fn write_mhit(
    track: &IpodTrack,
    artwork_count: u32,
    album_id: u32,
    artist_id: u32,
    id_0x24: u64,
) -> Vec<u8> {
    // If we have the raw blob (header + mhods) from a parsed database, replay
    // it verbatim. This preserves the exact mhod set, order, and content that
    // iTunes wrote — the firmware depends on this for playback routing.
    if let Some(ref raw) = track.raw_mhit_header {
        return write_mhit_from_raw(raw, artwork_count);
    }

    // Match iTunes (0x270 = 624). libgpod writes 0x248 = 584, but when mixed
    // with iTunes-written 624-byte headers in the same DB, some firmware
    // versions reject the smaller ones. Pad to 624 for consistency.
    let header_size: u32 = 0x270;

    let mhods = build_track_mhods(track, artwork_count);
    let num_mhods = mhods.len() as u32;
    let mhod_bytes: usize = mhods.iter().map(|m| m.len()).sum();
    let total_size = header_size + mhod_bytes as u32;

    let now = mac_timestamp_now();
    let sample_rate = track.sample_rate.unwrap_or(44100);
    let sr_fixed = (sample_rate as u32) << 16;
    let sr_float = (sample_rate as f32).to_bits();

    // Determine format-specific defaults (from libgpod itdb_track_set_defaults).
    let is_mp3 = track.filetype == 0x4d503320;
    let is_aac = track.filetype == 0x4d344120;
    let unk126: u16 = if is_mp3 || is_aac { 0xffff } else { 0 };
    let unk144: u16 = if is_mp3 {
        0x000c
    } else if is_aac {
        0x0033
    } else {
        0
    };
    // unk204 at +0xCC: libgpod writes 1 for MP3, 0 for others. But iTunes
    // actually writes 0x02000003 for AAC tracks (verified from golden DB).
    // MP3s synced by iTunes play even with libgpod's value, so MP3=1 is fine.
    // AAC tracks need the iTunes-compatible value or they skip on playback.
    let unk204: u32 = if is_mp3 {
        1
    } else if is_aac {
        0x0200_0003
    } else {
        0
    };

    // Sequential write matching libgpod mk_mhit field order exactly.
    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhit").unwrap(); // +0x00
    buf.write_u32::<LittleEndian>(header_size).unwrap(); // +0x04
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +0x08
    buf.write_u32::<LittleEndian>(num_mhods).unwrap(); // +0x0C
                                                       // +0x10
    buf.write_u32::<LittleEndian>(track.track_id).unwrap(); // id
    buf.write_u32::<LittleEndian>(1).unwrap(); // visible
    buf.write_u32::<LittleEndian>(track.filetype).unwrap(); // filetype_marker (FourCC: MP3/M4A/WAV)
    buf.write_u8(0).unwrap(); // +0x1C type1 (0=CBR, 1=VBR MP3)
    buf.write_u8(if is_mp3 { 1 } else { 0 }).unwrap(); // +0x1D type2 (1=MP3, 0=AAC)
    buf.write_u8(0).unwrap(); // +0x1E compilation
    buf.write_u8(track.rating).unwrap(); // +0x1F rating (0..=100, 5-star × 20)
                                         // +0x20
    buf.write_u32::<LittleEndian>(now).unwrap(); // time_modified
    buf.write_u32::<LittleEndian>(track.file_size).unwrap(); // size
    buf.write_u32::<LittleEndian>(track.total_time_ms.unwrap_or(0))
        .unwrap(); // tracklen
    buf.write_u32::<LittleEndian>(track.track_number.unwrap_or(0) as u32)
        .unwrap(); // track_nr
                   // +0x30
    buf.write_u32::<LittleEndian>(track.total_tracks.unwrap_or(0) as u32)
        .unwrap(); // tracks (total tracks in album)
    buf.write_u32::<LittleEndian>(track.year.unwrap_or(0) as u32)
        .unwrap(); // year
    buf.write_u32::<LittleEndian>(track.bitrate.unwrap_or(0) as u32)
        .unwrap(); // bitrate
    buf.write_u32::<LittleEndian>(sr_fixed).unwrap(); // samplerate (fixed-point)
                                                      // +0x40
    buf.write_u32::<LittleEndian>(0).unwrap(); // volume
    buf.write_u32::<LittleEndian>(0).unwrap(); // starttime
    buf.write_u32::<LittleEndian>(0).unwrap(); // stoptime
    buf.write_u32::<LittleEndian>(0).unwrap(); // soundcheck
                                               // +0x50
    buf.write_u32::<LittleEndian>(track.play_count).unwrap(); // playcount
    buf.write_u32::<LittleEndian>(track.play_count).unwrap(); // playcount2 (libgpod duplicates)
    buf.write_u32::<LittleEndian>(track.last_played).unwrap(); // last_played (Mac HFS epoch)
    buf.write_u32::<LittleEndian>(track.disc_number.unwrap_or(0) as u32)
        .unwrap(); // cd_nr
                   // +0x60
    buf.write_u32::<LittleEndian>(track.total_discs.unwrap_or(0) as u32)
        .unwrap(); // cds (total discs)
    buf.write_u32::<LittleEndian>(0).unwrap(); // drm_userid
    buf.write_u32::<LittleEndian>(now).unwrap(); // time_added
    buf.write_u32::<LittleEndian>(0).unwrap(); // bookmark_time
                                               // +0x70
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // dbid
    buf.write_u8(0).unwrap(); // +0x78 checked
    buf.write_u8(0).unwrap(); // +0x79 app_rating
    buf.write_u16::<LittleEndian>(0).unwrap(); // +0x7A BPM
    buf.write_u16::<LittleEndian>(artwork_count as u16).unwrap(); // +0x7C artwork_count
    buf.write_u16::<LittleEndian>(unk126).unwrap(); // +0x7E unk126 (0xFFFF for MP3/AAC)
                                                    // +0x80
    buf.write_u32::<LittleEndian>(0).unwrap(); // artwork_size (JPEG bytes, 0 if none)
    buf.write_u32::<LittleEndian>(0).unwrap(); // unk132
    buf.write_u32::<LittleEndian>(sr_float).unwrap(); // +0x88 samplerate2 (IEEE float)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0x8C time_released
                                               // +0x90
    buf.write_u16::<LittleEndian>(unk144).unwrap(); // unk144 (0x0C=MP3, 0x33=AAC)
    buf.write_u16::<LittleEndian>(0).unwrap(); // explicit_flag
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0x94 unk148
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0x98 unk152
    buf.write_u32::<LittleEndian>(track.skip_count).unwrap(); // +0x9C skipcount
                                                              // +0xA0
    buf.write_u32::<LittleEndian>(track.last_skipped).unwrap(); // last_skipped (Mac HFS epoch)
    buf.write_u8(if artwork_count > 0 { 1 } else { 2 }).unwrap(); // +0xA4 has_artwork (1=yes, 2=no)
    buf.write_u8(0).unwrap(); // +0xA5 skip_when_shuffling
    buf.write_u8(0).unwrap(); // +0xA6 remember_playback_position
    buf.write_u8(0).unwrap(); // +0xA7 flag4
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // +0xA8 dbid2
                                                        // +0xB0
    buf.write_u8(0).unwrap(); // lyrics_flag
    buf.write_u8(0).unwrap(); // movie_flag
    buf.write_u8(0x01).unwrap(); // mark_unplayed (0x01 = normal music)
    buf.write_u8(0).unwrap(); // unk179
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0xB4 unk180
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0xB8 pregap
                                               // +0xBC samplecount: total PCM samples = duration_ms * sample_rate / 1000.
                                               // The firmware needs this for gapless playback and seeking.
    let samplecount: u64 = track
        .total_time_ms
        .map(|ms| (ms as u64) * (sample_rate as u64) / 1000)
        .unwrap_or(0);
    buf.write_u64::<LittleEndian>(samplecount).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0xC4 unk196
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0xC8 postgap
    buf.write_u32::<LittleEndian>(unk204).unwrap(); // +0xCC unk204 (1=MP3, 0=other)
                                                    // +0xD0
    buf.write_u32::<LittleEndian>(0x0000_0001).unwrap(); // mediatype (1=audio)
    buf.write_u32::<LittleEndian>(0).unwrap(); // season_nr
    buf.write_u32::<LittleEndian>(0).unwrap(); // episode_nr
    buf.write_u32::<LittleEndian>(0).unwrap(); // unk220
                                               // +0xE0
    for _ in 0..4 {
        buf.write_u32::<LittleEndian>(0).unwrap(); // unk224..unk236
    }
    // +0xF0
    buf.write_u32::<LittleEndian>(0).unwrap(); // unk240
    buf.write_u32::<LittleEndian>(0).unwrap(); // unk244
    buf.write_u32::<LittleEndian>(0).unwrap(); // gapless_data
    buf.write_u32::<LittleEndian>(0).unwrap(); // unk252
                                               // +0x100
    buf.write_u16::<LittleEndian>(1).unwrap(); // gapless_track_flag (1 = has gapless info)
    buf.write_u16::<LittleEndian>(0).unwrap(); // gapless_album_flag
    for _ in 0..7 {
        buf.write_u32::<LittleEndian>(0).unwrap(); // 7x zero padding
    }
    // +0x120
    buf.write_u32::<LittleEndian>(album_id).unwrap(); // album_id
    buf.write_u64::<LittleEndian>(id_0x24).unwrap(); // +0x124 id_0x24 (from mhbd+0x24, not db_id!)
    buf.write_u32::<LittleEndian>(track.file_size).unwrap(); // +0x12C size (duplicate)
                                                             // +0x130
    buf.write_u32::<LittleEndian>(0).unwrap();
    // +0x134 mystery constant. libgpod writes 0x0000_8080_8080_8080 but iTunes
    // actually writes 0x0000_8080_0303_8080 — verified from golden DB on iPod
    // Classic. The low 4 bytes differ.
    buf.write_u64::<LittleEndian>(0x0000_8080_0303_8080)
        .unwrap(); // +0x134
    buf.write_u32::<LittleEndian>(0).unwrap();
    // +0x140
    buf.write_u32::<LittleEndian>(0).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0x148 (0 for music, 0x00010001 for books)
    for _ in 0..5 {
        buf.write_u32::<LittleEndian>(0).unwrap(); // 5x zero
    }
    // +0x160
    buf.write_u32::<LittleEndian>(0).unwrap(); // mhii_link (artwork DB link)
    buf.write_u32::<LittleEndian>(0).unwrap();
    buf.write_u32::<LittleEndian>(1).unwrap(); // +0x168 hardcoded 1
    buf.write_u32::<LittleEndian>(0).unwrap();
    // +0x170
    for _ in 0..28 {
        buf.write_u32::<LittleEndian>(0).unwrap(); // 28x zero padding
    }
    // +0x1E0
    buf.write_u32::<LittleEndian>(artist_id).unwrap(); // artist_id (references mhsd type 8)
    for _ in 0..4 {
        buf.write_u32::<LittleEndian>(0).unwrap(); // 4x zero
    }
    // +0x1F4
    buf.write_u32::<LittleEndian>(0).unwrap(); // composer_id
                                               // 20x u32 padding to 0x248, except +0x20C = 2 (matches iTunes golden).
    for i in 0..20 {
        let off = 0x1F8 + i * 4;
        let val: u32 = if off == 0x20C { 2 } else { 0 };
        buf.write_u32::<LittleEndian>(val).unwrap();
    }
    // Pad from 0x248 (584) to 0x270 (624) to match iTunes header size.
    // Existing tracks (from iTunes) have 624-byte headers; mixing 584-byte
    // new tracks with them causes the firmware to reject the smaller ones.
    while buf.len() < header_size as usize {
        buf.push(0);
    }

    debug_assert_eq!(
        buf.len(),
        header_size as usize,
        "mhit header size mismatch: wrote {} bytes, expected {}",
        buf.len(),
        header_size
    );

    for mhod in &mhods {
        buf.extend_from_slice(mhod);
    }
    buf
}

/// Write an mhip (playlist item) chunk with child mhod type 100.
///
/// Each mhip references a track and includes a group_id + child mhod that
/// the iPod firmware uses for album grouping in the playlist view.
fn write_mhip(track_id: u32, group_id: u32) -> Vec<u8> {
    // Child mhod type=100 (album grouping reference, 44 bytes).
    let child_size: u32 = 44;
    let mut child = Vec::with_capacity(child_size as usize);
    child.write_all(b"mhod").unwrap();
    child.write_u32::<LittleEndian>(24).unwrap(); // header_size
    child.write_u32::<LittleEndian>(child_size).unwrap(); // total_size
    child.write_u32::<LittleEndian>(100).unwrap(); // type
    child.write_u32::<LittleEndian>(0).unwrap(); // padding
    child.write_u32::<LittleEndian>(0).unwrap(); // padding
    child.write_u32::<LittleEndian>(group_id).unwrap(); // group_id reference
                                                        // Pad rest to child_size.
    let written = child.len();
    for _ in 0..(child_size as usize - written) {
        child.write_u8(0).unwrap();
    }

    let header_size: u32 = 76;
    let total_size: u32 = header_size + child_size;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhip").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(1).unwrap(); // num_mhods = 1
    buf.write_u32::<LittleEndian>(0).unwrap(); // podcast_grouping
    buf.write_u32::<LittleEndian>(group_id).unwrap(); // group_id
    buf.write_u32::<LittleEndian>(track_id).unwrap();

    // Pad to header_size.
    let written = buf.len();
    for _ in 0..(header_size as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&child).unwrap();
    buf
}

/// Sort type constants for mhod type 52 (sort index).
/// Each corresponds to a browse category on the iPod.
/// Standard: 3=title, 4=album, 5=artist, 7=genre, 18=composer.
/// Extended (db_version >= 0x73): 29=sort_album_artist, 30=sort_composer,
/// 31=sort_title, 35=sort_album, 36=sort_artist.
/// Extended sorts use the same data as their base types (we use identical keys).
const SORT_TYPES: &[u32] = &[3, 5, 4, 7, 18, 35, 36, 29, 30, 31];

/// Build a type-52 sort index mhod for a given sort type.
///
/// Ported from libgpod's mhod52_sort_* functions (itdb_itunesdb.c line 4151).
/// Sort order uses collation keys with tiebreakers matching libgpod:
///   title:    title
///   album:    album → cd_nr → track_nr → title
///   artist:   artist → album → cd_nr → track_nr → title
///   genre:    genre → artist → album → cd_nr → track_nr → title
///   composer: composer → album → cd_nr → track_nr → title
///
/// Layout: mhod header (24) + sort_type (4) + count (4) + padding (40) + entries (count*4).
fn write_sort_index(tracks: &[IpodTrack], sort_type: u32) -> Vec<u8> {
    let count = tracks.len();

    let mut indices: Vec<usize> = (0..count).collect();
    indices.sort_by(|&a, &b| sort_compare(&tracks[a], &tracks[b], sort_type));

    let header_size: u32 = 24;
    let payload_size = 4 + 4 + 40 + (count as u32) * 4;
    let total_size = header_size + payload_size;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhod").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(52).unwrap(); // mhod type = sort index
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    // Sort index payload.
    buf.write_u32::<LittleEndian>(sort_type).unwrap();
    buf.write_u32::<LittleEndian>(count as u32).unwrap();
    // 40 bytes padding BEFORE entries (firmware skips these).
    for _ in 0..10 {
        buf.write_u32::<LittleEndian>(0).unwrap();
    }
    for &idx in &indices {
        buf.write_u32::<LittleEndian>(idx as u32).unwrap();
    }

    buf
}

/// Build a collation key for a string, matching libgpod's approach.
///
/// Uses lowercasing as a simple locale-independent collation. libgpod uses
/// `g_utf8_collate_key()` which is locale-dependent, but any consistent
/// ordering works — the firmware uses the pre-computed index as-is.
fn collate_key(s: &str) -> String {
    s.to_lowercase()
}

/// Compare two tracks for a given sort type, with tiebreakers matching libgpod.
///
/// Ported from libgpod's mhod52_sort_* functions (itdb_itunesdb.c line 4151).
fn sort_compare(a: &IpodTrack, b: &IpodTrack, sort_type: u32) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let cmp_title = || collate_key(&a.title).cmp(&collate_key(&b.title));
    let cmp_album = || collate_key(&a.album).cmp(&collate_key(&b.album));
    let cmp_artist = || collate_key(&a.artist).cmp(&collate_key(&b.artist));
    let cmp_genre = || {
        let ga = a.genre.as_deref().unwrap_or("");
        let gb = b.genre.as_deref().unwrap_or("");
        collate_key(ga).cmp(&collate_key(gb))
    };
    let cmp_cd = || a.disc_number.unwrap_or(0).cmp(&b.disc_number.unwrap_or(0));
    let cmp_track = || {
        a.track_number
            .unwrap_or(0)
            .cmp(&b.track_number.unwrap_or(0))
    };

    match sort_type {
        // title sort: title only
        3 | 31 => cmp_title(),
        // album sort: album → cd_nr → track_nr → title
        4 | 35 => cmp_album()
            .then_with(cmp_cd)
            .then_with(cmp_track)
            .then_with(cmp_title),
        // artist sort: artist → album → cd_nr → track_nr → title
        5 | 36 => cmp_artist()
            .then_with(cmp_album)
            .then_with(cmp_cd)
            .then_with(cmp_track)
            .then_with(cmp_title),
        // genre sort: genre → artist → album → cd_nr → track_nr → title
        7 => cmp_genre()
            .then_with(cmp_artist)
            .then_with(cmp_album)
            .then_with(cmp_cd)
            .then_with(cmp_track)
            .then_with(cmp_title),
        // composer sort: just title (we don't track composer)
        18 | 30 => cmp_title(),
        // album artist sort: same as artist
        29 => {
            let aa_a = a.album_artist.as_deref().unwrap_or(&a.artist);
            let aa_b = b.album_artist.as_deref().unwrap_or(&b.artist);
            collate_key(aa_a)
                .cmp(&collate_key(aa_b))
                .then_with(cmp_album)
                .then_with(cmp_cd)
                .then_with(cmp_track)
                .then_with(cmp_title)
        }
        _ => Ordering::Equal,
    }
}

/// Get the collation key for a track field by sort type (for letter index grouping).
fn sort_key_for_letter(track: &IpodTrack, sort_type: u32) -> String {
    match sort_type {
        3 | 31 => collate_key(&track.title),
        4 | 35 => collate_key(&track.album),
        5 | 36 => collate_key(&track.artist),
        7 => collate_key(track.genre.as_deref().unwrap_or("")),
        18 | 30 => String::new(),
        29 => collate_key(track.album_artist.as_deref().unwrap_or(&track.artist)),
        _ => String::new(),
    }
}

/// Build a type-53 library_playlist_index mhod for a given sort type.
///
/// This is the alphabetical letter scrubber: groups the sorted track list by
/// first character so the iPod can display the A-B-C overlay when scrolling.
///
/// Layout: mhod header (24) + sort_type (4) + count (4) + padding (8) +
///         entries (count * 12): each is (letter u32, start_index u32, group_count u32).
///
/// Only emitted for sort types that have meaningful letter groupings.
/// Sort types 35/36/29/30/31 typically mirror their base types.
fn write_letter_index(tracks: &[IpodTrack], sort_type: u32) -> Option<Vec<u8>> {
    if tracks.is_empty() {
        return None;
    }

    // Sort tracks by the same comparator used for type 52.
    let mut indices: Vec<usize> = (0..tracks.len()).collect();
    indices.sort_by(|&a, &b| sort_compare(&tracks[a], &tracks[b], sort_type));

    // Group by first alphanumeric character (uppercase for alpha, '0' for digits).
    // Ported from libgpod's jump_table_letter (itdb_itunesdb.c line 4225).
    let mut groups: Vec<(char, u32, u32)> = Vec::new(); // (letter, start_idx, count)
    for (sorted_pos, &track_idx) in indices.iter().enumerate() {
        let key = sort_key_for_letter(&tracks[track_idx], sort_type);
        let first_char = key
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| {
                if c.is_alphabetic() {
                    c.to_ascii_uppercase()
                } else {
                    '0' // digits get '0' per libgpod
                }
            })
            .unwrap_or('0');
        if let Some(last) = groups.last_mut() {
            if last.0 == first_char {
                last.2 += 1;
                continue;
            }
        }
        groups.push((first_char, sorted_pos as u32, 1));
    }

    let count = groups.len() as u32;
    let header_size: u32 = 24;
    let payload_size = 4 + 4 + 8 + count * 12;
    let total_size = header_size + payload_size;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhod").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(53).unwrap(); // mhod type = letter index
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    // Payload.
    buf.write_u32::<LittleEndian>(sort_type).unwrap();
    buf.write_u32::<LittleEndian>(count).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    for (letter, start, group_count) in &groups {
        buf.write_u32::<LittleEndian>(*letter as u32).unwrap();
        buf.write_u32::<LittleEndian>(*start).unwrap();
        buf.write_u32::<LittleEndian>(*group_count).unwrap();
    }

    Some(buf)
}

/// Sort types that get a type-53 letter index paired with their type-52.
/// Not all sort types need one — only the ones the firmware uses for the
/// alphabetical scrubber in browse mode.
const LETTER_INDEX_SORT_TYPES: &[u32] = &[3, 4, 5, 7, 18, 29];

/// Column info mhod type 100 (648 bytes). Constant blob from iTunes reference.
/// Describes the column layout for list view in the iPod UI.
fn write_column_info_100() -> Vec<u8> {
    let mut buf = vec![0u8; 648];
    // mhod header
    buf[0..4].copy_from_slice(b"mhod");
    buf[4..8].copy_from_slice(&24u32.to_le_bytes()); // header_size
    buf[8..12].copy_from_slice(&648u32.to_le_bytes()); // total_size
    buf[12..16].copy_from_slice(&100u32.to_le_bytes()); // type
                                                        // Payload flags from reference.
    buf[40] = 0x01;
    buf[42] = 0x01;
    buf[568] = 0x8c;
    buf
}

/// Column info mhod type 102 (356 bytes). Constant blob from iTunes reference.
fn write_column_info_102() -> Vec<u8> {
    let mut buf = vec![0u8; 356];
    // mhod header
    buf[0..4].copy_from_slice(b"mhod");
    buf[4..8].copy_from_slice(&24u32.to_le_bytes()); // header_size
    buf[8..12].copy_from_slice(&356u32.to_le_bytes()); // total_size
    buf[12..16].copy_from_slice(&102u32.to_le_bytes()); // type
                                                        // Payload flags from reference.
    buf[24] = 0x01;
    buf[32] = 0x01;
    buf[100] = 0x04;
    buf[164] = 0x78;
    buf
}

/// Write an mhyp (playlist) chunk with its children. Returns serialized bytes.
fn write_mhyp(
    playlist: &crate::IpodPlaylist,
    dbid_to_track_id: &std::collections::HashMap<u64, u32>,
    dbid_to_group_id: &std::collections::HashMap<u64, u32>,
    playlist_id: u64,
    tracks: &[IpodTrack],
) -> Vec<u8> {
    let header_size: u32 = 184; // match iPod Classic header size

    // Build child mhods (playlist name + column info).
    let mut mhods = Vec::new();
    let mut num_mhods: u32 = 0;

    if !playlist.name.is_empty() {
        mhods.extend(write_mhod(1, &playlist.name));
        num_mhods += 1;
    }

    // Column info (types 100, 102) — present on every playlist in iTunes DBs.
    mhods.extend(write_column_info_100());
    num_mhods += 1;
    mhods.extend(write_column_info_102());
    num_mhods += 1;

    // Master playlist gets sort indexes (type 52) and letter indexes (type 53)
    // for all browse categories. These tell the firmware how to order tracks in
    // the browse UI. The iPod will show "no music" without them when our tracks
    // dataset is also present (the firmware doesn't rebuild them on its own).
    if playlist.is_master && !tracks.is_empty() {
        for &sort_type in SORT_TYPES {
            mhods.extend(write_sort_index(tracks, sort_type));
            num_mhods += 1;

            // Pair with letter index if this sort type uses one.
            if LETTER_INDEX_SORT_TYPES.contains(&sort_type) {
                if let Some(letter_idx) = write_letter_index(tracks, sort_type) {
                    mhods.extend(letter_idx);
                    num_mhods += 1;
                }
            }
        }
    }

    // Build mhips.
    let mut mhips = Vec::new();
    let mut num_mhips: u32 = 0;
    for &dbid in &playlist.track_ids {
        if let Some(&tid) = dbid_to_track_id.get(&dbid) {
            let gid = dbid_to_group_id.get(&dbid).copied().unwrap_or(0);
            mhips.extend(write_mhip(tid, gid));
            num_mhips += 1;
        }
    }

    let total_size = header_size + mhods.len() as u32 + mhips.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhyp").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(num_mhods).unwrap();
    buf.write_u32::<LittleEndian>(num_mhips).unwrap();
    buf.write_u32::<LittleEndian>(if playlist.is_master { 1 } else { 0 })
        .unwrap();
    buf.write_u64::<LittleEndian>(0).unwrap(); // timestamp
    buf.write_u64::<LittleEndian>(playlist_id).unwrap(); // playlist persistent ID

    // Pad to header_size.
    let written = buf.len();
    for _ in 0..(header_size as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&mhods).unwrap();
    buf.write_all(&mhips).unwrap();
    buf
}

/// Build an mhia (album item) chunk. Returns serialized bytes.
fn write_mhia(album_id: u32, album_name: &str, artist: &str, track_count: u32) -> Vec<u8> {
    let header_size: u32 = 88;

    let mut mhods = Vec::new();
    let mut num_mhods: u32 = 0;

    // Album name (type 200).
    mhods.extend(write_mhod(200, album_name));
    num_mhods += 1;

    // Album artist (type 201).
    mhods.extend(write_mhod(201, artist));
    num_mhods += 1;

    // Sort album artist (type 202).
    mhods.extend(write_mhod(202, artist));
    num_mhods += 1;

    let total_size = header_size + mhods.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhia").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(num_mhods).unwrap();
    buf.write_u32::<LittleEndian>(album_id).unwrap(); // +16
    buf.write_u32::<LittleEndian>(0).unwrap(); // +20  unknown
    buf.write_u64::<LittleEndian>(0).unwrap(); // +24  persistent_id
    buf.write_u32::<LittleEndian>(track_count).unwrap(); // +32
    buf.write_u64::<LittleEndian>(0).unwrap(); // +36  unknown

    // Pad to header_size.
    let written = buf.len();
    for _ in 0..(header_size as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&mhods).unwrap();
    buf
}

/// Build the mhsd type=4 album dataset from track data.
fn build_album_dataset(tracks: &[IpodTrack]) -> Vec<u8> {
    // Group tracks by (album, artist) to build album entries.
    let mut album_map: std::collections::BTreeMap<(String, String), u32> =
        std::collections::BTreeMap::new();
    for track in tracks {
        let key = (
            track.album.clone(),
            track
                .album_artist
                .clone()
                .unwrap_or_else(|| track.artist.clone()),
        );
        *album_map.entry(key).or_insert(0) += 1;
    }

    let mut album_data = Vec::new();
    for (album_id, ((album_name, artist), count)) in (1u32..).zip(album_map.iter()) {
        album_data.extend(write_mhia(album_id, album_name, artist, *count));
    }

    let album_count = album_map.len() as u32;

    let mhla_header_size: u32 = 92;
    let mut mhla = Vec::new();
    mhla.write_all(b"mhla").unwrap();
    mhla.write_u32::<LittleEndian>(mhla_header_size).unwrap();
    mhla.write_u32::<LittleEndian>(album_count).unwrap();
    let written = mhla.len();
    for _ in 0..(mhla_header_size as usize - written) {
        mhla.write_u8(0).unwrap();
    }
    mhla.extend(album_data);

    let mhsd_header_size: u32 = 96;
    let mhsd_total_size = mhsd_header_size + mhla.len() as u32;
    let mut mhsd = Vec::new();
    mhsd.write_all(b"mhsd").unwrap();
    mhsd.write_u32::<LittleEndian>(mhsd_header_size).unwrap();
    mhsd.write_u32::<LittleEndian>(mhsd_total_size).unwrap();
    mhsd.write_u32::<LittleEndian>(4).unwrap(); // type = album list
    let written = mhsd.len();
    for _ in 0..(mhsd_header_size as usize - written) {
        mhsd.write_u8(0).unwrap();
    }
    mhsd.extend(mhla);
    mhsd
}

/// Serialize an `IpodDatabase` into iTunesDB binary format.
pub fn serialize(db: &IpodDatabase) -> Vec<u8> {
    let dbid_to_track_id: std::collections::HashMap<u64, u32> =
        db.tracks.iter().map(|t| (t.dbid, t.track_id)).collect();

    // Pre-build artwork count map for O(1) lookup per track.
    let art_counts: std::collections::HashMap<u64, u32> = db
        .artwork_store
        .as_ref()
        .map(|s| {
            s.track_artworks
                .iter()
                .map(|ta| (ta.dbid, ta.thumbnails.len() as u32))
                .collect()
        })
        .unwrap_or_default();

    // Build dbid → album group_id map for mhip children.
    let mut album_groups: std::collections::BTreeMap<(String, String), u32> =
        std::collections::BTreeMap::new();
    let mut dbid_to_group_id: std::collections::HashMap<u64, u32> =
        std::collections::HashMap::new();
    let mut next_group_id = 1u32;
    for track in &db.tracks {
        let key = (
            track.album.clone(),
            track
                .album_artist
                .clone()
                .unwrap_or_else(|| track.artist.clone()),
        );
        let gid = *album_groups.entry(key).or_insert_with(|| {
            let id = next_group_id;
            next_group_id += 1;
            id
        });
        dbid_to_group_id.insert(track.dbid, gid);
    }

    // Read id_0x24 from mhbd header (+0x24, 8 bytes). Tracks reference this
    // persistent DB ID at their mhit +0x124 — NOT the db_id at mhbd+0x18.
    // libgpod calls this `itdb->priv->id_0x24`.
    let id_0x24 = db
        .raw_mhbd_header
        .as_ref()
        .and_then(|h| h.get(0x24..0x2C))
        .and_then(|s| s.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(db.db_id);

    // Build dbid → artist_id map. Sequential IDs starting at 1, same ordering
    // as the mhsd type 8 builder below (BTreeMap by artist name).
    let mut artist_groups: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();
    let mut next_artist_id = 1u32;
    let mut dbid_to_artist_id: std::collections::HashMap<u64, u32> =
        std::collections::HashMap::new();
    for track in &db.tracks {
        if !track.artist.is_empty() {
            let aid = *artist_groups
                .entry(track.artist.clone())
                .or_insert_with(|| {
                    let id = next_artist_id;
                    next_artist_id += 1;
                    id
                });
            dbid_to_artist_id.insert(track.dbid, aid);
        }
    }

    // Build track dataset (mhsd type 1 = mhlt + mhits).
    let mut track_data = Vec::new();
    for track in &db.tracks {
        let art_count = art_counts.get(&track.dbid).copied().unwrap_or(0);
        let alb_id = dbid_to_group_id.get(&track.dbid).copied().unwrap_or(0);
        let art_id = dbid_to_artist_id.get(&track.dbid).copied().unwrap_or(0);
        track_data.extend(write_mhit(track, art_count, alb_id, art_id, id_0x24));
    }

    let mhlt_header_size: u32 = 92;
    let mut mhlt = Vec::new();
    mhlt.write_all(b"mhlt").unwrap();
    mhlt.write_u32::<LittleEndian>(mhlt_header_size).unwrap();
    mhlt.write_u32::<LittleEndian>(db.tracks.len() as u32)
        .unwrap();
    // Pad to header_size.
    let written = mhlt.len();
    for _ in 0..(mhlt_header_size as usize - written) {
        mhlt.write_u8(0).unwrap();
    }
    mhlt.extend(track_data);

    let mhsd1_header_size: u32 = 96;
    let mhsd1_total_size = mhsd1_header_size + mhlt.len() as u32;
    let mut mhsd1 = Vec::new();
    mhsd1.write_all(b"mhsd").unwrap();
    mhsd1.write_u32::<LittleEndian>(mhsd1_header_size).unwrap();
    mhsd1.write_u32::<LittleEndian>(mhsd1_total_size).unwrap();
    mhsd1.write_u32::<LittleEndian>(1).unwrap(); // type = tracks
    let written = mhsd1.len();
    for _ in 0..(mhsd1_header_size as usize - written) {
        mhsd1.write_u8(0).unwrap();
    }
    mhsd1.extend(mhlt);

    // Build playlist dataset (mhsd type 2 = mhlp + mhyps).
    let mut playlist_data = Vec::new();
    for (i, pl) in db.playlists.iter().enumerate() {
        playlist_data.extend(write_mhyp(
            pl,
            &dbid_to_track_id,
            &dbid_to_group_id,
            (i + 1) as u64,
            &db.tracks,
        ));
    }

    let mhlp_header_size: u32 = 92;
    let mut mhlp = Vec::new();
    mhlp.write_all(b"mhlp").unwrap();
    mhlp.write_u32::<LittleEndian>(mhlp_header_size).unwrap();
    mhlp.write_u32::<LittleEndian>(db.playlists.len() as u32)
        .unwrap();
    let written = mhlp.len();
    for _ in 0..(mhlp_header_size as usize - written) {
        mhlp.write_u8(0).unwrap();
    }
    mhlp.extend(playlist_data);

    // Build mhsd wrappers for the playlist data.
    // Type 2 = playlists, Type 3 = podcast playlists (identical copy, required by firmware).
    let mhsd_pl_header_size: u32 = 96;

    let mut mhsd2 = Vec::new();
    mhsd2.write_all(b"mhsd").unwrap();
    mhsd2
        .write_u32::<LittleEndian>(mhsd_pl_header_size)
        .unwrap();
    mhsd2
        .write_u32::<LittleEndian>(mhsd_pl_header_size + mhlp.len() as u32)
        .unwrap();
    mhsd2.write_u32::<LittleEndian>(2).unwrap(); // type = playlists
    let written = mhsd2.len();
    for _ in 0..(mhsd_pl_header_size as usize - written) {
        mhsd2.write_u8(0).unwrap();
    }
    mhsd2.extend(&mhlp);

    // Type 3 = podcast playlists (duplicate of type 2 with different type field).
    let mut mhsd3 = Vec::new();
    mhsd3.write_all(b"mhsd").unwrap();
    mhsd3
        .write_u32::<LittleEndian>(mhsd_pl_header_size)
        .unwrap();
    mhsd3
        .write_u32::<LittleEndian>(mhsd_pl_header_size + mhlp.len() as u32)
        .unwrap();
    mhsd3.write_u32::<LittleEndian>(3).unwrap(); // type = podcasts (copy of playlists)
    let written = mhsd3.len();
    for _ in 0..(mhsd_pl_header_size as usize - written) {
        mhsd3.write_u8(0).unwrap();
    }
    mhsd3.extend(&mhlp);

    // Type 4 = album list (mhla with mhia entries built from tracks).
    let mhsd4 = build_album_dataset(&db.tracks);

    // Type 8 = artist list (mhli with mhii entries). Reuses `artist_groups`
    // built above so the mhii IDs match what's written into mhit +0x1E0.
    let mhsd8 = {
        let artist_map = &artist_groups;
        // Build mhii entries (each: 80-byte header + mhod with artist name).
        let mut mhii_data = Vec::new();
        for (artist_name, artist_id) in artist_map {
            let name_mhod = write_mhod(201, artist_name); // MHOD_ID_ALBUM_ARTIST_MHII
            let mhii_header_size: u32 = 80;
            let mhii_total = mhii_header_size + name_mhod.len() as u32;
            let mut mhii = Vec::new();
            mhii.write_all(b"mhii").unwrap();
            mhii.write_u32::<LittleEndian>(mhii_header_size).unwrap();
            mhii.write_u32::<LittleEndian>(mhii_total).unwrap();
            mhii.write_u32::<LittleEndian>(1).unwrap(); // num children
            mhii.write_u32::<LittleEndian>(*artist_id).unwrap(); // artist id
            mhii.write_u64::<LittleEndian>(0).unwrap(); // sql_id
            mhii.write_u32::<LittleEndian>(2).unwrap(); // unknown=2 per libgpod
            let written = mhii.len();
            for _ in 0..(mhii_header_size as usize - written) {
                mhii.write_u8(0).unwrap();
            }
            mhii.extend(name_mhod);
            mhii_data.extend(mhii);
        }
        // mhli header
        let mhli_header_size: u32 = 92;
        let mut mhli = Vec::new();
        mhli.write_all(b"mhli").unwrap();
        mhli.write_u32::<LittleEndian>(mhli_header_size).unwrap();
        mhli.write_u32::<LittleEndian>(artist_map.len() as u32)
            .unwrap();
        let written = mhli.len();
        for _ in 0..(mhli_header_size as usize - written) {
            mhli.write_u8(0).unwrap();
        }
        mhli.extend(mhii_data);
        // mhsd wrapper
        let mhsd_hs: u32 = 96;
        let mut mhsd = Vec::new();
        mhsd.write_all(b"mhsd").unwrap();
        mhsd.write_u32::<LittleEndian>(mhsd_hs).unwrap();
        mhsd.write_u32::<LittleEndian>(mhsd_hs + mhli.len() as u32)
            .unwrap();
        mhsd.write_u32::<LittleEndian>(8).unwrap(); // type = artists
        let written = mhsd.len();
        for _ in 0..(mhsd_hs as usize - written) {
            mhsd.write_u8(0).unwrap();
        }
        mhsd.extend(mhli);
        mhsd
    };

    // Types 6 and 10 = empty datasets (libgpod writes these, purpose unknown).
    let build_empty_mhsd = |ds_type: u32| -> Vec<u8> {
        let mhlt_hs: u32 = 92;
        let mut mhlt = Vec::new();
        mhlt.write_all(b"mhlt").unwrap();
        mhlt.write_u32::<LittleEndian>(mhlt_hs).unwrap();
        mhlt.write_u32::<LittleEndian>(0).unwrap(); // 0 entries
        let written = mhlt.len();
        for _ in 0..(mhlt_hs as usize - written) {
            mhlt.write_u8(0).unwrap();
        }
        let mhsd_hs: u32 = 96;
        let mut mhsd = Vec::new();
        mhsd.write_all(b"mhsd").unwrap();
        mhsd.write_u32::<LittleEndian>(mhsd_hs).unwrap();
        mhsd.write_u32::<LittleEndian>(mhsd_hs + mhlt.len() as u32)
            .unwrap();
        mhsd.write_u32::<LittleEndian>(ds_type).unwrap();
        let written = mhsd.len();
        for _ in 0..(mhsd_hs as usize - written) {
            mhsd.write_u8(0).unwrap();
        }
        mhsd.extend(mhlt);
        mhsd
    };
    let mhsd6 = build_empty_mhsd(6);
    let mhsd10 = build_empty_mhsd(10);

    // Type 5 = smart playlists. Replay raw blob if available, otherwise empty.
    let mhsd5 = if let Some(ref raw) = db.raw_smart_playlists {
        raw.clone()
    } else {
        let mhlp5_header_size: u32 = 92;
        let mut mhlp5 = Vec::new();
        mhlp5.write_all(b"mhlp").unwrap();
        mhlp5.write_u32::<LittleEndian>(mhlp5_header_size).unwrap();
        mhlp5.write_u32::<LittleEndian>(0).unwrap(); // 0 smart playlists
        let written = mhlp5.len();
        for _ in 0..(mhlp5_header_size as usize - written) {
            mhlp5.write_u8(0).unwrap();
        }

        let mhsd5_header_size: u32 = 96;
        let mhsd5_total_size = mhsd5_header_size + mhlp5.len() as u32;
        let mut mhsd5 = Vec::new();
        mhsd5.write_all(b"mhsd").unwrap();
        mhsd5.write_u32::<LittleEndian>(mhsd5_header_size).unwrap();
        mhsd5.write_u32::<LittleEndian>(mhsd5_total_size).unwrap();
        mhsd5.write_u32::<LittleEndian>(5).unwrap();
        let written = mhsd5.len();
        for _ in 0..(mhsd5_header_size as usize - written) {
            mhsd5.write_u8(0).unwrap();
        }
        mhsd5.extend(mhlp5);
        mhsd5
    };

    // mhbd header (244 bytes for hash58 compatibility with iPod Classic).
    // Dataset order matches libgpod: 1, 3, 2, 4, 8, 6, 10, 5.
    let mhbd_header_size: u32 = crate::hash::MHBD_HEADER_SIZE;
    let num_datasets: u32 = 8;
    let mhbd_total_size = mhbd_header_size
        + mhsd1.len() as u32
        + mhsd3.len() as u32
        + mhsd2.len() as u32
        + mhsd4.len() as u32
        + mhsd8.len() as u32
        + mhsd6.len() as u32
        + mhsd10.len() as u32
        + mhsd5.len() as u32;

    // If we have a raw mhbd header from a parsed database, replay it to preserve
    // all firmware-critical fields (language, platform, persistent IDs,
    // timezone, etc.), then patch only the fields that change.
    let mut result = if let Some(ref raw) = db.raw_mhbd_header {
        let mut hdr = raw.clone();
        // Ensure it's the right size.
        hdr.resize(mhbd_header_size as usize, 0);
        // Patch total_size (+0x08) and num_datasets (+0x14).
        hdr[8..12].copy_from_slice(&mhbd_total_size.to_le_bytes());
        hdr[0x14..0x18].copy_from_slice(&num_datasets.to_le_bytes());
        // Patch db_id (+0x18) in case it was updated.
        hdr[0x18..0x20].copy_from_slice(&db.db_id.to_le_bytes());
        // Zero hash58 (+0x58, 20 bytes) — will be recomputed by sign_hash58().
        hdr[0x58..0x6C].fill(0);
        // Zero hash72 (+0x72, 46 bytes) — stale hash from original content
        // would be invalid for our rebuilt datasets. Firmware should skip
        // validation when the field is zeroed.
        hdr[0x72..0xA0].fill(0);
        hdr
    } else {
        // Fresh database — build header from scratch.
        let mut hdr = Vec::with_capacity(mhbd_header_size as usize);
        hdr.write_all(b"mhbd").unwrap();
        hdr.write_u32::<LittleEndian>(mhbd_header_size).unwrap();
        hdr.write_u32::<LittleEndian>(mhbd_total_size).unwrap();
        hdr.write_u32::<LittleEndian>(1).unwrap(); // +12 db_type
        hdr.write_u32::<LittleEndian>(db.db_version).unwrap(); // +16
        hdr.write_u32::<LittleEndian>(num_datasets).unwrap(); // +20
        hdr.write_u64::<LittleEndian>(db.db_id).unwrap(); // +24 db_id
                                                          // Remaining fields zero-padded. hash58 written by sign_hash58() later.
        let written = hdr.len();
        for _ in 0..(mhbd_header_size as usize - written) {
            hdr.write_u8(0).unwrap();
        }
        hdr
    };

    // Dataset order matches libgpod: 1, 3, 2, 4, 8, 6, 10, 5.
    result.extend(mhsd1);
    result.extend(mhsd3);
    result.extend(mhsd2);
    result.extend(mhsd4);
    result.extend(mhsd8);
    result.extend(mhsd6);
    result.extend(mhsd10);
    result.extend(mhsd5);
    result
}

/// Write the database to disk with atomic temp+rename and .bak backup.
///
/// If `firewire_id` is provided, signs the database with hash58 (required for
/// iPod Classic). Pass `None` for older iPods that don't check the hash.
pub fn write_to_disk(db: &IpodDatabase, firewire_id: Option<&[u8; 20]>) -> crate::Result<()> {
    let db_path = db.db_path();

    // Ensure parent directory exists.
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Backup existing file.
    if db_path.exists() {
        let backup = db_path.with_extension("bak");
        std::fs::copy(&db_path, &backup)
            .map_err(|e| IpodDbError::Filesystem(format!("failed to create backup: {e}")))?;
    }

    // Write ArtworkDB and .ithmb files before iTunesDB so that artwork flags
    // in mhit entries never reference files that don't exist on disk yet.
    if let Some(ref store) = db.artwork_store {
        crate::artwork::ithmb::write_ithmb_files(&db.mount_point, &store.ithmb_files)?;
        if let Err(e) = crate::artwork::artworkdb::write_to_disk(&db.mount_point, store) {
            // Clean up orphaned .ithmb files so we don't leave partial artwork state.
            for f in &store.ithmb_files {
                let path = db
                    .mount_point
                    .join("iPod_Control")
                    .join("Artwork")
                    .join(&f.filename);
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    }

    let mut data = serialize(db);

    if let Some(fwid) = firewire_id {
        crate::hash::sign_hash58(&mut data, fwid)?;
    }

    // Write iTunesDB to temp file, then rename for atomicity.
    let tmp_path = db_path.with_extension("tmp");
    std::fs::write(&tmp_path, &data)?;
    let rename_result = std::fs::rename(&tmp_path, &db_path);
    if rename_result.is_err() {
        // Clean up orphaned temp file.
        let _ = std::fs::remove_file(&tmp_path);
    }
    rename_result.map_err(|e| {
        IpodDbError::Filesystem(format!("failed to atomically replace iTunesDB: {e}"))
    })?;

    Ok(())
}
