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
    resize_to_rgb565_with_stride(img, width, height, width)
}

/// Resize and convert to RGB565 LE with an explicit per-row storage stride.
///
/// When `row_stride_pixels > width`, each row is zero-padded from `width*2`
/// bytes out to `row_stride_pixels*2` bytes. Used for the iPod Classic small
/// thumbnail, which iTunes stores as 55 displayed pixels per row but 56
/// pixels of storage per row (6160 bytes per 55×55 entry instead of 6050).
pub fn resize_to_rgb565_with_stride(
    img: &image::DynamicImage,
    width: u16,
    height: u16,
    row_stride_pixels: u16,
) -> Vec<u8> {
    assert!(
        row_stride_pixels >= width,
        "row_stride_pixels ({row_stride_pixels}) must be >= width ({width})"
    );
    let resized = img.resize_exact(width as u32, height as u32, FilterType::Lanczos3);
    let stride_bytes = row_stride_pixels as usize * 2;
    let mut buf = vec![0u8; stride_bytes * height as usize];

    for y in 0..height as u32 {
        let row_start = y as usize * stride_bytes;
        for x in 0..width as u32 {
            let pixel = resized.get_pixel(x, y);
            let bytes = rgb_to_565(pixel[0], pixel[1], pixel[2]);
            let p = row_start + x as usize * 2;
            buf[p] = bytes[0];
            buf[p + 1] = bytes[1];
        }
        // Trailing bytes in the row (from `width*2` to `stride_bytes`) stay zero.
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

    fn solid_red_png(size: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(size, size);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgb([0xFF, 0x00, 0x00]);
        }
        let mut png_bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            size,
            size,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        png_bytes
    }

    #[test]
    fn test_resize_with_stride_matches_reference_shape() {
        // Classic small thumbnail: 55×55 display, 56-pixel row stride.
        // Produces 6160 bytes (56×55×2) with the last 2 bytes of each row padded to zero.
        let png = solid_red_png(100);
        let img = decode_image(&png).unwrap();
        let out = resize_to_rgb565_with_stride(&img, 55, 55, 56);
        assert_eq!(out.len(), 6160);

        let red_le = 0xF800u16.to_le_bytes();
        let stride_bytes = 56 * 2;
        for row in 0..55usize {
            let row_start = row * stride_bytes;
            // First 55 pixels are the resized solid-red image.
            for x in 0..55usize {
                let p = row_start + x * 2;
                assert_eq!(&out[p..p + 2], &red_le, "row {row} px {x}");
            }
            // Last pixel of each row is zero-padded.
            let pad_start = row_start + 55 * 2;
            assert_eq!(&out[pad_start..pad_start + 2], &[0, 0], "row {row} padding");
        }
    }

    #[test]
    fn test_resize_without_stride_padding_is_compact() {
        // When stride == width, output is exactly width*height*2 with no padding.
        let png = solid_red_png(10);
        let img = decode_image(&png).unwrap();
        let out = resize_to_rgb565_with_stride(&img, 10, 10, 10);
        assert_eq!(out.len(), 200);
    }

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
        for chunk in rgb565.as_chunks::<2>().0 {
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
