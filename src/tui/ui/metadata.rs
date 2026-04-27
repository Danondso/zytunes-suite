//! Track-info popup metadata formatter.
//!
//! Produces the row list rendered by `draw_track_info_overlay`. Pure
//! transformation: takes a `TrackInfo` (always available) plus an optional
//! library `Track` (Library browse mode), and returns a sequence of
//! `Section` dividers + `Field` key/value rows. Sections are emitted only
//! when at least one of their fields is non-empty so lightly-tagged tracks
//! stay terse.
//!
//! Lives next to the renderer because the row shape is purely a UI concern,
//! but it has no `ratatui` dependency on its own — that lets the unit
//! tests assert against the structured output directly.

use crate::app::{format_duration, TrackInfo};
use zytunes::library::Track;

/// One row inside the track-info popup body. `Section` renders as a dim
/// divider header; `Field` renders as a key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataRow {
    Section(&'static str),
    Field { key: &'static str, value: String },
}

/// Build the displayable rows for the track-info popup. The TUI's
/// `TrackInfo` carries the always-present identity fields plus the bits the
/// device cares about (duration, kind, on_device); when a matching library
/// `Track` is supplied (Library browse mode), the richer extended-metadata
/// fields (composer, ISRC, MusicBrainz IDs, audio properties, …) are
/// surfaced too.
///
/// Sections are emitted **only** if at least one field inside them is
/// non-empty — keeps the popup terse on lightly-tagged tracks.
pub fn format_metadata_pairs(track: &TrackInfo, lib: Option<&Track>) -> Vec<MetadataRow> {
    let mut out: Vec<MetadataRow> = Vec::new();

    // -- Identity (always shown) --
    out.push(MetadataRow::Section("Identity"));
    out.push(MetadataRow::Field {
        key: "Title",
        value: track.name.clone(),
    });
    out.push(MetadataRow::Field {
        key: "Artist",
        value: track.artist.clone(),
    });
    if let Some(aa) = lib.and_then(|t| t.album_artist.as_ref()) {
        out.push(MetadataRow::Field {
            key: "Album Artist",
            value: aa.clone(),
        });
    }
    out.push(MetadataRow::Field {
        key: "Album",
        value: track.album.clone(),
    });
    let track_total = lib.and_then(|t| t.track_total);
    if let Some(n) = track.track_number {
        let v = match track_total {
            Some(total) => format!("{n} / {total}"),
            None => format!("{n}"),
        };
        out.push(MetadataRow::Field {
            key: "Track #",
            value: v,
        });
    }
    let disc_total = lib.and_then(|t| t.disc_total);
    if let Some(n) = track.disc_number {
        let v = match disc_total {
            Some(total) => format!("{n} / {total}"),
            None => format!("{n}"),
        };
        out.push(MetadataRow::Field {
            key: "Disc #",
            value: v,
        });
    }

    // -- Classification --
    let mut class_rows: Vec<MetadataRow> = Vec::new();
    if let Some(g) = track.genre.as_ref() {
        class_rows.push(MetadataRow::Field {
            key: "Genre",
            value: g.clone(),
        });
    } else if let Some(g) = lib.and_then(|t| t.genre.as_ref()) {
        class_rows.push(MetadataRow::Field {
            key: "Genre",
            value: g.clone(),
        });
    }
    if let Some(y) = lib.and_then(|t| t.year) {
        class_rows.push(MetadataRow::Field {
            key: "Year",
            value: y.to_string(),
        });
    }
    if let Some(b) = lib.and_then(|t| t.bpm) {
        class_rows.push(MetadataRow::Field {
            key: "BPM",
            value: b.to_string(),
        });
    }
    push_lib_string(&mut class_rows, "Initial Key", lib, |t| {
        t.initial_key.as_deref()
    });
    push_lib_string(&mut class_rows, "Mood", lib, |t| t.mood.as_deref());
    push_lib_string(&mut class_rows, "Language", lib, |t| t.language.as_deref());
    if let Some(r) = lib.and_then(|t| t.rating) {
        class_rows.push(MetadataRow::Field {
            key: "Rating",
            value: format!("{r}/255"),
        });
    }
    if !class_rows.is_empty() {
        out.push(MetadataRow::Section("Classification"));
        out.append(&mut class_rows);
    }

    // -- Credits --
    let mut cred_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut cred_rows, "Composer", lib, |t| t.composer.as_deref());
    push_lib_string(&mut cred_rows, "Conductor", lib, |t| t.conductor.as_deref());
    push_lib_string(&mut cred_rows, "Lyricist", lib, |t| t.lyricist.as_deref());
    push_lib_string(&mut cred_rows, "Original Artist", lib, |t| {
        t.original_artist.as_deref()
    });
    push_lib_string(&mut cred_rows, "Original Album", lib, |t| {
        t.original_album.as_deref()
    });
    push_lib_string(&mut cred_rows, "Original Release Date", lib, |t| {
        t.original_release_date.as_deref()
    });
    if !cred_rows.is_empty() {
        out.push(MetadataRow::Section("Credits"));
        out.append(&mut cred_rows);
    }

    // -- Identifiers --
    let mut id_rows: Vec<MetadataRow> = Vec::new();
    if let Some(id) = lib.map(|t| t.id) {
        id_rows.push(MetadataRow::Field {
            key: "Library ID",
            value: id.to_string(),
        });
    }
    push_lib_string(&mut id_rows, "ISRC", lib, |t| t.isrc.as_deref());
    push_lib_string(&mut id_rows, "Barcode", lib, |t| t.barcode.as_deref());
    push_lib_string(&mut id_rows, "Catalog #", lib, |t| {
        t.catalog_number.as_deref()
    });
    push_lib_string(&mut id_rows, "Publisher", lib, |t| t.publisher.as_deref());
    push_lib_string(&mut id_rows, "Copyright", lib, |t| t.copyright.as_deref());
    if !id_rows.is_empty() {
        out.push(MetadataRow::Section("Identifiers"));
        out.append(&mut id_rows);
    }

    // -- MusicBrainz --
    let mut mb_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut mb_rows, "Recording ID", lib, |t| {
        t.mb_recording_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Track ID", lib, |t| t.mb_track_id.as_deref());
    push_lib_string(&mut mb_rows, "Release ID", lib, |t| {
        t.mb_release_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Release Group ID", lib, |t| {
        t.mb_release_group_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Work ID", lib, |t| t.mb_work_id.as_deref());
    push_lib_string(&mut mb_rows, "Artist ID", lib, |t| {
        t.mb_artist_id.as_deref()
    });
    push_lib_string(&mut mb_rows, "Release Artist ID", lib, |t| {
        t.mb_release_artist_id.as_deref()
    });
    if !mb_rows.is_empty() {
        out.push(MetadataRow::Section("MusicBrainz"));
        out.append(&mut mb_rows);
    }

    // -- ReplayGain --
    let mut rg_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut rg_rows, "Track Gain", lib, |t| {
        t.replaygain_track_gain.as_deref()
    });
    push_lib_string(&mut rg_rows, "Track Peak", lib, |t| {
        t.replaygain_track_peak.as_deref()
    });
    push_lib_string(&mut rg_rows, "Album Gain", lib, |t| {
        t.replaygain_album_gain.as_deref()
    });
    push_lib_string(&mut rg_rows, "Album Peak", lib, |t| {
        t.replaygain_album_peak.as_deref()
    });
    if !rg_rows.is_empty() {
        out.push(MetadataRow::Section("ReplayGain"));
        out.append(&mut rg_rows);
    }

    // -- Audio properties --
    let mut audio_rows: Vec<MetadataRow> = Vec::new();
    if let Some(k) = track.kind.as_ref() {
        audio_rows.push(MetadataRow::Field {
            key: "Format",
            value: k.clone(),
        });
    }
    if let Some(rate) = lib.and_then(|t| t.sample_rate) {
        audio_rows.push(MetadataRow::Field {
            key: "Sample Rate",
            value: format!("{:.1} kHz", rate as f64 / 1000.0),
        });
    }
    if let Some(c) = lib.and_then(|t| t.channels) {
        audio_rows.push(MetadataRow::Field {
            key: "Channels",
            value: c.to_string(),
        });
    }
    if let Some(bd) = lib.and_then(|t| t.bit_depth) {
        audio_rows.push(MetadataRow::Field {
            key: "Bit Depth",
            value: format!("{bd} bit"),
        });
    }
    if let Some(br) = lib.and_then(|t| t.audio_bitrate_kbps) {
        audio_rows.push(MetadataRow::Field {
            key: "Bitrate",
            value: format!("{br} kbps"),
        });
    }
    if let Some(d) = track.duration_ms {
        audio_rows.push(MetadataRow::Field {
            key: "Duration",
            value: format_duration(d),
        });
    }
    if !audio_rows.is_empty() {
        out.push(MetadataRow::Section("Audio"));
        out.append(&mut audio_rows);
    }

    // -- File --
    let mut file_rows: Vec<MetadataRow> = Vec::new();
    if let Some(loc) = track.location.as_ref() {
        file_rows.push(MetadataRow::Field {
            key: "Path",
            value: loc.clone(),
        });
    }
    if let Some(sz) = lib.and_then(|t| t.file_size_bytes) {
        file_rows.push(MetadataRow::Field {
            key: "File Size",
            value: format_bytes(sz),
        });
    }
    push_lib_string(&mut file_rows, "Encoder", lib, |t| t.encoder.as_deref());
    push_lib_string(&mut file_rows, "Encoder Settings", lib, |t| {
        t.encoder_settings.as_deref()
    });
    push_lib_string(&mut file_rows, "AcoustID", lib, |t| {
        t.acoustic_id.as_deref()
    });
    if !file_rows.is_empty() {
        out.push(MetadataRow::Section("File"));
        out.append(&mut file_rows);
    }

    // -- Notes --
    let mut note_rows: Vec<MetadataRow> = Vec::new();
    push_lib_string(&mut note_rows, "Comment", lib, |t| t.comment.as_deref());
    push_lib_string(&mut note_rows, "Description", lib, |t| {
        t.description.as_deref()
    });
    if let Some(lyr) = lib.and_then(|t| t.lyrics.as_ref()) {
        // Lyrics can be many KB; show only the first line + a count, the
        // rest would dominate the popup.
        let line_count = lyr.lines().count().max(1);
        let preview = lyr.lines().next().unwrap_or("").to_string();
        let value = if line_count > 1 {
            format!("{preview}  … ({line_count} lines)")
        } else {
            preview
        };
        if !value.is_empty() {
            note_rows.push(MetadataRow::Field {
                key: "Lyrics",
                value,
            });
        }
    }
    if !note_rows.is_empty() {
        out.push(MetadataRow::Section("Notes"));
        out.append(&mut note_rows);
    }

    // -- Listening (aggregate plays/skips, valid whether on-device or not) --
    let mut listening_rows: Vec<MetadataRow> = Vec::new();
    if let Some(p) = track.play_count {
        listening_rows.push(MetadataRow::Field {
            key: "Plays",
            value: p.to_string(),
        });
    }
    if let Some(s) = track.skip_count {
        listening_rows.push(MetadataRow::Field {
            key: "Skips",
            value: s.to_string(),
        });
    }
    if let Some(t) = track.last_played_at_ms {
        listening_rows.push(MetadataRow::Field {
            key: "Last played",
            value: humanize_relative_ms(t, now_unix_ms_for_humanize()),
        });
    }
    if !listening_rows.is_empty() {
        out.push(MetadataRow::Section("Listening"));
        out.append(&mut listening_rows);
    }

    // -- Device-side bits (only when on-device) --
    if track.on_device {
        let mut dev_rows: Vec<MetadataRow> = Vec::new();
        if let Some(r) = track.rating {
            dev_rows.push(MetadataRow::Field {
                key: "Device Rating",
                value: format!("{}/100", r),
            });
        }
        if let Some(t) = track.last_synced_from_device_at_ms {
            dev_rows.push(MetadataRow::Field {
                key: "Last synced",
                value: humanize_relative_ms(t, now_unix_ms_for_humanize()),
            });
        }
        if !dev_rows.is_empty() {
            out.push(MetadataRow::Section("Device"));
            out.append(&mut dev_rows);
        }
    }

    out
}

/// Wall-clock now in unix ms. Wrapped here so the popup formatter has a
/// single point to swap for tests (the `humanize_relative_ms` helper takes
/// `now_ms` explicitly so unit tests pin the relative output).
pub(super) fn now_unix_ms_for_humanize() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Render a unix-epoch-ms timestamp as a relative string against `now_ms`,
/// e.g. `"just now"`, `"5 minutes ago"`, `"yesterday"`, `"3 days ago"`,
/// or `"2026-04-12"` for older. Future timestamps (clock skew between
/// devices) clamp to "just now" so the popup never shows nonsense like
/// "in 5 minutes."
pub fn humanize_relative_ms(then_ms: u64, now_ms: u64) -> String {
    if then_ms >= now_ms {
        return "just now".to_string();
    }
    let secs = (now_ms - then_ms) / 1000;
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{mins} minutes ago")
        };
    }
    let hours = mins / 60;
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        };
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{days} days ago");
    }
    if days < 30 {
        let weeks = days / 7;
        return if weeks == 1 {
            "1 week ago".to_string()
        } else {
            format!("{weeks} weeks ago")
        };
    }
    // Older than a month — render as a calendar date in the user's local
    // sense of "year-month-day". We don't pull in chrono just for this;
    // do the math by hand against the unix epoch (1970-01-01 UTC).
    format_iso_date(then_ms / 1000)
}

/// Format a unix-second timestamp as `YYYY-MM-DD` (UTC). Standalone helper
/// instead of pulling in chrono — the popup only ever needs UTC date,
/// not full datetime formatting.
fn format_iso_date(unix_secs: u64) -> String {
    // Days since 1970-01-01.
    let mut days = (unix_secs / 86_400) as i64;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap_year(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months_leap = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = if is_leap_year(year) {
        &months_leap
    } else {
        &months_normal
    };
    let mut month: i64 = 1;
    for &dm in months {
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    let day = days + 1; // 1-based day
    format!("{year:04}-{month:02}-{day:02}")
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Helper: append a `Field` row from a library getter, only when the value
/// is non-empty after trimming.
fn push_lib_string(
    rows: &mut Vec<MetadataRow>,
    key: &'static str,
    lib: Option<&Track>,
    pick: impl Fn(&Track) -> Option<&str>,
) {
    if let Some(v) = lib.and_then(pick) {
        if !v.trim().is_empty() {
            rows.push(MetadataRow::Field {
                key,
                value: v.to_string(),
            });
        }
    }
}

/// Render byte counts in a compact human-readable form (KB / MB / GB).
pub(super) fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TrackInfo;

    fn empty_track_info() -> TrackInfo {
        TrackInfo::new(
            "Some Title".into(),
            "Some Artist".into(),
            "Some Album".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
    }

    #[test]
    fn format_metadata_pairs_skips_empty_fields() {
        let ti = empty_track_info();
        let rows = format_metadata_pairs(&ti, None);
        let field_keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(field_keys.contains(&"Title"));
        assert!(field_keys.contains(&"Artist"));
        assert!(field_keys.contains(&"Album"));
        assert!(!field_keys.contains(&"Composer"));
        assert!(!field_keys.contains(&"ISRC"));
        assert!(!field_keys.contains(&"Sample Rate"));
        assert!(!field_keys.contains(&"BPM"));
    }

    #[test]
    fn format_metadata_pairs_groups_into_sections() {
        let mut ti = empty_track_info();
        ti.duration_ms = Some(120_000);
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            isrc: Some("USRC17607839".into()),
            sample_rate: Some(44_100),
            channels: Some(2),
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let sections: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Section(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert!(sections.contains(&"Identifiers"));
        assert!(sections.contains(&"Audio"));
    }

    #[test]
    fn format_metadata_pairs_formats_audio_properties() {
        let ti = empty_track_info();
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            sample_rate: Some(44_100),
            channels: Some(2),
            audio_bitrate_kbps: Some(320),
            file_size_bytes: Some(5_242_880),
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let mut found_rate = false;
        let mut found_channels = false;
        let mut found_bitrate = false;
        let mut found_size = false;
        for row in &rows {
            if let MetadataRow::Field { key, value } = row {
                match *key {
                    "Sample Rate" => {
                        assert!(value.contains("44.1") && value.contains("kHz"));
                        found_rate = true;
                    }
                    "Channels" => {
                        assert_eq!(value, "2");
                        found_channels = true;
                    }
                    "Bitrate" => {
                        assert!(value.contains("320") && value.contains("kbps"));
                        found_bitrate = true;
                    }
                    "File Size" => {
                        assert!(value.contains("MB") || value.contains("MiB"));
                        found_size = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(found_rate);
        assert!(found_channels);
        assert!(found_bitrate);
        assert!(found_size);
    }

    #[test]
    fn format_metadata_pairs_enriches_with_library_track() {
        let ti = empty_track_info();
        let lib = Track {
            id: 1,
            name: "Some Title".into(),
            artist: "Some Artist".into(),
            album: "Some Album".into(),
            composer: Some("Hans Zimmer".into()),
            mb_release_id: Some("aaaa-bbbb".into()),
            replaygain_track_gain: Some("-7.20 dB".into()),
            ..Default::default()
        };
        let rows = format_metadata_pairs(&ti, Some(&lib));
        let by_key: std::collections::HashMap<&str, &str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, value } => Some((*key, value.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(by_key.get("Composer"), Some(&"Hans Zimmer"));
        assert_eq!(by_key.get("Release ID"), Some(&"aaaa-bbbb"));
        assert_eq!(by_key.get("Track Gain"), Some(&"-7.20 dB"));
    }

    #[test]
    fn humanize_just_now_for_under_one_minute() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now, now), "just now");
        assert_eq!(humanize_relative_ms(now - 30_000, now), "just now");
        assert_eq!(humanize_relative_ms(now - 59_999, now), "just now");
    }

    #[test]
    fn humanize_future_clamps_to_just_now() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now + 300_000, now), "just now");
    }

    #[test]
    fn humanize_minutes() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now - 60_000, now), "1 minute ago");
        assert_eq!(humanize_relative_ms(now - 600_000, now), "10 minutes ago");
        assert_eq!(
            humanize_relative_ms(now - 59 * 60_000, now),
            "59 minutes ago"
        );
    }

    #[test]
    fn humanize_hours() {
        let now = 1_700_000_000_000u64;
        assert_eq!(humanize_relative_ms(now - 3_600_000, now), "1 hour ago");
        assert_eq!(
            humanize_relative_ms(now - 5 * 3_600_000, now),
            "5 hours ago"
        );
    }

    #[test]
    fn humanize_yesterday_then_days() {
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        assert_eq!(humanize_relative_ms(now - day, now), "yesterday");
        assert_eq!(humanize_relative_ms(now - 3 * day, now), "3 days ago");
        assert_eq!(humanize_relative_ms(now - 6 * day, now), "6 days ago");
    }

    #[test]
    fn humanize_weeks() {
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        assert_eq!(humanize_relative_ms(now - 7 * day, now), "1 week ago");
        assert_eq!(humanize_relative_ms(now - 14 * day, now), "2 weeks ago");
        assert_eq!(humanize_relative_ms(now - 28 * day, now), "4 weeks ago");
    }

    #[test]
    fn humanize_falls_through_to_iso_date_after_a_month() {
        let now = 1_700_000_000_000u64;
        let day = 86_400_000u64;
        let result = humanize_relative_ms(now - 60 * day, now);
        assert_eq!(result, "2023-09-15");
    }

    #[test]
    fn format_iso_date_unix_epoch() {
        assert_eq!(format_iso_date(0), "1970-01-01");
    }

    #[test]
    fn format_iso_date_known_dates() {
        assert_eq!(format_iso_date(1_700_000_000), "2023-11-14");
        assert_eq!(format_iso_date(1_709_164_800), "2024-02-29");
        assert_eq!(format_iso_date(951_868_800), "2000-03-01");
        let day = 86_400u64;
        let feb28_2100 = 4_107_456_000;
        assert_eq!(format_iso_date(feb28_2100), "2100-02-28");
        assert_eq!(format_iso_date(feb28_2100 + day), "2100-03-01");
    }

    #[test]
    fn format_metadata_pairs_renders_aggregate_plays_and_skips() {
        let mut ti = empty_track_info();
        ti.play_count = Some(9);
        ti.skip_count = Some(2);
        let rows = format_metadata_pairs(&ti, None);
        let listening_rows: Vec<(&str, &str)> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, value } => Some((*key, value.as_str())),
                _ => None,
            })
            .collect();
        assert!(listening_rows.contains(&("Plays", "9")));
        assert!(listening_rows.contains(&("Skips", "2")));
    }

    #[test]
    fn format_metadata_pairs_renders_last_played_when_set() {
        let mut ti = empty_track_info();
        ti.play_count = Some(1);
        ti.last_played_at_ms = Some(1_700_000_000_000);
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Last played"));
    }

    #[test]
    fn format_metadata_pairs_omits_last_played_when_only_device_plays() {
        let mut ti = empty_track_info();
        ti.play_count = Some(7);
        ti.last_played_at_ms = None;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Plays"));
        assert!(!keys.contains(&"Last played"));
    }

    #[test]
    fn format_metadata_pairs_omits_skips_row_when_unset() {
        let mut ti = empty_track_info();
        ti.play_count = Some(5);
        ti.skip_count = None;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Plays"));
        assert!(!keys.contains(&"Skips"));
    }

    #[test]
    fn format_metadata_pairs_listening_section_appears_off_device() {
        let mut ti = empty_track_info();
        ti.on_device = false;
        ti.play_count = Some(4);
        let rows = format_metadata_pairs(&ti, None);
        let sections: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Section(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert!(sections.contains(&"Listening"));
    }

    #[test]
    fn format_metadata_pairs_last_synced_only_when_on_device() {
        let mut ti = empty_track_info();
        ti.on_device = false;
        ti.last_synced_from_device_at_ms = Some(1_700_000_000_000);
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(!keys.contains(&"Last synced"));

        ti.on_device = true;
        let rows = format_metadata_pairs(&ti, None);
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MetadataRow::Field { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"Last synced"));
    }
}
