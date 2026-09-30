# File-into-library review remediation plan (PR #35)

Source: code review of `cursor/file-into-library` against `main` (3 high, 5 medium,
2 low). Each finding has an inline comment on PR #35. None were reproduced by a
test run yet, so step one of every item is a failing test that proves the bug.

## TDD rules for every item

1. **Red** — write the named test(s) first. Run `cargo test -p zytunes <name>` and
   confirm the failure is the bug described, not a typo in the test. If the test
   passes on current code, the finding is wrong: reply on the PR thread and drop it.
2. **Green** — smallest change that passes.
3. **Refactor** — tidy, then `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`.
4. One `fix:` commit per item (test + fix together), then reply on the PR thread
   with the commit hash.

Filesystem tests reuse the existing helpers in `tag_ops.rs` tests (`fresh_dir`,
`write_sine_wav`, `make_release`, `volume_folds_ascii_case`). Tests that need a
case-sensitive volume early-return when `volume_folds_ascii_case(dir)` is true;
tests that need a folding volume early-return when it is false.

## Order

Data-loss first, then the inbox loop, then naming/lookup, then polish. Items 3
and 4 touch the same phase-0 block and land before 1 and 2, which change
`perform_rename`.

| # | Sev | Finding | File |
|---|-----|---------|------|
| 3 | high | same-inode guard only covers ASCII case | `tag_ops.rs:833`, `:207` |
| 4 | med  | dest deleted before the move succeeds | `tag_ops.rs:838` |
| 1 | high | retitle strands artist folder under temp name | `tag_ops.rs:1087` |
| 2 | high | directory retitle runs once per track | `tag_ops.rs:1123` |
| 5 | med  | overlay reopens every 5 s after Done | `tui/app.rs:2716` |
| 6 | med  | dismissed files re-fingerprinted each poll | `tui/background.rs:2447` |
| 7 | med  | `A & B feat. C` collapses to `A` | `musicbrainz.rs:644` |
| 8 | med  | `F` from track list uses track artist | `tui/app.rs:2502` |
| 9 | low  | overlay opens over other modals | `tui/app.rs:6017` |
| 10 | low | `create_dir_all` on every poll | `tui/background.rs:2439` |

## Items

### 3. Same-inode guard independent of ASCII case

- **Red**
  - `apply_release_diff_replace_never_deletes_source_when_dest_is_same_inode`:
    make `dest` a hard link to `src` with a non-ASCII-different name
    (`Beyonce\u{301}/…` vs `Beyoncé/…`), so `same_inode` is true and
    `is_case_only_rename` is false on any filesystem. Apply with
    `replace_existing = true`. Assert the audio is still reachable and the
    result is `Ok`.
  - `replacing_existing_dests_ignores_same_inode_dest`: same hard-link setup,
    plus a pure case retitle on a folding volume; assert the list is empty.
- **Green** — in phase 0 check `same_inode(src, dest)` alone, before any
  removal, and route it to the retitle path. Apply the same check in
  `replacing_existing_dests`.
- **Refactor** — one `dest_is_source(src, dest)` helper used by both sites.

### 4. Replace only after the move succeeds

- **Red** — `apply_release_diff_replace_keeps_dest_when_rename_fails`: existing
  dest, `replace_existing = true`, rename forced to fail (source removed after
  the diff is built, or a read-only dest parent restored in cleanup). Assert the
  original dest file still exists with its original bytes.
- **Green** — drop the phase-0 `remove_file`. Carry a "replacing" flag into the
  rename phase and let `fs::rename` overwrite the dest atomically; for the
  cross-device copy fallback, copy to `dest.tmp`, then rename over.
- **Check** — `apply_release_diff_replace_existing_overwrites_and_prunes` still passes.

### 1. Retitle must not strand a directory

- **Red** (case-sensitive volume)
  - `case_retitle_merges_into_existing_canonical_folder`: `alice in chains/Dirt/01.wav`
    plus a non-empty `Alice In Chains/Facelift/`. Apply the diff. Assert the
    track lands in `Alice In Chains/Dirt/`, `Facelift` is untouched, and no
    `.zytunes-case-*` entry exists anywhere under the root.
  - `retitle_case_along_rolls_back_on_second_rename_failure`: unit test on the
    helper; after an `Err`, the original directory name is back.
- **Green** — per component, if `parent/want` exists and is a different inode,
  stop retitling and fall through to the normal per-file move (with
  `create_dir_all` for the missing album folder). If the second rename fails
  for any other reason, rename the temp back before returning the error.

### 2. Directory retitle is a directory-level operation

- **Red**
  - `case_retitle_multi_track_album_all_succeed`: two tracks,
    `Alice in Chains/Dirt` to `Alice In Chains/Dirt`. Assert both results are
    `Ok` and `rename_map` has both entries. Runs on both volume kinds.
  - `case_retitle_reports_sibling_album_moves`: add `Alice in Chains/Facelift/01.wav`
    that is not in the diff. Assert the returned map (or a new
    `dir_renames` output) covers the Facelift path.
  - `dirlib.rs`: `reread_paths_follows_directory_rename` — a library holding the
    Facelift track keeps it, at the new path, after the reread.
  - `tui/app.rs`: `queued_file_clusters_follow_directory_retitle` — a queued
    cluster under the old artist spelling has its `location`s remapped.
- **Green**
  - In `perform_rename`, a missing `src` whose `dest` exists (moved by an
    earlier retitle in the same batch) is success.
  - `apply_release_diff_with` returns the directory renames it performed
    (`Vec<(PathBuf, PathBuf)>`, old prefix to new prefix).
  - The apply-done handler rewrites matching prefixes in the library
    (via `reread_paths`) and in `inbox.queue` / pending `F` clusters.
- **Refactor** — a small `ApplyOutcome { results, rename_map, dir_renames }`
  struct instead of a growing tuple; update the ten existing call sites.

### 5. Done-close must dismiss leftovers

- **Red** (`tui/app.rs` tests, using `tui/testing.rs`)
  - `filing_done_dismisses_sources_still_in_inbox`: overlay in `Done`, one
    source file still present in the inbox; after `close_tag_manager`, feed the
    same track to `on_inbox_scanned` and assert `tag_manager` stays `None`.
  - `filing_done_does_not_add_inbox_leftovers_to_library`: after apply, the
    library has no track whose location is under the inbox dir.
- **Green** — on any filing close, dismiss every source path that still exists
  and is not a key in `last_rename_map`. Filter inbox-rooted paths out of the
  post-apply `reread_paths` call.
- **Refactor** — this makes the Done and non-Done branches share the dismiss loop.

### 6. Skip dismissed files before the scan work

- **Red**
  - `library_layout.rs`: `scan_inbox_skips_excluded_paths` — new `exclude:
    &HashSet<PathBuf>` parameter; excluded files are not returned (and so never
    reach tag read or fingerprinting).
  - `tui/app.rs`: `scan_inbox_command_carries_dismissed_set` — after a dismiss,
    the next queued `BgCommand::ScanInbox` contains the path.
- **Green** — add `dismissed: HashSet<PathBuf>` (or `Arc`) to `ScanInbox`, pass
  it to `scan_inbox`, filter on filename before opening the file. Keep the
  app-side filter as a backstop for scans already in flight.

### 7. Keep collaborators before the featuring join

- **Red** (`musicbrainz.rs`)
  - `canonical_album_artist_keeps_collaborators_before_feat`:
    `A` + `" & "` + `B` + `" feat. "` + `C` gives `A & B`.
  - `canonical_album_artist_feat_first_join_still_primary_only`: `2Pac feat. X`
    gives `2Pac` (guards the existing behaviour).
- **Green** — render credits up to and excluding the first featuring
  joinphrase; trim trailing whitespace. Use `primary_credit_name` only when
  that prefix is a single credit.
- **Refactor** — `is_featuring_join(&str)` extracted from `is_featuring_credit`.

### 8. `F` from the track list uses the grouping artist

- **Red** — `file_and_enrich_from_track_list_uses_album_artist`: library track
  with `artist = "2Pac feat. Dr. Dre"`, `album_artist = "2Pac"`, focus
  `Panel::TrackList`; `start_file_and_enrich` opens the overlay with that
  track instead of toasting "No tracks to file".
- **Green** — resolve the selected row to its library `Track` and pass
  `grouping_artist()` to `album_tracks_by_artist`.
- **Check** — confirm what type `track_list` rows are; if they lack
  `album_artist`, look the library track up by id first.

### 9. Do not open the filing overlay over another modal

- **Red** — `inbox_scan_does_not_open_overlay_while_search_active`, plus one
  case each for the theme picker and the stem-consent modal. Assert
  `tag_manager` is `None` after `on_inbox_scanned`, and that a later scan with
  the modal closed does open it.
- **Green** — add `App::modal_or_input_active()` covering every modal that
  `handle_key` tries ahead of the global map, and gate `on_inbox_scanned` on it.
- **Refactor** — reuse the predicate anywhere `keys.rs` repeats that list.

### 10. No mkdir on every poll

- **Red**
  - `scan_inbox_missing_dir_returns_empty_without_creating_it` (`library_layout.rs`).
  - `inbox_create_failure_logged_once` (`tui/app.rs`): two ticks against an
    uncreatable inbox path produce one log line.
- **Green** — `ScanInbox` carries `create_dir`, set only on the first scan of
  a session. A failure logs once; later polls just read (and pick the folder up
  if the user creates it by hand).
- **Decided** — the inbox is created by default. A place to drop music for
  sorting is part of using zytunes, so there is no opt-in or opt-out key. The
  review's "without any opt-in" point is declined; only the every-poll mkdir
  and the repeated log line are fixed.

## Docs

- README "File into library" bullet: update if item 7 changes the documented
  `feat.` behaviour wording, and note that the inbox is created at startup (item 10).
- CLAUDE.md: no module-level changes expected beyond `ApplyOutcome` (item 2).

## Todo

Status: all items landed, one `fix:` commit each (see `git log`).

- [x] 3a. Red: same-inode hard-link tests (apply + `replacing_existing_dests`)
- [x] 3b. Green/refactor: `dest_is_source` guard at both sites; commit; reply on PR
- [x] 4a. Red: dest survives a failed rename
- [x] 4b. Green: replace via rename-over, remove phase-0 delete; commit; reply
- [x] 1a. Red: merge-into-existing-folder test + rollback unit test
- [x] 1b. Green: different-inode target falls through to move; rollback; commit; reply
- [x] 2a. Red: multi-track retitle, sibling album, dirlib reread, queued cluster tests
- [x] 2b. Green: already-moved source is Ok; return `dir_renames`
- [x] 2c. Green: library + queue prefix remap in the apply-done handler
- [x] 2d. Refactor: `ApplyOutcome` struct; commit; reply
- [x] 5a. Red: Done-close leftover tests
- [x] 5b. Green: dismiss leftovers, keep inbox paths out of reread; commit; reply
- [x] 6a. Red: `scan_inbox` exclude test + command-carries-dismissed test
- [x] 6b. Green: `ScanInbox { dismissed }`; commit; reply
- [x] 7a. Red: `A & B feat. C` and `2Pac feat. X` tests
- [x] 7b. Green: prefix-before-feat rendering; commit; reply
- [x] 8a. Red: track-list `F` on a feat track
- [x] 8b. Green: grouping-artist lookup; commit; reply
- [x] 9a. Red: overlay-vs-modal tests
- [x] 9b. Green: `modal_or_input_active` gate; commit; reply
- [x] 10a. Red: missing-dir scan + log-once tests
- [x] 10b. Green: create once at startup, log once; commit; reply
- [x] README / CLAUDE.md sync
- [x] Final: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test --workspace`; push
