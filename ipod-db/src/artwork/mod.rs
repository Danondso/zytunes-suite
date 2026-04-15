//! ArtworkDB and ITHMB support for iPod album art.
//!
//! Classic iPods never read embedded ID3 album art — they only read from
//! `iPod_Control/Artwork/ArtworkDB` plus `.ithmb` raw pixel files. This module
//! handles converting source images (JPEG/PNG) to the iPod's RGB565 pixel format,
//! accumulating them into `.ithmb` files, and serializing the ArtworkDB binary.

pub mod artworkdb;
pub mod ithmb;

use std::collections::HashSet;
use std::path::PathBuf;

/// Pixel format for ITHMB thumbnail data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 16-bit RGB565 little-endian (2 bytes per pixel).
    Rgb565,
}

/// Describes one thumbnail size that an iPod model expects.
#[derive(Debug, Clone)]
pub struct ThumbnailSpec {
    /// Correlation ID linking mhni entries to mhif entries and .ithmb filenames.
    pub correlation_id: u32,
    /// Displayed pixel width (goes in the mhni width field).
    pub width: u16,
    /// Displayed pixel height (goes in the mhni height field).
    pub height: u16,
    /// Pixels per row in the ithmb storage. Equals `width` for square entries
    /// without padding. iTunes pads the Classic small thumbnail from a
    /// displayed 55×55 to a 56-pixel row stride, giving 6160-byte entries.
    pub row_stride_pixels: u16,
    /// Pixel format (always RGB565 for now).
    pub pixel_format: PixelFormat,
}

impl ThumbnailSpec {
    /// Bytes per image in the ithmb (row_stride_pixels * height * bpp).
    ///
    /// This is the per-entry allocation in the `.ithmb` file and the value
    /// written to mhni +0x18. When `row_stride_pixels > width` the extra
    /// pixels per row are zero padding; the firmware still treats the image
    /// as `width × height` for display.
    pub fn image_byte_size(&self) -> u32 {
        let bpp = match self.pixel_format {
            PixelFormat::Rgb565 => 2,
        };
        self.row_stride_pixels as u32 * self.height as u32 * bpp
    }
}

/// Tracks the accumulated state of one `.ithmb` file during a write session.
#[derive(Debug, Clone)]
pub struct ItmbFileState {
    /// Correlation ID (matches ThumbnailSpec and mhif entries).
    pub correlation_id: u32,
    /// Filename, e.g. `"F1028_1.ithmb"`.
    pub filename: String,
    /// Bytes per image slot (width * height * 2 for RGB565).
    pub image_size: u32,
    /// Next write offset in bytes.
    pub current_offset: u32,
    /// Accumulated raw pixel data for all images.
    pub data: Vec<u8>,
}

/// One thumbnail entry for a track, produced after conversion.
#[derive(Debug, Clone)]
pub struct ThumbnailEntry {
    /// Correlation ID (matches an mhif / ItmbFileState).
    pub correlation_id: u32,
    /// Byte offset within the `.ithmb` file where this image starts.
    pub image_offset: u32,
    /// Size in bytes of this image's pixel data.
    pub image_size: u32,
    /// Thumbnail width in pixels.
    pub width: u16,
    /// Thumbnail height in pixels.
    pub height: u16,
}

/// All thumbnail entries for one track.
#[derive(Debug, Clone)]
pub struct TrackArtwork {
    /// Track dbid (matches mhit dbid in iTunesDB).
    pub dbid: u64,
    /// Byte count of the original source image. Written to mhii +0x30 —
    /// iTunes populates this field and firmware appears to use it to validate
    /// that the artwork record corresponds to the on-disk source.
    pub source_image_bytes: u32,
    /// One entry per thumbnail size.
    pub thumbnails: Vec<ThumbnailEntry>,
}

/// In-memory artwork state for the entire database.
#[derive(Debug, Clone)]
pub struct ArtworkStore {
    /// Thumbnail size specifications for the target iPod model.
    pub specs: Vec<ThumbnailSpec>,
    /// Accumulated .ithmb file data, one per spec.
    pub ithmb_files: Vec<ItmbFileState>,
    /// Per-track artwork entries.
    pub track_artworks: Vec<TrackArtwork>,
}

impl ArtworkStore {
    /// Create a new artwork store for the given thumbnail specs.
    pub fn new(specs: Vec<ThumbnailSpec>) -> Self {
        let ithmb_files = specs
            .iter()
            .map(|spec| ItmbFileState {
                correlation_id: spec.correlation_id,
                filename: ithmb_filename(spec.correlation_id),
                image_size: spec.image_byte_size(),
                current_offset: 0,
                data: Vec::new(),
            })
            .collect();

        Self {
            specs,
            ithmb_files,
            track_artworks: Vec::new(),
        }
    }

    /// Add or replace artwork for a track. Decodes the image once, then resizes for each
    /// thumbnail size. If artwork already exists for this dbid, the old entry is replaced.
    pub fn add_artwork(&mut self, dbid: u64, image_bytes: &[u8]) -> crate::Result<()> {
        let img = ithmb::decode_image(image_bytes)?;
        let source_image_bytes = image_bytes.len() as u32;
        let mut thumbnails = Vec::with_capacity(self.specs.len());

        for (i, spec) in self.specs.iter().enumerate() {
            let rgb565 = ithmb::resize_to_rgb565_with_stride(
                &img,
                spec.width,
                spec.height,
                spec.row_stride_pixels,
            );
            let offset = ithmb::append_to_ithmb(&mut self.ithmb_files[i], &rgb565);

            thumbnails.push(ThumbnailEntry {
                correlation_id: spec.correlation_id,
                image_offset: offset,
                image_size: spec.image_byte_size(),
                width: spec.width,
                height: spec.height,
            });
        }

        // Replace existing entry for this dbid if present (avoids duplicate mhii entries).
        if let Some(existing) = self.track_artworks.iter_mut().find(|ta| ta.dbid == dbid) {
            existing.source_image_bytes = source_image_bytes;
            existing.thumbnails = thumbnails;
        } else {
            self.track_artworks.push(TrackArtwork {
                dbid,
                source_image_bytes,
                thumbnails,
            });
        }
        Ok(())
    }

    /// Whether artwork exists for a given track dbid.
    pub fn has_artwork(&self, dbid: u64) -> bool {
        self.track_artworks.iter().any(|ta| ta.dbid == dbid)
    }

    /// Number of thumbnail entries for a track (0 if no artwork).
    pub fn artwork_count(&self, dbid: u64) -> u32 {
        self.track_artworks
            .iter()
            .find(|ta| ta.dbid == dbid)
            .map(|ta| ta.thumbnails.len() as u32)
            .unwrap_or(0)
    }

    /// Set of all dbids that have artwork.
    pub fn dbids_with_artwork(&self) -> HashSet<u64> {
        self.track_artworks.iter().map(|ta| ta.dbid).collect()
    }

    /// Path to the ArtworkDB file on disk.
    pub fn db_path(mount_point: &std::path::Path) -> PathBuf {
        mount_point
            .join("iPod_Control")
            .join("Artwork")
            .join("ArtworkDB")
    }
}

/// Generate the .ithmb filename for a given correlation ID.
pub fn ithmb_filename(correlation_id: u32) -> String {
    format!("F{correlation_id}_1.ithmb")
}

/// Thumbnail specs for iPod Video (5G): 100x100 + 200x200.
pub fn model_specs_video() -> Vec<ThumbnailSpec> {
    vec![
        ThumbnailSpec {
            correlation_id: 1028,
            width: 100,
            height: 100,
            row_stride_pixels: 100,
            pixel_format: PixelFormat::Rgb565,
        },
        ThumbnailSpec {
            correlation_id: 1029,
            width: 200,
            height: 200,
            row_stride_pixels: 200,
            pixel_format: PixelFormat::Rgb565,
        },
    ]
}

/// Thumbnail specs for iPod Classic (6G/7G): 55x55 (stride 56) + 128x128 + 320x320.
///
/// The small spec (F1061) is displayed at 55×55 but stored with a 56-pixel
/// row stride — each entry is 6160 bytes (56×55×2), confirmed by byte-diff
/// against an iTunes reference. Using a plain 55×55 or 56×56 spec produces
/// the wrong per-entry byte size and the firmware reads misaligned data.
pub fn model_specs_classic() -> Vec<ThumbnailSpec> {
    vec![
        ThumbnailSpec {
            correlation_id: 1061,
            width: 55,
            height: 55,
            row_stride_pixels: 56,
            pixel_format: PixelFormat::Rgb565,
        },
        ThumbnailSpec {
            correlation_id: 1055,
            width: 128,
            height: 128,
            row_stride_pixels: 128,
            pixel_format: PixelFormat::Rgb565,
        },
        ThumbnailSpec {
            correlation_id: 1060,
            width: 320,
            height: 320,
            row_stride_pixels: 320,
            pixel_format: PixelFormat::Rgb565,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ithmb_filename() {
        assert_eq!(ithmb_filename(1028), "F1028_1.ithmb");
        assert_eq!(ithmb_filename(1060), "F1060_1.ithmb");
    }

    #[test]
    fn test_thumbnail_spec_byte_size() {
        let spec = ThumbnailSpec {
            correlation_id: 1028,
            width: 100,
            height: 100,
            row_stride_pixels: 100,
            pixel_format: PixelFormat::Rgb565,
        };
        assert_eq!(spec.image_byte_size(), 100 * 100 * 2);
    }

    #[test]
    fn test_thumbnail_spec_byte_size_with_padded_stride() {
        // Classic small thumbnail: 55×55 display, 56-pixel row stride.
        let spec = ThumbnailSpec {
            correlation_id: 1061,
            width: 55,
            height: 55,
            row_stride_pixels: 56,
            pixel_format: PixelFormat::Rgb565,
        };
        assert_eq!(spec.image_byte_size(), 6160, "56 × 55 × 2 = 6160");
    }

    #[test]
    fn test_classic_small_spec_matches_reference() {
        let specs = model_specs_classic();
        let small = specs.iter().find(|s| s.correlation_id == 1061).unwrap();
        assert_eq!(small.width, 55);
        assert_eq!(small.height, 55);
        assert_eq!(small.row_stride_pixels, 56);
        assert_eq!(small.image_byte_size(), 6160);
    }

    #[test]
    fn test_model_presets_video() {
        let specs = model_specs_video();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].correlation_id, 1028);
        assert_eq!(specs[0].width, 100);
        assert_eq!(specs[1].correlation_id, 1029);
        assert_eq!(specs[1].width, 200);
    }

    #[test]
    fn test_model_presets_classic() {
        let specs = model_specs_classic();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].correlation_id, 1061);
        assert_eq!(specs[0].width, 55);
        assert_eq!(specs[1].correlation_id, 1055);
        assert_eq!(specs[1].width, 128);
        assert_eq!(specs[2].correlation_id, 1060);
        assert_eq!(specs[2].width, 320);
    }

    #[test]
    fn test_artwork_store_new() {
        let store = ArtworkStore::new(model_specs_video());
        assert_eq!(store.ithmb_files.len(), 2);
        assert_eq!(store.ithmb_files[0].filename, "F1028_1.ithmb");
        assert_eq!(store.ithmb_files[0].image_size, 100 * 100 * 2);
        assert_eq!(store.ithmb_files[1].filename, "F1029_1.ithmb");
        assert_eq!(store.ithmb_files[1].image_size, 200 * 200 * 2);
    }

    #[test]
    fn test_artwork_store_has_artwork_empty() {
        let store = ArtworkStore::new(model_specs_video());
        assert!(!store.has_artwork(1));
        assert_eq!(store.artwork_count(1), 0);
        assert!(store.dbids_with_artwork().is_empty());
    }

    /// Helper: create a minimal 4x4 solid-red PNG in memory.
    fn make_test_png() -> Vec<u8> {
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

    #[test]
    fn test_add_artwork_full_pipeline() {
        let png = make_test_png();
        let mut store = ArtworkStore::new(model_specs_video());

        store.add_artwork(42, &png).unwrap();
        store.add_artwork(99, &png).unwrap();

        // Verify store state.
        assert!(store.has_artwork(42));
        assert!(store.has_artwork(99));
        assert!(!store.has_artwork(1));
        assert_eq!(store.artwork_count(42), 2); // 2 specs
        assert_eq!(store.dbids_with_artwork().len(), 2);

        // Verify ithmb data accumulated correctly.
        let expected_small = 100 * 100 * 2;
        let expected_large = 200 * 200 * 2;
        assert_eq!(store.ithmb_files[0].data.len(), expected_small * 2); // 2 images
        assert_eq!(store.ithmb_files[1].data.len(), expected_large * 2);

        // Verify offsets: first image at 0, second at image_size.
        let ta0 = &store.track_artworks[0];
        assert_eq!(ta0.thumbnails[0].image_offset, 0);
        let ta1 = &store.track_artworks[1];
        assert_eq!(ta1.thumbnails[0].image_offset, expected_small as u32);
    }

    #[test]
    fn test_add_artwork_duplicate_dbid_replaces() {
        let png = make_test_png();
        let mut store = ArtworkStore::new(model_specs_video());

        store.add_artwork(42, &png).unwrap();
        store.add_artwork(42, &png).unwrap();

        // Should have exactly one entry, not two.
        assert_eq!(store.track_artworks.len(), 1);
        assert_eq!(store.track_artworks[0].dbid, 42);
        assert_eq!(store.dbids_with_artwork().len(), 1);
    }

    #[test]
    fn test_full_db_artwork_integration() {
        use crate::{itunesdb, itunesdb_write, IpodDatabase, IpodTrack};

        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();

        let mut db = IpodDatabase::new(mount);
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Test".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: None,
            genre: None,
            track_number: Some(1),
            disc_number: None,
            total_time_ms: Some(180000),
            year: Some(2024),
            file_size: 5_000_000,
            bitrate: Some(320),
            sample_rate: Some(44100),
            ipod_path: ":iPod_Control:Music:F00:ABCD.mp3".into(),
            filetype: 0x4d503320,
        });

        // Add artwork.
        let png = make_test_png();
        db.init_artwork(model_specs_video());
        let dbid = db.tracks[0].dbid;
        db.set_track_artwork(dbid, &png).unwrap();

        // Serialize iTunesDB and verify mhit has artwork flags.
        let data = itunesdb_write::serialize(&db);
        let db2 = itunesdb::parse(&data, dir.path().to_path_buf()).unwrap();
        assert_eq!(db2.tracks.len(), 1);

        // Serialize ArtworkDB and verify structure.
        let art_data = super::artworkdb::serialize(db.artwork_store.as_ref().unwrap());
        assert_eq!(&art_data[0..4], b"mhfd");
    }

    #[test]
    fn test_add_artwork_invalid_image() {
        let mut store = ArtworkStore::new(model_specs_video());
        let result = store.add_artwork(1, b"not a valid image");
        assert!(result.is_err());
        assert!(!store.has_artwork(1));
        // No ithmb data should have been written.
        assert_eq!(store.ithmb_files[0].data.len(), 0);
    }

    #[test]
    fn test_set_track_artwork_without_init() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::IpodDatabase::new(dir.path().to_path_buf());
        // artwork_store is None — set_track_artwork should fail.
        let mut db = db;
        let result = db.set_track_artwork(1, &make_test_png());
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not initialized"));
    }

    #[test]
    fn test_write_to_disk_with_artwork() {
        use crate::{itunesdb_write, IpodDatabase, IpodTrack};

        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();

        let mut db = IpodDatabase::new(mount.clone());
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "Song".into(),
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
            ipod_path: ":iPod_Control:Music:F00:AAAA.mp3".into(),
            filetype: 0x4d503320,
        });

        let png = make_test_png();
        db.init_artwork(model_specs_video());
        let dbid = db.tracks[0].dbid;
        db.set_track_artwork(dbid, &png).unwrap();

        // write_to_disk writes iTunesDB + ArtworkDB + .ithmb files.
        itunesdb_write::write_to_disk(&db, None).unwrap();

        // Verify all files exist.
        assert!(mount.join("iPod_Control/iTunes/iTunesDB").exists());
        assert!(mount.join("iPod_Control/Artwork/ArtworkDB").exists());
        assert!(mount.join("iPod_Control/Artwork/F1028_1.ithmb").exists());
        assert!(mount.join("iPod_Control/Artwork/F1029_1.ithmb").exists());

        // Verify ithmb file sizes match expected pixel data.
        let small = std::fs::read(mount.join("iPod_Control/Artwork/F1028_1.ithmb")).unwrap();
        assert_eq!(small.len(), 100 * 100 * 2);
        let large = std::fs::read(mount.join("iPod_Control/Artwork/F1029_1.ithmb")).unwrap();
        assert_eq!(large.len(), 200 * 200 * 2);
    }

    #[test]
    fn test_mhit_artwork_fields_in_serialized_output() {
        use crate::{itunesdb_write, IpodDatabase, IpodTrack};

        let dir = tempfile::tempdir().unwrap();
        let mut db = IpodDatabase::new(dir.path().to_path_buf());
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "T".into(),
            artist: "A".into(),
            album: "A".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 1000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:AAAA.mp3".into(),
            filetype: 0x4d503320,
        });

        let png = make_test_png();
        db.init_artwork(model_specs_video());
        let dbid = db.tracks[0].dbid;
        db.set_track_artwork(dbid, &png).unwrap();

        let data = itunesdb_write::serialize(&db);

        // Find the mhit chunk (after mhbd header + mhsd4 + mhsd1 header + mhlt header).
        // Search for "mhit" magic in the output.
        let mhit_pos = data
            .windows(4)
            .position(|w| w == b"mhit")
            .expect("mhit not found");

        // artwork_count at offset +132
        let art_count =
            u32::from_le_bytes(data[mhit_pos + 132..mhit_pos + 136].try_into().unwrap());
        assert_eq!(art_count, 2); // 2 thumbnail specs

        // has_artwork at offset +156
        let has_art = u32::from_le_bytes(data[mhit_pos + 156..mhit_pos + 160].try_into().unwrap());
        assert_eq!(has_art, 1);
    }

    #[test]
    fn test_mhit_no_artwork_fields_zero() {
        use crate::{itunesdb_write, IpodDatabase, IpodTrack};

        let dir = tempfile::tempdir().unwrap();
        let mut db = IpodDatabase::new(dir.path().to_path_buf());
        db.add_track(IpodTrack {
            dbid: 0,
            track_id: 0,
            title: "T".into(),
            artist: "A".into(),
            album: "A".into(),
            album_artist: None,
            genre: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            year: None,
            file_size: 1000,
            bitrate: None,
            sample_rate: None,
            ipod_path: ":iPod_Control:Music:F00:AAAA.mp3".into(),
            filetype: 0x4d503320,
        });
        // No artwork initialized.

        let data = itunesdb_write::serialize(&db);
        let mhit_pos = data
            .windows(4)
            .position(|w| w == b"mhit")
            .expect("mhit not found");

        let art_count =
            u32::from_le_bytes(data[mhit_pos + 132..mhit_pos + 136].try_into().unwrap());
        assert_eq!(art_count, 0);

        let has_art = u32::from_le_bytes(data[mhit_pos + 156..mhit_pos + 160].try_into().unwrap());
        assert_eq!(has_art, 0);
    }
}
