//! ArtworkDB binary serialization.
//!
//! The ArtworkDB lives at `iPod_Control/Artwork/ArtworkDB` and uses the same
//! length-prefixed chunk format as the iTunesDB, with a different root magic
//! (`mhfd` instead of `mhbd`).
//!
//! Structure matches what iTunes writes on iPod Classic (verified by byte-diff
//! 2026-04-15 against a reference ArtworkDB pulled from a Classic):
//!
//! ```text
//! mhfd (root, header=132, db_type=0, num_datasets=3)
//! ├── mhsd type=1 (header=96) — image list
//! │   └── mhli (header=92)
//! │       └── mhii (header=152) ×N — one per track with artwork
//! │           ├── mhod type=2 (header=24) ×M — one per thumbnail size
//! │           │   └── mhni (header=76)
//! │           │       └── mhod type=3 — ithmb filename string
//! │           └── mhod type=6 (header=24) — fullsize metadata
//! │               └── mhaf (96 bytes, constant)
//! ├── mhsd type=2 (header=96) — album artwork list
//! │   └── mhla (header=92, count=0 — empty but required)
//! └── mhsd type=3 (header=96) — file list
//!     └── mhlf (header=92)
//!         └── mhif (header=124) ×K — one per .ithmb file
//! ```

use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;
use std::path::Path;

use super::{ArtworkStore, ItmbFileState, ThumbnailEntry, TrackArtwork};
use crate::encoding::encode_utf16le;
use crate::IpodDbError;

// Header sizes from libgpod db-artwork-writer.c get_padded_header_size(),
// cross-checked against iTunes-written reference.
const MHFD_HEADER_SIZE: u32 = 132;
const MHSD_HEADER_SIZE: u32 = 96;
const MHLI_HEADER_SIZE: u32 = 92;
const MHLA_HEADER_SIZE: u32 = 92;
const MHLF_HEADER_SIZE: u32 = 92;
const MHII_HEADER_SIZE: u32 = 152;
const MHNI_HEADER_SIZE: u32 = 76;
const MHIF_HEADER_SIZE: u32 = 124;
const MHOD_HEADER_SIZE: u32 = 24;
const MHAF_PAYLOAD_SIZE: u32 = 96;

/// First mhii.image_id. iTunes starts at 0x64 on iPod Classic (not 0x40 as
/// libgpod documents); subsequent records increment by 1.
const MHII_IMAGE_ID_START: u32 = 0x64;

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

/// Wrap an arbitrary payload in a 24-byte mhod container of the given type.
/// Used for mhii child containers: type=2 wraps an mhni, type=6 wraps an mhaf.
fn write_mhod_container(mhod_type: u32, payload: &[u8]) -> Vec<u8> {
    let total_size = MHOD_HEADER_SIZE + payload.len() as u32;
    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhod").unwrap();
    buf.write_u32::<LittleEndian>(MHOD_HEADER_SIZE).unwrap();
    buf.write_u32::<LittleEndian>(total_size).unwrap();
    buf.write_u32::<LittleEndian>(mhod_type).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_u32::<LittleEndian>(0).unwrap(); // padding
    buf.write_all(payload).unwrap();
    buf
}

/// Fullsize-image metadata payload that goes inside each mhii's mhod(type=6)
/// child. The iTunes-written reference uses an identical 96-byte blob for
/// every record, so we emit the same constant shape. Fields at +4/+8 mirror
/// the reference (0x60 and 0x3c); the rest is zero.
fn mhaf_payload() -> Vec<u8> {
    let mut buf = vec![0u8; MHAF_PAYLOAD_SIZE as usize];
    buf[0..4].copy_from_slice(b"mhaf");
    buf[4..8].copy_from_slice(&96u32.to_le_bytes());
    buf[8..12].copy_from_slice(&60u32.to_le_bytes());
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
///
/// Each thumbnail mhni is wrapped in an mhod(type=2) container, and a final
/// mhod(type=6) carrying an mhaf fullsize-metadata blob is appended. This
/// structure is what iTunes writes and what iPod Classic firmware expects —
/// mhni as a direct child of mhii does not render.
fn write_mhii(artwork: &TrackArtwork, image_id: u32) -> Vec<u8> {
    let mut children = Vec::new();
    for thumb in &artwork.thumbnails {
        let mhni = write_mhni(thumb);
        children.extend(write_mhod_container(2, &mhni));
    }
    // Required fullsize-metadata child.
    children.extend(write_mhod_container(6, &mhaf_payload()));

    let num_children = artwork.thumbnails.len() as u32 + 1;
    let total_size = MHII_HEADER_SIZE + children.len() as u32;

    let mut buf = Vec::with_capacity(total_size as usize);
    buf.write_all(b"mhii").unwrap(); // +0
    buf.write_u32::<LittleEndian>(MHII_HEADER_SIZE).unwrap(); // +4
    buf.write_u32::<LittleEndian>(total_size).unwrap(); // +8
    buf.write_u32::<LittleEndian>(num_children).unwrap(); // +12
    buf.write_u32::<LittleEndian>(image_id).unwrap(); // +16 image_id (unique per record)
    buf.write_u64::<LittleEndian>(artwork.dbid).unwrap(); // +20 song_id / dbid

    // Remaining 152-byte header fields (+0x1C rating, +0x28 dates,
    // +0x30 source_image_size, etc.) are populated in a later pass — tier 1
    // focuses on structural correctness.
    let written = buf.len();
    for _ in 0..(MHII_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf.write_all(&children).unwrap();
    buf
}

/// Write an mhif (image file info) chunk.
///
/// Field layout matches the iTunes reference:
///   +0x0C padding (0)
///   +0x10 correlation_id
///   +0x14 image_size (per-entry byte count)
fn write_mhif(ithmb: &ItmbFileState) -> Vec<u8> {
    let mut buf = Vec::with_capacity(MHIF_HEADER_SIZE as usize);
    buf.write_all(b"mhif").unwrap(); // +0x00
    buf.write_u32::<LittleEndian>(MHIF_HEADER_SIZE).unwrap(); // +0x04
    buf.write_u32::<LittleEndian>(MHIF_HEADER_SIZE).unwrap(); // +0x08 total_size (no children)
    buf.write_u32::<LittleEndian>(0).unwrap(); // +0x0C padding
    buf.write_u32::<LittleEndian>(ithmb.correlation_id).unwrap(); // +0x10
    buf.write_u32::<LittleEndian>(ithmb.image_size).unwrap(); // +0x14 image_size (per entry)

    // Pad to header size.
    let written = buf.len();
    for _ in 0..(MHIF_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }

    buf
}

/// Wrap a dataset payload (mhli / mhla / mhlf) in an mhsd header of the given
/// type. mhsd types on iPod Classic: 1=image list, 2=album artwork list,
/// 3=file list.
fn wrap_mhsd(mhsd_type: u32, payload: &[u8]) -> Vec<u8> {
    let total = MHSD_HEADER_SIZE + payload.len() as u32;
    let mut buf = Vec::with_capacity(total as usize);
    buf.write_all(b"mhsd").unwrap();
    buf.write_u32::<LittleEndian>(MHSD_HEADER_SIZE).unwrap();
    buf.write_u32::<LittleEndian>(total).unwrap();
    buf.write_u32::<LittleEndian>(mhsd_type).unwrap();
    let written = buf.len();
    for _ in 0..(MHSD_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }
    buf.extend_from_slice(payload);
    buf
}

/// Build an empty mhla (album-artwork list). The iTunes reference always
/// includes this dataset even with zero entries; omitting it prevents the
/// firmware from walking past the image list.
fn build_empty_mhla() -> Vec<u8> {
    let mut buf = Vec::with_capacity(MHLA_HEADER_SIZE as usize);
    buf.write_all(b"mhla").unwrap();
    buf.write_u32::<LittleEndian>(MHLA_HEADER_SIZE).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // count = 0
    let written = buf.len();
    for _ in 0..(MHLA_HEADER_SIZE as usize - written) {
        buf.write_u8(0).unwrap();
    }
    buf
}

/// Serialize the full ArtworkDB to binary.
pub fn serialize(store: &ArtworkStore) -> Vec<u8> {
    // Dataset 1: image list (mhli + mhii records).
    let mut mhii_data = Vec::new();
    for (i, artwork) in store.track_artworks.iter().enumerate() {
        let image_id = MHII_IMAGE_ID_START + i as u32;
        mhii_data.extend(write_mhii(artwork, image_id));
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
    let mhsd1 = wrap_mhsd(1, &mhli);

    // Dataset 2: album-artwork list (empty but required).
    let mhsd2 = wrap_mhsd(2, &build_empty_mhla());

    // Dataset 3: file list (mhlf + mhif records).
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
    let mhsd3 = wrap_mhsd(3, &mhlf);

    // mhfd root.
    let num_datasets: u32 = 3;
    let mhfd_total =
        MHFD_HEADER_SIZE + mhsd1.len() as u32 + mhsd2.len() as u32 + mhsd3.len() as u32;

    let mut result = Vec::with_capacity(mhfd_total as usize);
    result.write_all(b"mhfd").unwrap(); // +0x00
    result.write_u32::<LittleEndian>(MHFD_HEADER_SIZE).unwrap(); // +0x04
    result.write_u32::<LittleEndian>(mhfd_total).unwrap(); // +0x08
    result.write_u32::<LittleEndian>(0).unwrap(); // +0x0C db_type (iTunes writes 0 on Classic)
    result.write_u32::<LittleEndian>(0).unwrap(); // +0x10 unknown (tier 2)
    result.write_u32::<LittleEndian>(num_datasets).unwrap(); // +0x14

    let written = result.len();
    for _ in 0..(MHFD_HEADER_SIZE as usize - written) {
        result.write_u8(0).unwrap();
    }

    result.extend(mhsd1);
    result.extend(mhsd2);
    result.extend(mhsd3);
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

    /// Walk an ArtworkDB and return the offsets of each mhsd dataset along
    /// with their declared types. Used to verify root-level structure.
    fn find_mhsds(data: &[u8]) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        let mhfd_hs = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let mut cur = mhfd_hs;
        while cur + 16 <= data.len() && &data[cur..cur + 4] == b"mhsd" {
            let total = u32::from_le_bytes(data[cur + 8..cur + 12].try_into().unwrap()) as usize;
            let typ = u32::from_le_bytes(data[cur + 12..cur + 16].try_into().unwrap());
            out.push((cur, typ));
            cur += total;
        }
        out
    }

    #[test]
    fn test_serialize_empty_matches_reference_shape() {
        let store = ArtworkStore::new(model_specs_video());
        let data = serialize(&store);

        // mhfd header fields match the reference (db_type=0, num_datasets=3).
        assert_eq!(&data[0..4], b"mhfd");
        assert_eq!(
            u32::from_le_bytes(data[4..8].try_into().unwrap()),
            MHFD_HEADER_SIZE
        );
        assert_eq!(
            u32::from_le_bytes(data[12..16].try_into().unwrap()),
            0,
            "db_type must be 0 on Classic"
        );
        assert_eq!(
            u32::from_le_bytes(data[20..24].try_into().unwrap()),
            3,
            "num_datasets must be 3"
        );

        // Three mhsds with types 1, 2, 3 in order.
        let mhsds = find_mhsds(&data);
        assert_eq!(mhsds.len(), 3);
        assert_eq!(mhsds[0].1, 1, "first dataset must be image list (type 1)");
        assert_eq!(mhsds[1].1, 2, "middle dataset must be album list (type 2)");
        assert_eq!(mhsds[2].1, 3, "last dataset must be file list (type 3)");

        // Inner chunk magics.
        let mhli_start = mhsds[0].0 + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhli_start..mhli_start + 4], b"mhli");
        let mhla_start = mhsds[1].0 + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhla_start..mhla_start + 4], b"mhla");
        let mhlf_start = mhsds[2].0 + MHSD_HEADER_SIZE as usize;
        assert_eq!(&data[mhlf_start..mhlf_start + 4], b"mhlf");

        // mhla is empty but present.
        assert_eq!(
            u32::from_le_bytes(data[mhla_start + 8..mhla_start + 12].try_into().unwrap()),
            0
        );

        // mhlf has one mhif per spec.
        assert_eq!(
            u32::from_le_bytes(data[mhlf_start + 8..mhlf_start + 12].try_into().unwrap()),
            2
        );
    }

    #[test]
    fn test_mhii_children_are_wrapped_in_mhods() {
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

        // Locate the single mhii.
        let mhii_pos = data
            .windows(4)
            .position(|w| w == b"mhii")
            .expect("mhii missing");

        // num_children = 2 thumbnails + 1 mhaf = 3.
        let num_children =
            u32::from_le_bytes(data[mhii_pos + 12..mhii_pos + 16].try_into().unwrap());
        assert_eq!(num_children, 3);

        // First child must be mhod (not mhni directly).
        let first_child = mhii_pos + MHII_HEADER_SIZE as usize;
        assert_eq!(&data[first_child..first_child + 4], b"mhod");
        let first_type =
            u32::from_le_bytes(data[first_child + 12..first_child + 16].try_into().unwrap());
        assert_eq!(first_type, 2, "thumbnail wrapper must be mhod type=2");

        // Payload of the first mhod must begin with mhni.
        let mhod1_hs =
            u32::from_le_bytes(data[first_child + 4..first_child + 8].try_into().unwrap()) as usize;
        let mhni_start = first_child + mhod1_hs;
        assert_eq!(&data[mhni_start..mhni_start + 4], b"mhni");

        // Walk past both mhod(type=2) wrappers to find the mhod(type=6) + mhaf.
        let mut cur = first_child;
        let mut last_type = 0u32;
        let mut mhaf_found = false;
        for _ in 0..3 {
            let total = u32::from_le_bytes(data[cur + 8..cur + 12].try_into().unwrap()) as usize;
            let t = u32::from_le_bytes(data[cur + 12..cur + 16].try_into().unwrap());
            last_type = t;
            if t == 6 {
                let hs = u32::from_le_bytes(data[cur + 4..cur + 8].try_into().unwrap()) as usize;
                assert_eq!(&data[cur + hs..cur + hs + 4], b"mhaf");
                mhaf_found = true;
            }
            cur += total;
        }
        assert_eq!(last_type, 6, "final mhii child must be mhod type=6");
        assert!(mhaf_found, "mhod type=6 must wrap an mhaf payload");
    }

    #[test]
    fn test_mhii_image_ids_increment_from_0x64() {
        let png = tests::test_helpers::make_png();
        let mut store = ArtworkStore::new(model_specs_video());
        store.add_artwork(10, &png).unwrap();
        store.add_artwork(20, &png).unwrap();
        store.add_artwork(30, &png).unwrap();

        let data = serialize(&store);

        // Collect each mhii's image_id (+0x10) by walking the mhli.
        let mhli_pos = data
            .windows(4)
            .position(|w| w == b"mhli")
            .expect("mhli missing");
        let mut cur = mhli_pos + MHLI_HEADER_SIZE as usize;
        let mut ids = Vec::new();
        for _ in 0..3 {
            assert_eq!(&data[cur..cur + 4], b"mhii");
            let total = u32::from_le_bytes(data[cur + 8..cur + 12].try_into().unwrap()) as usize;
            let id = u32::from_le_bytes(data[cur + 16..cur + 20].try_into().unwrap());
            ids.push(id);
            cur += total;
        }
        assert_eq!(ids, vec![0x64, 0x65, 0x66]);
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
        // +0x0C must be 0 (padding).
        assert_eq!(u32::from_le_bytes(data[12..16].try_into().unwrap()), 0);
        // +0x10 correlation_id.
        let corr_id = u32::from_le_bytes(data[16..20].try_into().unwrap());
        assert_eq!(corr_id, 1029);
        // +0x14 image_size.
        let img_size = u32::from_le_bytes(data[20..24].try_into().unwrap());
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

        write_to_disk(mount, &store).unwrap();
        assert!(mount.join("iPod_Control/Artwork/ArtworkDB.bak").exists());
    }

    mod test_helpers {
        pub fn make_png() -> Vec<u8> {
            let mut img = image::RgbImage::new(4, 4);
            for pixel in img.pixels_mut() {
                *pixel = image::Rgb([0xFF, 0x00, 0x00]);
            }
            let mut png_bytes = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
            image::ImageEncoder::write_image(
                encoder,
                img.as_raw(),
                4,
                4,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
            png_bytes
        }
    }
}
