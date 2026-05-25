//! Apply MusicBrainz metadata to a freshly-ripped audio file via lofty.
//!
//! The rip pipeline produces a bare audio file (MP3 / FLAC / WAV) with no
//! tag. This module fills in the tag from the MB release the user picked in
//! the import overlay so the file lands in the library with full identity
//! and (for the Phase 4 fix-feature roadmap) MBIDs that future re-tagging
//! can key on.
//!
//! Tag fields populated:
//! - title, artist, album, album-artist (always)
//! - track number / total tracks (when MB provides them)
//! - year (parsed from `release.date` "YYYY-MM-DD" prefix)
//! - 6 MusicBrainz IDs: track, recording, release, release-group,
//!   release-artist, track-artist. The library scanner (`src/dirlib.rs`)
//!   reads all six; missing the *track* ID would break round-trip when
//!   the user later re-scans. The library `Track` struct has a seventh
//!   field (`mb_work_id`) but MB doesn't surface it on release lookup,
//!   so we don't write it here.
//!
//! The exact tag container is chosen by lofty based on the file extension.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, ItemValue, Tag, TagExt, TagItem};

use crate::musicbrainz::{render_artist_credit, Release, Track as MbTrack};

/// Tag `path` with metadata from the chosen MB release + track.
///
/// `track_position` is the 1-indexed CD track position (also the position
/// in MB's track list for an audio CD). `total_tracks` defaults to the
/// medium's track count when known.
pub fn tag_ripped_file(
    path: &Path,
    release: &Release,
    track: &MbTrack,
    track_position: u32,
    total_tracks: Option<u32>,
) -> Result<(), String> {
    let mut tagged = lofty::probe::read_from_path(path)
        .map_err(|e| format!("lofty probe failed on {}: {e}", path.display()))?;

    let tag_type = tagged.primary_tag_type();
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .ok_or_else(|| "lofty refused to attach a tag".to_string())?;

    set_string(tag, ItemKey::TrackTitle, &track.title);
    set_string(tag, ItemKey::AlbumTitle, &release.title);
    set_string(
        tag,
        ItemKey::TrackArtist,
        &render_artist_credit(&track.artist_credit),
    );

    let album_artist = render_artist_credit(&release.artist_credit);
    if !album_artist.is_empty() {
        set_string(tag, ItemKey::AlbumArtist, &album_artist);
    }

    set_string(tag, ItemKey::TrackNumber, &track_position.to_string());
    if let Some(total) = total_tracks {
        set_string(tag, ItemKey::TrackTotal, &total.to_string());
    }

    if let Some(year) = release.date.as_deref().and_then(extract_year) {
        set_string(tag, ItemKey::Year, &year.to_string());
    }

    // MusicBrainz IDs the future fix-feature roadmap needs.
    // The MB *track* ID (track.id) is distinct from the *recording* ID
    // (rec.id) — both round-trip through `src/dirlib.rs` (MusicBrainzTrackId
    // + MusicBrainzRecordingId), so we write both.
    set_string(tag, ItemKey::MusicBrainzTrackId, &track.id);
    if let Some(rec) = &track.recording {
        set_string(tag, ItemKey::MusicBrainzRecordingId, &rec.id);
    }
    set_string(tag, ItemKey::MusicBrainzReleaseId, &release.id);
    if let Some(rg) = &release.release_group {
        set_string(tag, ItemKey::MusicBrainzReleaseGroupId, &rg.id);
    }
    if let Some(artist_id) = release
        .artist_credit
        .first()
        .and_then(|ac| ac.artist.as_ref())
        .map(|a| a.id.clone())
    {
        set_string(tag, ItemKey::MusicBrainzReleaseArtistId, &artist_id);
    }
    if let Some(artist_id) = track
        .artist_credit
        .first()
        .and_then(|ac| ac.artist.as_ref())
        .map(|a| a.id.clone())
    {
        set_string(tag, ItemKey::MusicBrainzArtistId, &artist_id);
    }

    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("lofty save failed on {}: {e}", path.display()))?;
    Ok(())
}

fn set_string(tag: &mut Tag, key: ItemKey, value: &str) {
    if value.is_empty() {
        return;
    }
    tag.insert(TagItem::new(key, ItemValue::Text(value.to_string())));
}

/// Extract a 4-digit year prefix from an MB date string.
///
/// MB dates come as `YYYY`, `YYYY-MM`, or `YYYY-MM-DD`. The year prefix
/// is always the first four chars when present.
fn extract_year(date: &str) -> Option<u16> {
    let year_str = date.split('-').next()?;
    if year_str.len() != 4 {
        return None;
    }
    year_str.parse().ok()
}

/// Build a path the rip pipeline writes a finished track to. Returns
/// `{dest_dir}/{Artist}/{Album}/{TT - Title}.{ext}` with characters that
/// would break filesystems (slashes, NULs, ASCII control) sanitised out.
///
/// `Artist` is the album artist (so a single-album folder doesn't fan out
/// per-track-artist on compilation discs).
pub fn ripped_track_destination(
    dest_dir: &Path,
    release: &Release,
    track: &MbTrack,
    track_position: u32,
    extension: &str,
) -> std::path::PathBuf {
    let artist = sanitise_filename_component(&render_artist_credit(&release.artist_credit));
    let album = sanitise_filename_component(&release.title);
    let title = sanitise_filename_component(&track.title);
    let filename = format!("{track_position:02} - {title}.{extension}");
    dest_dir.join(artist).join(album).join(filename)
}

/// Replace filesystem-hostile characters in a filename component. Keeps
/// the result identifiable (doesn't aggressively transliterate) while
/// ensuring it can land on macOS/Linux/Windows-via-network mounts.
fn sanitise_filename_component(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "Unknown".to_string();
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 => {} // strip controls
            c => out.push(c),
        }
    }
    // Trailing dots/spaces break Windows path resolution on shared mounts.
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        "Unknown".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musicbrainz::{Artist, ArtistCredit, Medium, Release, Track as MbTrack};

    fn release(title: &str, artist: &str, date: Option<&str>) -> Release {
        Release {
            id: "rel-1".into(),
            title: title.into(),
            date: date.map(str::to_string),
            country: None,
            artist_credit: vec![ArtistCredit {
                name: artist.into(),
                joinphrase: None,
                artist: Some(Artist {
                    id: "art-1".into(),
                    name: artist.into(),
                    sort_name: None,
                }),
            }],
            media: vec![Medium {
                position: Some(1),
                format: Some("CD".into()),
                track_count: Some(1),
                tracks: vec![],
            }],
            release_group: None,
        }
    }

    fn mb_track(title: &str, position: u32) -> MbTrack {
        MbTrack {
            id: format!("trk-{position}"),
            number: position.to_string(),
            position: Some(position),
            title: title.into(),
            length: Some(180_000),
            recording: None,
            artist_credit: vec![],
        }
    }

    #[test]
    fn dest_path_uses_album_artist_and_track_position() {
        let dest = ripped_track_destination(
            Path::new("/m"),
            &release("X", "A", None),
            &mb_track("T", 5),
            5,
            "flac",
        );
        assert_eq!(dest, Path::new("/m/A/X/05 - T.flac"));
    }

    #[test]
    fn dest_path_pads_track_number_to_two_digits() {
        let dest = ripped_track_destination(
            Path::new("/m"),
            &release("X", "A", None),
            &mb_track("T", 1),
            1,
            "mp3",
        );
        assert!(dest.to_string_lossy().contains("01 - T.mp3"));
    }

    #[test]
    fn sanitise_filename_replaces_path_separators() {
        assert_eq!(sanitise_filename_component("AC/DC"), "AC_DC");
        assert_eq!(sanitise_filename_component("a:b"), "a_b");
        assert_eq!(sanitise_filename_component("Foo?Bar*"), "Foo_Bar_");
    }

    #[test]
    fn sanitise_filename_strips_controls() {
        assert_eq!(sanitise_filename_component("a\x01b\x02c"), "abc");
    }

    #[test]
    fn sanitise_filename_falls_back_for_empty_input() {
        assert_eq!(sanitise_filename_component(""), "Unknown");
        assert_eq!(sanitise_filename_component("   "), "Unknown");
        // Input that decays to empty after stripping (only control chars).
        assert_eq!(sanitise_filename_component("\x01\x02\x03"), "Unknown");
    }

    #[test]
    fn sanitise_filename_replaces_but_keeps_slash_only_input() {
        // "///" becomes "___" — the caller's input was nonsense but we
        // preserve the structural shape rather than wiping to "Unknown".
        assert_eq!(sanitise_filename_component("///"), "___");
    }

    #[test]
    fn sanitise_filename_strips_trailing_dots_and_spaces() {
        // Windows network mounts choke on these.
        assert_eq!(sanitise_filename_component("Foo..."), "Foo");
        assert_eq!(sanitise_filename_component("Foo   "), "Foo");
    }

    #[test]
    fn extract_year_parses_each_mb_date_form() {
        assert_eq!(extract_year("1969"), Some(1969));
        assert_eq!(extract_year("1969-09"), Some(1969));
        assert_eq!(extract_year("1969-09-26"), Some(1969));
        assert_eq!(extract_year(""), None);
        assert_eq!(extract_year("abcd"), None);
        assert_eq!(extract_year("69"), None);
    }
}
