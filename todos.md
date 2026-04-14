# TODO: Performance & architectural cleanup

Follow-ups from the `/review`-style audit. Ordered roughly by impact.

## Structural

### Device index full-rebuild on every delta
`src/tui/app.rs:549-629`. `device_index_dirty` flag triggers a clear-and-resort
of `artists`, `albums`, `album_tracks`, `track_set` on any track addition —
O(N²) during sync of many tracks. Incremental inserts into the BTreeMaps would
keep it O(log N) per delta. Deduplicate in place; avoid the full resort of the
artist list on each flush.

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

## UX follow-ups

### Adding to a running sync queue clobbers it
Currently, pressing `a` to add more tracks while a sync is already executing
clears the visible queue panel even though the sync keeps running in the
background. Expected behavior: append the new items to the existing queue so
they get picked up after the current batch (or at least stay visible and
queued). Verify against both `a` (single track) and `A` (add all visible).

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

### Newport Lights spinner skips a frame
The throbber in the Newport Lights theme doesn't advance smoothly — it
looks like the spinner set is missing a frame or the frames don't cycle
cleanly. Check `src/tui/theme.rs` for the Newport spinner set and compare
frame count / symbols against a theme that spins smoothly.
