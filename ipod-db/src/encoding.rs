//! Shared encoding utilities for iTunesDB and ArtworkDB binary formats.

use byteorder::{LittleEndian, WriteBytesExt};

/// Encode a Rust string as UTF-16LE bytes.
pub fn encode_utf16le(s: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        buf.write_u16::<LittleEndian>(unit).unwrap();
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_utf16le_ascii() {
        let bytes = encode_utf16le("AB");
        assert_eq!(bytes, &[0x41, 0x00, 0x42, 0x00]);
    }

    #[test]
    fn test_encode_utf16le_empty() {
        assert!(encode_utf16le("").is_empty());
    }

    #[test]
    fn test_encode_utf16le_non_ascii() {
        // CJK character U+4E16 (世) = 0x4E16 LE = [0x16, 0x4E]
        let bytes = encode_utf16le("世");
        assert_eq!(bytes, &[0x16, 0x4E]);
    }
}
