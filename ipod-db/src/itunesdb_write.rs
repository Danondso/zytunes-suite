use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;

use crate::encoding::encode_utf16le;
use crate::{IpodDatabase, IpodDbError, IpodTrack};

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

/// Write an mhit chunk with its child mhods. Returns the serialized bytes.
///
/// `artwork_count` is the number of thumbnail entries (0 if no artwork).
fn write_mhit(track: &IpodTrack, artwork_count: u32) -> Vec<u8> {
    // Header size 0x270 (624) matches iPod Classic/Video/Mini (db versions 0x73-0x75).
    let header_size: u32 = 0x270;

    // Build child mhods.
    let mut mhods = Vec::new();
    let mut num_mhods: u32 = 0;

    // Title (type 1) — always present.
    mhods.extend(write_mhod(1, &track.title));
    num_mhods += 1;

    // Location (type 2) — always present.
    mhods.extend(write_mhod(2, &track.ipod_path));
    num_mhods += 1;

    // Album (type 3).
    if !track.album.is_empty() {
        mhods.extend(write_mhod(3, &track.album));
        num_mhods += 1;
    }

    // Artist (type 4).
    if !track.artist.is_empty() {
        mhods.extend(write_mhod(4, &track.artist));
        num_mhods += 1;
    }

    // Genre (type 5).
    if let Some(ref genre) = track.genre {
        mhods.extend(write_mhod(5, genre));
        num_mhods += 1;
    }

    // Album artist (type 14).
    if let Some(ref aa) = track.album_artist {
        mhods.extend(write_mhod(14, aa));
        num_mhods += 1;
    }

    let total_size = header_size + mhods.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhit").unwrap(); // +0
    buf.write_u32::<LittleEndian>(header_size).unwrap(); // +4
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +8
    buf.write_u32::<LittleEndian>(num_mhods).unwrap(); // +12
    buf.write_u32::<LittleEndian>(track.track_id).unwrap(); // +16
    buf.write_u32::<LittleEndian>(1).unwrap(); // +20  visible
    buf.write_u32::<LittleEndian>(track.filetype).unwrap(); // +24
    buf.write_u8(0).unwrap(); // +28  type (audio)
    buf.write_u8(0).unwrap(); // +29  compilation
    buf.write_u8(0).unwrap(); // +30  rating
    buf.write_u8(0).unwrap(); // +31  padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // +32  date_modified
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
    let sr = (track.sample_rate.unwrap_or(0) as u32) << 16;
    buf.write_u32::<LittleEndian>(sr).unwrap(); // +60  sample_rate
    buf.write_u32::<LittleEndian>(0).unwrap(); // +64  volume_adjust
    buf.write_u32::<LittleEndian>(0).unwrap(); // +68  start_time
    buf.write_u32::<LittleEndian>(0).unwrap(); // +72  stop_time
    buf.write_u32::<LittleEndian>(0).unwrap(); // +76  sound_check
    buf.write_u32::<LittleEndian>(0).unwrap(); // +80  play_count
    buf.write_u32::<LittleEndian>(0).unwrap(); // +84  last_played
    buf.write_u32::<LittleEndian>(0).unwrap(); // +88  date_added_to_device
    buf.write_u32::<LittleEndian>(track.disc_number.unwrap_or(0) as u32)
        .unwrap(); // +92
    buf.write_u32::<LittleEndian>(0).unwrap(); // +96  disc_total
    buf.write_u32::<LittleEndian>(0).unwrap(); // +100 sort_order
    buf.write_u32::<LittleEndian>(0).unwrap(); // +104 date_added
    buf.write_u32::<LittleEndian>(0).unwrap(); // +108 date_released
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // +112 dbid
    buf.write_u32::<LittleEndian>(0).unwrap(); // +120 checked
    buf.write_u32::<LittleEndian>(0).unwrap(); // +124 app_rating
    buf.write_u32::<LittleEndian>(0).unwrap(); // +128 bpm
    buf.write_u32::<LittleEndian>(artwork_count).unwrap(); // +132 artwork_count
    buf.write_u32::<LittleEndian>(sr).unwrap(); // +136 sample_rate (dup)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +140 date_released2
    buf.write_u32::<LittleEndian>(0).unwrap(); // +144 explicit_flag
    buf.write_u32::<LittleEndian>(0).unwrap(); // +148 skip_count
    buf.write_u32::<LittleEndian>(0).unwrap(); // +152 last_skipped
    buf.write_u32::<LittleEndian>(if artwork_count > 0 { 1 } else { 0 })
        .unwrap(); // +156 has_artwork
    buf.write_u32::<LittleEndian>(0).unwrap(); // +160 skip_shuffling
    buf.write_u32::<LittleEndian>(0).unwrap(); // +164 remember_playback_pos
    buf.write_u64::<LittleEndian>(track.dbid).unwrap(); // +168 dbid2 (duplicate)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +176 lyrics_flag
    buf.write_u32::<LittleEndian>(0).unwrap(); // +180 movie_flag
    buf.write_u32::<LittleEndian>(0).unwrap(); // +184 mark_unplayed
    buf.write_u32::<LittleEndian>(0).unwrap(); // +188 size_on_disk (0 = let firmware calculate)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +192 date_modified2
    buf.write_u32::<LittleEndian>(0).unwrap(); // +196 hash
    buf.write_u32::<LittleEndian>(0).unwrap(); // +200 (not media_type; per-track audio property)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +204 season|episode
    buf.write_u32::<LittleEndian>(0).unwrap(); // +208 has_gapless_data

    // Pad rest to header_size. We've written 212 bytes.
    let written = buf.len();
    for _ in 0..(header_size as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&mhods).unwrap();
    buf
}

/// Write an mhip (playlist item) chunk. Returns serialized bytes.
fn write_mhip(track_id: u32) -> Vec<u8> {
    let header_size: u32 = 76;
    let total_size: u32 = 76;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhip").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // num_mhods
    buf.write_u32::<LittleEndian>(0).unwrap(); // podcast_grouping
    buf.write_u32::<LittleEndian>(0).unwrap(); // group_id
    buf.write_u32::<LittleEndian>(track_id).unwrap();

    // Pad to header_size.
    let written = buf.len();
    for _ in 0..(header_size as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf
}

/// Sort type constants for mhod type 52 (sort index).
/// Each corresponds to a browse category on the iPod.
/// Mapping from libgpod: 3=title, 4=album, 5=artist, 7=genre, 18=composer.
const SORT_TYPES: &[u32] = &[3, 4, 5, 7, 18];

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
/// Sort type mapping from libgpod: 3=title, 4=album, 5=artist, 7=genre, 18=composer.
fn sort_key(track: &IpodTrack, sort_type: u32) -> String {
    match sort_type {
        3 => track.title.to_lowercase(),
        4 => track.album.to_lowercase(),
        5 => track.artist.to_lowercase(),
        7 => track.genre.as_deref().unwrap_or("").to_lowercase(),
        18 => String::new(), // composer — not tracked yet
        _ => String::new(),
    }
}

/// Write an mhyp (playlist) chunk with its children. Returns serialized bytes.
fn write_mhyp(
    playlist: &crate::IpodPlaylist,
    dbid_to_track_id: &std::collections::HashMap<u64, u32>,
    playlist_id: u64,
    tracks: &[IpodTrack],
) -> Vec<u8> {
    let header_size: u32 = 184; // match iPod Classic header size

    // Build child mhods (playlist name).
    let mut mhods = Vec::new();
    let mut num_mhods: u32 = 0;

    if !playlist.name.is_empty() {
        mhods.extend(write_mhod(1, &playlist.name));
        num_mhods += 1;
    }

    // Master playlist gets sort indexes for all browse categories.
    // These tell the firmware how to order tracks in the browse UI.
    // The iPod will show "no music" without them when our tracks dataset
    // is also present (the firmware doesn't rebuild them on its own).
    if playlist.is_master && !tracks.is_empty() {
        for &sort_type in SORT_TYPES {
            mhods.extend(write_sort_index(tracks, sort_type));
            num_mhods += 1;
        }
    }

    // Build mhips.
    let mut mhips = Vec::new();
    let mut num_mhips: u32 = 0;
    for &dbid in &playlist.track_ids {
        if let Some(&tid) = dbid_to_track_id.get(&dbid) {
            mhips.extend(write_mhip(tid));
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

    // Build track dataset (mhsd type 1 = mhlt + mhits).
    let mut track_data = Vec::new();
    for track in &db.tracks {
        let art_count = art_counts.get(&track.dbid).copied().unwrap_or(0);
        track_data.extend(write_mhit(track, art_count));
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

    // Type 4 = album list (mhla with zero entries).
    let mhla_header_size: u32 = 92;
    let mut mhla = Vec::new();
    mhla.write_all(b"mhla").unwrap();
    mhla.write_u32::<LittleEndian>(mhla_header_size).unwrap();
    mhla.write_u32::<LittleEndian>(0).unwrap(); // 0 album entries
    let written = mhla.len();
    for _ in 0..(mhla_header_size as usize - written) {
        mhla.write_u8(0).unwrap();
    }

    let mhsd4_header_size: u32 = 96;
    let mhsd4_total_size = mhsd4_header_size + mhla.len() as u32;
    let mut mhsd4 = Vec::new();
    mhsd4.write_all(b"mhsd").unwrap();
    mhsd4.write_u32::<LittleEndian>(mhsd4_header_size).unwrap();
    mhsd4.write_u32::<LittleEndian>(mhsd4_total_size).unwrap();
    mhsd4.write_u32::<LittleEndian>(4).unwrap(); // type = album list
    let written = mhsd4.len();
    for _ in 0..(mhsd4_header_size as usize - written) {
        mhsd4.write_u8(0).unwrap();
    }
    mhsd4.extend(mhla);

    // Type 5 = smart playlists (empty mhlp).
    let mut mhlp5 = Vec::new();
    mhlp5.write_all(b"mhlp").unwrap();
    mhlp5.write_u32::<LittleEndian>(mhla_header_size).unwrap();
    mhlp5.write_u32::<LittleEndian>(0).unwrap(); // 0 smart playlists
    let written = mhlp5.len();
    for _ in 0..(mhla_header_size as usize - written) {
        mhlp5.write_u8(0).unwrap();
    }

    let mhsd5_header_size: u32 = 96;
    let mhsd5_total_size = mhsd5_header_size + mhlp5.len() as u32;
    let mut mhsd5 = Vec::new();
    mhsd5.write_all(b"mhsd").unwrap();
    mhsd5.write_u32::<LittleEndian>(mhsd5_header_size).unwrap();
    mhsd5.write_u32::<LittleEndian>(mhsd5_total_size).unwrap();
    mhsd5.write_u32::<LittleEndian>(5).unwrap(); // type = smart playlists
    let written = mhsd5.len();
    for _ in 0..(mhsd5_header_size as usize - written) {
        mhsd5.write_u8(0).unwrap();
    }
    mhsd5.extend(mhlp5);

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

    let mut result = Vec::with_capacity(mhbd_total_size as usize);
    result.write_all(b"mhbd").unwrap();
    result.write_u32::<LittleEndian>(mhbd_header_size).unwrap();
    result.write_u32::<LittleEndian>(mhbd_total_size).unwrap();
    result.write_u32::<LittleEndian>(1).unwrap(); // +12 db_type (1 = iTunesDB)
    result.write_u32::<LittleEndian>(db.db_version).unwrap(); // +16
    result.write_u32::<LittleEndian>(num_datasets).unwrap(); // +20
    result.write_u64::<LittleEndian>(0).unwrap(); // +24 db_id
    result.write_u16::<LittleEndian>(0).unwrap(); // +32 platform
    result.write_u16::<LittleEndian>(0).unwrap(); // +34 unk_0x22
    result.write_u64::<LittleEndian>(0).unwrap(); // +36 id_0x24
    result.write_u32::<LittleEndian>(0).unwrap(); // +44 unk_0x2c
                                                  // +48 hashing_scheme (set by sign_hash58, leave 0 for unsigned)
                                                  // Remaining fields (+48 through +244) are zero-padded.
                                                  // hash58 at +0x58 will be written by sign_hash58() after serialization.

    let written = result.len();
    for _ in 0..(mhbd_header_size as usize - written) {
        result.write_u8(0).unwrap();
    }

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

    let mut data = serialize(db);

    if let Some(fwid) = firewire_id {
        crate::hash::sign_hash58(&mut data, fwid)?;
    }

    // Write to temp file, then rename for atomicity.
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

    // Write ArtworkDB and .ithmb files if artwork is present.
    if let Some(ref store) = db.artwork_store {
        crate::artwork::ithmb::write_ithmb_files(&db.mount_point, &store.ithmb_files)?;
        crate::artwork::artworkdb::write_to_disk(&db.mount_point, store)?;
    }

    Ok(())
}
