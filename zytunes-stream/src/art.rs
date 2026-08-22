//! Album-art loading via ArtCache + lofty embedded pictures.

use std::path::Path;

use zytunes::art_cache::ArtCache;

/// Image bytes plus a sniffed MIME type for `GET /tracks/{id}/art`.
pub struct TrackArt {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

/// Prefer the on-disk art cache; on miss, read the first embedded picture
/// via lofty. `ArtCache` files are named `.jpg`, so only JPEG payloads are
/// stored — PNG/WebP/GIF still serve, they just skip the cache.
pub fn load_track_art(
    artist: &str,
    album: &str,
    source: &Path,
    cache: Option<&ArtCache>,
) -> Option<TrackArt> {
    if let Some(cache) = cache {
        if let Some(bytes) = cache.lookup(artist, album) {
            return Some(TrackArt {
                content_type: image_content_type(&bytes).unwrap_or("application/octet-stream"),
                bytes,
            });
        }
    }

    let tagged = lofty::probe::read_from_path(source).ok()?;
    use lofty::file::TaggedFileExt;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pic = tag.pictures().first()?;
    let bytes = pic.data().to_vec();
    let content_type = image_content_type(&bytes).unwrap_or("application/octet-stream");

    if content_type == "image/jpeg" {
        if let Some(cache) = cache {
            let _ = cache.store(artist, album, source, &bytes);
        }
    }
    Some(TrackArt {
        bytes,
        content_type,
    })
}

/// Sniff a few common image magics. JPEG is `FF D8` (SOI); PNG/GIF/WebP use
/// their standard signatures. Unknown payloads still serve, but not as JPEG.
fn image_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_jpeg_png_gif_webp() {
        assert_eq!(image_content_type(&[0xFF, 0xD8, 0xFF]), Some("image/jpeg"));
        assert_eq!(
            image_content_type(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some("image/png")
        );
        assert_eq!(image_content_type(b"GIF89a...."), Some("image/gif"));
        let mut webp = [0u8; 12];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        assert_eq!(image_content_type(&webp), Some("image/webp"));
        assert_eq!(image_content_type(b"not-an-image"), None);
    }
}
