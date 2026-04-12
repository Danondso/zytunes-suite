/// A file or directory entry from the device.
#[derive(Debug, Clone, Default)]
pub struct DeviceEntry {
    pub object_id: u64,
    #[allow(dead_code)]
    pub storage_id: u64,
    pub format: String,
    pub size: u64,
    pub name: String,
    /// Track number (1-based), if known from ZMDB.
    pub track_number: Option<u32>,
    /// Disc number (1-based), if known from ZMDB.
    pub disc_number: Option<u32>,
}

impl DeviceEntry {
    pub fn is_dir(&self) -> bool {
        self.format == "Association"
    }
}

/// Parse output lines from `lsext` into DeviceEntry structs.
/// Format: `<object_id>  <storage_id>  <format>  <size>  <date>  <time>  <name>`
/// Example: `83886101   65537      Association          0 ????-??-?? ??:??:??  Albums`
pub fn parse_lsext(lines: &[String]) -> Vec<DeviceEntry> {
    let mut entries = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        // Need at least 7 tokens: oid, sid, format, size, date, time, name...
        if parts.len() < 7 {
            continue;
        }

        let oid = match parts[0].parse::<u64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let sid = match parts[1].parse::<u64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let format = parts[2].to_string();
        let size = parts[3].parse::<u64>().unwrap_or(0);
        // parts[4] = date, parts[5] = time, parts[6..] = name (may contain spaces)
        let name = parts[6..].join(" ");

        entries.push(DeviceEntry {
            object_id: oid,
            storage_id: sid,
            format,
            size,
            name,
            ..Default::default()
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lsext_valid_directory() {
        let lines = vec![
            "83886101   65537      Association          0 ????-??-?? ??:??:??  Albums".to_string(),
        ];
        let entries = parse_lsext(&lines);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].object_id, 83886101);
        assert_eq!(entries[0].storage_id, 65537);
        assert_eq!(entries[0].format, "Association");
        assert_eq!(entries[0].size, 0);
        assert_eq!(entries[0].name, "Albums");
        assert!(entries[0].is_dir());
    }

    #[test]
    fn parse_lsext_valid_file() {
        let lines = vec![
            "83886200   65537      MP3           5242880 2024-01-15 10:30:00  Song.mp3".to_string(),
        ];
        let entries = parse_lsext(&lines);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 5242880);
        assert_eq!(entries[0].name, "Song.mp3");
        assert!(!entries[0].is_dir());
    }

    #[test]
    fn parse_lsext_name_with_spaces() {
        let lines = vec![
            "100   65537      MP3           1000 2024-01-01 00:00:00  My Great Song.mp3"
                .to_string(),
        ];
        let entries = parse_lsext(&lines);
        assert_eq!(entries[0].name, "My Great Song.mp3");
    }

    #[test]
    fn parse_lsext_skips_invalid_input() {
        let lines = vec![
            "".to_string(),                                         // empty
            "   ".to_string(),                                      // blank
            "not a valid line".to_string(),                         // too few tokens
            "abc 65537 MP3 0 2024-01-01 00:00:00 Name".to_string(), // non-numeric oid
            "100 xyz MP3 0 2024-01-01 00:00:00 Name".to_string(),   // non-numeric sid
        ];
        assert!(parse_lsext(&lines).is_empty());
    }

    #[test]
    fn parse_lsext_multiple_entries() {
        let lines = vec![
            "100   65537      Association          0 ????-??-?? ??:??:??  Music".to_string(),
            "200   65537      MP3           3000 2024-01-01 00:00:00  track.mp3".to_string(),
            "300   65537      WMA           4000 2024-02-01 12:00:00  other.wma".to_string(),
        ];
        let entries = parse_lsext(&lines);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_dir());
        assert!(!entries[1].is_dir());
    }

    #[test]
    fn parse_lsext_invalid_size_defaults_to_zero() {
        let lines =
            vec!["100   65537      MP3           badnum 2024-01-01 00:00:00  song.mp3".to_string()];
        assert_eq!(parse_lsext(&lines)[0].size, 0);
    }

    #[test]
    fn is_dir_returns_false_for_non_association_formats() {
        for fmt in &["MP3", "WMA", "AAC", "JPEG", ""] {
            let entry = DeviceEntry {
                object_id: 1,
                storage_id: 1,
                format: fmt.to_string(),
                size: 0,
                name: "f".into(),
                ..Default::default()
            };
            assert!(!entry.is_dir(), "format '{}' should not be dir", fmt);
        }
    }
}
