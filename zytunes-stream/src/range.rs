//! Byte-Range parsing and audio Content-Type helpers.

/// Inclusive byte range from an HTTP `Range: bytes=start-end` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Parse `bytes=START-END` or `bytes=START-`. `file_len` is used to clamp
/// open-ended ranges. Returns `None` for unsatisfiable or malformed input.
pub fn parse_byte_range(header: &str, file_len: u64) -> Option<ByteRange> {
    let header = header.trim();
    let rest = header.strip_prefix("bytes=")?;
    let (start_s, end_s) = rest.split_once('-')?;
    if start_s.is_empty() {
        // suffix ranges (`bytes=-500`) — not needed for v1
        return None;
    }
    let start: u64 = start_s.parse().ok()?;
    if file_len == 0 || start >= file_len {
        return None;
    }
    let end = if end_s.is_empty() {
        file_len - 1
    } else {
        let end: u64 = end_s.parse().ok()?;
        end.min(file_len - 1)
    };
    if end < start {
        return None;
    }
    Some(ByteRange { start, end })
}

/// MIME type for a file extension (lowercase, no leading dot).
pub fn content_type_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" | "alac" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "opus" => "audio/ogg",
        "wma" => "audio/x-ms-wma",
        "aiff" | "aif" => "audio/aiff",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_closed_range() {
        assert_eq!(
            parse_byte_range("bytes=0-3", 100),
            Some(ByteRange { start: 0, end: 3 })
        );
    }

    #[test]
    fn parse_open_ended_range() {
        assert_eq!(
            parse_byte_range("bytes=10-", 100),
            Some(ByteRange { start: 10, end: 99 })
        );
    }

    #[test]
    fn parse_clamps_end_to_file() {
        assert_eq!(
            parse_byte_range("bytes=0-9999", 50),
            Some(ByteRange { start: 0, end: 49 })
        );
    }

    #[test]
    fn parse_rejects_start_past_eof() {
        assert!(parse_byte_range("bytes=100-", 100).is_none());
        assert!(parse_byte_range("bytes=0-10", 0).is_none());
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_byte_range("bytes=", 10).is_none());
        assert!(parse_byte_range("units=0-1", 10).is_none());
        assert!(parse_byte_range("bytes=5-2", 10).is_none());
        assert!(parse_byte_range("bytes=-5", 10).is_none());
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_for("flac"), "audio/flac");
        assert_eq!(content_type_for("MP3"), "audio/mpeg");
        assert_eq!(content_type_for("m4a"), "audio/mp4");
        assert_eq!(content_type_for("wav"), "audio/wav");
        assert_eq!(content_type_for("xyz"), "application/octet-stream");
    }

    #[test]
    fn byte_range_len() {
        assert_eq!(ByteRange { start: 0, end: 3 }.len(), 4);
        assert_eq!(ByteRange { start: 10, end: 10 }.len(), 1);
    }
}
