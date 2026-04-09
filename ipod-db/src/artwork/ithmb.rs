//! ITHMB pixel format conversion and file I/O.
//!
//! iPod `.ithmb` files contain raw RGB565 pixel data — no headers, just
//! concatenated images at a fixed size per entry. This module converts
//! JPEG/PNG source images to RGB565 and manages the accumulation of
//! pixel data for writing to disk.

use std::path::Path;

use image::imageops::FilterType;
use image::GenericImageView;

use super::ItmbFileState;
use crate::IpodDbError;

/// Convert a single RGB pixel to RGB565 little-endian (2 bytes).
///
/// Layout: `RRRRR GGGGGG BBBBB` packed into a u16 LE.
fn rgb_to_565(r: u8, g: u8, b: u8) -> [u8; 2] {
    let val: u16 = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
    val.to_le_bytes()
}

/// Decode a JPEG/PNG image and convert to RGB565 LE at the target dimensions.
///
/// The image is resized using Lanczos3 filtering for quality. Output length
/// is always `width * height * 2` bytes.
pub fn encode_rgb565(image_bytes: &[u8], width: u16, height: u16) -> crate::Result<Vec<u8>> {
    let img = decode_image(image_bytes)?;
    Ok(resize_to_rgb565(&img, width, height))
}

/// Decode JPEG/PNG bytes into a DynamicImage.
pub fn decode_image(image_bytes: &[u8]) -> crate::Result<image::DynamicImage> {
    image::load_from_memory(image_bytes)
        .map_err(|e| IpodDbError::Artwork(format!("failed to decode image: {e}")))
}

/// Resize a pre-decoded image to the target dimensions and convert to RGB565 LE.
pub fn resize_to_rgb565(img: &image::DynamicImage, width: u16, height: u16) -> Vec<u8> {
    let resized = img.resize_exact(width as u32, height as u32, FilterType::Lanczos3);

    let mut buf = Vec::with_capacity(width as usize * height as usize * 2);
    for y in 0..height as u32 {
        for x in 0..width as u32 {
            let pixel = resized.get_pixel(x, y);
            buf.extend_from_slice(&rgb_to_565(pixel[0], pixel[1], pixel[2]));
        }
    }

    buf
}

/// Append RGB565 pixel data to an ItmbFileState.
///
/// Returns the byte offset where this image was placed.
pub fn append_to_ithmb(ithmb: &mut ItmbFileState, rgb565_data: &[u8]) -> u32 {
    let offset = ithmb.current_offset;
    ithmb.data.extend_from_slice(rgb565_data);
    ithmb.current_offset += rgb565_data.len() as u32;
    offset
}

/// Write all accumulated .ithmb files to `iPod_Control/Artwork/` on disk.
pub fn write_ithmb_files(mount_point: &Path, ithmb_files: &[ItmbFileState]) -> crate::Result<()> {
    let artwork_dir = mount_point.join("iPod_Control").join("Artwork");
    std::fs::create_dir_all(&artwork_dir)?;

    for ithmb in ithmb_files {
        if ithmb.data.is_empty() {
            continue;
        }
        let path = artwork_dir.join(&ithmb.filename);
        std::fs::write(&path, &ithmb.data)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_565_known_colors() {
        // Pure red: R=0xFF -> 0x1F<<11 = 0xF800
        assert_eq!(rgb_to_565(0xFF, 0x00, 0x00), 0xF800u16.to_le_bytes());
        // Pure green: G=0xFF -> 0x3F<<5 = 0x07E0
        assert_eq!(rgb_to_565(0x00, 0xFF, 0x00), 0x07E0u16.to_le_bytes());
        // Pure blue: B=0xFF -> 0x1F = 0x001F
        assert_eq!(rgb_to_565(0x00, 0x00, 0xFF), 0x001Fu16.to_le_bytes());
        // Black
        assert_eq!(rgb_to_565(0x00, 0x00, 0x00), 0x0000u16.to_le_bytes());
        // White
        assert_eq!(rgb_to_565(0xFF, 0xFF, 0xFF), 0xFFFFu16.to_le_bytes());
    }

    #[test]
    fn test_encode_rgb565_solid_red() {
        // Create a 4x4 solid red PNG in memory.
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

        let rgb565 = encode_rgb565(&png_bytes, 4, 4).unwrap();
        assert_eq!(rgb565.len(), 4 * 4 * 2);
        // Every pixel should be 0xF800 LE.
        for chunk in rgb565.chunks_exact(2) {
            assert_eq!(chunk, &0xF800u16.to_le_bytes());
        }
    }

    #[test]
    fn test_encode_rgb565_resize() {
        // Create a 100x100 image, encode at 50x50.
        let img = image::RgbImage::new(100, 100);
        let mut png_bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            100,
            100,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

        let rgb565 = encode_rgb565(&png_bytes, 50, 50).unwrap();
        assert_eq!(rgb565.len(), 50 * 50 * 2);
    }

    #[test]
    fn test_append_to_ithmb_offsets() {
        let mut state = ItmbFileState {
            correlation_id: 1028,
            filename: "F1028_1.ithmb".into(),
            image_size: 20000, // 100*100*2
            current_offset: 0,
            data: Vec::new(),
        };

        let data1 = vec![0u8; 20000];
        let offset1 = append_to_ithmb(&mut state, &data1);
        assert_eq!(offset1, 0);
        assert_eq!(state.current_offset, 20000);

        let data2 = vec![0u8; 20000];
        let offset2 = append_to_ithmb(&mut state, &data2);
        assert_eq!(offset2, 20000);
        assert_eq!(state.current_offset, 40000);
        assert_eq!(state.data.len(), 40000);
    }

    #[test]
    fn test_write_ithmb_files() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path();

        let files = vec![
            ItmbFileState {
                correlation_id: 1028,
                filename: "F1028_1.ithmb".into(),
                image_size: 4,
                current_offset: 4,
                data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            },
            ItmbFileState {
                correlation_id: 1029,
                filename: "F1029_1.ithmb".into(),
                image_size: 0,
                current_offset: 0,
                data: Vec::new(), // empty — should be skipped
            },
        ];

        write_ithmb_files(mount, &files).unwrap();

        let written = std::fs::read(mount.join("iPod_Control/Artwork/F1028_1.ithmb")).unwrap();
        assert_eq!(written, vec![0xDE, 0xAD, 0xBE, 0xEF]);

        // Empty file should not be written.
        assert!(!mount.join("iPod_Control/Artwork/F1029_1.ithmb").exists());
    }
}
