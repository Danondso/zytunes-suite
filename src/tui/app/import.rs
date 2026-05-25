//! CD import overlay state.
//!
//! The overlay is a modal — when `App::import_overlay` is `Some`, all key
//! events route into the overlay's own dispatcher and the rest of the TUI
//! is read-only behind it. Phase 2 builds the state machine and renderer;
//! Phase 3 wires `Enter` to the actual rip pipeline. Today `Enter`
//! produces a toast and closes the overlay.
//!
//! The overlay holds *its own copy* of the identified-disc data rather
//! than borrowing from `App.cd.last_status`. This lets the user keep the
//! overlay open while a later CD-detect poll arrives — without it, a
//! benign re-detect would wipe the selection state mid-edit.

use std::collections::BTreeMap;

use zytunes::cd::discid::DiscToc;
use zytunes::cd::drive::CdDrive;
use zytunes::cd::rip::RipFidelity;
use zytunes::musicbrainz::{render_artist_credit, Release, Track as MbTrack};

/// Which subsection of the overlay currently has keyboard focus. The
/// overlay's key dispatcher consults this to interpret arrow keys,
/// `Space`, `[`/`]`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportField {
    /// The track-toggle list. `Up`/`Down` moves the row cursor; `Space`
    /// toggles the row; `a`/`n` selects all/none.
    Tracks,
    /// The fidelity picker. `Left`/`Right` (or `f`/`F`) cycles.
    Fidelity,
    /// The alternate-match picker. `[`/`]` cycles `release_idx`.
    AlternateMatch,
    /// Auto-eject toggle. `Space` flips it.
    AutoEject,
}

impl ImportField {
    pub fn next(self) -> Self {
        match self {
            ImportField::Tracks => ImportField::Fidelity,
            ImportField::Fidelity => ImportField::AlternateMatch,
            ImportField::AlternateMatch => ImportField::AutoEject,
            ImportField::AutoEject => ImportField::Tracks,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ImportField::Tracks => ImportField::AutoEject,
            ImportField::Fidelity => ImportField::Tracks,
            ImportField::AlternateMatch => ImportField::Fidelity,
            ImportField::AutoEject => ImportField::AlternateMatch,
        }
    }
}

/// Live state for the CD import overlay.
#[derive(Debug, Clone)]
pub struct ImportOverlay {
    /// Drive the disc is in. Carried in for the Phase 3 rip command.
    #[allow(dead_code)]
    pub drive: CdDrive,
    /// Disc TOC — Phase 3 hands this to the rip pipeline for length
    /// estimation and per-track LBA bounds.
    #[allow(dead_code)]
    pub toc: DiscToc,
    /// MusicBrainz disc ID. Phase 3 surfaces this as a small footer label
    /// so the user can copy it when submitting unknown discs to MB.
    #[allow(dead_code)]
    pub mb_disc_id: String,
    /// All releases MB returned, primary first. The user picks one via
    /// `release_idx`. Non-public: invariants (non-empty, `release_idx <
    /// releases.len()`) are upheld by the impl methods; external code
    /// reads through `release_count` / `current_release`.
    pub(super) releases: Vec<Release>,
    /// Index into `releases`. Always valid (`< releases.len()`).
    pub release_idx: usize,
    /// Per-track include flag, keyed by track position from the chosen
    /// release. Reset when `release_idx` changes (different release =
    /// different track count) — see [`Self::switch_release`].
    pub track_selection: BTreeMap<u32, bool>,
    /// Cursor row within the track list (for arrow-key navigation).
    pub track_cursor: usize,
    /// Index into [`RipFidelity::all`].
    pub fidelity_idx: usize,
    /// Whether to eject the drive when the rip succeeds.
    pub auto_eject: bool,
    /// Currently focused subsection.
    pub focus: ImportField,
}

impl ImportOverlay {
    /// Open with a primary release as the initial selection. All tracks
    /// from `primary` start selected; `alternates` populate the picker.
    pub fn new(
        drive: CdDrive,
        toc: DiscToc,
        mb_disc_id: String,
        primary: Release,
        alternates: Vec<Release>,
        default_fidelity: RipFidelity,
        auto_eject_default: bool,
    ) -> Self {
        let mut releases = Vec::with_capacity(1 + alternates.len());
        releases.push(primary);
        releases.extend(alternates);

        let fidelity_idx = RipFidelity::all()
            .iter()
            .position(|f| *f == default_fidelity)
            .unwrap_or(0);

        let mut overlay = ImportOverlay {
            drive,
            toc,
            mb_disc_id,
            releases,
            release_idx: 0,
            track_selection: BTreeMap::new(),
            track_cursor: 0,
            fidelity_idx,
            auto_eject: auto_eject_default,
            focus: ImportField::Tracks,
        };
        overlay.reset_selection_for_current_release();
        overlay
    }

    /// Total number of releases the user can cycle through. External
    /// callers (e.g. the renderer) read this instead of poking
    /// `releases.len()` directly so the non-empty invariant stays
    /// enforceable inside the impl.
    pub fn release_count(&self) -> usize {
        self.releases.len()
    }

    /// The release the user is currently looking at.
    ///
    /// `releases` is non-empty post-construction (`new()` always pushes
    /// `primary` first) and `release_idx` is only mutated via the
    /// `next_release`/`prev_release` helpers which apply modular
    /// arithmetic. With `releases` now `pub(super)` external code can't
    /// `.clear()` it, so the invariant `release_idx < releases.len()`
    /// holds for the lifetime of an overlay. `debug_assert!` catches
    /// any future regression that breaks it from inside the impl.
    pub fn current_release(&self) -> &Release {
        debug_assert!(
            !self.releases.is_empty(),
            "ImportOverlay::releases is empty — `new()` guarantees a primary entry"
        );
        let idx = self.release_idx.min(self.releases.len().saturating_sub(1));
        &self.releases[idx]
    }

    /// Tracks of the current release (drawn from the first medium with any
    /// tracks). A real audio CD has exactly one medium; multi-disc box
    /// sets get the first one and a UI follow-up for Phase 3+.
    pub fn current_tracks(&self) -> &[MbTrack] {
        self.current_release()
            .media
            .iter()
            .find(|m| !m.tracks.is_empty())
            .map(|m| m.tracks.as_slice())
            .unwrap_or(&[])
    }

    /// Display string: "Artist — Album". Used in the overlay header.
    pub fn current_release_label(&self) -> String {
        let r = self.current_release();
        let artist = render_artist_credit(&r.artist_credit);
        format!("{artist} — {}", r.title)
    }

    /// Number of tracks the user has selected for import.
    pub fn selected_count(&self) -> usize {
        self.track_selection.values().filter(|v| **v).count()
    }

    /// Toggle the selection for the track under the cursor.
    ///
    /// Seeded entries flip normally; unseeded entries (an MB track whose
    /// `position` we somehow didn't initialise in `reset_selection_for_
    /// current_release`) default to `false` so the first toggle *selects*
    /// the track. The previous form (`or_insert(true)` then `!*entry`)
    /// did the opposite, leaving the user confused about why a fresh
    /// toggle silently deselected.
    pub fn toggle_current_track(&mut self) {
        let Some(position) = self.cursor_track_position() else {
            return;
        };
        let entry = self.track_selection.entry(position).or_insert(false);
        *entry = !*entry;
    }

    pub fn select_all(&mut self) {
        for v in self.track_selection.values_mut() {
            *v = true;
        }
    }

    pub fn select_none(&mut self) {
        for v in self.track_selection.values_mut() {
            *v = false;
        }
    }

    /// Move the cursor up by one row (saturating at 0).
    pub fn cursor_up(&mut self) {
        self.track_cursor = self.track_cursor.saturating_sub(1);
    }

    /// Move the cursor down by one row (clamped to last row).
    pub fn cursor_down(&mut self) {
        let n = self.current_tracks().len();
        if n == 0 {
            self.track_cursor = 0;
            return;
        }
        self.track_cursor = (self.track_cursor + 1).min(n - 1);
    }

    /// Cycle to the next release in the picker. Wraps at the end so the
    /// user can spin through a multi-pressing list either direction.
    pub fn next_release(&mut self) {
        if self.releases.len() <= 1 {
            return;
        }
        self.release_idx = (self.release_idx + 1) % self.releases.len();
        self.switch_release();
    }

    pub fn prev_release(&mut self) {
        if self.releases.len() <= 1 {
            return;
        }
        self.release_idx = if self.release_idx == 0 {
            self.releases.len() - 1
        } else {
            self.release_idx - 1
        };
        self.switch_release();
    }

    /// Cycle to the next fidelity option.
    pub fn next_fidelity(&mut self) {
        let n = RipFidelity::all().len();
        self.fidelity_idx = (self.fidelity_idx + 1) % n;
    }

    pub fn prev_fidelity(&mut self) {
        let n = RipFidelity::all().len();
        self.fidelity_idx = if self.fidelity_idx == 0 {
            n - 1
        } else {
            self.fidelity_idx - 1
        };
    }

    /// Current fidelity selection.
    pub fn current_fidelity(&self) -> RipFidelity {
        RipFidelity::all()[self.fidelity_idx]
    }

    /// Track positions the user wants to import, in CD order.
    pub fn selected_track_positions(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .track_selection
            .iter()
            .filter_map(|(pos, selected)| if *selected { Some(*pos) } else { None })
            .collect();
        out.sort_unstable();
        out
    }

    fn switch_release(&mut self) {
        self.track_cursor = 0;
        self.reset_selection_for_current_release();
    }

    fn reset_selection_for_current_release(&mut self) {
        // Snapshot positions first to avoid co-borrowing `self` immutably
        // (via `current_tracks`) and mutably (via `track_selection`).
        // Tracks with `position = None` fall back to `(index + 1)` so the
        // renderer (which uses the same fallback) and the cursor handler
        // agree on what "the track at row N" means — without this the
        // user would see unpositioned rows that Space silently no-op'd on.
        let positions: Vec<u32> = self
            .current_tracks()
            .iter()
            .enumerate()
            .map(|(i, t)| effective_position(t, i))
            .filter(|p| *p > 0)
            .collect();
        self.track_selection.clear();
        for p in positions {
            self.track_selection.insert(p, true);
        }
    }

    /// Returns the position the renderer would show for the track at the
    /// cursor row. Falls back to row-index-based numbering when MB didn't
    /// provide a `position` — must match `effective_position` so the
    /// handler agrees with the renderer.
    fn cursor_track_position(&self) -> Option<u32> {
        let tracks = self.current_tracks();
        let row = tracks.get(self.track_cursor)?;
        Some(effective_position(row, self.track_cursor))
    }
}

/// Compute the user-visible position for a track row.
///
/// MB usually provides `position` on every track, but for releases where
/// the field is missing the renderer falls back to `index + 1` (so the
/// first row reads as "1.", second as "2.", etc.). Centralised here so
/// `reset_selection_for_current_release`, `cursor_track_position`, and
/// the renderer all use the same value — the agent review of #76 caught
/// that the renderer and handler disagreed before this helper existed.
pub fn effective_position(track: &MbTrack, index: usize) -> u32 {
    track.position.unwrap_or((index as u32) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zytunes::cd::discid::{DiscToc, TocTrack};
    use zytunes::cd::drive::CdDrive;
    use zytunes::musicbrainz::{ArtistCredit, Medium, Release, Track as MbTrack};

    fn drive() -> CdDrive {
        CdDrive {
            path: std::path::PathBuf::from("/dev/disk4"),
            name: "Optical Drive".into(),
            media_present: Some(true),
        }
    }

    fn toc() -> DiscToc {
        DiscToc {
            first_track: 1,
            last_track: 3,
            lead_out_lba: 200_000,
            tracks: vec![
                TocTrack {
                    number: 1,
                    offset_lba: 150,
                },
                TocTrack {
                    number: 2,
                    offset_lba: 20_000,
                },
                TocTrack {
                    number: 3,
                    offset_lba: 80_000,
                },
            ],
        }
    }

    fn release(title: &str, artist: &str, n_tracks: u32) -> Release {
        Release {
            id: format!("rel-{title}"),
            title: title.into(),
            date: None,
            country: None,
            artist_credit: vec![ArtistCredit {
                name: artist.into(),
                joinphrase: None,
                artist: None,
            }],
            media: vec![Medium {
                position: Some(1),
                format: Some("CD".into()),
                track_count: Some(n_tracks),
                tracks: (1..=n_tracks)
                    .map(|i| MbTrack {
                        id: format!("trk-{title}-{i}"),
                        number: i.to_string(),
                        position: Some(i),
                        title: format!("Track {i}"),
                        length: None,
                        recording: None,
                        artist_credit: vec![],
                    })
                    .collect(),
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

    fn open() -> ImportOverlay {
        ImportOverlay::new(
            drive(),
            toc(),
            "discid".into(),
            release("Primary", "Artist", 3),
            vec![release("Alt One", "Artist", 4)],
            RipFidelity::Mp3V0,
            true,
        )
    }

    #[test]
    fn opens_with_all_tracks_selected() {
        let o = open();
        assert_eq!(o.selected_count(), 3);
        assert_eq!(o.selected_track_positions(), vec![1, 2, 3]);
    }

    #[test]
    fn toggling_current_track_flips_it() {
        let mut o = open();
        o.cursor_down(); // cursor on track 2
        o.toggle_current_track();
        assert_eq!(o.selected_count(), 2);
        assert_eq!(o.selected_track_positions(), vec![1, 3]);
        o.toggle_current_track();
        assert_eq!(o.selected_count(), 3);
    }

    /// Regression: a toggle of a track whose position was never seeded in
    /// `track_selection` should *select* the track, not deselect it. The
    /// old `or_insert(true)` + flip wrote `false` on first toggle.
    #[test]
    fn toggling_unseeded_track_selects_not_deselects() {
        let mut o = open();
        // Drop the position-2 entry so the next toggle exercises the
        // unseeded path.
        o.track_selection.remove(&2);
        // Cursor on track 2 (row index 1).
        o.cursor_down();
        let before = o.selected_count();
        o.toggle_current_track();
        let after = o.selected_count();
        assert_eq!(
            after,
            before + 1,
            "first toggle on unseeded entry should select, not deselect"
        );
    }

    /// Regression: a track with `position = None` is still selectable.
    /// The renderer and `cursor_track_position` both fall back to
    /// `(index + 1)` via `effective_position`, so the cursor lands on
    /// the right row and the user can toggle it.
    #[test]
    fn unpositioned_track_is_selectable_via_index_fallback() {
        // Build a release where MB returned a track with `position = None`.
        let mut rel = release("Compilation", "Various", 2);
        rel.media[0].tracks[1].position = None;

        let mut o = ImportOverlay::new(
            drive(),
            toc(),
            "discid".into(),
            rel,
            vec![],
            RipFidelity::Mp3V0,
            true,
        );
        // Both tracks should be selected by default — including the one
        // with no MB position.
        assert_eq!(o.selected_count(), 2);
        // Move cursor to the unpositioned row and toggle it off.
        o.cursor_down();
        o.toggle_current_track();
        assert_eq!(o.selected_count(), 1);
        // Toggling back on must work too.
        o.toggle_current_track();
        assert_eq!(o.selected_count(), 2);
    }

    #[test]
    fn cursor_clamps_at_bounds() {
        let mut o = open();
        o.cursor_up(); // already at 0, stays
        assert_eq!(o.track_cursor, 0);
        for _ in 0..10 {
            o.cursor_down();
        }
        assert_eq!(o.track_cursor, 2); // last row of a 3-track release
    }

    #[test]
    fn select_all_and_none_round_trip() {
        let mut o = open();
        o.select_none();
        assert_eq!(o.selected_count(), 0);
        o.select_all();
        assert_eq!(o.selected_count(), 3);
    }

    #[test]
    fn switching_release_resets_selection() {
        let mut o = open();
        o.select_none(); // 0 selected on primary
        o.next_release(); // jump to alternate (4 tracks)
        assert_eq!(o.current_tracks().len(), 4);
        // Selection rebuilds to all-on for the new release.
        assert_eq!(o.selected_count(), 4);
        // Cursor resets to 0 so it doesn't dangle out of range.
        assert_eq!(o.track_cursor, 0);
    }

    #[test]
    fn release_picker_wraps_in_both_directions() {
        let mut o = open();
        o.next_release(); // 0 → 1
        assert_eq!(o.release_idx, 1);
        o.next_release(); // 1 → 0 (wrap)
        assert_eq!(o.release_idx, 0);
        o.prev_release(); // 0 → 1 (wrap backwards)
        assert_eq!(o.release_idx, 1);
    }

    #[test]
    fn release_picker_no_op_with_single_release() {
        let mut o = ImportOverlay::new(
            drive(),
            toc(),
            "d".into(),
            release("Only", "Artist", 2),
            vec![],
            RipFidelity::Flac,
            false,
        );
        o.next_release();
        assert_eq!(o.release_idx, 0);
        o.prev_release();
        assert_eq!(o.release_idx, 0);
    }

    #[test]
    fn fidelity_picker_cycles_and_wraps() {
        let mut o = open();
        let start = o.fidelity_idx;
        let n = RipFidelity::all().len();
        // Advance N times — full revolution, lands on the same entry.
        for _ in 0..n {
            o.next_fidelity();
        }
        assert_eq!(o.fidelity_idx, start);
        // Backward also wraps.
        o.prev_fidelity();
        assert_eq!(o.fidelity_idx, (start + n - 1) % n);
    }

    #[test]
    fn fidelity_defaults_to_passed_in_value() {
        let o = ImportOverlay::new(
            drive(),
            toc(),
            "d".into(),
            release("Primary", "Artist", 1),
            vec![],
            RipFidelity::Flac,
            true,
        );
        assert_eq!(o.current_fidelity(), RipFidelity::Flac);
    }

    #[test]
    fn focus_cycles_through_all_fields() {
        let order = [
            ImportField::Tracks,
            ImportField::Fidelity,
            ImportField::AlternateMatch,
            ImportField::AutoEject,
        ];
        for (i, &f) in order.iter().enumerate() {
            assert_eq!(f.next(), order[(i + 1) % order.len()]);
            assert_eq!(f.prev(), order[(i + order.len() - 1) % order.len()]);
        }
    }

    #[test]
    fn release_label_renders_artist_and_title() {
        let o = open();
        let label = o.current_release_label();
        assert!(label.contains("Primary"));
        assert!(label.contains("Artist"));
    }
}
