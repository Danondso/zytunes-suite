# Codebase Audit — Reviewed Findings & Remediation Plan

Date: 2026-07-02. Scope: full workspace (`zytunes`, `zune-mtp`, `ipod-db`).

An automated multi-agent audit was run across the workspace, then every
load-bearing claim was verified against the actual source before making this
plan. Several audit findings were exaggerated or contradicted documented
design decisions; those are listed at the bottom with the evidence that
rejected them. What remains below is confirmed and actionable.

---

## Confirmed bug (fix first)

### B1. `cleanup_empty_folders` deletes folders without invalidating the library cache

`src/mtp/native.rs` — `cleanup_empty_folders` (~line 1065) deletes empty
album and artist folders from the device via `delete_object`, but never
removes the corresponding entries from `DeviceLibrary` (the artist/album →
MTP-handle cache) and never persists the invalidation. `DeviceLibrary` is
cached to disk (`~/.zytunes-library-cache-{serial}`), so the stale handles
survive reconnects.

This is the exact failure class documented in `findings.md`: stale cached
artist/album handles fed into `send_object_prop_list` halt the OUT pipe on
firmware v1.4 and cascade every queued track. The `rm()` path was fixed for
that incident (it calls `invalidate_library_for_path`); the TUI removal path
was not — it goes through `rm_by_id` (`tui/background.rs:778`, `:1220`)
followed by `cleanup_empty_folders` (`tui/background.rs:827`), and neither
touches the library cache.

**Repro scenario:** in the TUI, remove the last track of an artist (folders
get cleaned up), then queue a new track by that same artist. `import_track`
reuses the cached (now dangling) artist/album folder handles → OUT pipe halt
on v1.4, every queued track fails until replug.

**Fix:** `cleanup_empty_folders` already has `artist_info.filename` /
`album_info.filename` in hand at each successful `delete_object` — remove
those entries from `self.library` and persist the library cache once at the
end (only if anything was removed). Add a unit test at the `DeviceLibrary`
level for entry removal + persistence round-trip (hardware-free).

Also audit `recent_imports` (session-scoped handle map) for the same
staleness at these two deletion sites.

Commit: `fix(mtp): invalidate library cache when empty folders are cleaned up`

### B2. Panic on non-UTF-8 temp path in `transcode_to_wmv`

`src/lib.rs:278` — `output.to_str().unwrap()` panics if the temp dir path is
non-UTF-8. The sibling ALAC branch (line ~390) already handles this with
`ok_or`. Use the same pattern.

Commit: `fix: replace panic on non-UTF-8 transcode output path with error`

### B3. `NativeSession::clear_cache` is dead code — and a footgun if ever called

`src/mtp/native.rs:1808` — no callers anywhere in the workspace, and if it
were called it would clear the track cache while leaving `DeviceLibrary`
populated (stale-handle divergence). Delete it. If a full-clear entry point
is ever needed, it must clear both caches together.

Commit: rolled into B1's commit or `refactor(mtp): remove unused clear_cache`

---

## Refactors (behavior-preserving, one commit each)

Ordered by value ÷ risk. Each phase must end with `cargo fmt`,
`cargo clippy -- -D warnings`, and `cargo test` green.

### R1. Extract the transcode pipeline out of `lib.rs`

`src/lib.rs` lines ~201–639 contain a complete, self-contained audio/video
transcode pipeline (`transcode_for_device`, `transcode_to_mp3`,
`transcode_to_wmv`, `transcode_flac_to_alac`, plus symphonia/LAME helpers)
— ~450 lines plus ~430 lines of codec tests. Move to `src/transcode.rs`
with `pub use` re-exports from `lib.rs` so no caller changes. Move the
transcode tests with it.

This leaves `lib.rs` with device-sync orchestration, file collection, and
config resolution — still worth watching, but no longer a dumping ground.

Commit: `refactor: extract transcode pipeline from lib.rs into transcode module`

### R2. Collapse the triplicated file-collection functions

`src/lib.rs:843–940` — `collect_music_files_with_logger`,
`collect_photo_files_with_logger`, `collect_video_files_with_logger` are
byte-for-byte identical except for the extension array and the noun in the
log message ("non-music" / "non-photo" / "non-video"). Extract one
`collect_media_files(paths, extensions, kind, log)` and keep the six public
wrappers as one-liners (API unchanged). Existing tests in `lib.rs` cover all
three kinds and stay as-is.

Commit: `refactor: deduplicate music/photo/video file collection`

### R3. Decompose `handle_key` and `handle_bg_event` in `tui/app.rs`

The production half of `app.rs` is ~6.3k lines (the rest is tests). The two
worst functions:

- `handle_key` (5248–5771, ~520 lines): ~10 overlay gates each with inline
  key matching, then a ~70-arm main match. Extract one method per overlay
  (`handle_import_overlay_key`, `handle_playlist_picker_key`,
  `handle_tag_manager_key`, …) following the existing `app/` submodule
  pattern (`app/import.rs`, `app/playlists.rs` already exist — put overlay
  handlers next to their state where one exists). Target: `handle_key`
  becomes a dispatcher under ~150 lines.
- `handle_bg_event` (4490–4791, ~300 lines, 37-arm match): extract the heavy
  arms (`LibraryLoaded`, `DeviceTracksLoaded`, `SyncComplete`, CD/rip
  events) into `on_*` methods. The existing tests call `handle_bg_event`
  directly and continue to pass unchanged.

Purely mechanical; no state-shape changes. This is deliberately scoped
*instead of* the audit's "split App into AppState/AppController" — a full
rearchitecture of a working, heavily-tested TUI is not worth the churn; the
readability cost is concentrated in these two functions.

Commit: `refactor(tui): extract overlay key handlers and bg-event handlers`

### R4. Factor the two long MTP session functions

`src/mtp/native.rs`:

- `import_playlist` (1364–~1681, ~317 lines): split into
  `resolve_playlist_handles` (partially exists), `create_playlist_object`,
  and `replace_playlist_references` step helpers.
- `ensure_library` (594–~772, ~178 lines): split the artist scan and album
  scan into `load_artist_folders` / `load_album_folders`.

Commit: `refactor(mtp): factor import_playlist and ensure_library into step helpers`

### R5. Typed error at the `DeviceSession` boundary

`MtpResultExt` (`src/mtp/native.rs:25`) erases `MtpError` to `String` at the
trait boundary. Two concrete costs today:

1. `is_device_gone` in `tui/background.rs` detects fatal USB failure by
   substring-matching error strings (`0xe00002c0`, `ReadPipe timed out`, …).
2. The ZMDB → handle-walk fallback in `collect_all_tracks` can't distinguish
   "vendor op unsupported" (expected on old firmware → fall back) from a
   fatal USB error (→ should abort, not walk hundreds of handles on a dead
   pipe).

**Fix:** introduce a small enum at the boundary —
`DeviceError { Unsupported, DeviceGone, Other(String) }` with `Display` —
and change `DeviceSession` methods from `Result<T, String>` to
`Result<T, DeviceError>`. `MtpResultExt` maps `MtpError` variants into it
(the transport already knows which IOKit codes are fatal — classify there,
not by string). `is_device_gone` becomes a match on `DeviceError::DeviceGone`
(keep the string heuristic as a fallback arm during transition).

This touches every `DeviceSession` impl and call site — do it as its own PR
after R1–R4 land, not mixed with anything else.

Commit: `refactor(mtp)!: typed DeviceError at the DeviceSession boundary`
(internal API only; not a release-triggering `feat!`)

---

## Small cleanups (batch into one or two commits)

- **S1.** `ipod-db/src/itunesdb_write.rs`: extract the FourCC file-type codes
  (`0x4d503320` "MP3 ", `0x4d344120` "M4A ", …) and the per-format mystery
  constants (`unk126`/`unk144`/`unk204` values) into a named `const` block
  with the existing inline comments promoted to doc comments. 113 hex
  literals in the file; most are documented offsets that can stay, the
  format-identity ones should be named.
- **S2.** Review the 11 `#[allow(dead_code)]` fields in `tui/background.rs`
  and `tui/app.rs`. Keep the ones tied to a documented upcoming phase
  (e.g. `DeviceTrackInfo::play_count` / Phase 4b) with a comment naming the
  phase; delete the rest (`RipEvent` `track_position`/`dest_path` fields if
  nothing planned consumes them).
- **S3.** `src/tui/ui/mod.rs`: name the responsive-layout tier thresholds
  (`const COMPACT_TIER_MAX_W: u16 = 100;` etc.) in `LayoutMetrics::new`.
- **S4.** Document the deliberate path-format difference between backends on
  the `DeviceSession::ls`/`collect_all_tracks` doc comments: `NativeSession`
  yields `/Music/Artist/Album/file.ext`, `IpodSession` yields
  `Artist/Album/Title.ext` (by design — see CLAUDE.md), so callers must not
  assume a shared shape.
- **S5.** Add a short `ipod-db/examples/README.md` explaining the 17 example
  files are manual bring-up/forensics tools (paired with `DEBUG.md`), not
  test coverage.
- **S6. (optional)** Shared `atomic_write_json(path, bytes)` helper used by
  `local_plays.rs` and `playlist_store.rs` for the `.tmp` + rename mechanics.
  Their schema-mismatch policies (discard vs. preserve-as-`.bak`) are
  deliberately different and stay separate.

Commit: `chore: audit follow-ups — named constants, dead fields, doc comments`

---

## Audit findings reviewed and REJECTED (do not action)

| Claim | Verdict | Evidence |
|---|---|---|
| "App has 160+ fields, 125 unwraps, 350 lines of dead test helpers" | Exaggerated/wrong | `App` has 78 fields; **0** `.unwrap()` in production app.rs (all in `mod tests`, line 6267+); the "dead helpers" at line 7998+ are `#[test]` functions. |
| "app.rs is a 12,351-line monolith" | Half true | ~6.3k production lines, ~6k test lines. Still large — addressed by R3, not a rewrite. |
| "`render_album_art` mutates state during rendering — design error" | Rejected | It's a memoized pre-render pass called from `tui/main.rs:129` *before* `draw()`; the render layer itself takes `&App`. Covered by tests. |
| "Replace `art_disabled` session flag with per-track retry" | Rejected | Contradicts the documented incident (CLAUDE.md, findings.md): one bad JPEG wedges the MTP session and each retry costs a 45 s timeout cascade. The session-level kill switch is the deliberate fix. |
| "DeviceSession default `Err("not supported")` impls silently accept unsupported ops" | Rejected | Returning `Err` *is* failing fast; capability gating already exists via `DeviceCapabilities`. |
| "Split the 54-field `Track` struct into Track + ExtendedMetadata" | Deferred | The wide-bag design is documented and deliberate: `skip_serializing_if` keeps the cache compact and forward-compatible; a split forces a `CACHE_SCHEMA_VERSION` bump and touches every consumer for readability-only payoff. Revisit only if the struct keeps growing. |
| "dirlib clones Track millions of times / snapshot clone is wasteful" | Rejected | The snapshot clone is gated by `SAVE_DIRTY_THRESHOLD` (commented rationale in `dirlib.rs:262`); per-file progress samples clone three `String`s, not whole `Track`s. |
| "`let _ = cmd_tx.send(...)` swallows errors (22×)" | Rejected | Standard UI→worker channel pattern; if the worker thread is gone the TUI is exiting anyway. |
| "MTPZ handshake needs a state enum" | Rejected (doc only) | Single call site, linear `authenticate()`, fully `Result`-typed. A numbered step list in the doc comment is enough — fold into S4-style doc pass if touched. |
| "Split App into AppState + AppController" | Rejected in favor of R3 | High-churn rearchitecture of a working, heavily-tested module; the pain is concentrated in two functions. |
| "`itunesdb.rs` / `itunesdb_write.rs` split is dirty" | Rejected | Verified clean: two public entry points, `pub(crate)` raw-blob fields justified by lossless round-trip. |

---

## Execution order & release hygiene

1. **PR 1 (bugs):** B1 + B2 + B3 — `fix:` commits, patch release.
2. **PR 2 (lib.rs):** R1 + R2 — `refactor:` commits, no release.
3. **PR 3 (TUI):** R3 — `refactor(tui):`, no release.
4. **PR 4 (MTP):** R4 + S1–S5 — `refactor(mtp):` / `chore:`, no release.
5. **PR 5 (typed errors):** R5 alone — largest blast radius, easiest to
   review in isolation.

Every PR: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` across the
workspace. B1 gets a regression test at the `DeviceLibrary` level; refactors
rely on the existing suites (app.rs alone has ~6k lines of tests that pin
`handle_key`/`handle_bg_event` behavior).
