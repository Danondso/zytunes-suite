//! Build and apply tag/filename diffs against library files.
//!
//! Pure module — no TUI dependency. Takes a slice of library `Track`s plus
//! a MusicBrainz `Release`, computes a `ReleaseTagDiff` describing every
//! field that would change, and applies it via lofty + `std::fs::rename`.
//!
//! Powers the `m` "tag manager" overlay in the TUI: the overlay surfaces
//! the diff for user toggle / approval, then routes the approved
//! `ReleaseTagDiff` through the background worker which calls
//! [`apply_release_diff`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::ItemKey;
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagType};

use crate::cd::metadata::{
    probe_by_content, ripped_track_destination, set_string, set_unknown_string,
};
use crate::library::Track;
use crate::musicbrainz::{render_artist_credit, Medium, Release, Track as MbTrack};

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
    let media = tag
        .get_string(&ItemKey::OriginalMediaType)
        .map(|s| s.to_string());
    // Picard TXXX fields are stored as `ItemKey::Unknown(description)`.
    // Match case-insensitively because some legacy taggers used title-cased
    // descriptions ("MusicBrainz Album Type" vs "MUSICBRAINZ_ALBUMTYPE")
    // and lofty surfaces both verbatim.
    let mut musicbrainz_album_type = None;
    let mut musicbrainz_album_status = None;
    let mut musicbrainz_album_packaging = None;
    let mut release_country = None;
    let mut script = None;
    let mut acoustid_fingerprint = None;
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
        let slot = if name.eq_ignore_ascii_case("MUSICBRAINZ_ALBUMTYPE")
            || name.eq_ignore_ascii_case("MusicBrainz Album Type")
        {
            &mut musicbrainz_album_type
        } else if name.eq_ignore_ascii_case("MUSICBRAINZ_ALBUMSTATUS")
            || name.eq_ignore_ascii_case("MusicBrainz Album Status")
        {
            &mut musicbrainz_album_status
        } else if name.eq_ignore_ascii_case("MUSICBRAINZ_ALBUMPACKAGING")
            || name.eq_ignore_ascii_case("MusicBrainz Album Packaging")
        {
            &mut musicbrainz_album_packaging
        } else if name.eq_ignore_ascii_case("RELEASECOUNTRY")
            || name.eq_ignore_ascii_case("MusicBrainz Album Release Country")
        {
            &mut release_country
        } else if name.eq_ignore_ascii_case("SCRIPT") {
            &mut script
        } else if name.eq_ignore_ascii_case("ACOUSTID_FINGERPRINT")
            || name.eq_ignore_ascii_case("Acoustid Fingerprint")
        {
            &mut acoustid_fingerprint
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

impl ReleaseTagDiff {
    /// Returns `true` if at least one enabled field across all tracks would
    /// change something. Used by the overlay to gate the Enter→Apply branch.
    pub fn has_any_enabled(&self) -> bool {
        self.tracks
            .iter()
            .any(|t| t.fields.iter().any(|f| f.enabled))
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

    let mut out: Vec<TrackTagDiff> = Vec::new();
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
            Some(ripped_track_destination(
                music_root, release, mb_track, position, &extension,
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

        out.push(TrackTagDiff {
            src_path: location,
            dest_path,
            library_id: lib.id,
            fields,
        });
    }

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
///      track doesn't carry a disc number).
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
    // 3. Track-number fallback, any medium. Ambiguous on multi-disc releases
    // when the lib side has no disc — accept the first hit and move on.
    if let Some(n) = lib.track_number {
        for medium in media {
            if let Some(m) = medium.tracks.iter().find(|m| m.position == Some(n)) {
                return Some((medium, m));
            }
        }
    }
    // 4. Title fallback.
    for medium in media {
        if let Some(m) = medium
            .tracks
            .iter()
            .find(|m| m.title.eq_ignore_ascii_case(&lib.name))
        {
            return Some((medium, m));
        }
    }
    None
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
    let album_artist = render_artist_credit(&release.artist_credit);
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

    // Album Type (Picard MUSICBRAINZ_ALBUMTYPE): "Album" / "Single" / "EP" / ...
    let album_type = release
        .release_group
        .as_ref()
        .and_then(|rg| rg.primary_type.clone());
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
        release.status.clone(),
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
    // ACOUSTID_ID is the parent AcoustID UUID — only available when
    // we resolved via the AcoustID fingerprint path. Skipped when the
    // overlay reached this point through MBID-direct or MB-search.
    // (Closure-based push first so the &mut fields borrow it captures
    // ends before the direct fields.push below.)
    if let Some(uuid) = acoustid_uuid.filter(|s| !s.is_empty()) {
        push(
            FieldKind::Picard,
            "ACOUSTID_ID",
            None,
            Some(uuid.to_string()),
        );
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

/// Apply an approved `ReleaseTagDiff` to disk.
///
/// Three phases:
///   0. **Pre-flight** — detect rename collisions BEFORE touching disk.
///      Any track whose proposed dest collides (with another track in the
///      batch or with a pre-existing file outside the batch) gets recorded
///      as `Err` and is skipped by both subsequent phases — so we never
///      end up with new tags written at the old path while the rename
///      half failed.
///   1. **Tag writes** — open each surviving `src_path` via lofty and apply
///      every enabled non-`Filename` field.
///   2. **Renames** — `std::fs::rename` the surviving entries (falling
///      back to copy+remove on `EXDEV`).
///
/// Collision policy: when two batch entries propose the same dest, BOTH
/// are skipped with a collision error. Picking one as the "winner" by
/// iteration order would be arbitrary and lossy — better to surface the
/// conflict so the user disables one in the overlay.
///
/// Returns `(per-track results, rename_map)`. The rename map covers only
/// successful renames; callers feed both sets into
/// [`crate::dirlib::DirectoryLibrary::reread_paths`] so the cache picks up
/// new tags AND new locations.
pub fn apply_release_diff(
    diff: &ReleaseTagDiff,
) -> (Vec<Result<(), String>>, HashMap<PathBuf, PathBuf>) {
    let mut results: Vec<Result<(), String>> = vec![Ok(()); diff.tracks.len()];
    let mut rename_map: HashMap<PathBuf, PathBuf> = HashMap::new();

    // Index tracks-with-rename by src_path. The collision check works in src
    // space because that's what `results` is keyed on.
    let renaming: Vec<(usize, &PathBuf, &PathBuf)> = diff
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| match &t.dest_path {
            Some(dest)
                if t.fields
                    .iter()
                    .any(|f| f.kind == FieldKind::Filename && f.enabled) =>
            {
                Some((i, &t.src_path, dest))
            }
            _ => None,
        })
        .collect();

    let mut dest_count: HashMap<&PathBuf, usize> = HashMap::new();
    for (_, _, d) in &renaming {
        *dest_count.entry(*d).or_insert(0) += 1;
    }
    let source_set: std::collections::HashSet<&PathBuf> =
        renaming.iter().map(|(_, s, _)| *s).collect();

    // Phase 0: mark all colliders as Err and remove them from the work set.
    let mut skipped: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut to_rename: Vec<(usize, &PathBuf, &PathBuf)> = Vec::new();
    for &(idx, src, dest) in &renaming {
        if dest_count.get(dest).copied().unwrap_or(0) > 1 {
            results[idx] = Err(format!(
                "rename collision: two tracks would land at {}",
                dest.display()
            ));
            skipped.insert(idx);
            continue;
        }
        if dest.exists() && !source_set.contains(dest) && dest != src {
            results[idx] = Err(format!(
                "rename collision: {} already exists",
                dest.display()
            ));
            skipped.insert(idx);
            continue;
        }
        to_rename.push((idx, src, dest));
    }

    // Phase 1: tag writes. Skip tracks marked as collided in phase 0 so we
    // don't leave new tags at an old path that we'll never rename.
    for (i, track) in diff.tracks.iter().enumerate() {
        if skipped.contains(&i) {
            continue;
        }
        if let Err(e) = write_track_tags(track) {
            results[i] = Err(e);
        }
    }

    // Phase 2: renames. Tag-write failures don't block the rename — the file
    // still moves with whatever tags it has. The reverse direction (skip
    // rename on tag-write failure) would leave the file in the wrong
    // canonical location AND with old tags, which is the worse failure mode.
    for (idx, src, dest) in to_rename {
        match perform_rename(src, dest) {
            Ok(()) => {
                rename_map.insert(src.clone(), dest.clone());
            }
            Err(e) => {
                if results[idx].is_ok() {
                    results[idx] = Err(e);
                }
            }
        }
    }

    (results, rename_map)
}

fn write_track_tags(track: &TrackTagDiff) -> Result<(), String> {
    let to_apply: Vec<&FieldDiff> = track
        .fields
        .iter()
        .filter(|f| f.enabled && f.kind != FieldKind::Filename)
        .collect();
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
    let src = &track.src_path;
    let tmp = tagtmp_path(src);
    // Clean up a leftover from a prior crash before copying.
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(src, &tmp)
        .map_err(|e| format!("tag-save: copy {} -> {}: {e}", src.display(), tmp.display()))?;

    let result = write_then_swap(&tmp, src, &to_apply);
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
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .tag_mut(tag_type)
        .ok_or_else(|| "lofty refused to attach a tag".to_string())?;
    for field in to_apply {
        apply_field(tag, field);
    }
    tagged
        .save_to_path(tmp, WriteOptions::default())
        .map_err(|e| format!("lofty save failed on {}: {e}", src.display()))?;
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

fn perform_rename(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    match std::fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Cross-device rename — fall back to copy + remove. The kind we
            // check for varies by platform; match on raw error code 18 for
            // Linux/macOS EXDEV, and fall back to a string check otherwise.
            #[cfg(unix)]
            let is_exdev = e.raw_os_error() == Some(18);
            #[cfg(not(unix))]
            let is_exdev = false;
            if is_exdev {
                std::fs::copy(src, dest).map_err(|e| {
                    format!(
                        "rename {} -> {}: cross-device copy failed: {e}",
                        src.display(),
                        dest.display()
                    )
                })?;
                std::fs::remove_file(src)
                    .map_err(|e| format!("rename cleanup of {}: {e}", src.display()))?;
                Ok(())
            } else {
                Err(format!(
                    "rename {} -> {} failed: {e}",
                    src.display(),
                    dest.display()
                ))
            }
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

        let (results, _rename_map) = apply_release_diff(&diff);
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

        let (results, _) = apply_release_diff(&diff);
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

        let (results, rename_map) = apply_release_diff(&diff);
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

        let (results, _) = apply_release_diff(&diff);
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

        let (results, rename_map) = apply_release_diff(&diff);
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
    fn apply_release_diff_detects_rename_collision() {
        let dir = fresh_dir("collision");
        // Two source files; both want to land at the same new path.
        let src1 = dir.join("a.wav");
        let src2 = dir.join("b.wav");
        write_sine_wav(&src1, 1);
        write_sine_wav(&src2, 1);
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

        let (results, rename_map) = apply_release_diff(&diff);
        // Both should fail with a collision error — neither side is treated
        // as the winner. Picking arbitrarily would silently overwrite.
        let collisions = results
            .iter()
            .filter(|r| r.as_ref().err().is_some_and(|e| e.contains("collision")))
            .count();
        assert_eq!(
            collisions, 2,
            "expected both srcs to fail, got: {results:?}"
        );
        assert!(rename_map.is_empty(), "no rename should succeed");
        // Source files must remain untouched at their original paths.
        assert!(src1.exists(), "src1 should still exist");
        assert!(src2.exists(), "src2 should still exist");
        assert!(!dest.exists(), "dest must not be created");
    }

    #[test]
    fn apply_release_diff_skips_tag_write_on_collision() {
        // Regression: previously, tag writes ran in phase 1 BEFORE the
        // collision check in phase 2 — so a colliding rename left the
        // source file with new tags at the OLD path while results said
        // "collision". Verify tags do NOT get written when a collision
        // is detected.
        let dir = fresh_dir("collision-skip-tag");
        let src1 = dir.join("a.wav");
        let src2 = dir.join("b.wav");
        write_sine_wav(&src1, 1);
        write_sine_wav(&src2, 1);
        let dest = dir.join("c.wav");

        let diff = ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "test".into(),
            tracks: vec![
                TrackTagDiff {
                    src_path: src1.clone(),
                    dest_path: Some(dest.clone()),
                    library_id: 1,
                    fields: vec![
                        FieldDiff {
                            kind: FieldKind::Identity,
                            name: "Album",
                            current: Some("Old".into()),
                            proposed: Some("New Album From MB".into()),
                            enabled: true,
                        },
                        FieldDiff {
                            kind: FieldKind::Filename,
                            name: "Filename",
                            current: Some(src1.display().to_string()),
                            proposed: Some(dest.display().to_string()),
                            enabled: true,
                        },
                    ],
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

        let (_results, _rename_map) = apply_release_diff(&diff);
        // src1 had an enabled Album field — but because its rename collided
        // in phase 0, phase 1 must have skipped it. Verify no Album tag was
        // written to src1.
        let tagged = lofty::probe::read_from_path(&src1).unwrap();
        let id3_album = tagged
            .tag(lofty::tag::TagType::Id3v2)
            .and_then(|t| t.album().map(|s| s.to_string()));
        assert!(
            id3_album.is_none() || id3_album.as_deref() == Some(""),
            "src1 should NOT have a written Album tag — phase 1 must skip collided entries; got {id3_album:?}"
        );
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
        let (results, _renames) = apply_release_diff(&diff1);
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
        let (results, _) = apply_release_diff(&diff);
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
            find("ACOUSTID_FINGERPRINT").as_deref(),
            Some("AQADtBR=writeMe")
        );
        assert_eq!(find("ACOUSTID_ID").as_deref(), Some("uuid-to-write"));
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
}
