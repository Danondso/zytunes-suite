//! Apply MusicBrainz metadata to a freshly-ripped audio file via lofty.
//!
//! The rip pipeline produces a bare audio file (MP3 / FLAC / WAV) with no
//! tag. This module fills in the tag from the MB release the user picked in
//! the import overlay so the file lands in the library with full identity
//! and Picard-equivalent release metadata.
//!
//! Tag fields populated:
//! - Identity: title, artist, album, album-artist (always)
//! - Numbering: track number / total tracks, disc number / total discs
//! - Dates: full release date (RecordingDate / TDRC — the ID3v2.4
//!   canonical date frame; readers that expose a 4-digit year pick the
//!   YYYY prefix from the same frame), original release date (TDOR
//!   from `release_group.first_release_date`)
//! - Release identifiers: ISRC (from `recording.isrcs[0]`), barcode,
//!   catalog number, label, original media type, script, language
//! - 6 MusicBrainz IDs: track, recording, release, release-group,
//!   release-artist, track-artist. The library scanner (`src/dirlib.rs`)
//!   reads all six; missing the *track* ID would break round-trip when
//!   the user later re-scans. The library `Track` struct has a seventh
//!   field (`mb_work_id`) but MB doesn't surface it on release lookup,
//!   so we don't write it here.
//! - Picard-compatible Unknown-keyed tags (no lofty enum variant):
//!   MUSICBRAINZ_ALBUMSTATUS, MUSICBRAINZ_ALBUMTYPE, RELEASECOUNTRY,
//!   MUSICBRAINZ_ALBUMPACKAGING, SCRIPT. ASIN is skipped — see comment
//!   in `write_release_identifiers`.
//!
//! The exact tag container is chosen by lofty based on the file extension.

use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

use crate::musicbrainz::{render_artist_credit, Medium, Release, Track as MbTrack};

/// One-line summary of the tags `tag_ripped_file` + `tag_ripped_fingerprint`
/// embed. Lofty handles container-specific encoding (ID3v2 for MP3/WAV,
/// Vorbis Comments for FLAC, iTunes-style atoms for M4A/ALAC/AAC), so the
/// *set* of tags is uniform across all `RipFidelity` choices.
///
/// Surfaced in the TUI import overlay so users can see what metadata
/// they'll get before kicking off a rip.
pub const RIP_TAG_SUMMARY: &str =
    "title, artist, album, track/disc #, release date, MBIDs (×6), ISRC, barcode, label, catalog #, AcoustID fingerprint";

/// Tag `path` with metadata from the chosen MB release + track.
///
/// `track_position` is the 1-indexed CD track position (also the position
/// in MB's track list for an audio CD). `total_tracks` defaults to the
/// medium's track count when known. `medium` is the MB medium the track
/// belongs to — used to write disc number, total discs, and the original
/// media type. `total_discs` is the count of media on the release.
pub fn tag_ripped_file(
    path: &Path,
    release: &Release,
    track: &MbTrack,
    track_position: u32,
    total_tracks: Option<u32>,
    medium: Option<&Medium>,
    total_discs: Option<u32>,
) -> Result<(), String> {
    let mut tagged = probe_by_content(path)?;

    // Promote WAV's primary from RIFF INFO to ID3v2. RIFF INFO uses 4-char
    // FourCC keys and silently drops both Unknown-keyed Picard tags and
    // several ItemKey variants (ISRC, Barcode, etc.). Picard does the
    // same promotion for its own WAV writes.
    let tag_type = match tagged.primary_tag_type() {
        TagType::RiffInfo => TagType::Id3v2,
        other => other,
    };
    if tagged.tag(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .tag_mut(tag_type)
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

    if let Some(pos) = medium.and_then(|m| m.position) {
        set_string(tag, ItemKey::DiscNumber, &pos.to_string());
    }
    if let Some(total) = total_discs {
        set_string(tag, ItemKey::DiscTotal, &total.to_string());
    }

    // Date strategy: write the full date to `RecordingDate` (TDRC, the
    // ID3v2.4 canonical date frame and what Picard reads). Lofty has no
    // ID3v2 mapping for the legacy `Year` ItemKey — `Tag::insert` would
    // silently drop it. RecordingDate accepts a "YYYY-MM-DD" or "YYYY"
    // string and is what every modern tag reader picks up.
    if let Some(date) = release.date.as_deref() {
        set_string(tag, ItemKey::RecordingDate, date);
    }
    if let Some(first) = release
        .release_group
        .as_ref()
        .and_then(|rg| rg.first_release_date.as_deref())
    {
        set_string(tag, ItemKey::OriginalReleaseDate, first);
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

    write_release_identifiers(tag, release, track, medium);

    // Drop the borrow before save_to_path takes &self.
    let _ = tag;
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("lofty save failed on {}: {e}", path.display()))?;
    Ok(())
}

/// Write release-level identifiers + Picard-compatible Unknown-keyed tags.
/// Split from `tag_ripped_file` to keep the identity/MBID/identifier groups
/// visually separable.
fn write_release_identifiers(
    tag: &mut Tag,
    release: &Release,
    track: &MbTrack,
    medium: Option<&Medium>,
) {
    if let Some(isrc) = track
        .recording
        .as_ref()
        .and_then(|r| r.isrcs.first())
        .map(String::as_str)
    {
        set_string(tag, ItemKey::Isrc, isrc);
    }

    if let Some(barcode) = release.barcode.as_deref() {
        set_string(tag, ItemKey::Barcode, barcode);
    }

    if let Some(li) = release.label_info.first() {
        if let Some(catalog) = li.catalog_number.as_deref() {
            set_string(tag, ItemKey::CatalogNumber, catalog);
        }
        if let Some(name) = li.label.as_ref().map(|l| l.name.as_str()) {
            set_string(tag, ItemKey::Label, name);
        }
    }

    if let Some(format) = medium.and_then(|m| m.format.as_deref()) {
        set_string(tag, ItemKey::OriginalMediaType, format);
    }

    if let Some(tr) = release.text_representation.as_ref() {
        // Script has no ID3v2 enum-variant mapping — Picard writes it as
        // a TXXX:SCRIPT user-defined text frame, which `set_unknown_string`
        // emits via the `Unknown` path that bypasses the format check.
        if let Some(script) = tr.script.as_deref() {
            set_unknown_string(tag, "SCRIPT", script);
        }
        if let Some(lang) = tr.language.as_deref() {
            set_string(tag, ItemKey::Language, lang);
        }
    }

    // Picard-canonical tag names that lofty doesn't model as enum variants.
    // Writing under these names so a user opening the ripped file in
    // Picard sees populated fields and Picard does not re-fetch.
    //
    // Limitation: lofty's abstract `Tag::insert_unchecked` parses any
    // exactly-4-char `ItemKey::Unknown` name as a literal ID3v2 frame ID,
    // which it then rejects if the name isn't a known frame. That makes
    // it impossible to write Picard's `TXXX:ASIN` via this path — Picard
    // reaches `Id3v2Tag::insert_user_text` directly. We skip ASIN; our
    // library scanner doesn't read it, and Picard can re-fetch from MB.
    if let Some(status) = release.status.as_deref() {
        set_unknown_string(tag, "MUSICBRAINZ_ALBUMSTATUS", status);
    }
    if let Some(album_type) = release
        .release_group
        .as_ref()
        .and_then(|rg| rg.primary_type.as_deref())
    {
        set_unknown_string(tag, "MUSICBRAINZ_ALBUMTYPE", album_type);
    }
    if let Some(country) = release.country.as_deref() {
        set_unknown_string(tag, "RELEASECOUNTRY", country);
    }
    if let Some(packaging) = release.packaging.as_deref() {
        set_unknown_string(tag, "MUSICBRAINZ_ALBUMPACKAGING", packaging);
    }
}

fn set_string(tag: &mut Tag, key: ItemKey, value: &str) {
    if value.is_empty() {
        return;
    }
    tag.insert(TagItem::new(key, ItemValue::Text(value.to_string())));
}

fn set_unknown_string(tag: &mut Tag, name: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    // `Tag::insert` verifies a static ItemKey↔TagType mapping exists and
    // silently drops `ItemKey::Unknown`. `insert_unchecked` skips the
    // check; lofty's per-format writer then routes the Unknown item
    // through TXXX (ID3v2) / generic user-defined keys (Vorbis Comments,
    // MP4 freeform) by description. See lofty::tag::Tag::insert docs.
    tag.insert_unchecked(TagItem::new(
        ItemKey::Unknown(name.to_string()),
        ItemValue::Text(value.to_string()),
    ));
}

/// Write a Chromaprint fingerprint into the file's tag as `ACOUSTID_FINGERPRINT`.
///
/// Picard's canonical tag name; `src/fingerprint.rs::read_embedded_fingerprint`
/// matches both the Vorbis-style upper-case form and the ID3v2 `TXXX:Acoustid
/// Fingerprint` description case-insensitively, so writing just the upper-case
/// form is sufficient for the library scanner to pick it up.
///
/// Returns Err if lofty can't open the file or fails to save. The caller
/// treats this as best-effort — a fingerprint write failure shouldn't fail
/// the rip itself.
pub fn tag_ripped_fingerprint(path: &Path, fingerprint: &str) -> Result<(), String> {
    if fingerprint.is_empty() {
        return Ok(());
    }
    let mut tagged = probe_by_content(path)?;

    let tag_type = match tagged.primary_tag_type() {
        TagType::RiffInfo => TagType::Id3v2,
        other => other,
    };
    if tagged.tag(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .tag_mut(tag_type)
        .ok_or_else(|| "lofty refused to attach a tag".to_string())?;

    set_unknown_string(tag, "ACOUSTID_FINGERPRINT", fingerprint);

    let _ = tag;
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("lofty save failed on {}: {e}", path.display()))?;
    Ok(())
}

/// Open `path` for tag I/O using content-based format detection.
///
/// Why not `lofty::probe::read_from_path`: that function picks the
/// backend purely by file extension and returns `UnknownFormat` for
/// anything it can't match. The rip pipeline tags through a temp
/// filename ending in `.part` (`track.m4a.part` etc.) so the dirlib
/// scanner doesn't index incomplete files mid-rip — and `.part` is
/// not a recognised audio extension, so `read_from_path` aborts and
/// the rip's tag step silently drops everything as a `RippedUntagged`
/// warning.
///
/// `Probe::open().guess_file_type().read()` ignores the extension and
/// sniffs the file's magic bytes. The returned `TaggedFile` carries
/// the correctly-identified `FileType`, and the subsequent
/// `tagged.save_to_path` then writes through the matching backend.
fn probe_by_content(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    Probe::open(path)
        .map_err(|e| format!("lofty open failed on {}: {e}", path.display()))?
        .options(ParseOptions::new())
        .guess_file_type()
        .map_err(|e| format!("lofty content sniff failed on {}: {e}", path.display()))?
        .read()
        .map_err(|e| format!("lofty read failed on {}: {e}", path.display()))
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
            barcode: None,
            asin: None,
            status: None,
            packaging: None,
            text_representation: None,
            label_info: vec![],
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

    // -------------------- Phase B tag-write tests --------------------
    //
    // Each test writes a real WAV via `test_audio::write_sine_wav`, calls
    // `tag_ripped_file`, and re-reads via lofty. Tag assertions go through
    // the ID3v2 tag (the WAV primary is promoted to ID3v2 inside
    // `tag_ripped_file` — see the RIFF INFO comment there).

    use crate::musicbrainz::{
        Label as MbLabel, LabelInfo, Recording, ReleaseGroup, TextRepresentation,
    };
    use crate::test_audio::write_sine_wav;
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zytunes-tag-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_test_wav(name: &str) -> std::path::PathBuf {
        let dir = fresh_dir(name);
        let path = dir.join("audio.wav");
        write_sine_wav(&path, 1);
        path
    }

    fn read_text(path: &Path, key: &ItemKey) -> Option<String> {
        let tagged = lofty::probe::read_from_path(path).unwrap();
        let tag = tagged.tag(TagType::Id3v2)?;
        tag.get(key)
            .and_then(|i| i.value().text())
            .map(|s| s.to_string())
    }

    fn read_unknown(path: &Path, name: &str) -> Option<String> {
        let tagged = lofty::probe::read_from_path(path).unwrap();
        let tag = tagged.tag(TagType::Id3v2)?;
        for item in tag.items() {
            if let ItemKey::Unknown(k) = item.key() {
                if k == name {
                    if let ItemValue::Text(v) = item.value() {
                        return Some(v.clone());
                    }
                }
            }
        }
        None
    }

    fn medium_with(position: u32, format: &str) -> Medium {
        Medium {
            position: Some(position),
            format: Some(format.into()),
            track_count: Some(1),
            tracks: vec![],
        }
    }

    #[test]
    fn tags_disc_number_and_total_when_release_has_multiple_media() {
        let path = write_test_wav("disc-number-total");
        let m1 = medium_with(1, "CD");
        let m2 = medium_with(2, "CD");
        let mut rel = release("Album", "Artist", None);
        rel.media = vec![m1.clone(), m2];

        tag_ripped_file(
            &path,
            &rel,
            &mb_track("T", 1),
            1,
            Some(1),
            Some(&m1),
            Some(2),
        )
        .unwrap();

        assert_eq!(read_text(&path, &ItemKey::DiscNumber).as_deref(), Some("1"));
        assert_eq!(read_text(&path, &ItemKey::DiscTotal).as_deref(), Some("2"));
    }

    #[test]
    fn tags_recording_date_full_yyyymmdd() {
        // Single date frame (TDRC) carries the full release date — ID3v2.4
        // canonical, what Picard reads as "Date". Tag-readers that only
        // expose a 4-digit year (older ID3v1 tools, some embedded players)
        // pick up the YYYY prefix from the same frame.
        let path = write_test_wav("release-date");
        let rel = release("Album", "Artist", Some("1973-03-24"));
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_text(&path, &ItemKey::RecordingDate).as_deref(),
            Some("1973-03-24")
        );
    }

    #[test]
    fn tags_original_release_date_from_release_group_first_release_date() {
        let path = write_test_wav("orig-release-date");
        let mut rel = release("Album", "Artist", Some("1987-01-01"));
        rel.release_group = Some(ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Album".into()),
            first_release_date: Some("1973-03-24".into()),
        });
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_text(&path, &ItemKey::OriginalReleaseDate).as_deref(),
            Some("1973-03-24")
        );
        // The pressing date lands in RecordingDate (TDRC).
        assert_eq!(
            read_text(&path, &ItemKey::RecordingDate).as_deref(),
            Some("1987-01-01")
        );
    }

    #[test]
    fn tags_isrc_from_recording_first_isrc() {
        let path = write_test_wav("isrc-present");
        let mut track = mb_track("T", 1);
        track.recording = Some(Recording {
            id: "rec-1".into(),
            title: "T".into(),
            length: None,
            artist_credit: vec![],
            isrcs: vec!["GBAYE6900001".into(), "USRC17607839".into()],
        });
        let rel = release("Album", "Artist", None);
        tag_ripped_file(&path, &rel, &track, 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_text(&path, &ItemKey::Isrc).as_deref(),
            Some("GBAYE6900001"),
            "should take the first ISRC from the recording"
        );
    }

    #[test]
    fn tags_isrc_absent_when_recording_has_no_isrcs() {
        let path = write_test_wav("isrc-absent");
        let mut track = mb_track("T", 1);
        track.recording = Some(Recording {
            id: "rec-1".into(),
            title: "T".into(),
            length: None,
            artist_credit: vec![],
            isrcs: vec![],
        });
        let rel = release("Album", "Artist", None);
        tag_ripped_file(&path, &rel, &track, 1, Some(1), None, None).unwrap();

        assert!(read_text(&path, &ItemKey::Isrc).is_none());
    }

    #[test]
    fn tags_barcode_when_present_skipped_when_empty() {
        // Real MB returns `""` for no-barcode releases — the set_string
        // helper's empty check must skip writing those.
        let path = write_test_wav("barcode-present");
        let mut rel = release("Album", "Artist", None);
        rel.barcode = Some("077774644020".into());
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();
        assert_eq!(
            read_text(&path, &ItemKey::Barcode).as_deref(),
            Some("077774644020")
        );

        let path2 = write_test_wav("barcode-empty");
        let mut rel2 = release("Album", "Artist", None);
        rel2.barcode = Some(String::new());
        tag_ripped_file(&path2, &rel2, &mb_track("T", 1), 1, Some(1), None, None).unwrap();
        assert!(read_text(&path2, &ItemKey::Barcode).is_none());
    }

    #[test]
    fn tags_label_and_catalog_number_from_first_label_info() {
        let path = write_test_wav("label-catalog");
        let mut rel = release("Album", "Artist", None);
        rel.label_info = vec![
            LabelInfo {
                catalog_number: Some("SHVL 804".into()),
                label: Some(MbLabel {
                    id: "lbl-1".into(),
                    name: "Harvest".into(),
                }),
            },
            LabelInfo {
                catalog_number: Some("OTHER 1".into()),
                label: Some(MbLabel {
                    id: "lbl-2".into(),
                    name: "Other".into(),
                }),
            },
        ];
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_text(&path, &ItemKey::CatalogNumber).as_deref(),
            Some("SHVL 804")
        );
        assert_eq!(
            read_text(&path, &ItemKey::Label).as_deref(),
            Some("Harvest")
        );
    }

    #[test]
    fn tags_release_status_via_unknown_key() {
        let path = write_test_wav("release-status");
        let mut rel = release("Album", "Artist", None);
        rel.status = Some("Official".into());
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_unknown(&path, "MUSICBRAINZ_ALBUMSTATUS").as_deref(),
            Some("Official")
        );
    }

    #[test]
    fn tags_release_country_via_unknown_releasecountry_key() {
        let path = write_test_wav("release-country");
        let mut rel = release("Album", "Artist", None);
        rel.country = Some("GB".into());
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(read_unknown(&path, "RELEASECOUNTRY").as_deref(), Some("GB"));
    }

    #[test]
    fn tags_album_type_and_packaging_via_unknown_keys() {
        let path = write_test_wav("album-type-packaging");
        let mut rel = release("Album", "Artist", None);
        rel.packaging = Some("Gatefold Cover".into());
        rel.release_group = Some(ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Album".into()),
            first_release_date: None,
        });
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert_eq!(
            read_unknown(&path, "MUSICBRAINZ_ALBUMTYPE").as_deref(),
            Some("Album")
        );
        assert_eq!(
            read_unknown(&path, "MUSICBRAINZ_ALBUMPACKAGING").as_deref(),
            Some("Gatefold Cover")
        );
    }

    #[test]
    fn tags_skips_asin_due_to_lofty_4char_unknown_limitation() {
        // ASIN is exactly 4 chars — lofty parses ItemKey::Unknown("ASIN")
        // as a literal ID3v2 frame ID and rejects on save. Documenting the
        // skip with a test so a future lofty upgrade or refactor doesn't
        // silently re-introduce broken ASIN writes.
        let path = write_test_wav("asin-skipped");
        let mut rel = release("Album", "Artist", None);
        rel.asin = Some("B000002UAS".into());
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        assert!(
            read_unknown(&path, "ASIN").is_none(),
            "ASIN should be silently skipped; see write_release_identifiers doc comment"
        );
    }

    #[test]
    fn tags_media_type_from_medium_format() {
        let path = write_test_wav("media-type");
        let rel = release("Album", "Artist", None);
        let m = medium_with(1, "Vinyl");
        tag_ripped_file(
            &path,
            &rel,
            &mb_track("T", 1),
            1,
            Some(1),
            Some(&m),
            Some(1),
        )
        .unwrap();

        assert_eq!(
            read_text(&path, &ItemKey::OriginalMediaType).as_deref(),
            Some("Vinyl")
        );
    }

    #[test]
    fn tags_script_and_language_from_text_representation() {
        let path = write_test_wav("script-language");
        let mut rel = release("Album", "Artist", None);
        rel.text_representation = Some(TextRepresentation {
            language: Some("eng".into()),
            script: Some("Latn".into()),
        });
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();

        // Script has no ID3v2 enum mapping — Picard writes it as TXXX:SCRIPT.
        assert_eq!(read_unknown(&path, "SCRIPT").as_deref(), Some("Latn"));
        assert_eq!(read_text(&path, &ItemKey::Language).as_deref(), Some("eng"));
    }

    #[test]
    fn tags_round_trip_through_part_temp_extension() {
        // Regression: the rip pipeline tags through a temp filename
        // ending in `.part` (`track.m4a.part`, `track.flac.part`, ...).
        // Lofty's old `read_from_path` returned `UnknownFormat` for any
        // unrecognised extension, the tag write silently became a
        // RippedUntagged warning, and the renamed `.m4a` file shipped
        // with zero tags. `probe_by_content` sniffs the magic bytes
        // instead so the extension doesn't matter — verified here with
        // a `.wav.part` substrate (WAV→ID3v2 promotion path).
        let dir = fresh_dir("part-extension-roundtrip");
        let path = dir.join("audio.wav.part");
        write_sine_wav(&path, 1);
        let rel = release("Album", "Artist", Some("1998-02-22"));
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();
        // Re-read also goes through content-sniff so the .part extension
        // doesn't break the assertion.
        let tagged = lofty::probe::Probe::open(&path)
            .unwrap()
            .guess_file_type()
            .unwrap()
            .read()
            .unwrap();
        let tag = tagged
            .tag(TagType::Id3v2)
            .expect("id3v2 tag landed on the .part file");
        assert_eq!(
            tag.get(&ItemKey::TrackTitle)
                .and_then(|i| i.value().text())
                .map(str::to_string)
                .as_deref(),
            Some("T")
        );
        assert_eq!(
            tag.get(&ItemKey::AlbumTitle)
                .and_then(|i| i.value().text())
                .map(str::to_string)
                .as_deref(),
            Some("Album")
        );
        assert_eq!(
            tag.get(&ItemKey::MusicBrainzTrackId)
                .and_then(|i| i.value().text())
                .map(str::to_string)
                .as_deref(),
            Some("trk-1")
        );
    }

    #[test]
    fn tags_identity_and_mbids_round_trip() {
        // Sanity check that the existing identity + MBID writes still
        // land in ID3v2 after the WAV-primary promotion. Guards against
        // accidentally regressing the round-trip the rest of the rip
        // pipeline relies on.
        let path = write_test_wav("identity-mbids");
        let rel = release("Abbey Road", "The Beatles", Some("1969-09-26"));
        let track = mb_track("Come Together", 1);
        tag_ripped_file(&path, &rel, &track, 1, Some(2), None, None).unwrap();

        let tagged = lofty::probe::read_from_path(&path).unwrap();
        let tag = tagged.tag(TagType::Id3v2).expect("id3v2 tag present");
        assert_eq!(tag.title().as_deref(), Some("Come Together"));
        assert_eq!(tag.album().as_deref(), Some("Abbey Road"));
        assert_eq!(
            read_text(&path, &ItemKey::MusicBrainzTrackId).as_deref(),
            Some("trk-1")
        );
        assert_eq!(
            read_text(&path, &ItemKey::MusicBrainzReleaseId).as_deref(),
            Some("rel-1")
        );
        assert_eq!(
            read_text(&path, &ItemKey::MusicBrainzReleaseArtistId).as_deref(),
            Some("art-1")
        );
    }

    // -------------------- Phase C fingerprint-write tests --------------------

    #[test]
    fn tag_ripped_fingerprint_writes_acoustid_unknown_key() {
        let path = write_test_wav("acoustid-fp-write");
        // Write the MB tags first so we exercise the typical sequence
        // `tag_ripped_file` → `tag_ripped_fingerprint`.
        let rel = release("Album", "Artist", None);
        tag_ripped_file(&path, &rel, &mb_track("T", 1), 1, Some(1), None, None).unwrap();
        tag_ripped_fingerprint(&path, "AQADtIqYRYmS_AeOJUuOK0d6_FcOpcePZkePI8eRJD8q5FdyZP9hHB-OH_2P_DhxJEdy_DhyHEdy_NCPI9eR/zhxnEcePOmRH8mPHzmS_ChyHEdy_PiP/8jx48iRHTny47i").unwrap();

        assert!(
            read_unknown(&path, "ACOUSTID_FINGERPRINT").is_some_and(|v| v.starts_with("AQAD")),
            "ACOUSTID_FINGERPRINT tag should be present and carry the written value"
        );
        // Pre-existing MB tags must still be there — the fingerprint write
        // re-opens the file and we should not be wiping prior items.
        assert_eq!(
            read_text(&path, &ItemKey::MusicBrainzTrackId).as_deref(),
            Some("trk-1")
        );
    }

    #[test]
    fn tag_ripped_fingerprint_round_trips_through_read_embedded_fingerprint() {
        // Exercises the same read path the library scanner uses
        // (`crate::fingerprint::read_embedded_fingerprint`) so a future
        // tag-name change can't silently disconnect rip-time writes from
        // scan-time reads.
        let path = write_test_wav("acoustid-fp-roundtrip");
        let sentinel = "AQADxIqIRYmS_OdwPDmS40iOoz/+L0fy40jy48hxJD-OI8mPI8eRJD-S/PiR48iRJD-OJEeOIz9-_DiSHEny4z_-Iz-OI_lxJMmRHEny4zh-HMmR_DiOI8eR/Mhx5Eh-HEmO5EeSHzmS_DiOI8mPHEmO5EeOJD_-IzmO5EhyJDmS_MiR_EiOI8mRHMmRJMmRHDmS5MeP/EhyHMmPI8mRHEmOJEeS5EeS5EeS5MePI0eOH8mPJEdy5EiSH8mP/Eh-HEmS5EeSHzmS_EhyHEmO5MiR5MeRJD_-4ziSH8mRJD-O5EeS5EceyZH8KI4cyY8jSY7kyHEkR_LjOJIcyZHkSI4cyZEkRw4=";
        tag_ripped_fingerprint(&path, sentinel).unwrap();

        let read_back = crate::fingerprint::read_embedded_fingerprint(&path)
            .expect("scanner should find the embedded fingerprint");
        assert_eq!(read_back, sentinel);
    }

    #[test]
    fn tag_ripped_fingerprint_skips_empty_input() {
        // Nothing should be written when the fingerprint string is empty;
        // protects against a `compute_fingerprint() -> Some("")` slip in
        // the caller from accidentally clearing a previously-written tag.
        let path = write_test_wav("acoustid-fp-empty");
        tag_ripped_fingerprint(&path, "").unwrap();
        assert!(read_unknown(&path, "ACOUSTID_FINGERPRINT").is_none());
    }

    #[test]
    fn tag_ripped_fingerprint_end_to_end_with_real_audio() {
        // Sanity check the full rip-time path: compute a real fingerprint
        // from a synthetic WAV via the same module the worker uses, write
        // it, and confirm a non-empty value lands in the tag. 10 s of
        // audio because Chromaprint's Test2 needs enough signal to emit
        // hashes (the 1 s WAVs used by other tests in this file return
        // `None` from compute_fingerprint). Slow but worth covering to
        // catch shape mismatches between `compute_fingerprint` and
        // `tag_ripped_fingerprint`.
        let dir = fresh_dir("acoustid-fp-end-to-end");
        let path = dir.join("audio.wav");
        write_sine_wav(&path, 10);

        let fp = crate::fingerprint::compute_fingerprint(&path)
            .expect("sine wav fingerprints to a non-empty string");
        assert!(!fp.is_empty());
        tag_ripped_fingerprint(&path, &fp).unwrap();

        let read_back = crate::fingerprint::read_embedded_fingerprint(&path)
            .expect("read_embedded_fingerprint should find what we just wrote");
        assert_eq!(read_back, fp);
    }
}
