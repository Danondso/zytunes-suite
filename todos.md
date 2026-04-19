# TODO: Performance & architectural cleanup

Follow-ups from the `/review`-style audit. Ordered roughly by impact.

## Structural

### ~~Device index full-rebuild on every delta~~ (done)
~~`src/tui/app.rs:549-629`. `device_index_dirty` flag triggers a clear-and-resort
of `artists`, `albums`, `album_tracks`, `track_set` on any track addition —
O(N²) during sync of many tracks. Incremental inserts into the BTreeMaps would
keep it O(log N) per delta. Deduplicate in place; avoid the full resort of the
artist list on each flush.~~

Replaced with `DeviceState::add_indexed_track` / `remove_indexed_track`
performing binary-search inserts and artist-scoped lookup-set rebuilds.
`flush_device_index` now only triggers UI re-derivations
(`rebuild_artist_device_status`, sidebar/retag). Initial load still uses
`build_device_index` which drains `tracks` and replays via the incremental path.

### ~~Sidebar stitches `"Artist — Album"` then splits it back~~ (done)
Replaced `Vec<String>` with `Vec<SidebarEntry>` (`Artist(String)` or
`Album { artist, album }`). `refresh_sidebar` builds the structured form
directly; `select_sidebar_item`, `collect_sidebar_removal_paths`,
`resolve_device_artist_album`, and the `add_sidebar_item_to_queue` queue
label all match on the variant. `display()` / `fmt::Display` produces the
legacy `"Artist \u{2014} Album"` string at render time only; ui.rs reads
device-status icons directly from the variant without splitting. Three
regression tests guard the behavior (`sidebar_entry_display_matches_legacy_stitched_form`,
`sidebar_entry_matches_query_album_matches_either_side`,
`sidebar_entry_nav_key_is_artist_for_both_variants`).

### ~~`is_on_device` / `normalize_for_match` run per render~~ (done)
`TrackInfo` now carries pre-normalized `artist_key` / `name_key` fields
(populated via `TrackInfo::new`); `retag_on_device` dispatches through
`DeviceState::contains_track(&artist_key, &name_key)` with zero allocations
on the hot flush path. `is_on_device(raw_artist, raw_name, device)` remains
as a wrapper for callers that still hold raw strings (`rebuild_artist_device_status`,
`execute_sync`), delegating to `contains_track` after one normalize per
call site. Regression tests cover the precompute
(`track_info_new_precomputes_match_keys`) and the flush path
(`retag_on_device_uses_precomputed_keys`).

### `MtpResultExt` discards `MtpError` variants at the session boundary
`src/mtp/native.rs`. The `DeviceSession` trait returns `Result<T, String>`, so
callers lose the structured error info that `zune-mtp` goes to the trouble of
providing. Change the trait to return `Result<T, MtpError>` and do the string
formatting at the CLI/TUI edge where user-facing messages are built.

## Hygiene / smaller wins

### `get_now_playing` clones the whole track list
`src/tui/app.rs:427-432`. Clones `self.track_list` into `NowPlaying.playlist`
every time playback starts. Wrap the track list in `Arc<[TrackInfo]>` (or an
equivalent shared slice) and share instead of cloning.

### `MusicLibrary::artist_tracks` / `album_tracks` always return `Vec<&Track>`
`src/library.rs:109-189`. Each sidebar/album selection triggers a `.collect()`.
Return `impl Iterator<Item = &Track>` and let the caller decide whether to
materialize.

### Dead `#[allow(dead_code)]` fields
- `src/library.rs:16-26`: `Track.album_artist`, `Track.genre`, `Track.year`,
  etc. Tagged "parsed for future sync" but never read.
- `src/tui/app.rs:40`: `DeviceTrackInfo.size`.

Delete or actually use. Each one is parsing + memory cost per track.

### Search sidebar filter still allocates per item
`src/tui/app.rs:907-908`. `item.to_lowercase().contains(&q)` allocates one
`String` per sidebar row per keystroke. Consider:
- Pre-compute lowercase form of each sidebar item once in `refresh_sidebar`
  and store alongside the display string.
- Or a custom `contains_ignore_case` that walks without allocating (awkward
  for non-ASCII, so the pre-compute is likely cleaner).

## Linux transport parity

### ~~`LibusbTransport` has no stall recovery on write/read failures~~ (done)
`LibusbTransport::write` and `LibusbTransport::read` now classify recoverable
`rusb::Error` cases (`Pipe`, `Io`, `Interrupted`, `Overflow`) via
`is_recoverable_libusb_error`, call `DeviceHandle::clear_halt(endpoint)` on
the affected bulk endpoint, and retry once — matching the IOKit backend's
`ClearPipeStall` pattern. `read_with_timeout` clears stalls on BOTH bulk
endpoints on a `Timeout` so the OUT pipe can't desync the way the IOKit
timeout path already guards against. Failure messages use `retry after
clear_halt` / `read_bulk timed out` wording, and `is_device_gone` in
`tui/background.rs` now matches those Linux phrasings alongside the
existing IOKit strings so the cascade-to-dead detection fires on Linux.
Two classifier tests guard recoverable vs terminal cases
(`is_recoverable_libusb_error_{recognises_stall_and_transients,rejects_terminal_conditions}`),
and `is_device_gone` has an extended case covering the Linux-form error
strings.

## UX follow-ups

### Dedupe-on-sync prompt
After a sync completes (or on demand from the Device panel), detect duplicate
tracks already on the device — same `(artist, album, track_name)` appearing
under more than one `object_id`. Open a modal with a scrollable list of
duplicate groups (one entry per group, expandable to show all copies with
their object IDs and sizes) and let the user select which copies to remove.
Reuse the existing bulk-remove path (`BgCommand::RemoveFromDevice`) so storage
refresh and index updates fall out for free. Decide up front whether to keep
the oldest vs. newest object ID by default.

### ~~TUI contrast audit~~ (done)
- ~~Zune Original theme: the brown `main_bg` makes the existing border color
  nearly unreadable.~~
- ~~Active-panel highlighting in general is hard to see — the current
  `selection_bg` border tint doesn't stand out enough from the inactive
  border color on several themes.~~
- ~~Sweep every theme for contrast issues once the scheme changes.~~

Zune Original: `border` changed from `(42,42,42)` (near-invisible on the
chocolate `main_bg`) to a warm tan `(188,134,92)`, and `dim_text` moved
from a cool gray `(110,110,110)` to a warmer `(180,150,120)` so dim rows
stay legible on the brown. Active-panel focus now routes through
`Theme::active_border()` — `selection_bg` foreground plus `BOLD` — applied
to every panel (Library/Albums/TrackList/Device/SyncQueue/album-detail)
so the focused border pops even on themes where `selection_bg` sits close
to the inactive `border` in luminance. Two regression tests guard the
change: `active_border_is_distinguishable_from_inactive` (every theme
must carry BOLD + `selection_bg`) and `zune_original_border_contrasts_main_bg`
(luminance delta > 60). Remaining per-theme pairs (sidebar text on
sidebar_bg, alt_row on main_bg) reviewed and left as-is — alt rows are
intentionally subtle, and sidebar text is already high-contrast on every
built-in.
