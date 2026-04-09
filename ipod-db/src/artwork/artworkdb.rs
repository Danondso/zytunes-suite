//! ArtworkDB binary serialization.
//!
//! The ArtworkDB lives at `iPod_Control/Artwork/ArtworkDB` and uses the same
//! length-prefixed chunk format as the iTunesDB, with a different root magic
//! (`mhfd` instead of `mhbd`).
//!
//! ```text
//! mhfd (root, header=132)
//! ├── mhsd type=1 (header=96) — image list
//! │   └── mhli (header=92)
//! │       └── mhii (header=152) ×N — one per track with artwork
//! │           └── mhni (header=76) ×M — one per thumbnail size
//! │               └── mhod type=3 — ithmb filename string
//! └── mhsd type=2 (header=96) — file list
//!     └── mhlf (header=92)
//!         └── mhif (header=124) ×K — one per .ithmb file
//! ```

use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;
use std::path::Path;

use super::{ArtworkStore, ItmbFileState, ThumbnailEntry, TrackArtwork};
use crate::encoding::encode_utf16le;
use crate::IpodDbError;

// Header sizes from libgpod db-artwork-writer.c get_padded_header_size().
const MHFD_HEADER_SIZE: u32 = 132;
const MHSD_HEADER_SIZE: u32 = 96;
const MHLI_HEADER_SIZE: u32 = 92;
const MHII_HEADER_SIZE: u32 = 152;
const MHNI_HEADER_SIZE: u32 = 76;
const MHLF_HEADER_SIZE: u32 = 92;
const MHIF_HEADER_SIZE: u32 = 124;
const MHOD_HEADER_SIZE: u32 = 24;

/// Write an mhod type 3 (filename string) for artwork.
fn write_mhod_filename(filename: &str) -> Vec<u8> {
    let string_bytes = encode_utf16le(filename);
    let string_header_size: u32 = 16;
    let total_size = MHOD_HEADER_SIZE + string_header_size + string_bytes.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhod").unwrap();
    buf.write_u32::<LittleEndian>(MHOD_HEADER_SIZE).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(3).unwrap(); // type = filename
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

/// Write an mhni (image name/info) chunk.
fn write_mhni(thumb: &ThumbnailEntry) -> Vec<u8> {
    let ithmb_path = format!(":F{}_{}.ithmb", thumb.correlation_id, 1);
    let mhod = write_mhod_filename(&ithmb_path);
    let num_children: u32 = 1;
    let total_size = MHNI_HEADER_SIZE + mhod.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhni").unwrap(); // +0
    buf.write_u32::<LittleEndian>(MHNI_HEADER_SIZE).unwrap(); // +4
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +8
    buf.write_u32::<LittleEndian>(num_children).unwrap(); // +12
    buf.write_u32::<LittleEndian>(thumb.correlation_id).unwrap(); // +16
    buf.write_u32::<LittleEndian>(thumb.image_offset).unwrap(); // +20
    buf.write_u32::<LittleEndian>(thumb.image_size).unwrap(); // +24
    buf.write_u16::<LittleEndian>(0).unwrap(); // +28 vertical_padding
    buf.write_u16::<LittleEndian>(0).unwrap(); // +30 horizontal_padding
    buf.write_u16::<LittleEndian>(thumb.height).unwrap(); // +32
    buf.write_u16::<LittleEndian>(thumb.width).unwrap(); // +34

    // Pad to header size.
    let written = buf.len();
    for _ in 0..(MHNI_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&mhod).unwrap();
    buf
}

/// Write an mhii (image item) chunk for one track's artwork.
fn write_mhii(artwork: &TrackArtwork) -> Vec<u8> {
    let mut children = Vec::new();
    for thumb in &artwork.thumbnails {
        children.extend(write_mhni(thumb));
    }

    let num_children = artwork.thumbnails.len() as u32;
    let total_size = MHII_HEADER_SIZE + children.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhii").unwrap(); // +0
    buf.write_u32::<LittleEndian>(MHII_HEADER_SIZE).unwrap(); // +4
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +8
    buf.write_u32::<LittleEndian>(num_children).unwrap(); // +12
    buf.write_u32::<LittleEndian>(0).unwrap(); // +16 image_id (auto)
    buf.write_u64::<LittleEndian>(artwork.dbid).unwrap(); // +20 song_id / dbid
    buf.write_u32::<LittleEndian>(0).unwrap(); // +28 unknown
    buf.write_u32::<LittleEndian>(0).unwrap(); // +32 rating
    buf.write_u32::<LittleEndian>(0).unwrap(); // +36 unknown
    buf.write_u32::<LittleEndian>(0).unwrap(); // +40 original_date

    // Pad to header size.
    let written = buf.len();
    for _ in 0..(MHII_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&children).unwrap();
    buf
}

/// Write an mhif (image file info) chunk.
fn write_mhif(ithmb: &ItmbFileState) -> Vec<u8> {
    let mut buf = Vec::with_capacity(MHIF_HEADER_SIZE as usize);
    buf.write_all(b"mhif").unwrap(); // +0
    buf.write_u32::<LittleEndian>(MHIF_HEADER_SIZE).unwrap(); // +4
    buf.write_u32::<LittleEndian>(MHIF_HEADER_SIZE).unwrap(); // +8 total_size (no children)
    buf.write_u32::<LittleEndian>(ithmb.correlation_id).unwrap(); // +12
    buf.write_u32::<LittleEndian>(ithmb.image_size).unwrap(); // +16 image_size (per entry)

    // Pad to header size.
    let written = buf.len();
    for _ in 0..(MHIF_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf
}

/// Serialize the full ArtworkDB to binary.
pub fn serialize(store: &ArtworkStore) -> Vec<u8> {
    // Build image list: mhli + mhii entries.
    let mut mhii_data = Vec::new();
    for artwork in &store.track_artworks {
        mhii_data.extend(write_mhii(artwork));
    }

    let mut mhli = Vec::new();
    mhli.write_all(b"mhli").unwrap();
    mhli.write_u32::<LittleEndian>(MHLI_HEADER_SIZE).unwrap();
    mhli.write_u32::<LittleEndian>(store.track_artworks.len() as u32)
        .unwrap();
    let written = mhli.len();
    for _ in 0..(MHLI_HEADER_SIZE as usize - written) {
        mhli.write_u8(0).unwrap();
    }
    mhli.extend(mhii_data);

    // mhsd type 1 (image list).
    let mhsd1_total = MHSD_HEADER_SIZE + mhli.len() as u32;
    let mut mhsd1 = Vec::new();
    mhsd1.write_all(b"mhsd").unwrap();
    mhsd1.write_u32::<LittleEndian>(MHSD_HEADER_SIZE).unwrap();
    mhsd1.write_u32::<LittleEndian>(mhsd1_total).unwrap();
    mhsd1.write_u32::<LittleEndian>(1).unwrap(); // type = image list
    let written = mhsd1.len();
    for _ in 0..(MHSD_HEADER_SIZE as usize - written) {
        mhsd1.write_u8(0).unwrap();
    }
    mhsd1.extend(mhli);

    // Build file list: mhlf + mhif entries.
    let mut mhif_data = Vec::new();
    for ithmb in &store.ithmb_files {
        mhif_data.extend(write_mhif(ithmb));
    }

    let mut mhlf = Vec::new();
    mhlf.write_all(b"mhlf").unwrap();
    mhlf.write_u32::<LittleEndian>(MHLF_HEADER_SIZE).unwrap();
    mhlf.write_u32::<LittleEndian>(store.ithmb_files.len() as u32)
        .unwrap();
    let written = mhlf.len();
    for _ in 0..(MHLF_HEADER_SIZE as usize - written) {
        mhlf.write_u8(0).unwrap();
    }
    mhlf.extend(mhif_data);

    // mhsd type 2 (file list).
    let mhsd2_total = MHSD_HEADER_SIZE + mhlf.len() as u32;
    let mut mhsd2 = Vec::new();
    mhsd2.write_all(b"mhsd").unwrap();
    mhsd2.write_u32::<LittleEndian>(MHSD_HEADER_SIZE).unwrap();
    mhsd2.write_u32::<LittleEndian>(mhsd2_total).unwrap();
    mhsd2.write_u32::<LittleEndian>(2).unwrap(); // type = file list
    let written = mhsd2.len();
    for _ in 0..(MHSD_HEADER_SIZE as usize - written) {
        mhsd2.write_u8(0).unwrap();
    }
    mhsd2.extend(mhlf);

    // mhfd root header.
    let num_datasets: u32 = 2;
    let mhfd_total = MHFD_HEADER_SIZE + mhsd1.len() as u32 + mhsd2.len() as u32;

    let mut result = Vec::with_capacity(mhfd_total as usize);
    result.write_all(b"mhfd").unwrap(); // +0
    result.write_u32::<LittleEndian>(MHFD_HEADER_SIZE).unwrap(); // +4
    result.write_u32::<LittleEndian>(mhfd_total).unwrap(); // +8
    result.write_u32::<LittleEndian>(2).unwrap(); // +12 db_type (2 = ArtworkDB)
    result.write_u32::<LittleEndian>(0).unwrap(); // +16 unknown
    result.write_u32::<LittleEndian>(num_datasets).unwrap(); // +20

    // Pad to header size.
    let written = result.len();
    for _ in 0..(MHFD_HEADER_SIZE as usize - written) {
        result.write_u8(0).unwrap();
    }

    result.extend(mhsd1);
    result.extend(mhsd2);
    result
}

/// Write the ArtworkDB to disk with atomic temp+rename and .bak backup.
pub fn write_to_disk(mount_point: &Path, store: &ArtworkStore) -> crate::Result<()> {
    let db_path = ArtworkStore::db_path(mount_point);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if db_path.exists() {
        let backup = db_path.with_extension("bak");
        std::fs::copy(&db_path, &backup)
            .map_err(|e| IpodDbError::Filesystem(format!("failed to backup ArtworkDB: {e}")))?;
    }

    let data = serialize(store);

    let tmp_path = db_path.with_extension("tmp");
    std::fs::write(&tmp_path, &data)?;
    let rename_result = std::fs::rename(&tmp_path, &db_path);
    if rename_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    rename_result
        .map_err(|e| IpodDbError::Filesystem(format!("failed to replace ArtworkDB: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::{model_specs_video, ArtworkStore, ThumbnailEntry, TrackArtwork};

    #[test]
    fn test_serialize_empty() {
        let store = ArtworkStore::new(model_specs_video());
        let data = serialize(&store);

        // mhfd header
        assert_eq!(&data[0..4], b"mhfd");
        let header_size = u32::from_le_bytes(data[4..8].try_into().unwrap());
        assert_eq!(header_size, MHFD_HEADER_SIZE);
        let db_type = u32::from_le_bytes(data[12..16].try_into().unwrap());
        assert_eq!(db_type, 2);
        let num_datasets = u32::from_le_bytes(data[20..24].try_into().unwrap());
        assert_eq!(num_datasets, 2);

        // mhsd type 1 follows mhfd header
        let mhsd1_start = MHFD_HEADER_SIZE as usize;
        assert_eq!(&data[mhsd1_start..mhsd1_start + 4], b"mhsd");
        let mhsd1_type =
            u32::from_le_bytes(data[mhsd1_start + 12..mhsd1_start + 16].try_into().unwrap());
        assert_eq!(mhsd1_type, 1);

        // mhli inside mhsd1
        let mhli_start = mhsd1_start + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhli_start..mhli_start + 4], b"mhli");
        let mhli_count =
            u32::from_le_bytes(data[mhli_start + 8..mhli_start + 12].try_into().unwrap());
        assert_eq!(mhli_count, 0); // no images

        // mhsd type 2 follows mhsd1
        let mhsd1_total =
            u32::from_le_bytes(data[mhsd1_start + 8..mhsd1_start + 12].try_into().unwrap());
        let mhsd2_start = mhsd1_start + mhsd1_total as usize;
        assert_eq!(&data[mhsd2_start..mhsd2_start + 4], b"mhsd");
        let mhsd2_type =
            u32::from_le_bytes(data[mhsd2_start + 12..mhsd2_start + 16].try_into().unwrap());
        assert_eq!(mhsd2_type, 2);

        // mhlf inside mhsd2 has 2 entries (one per spec)
        let mhlf_start = mhsd2_start + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhlf_start..mhlf_start + 4], b"mhlf");
        let mhlf_count =
            u32::from_le_bytes(data[mhlf_start + 8..mhlf_start + 12].try_into().unwrap());
        assert_eq!(mhlf_count, 2);
    }

    #[test]
    fn test_serialize_one_track() {
        let mut store = ArtworkStore::new(model_specs_video());
        store.track_artworks.push(TrackArtwork {
            dbid: 42,
            thumbnails: vec![
                ThumbnailEntry {
                    correlation_id: 1028,
                    image_offset: 0,
                    image_size: 20000,
                    width: 100,
                    height: 100,
                },
                ThumbnailEntry {
                    correlation_id: 1029,
                    image_offset: 0,
                    image_size: 80000,
                    width: 200,
                    height: 200,
                },
            ],
        });

        let data = serialize(&store);

        // Find mhli and verify 1 child.
        let mhli_start = MHFD_HEADER_SIZE as usize + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhli_start..mhli_start + 4], b"mhli");
        let count = u32::from_le_bytes(data[mhli_start + 8..mhli_start + 12].try_into().unwrap());
        assert_eq!(count, 1);

        // Find mhii inside mhli.
        let mhii_start = mhli_start + MHLI_HEADER_SIZE as usize;
        assert_eq!(&data[mhii_start..mhii_start + 4], b"mhii");
        let mhii_children =
            u32::from_le_bytes(data[mhii_start + 12..mhii_start + 16].try_into().unwrap());
        assert_eq!(mhii_children, 2); // two thumbnail sizes

        // Verify dbid at +20.
        let dbid = u64::from_le_bytes(data[mhii_start + 20..mhii_start + 28].try_into().unwrap());
        assert_eq!(dbid, 42);
    }

    #[test]
    fn test_mhni_fields() {
        let thumb = ThumbnailEntry {
            correlation_id: 1028,
            image_offset: 20000,
            image_size: 20000,
            width: 100,
            height: 100,
        };
        let data = write_mhni(&thumb);

        assert_eq!(&data[0..4], b"mhni");
        let header_size = u32::from_le_bytes(data[4..8].try_into().unwrap());
        assert_eq!(header_size, MHNI_HEADER_SIZE);
        let corr_id = u32::from_le_bytes(data[16..20].try_into().unwrap());
        assert_eq!(corr_id, 1028);
        let offset = u32::from_le_bytes(data[20..24].try_into().unwrap());
        assert_eq!(offset, 20000);
        let size = u32::from_le_bytes(data[24..28].try_into().unwrap());
        assert_eq!(size, 20000);
        let height = u16::from_le_bytes(data[32..34].try_into().unwrap());
        assert_eq!(height, 100);
        let width = u16::from_le_bytes(data[34..36].try_into().unwrap());
        assert_eq!(width, 100);

        // Child mhod should start at header boundary.
        assert_eq!(
            &data[MHNI_HEADER_SIZE as usize..MHNI_HEADER_SIZE as usize + 4],
            b"mhod"
        );
    }

    #[test]
    fn test_mhif_fields() {
        let ithmb = ItmbFileState {
            correlation_id: 1029,
            filename: "F1029_1.ithmb".into(),
            image_size: 80000,
            current_offset: 160000,
            data: Vec::new(),
        };
        let data = write_mhif(&ithmb);

        assert_eq!(&data[0..4], b"mhif");
        assert_eq!(data.len(), MHIF_HEADER_SIZE as usize);
        let corr_id = u32::from_le_bytes(data[12..16].try_into().unwrap());
        assert_eq!(corr_id, 1029);
        let img_size = u32::from_le_bytes(data[16..20].try_into().unwrap());
        assert_eq!(img_size, 80000);
    }

    #[test]
    fn test_write_to_disk_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path();

        let store = ArtworkStore::new(model_specs_video());
        write_to_disk(mount, &store).unwrap();

        let db_path = mount.join("iPod_Control/Artwork/ArtworkDB");
        assert!(db_path.exists());

        // Write again — should create .bak.
        write_to_disk(mount, &store).unwrap();
        assert!(mount.join("iPod_Control/Artwork/ArtworkDB.bak").exists());
    }
}
