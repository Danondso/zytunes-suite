//! Tag-manager overlay state.
//!
//! Holds the in-flight state of an interactive MusicBrainz-driven retag.
//! Pure state — no rendering, no I/O. The renderer lives in `ui/mod.rs`
//! (`draw_tag_manager_overlay`) and the worker plumbing lives in
//! `background.rs` (`MbSearchReleases` / `MbReleaseDetails` / `ApplyTagDiff` /
//! `RereadLibraryPaths`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use zytunes::musicbrainz::ReleaseSearchHit;
use zytunes::tag_ops::{DiffScope, ReleaseTagDiff};

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
    /// Flattened row index into the diff body for `Up`/`Down`/`Space`.
    /// Indexes into `flattened_field_paths` produced by `flatten_field_rows`.
    pub focused_row: usize,
    pub error: Option<String>,
    pub anchor: SelectionAnchor,
    /// `Vec<(track_idx, field_idx)>` view over the diff for toggle / focus.
    /// Re-computed each time `diff` is set.
    pub flattened_field_paths: Vec<(usize, usize)>,
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
            flattened_field_paths: Vec::new(),
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

    /// Recompute the flattened field index from the current diff.
    pub fn rebuild_flattened_paths(&mut self) {
        self.flattened_field_paths.clear();
        let Some(diff) = &self.diff else {
            return;
        };
        for (ti, t) in diff.tracks.iter().enumerate() {
            for (fi, _) in t.fields.iter().enumerate() {
                self.flattened_field_paths.push((ti, fi));
            }
        }
        self.focused_row = self
            .focused_row
            .min(self.flattened_field_paths.len().saturating_sub(1));
    }

    /// Toggle the `enabled` flag on the currently focused field.
    pub fn toggle_focused_field(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let Some(&(ti, fi)) = self.flattened_field_paths.get(self.focused_row) else {
            return;
        };
        if let Some(track) = diff.tracks.get_mut(ti) {
            if let Some(field) = track.fields.get_mut(fi) {
                field.enabled = !field.enabled;
            }
        }
    }

    /// Set `enabled` to `on` for every field on every track.
    pub fn set_all_enabled(&mut self, on: bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        for t in &mut diff.tracks {
            for f in &mut t.fields {
                f.enabled = on;
            }
        }
    }

    pub fn move_focus(&mut self, delta: isize) {
        if self.flattened_field_paths.is_empty() {
            self.focused_row = 0;
            return;
        }
        let max = self.flattened_field_paths.len().saturating_sub(1);
        let next = self.focused_row as isize + delta;
        self.focused_row = next.clamp(0, max as isize) as usize;
    }

    /// Jump focus to the first / last field. Used by Home / End in the
    /// diff overlay so long album diffs are navigable without holding j/k.
    pub fn focus_first(&mut self) {
        self.focused_row = 0;
    }

    pub fn focus_last(&mut self) {
        self.focused_row = self.flattened_field_paths.len().saturating_sub(1);
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
        overlay.flattened_field_paths = (0..n).map(|i| (0, i)).collect();
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
        let mut overlay = overlay_with_field_count(50);
        overlay.focus_last();
        assert_eq!(overlay.focused_row, 49);
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
    fn move_focus_page_sized_jumps_clamp() {
        let mut overlay = overlay_with_field_count(15);
        overlay.focused_row = 5;
        overlay.move_focus(10); // PageDown
        assert_eq!(overlay.focused_row, 14);
        overlay.move_focus(-10); // PageUp
        assert_eq!(overlay.focused_row, 4);
        overlay.move_focus(-100); // PageUp past start clamps
        assert_eq!(overlay.focused_row, 0);
    }
}
