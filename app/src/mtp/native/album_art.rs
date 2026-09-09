//! Album-art extraction and JPEG normalization for the Zune `SetObjectPropValue`
//! representative-sample-data path.
//!
//! Lives in its own module because the JPEG-normalization steps below are
//! specifically structured to dodge the Zune 30 firmware's prop-handler bug,
//! and they're easier to reason about (and to test in isolation) when not
//! interleaved with `NativeSession` plumbing.

/// Extract and re-encode album art as a conservative 200x200 baseline JPEG.
///
/// We go through explicit RGB8 and a concrete `JpegEncoder` (rather than
/// `DynamicImage::write_to(Jpeg)`) to keep the output defensive:
///   - Force RGB8: strips alpha / palette / CMYK / grayscale / 16-bit quirks.
///   - Explicit baseline quality: no progressive scan, no surprise defaults.
///   - No ICC / EXIF / XMP carried over from the source picture.
///   - Validate SOI/EOI markers so we never hand the Zune a truncated stream.
///
/// Context: the Zune 30 firmware occasionally wedges (ReadPipe timeout with
/// no response code) when `SetObjectPropValue` is called with certain JPEGs.
/// A single wedge poisons the whole MTP session. Normalizing the output
/// through a minimal, metadata-free encoder drops the probability of hitting
/// the decoder bug.
pub(super) fn extract_album_art(path: &str) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ExtendedColorType;
    use lofty::file::TaggedFileExt;

    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pic = tag.pictures().first()?;
    let img = image::load_from_memory(pic.data()).ok()?;
    let resized = img.resize_exact(200, 200, image::imageops::FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut jpeg_buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, 85);
    encoder
        .encode(rgb.as_raw(), 200, 200, ExtendedColorType::Rgb8)
        .ok()?;

    // Round-trip pass: decode the freshly encoded JPEG and re-encode at the
    // same quality. mtp-probe album-art-check showed v1.4 firmware hangs on
    // certain first-pass JPEG byte patterns (a ~21 KB encode) but accepts
    // the slightly smaller round-tripped output from the same source.
    // Re-encoding from already-quantized DCT data smooths the high-frequency
    // content just enough to dodge the firmware's prop-handler bug.
    let final_buf = match image::load_from_memory(&jpeg_buf) {
        Ok(img2) => {
            let rgb2 = img2.to_rgb8();
            let mut buf2: Vec<u8> = Vec::with_capacity(jpeg_buf.len());
            let mut enc2 = JpegEncoder::new_with_quality(&mut buf2, 85);
            if enc2
                .encode(rgb2.as_raw(), 200, 200, ExtendedColorType::Rgb8)
                .is_ok()
            {
                buf2
            } else {
                jpeg_buf
            }
        }
        Err(_) => jpeg_buf,
    };

    // Sanity: baseline JPEG starts with FFD8 (SOI) and ends with FFD9 (EOI).
    // If the encoder produced something truncated, don't send it.
    if final_buf.len() < 4
        || final_buf[..2] != [0xFF, 0xD8]
        || final_buf[final_buf.len() - 2..] != [0xFF, 0xD9]
    {
        return None;
    }
    Some(final_buf)
}
