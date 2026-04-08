use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;

use crate::{IpodDatabase, IpodDbError, IpodTrack};

/// Encode a Rust string as UTF-16LE bytes.
fn encode_utf16le(s: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        buf.write_u16::<LittleEndian>(unit).unwrap();
    }
    buf
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

/// Write an mhit chunk with its child mhods. Returns the serialized bytes.
fn write_mhit(track: &IpodTrack) -> Vec<u8> {
    // We use a fixed header size of 0x148 (328) which is compatible with most iPod models.
    let header_size: u32 = 0x148;

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
    buf.write_all(b"mhit").unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(num_mhods).unwrap();
    buf.write_u32::<LittleEndian>(track.track_id).unwrap(); // track_id
    buf.write_u32::<LittleEndian>(1).unwrap(); // visible
    buf.write_u32::<LittleEndian>(track.filetype).unwrap();
    buf.write_u8(0).unwrap(); // type (audio)
    buf.write_u8(0).unwrap(); // compilation
    buf.write_u8(0).unwrap(); // rating
    buf.write_u8(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // date_modified
    buf.write_u32::<LittleEndian>(track.file_size).unwrap();
    buf.write_u32::<LittleEndian>(track.total_time_ms.unwrap_or(0))
        .unwrap();
    buf.write_u32::<LittleEndian>(track.track_number.unwrap_or(0) as u32)
        .unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // total_tracks
    buf.write_u32::<LittleEndian>(track.year.unwrap_or(0) as u32)
        .unwrap();
    buf.write_u32::<LittleEndian>(track.bitrate.unwrap_or(0) as u32)
        .unwrap();
    // Sample rate: upper 16 bits = Hz, lower 16 = 0.
    let sr = (track.sample_rate.unwrap_or(0) as u32) << 16;
    buf.write_u32::<LittleEndian>(sr).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // volume_adjust
    buf.write_u32::<LittleEndian>(0).unwrap(); // start_time
    buf.write_u32::<LittleEndian>(0).unwrap(); // stop_time

    // Pad with zeros: we've written 76 bytes so far, need to reach offset 80.
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding to offset 80

    buf.write_u32::<LittleEndian>(0).unwrap(); // play_count
    buf.write_u32::<LittleEndian>(0).unwrap(); // last_played
    buf.write_u32::<LittleEndian>(track.disc_number.unwrap_or(0) as u32)
        .unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // total_discs

    // Pad from offset 96 to offset 112 (16 bytes = 4 u32s).
    for _ in 0..4 {
        buf.write_u32::<LittleEndian>(0).unwrap();
    }

    // dbid (persistent ID) at offset 112.
    buf.write_u64::<LittleEndian>(track.dbid).unwrap();

    // Pad rest to header_size. We've written 120 bytes.
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

/// Write an mhyp (playlist) chunk with its children. Returns serialized bytes.
fn write_mhyp(
    playlist: &crate::IpodPlaylist,
    dbid_to_track_id: &std::collections::HashMap<u64, u32>,
    playlist_id: u64,
) -> Vec<u8> {
    let header_size: u32 = 108;

    // Build child mhods (playlist name).
    let mut mhods = Vec::new();
    let mut num_mhods: u32 = 0;

    if !playlist.name.is_empty() {
        mhods.extend(write_mhod(1, &playlist.name));
        num_mhods += 1;
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

    // Build track dataset (mhsd type 1 = mhlt + mhits).
    let mut track_data = Vec::new();
    for track in &db.tracks {
        track_data.extend(write_mhit(track));
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
        playlist_data.extend(write_mhyp(pl, &dbid_to_track_id, (i + 1) as u64));
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

    let mhsd2_header_size: u32 = 96;
    let mhsd2_total_size = mhsd2_header_size + mhlp.len() as u32;
    let mut mhsd2 = Vec::new();
    mhsd2.write_all(b"mhsd").unwrap();
    mhsd2.write_u32::<LittleEndian>(mhsd2_header_size).unwrap();
    mhsd2.write_u32::<LittleEndian>(mhsd2_total_size).unwrap();
    mhsd2.write_u32::<LittleEndian>(2).unwrap(); // type = playlists
    let written = mhsd2.len();
    for _ in 0..(mhsd2_header_size as usize - written) {
        mhsd2.write_u8(0).unwrap();
    }
    mhsd2.extend(mhlp);

    // mhbd header.
    let mhbd_header_size: u32 = 104;
    let num_datasets: u32 = 2;
    let mhbd_total_size = mhbd_header_size + mhsd1.len() as u32 + mhsd2.len() as u32;

    let mut result = Vec::with_capacity(mhbd_total_size as usize);
    result.write_all(b"mhbd").unwrap();
    result.write_u32::<LittleEndian>(mhbd_header_size).unwrap();
    result.write_u32::<LittleEndian>(mhbd_total_size).unwrap();
    result.write_u32::<LittleEndian>(1).unwrap(); // db_type (1 = iTunesDB)
    result.write_u32::<LittleEndian>(db.db_version).unwrap();
    result.write_u32::<LittleEndian>(num_datasets).unwrap();
    result.write_u64::<LittleEndian>(0).unwrap(); // db_id

    // Pad to header_size.
    let written = result.len();
    for _ in 0..(mhbd_header_size as usize - written) {
        result.write_u8(0).unwrap();
    }

    result.extend(mhsd1);
    result.extend(mhsd2);
    result
}

/// Write the database to disk with atomic temp+rename and .bak backup.
pub fn write_to_disk(db: &IpodDatabase) -> crate::Result<()> {
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

    let data = serialize(db);

    // Write to temp file, then rename for atomicity.
    let tmp_path = db_path.with_extension("tmp");
    std::fs::write(&tmp_path, &data)?;
    std::fs::rename(&tmp_path, &db_path).map_err(|e| {
        IpodDbError::Filesystem(format!("failed to atomically replace iTunesDB: {e}"))
    })?;

    Ok(())
}
