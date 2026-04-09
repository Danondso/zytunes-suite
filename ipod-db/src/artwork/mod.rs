//! ArtworkDB and ITHMB support for iPod album art.
//!
//! Classic iPods never read embedded ID3 album art — they only read from
//! `iPod_Control/Artwork/ArtworkDB` plus `.ithmb` raw pixel files. This module
//! handles converting source images (JPEG/PNG) to the iPod's RGB565 pixel format,
//! accumulating them into `.ithmb` files, and serializing the ArtworkDB binary.

pub mod artworkdb;
pub mod ithmb;

use std::collections::{HashMap, HashSet};
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
    /// Thumbnail width in pixels.
    pub width: u16,
    /// Thumbnail height in pixels.
    pub height: u16,
    /// Pixel format (always RGB565 for now).
    pub pixel_format: PixelFormat,
}

impl ThumbnailSpec {
    /// Bytes per image at this thumbnail size (width * height * bytes_per_pixel).
    pub fn image_byte_size(&self) -> u32 {
        let bpp = match self.pixel_format {
            PixelFormat::Rgb565 => 2,
        };
        self.width as u32 * self.height as u32 * bpp
    }
}

/// Tracks the accumulated state of one `.ithmb` file during a write session.
#[derive(Debug)]
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
    /// One entry per thumbnail size.
    pub thumbnails: Vec<ThumbnailEntry>,
}

/// In-memory artwork state for the entire database.
#[derive(Debug)]
pub struct ArtworkStore {
    /// Thumbnail size specifications for the target iPod model.
    pub specs: Vec<ThumbnailSpec>,
    /// Accumulated .ithmb file data, one per spec.
    pub ithmb_files: Vec<ItmbFileState>,
    /// Per-track artwork entries.
    pub track_artworks: Vec<TrackArtwork>,
    /// Source image bytes keyed by dbid (kept for potential re-encoding).
    source_images: HashMap<u64, Vec<u8>>,
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
            source_images: HashMap::new(),
        }
    }

    /// Add artwork for a track. Encodes the image into all thumbnail sizes.
    pub fn add_artwork(&mut self, dbid: u64, image_bytes: &[u8]) -> crate::Result<()> {
        let mut thumbnails = Vec::with_capacity(self.specs.len());

        for (i, spec) in self.specs.iter().enumerate() {
            let rgb565 = ithmb::encode_rgb565(image_bytes, spec.width, spec.height)?;
            let offset = ithmb::append_to_ithmb(&mut self.ithmb_files[i], &rgb565);

            thumbnails.push(ThumbnailEntry {
                correlation_id: spec.correlation_id,
                image_offset: offset,
                image_size: spec.image_byte_size(),
                width: spec.width,
                height: spec.height,
            });
        }

        self.track_artworks.push(TrackArtwork { dbid, thumbnails });
        self.source_images.insert(dbid, image_bytes.to_vec());
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
            pixel_format: PixelFormat::Rgb565,
        },
        ThumbnailSpec {
            correlation_id: 1029,
            width: 200,
            height: 200,
            pixel_format: PixelFormat::Rgb565,
        },
    ]
}

/// Thumbnail specs for iPod Classic (6G/7G): 128x128 + 320x320.
pub fn model_specs_classic() -> Vec<ThumbnailSpec> {
    vec![
        ThumbnailSpec {
            correlation_id: 1055,
            width: 128,
            height: 128,
            pixel_format: PixelFormat::Rgb565,
        },
        ThumbnailSpec {
            correlation_id: 1060,
            width: 320,
            height: 320,
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
            pixel_format: PixelFormat::Rgb565,
        };
        assert_eq!(spec.image_byte_size(), 100 * 100 * 2);
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
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].correlation_id, 1055);
        assert_eq!(specs[0].width, 128);
        assert_eq!(specs[1].correlation_id, 1060);
        assert_eq!(specs[1].width, 320);
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
}
