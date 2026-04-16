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

/// Mac epoch offset: seconds between Unix epoch (1970-01-01) and Mac epoch (2001-01-01).
const MAC_EPOCH_OFFSET: u64 = 978_307_200;

/// Current time as a Mac timestamp (seconds since 2001-01-01 00:00:00 UTC).
fn mac_timestamp_now() -> u32 {
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Mac timestamps are u32, wrapping is fine (won't overflow until ~2137).
    unix_secs.saturating_sub(MAC_EPOCH_OFFSET) as u32
}

/// Strip leading articles ("The ", "A ", "An ") for sort keys.
fn strip_article(s: &str) -> String {
    let lower = s.to_lowercase();
    for prefix in &["the ", "a ", "an "] {
        if lower.starts_with(prefix) {
            return s[prefix.len()..].to_string();
        }
    }
    s.to_string()
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
    buf.write_u32::<LittleEndian>(0).unwrap(); // string_position
    buf.write_u32::<LittleEndian>(string_bytes.len() as u32)
        .unwrap();
    buf.write_u32::<LittleEndian>(1).unwrap(); // encoding (1 = UTF-16LE)
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding

    buf.write_all(&string_bytes).unwrap();
    buf
}

/// Write an mhit from a preserved raw header + rebuilt mhods.
/// Patches only the fields we manage: total_size, num_mhods, artwork_count,
/// has_artwork. Everything else (timestamps, media_type, hashes, gapless data)
/// stays as the firmware originally wrote it.
fn write_mhit_from_raw(
    raw_header: &[u8],
    track: &IpodTrack,
    artwork_count: u32,
    _album_id: u32,
) -> Vec<u8> {
    let header_size = u32::from_le_bytes(raw_header[4..8].try_into().unwrap());

    // Build child mhods (same as the from-scratch path).
    let mhods = build_track_mhods(track, artwork_count);
    let num_mhods = mhods.len() as u32;
    let total_size = header_size + mhods.iter().map(|m| m.len() as u32).sum::<u32>();

    let mut buf = raw_header.to_vec();
    buf.resize(header_size as usize, 0);

    // Patch managed fields.
    put_u32_at(&mut buf, 8, total_size); // total_size
    put_u32_at(&mut buf, 12, num_mhods); // num_mhods
    put_u32_at(&mut buf, 132, artwork_count); // artwork_count
    put_u32_at(&mut buf, 156, if artwork_count > 0 { 1 } else { 0 }); // has_artwork

    for mhod in &mhods {
        buf.extend_from_slice(mhod);
    }
    buf
}

/// Build the child mhods for a track. Shared between from-scratch and from-raw paths.
fn build_track_mhods(track: &IpodTrack, _artwork_count: u32) -> Vec<Vec<u8>> {
    let mut mhods = Vec::new();

    // Title (type 1) — always present.
    mhods.push(write_mhod(1, &track.title));

    // Location (type 2) — always present.
    mhods.push(write_mhod(2, &track.ipod_path));

    // Album (type 3).
    if !track.album.is_empty() {
        mhods.push(write_mhod(3, &track.album));
    }

    // Artist (type 4).
    if !track.artist.is_empty() {
        mhods.push(write_mhod(4, &track.artist));
    }

    // Genre (type 5).
    if let Some(ref genre) = track.genre {
        mhods.push(write_mhod(5, genre));
    }

    // Filetype string (type 6).
    let filetype_str = match track.filetype {
        0x4d503320 => "MPEG audio file",
        0x4d344120 => "AAC audio file",
        0x4d345020 => "Protected AAC audio file",
        0x57415620 => "WAV audio file",
        0x574d4120 => "WMA audio file",
        _ => "Audio file",
    };
    mhods.push(write_mhod(6, filetype_str));

    // Album artist (type 14).
    if let Some(ref aa) = track.album_artist {
        mhods.push(write_mhod(14, aa));
    }

    // Sort string mhods.
    if !track.artist.is_empty() {
        mhods.push(write_mhod(22, &track.artist));
    }
    let sort_album_artist = track.album_artist.as_deref().unwrap_or(&track.artist);
    if !sort_album_artist.is_empty() {
        mhods.push(write_mhod(23, sort_album_artist));
    }
    if !track.title.is_empty() {
        mhods.push(write_mhod(27, &strip_article(&track.title)));
    }
    if !track.album.is_empty() {
        mhods.push(write_mhod(28, &strip_article(&track.album)));
    }

    mhods
}

/// Write an mhit chunk with its child mhods. Returns the serialized bytes.
///
/// `artwork_count` is the number of thumbnail entries (0 if no artwork).
/// `album_id` is the cross-reference into the mhsd type=4 album list.
fn write_mhit(track: &IpodTrack, artwork_count: u32, album_id: u32) -> Vec<u8> {
    // If we have a raw mhit header from a parsed database, replay it and only
    // patch the fields we actively manage. This preserves all firmware-critical
    // fields (media_type, gapless data, per-track hashes, timestamps, etc.)
    // that we don't yet know how to generate from scratch.
    if let Some(ref raw) = track.raw_mhit_header {
        return write_mhit_from_raw(raw, track, artwork_count, album_id);
    }

    // Header size 0x270 (624) matches iPod Classic/Video/Mini (db versions 0x73-0x75).
    let header_size: u32 = 0x270;

    // Build child mhods (shared with the raw-header replay path).
    let mhods = build_track_mhods(track, artwork_count);
    let num_mhods = mhods.len() as u32;
    let mhod_bytes: usize = mhods.iter().map(|m| m.len()).sum();
    let total_size = header_size + mhod_bytes as u32;

    let now = mac_timestamp_now();
    let sample_rate = track.sample_rate.unwrap_or(44100);
    let sr = (sample_rate as u32) << 16;

    // Core fields (+0 through +211), written sequentially.
    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhit").unwrap(); // +0
    buf.write_u32::<LittleEndian>(header_size).unwrap(); // +4
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +8
    buf.write_u32::<LittleEndian>(num_mhods).unwrap(); // +12
    buf.write_u32::<LittleEndian>(track.track_id).unwrap(); // +16
    buf.write_u32::<LittleEndian>(1).unwrap(); // +20  visible
    buf.write_u32::<LittleEndian>(track.filetype).unwrap(); // +24
    buf.write_u8(1).unwrap(); // +28  type (1=audio, 2=video)
    buf.write_u8(0).unwrap(); // +29  compilation
    buf.write_u8(0).unwrap(); // +30  rating
    buf.write_u8(0).unwrap(); // +31  padding
    buf.write_u32::<LittleEndian>(now).unwrap(); // +32  date_modified (Mac timestamp)
    buf.write_u32::<LittleEndian>(track.file_size).unwrap(); // +36
    buf.write_u32::<LittleEndian>(track.total_time_ms.unwrap_or(0))
        .unwrap(); // +40
    buf.write_u32::<LittleEndian>(track.track_number.unwrap_or(0) as u32)
        .unwrap(); // +44
    buf.write_u32::<LittleEndian>(0).unwrap(); // +48  total_tracks
    buf.write_u32::<LittleEndian>(track.year.unwrap_or(0) as u32)
        .unwrap(); // +52
    buf.write_u32::<LittleEndian>(track.bitrate.unwrap_or(0) as u32)
        .unwrap(); // +56
    buf.write_u32::<LittleEndian>(sr).unwrap(); // +60  sample_rate (fixed-point Hz<<16)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +64  volume_adjust
    buf.write_u32::<LittleEndian>(0).unwrap(); // +68  start_time
    buf.write_u32::<LittleEndian>(0).unwrap(); // +72  stop_time
    buf.write_u32::<LittleEndian>(0).unwrap(); // +76  sound_check
    buf.write_u32::<LittleEndian>(0).unwrap(); // +80  play_count
    buf.write_u32::<LittleEndian>(0).unwrap(); // +84  last_played
    buf.write_u32::<LittleEndian>(now).unwrap(); // +88  date_added_to_device (Mac timestamp)
    buf.write_u32::<LittleEndian>(track.disc_number.unwrap_or(0) as u32)
        .unwrap(); // +92
    buf.write_u32::<LittleEndian>(0).unwrap(); // +96  disc_total
    buf.write_u32::<LittleEndian>(0).unwrap(); // +100 sort_order
    buf.write_u32::<LittleEndian>(now).unwrap(); // +104 date_added (Mac timestamp)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +108 date_released
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // +112 dbid
    buf.write_u32::<LittleEndian>(0).unwrap(); // +120 checked
    buf.write_u32::<LittleEndian>(0xffff_0000).unwrap(); // +124 app_rating (ref: always 0xffff0000+)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +128 bpm
    buf.write_u32::<LittleEndian>(artwork_count).unwrap(); // +132 artwork_count
                                                           // +136: sample_rate as IEEE 754 float (NOT fixed-point like +60).
    let sr_float = (sample_rate as f32).to_bits();
    buf.write_u32::<LittleEndian>(sr_float).unwrap(); // +136 sample_rate_float
    buf.write_u32::<LittleEndian>(0).unwrap(); // +140 date_released2
    buf.write_u32::<LittleEndian>(0x0c).unwrap(); // +144 explicit_flag (ref: always >= 0x0c)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +148 skip_count
    buf.write_u32::<LittleEndian>(0).unwrap(); // +152 last_skipped
    buf.write_u32::<LittleEndian>(if artwork_count > 0 { 1 } else { 0 })
        .unwrap(); // +156 has_artwork
    buf.write_u32::<LittleEndian>(0).unwrap(); // +160 skip_shuffling
    buf.write_u32::<LittleEndian>(1).unwrap(); // +164 remember_playback_pos (ref: always 1 or 2)
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // +168 dbid2 (duplicate)
    buf.write_u32::<LittleEndian>(0x0001_0000).unwrap(); // +176 lyrics_flag (ref: always 0x00010000+)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +180 movie_flag
    buf.write_u32::<LittleEndian>(0).unwrap(); // +184 mark_unplayed
    buf.write_u32::<LittleEndian>(track.file_size).unwrap(); // +188 size_on_disk
    buf.write_u32::<LittleEndian>(now).unwrap(); // +192 date_modified2 (mirrors +32)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +196 per_track_hash (firmware may recompute)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +200 media_type_legacy
    buf.write_u32::<LittleEndian>(1).unwrap(); // +204 (ref: 99% nonzero, typically 1)
    buf.write_u32::<LittleEndian>(1).unwrap(); // +208 has_gapless_data

    // Extended fields (+212 through +623). The iPod Classic firmware (db_version
    // 0x75) requires several of these to be populated — zeroing them causes
    // the firmware to reject the database entirely.
    //
    // Fields are written at their exact offsets via a position-indexed buffer.
    // Unknown/unused fields stay zero.
    buf.resize(header_size as usize, 0);

    // +248: gapless_encoding_drain (per-track, we don't have this — leave 0)
    // +252: gapless_encoding_delay (per-track, we don't have this — leave 0)
    // +256: has_gapless_encoding — set to 1 (firmware expects this on all tracks)
    put_u32_at(&mut buf, 256, 1);
    // +288: album_id — cross-reference into mhsd type=4 album list.
    put_u32_at(&mut buf, 288, album_id);
    // +292, +296: unknown constants (identical across all iTunes-written tracks).
    put_u32_at(&mut buf, 292, 0xfc75_23d0);
    put_u32_at(&mut buf, 296, 0xa999_476a);
    // +300: secondary_id — reference shows this equals file_size, not dbid.
    put_u32_at(&mut buf, 300, track.file_size);
    // +308: media_type_detailed — audio type flags.
    //        Reference shows 0x03038080 most common, 0x03038003 for some.
    put_u32_at(&mut buf, 308, 0x0303_8080);
    // +312: media subtype flags — 0x8080 for most audio, 0x8003 for some.
    put_u32_at(&mut buf, 312, 0x0000_8080);
    // +352: unknown — observed values 0x64..0x2a7. Set to 0x64 (100).
    put_u32_at(&mut buf, 352, 0x64);
    // +360: unknown — constant 1 on all tracks.
    put_u32_at(&mut buf, 360, 1);
    // +480: artist_id — reference shows constant 0x218 across all tracks.
    put_u32_at(&mut buf, 480, 0x218);
    // +500: per-track unique reference. Reference shows track_id + 1.
    put_u32_at(&mut buf, 500, track.track_id.wrapping_add(1));
    // +524: media_type — 1=audio. Reference shows ~49% set (only newer tracks).
    put_u32_at(&mut buf, 524, 1);

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
/// The index is a permutation array: entry[i] = index of track that belongs at
/// position i when sorted by the given field. The firmware uses this to build
/// the browse UI without re-sorting on device.
///
/// Layout: mhod header (24) + sort_type (4) + count (4) + padding (40) + entries (count*4).
/// The 40-byte padding block comes BEFORE entries — the firmware skips it.
fn write_sort_index(tracks: &[IpodTrack], sort_type: u32) -> Vec<u8> {
    let count = tracks.len();

    let mut indices: Vec<usize> = (0..count).collect();
    indices.sort_by(|&a, &b| {
        let key_a = sort_key(&tracks[a], sort_type);
        let key_b = sort_key(&tracks[b], sort_type);
        key_a.cmp(&key_b)
    });

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

/// Get the lowercase sort key for a track by sort type.
/// Base: 3=title, 4=album, 5=artist, 7=genre, 18=composer.
/// Extended: 29=sort_album_artist, 30=sort_composer, 31=sort_title,
/// 35=sort_album, 36=sort_artist. Extended use the same base field data.
fn sort_key(track: &IpodTrack, sort_type: u32) -> String {
    match sort_type {
        3 | 31 => track.title.to_lowercase(),
        4 | 35 => track.album.to_lowercase(),
        5 | 36 => track.artist.to_lowercase(),
        7 => track.genre.as_deref().unwrap_or("").to_lowercase(),
        18 | 30 => String::new(), // composer — not tracked yet
        29 => track
            .album_artist
            .as_deref()
            .unwrap_or(&track.artist)
            .to_lowercase(),
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

    // Sort tracks by the same key used for type 52.
    let mut indices: Vec<usize> = (0..tracks.len()).collect();
    indices.sort_by(|&a, &b| sort_key(&tracks[a], sort_type).cmp(&sort_key(&tracks[b], sort_type)));

    // Group by first character (uppercase).
    let mut groups: Vec<(char, u32, u32)> = Vec::new(); // (letter, start_idx, count)
    for (sorted_pos, &track_idx) in indices.iter().enumerate() {
        let key = sort_key(&tracks[track_idx], sort_type);
        let first_char = key.chars().next().unwrap_or('\0').to_ascii_uppercase();
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
    let mut album_id = 1u32;
    for ((album_name, artist), count) in &album_map {
        album_data.extend(write_mhia(album_id, album_name, artist, *count));
        album_id += 1;
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

    // Build track dataset (mhsd type 1 = mhlt + mhits).
    let mut track_data = Vec::new();
    for track in &db.tracks {
        let art_count = art_counts.get(&track.dbid).copied().unwrap_or(0);
        let alb_id = dbid_to_group_id.get(&track.dbid).copied().unwrap_or(0);
        track_data.extend(write_mhit(track, art_count, alb_id));
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
    // Dataset order matches original: 4 (albums), 1 (tracks), 3 (podcasts), 2 (playlists), 5 (smart).
    let mhbd_header_size: u32 = crate::hash::MHBD_HEADER_SIZE;
    let num_datasets: u32 = 5;
    let mhbd_total_size = mhbd_header_size
        + mhsd4.len() as u32
        + mhsd1.len() as u32
        + mhsd3.len() as u32
        + mhsd2.len() as u32
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

    // Dataset order: 4 (albums), 1 (tracks), 3 (podcasts), 2 (playlists), 5 (smart).
    result.extend(mhsd4);
    result.extend(mhsd1);
    result.extend(mhsd3);
    result.extend(mhsd2);
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
