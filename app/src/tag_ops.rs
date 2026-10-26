//! Build and apply tag/filename diffs against library files.
//!
//! Pure module — no TUI dependency. Takes a slice of library `Track`s plus
//! a MusicBrainz `Release`, computes a `ReleaseTagDiff` describing every
//! field that would change, and applies it via lofty + `std::fs::rename`.
//!
//! Powers the `m` "tag manager" overlay in the TUI: the overlay surfaces
//! the diff for user toggle / approval, then routes the approved
//! `ReleaseTagDiff` through the background worker which calls
//! [`apply_and_record_moves`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::ItemKey;
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagType};

use crate::cd::metadata::{
    probe_by_content, ripped_track_destination, save_tag, set_string, set_unknown_string,
};
use crate::library::Track;
use crate::musicbrainz::{
    canonical_album_artist, render_artist_credit, Medium, Release, Track as MbTrack,
};

/// Tag-side values the library `Track` schema doesn't model — but the diff
/// still wants to surface. Read directly from the audio file once per
/// diff-build so the `current` side reflects what's actually on disk.
///
/// Without this, every Picard TXXX field (MUSICBRAINZ_*, RELEASECOUNTRY,
/// SCRIPT) and the OriginalMediaType frame would perpetually show as a
/// delta — even immediately after the user applied them — because the
/// library's `Track` has no slot for them and the diff comparator was
/// hardcoding `current: None`. Reread-aware: lofty reopens the file, so a
/// freshly-written tag is visible on the next diff build.
#[derive(Default)]
struct OnDiskExtras {
    media: Option<String>,
    musicbrainz_album_type: Option<String>,
    musicbrainz_album_status: Option<String>,
    musicbrainz_album_packaging: Option<String>,
    release_country: Option<String>,
    script: Option<String>,
    /// Picard's `ACOUSTID_FINGERPRINT` TXXX value, if present. Read here so
    /// the ACOUSTID_FINGERPRINT row doesn't pay a second lofty probe of the
    /// same file — `fingerprint::read_embedded_fingerprint` does its own
    /// `Probe::open` + `read()`, and on a 20-track release that doubled
    /// the file-open count for no benefit.
    acoustid_fingerprint: Option<String>,
    /// The AcoustID track UUID (Picard's `ACOUSTID_ID`), so a file that
    /// already carries it does not show the row as a change on every open.
    acoustid_id: Option<String>,
}

fn read_on_disk_extras(path: &Path) -> OnDiskExtras {
    // `read_properties(false)` skips lofty's audio-properties parse,
    // which scans frames across the whole file for VBR MP3 / FLAC duration
    // estimates. The diff-build path only needs tag items, not bitrate /
    // sample-rate / etc. — disabling the parse drops per-track cost from
    // tens of milliseconds to sub-millisecond on a typical library and is
    // the difference between an instant `m` and a noticeable stall on a
    // 20-track album. Mirrors `fingerprint::read_embedded_fingerprint`.
    let Ok(probe) = Probe::open(path) else {
        return OnDiskExtras::default();
    };
    let Ok(probe) = probe
        .options(ParseOptions::new().read_properties(false))
        .guess_file_type()
    else {
        return OnDiskExtras::default();
    };
    let Ok(tagged) = probe.read() else {
        return OnDiskExtras::default();
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return OnDiskExtras::default();
    };
    extras_from_tag(tag)
}

/// Pull the Picard-style fields out of one tag, whatever the container
/// spelled them as (see [`crate::picard_keys`]).
fn extras_from_tag(tag: &Tag) -> OnDiskExtras {
    use crate::picard_keys::PicardField;
    let media = tag
        .get_string(&ItemKey::OriginalMediaType)
        .map(|s| s.to_string());
    // lofty knows SCRIPT in every container; older files may still carry
    // it under an unknown key.
    let mut script = tag.get_string(&ItemKey::Script).map(|s| s.to_string());
    let mut musicbrainz_album_type = None;
    let mut musicbrainz_album_status = None;
    let mut musicbrainz_album_packaging = None;
    let mut release_country = None;
    let mut acoustid_fingerprint = None;
    let mut acoustid_id = None;
    for item in tag.items() {
        let ItemKey::Unknown(name) = item.key() else {
            continue;
        };
        let ItemValue::Text(value) = item.value() else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let slot = if PicardField::AlbumType.matches(name) {
            &mut musicbrainz_album_type
        } else if PicardField::AlbumStatus.matches(name) {
            &mut musicbrainz_album_status
        } else if PicardField::AlbumPackaging.matches(name) {
            &mut musicbrainz_album_packaging
        } else if PicardField::ReleaseCountry.matches(name) {
            &mut release_country
        } else if crate::picard_keys::strip_freeform_prefix(name).eq_ignore_ascii_case("SCRIPT") {
            &mut script
        } else if PicardField::AcoustidFingerprint.matches(name) {
            &mut acoustid_fingerprint
        } else if PicardField::AcoustidId.matches(name) {
            &mut acoustid_id
        } else {
            continue;
        };
        if slot.is_none() {
            *slot = Some(value.to_string());
        }
    }
    OnDiskExtras {
        media,
        musicbrainz_album_type,
        musicbrainz_album_status,
        musicbrainz_album_packaging,
        release_country,
        script,
        acoustid_fingerprint,
        acoustid_id,
    }
}

/// Whether the diff covers an entire release or a single track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    Album,
    Track,
}

/// Coarse grouping for UI sectioning. `Filename` is special — its `proposed`
/// is the new on-disk path, and applying flips through the rename code path
/// instead of the lofty write path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Identity,
    Numbering,
    Date,
    MbId,
    Identifier,
    Picard,
    Filename,
}

/// One row in the diff table.
#[derive(Debug, Clone)]
pub struct FieldDiff {
    pub kind: FieldKind,
    pub name: &'static str,
    pub current: Option<String>,
    pub proposed: Option<String>,
    pub enabled: bool,
}

/// Per-track diff, scoped to one library file.
#[derive(Debug, Clone)]
pub struct TrackTagDiff {
    pub src_path: PathBuf,
    /// Set when the track's filename would change after applying the rename
    /// implied by the proposed title / track number. `None` when the
    /// existing filename already matches the target.
    pub dest_path: Option<PathBuf>,
    /// Library track id, surfaced so the TUI can pair diffs with the
    /// rendered track-list rows without re-walking the library.
    pub library_id: u64,
    pub fields: Vec<FieldDiff>,
}

/// Top-level diff for an album or track.
#[derive(Debug, Clone)]
pub struct ReleaseTagDiff {
    pub release_mbid: String,
    /// Single-line label rendered in the overlay title bar.
    /// e.g. `"Abbey Road — The Beatles (1969 GB)"`.
    pub summary: String,
    pub tracks: Vec<TrackTagDiff>,
}

impl TrackTagDiff {
    /// The apply will try to move this file: a rename is proposed and its
    /// Filename row is still on.
    pub fn wants_rename(&self) -> bool {
        self.dest_path.is_some()
            && self
                .fields
                .iter()
                .any(|f| f.kind == FieldKind::Filename && f.enabled)
    }
}

impl ReleaseTagDiff {
    /// Returns `true` if at least one enabled field across all tracks would
    /// change something. Used by the overlay to gate the Enter→Apply branch.
    pub fn has_any_enabled(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.fields.iter().any(|f| f.enabled))
    }

    /// Any field whose on-disk value differs from MusicBrainz, whether or
    /// not the user left it enabled. False means the tags already match.
    pub fn has_any_change(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.fields.iter().any(|f| f.current != f.proposed))
    }
}

/// Build a diff for an album or single track, pairing library tracks to MB
/// tracks by (a) `mb_track_id`, (b) `track_number`, (c) title (case-insensitive).
///
/// `library_tracks` for `DiffScope::Track` should contain exactly one entry;
/// for `DiffScope::Album` it should contain every library track on the album
/// (passing more is harmless — unmatched tracks just don't show up in the diff).
///
/// `music_root` is the on-disk library root (e.g. `~/Music`); used to compute
/// the proposed filename via [`ripped_track_destination`].
pub fn build_release_diff(
    library_tracks: &[Track],
    release: &Release,
    music_root: &Path,
    _scope: DiffScope,
    acoustid_uuid: Option<&str>,
) -> ReleaseTagDiff {
    let summary = release_summary(release);
    let total_discs_on_release = if release.media.is_empty() {
        None
    } else {
        Some(release.media.len() as u32)
    };

    // The destination uses the folder spellings already on disk, the same
    // way the apply does, so a file already in place proposes no rename.
    let mut folders = FolderSpelling::default();
    let mut out: Vec<((u32, u32), TrackTagDiff)> = Vec::new();
    for lib in library_tracks {
        let location = match lib.location.as_deref() {
            Some(p) => PathBuf::from(p),
            None => continue,
        };
        // Pair against every medium so multi-disc releases work. The matched
        // medium carries the track total + disc position the diff needs.
        let Some((medium, mb_track)) = pair_to_mb_track_across_media(lib, &release.media) else {
            continue;
        };
        let total_tracks_on_medium = medium.track_count.or(Some(medium.tracks.len() as u32));
        let Some(position) = mb_track.position else {
            // Without a position MB can't tell us where on the medium this
            // track lives — skip rather than fabricate a `00 - Title.ext`
            // filename or a 0-valued `Track #` tag.
            continue;
        };
        let extension = location
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let proposed_filename = if extension.is_empty() {
            None
        } else {
            Some(folders.adopt(
                &ripped_track_destination(music_root, release, mb_track, position, &extension),
                music_root,
            ))
        };

        let fields = build_track_fields(
            lib,
            release,
            mb_track,
            position,
            total_tracks_on_medium,
            Some(medium),
            total_discs_on_release,
            proposed_filename.as_deref(),
            &location,
            acoustid_uuid,
        );

        let dest_path = proposed_filename.filter(|p| p != &location);

        out.push((
            (medium.position.unwrap_or(0), position),
            TrackTagDiff {
                src_path: location,
                dest_path,
                library_id: lib.id,
                fields,
            },
        ));
    }
    // The library hands tracks over in hash order; list them the way the
    // release does (disc, then track), and keep that stable for ties.
    out.sort_by_key(|(order, _)| *order);
    let out: Vec<TrackTagDiff> = out.into_iter().map(|(_, t)| t).collect();

    ReleaseTagDiff {
        release_mbid: release.id.clone(),
        summary,
        tracks: out,
    }
}

/// Pick the highest-voted genre name across release-group and release-level
/// genres. Returns `None` if neither carries any. Picard's tagger applies
/// the same precedence (release-group > release).
fn pick_top_genre(release: &Release) -> Option<String> {
    let from_rg = release
        .release_group
        .as_ref()
        .and_then(|rg| rg.genres.iter().max_by_key(|g| g.count));
    let from_rel = release.genres.iter().max_by_key(|g| g.count);
    match (from_rg, from_rel) {
        (Some(rg), Some(rel)) if rg.count >= rel.count => Some(rg.name.clone()),
        (_, Some(rel)) => Some(rel.name.clone()),
        (Some(rg), None) => Some(rg.name.clone()),
        (None, None) => None,
    }
}

fn release_summary(release: &Release) -> String {
    let artist = render_artist_credit(&release.artist_credit);
    let year = release
        .date
        .as_deref()
        .and_then(|d| d.get(..4))
        .unwrap_or("");
    let country = release.country.as_deref().unwrap_or("");
    match (year.is_empty(), country.is_empty()) {
        (true, true) => format!("{} — {}", release.title, artist),
        (true, false) => format!("{} — {} ({country})", release.title, artist),
        (false, true) => format!("{} — {} ({year})", release.title, artist),
        (false, false) => format!("{} — {} ({year} {country})", release.title, artist),
    }
}

/// Pair one library track to a (medium, track) across every medium on the
/// release. Multi-disc releases require this — pairing against a single
/// medium drops tracks on the other discs.
///
/// Strategy order (each step short-circuits, prefers the matching disc if
/// the library track carries `disc_number`):
///   1. `mb_track_id` equality (when both sides carry one).
///   2. `disc_number` + `track_number` equality.
///   3. `track_number` equality on any medium (fallback when the library
///      track doesn't carry a disc number). When more than one medium has
///      that track number, a title match (step 4) is preferred.
///   4. Title equality, case-insensitive — across every medium.
fn pair_to_mb_track_across_media<'a>(
    lib: &Track,
    media: &'a [Medium],
) -> Option<(&'a Medium, &'a MbTrack)> {
    // 1. MBID hit.
    if let Some(mb_track_id) = lib.mb_track_id.as_deref() {
        for medium in media {
            if let Some(m) = medium.tracks.iter().find(|m| m.id == mb_track_id) {
                return Some((medium, m));
            }
        }
    }
    // 2. Disc + track number hit (only when both are present on the lib side).
    if let (Some(disc), Some(n)) = (lib.disc_number, lib.track_number) {
        for medium in media {
            if medium.position == Some(disc) {
                if let Some(m) = medium.tracks.iter().find(|m| m.position == Some(n)) {
                    return Some((medium, m));
                }
            }
        }
    }
    let by_title = || {
        media.iter().find_map(|medium| {
            medium
                .tracks
                .iter()
                .find(|m| m.title.eq_ignore_ascii_case(&lib.name))
                .map(|m| (medium, m))
        })
    };
    // 3. Track-number fallback, any medium. On a multi-disc release every
    // disc has a track N, so without a disc number the title decides
    // first: taking the first disc's track N filed disc 2 track 1 as (and
    // on top of) disc 1 track 1. The first hit stands only when no title
    // matches either.
    if let Some(n) = lib.track_number {
        let mut hits = media.iter().filter_map(|medium| {
            medium
                .tracks
                .iter()
                .find(|m| m.position == Some(n))
                .map(|m| (medium, m))
        });
        if let Some(first) = hits.next() {
            if hits.next().is_none() {
                return Some(first);
            }
            return by_title().or(Some(first));
        }
    }
    // 4. Title fallback.
    by_title()
}

#[allow(clippy::too_many_arguments)]
fn build_track_fields(
    lib: &Track,
    release: &Release,
    mb_track: &MbTrack,
    position: u32,
    total_tracks_on_release: Option<u32>,
    medium: Option<&Medium>,
    total_discs_on_release: Option<u32>,
    proposed_filename: Option<&Path>,
    src_path: &Path,
    acoustid_uuid: Option<&str>,
) -> Vec<FieldDiff> {
    let mut fields: Vec<FieldDiff> = Vec::new();
    // Read on-disk values for tag fields the library `Track` schema doesn't
    // carry (Picard TXXX, Media). Done once per track so the `current` side
    // of the diff stays accurate after an apply — without this, those rows
    // would re-appear as deltas on every `m` press regardless of what the
    // file actually holds.
    let extras = read_on_disk_extras(src_path);
    let mut push =
        |kind: FieldKind, name: &'static str, current: Option<String>, proposed: Option<String>| {
            // Treat empty strings as None for diff purposes so a "" → None
            // proposed value doesn't appear as a no-op change.
            let cur = current.filter(|s| !s.is_empty());
            let prop = proposed.filter(|s| !s.is_empty());
            // Skip rows where MB has nothing AND the file has nothing — they
            // carry no information and would just bloat the diff. Every
            // other case (changed, or unchanged-but-present-on-at-least-one-
            // side) is surfaced so the user can audit the full tag set,
            // including fields that MB has no opinion on.
            if cur.is_none() && prop.is_none() {
                return;
            }
            // Picard-style "don't blank what MB has no opinion on": when the
            // file has a value and MB doesn't, propose to KEEP the existing
            // value (proposed = current). Without this, the row would show
            // "<existing> → (empty)" and default to enabled, which looks
            // like we're about to clear the tag — alarming for tags like
            // Genre / BPM that the user curates themselves. `set_string`
            // already skips on empty value, so the file wasn't actually
            // being blanked, but the visual delta was wrong and a future
            // tweak to set_string could turn the lie into a real bug.
            //
            // Consequence: this closure cannot express an explicit "MB
            // proposes blanking this tag". If a future caller needs that,
            // it must push the `FieldDiff` directly rather than route
            // through `push`.
            let prop = if prop.is_none() && cur.is_some() {
                cur.clone()
            } else {
                prop
            };
            let enabled = cur != prop;
            fields.push(FieldDiff {
                kind,
                name,
                current: cur,
                proposed: prop,
                enabled,
            });
        };

    // -- Identity --
    push(
        FieldKind::Identity,
        "Title",
        Some(lib.name.clone()),
        Some(mb_track.title.clone()),
    );
    let track_artist = render_artist_credit(&mb_track.artist_credit);
    push(
        FieldKind::Identity,
        "Artist",
        Some(lib.artist.clone()),
        Some(track_artist),
    );
    push(
        FieldKind::Identity,
        "Album",
        Some(lib.album.clone()),
        Some(release.title.clone()),
    );
    let album_artist = canonical_album_artist(&release.artist_credit);
    push(
        FieldKind::Identity,
        "Album Artist",
        lib.album_artist.clone(),
        Some(album_artist),
    );

    // -- Numbering --
    push(
        FieldKind::Numbering,
        "Track #",
        lib.track_number.map(|n| n.to_string()),
        Some(position.to_string()),
    );
    push(
        FieldKind::Numbering,
        "Track Total",
        lib.track_total.map(|n| n.to_string()),
        total_tracks_on_release.map(|n| n.to_string()),
    );
    let medium_pos = medium.and_then(|m| m.position);
    push(
        FieldKind::Numbering,
        "Disc #",
        lib.disc_number.map(|n| n.to_string()),
        medium_pos.map(|n| n.to_string()),
    );
    push(
        FieldKind::Numbering,
        "Disc Total",
        lib.disc_total.map(|n| n.to_string()),
        total_discs_on_release.map(|n| n.to_string()),
    );

    // -- Date --
    push(
        FieldKind::Date,
        "Year",
        // The lib `Track::year` is just the 4-digit prefix; comparing against
        // the MB full date would always look like a diff. Format both sides
        // so we only flag a real change. Field renamed from "Release Date"
        // to "Year" so the label matches what's actually being compared.
        lib.year.map(|y| y.to_string()),
        release
            .date
            .as_deref()
            .and_then(|d| d.get(..4))
            .map(String::from),
    );
    push(
        FieldKind::Date,
        "Original Release",
        lib.original_release_date.clone(),
        release
            .release_group
            .as_ref()
            .and_then(|rg| rg.first_release_date.clone()),
    );

    // -- Classification --
    // Genre comes from MB tags/genres. Prefer release-group genres
    // (aggregated across all releases in the group) and fall back to
    // release-level. Pick the most-voted as the single value to write —
    // Picard does the same. The library's `genre` is the single string
    // tag set in the file, so a single string is the right comparison.
    let mb_genre = pick_top_genre(release);
    push(FieldKind::Identity, "Genre", lib.genre.clone(), mb_genre);

    // Album Type (Picard MUSICBRAINZ_ALBUMTYPE): "album" / "single" / "ep".
    // MusicBrainz capitalises these; Picard stores them lowercase, and a
    // Picard-tagged file must not show "single" → "Single" as a change.
    let album_type = release
        .release_group
        .as_ref()
        .and_then(|rg| rg.primary_type.as_deref())
        .map(str::to_lowercase);
    push(
        FieldKind::Picard,
        "MUSICBRAINZ_ALBUMTYPE",
        extras.musicbrainz_album_type.clone(),
        album_type,
    );

    // Media format ("CD", "12\" Vinyl"). Picard writes to ID3v2's
    // OriginalMediaType frame, which lofty exposes as ItemKey::OriginalMediaType.
    let media = medium.and_then(|m| m.format.clone());
    push(FieldKind::Identifier, "Media", extras.media.clone(), media);

    // Release-level Picard TXXX fields.
    push(
        FieldKind::Picard,
        "MUSICBRAINZ_ALBUMSTATUS",
        extras.musicbrainz_album_status.clone(),
        release.status.as_deref().map(str::to_lowercase),
    );
    push(
        FieldKind::Picard,
        "MUSICBRAINZ_ALBUMPACKAGING",
        extras.musicbrainz_album_packaging.clone(),
        release.packaging.clone(),
    );
    push(
        FieldKind::Picard,
        "RELEASECOUNTRY",
        extras.release_country.clone(),
        release.country.clone(),
    );
    push(
        FieldKind::Picard,
        "SCRIPT",
        extras.script.clone(),
        release
            .text_representation
            .as_ref()
            .and_then(|tr| tr.script.clone()),
    );

    // -- MBIDs --
    push(
        FieldKind::MbId,
        "MB Track ID",
        lib.mb_track_id.clone(),
        Some(mb_track.id.clone()),
    );
    push(
        FieldKind::MbId,
        "MB Recording ID",
        lib.mb_recording_id.clone(),
        mb_track.recording.as_ref().map(|r| r.id.clone()),
    );
    push(
        FieldKind::MbId,
        "MB Release ID",
        lib.mb_release_id.clone(),
        Some(release.id.clone()),
    );
    push(
        FieldKind::MbId,
        "MB Release Group ID",
        lib.mb_release_group_id.clone(),
        release.release_group.as_ref().map(|rg| rg.id.clone()),
    );
    let release_artist_id = release
        .artist_credit
        .first()
        .and_then(|ac| ac.artist.as_ref())
        .map(|a| a.id.clone());
    push(
        FieldKind::MbId,
        "MB Release Artist ID",
        lib.mb_release_artist_id.clone(),
        release_artist_id,
    );
    let track_artist_id = mb_track
        .artist_credit
        .first()
        .and_then(|ac| ac.artist.as_ref())
        .map(|a| a.id.clone());
    push(
        FieldKind::MbId,
        "MB Artist ID",
        lib.mb_artist_id.clone(),
        track_artist_id,
    );

    // -- Identifiers --
    let isrc = mb_track
        .recording
        .as_ref()
        .and_then(|r| r.isrcs.first().cloned());
    push(FieldKind::Identifier, "ISRC", lib.isrc.clone(), isrc);
    push(
        FieldKind::Identifier,
        "Barcode",
        lib.barcode.clone(),
        release.barcode.clone(),
    );
    let li = release.label_info.first();
    push(
        FieldKind::Identifier,
        "Catalog #",
        lib.catalog_number.clone(),
        li.and_then(|l| l.catalog_number.clone()),
    );
    push(
        FieldKind::Identifier,
        "Label",
        lib.publisher.clone(),
        li.and_then(|l| l.label.as_ref().map(|lb| lb.name.clone())),
    );
    push(
        FieldKind::Identifier,
        "Language",
        lib.language.clone(),
        release
            .text_representation
            .as_ref()
            .and_then(|tr| tr.language.clone()),
    );

    // -- AcoustID tags --
    //
    // Both are pushed directly, not through `push`, which would mark them
    // as release values: they come from this file's audio, and must not
    // be written to another copy kept in its place.
    //
    // ACOUSTID_ID is the parent AcoustID UUID — only available when
    // we resolved via the AcoustID fingerprint path. Skipped when the
    // overlay reached this point through MBID-direct or MB-search. It was
    // looked up from this file's fingerprint.
    if let Some(uuid) = acoustid_uuid.filter(|s| !s.is_empty()) {
        let on_disk = extras.acoustid_id.clone().filter(|s| !s.is_empty());
        let enabled = on_disk.as_deref() != Some(uuid);
        fields.push(FieldDiff {
            kind: FieldKind::Picard,
            name: "ACOUSTID_ID",
            current: on_disk,
            proposed: Some(uuid.to_string()),
            enabled,
        });
    }

    // ACOUSTID_FINGERPRINT is sourced entirely from the library track's
    // `acoustic_id` — it's an audio-derived constant, not a release-side
    // value. The library field gets populated by either a tag read OR a
    // fresh compute, so we have to ask the file directly whether the tag
    // is already on disk: if it matches, pushing the row would trigger
    // an unnecessary full lofty rewrite for a no-op.
    if let Some(fp) = lib.acoustic_id.as_deref().filter(|s| !s.is_empty()) {
        // Sourced from the same probe as `extras` above — `read_on_disk_extras`
        // matches the same Picard TXXX names as `fingerprint::read_embedded_fingerprint`.
        let on_disk = extras.acoustid_fingerprint.clone();
        let enabled = on_disk.as_deref() != Some(fp);
        fields.push(FieldDiff {
            kind: FieldKind::Picard,
            name: "ACOUSTID_FINGERPRINT",
            current: on_disk,
            proposed: Some(fp.to_string()),
            enabled,
        });
    }

    // -- Filename rename --
    if let Some(dest) = proposed_filename {
        if dest != src_path {
            fields.push(FieldDiff {
                kind: FieldKind::Filename,
                name: "Filename",
                current: Some(src_path.display().to_string()),
                proposed: Some(dest.display().to_string()),
                enabled: true,
            });
        }
    }

    // Float deltas to the top of each track's field list while preserving
    // the original kind-grouped ordering within each subset. Without this,
    // a track with one Title change buried after 25 unchanged identifiers
    // would force the user to scroll past the noise to find the action.
    // Stable sort keeps the carefully-ordered Identity / Numbering / Date /
    // MbId / Identifier / Picard / Filename sequence intact within both
    // halves of the partition.
    fields.sort_by_key(|f| u8::from(!f.enabled));

    fields
}

/// What [`apply_and_record_moves`] did on disk.
#[derive(Debug, Clone, Default)]
pub struct ApplyOutcome {
    /// One entry per diff track, in diff order.
    pub results: Vec<Result<(), String>>,
    /// Old path → new path for every file that moved. Successes only. The
    /// new path is where the file really is, which is a numbered sibling
    /// of the diff's dest when that dest was already held.
    pub rename_map: HashMap<PathBuf, PathBuf>,
    /// Sidecar files (cover art, cue sheets, scans) that could not follow
    /// the audio out of a folder, one message each. The file is still
    /// where it was, and so is its folder.
    pub sidecar_errors: Vec<String>,
    /// The whole diff was refused before anything was written or moved
    /// (the track-ID registry could not be used): every result is `Err`,
    /// the disk is as it was, and there is nothing to re-read.
    pub refused: bool,
    /// The files moved, and then the track-ID registry could not be
    /// saved (it could when the apply began): why. The moved tracks come
    /// back as new ones, so this has to reach the user.
    pub ids_unsaved: Option<String>,
}

impl ApplyOutcome {
    /// Paths the library cache must re-read after this apply: every new
    /// location, plus every source that was retagged without being
    /// renamed, named where it is now. Moved sources are not listed; see
    /// [`Self::vacated`].
    ///
    /// A source still sitting in the inbox is left out. It was not filed
    /// (rename failed, Filename row off, no MusicBrainz pairing), and the
    /// inbox is outside `library_root`, so re-reading it would add a
    /// non-library file to the library.
    pub fn reread_paths(&self, diff: &ReleaseTagDiff, library_root: &Path) -> Vec<PathBuf> {
        let inbox = crate::library_layout::default_inbox_dir(library_root);
        let in_inbox = |src: &PathBuf| inbox.as_deref().is_some_and(|root| src.starts_with(root));
        let mut paths: Vec<PathBuf> = diff
            .tracks
            .iter()
            .map(|t| &t.src_path)
            .filter(|src| !self.rename_map.contains_key(*src) && !in_inbox(src))
            .cloned()
            .collect();
        paths.extend(self.rename_map.values().cloned());
        paths
    }

    /// Old paths of the files that moved. The cache drops these by key: a
    /// case-folding volume still resolves the old spelling, so "does the
    /// file exist" cannot tell a vacated path from a live one.
    pub fn vacated(&self) -> Vec<PathBuf> {
        self.rename_map.keys().cloned().collect()
    }
}

/// Apply an approved `ReleaseTagDiff` to disk.
///
/// Two phases:
///   1. **Tag writes** — open each `src_path` via lofty and apply every
///      enabled non-`Filename` field.
///   2. **Moves** — place each renamed file at its dest, or beside it
///      (see below).
///
/// An apply never replaces, overwrites, or removes a file. A file bound
/// for a dest that another file already holds, in the diff or not, lands
/// beside it under the first free numbered name (`01 - Song (2).flac`),
/// and `rename_map` records that path. Two tracks bound for one dest are
/// filed in diff order, the first under the canonical name. Whether the
/// numbered file is really a copy of the one it sits beside is not
/// asked: finding and resolving those is a separate step, after filing.
/// A numbered copy filed again stays where it is while the canonical
/// name is held, and takes it once it is free.
///
/// A dest held by a batch track that is itself moving is never taken
/// early; the move waits for it to move out and fails if it never does.
///
/// Callers feed the [`ApplyOutcome`] into
/// [`crate::dirlib::DirectoryLibrary::reread_paths`] so the cache picks up
/// new tags AND new locations.
///
/// `library_root` is the fence for empty-folder cleanup: parents are
/// removed only while they sit strictly inside the library or the sibling
/// inbox. The library root, the inbox folder, and anything above either
/// are left in place.
pub(crate) fn apply_release_diff(diff: &ReleaseTagDiff, library_root: &Path) -> ApplyOutcome {
    let mut results: Vec<Result<(), String>> = vec![Ok(()); diff.tracks.len()];
    let mut rename_map: HashMap<PathBuf, PathBuf> = HashMap::new();

    // Index tracks-with-rename by src_path. The collision check works in src
    // space because that's what `results` is keyed on. Each dest is respelled
    // to the folders already on disk first, so everything below compares the
    // paths the files will really have.
    let mut folders = FolderSpelling::default();
    let renaming: Vec<(usize, &PathBuf, PathBuf)> = diff
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.wants_rename())
        .filter_map(|(i, t)| {
            Some((
                i,
                &t.src_path,
                folders.adopt(t.dest_path.as_ref()?, library_root),
            ))
        })
        .collect();

    let source_set: std::collections::HashSet<&PathBuf> =
        renaming.iter().map(|(_, s, _)| *s).collect();

    // Phase 1: tag writes.
    for (i, track) in diff.tracks.iter().enumerate() {
        if let Err(e) = write_track_tags(track) {
            results[i] = Err(e);
        }
    }

    // Phase 2: renames. Tag-write failures don't block the rename — the file
    // still moves with whatever tags it has. The reverse direction (skip
    // rename on tag-write failure) would leave the file in the wrong
    // canonical location AND with old tags, which is the worse failure mode.
    //
    // A dest that is another batch track's source is free only once that
    // track has moved out, so such a rename is deferred and retried after
    // the others. Whatever is still blocked when a pass moves nothing (the
    // occupant's own rename failed or was skipped, or two tracks trade
    // places) fails and stays where it is.
    let mut pending = renaming;
    loop {
        let mut blocked: Vec<(usize, &PathBuf, PathBuf)> = Vec::new();
        let mut progressed = false;
        for (idx, src, dest) in pending {
            if source_set.contains(&dest) && dest_is_held(src, &dest) {
                blocked.push((idx, src, dest));
                continue;
            }
            progressed = true;
            match perform_rename(src, &dest) {
                // A numbered copy that is already beside its held dest
                // lands where it was: nothing moved, nothing to record.
                Ok(landed) if landed == *src => {}
                Ok(landed) => {
                    rename_map.insert(src.clone(), landed);
                    if let Some(parent) = src.parent() {
                        prune_empty_parents(parent, library_root);
                    }
                }
                Err(e) => {
                    // The dest folders are made before the move is tried.
                    // Left empty by a file that never arrived, they go
                    // again (as does a dest folder that was already there
                    // and empty: empty folders are not kept anywhere).
                    if let Some(parent) = dest.parent() {
                        prune_empty_parents(parent, library_root);
                    }
                    record_move_error(&mut results[idx], e);
                }
            }
        }
        pending = blocked;
        if pending.is_empty() || !progressed {
            break;
        }
    }
    for (idx, _, dest) in pending {
        record_move_error(
            &mut results[idx],
            format!(
                "rename collision: {} is still held by another track of this album",
                dest.display()
            ),
        );
    }

    let landing_dirs: std::collections::BTreeSet<&Path> =
        rename_map.values().filter_map(|p| p.parent()).collect();
    for dir in landing_dirs {
        sweep_stale_staged(dir);
    }

    let sidecar_errors = carry_sidecars(&rename_map, library_root);

    ApplyOutcome {
        results,
        rename_map,
        sidecar_errors,
        refused: false,
        ids_unsaved: None,
    }
}

/// Extension of the copy a cross-device move stages beside its dest.
const STAGED_EXTENSION: &str = "zytunes-part";

/// Extensions a writer gives a file it has not finished (browsers,
/// rsync, aria2, qBittorrent, our own rip): the writer will rename it
/// when it is done, so it is left where the writer expects it.
const IN_PROGRESS: &[&str] = &[
    STAGED_EXTENSION,
    "part",
    "partial",
    "crdownload",
    "download",
    "tmp",
    "aria2",
    "!qb",
];

fn is_in_progress(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IN_PROGRESS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// Move what is left in each folder the audio moved out of (cover art,
/// cue sheets, scans, a subfolder of them) to where the audio went, so
/// the folder can be removed instead of staying behind with no music in
/// it. Returns one message per file that could not be moved.
///
/// A folder is only cleared once no audio is left anywhere under it: a
/// track that stayed (failed, not in the diff, a numbered copy) keeps its
/// folder and everything in it. Files go to the folder most of the audio
/// landed in. Nothing is overwritten or deleted: a name already taken
/// there gets a numbered sibling, whatever the two files hold.
///
/// Folders above the one the audio left are cleared too, up to the
/// library root or the inbox (neither is ever cleared itself), but only
/// of loose files and only once they hold no folder at all: that carries
/// the cover that sits above `CD1`/`CD2`, without hauling along a sibling
/// folder that was never part of the album. Their files go to the folder
/// at the same depth below the library root on the landing side
/// (`artist.jpg` beside the artist's albums, not inside one; a cover
/// above `CD1` into the album), or into the landing folder itself when
/// the source lay deeper.
fn carry_sidecars(rename_map: &HashMap<PathBuf, PathBuf>, library_root: &Path) -> Vec<String> {
    use std::collections::BTreeMap;
    let inbox = crate::library_layout::default_inbox_dir(library_root);
    // Folder left → folders landed in, counted.
    let mut left: BTreeMap<PathBuf, BTreeMap<PathBuf, usize>> = BTreeMap::new();
    for (src, landed) in rename_map {
        let (Some(from), Some(to)) = (src.parent(), landed.parent()) else {
            continue;
        };
        if from != to {
            *left
                .entry(from.to_path_buf())
                .or_default()
                .entry(to.to_path_buf())
                .or_default() += 1;
        }
    }
    let mut errors = Vec::new();
    // Deepest folder first: a parent sorts before its children, and a
    // parent handled first would haul `CD1/` along whole instead of
    // flattening it into the album the way `CD1` alone is.
    for (from, landings) in left.into_iter().rev() {
        let Some((to, _)) = landings
            .iter()
            .max_by_key(|(dir, count)| (**count, std::cmp::Reverse(*dir)))
        else {
            continue;
        };
        let mut dir = from.as_path();
        let mut levels_up = 0;
        while may_remove_empty_dir(dir, library_root, inbox.as_deref()) {
            if dir.is_dir() {
                if levels_up > 0 && has_subfolder(dir) {
                    break;
                }
                match audio_beneath(dir) {
                    Ok(false) => {}
                    Ok(true) => break,
                    Err(e) => {
                        errors.push(e);
                        break;
                    }
                }
                let target = if levels_up == 0 {
                    to.to_path_buf()
                } else {
                    same_depth(dir, to, library_root)
                };
                move_sidecars(dir, &target, &mut errors);
                prune_empty_parents(dir, library_root);
                if dir.exists() {
                    break;
                }
            }
            levels_up += 1;
            let Some(parent) = dir.parent() else {
                break;
            };
            dir = parent;
        }
    }
    errors
}

/// The ancestor of `to` (or `to` itself) as deep below `library_root` as
/// `dir` is. `to` for a `dir` outside the library (the inbox) or deeper
/// than `to`.
fn same_depth(dir: &Path, to: &Path, library_root: &Path) -> PathBuf {
    let depth = |p: &Path| {
        p.strip_prefix(library_root)
            .map(|rel| rel.components().count())
            .ok()
    };
    match (depth(dir), depth(to)) {
        (Some(d), Some(t)) if d > 0 && d < t => to
            .ancestors()
            .nth(t - d)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| to.to_path_buf()),
        _ => to.to_path_buf(),
    }
}

/// Whether any audio file is anywhere under `dir`; an error names the
/// first folder or entry that could not be read, which leaves the question
/// open. A symlink is neither followed nor counted: what it points at is
/// not this folder's, and a link back up the tree would never end.
fn audio_beneath(dir: &Path) -> Result<bool, String> {
    let unreadable = |e: std::io::Error| format!("read {}: {e}", dir.display());
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let kind = entry.file_type().map_err(unreadable)?;
        if kind.is_dir() {
            if audio_beneath(&entry.path())? {
                return Ok(true);
            }
        } else if kind.is_file() && crate::dirlib::is_audio_file(&entry.path()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `dir` holds a folder (a symlink to one is not a folder here;
/// see [`audio_beneath`]). Anything unreadable counts as yes: the caller
/// uses it to leave a folder alone.
fn has_subfolder(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |entries| {
        entries
            .map(|e| e.and_then(|e| e.file_type()))
            .any(|kind| kind.map_or(true, |kind| kind.is_dir()))
    })
}

/// Move every file under `from` to the same place under `to`. Symlinks
/// are left (a relative one would dangle) and so is a file still being
/// written ([`IN_PROGRESS`]), which keeps `from` in place.
/// Whatever stays, and why, goes on `errors`.
fn move_sidecars(from: &Path, to: &Path, errors: &mut Vec<String>) {
    if same_inode(from, to) {
        return;
    }
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!("read {}: {e}", from.display()));
            return;
        }
    };
    for entry in entries {
        let (path, name, kind) =
            match entry.and_then(|e| Ok((e.path(), e.file_name(), e.file_type()?))) {
                Ok(entry) => entry,
                Err(e) => {
                    errors.push(format!("read {}: {e}", from.display()));
                    continue;
                }
            };
        if kind.is_symlink() {
            errors.push(format!("{} is a symlink; left in place", path.display()));
            continue;
        }
        if is_in_progress(&path) {
            errors.push(format!(
                "{} is still being written; left in place",
                path.display()
            ));
            continue;
        }
        if kind.is_dir() {
            // Anything that stayed in the subfolder is already on
            // `errors`, and the subfolder stays with it.
            let reported = errors.len();
            move_sidecars(&path, &to.join(&name), errors);
            if errors.len() == reported {
                if let Err(e) = std::fs::remove_dir(&path) {
                    errors.push(format!("remove {}: {e}", path.display()));
                }
            }
            continue;
        }
        if let Err(e) = move_to_free_name(&path, &to.join(&name)) {
            errors.push(e);
        }
    }
}

/// Where the file that was at `old` before an apply is now: its own
/// rename if it had one. Unchanged when the apply did not touch it.
pub fn moved_path(old: &Path, rename_map: &HashMap<PathBuf, PathBuf>) -> PathBuf {
    rename_map
        .get(old)
        .cloned()
        .unwrap_or_else(|| old.to_path_buf())
}

/// `apply_release_diff`, then move each moved file's track ID with it
/// ([`crate::track_ids::TrackIds::apply_moves`]).
///
/// This is the entry point for anything that goes on to read the library:
/// the IDs have to follow the files before the next scan or reread sees
/// the new paths, or every moved track comes back as a new one and its
/// playlists, play stats and stem splits are left behind. `music_dir` is
/// the library root exactly as the scan was given it (it names the
/// registry).
///
/// A diff that would move a file is applied inside the registry's
/// read-modify-write, so the registry is known usable (read, and saved
/// once unchanged) before the first move. When it is not, nothing is applied and every track reports
/// why: a file moved without its ID is a new track after the next scan,
/// and there is no putting that back.
pub fn apply_and_record_moves(
    diff: &ReleaseTagDiff,
    music_dir: &str,
    log: &crate::cache::Logger,
) -> ApplyOutcome {
    if !diff.tracks.iter().any(TrackTagDiff::wants_rename) {
        return apply_release_diff(diff, Path::new(music_dir));
    }
    crate::track_ids::with_writable_registry(music_dir, log, |ids| {
        let outcome = apply_release_diff(diff, Path::new(music_dir));
        if !outcome.rename_map.is_empty() {
            ids.apply_moves(
                |old| moved_path(old, &outcome.rename_map),
                |old| outcome.rename_map.contains_key(old),
            );
        }
        outcome
    })
    .map(|(outcome, saved)| ApplyOutcome {
        ids_unsaved: saved.err(),
        ..outcome
    })
    .unwrap_or_else(|why| ApplyOutcome {
        results: vec![Err(format!("not applied: {why}")); diff.tracks.len()],
        refused: true,
        ..Default::default()
    })
}

/// Record why a track did not move. The move error leads: the file is
/// reported as "not moved", and that is the reason for it. A tag-write
/// error from phase 1 is kept after it rather than replaced, since the
/// file is also still untagged.
fn record_move_error(result: &mut Result<(), String>, move_error: String) {
    let combined = match result {
        Ok(()) => move_error,
        Err(tag_error) => format!("{move_error}; its tags were not written either: {tag_error}"),
    };
    *result = Err(combined);
}

fn write_track_tags(track: &TrackTagDiff) -> Result<(), String> {
    write_tags_at(&track.src_path, &enabled_tag_rows(track))
}

/// The tag rows the user left switched on.
fn enabled_tag_rows(track: &TrackTagDiff) -> Vec<&FieldDiff> {
    track
        .fields
        .iter()
        .filter(|f| f.enabled && f.kind != FieldKind::Filename)
        .collect()
}

/// Write `to_apply` to the audio file at `src`.
fn write_tags_at(src: &Path, to_apply: &[&FieldDiff]) -> Result<(), String> {
    if to_apply.is_empty() {
        return Ok(());
    }

    // Atomic save: copy `src` to a sibling `.tagtmp`, let lofty rewrite the
    // tag in place on the temp, fsync, then `rename` over the original.
    // Without this, lofty's `save_to_path` writes in place — a crash, full
    // disk, or SIGKILL mid-write leaves the user's audio file truncated.
    // The temp lives in the same directory so the rename is a same-FS swap
    // (atomic on POSIX); any failure path removes the temp so the original
    // is never touched.
    let tmp = tagtmp_path(src);
    // Clean up a leftover from a prior crash before copying.
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = std::fs::copy(src, &tmp) {
        // A copy that ran out of disk leaves what it wrote so far. The
        // file may be moved next, and nothing would come back for a temp
        // beside its old path.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "tag-save: copy {} -> {}: {e}",
            src.display(),
            tmp.display()
        ));
    }

    let result = write_then_swap(&tmp, src, to_apply);
    if result.is_err() {
        // Leave the original untouched; clean up the temp.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_then_swap(tmp: &Path, src: &Path, to_apply: &[&FieldDiff]) -> Result<(), String> {
    let mut tagged = probe_by_content(tmp)?;
    let tag_type = match tagged.primary_tag_type() {
        TagType::RiffInfo => TagType::Id3v2,
        other => other,
    };
    if tagged.tag(tag_type).is_none() {
        let mut fresh = Tag::new(tag_type);
        // An ID3v1-only MP3. The new ID3v2 becomes the tag every reader
        // prefers (the scanner included), so it starts from what the file
        // already says: holding only the edited rows, it hid the title
        // and artist that were never part of the edit.
        if let Some(v1) = tagged.tag(TagType::Id3v1) {
            for item in v1.items() {
                fresh.insert(item.clone());
            }
        }
        tagged.insert_tag(fresh);
    }
    let tag = tagged
        .tag_mut(tag_type)
        .ok_or_else(|| "lofty refused to attach a tag".to_string())?;
    for field in to_apply {
        apply_field(tag, field);
    }
    let _ = tag;
    save_tag(&tagged, tag_type, tmp).map_err(|e| format!("{e} (source {})", src.display()))?;
    // fsync the temp's contents before swapping it in — otherwise a power
    // loss between rename and writeback can leave a renamed-but-empty file
    // on ext4/xfs.
    if let Ok(f) = std::fs::OpenOptions::new().read(true).open(tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(tmp, src).map_err(|e| {
        format!(
            "tag-save: rename {} -> {}: {e}",
            tmp.display(),
            src.display()
        )
    })?;
    Ok(())
}

/// Sibling temp path for the atomic save: `{src}.tagtmp` (appends, does
/// not replace the extension, so lofty's format probe on the temp still
/// matches the source).
fn tagtmp_path(src: &Path) -> PathBuf {
    let mut name = src
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("tagtmp"));
    name.push(".tagtmp");
    src.with_file_name(name)
}

fn apply_field(tag: &mut Tag, field: &FieldDiff) {
    let value = field.proposed.as_deref().unwrap_or("");
    match (field.kind, field.name) {
        (FieldKind::Identity, "Title") => set_string(tag, ItemKey::TrackTitle, value),
        (FieldKind::Identity, "Artist") => set_string(tag, ItemKey::TrackArtist, value),
        (FieldKind::Identity, "Album") => set_string(tag, ItemKey::AlbumTitle, value),
        (FieldKind::Identity, "Album Artist") => set_string(tag, ItemKey::AlbumArtist, value),
        (FieldKind::Numbering, "Track #") => set_string(tag, ItemKey::TrackNumber, value),
        (FieldKind::Numbering, "Track Total") => set_string(tag, ItemKey::TrackTotal, value),
        (FieldKind::Numbering, "Disc #") => set_string(tag, ItemKey::DiscNumber, value),
        (FieldKind::Numbering, "Disc Total") => set_string(tag, ItemKey::DiscTotal, value),
        (FieldKind::Date, "Year") => set_string(tag, ItemKey::RecordingDate, value),
        (FieldKind::Date, "Original Release") => {
            set_string(tag, ItemKey::OriginalReleaseDate, value);
        }
        (FieldKind::MbId, "MB Track ID") => set_string(tag, ItemKey::MusicBrainzTrackId, value),
        (FieldKind::MbId, "MB Recording ID") => {
            set_string(tag, ItemKey::MusicBrainzRecordingId, value);
        }
        (FieldKind::MbId, "MB Release ID") => set_string(tag, ItemKey::MusicBrainzReleaseId, value),
        (FieldKind::MbId, "MB Release Group ID") => {
            set_string(tag, ItemKey::MusicBrainzReleaseGroupId, value);
        }
        (FieldKind::MbId, "MB Release Artist ID") => {
            set_string(tag, ItemKey::MusicBrainzReleaseArtistId, value);
        }
        (FieldKind::MbId, "MB Artist ID") => set_string(tag, ItemKey::MusicBrainzArtistId, value),
        (FieldKind::Identifier, "ISRC") => set_string(tag, ItemKey::Isrc, value),
        (FieldKind::Identifier, "Barcode") => set_string(tag, ItemKey::Barcode, value),
        (FieldKind::Identifier, "Catalog #") => set_string(tag, ItemKey::CatalogNumber, value),
        (FieldKind::Identifier, "Label") => set_string(tag, ItemKey::Label, value),
        (FieldKind::Identifier, "Language") => set_string(tag, ItemKey::Language, value),
        // Media format ("CD", "12\" Vinyl") — Picard writes to TMED, which
        // lofty exposes as `ItemKey::OriginalMediaType`.
        (FieldKind::Identifier, "Media") => set_string(tag, ItemKey::OriginalMediaType, value),
        // Genre comes through as Identity by intent: it's a top-level field
        // like Title/Artist/Album that the user wants to scan quickly. The
        // tag key is `Genre` so lofty writes it as TCON (ID3v2) / `\xa9gen`
        // (M4A) / `GENRE` (vorbis) — the standard genre frame across formats.
        (FieldKind::Identity, "Genre") => set_string(tag, ItemKey::Genre, value),
        (FieldKind::Picard, name) => set_unknown_string(tag, name, value),
        // Filename is handled separately in phase 2; safety net only.
        (FieldKind::Filename, _) => {}
        _ => {
            // Unmapped (kind, name) — silently skip rather than panicking,
            // so future additions don't crash the apply path.
        }
    }
}

fn prune_empty_parents(start: &Path, library_root: &Path) {
    let inbox = crate::library_layout::default_inbox_dir(library_root);
    let mut dir = start.to_path_buf();
    for _ in 0..8 {
        if !may_remove_empty_dir(&dir, library_root, inbox.as_deref()) {
            break;
        }
        let empty = match std::fs::read_dir(&dir) {
            Ok(mut rd) => rd.next().is_none(),
            Err(_) => break,
        };
        if !empty {
            break;
        }
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
}

/// Empty dirs may be removed only when they are strictly inside the library
/// or strictly inside the inbox. That keeps `music_dir`, the inbox folder,
/// and every ancestor of both.
fn may_remove_empty_dir(dir: &Path, library_root: &Path, inbox: Option<&Path>) -> bool {
    is_strict_child(dir, library_root) || inbox.is_some_and(|root| is_strict_child(dir, root))
}

fn is_strict_child(path: &Path, root: &Path) -> bool {
    path.starts_with(root) && path != root
}

/// `dest` is occupied by a file other than `src` itself, so moving `src`
/// there would overwrite a second copy.
///
/// The identity check is the inode, never the spelling: APFS/HFS+ resolve
/// `Alice in Chains` / `Alice In Chains` and NFD / NFC `Beyoncé` to one
/// entry, so a differently spelled `dest` can BE `src`. That is a respelling,
/// not a second file.
fn dest_is_held(src: &Path, dest: &Path) -> bool {
    dest != src && dest.exists() && !same_inode(src, dest)
}

fn same_inode(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ma.ino() == mb.ino() && ma.dev() == mb.dev()
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        false
    }
}

/// Where a file bound for `dest` actually goes: `dest` when nothing else
/// holds it, otherwise the first free numbered sibling
/// (`01 - Song (2).flac`, `(3)`, …), so no file is ever overwritten.
///
/// A source that already is one of those siblings stays where it is, so
/// filing an album that holds a numbered copy again does not walk the
/// copy up to `(3)`.
///
/// `None` only if every number up to `u32::MAX` is taken.
fn landing_path(src: &Path, dest: &Path) -> Option<PathBuf> {
    if !dest_is_held(src, dest) {
        return Some(dest.to_path_buf());
    }
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest.extension().map(|e| e.to_string_lossy().into_owned());
    (2..=u32::MAX)
        .map(|n| {
            dest.with_file_name(match &ext {
                Some(ext) => format!("{stem} ({n}).{ext}"),
                None => format!("{stem} ({n})"),
            })
        })
        .find_map(|candidate| {
            if candidate == src || same_inode(src, &candidate) {
                Some(src.to_path_buf())
            } else {
                (!candidate.exists()).then_some(candidate)
            }
        })
}

/// Move `src` towards `dest` and return where it landed (see
/// [`landing_path`]; the same path as `src` when it did not need to move).
fn perform_rename(src: &Path, dest: &Path) -> Result<PathBuf, String> {
    // `rename` between two hard links of one file succeeds and does
    // nothing: both names stay, and the move would be reported as done.
    if is_hard_link_pair(src, dest) {
        return Err(format!(
            "{} and {} are hard links to one file; left as they are",
            src.display(),
            dest.display()
        ));
    }
    // One entry under two spellings (a case- or normalisation-folding
    // volume, or a case-only change of the file name): respell it in
    // place. Linking it to a second name would find its own entry there
    // and number it.
    if dest != src && same_inode(src, dest) {
        return std::fs::rename(src, dest)
            .map(|()| dest.to_path_buf())
            .map_err(|e| format!("rename {} -> {}: {e}", src.display(), dest.display()));
    }
    move_to_free_name(src, dest)
}

/// `src` and `dest` are two directory entries for one file (hard links),
/// as opposed to two spellings a case- or normalisation-folding volume
/// resolves to ONE entry. Both have the same inode; only real links are
/// both listed under their exact names.
fn is_hard_link_pair(src: &Path, dest: &Path) -> bool {
    fn listed(path: &Path) -> bool {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        std::fs::read_dir(parent)
            .is_ok_and(|entries| entries.flatten().any(|e| e.file_name() == name))
    }
    if src == dest || !same_inode(src, dest) || !listed(src) || !listed(dest) {
        return false;
    }
    // One entry reached through two spellings of its folder.
    let same_folder = match (src.parent(), dest.parent()) {
        (Some(a), Some(b)) => a == b || same_inode(a, b),
        _ => false,
    };
    !(same_folder && src.file_name() == dest.file_name())
}

/// A relative symlink moved to another folder points at nothing: the
/// track would vanish from where it was and never show up where it went.
fn refuse_relative_symlink(src: &Path, landing: &Path) -> Result<(), String> {
    if src.parent() == landing.parent() {
        return Ok(());
    }
    match std::fs::read_link(src) {
        Ok(target) if target.is_relative() => Err(format!(
            "{} is a relative symlink to {}; file the linked file instead",
            src.display(),
            target.display()
        )),
        _ => Ok(()),
    }
}

/// The folder spellings already on disk, so a file is filed into the
/// folder that is there instead of a second one that differs by case.
/// Folders are never renamed: that would move every album in them, the
/// ones outside the diff included. Answers are kept for the apply, since
/// every track of an album asks about the same folders and the first
/// question costs a listing of the library root (one entry per artist).
#[derive(Default)]
struct FolderSpelling {
    settled: HashMap<PathBuf, std::ffi::OsString>,
    /// The folders in each directory asked about, listed once: the library
    /// root is asked about for every new artist.
    listings: HashMap<PathBuf, Vec<std::ffi::OsString>>,
}

impl FolderSpelling {
    /// `dest` with each folder between `library_root` and the file spelled
    /// the way it already is on disk. A `dest` outside the library is
    /// left alone.
    fn adopt(&mut self, dest: &Path, library_root: &Path) -> PathBuf {
        let (Some(folders), Some(file)) = (
            dest.parent()
                .and_then(|p| p.strip_prefix(library_root).ok()),
            dest.file_name(),
        ) else {
            return dest.to_path_buf();
        };
        let mut built = library_root.to_path_buf();
        for component in folders.components() {
            let name = match component {
                std::path::Component::Normal(want) => self.spell(&built, want),
                other => other.as_os_str().to_owned(),
            };
            built.push(name);
        }
        built.join(file)
    }

    /// How the folder `want` in `dir` is spelled on disk.
    fn spell(&mut self, dir: &Path, want: &std::ffi::OsStr) -> std::ffi::OsString {
        let key = dir.join(want);
        if let Some(name) = self.settled.get(&key) {
            return name.clone();
        }
        let listing = self
            .listings
            .entry(dir.to_path_buf())
            .or_insert_with(|| folders_in(dir));
        let name = sole_case_variant(listing, want).unwrap_or_else(|| want.to_owned());
        // A folder the apply is about to create is as good as listed, or a
        // later track asking for another case of it would make a second.
        if !listing.contains(&name) {
            listing.push(name.clone());
        }
        self.settled.insert(key, name.clone());
        name
    }
}

/// The names of the folders in `dir` (none if it cannot be read).
fn folders_in(dir: &Path) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name())
        .collect()
}

/// The one folder among `folders` whose name is `want` in another ASCII
/// case. `None` when `want` itself is there (the folders are separate
/// entries and the file is moved into the canonical one), when nothing
/// matches, or when several spellings do and none is the obvious one.
fn sole_case_variant(
    folders: &[std::ffi::OsString],
    want: &std::ffi::OsStr,
) -> Option<std::ffi::OsString> {
    let want_text = want.to_string_lossy();
    let mut variant = None;
    for name in folders {
        if name == want {
            return None;
        }
        if name.to_string_lossy().eq_ignore_ascii_case(&want_text) {
            if variant.is_some() {
                return None;
            }
            variant = Some(name.clone());
        }
    }
    variant
}

/// Why a no-replace move did not happen.
enum MoveFailure {
    /// Something already sits at the dest.
    Taken,
    Failed(String),
}

/// Put `src` at `dest`, or at the first free numbered name beside it
/// ([`landing_path`]), and return where it went. Nothing is replaced even
/// if a file appears at the chosen name after it was looked at: the move
/// itself refuses to take an occupied name ([`move_no_replace`]), and the
/// next name is tried.
fn move_to_free_name(src: &Path, dest: &Path) -> Result<PathBuf, String> {
    // A name only stays taken because something keeps winning the race
    // for it; this is a bound on a loop that cannot otherwise end.
    for _ in 0..64 {
        let landing = landing_path(src, dest)
            .ok_or_else(|| format!("no free name for {}", dest.display()))?;
        if landing == src {
            return Ok(landing);
        }
        refuse_relative_symlink(src, &landing)?;
        match move_no_replace(src, &landing) {
            Ok(()) => return Ok(landing),
            Err(MoveFailure::Taken) => {}
            Err(MoveFailure::Failed(e)) => return Err(e),
        }
    }
    Err(format!("no free name for {}", dest.display()))
}

/// Move one file to a dest nothing may be holding, creating the dest
/// folder first. The file is linked at `dest` (which fails if the name
/// is taken) and only then unlinked from `src`, so an existing file is
/// never replaced. Where that cannot be done (another filesystem, one
/// without hard links) the bytes are copied beside `dest` and flushed to
/// disk before `src` is removed.
fn move_no_replace(src: &Path, dest: &Path) -> Result<(), MoveFailure> {
    use std::io::ErrorKind;
    let fail = |what: &str, e: std::io::Error| {
        MoveFailure::Failed(format!(
            "{what} {} -> {}: {e}",
            src.display(),
            dest.display()
        ))
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MoveFailure::Failed(format!("mkdir {}: {e}", parent.display())))?;
    }
    match std::fs::hard_link(src, dest) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(MoveFailure::Taken),
        Err(_) => copy_no_replace(src, dest)?,
    }
    // The file is now at both names. If `src` cannot be removed the move
    // did not happen: take the new name back out, so there is not a
    // second file the library has no record of.
    std::fs::remove_file(src).map_err(|e| {
        let _ = std::fs::remove_file(dest);
        fail("rename cleanup of", e)
    })
}

/// [`move_no_replace`] without hard links: copy to a staged name beside
/// `dest`, flush it, then give it its name only if nobody has it.
fn copy_no_replace(src: &Path, dest: &Path) -> Result<(), MoveFailure> {
    use std::io::ErrorKind;
    let mut staged = dest.as_os_str().to_owned();
    staged.push(".");
    staged.push(STAGED_EXTENSION);
    let staged = PathBuf::from(staged);
    let fail = |e: std::io::Error| {
        let _ = std::fs::remove_file(&staged);
        MoveFailure::Failed(format!(
            "rename {} -> {}: cross-device copy failed: {e}",
            src.display(),
            dest.display()
        ))
    };
    std::fs::copy(src, &staged).map_err(&fail)?;
    // Without this the data can still be in memory when `src` goes, and
    // a power cut leaves a short file under the final name.
    std::fs::File::open(&staged)
        .and_then(|f| f.sync_all())
        .map_err(&fail)?;
    // Link, not rename: a rename would replace a file that turned up
    // while the copy ran. A volume with no hard links at all (FAT) gets
    // the rename, after a last look at the name.
    let named = match std::fs::hard_link(&staged, dest) {
        Ok(()) => std::fs::remove_file(&staged),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&staged);
            return Err(MoveFailure::Taken);
        }
        Err(_) if dest.symlink_metadata().is_ok() => {
            let _ = std::fs::remove_file(&staged);
            return Err(MoveFailure::Taken);
        }
        Err(_) => std::fs::rename(&staged, dest),
    };
    named.map_err(fail)?;
    // `copy` stamps the copy with the time it was made. The stem cache and
    // the library scan fingerprint a file by (mtime, size), so a moved
    // file with a new mtime would be separated and rescanned again. Done
    // after the file has its name: until then the staged name's age is
    // what `sweep_stale_staged` goes by.
    if let Ok(modified) = std::fs::metadata(src).and_then(|m| m.modified()) {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(dest)
            .and_then(|f| f.set_modified(modified));
    }
    Ok(())
}

/// How long a staged copy may sit before it is taken for a crash's
/// leftover rather than a copy still running.
const STAGED_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Remove the staged copies ([`copy_no_replace`]) a crash left in `dir`.
/// A move that died between the copy and the naming leaves
/// `Song.flac.zytunes-part` behind; nothing else would ever look for it.
fn sweep_stale_staged(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = path.extension().is_some_and(|e| e == STAGED_EXTENSION)
            && entry
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STAGED_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Track as LibTrack;
    use crate::musicbrainz::{Artist, ArtistCredit, Medium, Recording, Release, Track as MbTrack};
    use crate::test_audio::write_sine_wav;
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zytunes-tag-ops").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_release(title: &str, artist: &str) -> Release {
        Release {
            id: "rel-1".into(),
            title: title.into(),
            date: Some("1969-09-26".into()),
            country: Some("GB".into()),
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
                track_count: Some(2),
                tracks: vec![
                    MbTrack {
                        id: "trk-1".into(),
                        number: "1".into(),
                        position: Some(1),
                        title: "First".into(),
                        length: Some(100_000),
                        recording: Some(Recording {
                            id: "rec-1".into(),
                            title: "First".into(),
                            length: Some(100_000),
                            isrcs: vec!["USRC11111111".into()],
                            artist_credit: vec![],
                        }),
                        artist_credit: vec![ArtistCredit {
                            name: artist.into(),
                            joinphrase: None,
                            artist: Some(Artist {
                                id: "art-1".into(),
                                name: artist.into(),
                                sort_name: None,
                            }),
                        }],
                    },
                    MbTrack {
                        id: "trk-2".into(),
                        number: "2".into(),
                        position: Some(2),
                        title: "Second".into(),
                        length: Some(200_000),
                        recording: Some(Recording {
                            id: "rec-2".into(),
                            title: "Second".into(),
                            length: Some(200_000),
                            isrcs: vec![],
                            artist_credit: vec![],
                        }),
                        artist_credit: vec![ArtistCredit {
                            name: artist.into(),
                            joinphrase: None,
                            artist: Some(Artist {
                                id: "art-1".into(),
                                name: artist.into(),
                                sort_name: None,
                            }),
                        }],
                    },
                ],
            }],
            release_group: None,
            barcode: None,
            asin: None,
            status: None,
            packaging: None,
            text_representation: None,
            label_info: vec![],
            genres: vec![],
        }
    }

    fn make_lib_track(path: &Path, name: &str, position: u32) -> LibTrack {
        LibTrack {
            id: 1,
            name: name.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: Some(position),
            location: Some(path.to_string_lossy().to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn build_release_diff_includes_unchanged_fields_but_marks_them_disabled() {
        // The diff now carries every field — changed AND unchanged — so the
        // user can audit existing tags rather than only seeing deltas. A
        // library track that already matches MB on Title/Artist/Album/etc.
        // must still surface those rows, but with `enabled=false` so the
        // apply path skips them and the UI can render them as no-ops.
        let dir = fresh_dir("includes-unchanged");
        let path = dir.join("Artist").join("Album").join("01 - First.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        let mut lib = make_lib_track(&path, "First", 1);
        lib.album = "Album".into();
        lib.track_total = Some(2);
        lib.year = Some(1969);
        lib.album_artist = Some("Artist".into());
        lib.mb_track_id = Some("trk-1".into());
        lib.mb_recording_id = Some("rec-1".into());
        lib.mb_release_id = Some("rel-1".into());
        lib.mb_release_artist_id = Some("art-1".into());
        lib.mb_artist_id = Some("art-1".into());
        lib.isrc = Some("USRC11111111".into());
        lib.disc_number = Some(1);
        lib.disc_total = Some(1);

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert_eq!(diff.tracks.len(), 1);
        let track = &diff.tracks[0];

        // Identity fields that already match must be present but disabled.
        for matched in ["Title", "Artist", "Album", "Track #", "Year"] {
            let field = track.fields.iter().find(|f| f.name == matched);
            assert!(
                field.is_some(),
                "expected {matched:?} in diff so the user can see the current value: {:?}",
                track.fields.iter().map(|f| f.name).collect::<Vec<_>>()
            );
            let f = field.unwrap();
            assert!(
                !f.enabled,
                "{matched:?} matches MB but came back enabled — apply path would rewrite a no-op",
            );
            assert_eq!(
                f.current, f.proposed,
                "{matched:?} should have current == proposed when no change",
            );
        }

        // The only fields that should come back enabled are release-level
        // Picard fields the library Track schema doesn't model (Media,
        // RELEASECOUNTRY, etc.) — those legitimately diff from None →
        // Some(...). Library-expressible fields should all be disabled.
        let library_expressible_enabled: Vec<&str> = track
            .fields
            .iter()
            .filter(|f| {
                f.enabled
                    && matches!(
                        f.name,
                        "Title"
                            | "Artist"
                            | "Album"
                            | "Album Artist"
                            | "Track #"
                            | "Track Total"
                            | "Disc #"
                            | "Disc Total"
                            | "Year"
                            | "MB Track ID"
                            | "MB Recording ID"
                            | "MB Release ID"
                            | "MB Release Artist ID"
                            | "MB Artist ID"
                            | "ISRC"
                    )
            })
            .map(|f| f.name)
            .collect();
        assert!(
            library_expressible_enabled.is_empty(),
            "library-expressible fields all match — none should be enabled, got: {library_expressible_enabled:?}",
        );
    }

    #[test]
    fn build_release_diff_floats_changed_fields_to_the_top() {
        // Deltas must come first within a track's field list so the user
        // doesn't have to scroll past matching rows to find the changes.
        let dir = fresh_dir("delta-sort");
        let path = dir.join("Artist").join("Album").join("01 - First.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        // Title and Artist match, but Album differs — the Album row should
        // land ahead of the unchanged Title/Artist rows.
        let mut lib = make_lib_track(&path, "First", 1);
        lib.album = "Stale Album".into();

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let track = &diff.tracks[0];
        let first_enabled = track
            .fields
            .iter()
            .position(|f| f.enabled)
            .expect("at least one delta expected");
        let first_unchanged = track
            .fields
            .iter()
            .position(|f| !f.enabled && f.current == f.proposed)
            .expect("at least one unchanged row expected");
        assert!(
            first_enabled < first_unchanged,
            "changed fields must sort ahead of unchanged ones (enabled={}, unchanged={})",
            first_enabled,
            first_unchanged,
        );
    }

    #[test]
    fn build_release_diff_pairs_by_mb_track_id_first() {
        let dir = fresh_dir("pair-mbid");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        // Position would match track 2, but mb_track_id matches track 1.
        let mut lib = make_lib_track(&path, "Renamed", 2);
        lib.mb_track_id = Some("trk-1".into());

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let title_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Title")
            .expect("Title diff should be present");
        assert_eq!(title_field.proposed.as_deref(), Some("First"));
    }

    #[test]
    fn build_release_diff_pairs_by_position_when_no_mbid() {
        let dir = fresh_dir("pair-position");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "Wrong Name", 2);
        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let title_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Title")
            .expect("Title diff should be present");
        assert_eq!(title_field.proposed.as_deref(), Some("Second"));
    }

    #[test]
    fn build_release_diff_pairs_by_title_when_no_mbid_or_position() {
        let dir = fresh_dir("pair-title");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let mut lib = make_lib_track(&path, "second", 99); // case-insensitive title, position mismatched
        lib.track_number = None;
        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let track_num = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Track #")
            .expect("Track # diff should be present");
        assert_eq!(track_num.proposed.as_deref(), Some("2"));
    }

    #[test]
    fn build_release_diff_includes_rename_when_position_changes() {
        let dir = fresh_dir("rename-pos");
        let path = dir.join("Artist").join("Album").join("99 - first.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        // Library has the wrong track number; MB says it's track 1.
        let mut lib = make_lib_track(&path, "First", 99);
        lib.mb_track_id = Some("trk-1".into());

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        // Track # field changes (99 -> 1) AND a filename diff is present.
        assert!(diff.tracks[0].fields.iter().any(|f| f.name == "Track #"));
        assert!(diff.tracks[0]
            .fields
            .iter()
            .any(|f| f.kind == FieldKind::Filename));
        assert!(diff.tracks[0].dest_path.is_some());
    }

    #[test]
    fn build_release_diff_proposes_no_rename_for_a_file_in_the_spelling_on_disk() {
        // The artist folder is `artist` on disk and the release spells it
        // `Artist`. Filing adopts the folder that is there, so the file
        // already is where the diff would put it.
        let dir = fresh_dir("diff-adopts-spelling");
        let path = dir.join("artist").join("Album").join("01 - First.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);
        let mut lib = make_lib_track(&path, "First", 1);
        lib.mb_track_id = Some("trk-1".into());

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert_eq!(diff.tracks[0].dest_path, None);
        assert!(
            !diff.tracks[0]
                .fields
                .iter()
                .any(|f| f.kind == FieldKind::Filename && f.enabled),
            "no Filename row to apply"
        );
    }

    #[test]
    fn apply_release_diff_writes_only_enabled_fields() {
        let dir = fresh_dir("apply-enabled");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        // Set the library's album to something different from the release
        // so the Album field genuinely appears in the diff.
        let mut lib = make_lib_track(&path, "Original", 1);
        lib.album = "Stale Album".into();

        let rel = make_release("Album", "Artist");
        let mut diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        // Disable the Title field; keep Album enabled.
        for f in &mut diff.tracks[0].fields {
            if f.name == "Title" {
                f.enabled = false;
            }
            // Disable filename rename so the original path stays put.
            if f.kind == FieldKind::Filename {
                f.enabled = false;
            }
        }

        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok(), "result: {:?}", results[0]);

        // Re-read the file: tag writes are routed through ID3v2 because the
        // WAV's primary RIFF INFO container can't hold MB-style frames. Read
        // from the ID3v2 tag directly to verify the writes landed.
        let tagged = lofty::probe::read_from_path(&path).unwrap();
        let tag = tagged
            .tag(lofty::tag::TagType::Id3v2)
            .expect("ID3v2 tag should be present after write");
        // No Title was written because we disabled it.
        assert!(
            tag.title().is_none() || tag.title().as_deref() == Some(""),
            "Title should be empty/None, got {:?}",
            tag.title()
        );
        assert_eq!(tag.album().as_deref(), Some("Album"));
    }

    #[test]
    fn apply_release_diff_writes_recording_id_to_id3v2() {
        // Regression: lofty has no static ID3v2 key for the recording ID
        // (it becomes a UFID frame at write time), so the checked insert
        // dropped it and the field came back as a change on every open.
        let dir = fresh_dir("apply-recording-id");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "First", 1);
        let rel = make_release("Album", "Artist");
        let mut diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        for f in &mut diff.tracks[0].fields {
            // Title too: the scanner ignores a tag with no title/artist.
            f.enabled = matches!(f.name, "MB Recording ID" | "Title");
        }
        assert!(diff.has_any_enabled(), "release must carry a recording id");

        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "{:?}", results[0]);

        let reread = crate::dirlib::track_from_lofty(&path, 1).expect("tagged file");
        assert_eq!(reread.mb_recording_id.as_deref(), Some("rec-1"));
        let again = build_release_diff(&[reread], &rel, &dir, DiffScope::Track, None);
        let row = again.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "MB Recording ID")
            .unwrap();
        assert!(!row.enabled, "second open must see the id as already set");
    }

    /// A file whose comment frame carries a null language code. Some
    /// taggers write `\0\0\0`; lofty refuses to write a tag containing it,
    /// so a save of ANY field fails until the frame is repaired.
    fn wav_with_null_language_comment(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use lofty::config::WriteOptions;
        use lofty::tag::{Accessor, ItemKey, Tag, TagExt, TagType};
        let dir = fresh_dir(name);
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("First".into());
        tag.set_artist("Artist".into());
        tag.insert_text(ItemKey::Comment, "Electro House".into());
        tag.save_to_path(&path, WriteOptions::default()).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes
            .windows(4)
            .position(|w| w == b"COMM")
            .expect("comment frame present");
        // 10-byte frame header, 1-byte text encoding, then the language.
        bytes[at + 11..at + 14].copy_from_slice(&[0, 0, 0]);
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn null_language_repair_never_displaces_a_valid_comment() {
        use lofty::id3::v2::{CommentFrame, Frame, Id3v2Tag};
        use lofty::tag::TagExt;
        use lofty::TextEncoding;
        let dir = fresh_dir("null-lang-keep");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        // Two well-formed comments with the same (empty) description, then
        // one of them gets the null language some taggers write.
        let mut id3 = Id3v2Tag::default();
        let comment = |lang: &[u8; 3], text: &str| {
            Frame::Comment(CommentFrame::new(
                TextEncoding::UTF8,
                *lang,
                String::new(),
                text.into(),
            ))
        };
        id3.insert(comment(b"eng", "Electro House"));
        id3.insert(comment(b"XXX", "Purchased at Beatport.com"));
        id3.set_title("First".into());
        id3.save_to_path(&path, lofty::config::WriteOptions::default())
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let electro = bytes
            .windows(13)
            .position(|w| w == b"Electro House")
            .unwrap();
        let at = bytes[..electro]
            .windows(4)
            .rposition(|w| w == b"COMM")
            .unwrap();
        assert_eq!(&bytes[at + 11..at + 14], b"eng");
        bytes[at + 11..at + 14].copy_from_slice(&[0, 0, 0]);
        std::fs::write(&path, bytes).unwrap();

        let tagged = probe_by_content(&path).unwrap();
        save_tag(&tagged, TagType::Id3v2, &path).unwrap();

        let after: Vec<(Vec<u8>, String)> = Id3v2Tag::from(
            probe_by_content(&path)
                .unwrap()
                .tag(TagType::Id3v2)
                .unwrap()
                .clone(),
        )
        .comments()
        .map(|c| (c.language.to_vec(), c.content.clone()))
        .collect();
        assert!(
            after.iter().all(|(lang, _)| lang == b"XXX"),
            "every comment is writable now: {after:?}"
        );
        assert!(
            after.iter().any(|(_, c)| c == "Purchased at Beatport.com"),
            "the valid comment is kept: {after:?}"
        );
    }

    #[test]
    fn apply_release_diff_repairs_a_null_comment_language() {
        let (dir, path) = wav_with_null_language_comment("null-lang");
        let mut lib = make_lib_track(&path, "First", 1);
        lib.album = "Stale".into();
        let rel = make_release("Album", "Artist");
        let mut diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        for f in &mut diff.tracks[0].fields {
            f.enabled = f.name == "Album";
        }
        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "{:?}", results[0]);
        let reread = crate::dirlib::track_from_lofty(&path, 1).unwrap();
        assert_eq!(reread.album, "Album");
        assert_eq!(
            reread.comment.as_deref(),
            Some("Electro House"),
            "the comment itself survives the repair"
        );
    }

    #[test]
    fn extras_read_picard_spellings_from_every_container() {
        use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};
        let unknown = |tag: &mut Tag, key: &str, value: &str| {
            tag.insert_unchecked(TagItem::new(
                ItemKey::Unknown(key.into()),
                ItemValue::Text(value.into()),
            ));
        };
        // MP4: freeform atoms come back with the full prefixed key.
        let mut mp4 = Tag::new(TagType::Mp4Ilst);
        unknown(
            &mut mp4,
            "----:com.apple.iTunes:MusicBrainz Album Type",
            "single",
        );
        unknown(
            &mut mp4,
            "----:com.apple.iTunes:MusicBrainz Album Status",
            "official",
        );
        unknown(
            &mut mp4,
            "----:com.apple.iTunes:MusicBrainz Album Release Country",
            "XW",
        );
        unknown(
            &mut mp4,
            "----:com.apple.iTunes:Acoustid Fingerprint",
            "AQAD...",
        );
        let ex = extras_from_tag(&mp4);
        assert_eq!(ex.musicbrainz_album_type.as_deref(), Some("single"));
        assert_eq!(ex.musicbrainz_album_status.as_deref(), Some("official"));
        assert_eq!(ex.release_country.as_deref(), Some("XW"));
        assert_eq!(ex.acoustid_fingerprint.as_deref(), Some("AQAD..."));

        // Vorbis: lofty maps SCRIPT to its own key.
        let mut vorbis = Tag::new(TagType::VorbisComments);
        vorbis.insert(TagItem::new(
            ItemKey::Script,
            ItemValue::Text("Latn".into()),
        ));
        assert_eq!(extras_from_tag(&vorbis).script.as_deref(), Some("Latn"));
    }

    #[test]
    fn picard_album_type_and_status_compare_lowercase() {
        let dir = fresh_dir("picard-lowercase");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "First", 1);
        let mut rel = make_release("Album", "Artist");
        rel.status = Some("Official".into());
        rel.release_group = Some(crate::musicbrainz::ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Single".into()),
            first_release_date: None,
            genres: vec![],
        });
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let proposed = |name: &str| {
            diff.tracks[0]
                .fields
                .iter()
                .find(|f| f.name == name)
                .and_then(|f| f.proposed.clone())
        };
        assert_eq!(proposed("MUSICBRAINZ_ALBUMTYPE").as_deref(), Some("single"));
        assert_eq!(
            proposed("MUSICBRAINZ_ALBUMSTATUS").as_deref(),
            Some("official")
        );
    }

    /// Apply a full release to a real file of each container ffmpeg can
    /// produce here, re-read it, and check nothing is left to change.
    /// Skipped when ffmpeg is not installed.
    #[test]
    fn apply_round_trips_in_every_container() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg not installed; skipping container round trip");
            return;
        }
        let dir = fresh_dir("container-round-trip");
        let seed = dir.join("seed.wav");
        write_sine_wav(&seed, 2);
        let mut rel = make_release("Album", "Artist");
        rel.country = Some("US".into());
        rel.status = Some("Official".into());
        rel.packaging = Some("Jewel Case".into());
        rel.barcode = Some("074645147529".into());
        rel.genres = vec![crate::musicbrainz::MbGenre {
            name: "grunge".into(),
            count: 5,
        }];
        rel.text_representation = Some(crate::musicbrainz::TextRepresentation {
            language: Some("eng".into()),
            script: Some("Latn".into()),
        });
        rel.release_group = Some(crate::musicbrainz::ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Single".into()),
            first_release_date: Some("1969".into()),
            genres: vec![],
        });
        for (ext, codec) in [
            ("mp3", "libmp3lame"),
            ("flac", "flac"),
            ("m4a", "alac"),
            ("ogg", "libvorbis"),
        ] {
            let root = dir.join(ext);
            let path = root
                .join("Artist")
                .join("Album")
                .join(format!("01 - First.{ext}"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let status = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-i"])
                .arg(&seed)
                .args([
                    "-c:a",
                    codec,
                    "-metadata",
                    "title=First",
                    "-metadata",
                    "artist=Artist",
                ])
                .arg(&path)
                .status()
                .unwrap();
            if !status.success() {
                eprintln!("ffmpeg cannot encode {ext} here; skipping");
                continue;
            }
            let read = || crate::dirlib::track_from_lofty(&path, 1).expect("tagged file");
            let uuid = Some("9ff43b6a-4f16-427c-93c2-92307ca505e0");
            let mut diff = build_release_diff(&[read()], &rel, &root, DiffScope::Track, uuid);
            for f in &mut diff.tracks[0].fields {
                if f.kind == FieldKind::Filename {
                    f.enabled = false;
                }
            }
            let out = apply_release_diff(&diff, &root);
            assert!(out.results[0].is_ok(), "{ext}: {:?}", out.results[0]);

            // The AcoustID UUID sits under the key Picard uses in this
            // container, and nowhere else.
            let tagged = lofty::read_from_path(&path).unwrap();
            let tag = tagged.primary_tag().expect("tag after apply");
            let acoustid_keys: Vec<&str> = tag
                .items()
                .filter(|i| i.value().text() == uuid)
                .filter_map(|i| match i.key() {
                    ItemKey::Unknown(k) => Some(k.as_str()),
                    _ => None,
                })
                .collect();
            let want = match ext {
                "mp3" => "Acoustid Id",
                "m4a" => "----:com.apple.iTunes:Acoustid Id",
                _ => "ACOUSTID_ID",
            };
            assert_eq!(acoustid_keys, vec![want], "{ext}");

            let again = build_release_diff(&[read()], &rel, &root, DiffScope::Track, uuid);
            let left: Vec<String> = again.tracks[0]
                .fields
                .iter()
                .filter(|f| f.enabled && f.kind != FieldKind::Filename)
                .map(|f| format!("{}: {:?} -> {:?}", f.name, f.current, f.proposed))
                .collect();
            assert!(left.is_empty(), "{ext} still proposes {left:#?}");
        }
    }

    #[test]
    fn diff_tracks_are_ordered_by_disc_and_track_number() {
        // The library hands tracks over in hash order; the overlay must
        // list them the way the release does.
        let dir = fresh_dir("diff-order");
        let rel = make_release("Album", "Artist");
        let mut lib: Vec<LibTrack> = Vec::new();
        for (n, title) in [(2u32, "Second"), (1, "First")] {
            let path = dir.join(format!("{n:02} - {title}.wav"));
            write_sine_wav(&path, 1);
            lib.push(make_lib_track(&path, title, n));
        }
        let diff = build_release_diff(&lib, &rel, &dir, DiffScope::Album, None);
        let order: Vec<String> = diff
            .tracks
            .iter()
            .map(|t| {
                t.src_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(order, vec!["01 - First.wav", "02 - Second.wav"]);
    }

    #[test]
    fn apply_release_diff_writes_year_field() {
        // Regression: `build_track_fields` pushes the date row with name
        // "Year", but `apply_field` previously matched only "Release Date" —
        // so Year edits silently never wrote to disk.
        let dir = fresh_dir("apply-year");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        let mut lib = make_lib_track(&path, "First", 1);
        lib.year = Some(2099);

        let rel = make_release("Album", "Artist");
        let mut diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        // Keep only the Year field enabled so we can isolate the write.
        for f in &mut diff.tracks[0].fields {
            f.enabled = matches!(f.kind, FieldKind::Date) && f.name == "Year";
        }

        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "result: {:?}", results[0]);

        let tagged = lofty::probe::read_from_path(&path).unwrap();
        let tag = tagged
            .tag(lofty::tag::TagType::Id3v2)
            .expect("ID3v2 tag should be present");
        let year = tag
            .get_string(&lofty::tag::ItemKey::RecordingDate)
            .expect("Year/RecordingDate should have been written");
        assert_eq!(year, "1969");
    }

    #[test]
    fn write_track_tags_atomic_failure_leaves_original_intact() {
        // Point `src_path` at a non-audio file. `probe_by_content` on the
        // temp copy fails, the rename never happens, and the temp is
        // cleaned up — so the original bytes are untouched and no
        // `.tagtmp` is left lying around.
        let dir = fresh_dir("atomic-save-failure");
        let path = dir.join("not-actually-audio.wav");
        let sentinel = b"this is not an audio file, lofty will reject it";
        std::fs::write(&path, sentinel).unwrap();

        let diff = ReleaseTagDiff {
            release_mbid: "rel-x".into(),
            summary: "test".into(),
            tracks: vec![TrackTagDiff {
                src_path: path.clone(),
                dest_path: None,
                library_id: 1,
                fields: vec![FieldDiff {
                    kind: FieldKind::Identity,
                    name: "Title",
                    current: None,
                    proposed: Some("Forced Write".into()),
                    enabled: true,
                }],
            }],
        };

        let ApplyOutcome {
            results,
            rename_map,
            ..
        } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_err(), "save should fail on non-audio bytes");
        assert!(rename_map.is_empty());

        // Original bytes survived.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            after, sentinel,
            "original file must not be modified when the save fails"
        );

        // No leftover temp.
        let tmp = path.with_file_name("not-actually-audio.wav.tagtmp");
        assert!(
            !tmp.exists(),
            "tagtmp temp must be cleaned up on failure: {}",
            tmp.display()
        );
    }

    #[test]
    fn write_track_tags_atomic_success_leaves_no_temp() {
        let dir = fresh_dir("atomic-save-success");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        let lib = make_lib_track(&path, "Original", 1);
        let rel = make_release("Album", "Artist");
        let mut diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        // Disable the Filename rename so the source path stays put and we
        // can assert the tag-save's atomic-swap step in isolation.
        for f in &mut diff.tracks[0].fields {
            if f.kind == FieldKind::Filename {
                f.enabled = false;
            }
        }

        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "result: {:?}", results[0]);

        let tmp = path.with_file_name("song.wav.tagtmp");
        assert!(
            !tmp.exists(),
            "tagtmp must be renamed away on success: {}",
            tmp.display()
        );
        // And the original path still exists (rename swapped temp -> src).
        assert!(path.exists(), "source path must still exist after save");
    }

    #[test]
    fn apply_release_diff_two_phase_rename() {
        let dir = fresh_dir("apply-rename");
        let old = dir.join("Artist").join("Album").join("99 - first.wav");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        write_sine_wav(&old, 1);

        let mut lib = make_lib_track(&old, "First", 99);
        lib.mb_track_id = Some("trk-1".into());
        // Force an album-field diff so the post-rename tag check has something
        // to verify (a no-diff field would never get written).
        lib.album = "Old Album".into();

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert!(diff.tracks[0].dest_path.is_some());

        let ApplyOutcome {
            results,
            rename_map,
            ..
        } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "result: {:?}", results[0]);
        let new_path = diff.tracks[0].dest_path.as_ref().unwrap();
        assert!(
            new_path.exists(),
            "renamed file should exist: {}",
            new_path.display()
        );
        assert!(
            !old.exists(),
            "original file should be gone: {}",
            old.display()
        );
        assert_eq!(rename_map.get(&old).cloned(), Some(new_path.clone()));

        // Tags should also be present on the renamed file (ID3v2 — see the
        // RIFF INFO comment in the enabled-fields test above).
        let tagged = lofty::probe::read_from_path(new_path).unwrap();
        let tag = tagged
            .tag(lofty::tag::TagType::Id3v2)
            .expect("ID3v2 tag should be present after write");
        assert_eq!(tag.album().as_deref(), Some("Album"));
    }

    #[test]
    fn two_tracks_bound_for_one_dest_both_land() {
        let dir = fresh_dir("collision");
        // Two source files; both want to land at the same new path.
        let src1 = dir.join("a.wav");
        let src2 = dir.join("b.wav");
        write_sine_wav(&src1, 1);
        write_sine_wav(&src2, 4);
        let dest = dir.join("c.wav");

        let diff = ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "test".into(),
            tracks: vec![
                TrackTagDiff {
                    src_path: src1.clone(),
                    dest_path: Some(dest.clone()),
                    library_id: 1,
                    fields: vec![FieldDiff {
                        kind: FieldKind::Filename,
                        name: "Filename",
                        current: Some(src1.display().to_string()),
                        proposed: Some(dest.display().to_string()),
                        enabled: true,
                    }],
                },
                TrackTagDiff {
                    src_path: src2.clone(),
                    dest_path: Some(dest.clone()),
                    library_id: 2,
                    fields: vec![FieldDiff {
                        kind: FieldKind::Filename,
                        name: "Filename",
                        current: Some(src2.display().to_string()),
                        proposed: Some(dest.display().to_string()),
                        enabled: true,
                    }],
                },
            ],
        };

        let ApplyOutcome {
            results,
            rename_map,
            ..
        } = apply_release_diff(&diff, &dir);
        // Neither is refused and neither is lost: the first in diff order
        // takes the name, the other lands beside it.
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert_eq!(rename_map.get(&src1), Some(&dest));
        assert_eq!(rename_map.get(&src2), Some(&numbered(&dest, 2)));
        assert!(dest.exists() && numbered(&dest, 2).exists());
        assert!(!src1.exists() && !src2.exists());
    }

    #[test]
    fn build_release_diff_emits_genre_album_type_media_country_status() {
        // Regression: tag-manager used to surface only ~14 fields. After adding
        // Picard-standard release-level fields it should emit Genre, album
        // type, media format, country, status, packaging, and script too.
        use crate::musicbrainz::{MbGenre, ReleaseGroup, TextRepresentation};
        let dir = fresh_dir("more-fields");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        let lib = make_lib_track(&path, "First", 1);

        let mut rel = make_release("Album", "Artist");
        rel.status = Some("Official".into());
        rel.packaging = Some("Cardboard/Paper Sleeve".into());
        rel.country = Some("GB".into());
        rel.text_representation = Some(TextRepresentation {
            language: Some("eng".into()),
            script: Some("Latn".into()),
        });
        rel.release_group = Some(ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Album".into()),
            first_release_date: None,
            genres: vec![
                MbGenre {
                    name: "rock".into(),
                    count: 14,
                },
                MbGenre {
                    name: "blues".into(),
                    count: 6,
                },
            ],
        });
        // media format already set on rel.media[0] = "CD" via make_release.

        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert_eq!(diff.tracks.len(), 1);
        let names: Vec<&str> = diff.tracks[0].fields.iter().map(|f| f.name).collect();
        // Most-voted release-group genre wins.
        let genre = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Genre")
            .expect("Genre field missing");
        assert_eq!(genre.proposed.as_deref(), Some("rock"));
        // Picard-style release-level fields.
        for expected in [
            "MUSICBRAINZ_ALBUMTYPE",
            "Media",
            "MUSICBRAINZ_ALBUMSTATUS",
            "MUSICBRAINZ_ALBUMPACKAGING",
            "RELEASECOUNTRY",
            "SCRIPT",
        ] {
            assert!(
                names.contains(&expected),
                "expected field {expected:?} in diff; got: {names:?}"
            );
        }
        // Also confirm the Date field was renamed to "Year".
        assert!(names.contains(&"Year"));
        assert!(!names.contains(&"Release Date"));
    }

    #[test]
    fn build_release_diff_emits_acoustid_fingerprint_when_missing_on_disk() {
        // Library track carries a Chromaprint fingerprint that the file's
        // tag does NOT have on disk (e.g. it was freshly computed). The
        // diff should include the row so apply writes it.
        let dir = fresh_dir("acoustid-fp");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);

        let mut lib = make_lib_track(&path, "First", 1);
        lib.acoustic_id = Some("AQADtBR=hellofingerprint".into());

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let fp_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "ACOUSTID_FINGERPRINT")
            .expect("ACOUSTID_FINGERPRINT field should be present");
        assert_eq!(fp_field.kind, FieldKind::Picard);
        assert_eq!(
            fp_field.proposed.as_deref(),
            Some("AQADtBR=hellofingerprint")
        );
        // Tag is not on disk yet, so `current` reflects that.
        assert!(fp_field.current.is_none());
    }

    #[test]
    fn genre_stays_when_mb_has_no_genre() {
        // Picard-style preservation: the file has Genre="Easy Listening", MB
        // has no genre on this release. The diff should NOT propose to
        // clear the tag — it should show the row as unchanged (current ==
        // proposed) with enabled=false, so the user can see the existing
        // value but `a` / Enter won't blank it.
        let dir = fresh_dir("genre-preserve");
        let path = dir.join("Artist").join("Album").join("01 - First.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        let mut lib = make_lib_track(&path, "First", 1);
        lib.genre = Some("Easy Listening".into());

        // make_release returns a release with no genres anywhere.
        let rel = make_release("Album", "Artist");
        assert!(pick_top_genre(&rel).is_none(), "test setup precondition");

        let diff = build_release_diff(
            std::slice::from_ref(&lib),
            &rel,
            &dir,
            DiffScope::Track,
            None,
        );
        let genre = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Genre")
            .expect("Genre row should be present so the user sees the existing value");
        assert_eq!(genre.current.as_deref(), Some("Easy Listening"));
        assert_eq!(
            genre.proposed.as_deref(),
            Some("Easy Listening"),
            "MB has nothing to propose — keep the existing tag",
        );
        assert!(
            !genre.enabled,
            "Genre row must default to disabled so an unsuspecting Apply doesn't blank a curated tag",
        );
    }

    #[test]
    fn picard_extras_become_unchanged_after_apply() {
        // Regression: `current` for Picard TXXX fields (RELEASECOUNTRY,
        // MUSICBRAINZ_*, etc.) and the Media row used to be hardcoded to
        // `None`. After applying the diff, those fields would re-appear as
        // deltas on the next `m` press even though the writes succeeded —
        // the diff comparator simply wasn't reading them back from disk.
        // This test exercises the full apply → rebuild round trip and
        // asserts that the second diff sees the fields as unchanged.
        let dir = fresh_dir("picard-extras-roundtrip");
        let path = dir.join("Artist").join("Album").join("01 - First.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        let lib = make_lib_track(&path, "First", 1);
        let mut rel = make_release("Album", "Artist");
        rel.country = Some("US".into());
        rel.status = Some("Official".into());
        rel.packaging = Some("Jewel Case".into());
        rel.text_representation = Some(crate::musicbrainz::TextRepresentation {
            language: Some("eng".into()),
            script: Some("Latn".into()),
        });
        rel.release_group = Some(crate::musicbrainz::ReleaseGroup {
            id: "rg-1".into(),
            title: "Album".into(),
            primary_type: Some("Album".into()),
            first_release_date: Some("1969".into()),
            genres: vec![],
        });

        // Round 1: build + apply.
        let diff1 = build_release_diff(
            std::slice::from_ref(&lib),
            &rel,
            &dir,
            DiffScope::Track,
            None,
        );
        // Sanity: the Picard rows should be deltas on the first pass —
        // file is freshly-written WAV with no tags.
        for name in [
            "MUSICBRAINZ_ALBUMTYPE",
            "MUSICBRAINZ_ALBUMSTATUS",
            "MUSICBRAINZ_ALBUMPACKAGING",
            "RELEASECOUNTRY",
            "SCRIPT",
            "Media",
        ] {
            let f = diff1.tracks[0]
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name:?} missing on first diff"));
            assert!(f.enabled, "{name:?} should be a real delta on first build");
            assert!(f.current.is_none());
        }
        let ApplyOutcome { results, .. } = apply_release_diff(&diff1, &dir);
        for r in &results {
            r.as_ref().expect("apply should succeed");
        }

        // Round 2: rebuild against the same release. After the apply, the
        // file carries every Picard extra MB proposed — they should now
        // round-trip as `current == proposed` and come back disabled.
        let diff2 = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        for name in [
            "MUSICBRAINZ_ALBUMTYPE",
            "MUSICBRAINZ_ALBUMSTATUS",
            "MUSICBRAINZ_ALBUMPACKAGING",
            "RELEASECOUNTRY",
            "SCRIPT",
            "Media",
        ] {
            let f = diff2.tracks[0]
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name:?} missing on second diff"));
            assert_eq!(
                f.current, f.proposed,
                "{name:?} should round-trip to unchanged after apply (current={:?}, proposed={:?})",
                f.current, f.proposed,
            );
            assert!(
                !f.enabled,
                "{name:?} should be disabled on the second diff — the value is already on disk",
            );
        }
    }

    #[test]
    fn build_release_diff_marks_acoustid_fingerprint_disabled_when_already_on_disk() {
        // The row is surfaced unconditionally (the user wants visibility into
        // existing metadata), but when the on-disk fingerprint already matches
        // the library value, the row must come back disabled so the apply
        // path doesn't rewrite the file for a no-op.
        // Write a WAV with an ID3v2 `ACOUSTID_FINGERPRINT` extended-text
        // frame already on disk. (WAV permits an embedded `id3` chunk;
        // lofty/symphonia both honour it. Mirrors the
        // `fingerprint::tests::write_acoustid_id3_tag` helper.)
        let dir = fresh_dir("acoustid-fp-already-tagged");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        {
            use id3::frame::ExtendedText;
            use id3::{Tag, TagLike, Version};
            let mut tag = Tag::new();
            tag.add_frame(ExtendedText {
                description: "ACOUSTID_FINGERPRINT".to_string(),
                value: "AQADtBR=hellofingerprint".to_string(),
            });
            tag.write_to_path(&path, Version::Id3v24).unwrap();
        }

        let mut lib = make_lib_track(&path, "First", 1);
        lib.acoustic_id = Some("AQADtBR=hellofingerprint".into());

        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let fp_row = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "ACOUSTID_FINGERPRINT")
            .expect("fingerprint row should be present so the user can see the tag");
        assert!(
            !fp_row.enabled,
            "ACOUSTID_FINGERPRINT should be disabled when on-disk value matches the library",
        );
        assert_eq!(fp_row.current, fp_row.proposed);
    }

    #[test]
    fn build_release_diff_omits_acoustid_fingerprint_when_library_has_none() {
        let dir = fresh_dir("acoustid-fp-none");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "First", 1); // acoustic_id defaults to None
        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert!(!diff.tracks[0]
            .fields
            .iter()
            .any(|f| f.name == "ACOUSTID_FINGERPRINT"));
    }

    #[test]
    fn build_release_diff_emits_acoustid_id_when_uuid_supplied() {
        let dir = fresh_dir("acoustid-id");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "First", 1);
        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, Some("ac-uuid-42"));
        let id_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "ACOUSTID_ID")
            .expect("ACOUSTID_ID field should be present");
        assert_eq!(id_field.kind, FieldKind::Picard);
        assert_eq!(id_field.proposed.as_deref(), Some("ac-uuid-42"));
        assert!(id_field.current.is_none());
        assert!(id_field.enabled);
    }

    #[test]
    fn build_release_diff_omits_acoustid_id_when_no_uuid() {
        // Pure MBID-direct or MB-search resolution path supplies no
        // AcoustID UUID — the field must not appear.
        let dir = fresh_dir("acoustid-id-none");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let lib = make_lib_track(&path, "First", 1);
        let rel = make_release("Album", "Artist");
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        assert!(!diff.tracks[0]
            .fields
            .iter()
            .any(|f| f.name == "ACOUSTID_ID"));
    }

    #[test]
    fn apply_writes_acoustid_fields_via_unknown_text_frame() {
        // End-to-end: apply a diff with ACOUSTID_FINGERPRINT and ACOUSTID_ID
        // and verify they land as TXXX user-defined text frames on disk.
        let dir = fresh_dir("acoustid-apply");
        let path = dir.join("song.wav");
        write_sine_wav(&path, 1);
        let mut lib = make_lib_track(&path, "First", 1);
        lib.acoustic_id = Some("AQADtBR=writeMe".into());
        let rel = make_release("Album", "Artist");
        let mut diff =
            build_release_diff(&[lib], &rel, &dir, DiffScope::Track, Some("uuid-to-write"));
        // Disable rename + all non-AcoustID writes so the test asserts
        // narrowly on the AcoustID landings.
        for f in &mut diff.tracks[0].fields {
            if f.kind == FieldKind::Filename {
                f.enabled = false;
            }
        }
        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &dir);
        assert!(results[0].is_ok(), "{:?}", results[0]);

        // Re-probe and look for the TXXX entries by name.
        let tagged = lofty::probe::read_from_path(&path).unwrap();
        let id3 = tagged
            .tag(lofty::tag::TagType::Id3v2)
            .expect("ID3v2 tag should exist after apply");
        let find = |name: &str| {
            id3.items()
                .find(|i| {
                    matches!(i.key(),
                        lofty::prelude::ItemKey::Unknown(s) if s.eq_ignore_ascii_case(name))
                })
                .and_then(|i| i.value().text().map(str::to_string))
        };
        assert_eq!(
            find("Acoustid Fingerprint").as_deref(),
            Some("AQADtBR=writeMe"),
            "ID3v2 uses Picard's TXXX description"
        );
        assert_eq!(
            find("Acoustid Id").as_deref(),
            Some("uuid-to-write"),
            "ID3v2 uses Picard's TXXX description"
        );
        assert_eq!(
            find("ACOUSTID_ID"),
            None,
            "no second copy under the Vorbis name"
        );
    }

    #[test]
    fn tagging_an_id3v1_only_mp3_keeps_what_it_already_said() {
        use lofty::config::WriteOptions;
        use lofty::file::AudioFile;
        let Some(path) = crate::test_audio::ffmpeg_mp3("id3v1-only-apply") else {
            return;
        };
        // ffmpeg writes an ID3v2 of its own; this file must have none.
        TagType::Id3v2.remove_from_path(&path).unwrap();
        let mut tf = lofty::read_from_path(&path).unwrap();
        let mut v1 = Tag::new(TagType::Id3v1);
        v1.set_title("Old Title".into());
        v1.set_artist("Old Artist".into());
        v1.set_album("Old Album".into());
        v1.set_genre("Rock".into());
        v1.set_comment("ripped 2003".into());
        tf.insert_tag(v1);
        tf.save_to_path(&path, WriteOptions::default()).unwrap();

        // An edit that touches none of those fields.
        write_track_tags(&TrackTagDiff {
            src_path: path.clone(),
            dest_path: None,
            library_id: 1,
            fields: vec![FieldDiff {
                kind: FieldKind::MbId,
                name: "MB Release ID",
                current: None,
                proposed: Some("rel-1".into()),
                enabled: true,
            }],
        })
        .unwrap();

        let back = lofty::read_from_path(&path).unwrap();
        let v1 = back.tag(TagType::Id3v1).expect("ID3v1 survives");
        assert_eq!(v1.title().as_deref(), Some("Old Title"));
        assert_eq!(v1.artist().as_deref(), Some("Old Artist"));
        assert_eq!(v1.album().as_deref(), Some("Old Album"));
        assert_eq!(v1.genre().as_deref(), Some("Rock"));
        assert_eq!(v1.comment().as_deref(), Some("ripped 2003"));
        let v2 = back.tag(TagType::Id3v2).expect("ID3v2 was added");
        assert_eq!(v2.get_string(&ItemKey::MusicBrainzReleaseId), Some("rel-1"));
        let scanned = crate::dirlib::track_from_lofty(&path, 1).expect("still tagged");
        assert_eq!(scanned.name, "Old Title");
        assert_eq!(scanned.artist, "Old Artist");
        assert_eq!(scanned.album, "Old Album");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn apply_replaces_a_legacy_acoustid_id_spelling() {
        // Earlier builds wrote `TXXX:ACOUSTID_ID` into ID3v2. The row reads
        // it as the current value, and a rewrite leaves one Picard-spelled
        // copy rather than two.
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_unchecked(lofty::tag::TagItem::new(
            ItemKey::Unknown("ACOUSTID_ID".into()),
            ItemValue::Text("old-uuid".into()),
        ));
        assert_eq!(
            extras_from_tag(&tag).acoustid_id.as_deref(),
            Some("old-uuid")
        );

        set_unknown_string(&mut tag, "ACOUSTID_ID", "new-uuid");
        let copies: Vec<(&str, Option<&str>)> = tag
            .items()
            .filter_map(|i| match i.key() {
                ItemKey::Unknown(k) => Some((k.as_str(), i.value().text())),
                _ => None,
            })
            .collect();
        assert_eq!(copies, vec![("Acoustid Id", Some("new-uuid"))]);
    }

    #[test]
    fn pick_top_genre_prefers_release_group_when_higher_count() {
        use crate::musicbrainz::{MbGenre, ReleaseGroup};
        let mut rel = make_release("X", "Y");
        rel.release_group = Some(ReleaseGroup {
            id: "rg".into(),
            title: "X".into(),
            primary_type: None,
            first_release_date: None,
            genres: vec![MbGenre {
                name: "jazz".into(),
                count: 10,
            }],
        });
        rel.genres = vec![MbGenre {
            name: "punk".into(),
            count: 2,
        }];
        assert_eq!(pick_top_genre(&rel).as_deref(), Some("jazz"));
    }

    #[test]
    fn pick_top_genre_returns_none_when_neither_side_has_any() {
        let rel = make_release("X", "Y");
        assert_eq!(pick_top_genre(&rel), None);
    }

    #[test]
    fn build_release_diff_pairs_across_multi_disc_release() {
        // Regression: previously only the first non-empty medium was
        // consulted, so a library track on disc 2 silently got dropped from
        // the diff. Set lib's MBID to the disc-2 track so pairing has to
        // walk both media to succeed.
        let dir = fresh_dir("multi-disc");
        let path = dir.join("Artist").join("Album").join("disc2-track1.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        let mut rel = make_release("Album", "Artist");
        let mut disc2 = rel.media[0].clone();
        disc2.position = Some(2);
        disc2.tracks = vec![MbTrack {
            id: "trk-d2-1".into(),
            number: "1".into(),
            position: Some(1),
            title: "Disc Two Opener".into(),
            length: Some(150_000),
            recording: Some(Recording {
                id: "rec-d2-1".into(),
                title: "Disc Two Opener".into(),
                length: Some(150_000),
                isrcs: vec![],
                artist_credit: vec![],
            }),
            artist_credit: vec![],
        }];
        rel.media.push(disc2);

        // Library side carries no disc/track numbers — pairing must hit on
        // MBID alone, walking both media.
        let mut lib = make_lib_track(&path, "Disc Two Opener", 1);
        lib.track_number = None;
        lib.mb_track_id = Some("trk-d2-1".into());
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Album, None);

        assert_eq!(
            diff.tracks.len(),
            1,
            "disc-2 library track should pair against disc 2 in the release"
        );
        let disc_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Disc #")
            .expect("Disc # field should reflect the matched medium");
        assert_eq!(
            disc_field.proposed.as_deref(),
            Some("2"),
            "Disc # must come from disc 2's medium.position, not disc 1"
        );
        let track_field = diff.tracks[0]
            .fields
            .iter()
            .find(|f| f.name == "Track #")
            .expect("Track # field should be present");
        assert_eq!(track_field.proposed.as_deref(), Some("1"));
    }

    /// When a release's track is credited to "*NSYNC feat. Lisa Lopes" but
    /// the release-level artist credit is just "*NSYNC", the diff must
    /// propose the feat-string for `Artist` and the canonical name for
    /// `Album Artist`. Pairing the feature credit only into `Artist` is
    /// what lets `dirlib::Track::grouping_artist()` keep the track filed
    /// under the album's canonical artist.
    #[test]
    fn release_diff_splits_feat_artist_from_album_artist() {
        let dir = fresh_dir("feat-split");
        let path = dir
            .join("Artist")
            .join("Album")
            .join("01 - Space Cowboy.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(&path, 1);

        let mut rel = make_release("No Strings Attached", "*NSYNC");
        // Override the first track's per-recording credit to include a feature.
        rel.media[0].tracks[0].title = "Space Cowboy".into();
        rel.media[0].tracks[0].artist_credit = vec![
            ArtistCredit {
                name: "*NSYNC".into(),
                joinphrase: Some(" feat. ".into()),
                artist: Some(Artist {
                    id: "art-1".into(),
                    name: "*NSYNC".into(),
                    sort_name: None,
                }),
            },
            ArtistCredit {
                name: "Lisa \"Left Eye\" Lopes".into(),
                joinphrase: None,
                artist: Some(Artist {
                    id: "art-feat".into(),
                    name: "Lisa \"Left Eye\" Lopes".into(),
                    sort_name: None,
                }),
            },
        ];

        let lib = make_lib_track(&path, "Space Cowboy", 1);
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let track = &diff.tracks[0];

        let artist_field = track
            .fields
            .iter()
            .find(|f| f.name == "Artist")
            .expect("Artist field present");
        assert_eq!(
            artist_field.proposed.as_deref(),
            Some("*NSYNC feat. Lisa \"Left Eye\" Lopes"),
            "Artist should carry the per-recording credit including the feature",
        );

        let album_artist_field = track
            .fields
            .iter()
            .find(|f| f.name == "Album Artist")
            .expect("Album Artist field present");
        assert_eq!(
            album_artist_field.proposed.as_deref(),
            Some("*NSYNC"),
            "Album Artist must come from the release-level credit so dirlib groups under the canonical artist",
        );
    }

    #[test]
    fn feat_on_release_credit_files_under_primary_artist() {
        let dir = fresh_dir("feat-release-folder");
        let src_dir = dir
            .join("2Pac featuring the Notorious B.I.G.")
            .join("All Eyez On Me");
        std::fs::create_dir_all(&src_dir).unwrap();
        let path = src_dir.join("01 - Track.wav");
        write_sine_wav(&path, 1);

        let mut rel = make_release("All Eyez On Me", "2Pac");
        rel.artist_credit = vec![
            ArtistCredit {
                name: "2Pac".into(),
                joinphrase: Some(" featuring ".into()),
                artist: Some(Artist {
                    id: "art-2pac".into(),
                    name: "2Pac".into(),
                    sort_name: None,
                }),
            },
            ArtistCredit {
                name: "The Notorious B.I.G.".into(),
                joinphrase: None,
                artist: Some(Artist {
                    id: "art-big".into(),
                    name: "The Notorious B.I.G.".into(),
                    sort_name: None,
                }),
            },
        ];
        rel.media[0].tracks[0].title = "Track".into();

        let lib = make_lib_track(&path, "Track", 1);
        let diff = build_release_diff(&[lib], &rel, &dir, DiffScope::Track, None);
        let track = &diff.tracks[0];
        assert_eq!(
            track
                .fields
                .iter()
                .find(|f| f.name == "Album Artist")
                .and_then(|f| f.proposed.as_deref()),
            Some("2Pac"),
        );
        let dest = track.dest_path.as_ref().expect("rename proposed");
        assert_eq!(
            dest,
            &dir.join("2Pac")
                .join("All Eyez On Me")
                .join("01 - Track.wav")
        );
    }

    fn folder_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn filing_lands_in_the_folder_spelling_already_on_disk() {
        // The artist folder is there as `alice in chains`. Filing adopts
        // that spelling: no folder is renamed, and so no album that is not
        // part of the diff moves.
        let parent = fresh_dir("adopt-from-inbox");
        let music = parent.join("Music");
        let inbox = parent.join("Automatically Add to Music");
        let facelift = music
            .join("alice in chains")
            .join("Facelift")
            .join("01 - We Die Young.wav");
        std::fs::create_dir_all(facelift.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&inbox).unwrap();
        write_sine_wav(&facelift, 1);
        let src = inbox.join("01 them bones.wav");
        write_sine_wav(&src, 1);
        let dest = music
            .join("Alice In Chains")
            .join("Dirt")
            .join("01 - Them Bones.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &music);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        let landed = music
            .join("alice in chains")
            .join("Dirt")
            .join("01 - Them Bones.wav");
        assert_eq!(out.rename_map.get(&src), Some(&landed));
        assert!(landed.exists());
        assert!(facelift.exists(), "the album already there did not move");
        assert_eq!(folder_names(&music), vec!["alice in chains".to_string()]);
    }

    #[test]
    fn a_filename_change_in_a_wrong_case_folder_keeps_the_folder() {
        let dir = fresh_dir("adopt-and-filename");
        let src_album = dir.join("Alice in Chains").join("Dirt");
        std::fs::create_dir_all(&src_album).unwrap();
        let src = src_album.join("01 Song.wav");
        let sibling = src_album.join("02 Other.wav");
        write_sine_wav(&src, 1);
        write_sine_wav(&sibling, 1);
        let dest = dir
            .join("Alice In Chains")
            .join("Dirt")
            .join("01 - Song.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        let landed = src_album.join("01 - Song.wav");
        assert_eq!(out.rename_map.get(&src), Some(&landed));
        assert!(landed.exists() && sibling.exists());
        assert_eq!(folder_names(&dir), vec!["Alice in Chains".to_string()]);
    }

    #[test]
    fn folders_differing_only_in_case_leave_the_file_where_it_is() {
        let dir = fresh_dir("adopt-nothing-to-do");
        let src = dir.join("Alice in Chains").join("Dirt").join("01 - A.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_sine_wav(&src, 1);
        let dest = dir.join("Alice In Chains").join("Dirt").join("01 - A.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(out.rename_map.is_empty(), "{:?}", out.rename_map);
        assert!(src.exists());
        assert_eq!(folder_names(&dir), vec!["Alice in Chains".to_string()]);
    }

    #[test]
    fn a_file_name_that_differs_only_in_case_is_respelled_not_numbered() {
        let dir = fresh_dir("leaf-case");
        let src = dir.join("01 - song.wav");
        write_sine_wav(&src, 1);
        let dest = dir.join("01 - Song.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert_eq!(out.rename_map.get(&src), Some(&dest));
        assert_eq!(folder_names(&dir), vec!["01 - Song.wav".to_string()]);
    }

    fn volume_folds_ascii_case(dir: &std::path::Path) -> bool {
        let probe = dir.join("ZyTunesCaseProbe");
        std::fs::write(&probe, b"x").unwrap();
        let folds = dir.join("zytunescaseprobe").exists();
        let _ = std::fs::remove_file(&probe);
        folds
    }

    #[test]
    fn prune_does_not_walk_above_library_or_inbox() {
        let parent = fresh_dir("prune-fence");
        let music = parent.join("Music");
        let inbox_root = parent.join("Automatically Add to Music");
        let feat = inbox_root.join("Feat Artist");
        std::fs::create_dir_all(&feat).unwrap();
        std::fs::create_dir_all(music.join("Canon")).unwrap();
        let src = feat.join("01 - Track.wav");
        let dest = music.join("Canon").join("01 - Track.wav");
        write_sine_wav(&src, 1);

        let diff = ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "test".into(),
            tracks: vec![TrackTagDiff {
                src_path: src.clone(),
                dest_path: Some(dest.clone()),
                library_id: 1,
                fields: vec![FieldDiff {
                    kind: FieldKind::Filename,
                    name: "Filename",
                    current: Some(src.display().to_string()),
                    proposed: Some(dest.display().to_string()),
                    enabled: true,
                }],
            }],
        };

        let ApplyOutcome { results, .. } = apply_release_diff(&diff, &music);
        assert!(results[0].is_ok(), "{:?}", results[0]);
        assert!(dest.exists());
        assert!(!feat.exists(), "empty inbox artist dir should go");
        assert!(inbox_root.exists(), "inbox folder itself must stay");
        assert!(music.exists(), "library root must stay");
        assert!(parent.exists(), "parent of the library must stay");
    }

    /// One-track diff that only renames `src` to `dest`.
    fn rename_only_diff(src: &std::path::Path, dest: &std::path::Path) -> ReleaseTagDiff {
        ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "test".into(),
            tracks: vec![TrackTagDiff {
                src_path: src.to_path_buf(),
                dest_path: Some(dest.to_path_buf()),
                library_id: 1,
                fields: vec![FieldDiff {
                    kind: FieldKind::Filename,
                    name: "Filename",
                    current: Some(src.display().to_string()),
                    proposed: Some(dest.display().to_string()),
                    enabled: true,
                }],
            }],
        }
    }

    /// A rename-only diff of several `(src, dest)` pairs.
    fn renames_diff(pairs: &[(&Path, &Path)]) -> ReleaseTagDiff {
        let mut diff = rename_only_diff(pairs[0].0, pairs[0].1);
        for (src, dest) in &pairs[1..] {
            diff.tracks.extend(rename_only_diff(src, dest).tracks);
        }
        diff
    }

    /// One-second wav at `path`, parents created.
    fn wav_at(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_sine_wav(path, 1);
    }

    fn file_at(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn sidecars_follow_the_audio_out_of_a_folder() {
        // The cover and the scans belong to the album. Left behind they
        // keep the old folder alive with no music in it.
        let dir = fresh_dir("sidecars-follow");
        let src = dir.join("loose").join("rip").join("a.wav");
        let album = dir.join("Artist").join("Album");
        wav_at(&src);
        file_at(&src.with_file_name("cover.jpg"), b"cover");
        file_at(&src.with_file_name("Scans").join("back.png"), b"back");
        let out = apply_release_diff(&rename_only_diff(&src, &album.join("01 - Song.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(out.sidecar_errors.is_empty(), "{:?}", out.sidecar_errors);
        assert_eq!(std::fs::read(album.join("cover.jpg")).unwrap(), b"cover");
        assert_eq!(
            std::fs::read(album.join("Scans").join("back.png")).unwrap(),
            b"back"
        );
        assert!(!dir.join("loose").exists(), "the old folders are gone");
    }

    #[test]
    fn sidecars_stay_while_the_folder_still_holds_audio() {
        let dir = fresh_dir("sidecars-stay");
        let src = dir.join("loose").join("a.wav");
        let staying = dir.join("loose").join("b.wav");
        wav_at(&src);
        wav_at(&staying);
        file_at(&src.with_file_name("cover.jpg"), b"cover");
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(src.with_file_name("cover.jpg").exists());
        assert!(!dest.with_file_name("cover.jpg").exists());
    }

    #[test]
    fn a_sidecar_never_overwrites_one_already_there() {
        let dir = fresh_dir("sidecars-held");
        let src = dir.join("loose").join("a.wav");
        let album = dir.join("Artist").join("Album");
        wav_at(&src);
        wav_at(&album.join("02 - Other.wav"));
        file_at(&src.with_file_name("cover.jpg"), b"new cover");
        file_at(&src.with_file_name("notes.txt"), b"same");
        file_at(&album.join("cover.jpg"), b"old cover");
        file_at(&album.join("notes.txt"), b"same");
        let out = apply_release_diff(&rename_only_diff(&src, &album.join("01 - Song.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert_eq!(
            std::fs::read(album.join("cover.jpg")).unwrap(),
            b"old cover"
        );
        assert_eq!(
            std::fs::read(album.join("cover (2).jpg")).unwrap(),
            b"new cover"
        );
        // Identical contents make no difference: nothing is dropped, so
        // removing duplicates is left to a later step.
        assert_eq!(std::fs::read(album.join("notes (2).txt")).unwrap(), b"same");
        assert!(!dir.join("loose").exists());
    }

    #[test]
    fn a_cover_above_disc_folders_follows_once_both_discs_left() {
        let dir = fresh_dir("sidecars-discs");
        let old = dir.join("Artist").join("old album");
        let one = old.join("CD1").join("a.wav");
        let two = old.join("CD2").join("b.wav");
        wav_at(&one);
        wav_at(&two);
        file_at(&old.join("cover.jpg"), b"cover");
        file_at(&old.join("CD2").join("disc2.cue"), b"cue");
        let album = dir.join("Artist").join("Album");
        let diff = renames_diff(&[
            (&one, &album.join("1-01 - A.wav")),
            (&two, &album.join("2-01 - B.wav")),
        ]);
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert!(album.join("cover.jpg").exists());
        assert!(album.join("disc2.cue").exists());
        assert!(!old.exists());
    }

    #[test]
    fn disc_folders_flatten_the_same_whether_or_not_a_loose_track_left_with_them() {
        // The album had a loose track beside its disc folders. The parent
        // leaves in the same apply as the discs, and the discs' cue sheets
        // still land in the album, not in `Album/CD1/`.
        let dir = fresh_dir("sidecars-discs-and-loose");
        let old = dir.join("Artist").join("old album");
        let loose = old.join("x.wav");
        let one = old.join("CD1").join("a.wav");
        let two = old.join("CD2").join("b.wav");
        wav_at(&loose);
        wav_at(&one);
        wav_at(&two);
        file_at(&old.join("cover.jpg"), b"cover");
        file_at(&old.join("CD1").join("disc1.cue"), b"cue1");
        file_at(&old.join("CD2").join("disc2.cue"), b"cue2");
        let album = dir.join("Artist").join("Album");
        let diff = renames_diff(&[
            (&loose, &album.join("1-00 - X.wav")),
            (&one, &album.join("1-01 - A.wav")),
            (&two, &album.join("2-01 - B.wav")),
        ]);
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert!(out.sidecar_errors.is_empty(), "{:?}", out.sidecar_errors);
        assert!(album.join("cover.jpg").exists());
        assert!(album.join("disc1.cue").exists());
        assert!(album.join("disc2.cue").exists());
        assert!(!album.join("CD1").exists());
        assert!(!album.join("CD2").exists());
        assert!(!old.exists());
    }

    #[test]
    fn a_file_still_being_written_is_left_where_its_writer_expects_it() {
        // `11 - Last.flac.part` is a download in progress (a browser, rsync,
        // our own rip). Moving it, or removing its folder, fails the
        // writer's final rename; it stays, the folder stays, and the apply
        // says so.
        let dir = fresh_dir("sidecars-in-progress");
        let rip = dir.join("inbox").join("rip");
        let src = rip.join("10 - Done.wav");
        wav_at(&src);
        file_at(&rip.join("11 - Last.flac.part"), b"half");
        file_at(&rip.join("cover.jpg"), b"cover");
        let album = dir.join("Artist").join("Album");
        let out = apply_release_diff(&rename_only_diff(&src, &album.join("10 - Done.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(rip.join("11 - Last.flac.part").exists());
        assert!(!album.join("11 - Last.flac.part").exists());
        assert!(album.join("cover.jpg").exists());
        assert!(
            out.sidecar_errors
                .iter()
                .any(|e| e.contains("11 - Last.flac.part") && e.contains("still being written")),
            "{:?}",
            out.sidecar_errors
        );
    }

    #[test]
    fn a_subfolder_kept_for_a_file_left_in_place_is_reported_once() {
        // `Scans/` stays because of the download inside it. That is one
        // thing left behind, not two: no "remove Scans: Directory not
        // empty" on top of the line that names the file.
        let dir = fresh_dir("sidecars-in-progress-nested");
        let rip = dir.join("inbox").join("rip");
        let src = rip.join("10 - Done.wav");
        wav_at(&src);
        file_at(&rip.join("Scans").join("back.png.part"), b"half");
        let album = dir.join("Artist").join("Album");
        let out = apply_release_diff(&rename_only_diff(&src, &album.join("10 - Done.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(rip.join("Scans").join("back.png.part").exists());
        assert_eq!(out.sidecar_errors.len(), 1, "{:?}", out.sidecar_errors);
        assert!(out.sidecar_errors[0].contains("still being written"));
    }

    #[test]
    fn os_junk_is_carried_like_any_other_sidecar() {
        // Dropping it would be a delete; a folder left holding only junk
        // would never go. It moves, and a later cleanup can drop it.
        let dir = fresh_dir("sidecars-junk");
        let rip = dir.join("inbox").join("rip");
        let src = rip.join("a.wav");
        wav_at(&src);
        file_at(&rip.join("Thumbs.db"), b"junk");
        let album = dir.join("Artist").join("Album");
        let out = apply_release_diff(&rename_only_diff(&src, &album.join("01 - A.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(out.sidecar_errors.is_empty(), "{:?}", out.sidecar_errors);
        assert_eq!(std::fs::read(album.join("Thumbs.db")).unwrap(), b"junk");
        assert!(!rip.exists());
    }

    #[test]
    fn artist_level_files_follow_to_the_artist_level() {
        // `artist.jpg` sits beside the artist's albums, not inside one.
        let dir = fresh_dir("sidecars-artist-level");
        let one = dir.join("alice in chains").join("Dirt").join("a.wav");
        wav_at(&one);
        file_at(&dir.join("alice in chains").join("artist.jpg"), b"portrait");
        wav_at(&dir.join("Alice In Chains").join("Facelift").join("b.wav"));
        let album = dir.join("Alice In Chains").join("Dirt");
        let out = apply_release_diff(&rename_only_diff(&one, &album.join("01 - A.wav")), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(dir.join("Alice In Chains").join("artist.jpg").exists());
        assert!(!album.join("artist.jpg").exists());
        assert!(!dir.join("alice in chains").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_in_a_vacated_folder_is_left_alone_not_followed() {
        // `loose/loop` points back at `loose`. Following it would recurse
        // until the stack overflowed, in the middle of an apply.
        let dir = fresh_dir("sidecars-symlink-loop");
        let loose = dir.join("loose");
        let src = loose.join("a.wav");
        wav_at(&src);
        std::os::unix::fs::symlink(&loose, loose.join("loop")).unwrap();
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);

        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(dest.exists());
        assert!(loose.join("loop").symlink_metadata().is_ok(), "link stays");
        assert!(
            out.sidecar_errors.iter().any(|e| e.contains("symlink")),
            "{:?}",
            out.sidecar_errors
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_of_audio_is_not_this_folders_audio() {
        // The link is left where it is, and the music it points at is
        // not touched or counted.
        let dir = fresh_dir("sidecars-symlink-audio");
        let elsewhere = dir.join("elsewhere");
        wav_at(&elsewhere.join("keep.wav"));
        let loose = dir.join("loose");
        let src = loose.join("a.wav");
        wav_at(&src);
        std::os::unix::fs::symlink(&elsewhere, loose.join("linked")).unwrap();
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);

        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(elsewhere.join("keep.wav").exists());
        assert!(loose.join("linked").symlink_metadata().is_ok());
        assert!(
            out.sidecar_errors.iter().any(|e| e.contains("symlink")),
            "{:?}",
            out.sidecar_errors
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_sidecar_folder_that_cannot_be_read_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dir("sidecars-unreadable");
        let src = dir.join("loose").join("a.wav");
        wav_at(&src);
        let scans = src.with_file_name("Scans");
        file_at(&scans.join("back.png"), b"back");
        std::fs::set_permissions(&scans, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&scans).is_err();
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        std::fs::set_permissions(&scans, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        if unreadable {
            // Not so when the tests run as root.
            assert!(scans.join("back.png").exists());
            assert!(
                out.sidecar_errors.iter().any(|e| e.contains("Scans")),
                "{:?}",
                out.sidecar_errors
            );
        }
    }

    #[test]
    fn a_folder_tree_above_the_album_is_not_hauled_along() {
        // Only loose files of a folder above the album follow. A sibling
        // folder with no audio in it (documents, artwork for something
        // else) is not this album's.
        let dir = fresh_dir("sidecars-above");
        let src = dir.join("unsorted").join("rip").join("a.wav");
        wav_at(&src);
        file_at(&dir.join("unsorted").join("docs").join("readme.txt"), b"x");
        file_at(&dir.join("unsorted").join("list.txt"), b"y");
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(dir
            .join("unsorted")
            .join("docs")
            .join("readme.txt")
            .exists());
        assert!(dir.join("unsorted").join("list.txt").exists());
        assert!(!dir.join("unsorted").join("rip").exists());
    }

    #[test]
    fn sidecars_follow_an_album_out_of_the_inbox_but_the_inbox_itself_is_left() {
        let base = fresh_dir("sidecars-inbox");
        let music = base.join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let inbox = crate::library_layout::default_inbox_dir(&music).unwrap();
        let dropped = inbox.join("some rip").join("a.wav");
        let loose = inbox.join("b.wav");
        wav_at(&dropped);
        wav_at(&loose);
        file_at(&dropped.with_file_name("cover.jpg"), b"cover");
        file_at(&inbox.join("notes.txt"), b"mine");
        let album = music.join("Artist").join("Album");
        let diff = renames_diff(&[
            (&dropped, &album.join("01 - A.wav")),
            (&loose, &album.join("02 - B.wav")),
        ]);
        let out = apply_release_diff(&diff, &music);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert!(album.join("cover.jpg").exists());
        assert!(!inbox.join("some rip").exists());
        assert!(
            inbox.join("notes.txt").exists(),
            "a file loose in the inbox stays"
        );
    }

    /// `(src, dest)` that are two spellings of ONE file, differing by
    /// Unicode normalisation rather than ASCII case. APFS/HFS+ resolve
    /// both names to the same entry on their own; elsewhere a hard link
    /// stands in, so `same_inode` is true on every filesystem.
    #[cfg(unix)]
    fn same_file_two_unicode_spellings(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = fresh_dir(name);
        let src = dir
            .join("Beyonce\u{301}")
            .join("Album")
            .join("01 - Track.wav");
        let dest = dir
            .join("Beyonc\u{e9}")
            .join("Album")
            .join("01 - Track.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_sine_wav(&src, 1);
        if !dest.exists() {
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::hard_link(&src, &dest).unwrap();
        }
        assert!(same_inode(&src, &dest));
        (src, dest)
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_src_and_dest_are_left_alone_and_reported() {
        // `rename` between two links of one file does nothing. Reporting
        // it as a move would drop `src` from the library while it is
        // still on disk.
        let (src, dest) = same_file_two_unicode_spellings("same-inode-no-replace");
        let root = src.ancestors().nth(3).unwrap().to_path_buf();
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &root);
        let err = out.results[0].as_ref().unwrap_err();
        assert!(err.contains("hard links"), "{err}");
        assert!(out.rename_map.is_empty(), "{:?}", out.rename_map);
        assert!(src.exists() && dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_case_variant_leaves_no_temp_name() {
        let dir = fresh_dir("hard-link-case");
        let src = dir.join("Artist").join("Album").join("a.wav");
        let dest = src.with_file_name("A.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_sine_wav(&src, 1);
        if std::fs::hard_link(&src, &dest).is_err() {
            return; // case-folding volume: `A.wav` is `a.wav`
        }
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_err(), "{:?}", out.results[0]);
        let mut names: Vec<_> = std::fs::read_dir(src.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["A.wav", "a.wav"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_symlink_is_not_moved_to_another_folder() {
        // Moved as a link it would point at nothing: gone from the inbox
        // and never listed in the library.
        let dir = fresh_dir("relative-symlink");
        let stash = dir.join("stash").join("x.wav");
        let src = dir.join("inbox").join("drop.wav");
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        std::fs::create_dir_all(stash.parent().unwrap()).unwrap();
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_sine_wav(&stash, 1);
        std::os::unix::fs::symlink("../stash/x.wav", &src).unwrap();
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        let err = out.results[0].as_ref().unwrap_err();
        assert!(err.contains("relative symlink"), "{err}");
        assert!(src.is_file(), "the link still resolves where it was");
        assert!(!dest.exists() && !dest.parent().unwrap().exists());
    }

    /// `Artist/Album` holding `01 Song.wav` (`extra_secs` long) beside the
    /// already-canonical `01 - Song.wav` (one second), and a diff that
    /// lists both: the first renamed onto the second, the second staying.
    fn second_file_beside_canonical(
        name: &str,
        extra_secs: u32,
    ) -> (PathBuf, PathBuf, PathBuf, ReleaseTagDiff) {
        let dir = fresh_dir(name);
        let album = dir.join("Artist").join("Album");
        std::fs::create_dir_all(&album).unwrap();
        let extra = album.join("01 Song.wav");
        let in_place = album.join("01 - Song.wav");
        write_sine_wav(&extra, extra_secs);
        write_sine_wav(&in_place, 1);
        let mut diff = rename_only_diff(&extra, &in_place);
        diff.tracks.push(TrackTagDiff {
            src_path: in_place.clone(),
            dest_path: None,
            library_id: 2,
            fields: vec![FieldDiff {
                kind: FieldKind::Identity,
                name: "Title",
                current: None,
                proposed: Some("Song".into()),
                enabled: true,
            }],
        });
        (dir, extra, in_place, diff)
    }

    #[test]
    fn a_failed_move_is_reported_even_when_the_tag_write_failed_first() {
        let dir = fresh_dir("move-and-tags-fail");
        // Not audio: the tag write fails. A regular file where the dest
        // folder would go: the move fails too.
        let src = dir.join("Drop").join("01 Song.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"not audio").unwrap();
        let blocker = dir.join("Artist");
        std::fs::write(&blocker, b"in the way").unwrap();
        let dest = blocker.join("Album").join("01 - Song.wav");

        let mut diff = rename_only_diff(&src, &dest);
        diff.tracks[0].fields.push(FieldDiff {
            kind: FieldKind::Identity,
            name: "Title",
            current: None,
            proposed: Some("Song".into()),
            enabled: true,
        });
        let out = apply_release_diff(&diff, &dir);

        assert!(src.exists() && out.rename_map.is_empty());
        let error = out.results[0].as_ref().unwrap_err();
        let tag_error = write_track_tags(&diff.tracks[0]).unwrap_err();
        let move_error = error
            .strip_suffix(&format!("; its tags were not written either: {tag_error}"))
            .unwrap_or_else(|| panic!("the tag error is kept: {error}"));
        assert!(
            !move_error.is_empty() && move_error != tag_error,
            "and the reason the file is still in place leads: {error}"
        );
    }

    #[test]
    fn rename_waits_for_the_batch_track_holding_its_dest_to_move_out() {
        // `a` is listed first but lands on `b`'s current path; `b` has to
        // move on to `c` before `a` may take its place.
        let dir = fresh_dir("rename-chain");
        let (a, b, c) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("c.wav"));
        write_sine_wav(&a, 1);
        write_sine_wav(&b, 2);
        let (a_len, b_len) = (a.metadata().unwrap().len(), b.metadata().unwrap().len());
        let mut diff = rename_only_diff(&a, &b);
        diff.tracks.push(rename_only_diff(&b, &c).tracks.remove(0));

        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert!(!a.exists());
        assert_eq!(b.metadata().unwrap().len(), a_len, "a's audio is now at b");
        assert_eq!(c.metadata().unwrap().len(), b_len, "b's audio is now at c");
    }

    #[test]
    fn rename_never_lands_on_a_batch_track_that_failed_to_move_out() {
        // `b` cannot move (a regular file sits where its dest folder would
        // go), so `a` must not be renamed over it.
        let dir = fresh_dir("rename-chain-blocked");
        let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
        write_sine_wav(&a, 1);
        write_sine_wav(&b, 2);
        std::fs::write(dir.join("blocker"), b"in the way").unwrap();
        let c = dir.join("blocker").join("c.wav");
        let b_len = b.metadata().unwrap().len();
        let mut diff = rename_only_diff(&a, &b);
        diff.tracks.push(rename_only_diff(&b, &c).tracks.remove(0));

        let out = apply_release_diff(&diff, &dir);
        assert!(out.results[1].is_err(), "b has nowhere to go");
        let err = out.results[0].as_ref().expect_err("a's dest is still held");
        assert!(err.contains("collision"), "{err}");
        assert!(a.exists());
        assert_eq!(b.metadata().unwrap().len(), b_len, "b keeps its audio");
        assert!(out.rename_map.is_empty());
    }

    #[test]
    fn a_chain_ending_on_a_held_dest_still_resolves() {
        // `b` is bound for `c`, which a file outside the batch holds, so
        // `b` lands beside it. That frees `b`'s path for `a`.
        let dir = fresh_dir("rename-chain-beside");
        let (a, b, c) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("c.wav"));
        write_sine_wav(&a, 1);
        write_sine_wav(&b, 2);
        write_sine_wav(&c, 3);
        let (a_len, b_len, c_len) = (
            a.metadata().unwrap().len(),
            b.metadata().unwrap().len(),
            c.metadata().unwrap().len(),
        );
        let mut diff = rename_only_diff(&a, &b);
        diff.tracks.push(rename_only_diff(&b, &c).tracks.remove(0));

        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert_eq!(c.metadata().unwrap().len(), c_len, "c is untouched");
        assert_eq!(numbered(&c, 2).metadata().unwrap().len(), b_len);
        assert_eq!(b.metadata().unwrap().len(), a_len, "a's audio is now at b");
        assert!(!a.exists());
    }

    #[test]
    fn multi_disc_pairing_without_disc_numbers_prefers_the_title() {
        let mut rel = make_release("Album", "Artist");
        let mut disc2 = rel.media[0].clone();
        disc2.position = Some(2);
        disc2.tracks[0].id = "trk-d2-1".into();
        disc2.tracks[0].title = "Third".into();
        disc2.tracks[1].id = "trk-d2-2".into();
        disc2.tracks[1].title = "Fourth".into();
        rel.media.push(disc2);

        // Disc 2 track 1, tagged with a track number but no disc number.
        let lib = make_lib_track(Path::new("/m/x.wav"), "Third", 1);
        let (medium, track) = pair_to_mb_track_across_media(&lib, &rel.media).unwrap();
        assert_eq!((medium.position, track.title.as_str()), (Some(2), "Third"));

        // No title to go on: the first disc's track N, as before.
        let lib = make_lib_track(Path::new("/m/y.wav"), "Untitled", 1);
        let (medium, track) = pair_to_mb_track_across_media(&lib, &rel.media).unwrap();
        assert_eq!((medium.position, track.title.as_str()), (Some(1), "First"));

        // A single-disc release still pairs by number over title.
        let single = make_release("Album", "Artist");
        let lib = make_lib_track(Path::new("/m/z.wav"), "Second", 1);
        let (_, track) = pair_to_mb_track_across_media(&lib, &single.media).unwrap();
        assert_eq!(track.title, "First");
    }

    #[test]
    fn the_exact_spelling_on_disk_wins_over_a_case_variant() {
        // Only a case-sensitive volume can hold both folders.
        let dir = fresh_dir("adopt-exact");
        if volume_folds_ascii_case(&dir) {
            return;
        }
        let src = dir
            .join("alice in chains")
            .join("Dirt")
            .join("01 - Them Bones.wav");
        let facelift = dir
            .join("Alice In Chains")
            .join("Facelift")
            .join("01 - We Die Young.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::create_dir_all(facelift.parent().unwrap()).unwrap();
        write_sine_wav(&src, 1);
        write_sine_wav(&facelift, 1);
        let dest = dir
            .join("Alice In Chains")
            .join("Dirt")
            .join("01 - Them Bones.wav");

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert_eq!(out.rename_map.get(&src), Some(&dest));
        assert!(dest.exists() && facelift.exists());
        assert!(
            !dir.join("alice in chains").exists(),
            "the folder the file left is pruned"
        );
    }

    #[test]
    fn reread_paths_skip_sources_left_in_the_inbox() {
        let music = PathBuf::from("/data/Music");
        let inbox = crate::library_layout::default_inbox_dir(&music).unwrap();
        let filed = (inbox.join("a.mp3"), music.join("A").join("B").join("a.mp3"));
        let stuck = inbox.join("b.mp3");
        let retag = music.join("A").join("B").join("c.mp3");
        let mut diff = rename_only_diff(&filed.0, &filed.1);
        for src in [&stuck, &retag] {
            let mut t = rename_only_diff(src, src).tracks.remove(0);
            t.dest_path = None;
            diff.tracks.push(t);
        }
        let outcome = ApplyOutcome {
            results: vec![Ok(()), Ok(()), Ok(())],
            rename_map: HashMap::from([filed.clone()]),
            ..Default::default()
        };
        let paths = outcome.reread_paths(&diff, &music);
        assert!(paths.contains(&filed.1), "new location is read");
        assert!(paths.contains(&retag), "in-library retag is re-read");
        assert!(
            !paths.contains(&filed.0),
            "a moved source is never re-read; on a folding volume it still resolves"
        );
        assert_eq!(outcome.vacated(), vec![filed.0.clone()]);
        assert!(
            !paths.contains(&stuck),
            "a file still sitting in the inbox is outside the library"
        );
    }

    #[test]
    fn a_failed_move_leaves_no_empty_folder_behind() {
        let dir = fresh_dir("failed-move-no-folders");
        let src = dir.join("drop").join("01 song.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_sine_wav(&src, 1);
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        let diff = rename_only_diff(&src, &dest);
        // The source vanishes after the diff was built: the dest folders
        // get created, then the rename fails.
        std::fs::remove_file(&src).unwrap();

        let out = apply_release_diff(&diff, &dir);
        assert!(out.results[0].is_err());
        assert!(
            !dir.join("Artist").exists(),
            "folders made for a file that never arrived are removed again"
        );
    }

    /// A folder on a second filesystem, so a move from it into the temp
    /// dir takes the cross-device copy path. `None` where there is none.
    #[cfg(target_os = "linux")]
    fn other_filesystem_dir(name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        let here = std::fs::metadata(std::env::temp_dir()).ok()?.dev();
        if std::fs::metadata(shm).ok()?.dev() == here {
            return None;
        }
        let dir = shm.join("zytunes-tag-ops").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cross_device_move_carries_the_file_over() {
        let Some(other) = other_filesystem_dir("exdev-ok") else {
            return;
        };
        let dir = fresh_dir("exdev-ok");
        let src = other.join("01 song.wav");
        write_sine_wav(&src, 1);
        let dest = dir.join("Artist").join("01 - Song.wav");
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(dest.exists() && !src.exists());
        let _ = std::fs::remove_dir_all(&other);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cross_device_move_that_cannot_remove_the_source_leaves_one_copy() {
        use std::os::unix::fs::PermissionsExt;
        let Some(other) = other_filesystem_dir("exdev-stuck") else {
            return;
        };
        let dir = fresh_dir("exdev-stuck");
        let src = other.join("01 song.wav");
        write_sine_wav(&src, 1);
        let dest = dir.join("Artist").join("01 - Song.wav");
        // The source folder is read-only: the file can be copied out of
        // it but not removed from it.
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(out.results[0].is_err(), "the file did not move");
        assert!(out.rename_map.is_empty());
        assert!(src.exists(), "the source is still where it was");
        assert!(
            !dest.exists() && !dir.join("Artist").exists(),
            "and no second copy is left where the library does not know of it"
        );
        let _ = std::fs::remove_dir_all(&other);
    }

    /// The ` (n)` sibling of `path`: `01 - Song.wav` → `01 - Song (2).wav`.
    fn numbered(path: &Path, n: u32) -> PathBuf {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let ext = path.extension().unwrap().to_string_lossy();
        path.with_file_name(format!("{stem} ({n}).{ext}"))
    }

    #[test]
    fn a_file_bound_for_a_held_dest_lands_beside_it() {
        // Filing never replaces a track. The copy already in the library
        // stays exactly as it is and the incoming one is filed next to it,
        // so duplicates are dealt with separately from filing.
        let dir = fresh_dir("lands-beside");
        let feat_album = dir
            .join("2Pac featuring the Notorious B.I.G.")
            .join("Album");
        std::fs::create_dir_all(&feat_album).unwrap();
        let src = feat_album.join("01 - Track.wav");
        let dest = dir.join("2Pac").join("Album").join("01 - Track.wav");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        write_sine_wav(&src, 2);
        write_sine_wav(&dest, 1);
        let (incoming, in_place) = (std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());

        let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);

        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        let beside = numbered(&dest, 2);
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            in_place,
            "the copy in place is untouched"
        );
        assert_eq!(std::fs::read(&beside).unwrap(), incoming);
        assert!(!src.exists());
        assert_eq!(
            out.rename_map.get(&src),
            Some(&beside),
            "the map says where the file really went"
        );
        assert!(
            !feat_album.parent().unwrap().exists(),
            "the emptied feat folders are pruned"
        );
    }

    #[test]
    fn each_further_copy_takes_the_next_free_number() {
        let dir = fresh_dir("lands-beside-twice");
        let dest = dir.join("Artist").join("Album").join("01 - Track.wav");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        write_sine_wav(&dest, 1);
        for n in [2, 3] {
            let src = dir.join("drop").join("01 track.wav");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            write_sine_wav(&src, n);
            let out = apply_release_diff(&rename_only_diff(&src, &dest), &dir);
            assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
            assert_eq!(out.rename_map.get(&src), Some(&numbered(&dest, n)));
        }
        assert!(dest.exists() && numbered(&dest, 2).exists() && numbered(&dest, 3).exists());
    }

    #[test]
    fn move_no_replace_refuses_a_taken_name_and_touches_neither_file() {
        // The retry in `move_to_free_name` leans on this: a name that was
        // free when looked at and is taken by the time of the move.
        let dir = fresh_dir("no-replace-taken");
        let (src, dest) = (dir.join("a.wav"), dir.join("b.wav"));
        write_sine_wav(&src, 1);
        write_sine_wav(&dest, 2);
        let (mine, theirs) = (std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());

        assert!(matches!(
            move_no_replace(&src, &dest),
            Err(MoveFailure::Taken)
        ));
        assert_eq!(std::fs::read(&src).unwrap(), mine);
        assert_eq!(std::fs::read(&dest).unwrap(), theirs);
    }

    #[test]
    fn move_to_free_name_skips_every_taken_number() {
        let dir = fresh_dir("free-name-skips");
        let dest = dir.join("01 - Track.wav");
        let src = dir.join("in.wav");
        write_sine_wav(&src, 1);
        let incoming = std::fs::read(&src).unwrap();
        write_sine_wav(&dest, 2);
        write_sine_wav(&numbered(&dest, 2), 3);

        let landed = move_to_free_name(&src, &dest).unwrap();

        assert_eq!(landed, numbered(&dest, 3));
        assert_eq!(std::fs::read(&landed).unwrap(), incoming);
        assert!(!src.exists());
    }

    #[test]
    fn a_staged_copy_keeps_the_sources_mtime_and_leaves_no_staging_file() {
        let dir = fresh_dir("staged-copy");
        let (src, dest) = (dir.join("a.wav"), dir.join("b.wav"));
        write_sine_wav(&src, 1);
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 30);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        assert!(copy_no_replace(&src, &dest).is_ok());

        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
        let moved = std::fs::metadata(&dest).unwrap().modified().unwrap();
        let drift = moved
            .duration_since(long_ago)
            .unwrap_or_else(|e| e.duration());
        assert!(drift < std::time::Duration::from_secs(2), "{drift:?}");
        assert!(!dir.join("b.wav.zytunes-part").exists());
    }

    #[test]
    fn a_staged_copy_does_not_take_a_name_that_turned_up() {
        let dir = fresh_dir("staged-copy-taken");
        let (src, dest) = (dir.join("a.wav"), dir.join("b.wav"));
        write_sine_wav(&src, 1);
        write_sine_wav(&dest, 2);
        let theirs = std::fs::read(&dest).unwrap();

        assert!(matches!(
            copy_no_replace(&src, &dest),
            Err(MoveFailure::Taken)
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), theirs);
        assert!(!dir.join("b.wav.zytunes-part").exists());
    }

    #[test]
    fn only_old_staged_copies_are_swept() {
        let dir = fresh_dir("sweep-staged");
        let (old, fresh, other) = (
            dir.join("Old.flac.zytunes-part"),
            dir.join("New.flac.zytunes-part"),
            dir.join("cover.jpg"),
        );
        for p in [&old, &fresh, &other] {
            std::fs::write(p, b"x").unwrap();
        }
        let stale = std::time::SystemTime::now() - STAGED_MAX_AGE * 2;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(stale)
            .unwrap();
        // An old file of any other kind is not ours to remove.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(stale)
            .unwrap();

        sweep_stale_staged(&dir);

        assert!(!old.exists(), "the crash's leftover goes");
        assert!(fresh.exists(), "a copy that may still be running stays");
        assert!(other.exists());
    }

    #[test]
    fn a_later_track_asking_for_another_case_of_a_new_folder_joins_it() {
        // Neither spelling is on disk yet: the first track creates one and
        // the second must land in it, not in a second folder that differs
        // by case.
        let dir = fresh_dir("spelling-new-folder");
        std::fs::create_dir_all(dir.join("Other")).unwrap();
        let mut folders = FolderSpelling::default();

        let first = folders.adopt(&dir.join("Artist").join("Album").join("01.wav"), &dir);
        let second = folders.adopt(&dir.join("ARTIST").join("Album").join("02.wav"), &dir);

        assert_eq!(first, dir.join("Artist").join("Album").join("01.wav"));
        assert_eq!(second, dir.join("Artist").join("Album").join("02.wav"));
    }

    #[test]
    fn a_folder_on_disk_keeps_its_spelling() {
        let dir = fresh_dir("spelling-on-disk");
        std::fs::create_dir_all(dir.join("alice in chains")).unwrap();
        let mut folders = FolderSpelling::default();

        let dest = folders.adopt(
            &dir.join("Alice In Chains").join("Dirt").join("01.wav"),
            &dir,
        );

        assert_eq!(
            dest,
            dir.join("alice in chains").join("Dirt").join("01.wav")
        );
    }

    #[test]
    fn a_numbered_copy_stays_put_while_its_dest_is_held() {
        // Filing the album again proposes the canonical name for the
        // `(2)` copy too. It is already beside the track that holds that
        // name, so there is nothing to do: no move, no `(3)`.
        let dir = fresh_dir("numbered-stays");
        let dest = dir.join("Artist").join("Album").join("01 - Track.wav");
        let copy = numbered(&dest, 2);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        write_sine_wav(&dest, 1);
        write_sine_wav(&copy, 2);

        let out = apply_release_diff(&rename_only_diff(&copy, &dest), &dir);

        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert!(out.rename_map.is_empty(), "{:?}", out.rename_map);
        assert!(dest.exists() && copy.exists());
        assert!(!numbered(&dest, 3).exists());
    }

    #[test]
    fn a_numbered_copy_takes_the_canonical_name_once_it_is_free() {
        let dir = fresh_dir("numbered-promoted");
        let dest = dir.join("Artist").join("Album").join("01 - Track.wav");
        let copy = numbered(&dest, 2);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        write_sine_wav(&copy, 2);

        let out = apply_release_diff(&rename_only_diff(&copy, &dest), &dir);

        assert!(out.results[0].is_ok(), "{:?}", out.results[0]);
        assert_eq!(out.rename_map.get(&copy), Some(&dest));
        assert!(dest.exists() && !copy.exists());
    }

    #[test]
    fn a_failed_move_leaves_the_held_dest_untouched() {
        let dir = fresh_dir("held-dest-failed-move");
        let src = dir.join("feat").join("01 - Track.wav");
        let dest = dir.join("2Pac").join("01 - Track.wav");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        write_sine_wav(&src, 2);
        write_sine_wav(&dest, 1);
        let canonical = std::fs::read(&dest).unwrap();
        let diff = rename_only_diff(&src, &dest);
        // The source vanishes between the diff and the apply, so the move
        // cannot succeed.
        std::fs::remove_file(&src).unwrap();

        let out = apply_release_diff(&diff, &dir);
        assert!(out.results[0].is_err());
        assert!(out.rename_map.is_empty());
        assert_eq!(std::fs::read(&dest).unwrap(), canonical);
        assert!(!numbered(&dest, 2).exists());
    }

    #[test]
    fn a_file_held_under_another_spelling_of_the_folder_is_not_overwritten() {
        // `dest` does not exist by its own spelling, but the folder it
        // names is the one already on disk, and the copy is in it. Only a
        // case-sensitive volume can tell the difference.
        let dir = fresh_dir("case-uncovers-dest");
        if volume_folds_ascii_case(&dir) {
            return;
        }
        let album = dir.join("alice in chains").join("Dirt");
        std::fs::create_dir_all(&album).unwrap();
        let src = album.join("01 them bones.wav");
        let held = album.join("01 - Them Bones.wav");
        write_sine_wav(&src, 1);
        std::fs::write(&held, b"the copy already filed").unwrap();
        let dest = dir
            .join("Alice In Chains")
            .join("Dirt")
            .join("01 - Them Bones.wav");

        let outcome = apply_release_diff(&rename_only_diff(&src, &dest), &dir);

        assert!(outcome.results[0].is_ok(), "{:?}", outcome.results[0]);
        assert_eq!(std::fs::read(&held).unwrap(), b"the copy already filed");
        assert_eq!(outcome.rename_map.get(&src), Some(&numbered(&held, 2)));
        assert!(numbered(&held, 2).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_second_name_for_the_source_is_not_a_held_dest() {
        let (src, dest) = same_file_two_unicode_spellings("same-inode-held");
        let len = src.metadata().unwrap().len();
        let root = src.ancestors().nth(3).unwrap().to_path_buf();
        assert!(!dest_is_held(&src, &dest));
        let _ = apply_release_diff(&rename_only_diff(&src, &dest), &root);
        assert_eq!(dest.metadata().unwrap().len(), len, "audio must survive");
        assert!(!numbered(&dest, 2).exists());
    }

    #[test]
    fn a_duplicate_of_a_track_in_place_lands_beside_it() {
        // Two files of one album paired to the same MusicBrainz track and
        // the same length: a second copy. Both stay, side by side, and
        // both get the tags.
        let (dir, extra, in_place, diff) = second_file_beside_canonical("stay-duplicate", 1);
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        let beside = numbered(&in_place, 2);
        assert_eq!(out.rename_map.get(&extra), Some(&beside));
        assert!(in_place.exists() && beside.exists() && !extra.exists());
        let tagged = lofty::read_from_path(&in_place).unwrap();
        assert_eq!(
            tagged.primary_tag().and_then(|t| t.title()).as_deref(),
            Some("Song"),
            "the track in place still gets its tags"
        );
    }

    #[test]
    fn a_different_song_paired_to_a_held_track_lands_beside_it() {
        // Same album, same MusicBrainz track, different length. Whether it
        // is a copy or a wrong pairing is not filing's call: it lands
        // beside the track in place, and nothing is overwritten.
        let (dir, extra, in_place, diff) = second_file_beside_canonical("stay-different", 4);
        let extra_len = extra.metadata().unwrap().len();
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        let beside = numbered(&in_place, 2);
        assert_eq!(out.rename_map.get(&extra), Some(&beside));
        assert_eq!(beside.metadata().unwrap().len(), extra_len);
        assert!(in_place.exists() && !extra.exists());
    }

    /// A diff filing `a` and `b` (in that order) onto the same `dest`.
    fn two_files_one_dest(name: &str, b_secs: u32) -> (PathBuf, PathBuf, PathBuf, ReleaseTagDiff) {
        let dir = fresh_dir(name);
        let (a, b) = (
            dir.join("drop").join("a.wav"),
            dir.join("drop").join("b.wav"),
        );
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        write_sine_wav(&a, 1);
        write_sine_wav(&b, b_secs);
        let dest = dir.join("Artist").join("Album").join("01 - Song.wav");
        let mut diff = rename_only_diff(&a, &dest);
        diff.tracks.extend(rename_only_diff(&b, &dest).tracks);
        (dir, a, b, diff)
    }

    #[test]
    fn two_copies_filed_together_land_side_by_side() {
        // Two rips of one album dropped in the inbox pair every track
        // twice. Both copies are filed: the first under the canonical
        // name, the second beside it.
        let (dir, a, b, diff) = two_files_one_dest("two-copies", 1);
        let dest = diff.tracks[0].dest_path.clone().unwrap();
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert_eq!(out.rename_map.get(&a), Some(&dest));
        assert_eq!(out.rename_map.get(&b), Some(&numbered(&dest, 2)));
    }

    #[test]
    fn two_different_songs_bound_for_one_dest_both_land() {
        let (dir, a, b, diff) = two_files_one_dest("two-songs", 5);
        let dest = diff.tracks[0].dest_path.clone().unwrap();
        let out = apply_release_diff(&diff, &dir);
        assert!(out.results.iter().all(Result::is_ok), "{:?}", out.results);
        assert_eq!(out.rename_map.get(&a), Some(&dest));
        assert_eq!(out.rename_map.get(&b), Some(&numbered(&dest, 2)));
        assert!(!a.exists() && !b.exists());
    }
}
