//! Tag-manager overlay state.
//!
//! Holds the in-flight state of an interactive MusicBrainz-driven retag.
//! Pure state — no rendering, no I/O. The renderer lives in `ui/mod.rs`
//! (`draw_tag_manager_overlay`) and the worker plumbing lives in
//! `background.rs` (`MbSearchReleases` / `MbReleaseDetails` / `ApplyTagDiff` /
//! `RereadLibraryPaths`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use zytunes::musicbrainz::ReleaseSearchHit;
use zytunes::tag_ops::{DiffScope, ReleaseTagDiff};

/// One navigable row in the diff overlay. Headers and field rows share the
/// same focus index so `c` can target a track regardless of whether the
/// user is currently sitting on the header or on one of its fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusRow {
    /// Header for `tracks[ti]`. Always present when the track has at least
    /// one field; the only thing the user can do here is press `c` to fold
    /// the track. Cannot be toggled with Space.
    Header(usize),
    /// `tracks[ti].fields[fi]`. Skipped from the flattened list when the
    /// parent track is collapsed.
    Field(usize, usize),
}

impl FocusRow {
    /// The owning track index for this row — needed by `c` so collapsing
    /// works whether focus is on the header or any of its child fields.
    pub fn track_index(self) -> usize {
        match self {
            FocusRow::Header(ti) | FocusRow::Field(ti, _) => ti,
        }
    }
}

/// Process-wide monotonic counter for MB worker request tokens. Used so
/// tokens stay distinct across overlay opens — a per-overlay counter
/// would reset to 0 on each open and collide with the previous overlay's
/// in-flight request.
static REQUEST_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Where the overlay is in its workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagManagerPhase {
    /// User is editing the artist / album query strings before firing the
    /// search. Reachable from `Esc` in `SearchResults`.
    SearchInput,
    /// Search is in flight — waiting for the worker to return
    /// `MbSearchResults`.
    SearchPending,
    /// Worker returned hits; user is picking one with arrow keys.
    SearchResults,
    /// Full-release lookup is in flight (selected a hit, waiting on details).
    LoadingRelease,
    /// Diff is rendered; user toggles fields with Space, applies with Enter.
    DiffPreview,
    /// Apply is in flight — disabled all keys except a final "ack" once Done.
    Applying,
    /// Apply succeeded. Any key closes.
    Done,
    /// A non-recoverable error occurred (worker error). `error` carries the
    /// human-readable reason. Any key closes.
    Error,
}

/// Which input field has focus inside the `SearchInput` phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchInputField {
    Artist,
    Album,
}

/// Anchors a selection across a library reload. Captured at overlay open
/// from the current TUI selection, then rewritten to point at the
/// *post-rename* target before the library re-reads.
#[derive(Debug, Clone, Default)]
pub struct SelectionAnchor {
    pub artist: String,
    pub album: Option<String>,
    pub track_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TagManagerOverlay {
    pub phase: TagManagerPhase,
    pub scope: DiffScope,
    /// Library state captured at overlay-open time. Used to redraw the diff
    /// against the original library tracks regardless of background re-scans.
    pub source_artist: String,
    pub source_album: String,
    pub source_track_name: Option<String>,
    /// Live query strings — editable while in `SearchInput`.
    pub query_artist: String,
    pub query_album: String,
    pub search_input_field: SearchInputField,
    pub search_hits: Vec<ReleaseSearchHit>,
    pub hit_idx: usize,
    pub diff: Option<ReleaseTagDiff>,
    /// Flattened row index into the diff body for `Up`/`Down`/`Space`/`c`.
    /// Indexes into [`Self::flattened_rows`].
    pub focused_row: usize,
    pub error: Option<String>,
    pub anchor: SelectionAnchor,
    /// All navigable rows in the diff (track headers + visible field rows),
    /// in display order. Field rows for collapsed tracks are omitted so
    /// `move_focus` skips over them naturally. Re-computed whenever `diff`
    /// or [`Self::collapsed_tracks`] changes.
    pub flattened_rows: Vec<FocusRow>,
    /// Track indices the user has folded with `c`. Field rows from these
    /// tracks are hidden in [`Self::flattened_rows`] and in the renderer,
    /// but the track header still shows so the user can `c` again to
    /// expand. Cleared on every new diff load.
    pub collapsed_tracks: HashSet<usize>,
    /// Once the apply finishes we may have a `rename_map` — the post-apply
    /// follow-up (library reread, anchor restore) needs both this and the
    /// list of source paths.
    pub last_rename_map: std::collections::HashMap<PathBuf, PathBuf>,
    /// Token stamped onto every outgoing MB worker request so we can drop
    /// stale responses. Incremented on each request; the worker echoes
    /// it back on the matching response, and `accepts_token` is the
    /// gate.
    ///
    /// Without this, the close-then-reopen race delivers the previous
    /// overlay's search hits into a freshly-opened overlay because the
    /// dispatcher just checks `tag_manager.is_some()`.
    pub pending_request_token: u64,
    /// Top AcoustID UUID for the current overlay session, captured when an
    /// `AcoustIdLookup` returns one or more hits. Used to populate the
    /// `ACOUSTID_ID` field in the diff so the apply writes it back as a
    /// Picard-canonical TXXX frame. `None` for overlays that resolved via
    /// the MBID-direct or MB-search paths (no AcoustID hit available).
    pub acoustid_uuid: Option<String>,
}

impl TagManagerOverlay {
    pub fn new(
        scope: DiffScope,
        source_artist: String,
        source_album: String,
        source_track_name: Option<String>,
        anchor: SelectionAnchor,
    ) -> Self {
        Self {
            phase: TagManagerPhase::SearchPending,
            scope,
            query_artist: source_artist.clone(),
            query_album: source_album.clone(),
            source_artist,
            source_album,
            source_track_name,
            search_input_field: SearchInputField::Artist,
            search_hits: Vec::new(),
            hit_idx: 0,
            diff: None,
            focused_row: 0,
            error: None,
            anchor,
            flattened_rows: Vec::new(),
            collapsed_tracks: HashSet::new(),
            last_rename_map: std::collections::HashMap::new(),
            pending_request_token: 0,
            acoustid_uuid: None,
        }
    }

    /// Bump the request token and return the new value. Callers stamp this
    /// onto outgoing `BgCommand::Mb*` requests so the response handler can
    /// drop stale events from previous opens. The counter is process-wide
    /// so a re-opened overlay starts ahead of every previous overlay's
    /// in-flight request.
    pub fn next_request_token(&mut self) -> u64 {
        self.pending_request_token = REQUEST_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.pending_request_token
    }

    /// Returns true if `token` matches the most recently issued request.
    /// Used to fence stale MB worker responses arriving against a re-opened
    /// overlay or a superseded query.
    pub fn accepts_token(&self, token: u64) -> bool {
        token == self.pending_request_token
    }

    /// Recompute [`Self::flattened_rows`] from the current diff and
    /// collapse state. Empty-field tracks are skipped entirely (no header,
    /// no fields) so a track that already matches MB on every modelled
    /// field doesn't take up a row just to show "0 changes".
    pub fn rebuild_flattened_paths(&mut self) {
        self.flattened_rows.clear();
        let Some(diff) = &self.diff else {
            return;
        };
        for (ti, t) in diff.tracks.iter().enumerate() {
            if t.fields.is_empty() {
                continue;
            }
            self.flattened_rows.push(FocusRow::Header(ti));
            if !self.collapsed_tracks.contains(&ti) {
                for (fi, _) in t.fields.iter().enumerate() {
                    self.flattened_rows.push(FocusRow::Field(ti, fi));
                }
            }
        }
        self.focused_row = self
            .focused_row
            .min(self.flattened_rows.len().saturating_sub(1));
    }

    /// Toggle the `enabled` flag on the currently focused field. No-op for
    /// unchanged fields (current == proposed) — toggling them would still
    /// be a no-op at apply time, but a flipped `[x]` would mislead the
    /// user into thinking they queued a real change. Also a no-op when
    /// focus is on a track header — headers carry no toggle state.
    pub fn toggle_focused_field(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let Some(&FocusRow::Field(ti, fi)) = self.flattened_rows.get(self.focused_row) else {
            return;
        };
        if let Some(track) = diff.tracks.get_mut(ti) {
            if let Some(field) = track.fields.get_mut(fi) {
                if field.current != field.proposed {
                    field.enabled = !field.enabled;
                }
            }
        }
    }

    /// Fold or unfold the track that owns the focused row. Operates on
    /// header rows AND field rows — the user shouldn't have to scroll to
    /// the header to collapse a track they're already viewing. After
    /// toggling, focus snaps to the track header so the user can see the
    /// fold state change and keep collapsing siblings with the same key.
    pub fn toggle_collapse_focused_track(&mut self) {
        let Some(&row) = self.flattened_rows.get(self.focused_row) else {
            return;
        };
        let ti = row.track_index();
        if !self.collapsed_tracks.remove(&ti) {
            self.collapsed_tracks.insert(ti);
        }
        self.rebuild_flattened_paths();
        // Snap focus to this track's header so repeated `c` presses on
        // sibling tracks work without re-finding the user's row.
        if let Some(pos) = self
            .flattened_rows
            .iter()
            .position(|r| matches!(r, FocusRow::Header(h) if *h == ti))
        {
            self.focused_row = pos;
        }
    }

    /// Expand every collapsed track. Cheap escape hatch from a fully-folded
    /// view that the user wants to scan in full again.
    pub fn expand_all_tracks(&mut self) {
        if self.collapsed_tracks.is_empty() {
            return;
        }
        self.collapsed_tracks.clear();
        self.rebuild_flattened_paths();
    }

    /// Fold every track. Useful on long releases where the user only
    /// wants to scan headers to find which tracks have real deltas.
    pub fn collapse_all_tracks(&mut self) {
        let Some(diff) = &self.diff else {
            return;
        };
        for (ti, t) in diff.tracks.iter().enumerate() {
            if !t.fields.is_empty() {
                self.collapsed_tracks.insert(ti);
            }
        }
        self.rebuild_flattened_paths();
    }

    /// Set `enabled` to `on` for every *changed* field on every track.
    /// Unchanged fields stay disabled — `a`/`n` are about queuing real
    /// writes, not toggling no-ops.
    pub fn set_all_enabled(&mut self, on: bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        for t in &mut diff.tracks {
            for f in &mut t.fields {
                if f.current != f.proposed {
                    f.enabled = on;
                }
            }
        }
    }

    pub fn move_focus(&mut self, delta: isize) {
        if self.flattened_rows.is_empty() {
            self.focused_row = 0;
            return;
        }
        let max = self.flattened_rows.len().saturating_sub(1);
        let next = self.focused_row as isize + delta;
        self.focused_row = next.clamp(0, max as isize) as usize;
    }

    /// Jump focus to the first / last navigable row. Used by Home / End in
    /// the diff overlay so long album diffs are navigable without holding j/k.
    pub fn focus_first(&mut self) {
        self.focused_row = 0;
    }

    pub fn focus_last(&mut self) {
        self.focused_row = self.flattened_rows.len().saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay_with_field_count(n: usize) -> TagManagerOverlay {
        let mut overlay = TagManagerOverlay::new(
            DiffScope::Album,
            "A".into(),
            "B".into(),
            None,
            SelectionAnchor::default(),
        );
        // Build a synthetic flattened index — actual diff content isn't
        // needed for these focus-movement tests.
        // Construct a synthetic row list: one Header at index 0, n Field
        // rows below. Tests that care about field-index semantics index
        // 1..=n, so the offsets stay easy to reason about.
        overlay.flattened_rows = std::iter::once(FocusRow::Header(0))
            .chain((0..n).map(|i| FocusRow::Field(0, i)))
            .collect();
        overlay
    }

    #[test]
    fn focus_first_jumps_to_index_zero() {
        let mut overlay = overlay_with_field_count(50);
        overlay.focused_row = 37;
        overlay.focus_first();
        assert_eq!(overlay.focused_row, 0);
    }

    #[test]
    fn focus_last_jumps_to_final_index() {
        // 1 header + 50 fields = 51 navigable rows, last index = 50.
        let mut overlay = overlay_with_field_count(50);
        overlay.focus_last();
        assert_eq!(overlay.focused_row, 50);
    }

    #[test]
    fn focus_last_handles_empty_flattened() {
        let mut overlay = overlay_with_field_count(0);
        overlay.focus_last();
        assert_eq!(overlay.focused_row, 0); // saturating sub keeps it sane
    }

    #[test]
    fn next_request_token_invalidates_prior_token() {
        // The ApplyTagDiff fence depends on this: when the user closes
        // mid-apply and reopens, the new overlay bumps the token, and the
        // in-flight TagsApplied/LibraryRereadComplete must fail
        // `accepts_token`. Counter is process-wide so the new token is
        // strictly greater than the old.
        let mut overlay = overlay_with_field_count(0);
        let t_apply = overlay.next_request_token();
        assert!(overlay.accepts_token(t_apply));

        let t_reopen = overlay.next_request_token();
        assert_ne!(t_apply, t_reopen);
        assert!(overlay.accepts_token(t_reopen));
        assert!(
            !overlay.accepts_token(t_apply),
            "stale apply token must be fenced after reopen"
        );
    }

    #[test]
    fn toggle_focused_field_no_ops_on_unchanged_row() {
        // Unchanged rows (current == proposed) are surfaced for context only.
        // Toggling them would still apply nothing at write-time, but a
        // ticked `[x]` next to an unchanged row would mislead the user.
        use zytunes::tag_ops::{FieldDiff, FieldKind, ReleaseTagDiff, TrackTagDiff};
        let mut overlay = overlay_with_field_count(0);
        overlay.diff = Some(ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "summary".into(),
            tracks: vec![TrackTagDiff {
                src_path: std::path::PathBuf::from("/tmp/x.flac"),
                dest_path: None,
                library_id: 1,
                fields: vec![
                    FieldDiff {
                        kind: FieldKind::Identity,
                        name: "Title",
                        current: Some("Same".into()),
                        proposed: Some("Same".into()),
                        enabled: false,
                    },
                    FieldDiff {
                        kind: FieldKind::Identity,
                        name: "Artist",
                        current: Some("Old".into()),
                        proposed: Some("New".into()),
                        enabled: true,
                    },
                ],
            }],
        });
        overlay.rebuild_flattened_paths();

        // flattened_rows is now [Header(0), Field(0,0), Field(0,1)] — the
        // header sits at index 0, fields at indices 1 and 2.

        // Header itself: toggle does nothing.
        overlay.focused_row = 0;
        overlay.toggle_focused_field();
        let fields = &overlay.diff.as_ref().unwrap().tracks[0].fields;
        assert!(!fields[0].enabled);
        assert!(fields[1].enabled);

        // Focus the unchanged Title row — toggling must be a no-op.
        overlay.focused_row = 1;
        overlay.toggle_focused_field();
        let title = &overlay.diff.as_ref().unwrap().tracks[0].fields[0];
        assert!(!title.enabled, "unchanged row must stay disabled");

        // Focus the changed Artist row — toggling flips it as before.
        overlay.focused_row = 2;
        overlay.toggle_focused_field();
        let artist = &overlay.diff.as_ref().unwrap().tracks[0].fields[1];
        assert!(!artist.enabled, "changed row toggles normally");
    }

    #[test]
    fn set_all_enabled_only_touches_changed_rows() {
        use zytunes::tag_ops::{FieldDiff, FieldKind, ReleaseTagDiff, TrackTagDiff};
        let mut overlay = overlay_with_field_count(0);
        overlay.diff = Some(ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "summary".into(),
            tracks: vec![TrackTagDiff {
                src_path: std::path::PathBuf::from("/tmp/x.flac"),
                dest_path: None,
                library_id: 1,
                fields: vec![
                    FieldDiff {
                        kind: FieldKind::Identity,
                        name: "Unchanged",
                        current: Some("Same".into()),
                        proposed: Some("Same".into()),
                        enabled: false,
                    },
                    FieldDiff {
                        kind: FieldKind::Identity,
                        name: "Changed",
                        current: Some("Old".into()),
                        proposed: Some("New".into()),
                        enabled: false,
                    },
                ],
            }],
        });

        overlay.set_all_enabled(true);
        let fields = &overlay.diff.as_ref().unwrap().tracks[0].fields;
        assert!(!fields[0].enabled, "unchanged row must remain disabled");
        assert!(fields[1].enabled, "changed row should be enabled by `a`");
    }

    #[test]
    fn move_focus_page_sized_jumps_clamp() {
        // 1 header + 15 fields = 16 navigable rows, last idx = 15.
        let mut overlay = overlay_with_field_count(15);
        overlay.focused_row = 5;
        overlay.move_focus(10); // PageDown
        assert_eq!(overlay.focused_row, 15);
        overlay.move_focus(-10); // PageUp
        assert_eq!(overlay.focused_row, 5);
        overlay.move_focus(-100); // PageUp past start clamps
        assert_eq!(overlay.focused_row, 0);
    }

    fn diff_with_two_nonempty_tracks() -> ReleaseTagDiff {
        use zytunes::tag_ops::{FieldDiff, FieldKind, TrackTagDiff};
        let track = |path: &str| TrackTagDiff {
            src_path: PathBuf::from(path),
            dest_path: None,
            library_id: 1,
            fields: vec![
                FieldDiff {
                    kind: FieldKind::Identity,
                    name: "Title",
                    current: Some("Old".into()),
                    proposed: Some("New".into()),
                    enabled: true,
                },
                FieldDiff {
                    kind: FieldKind::Identity,
                    name: "Artist",
                    current: Some("Same".into()),
                    proposed: Some("Same".into()),
                    enabled: false,
                },
            ],
        };
        ReleaseTagDiff {
            release_mbid: "rel-1".into(),
            summary: "summary".into(),
            tracks: vec![track("/tmp/a.flac"), track("/tmp/b.flac")],
        }
    }

    #[test]
    fn toggle_collapse_hides_field_rows_for_focused_track() {
        // With two tracks of two fields each, flattened_rows is:
        //   [H(0), F(0,0), F(0,1), H(1), F(1,0), F(1,1)]
        // Collapsing track 0 must drop F(0,*) and shrink the list.
        let mut overlay = overlay_with_field_count(0);
        overlay.diff = Some(diff_with_two_nonempty_tracks());
        overlay.rebuild_flattened_paths();
        assert_eq!(overlay.flattened_rows.len(), 6);

        // Focus a field within track 0, then collapse.
        overlay.focused_row = 1; // F(0, 0)
        overlay.toggle_collapse_focused_track();
        assert!(overlay.collapsed_tracks.contains(&0));
        assert_eq!(overlay.flattened_rows.len(), 4);
        // Focus snaps to the now-collapsed track's header so a follow-up
        // `c` toggles the same track back open.
        assert_eq!(
            overlay.flattened_rows[overlay.focused_row],
            FocusRow::Header(0)
        );

        // Expand again — full set restored.
        overlay.toggle_collapse_focused_track();
        assert!(!overlay.collapsed_tracks.contains(&0));
        assert_eq!(overlay.flattened_rows.len(), 6);
    }

    #[test]
    fn collapse_all_then_expand_all() {
        let mut overlay = overlay_with_field_count(0);
        overlay.diff = Some(diff_with_two_nonempty_tracks());
        overlay.rebuild_flattened_paths();
        overlay.collapse_all_tracks();
        // Only headers visible — 2 rows.
        assert_eq!(overlay.flattened_rows.len(), 2);
        assert!(overlay
            .flattened_rows
            .iter()
            .all(|r| matches!(r, FocusRow::Header(_))));
        overlay.expand_all_tracks();
        assert_eq!(overlay.flattened_rows.len(), 6);
    }
}
