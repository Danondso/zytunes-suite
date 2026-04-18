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

### Sidebar stitches `"Artist — Album"` then splits it back
`src/tui/app.rs:854-866` (and device branch 890-898). Builds
`format!("{} \u{2014} {}", artist, album)` for display, then downstream code
parses it with `split_once(" \u{2014} ")` to recover the parts. Store
`(artist_idx, album_idx)` or a small enum; format only at render time.

### `is_on_device` / `normalize_for_match` run per render
`src/tui/app.rs:1805-1820, 1845`. Normalization happens on every track during
filter/render passes. Pre-normalize when building `device.track_set` and
`device.artist_track_names` so lookup is a direct set membership test.

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

### `LibusbTransport` has no stall recovery on write/read failures
`zune-mtp/src/transport/libusb.rs`. The IOKit backend calls `ClearPipeStall`
(with a one-shot retry) on recoverable USB errors from `WritePipe` / `ReadPipe`,
and clears stalls on *both* bulk endpoints after a read timeout so the OUT
pipe doesn't desync. The libusb backend does neither — a single transient
stall (common during sync cascades, cable jostles, or Zune firmware hiccups)
surfaces as a raw `rusb::Error` and the user has to replug. Add equivalent
recovery via `DeviceHandle::clear_halt(endpoint)` on both bulk endpoints, with
the same one-shot retry pattern, so Linux sync tolerates the same class of
wedges macOS already does.

## UX follow-ups

### TUI contrast audit
- Zune Original theme: the brown `main_bg` makes the existing border color
  nearly unreadable. Pick a lighter tint or switch to an accent-coloured
  border for that theme.
- Active-panel highlighting in general is hard to see — the current
  `selection_bg` border tint doesn't stand out enough from the inactive
  border color on several themes. Consider a thicker border, brighter
  accent, or inverting the title bar for the active panel.
- Sweep every theme for contrast issues once the scheme changes: sidebar
  text on sidebar_bg, alt_row on main_bg, dim_text on main_bg, and the
  active-vs-inactive border pair.
