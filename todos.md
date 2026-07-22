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

### ~~`MtpResultExt` discards `MtpError` variants at the session boundary~~ (wontfix)
`src/mtp/native.rs`. The `DeviceSession` trait returns `Result<T, String>`, so
callers lose the structured error info that `zune-mtp` goes to the trouble of
providing. Original suggestion was to return `Result<T, MtpError>`.

**Why wontfix:** `DeviceSession` now has two implementations —
`NativeSession` (Zune, emits `MtpError` plus incidental `io::Error`) and
`IpodSession` (iPod, emits `ipod_db::Error` plus `io::Error`). Making
the trait's error type `MtpError` would force iPod errors into
ill-fitting variants. The clean fix is a unifying `DeviceError` enum,
but every call site changes (trait, both impls, `background.rs`,
`lib.rs::sync_to_device`, `transcode_and_import`, CLI `cmd_push`) for
modest practical gain — every consumer ultimately formats to `String`
for display, and `is_device_gone` already matches on stable substring
markers (`0xe00002c0`, `0xe00002ed`, `ClearPipeStall`, `clear_halt`,
`ReadPipe timed out`, `read_bulk timed out`) rather than variant
identity. Revisit if we grow retry logic or a diagnostic UI that
genuinely needs to branch on structured variants.

## Hygiene / smaller wins

### ~~`get_now_playing` clones the whole track list~~ (done)
`NowPlaying.playlist` is now `Arc<[TrackInfo]>`. `next_track`/`prev_track`
`Arc::clone` instead of cloning the slice, and `play_from_playlist` takes
ownership of the `Arc` so skip/prev hops no longer allocate. The first
`play_selected_track` pays a single `Arc::from(&[TrackInfo])` copy; every
subsequent playlist hop is a ref-count bump.

### ~~`MusicLibrary::artist_tracks` / `album_tracks` return `Vec<&Track>`~~ (done)
The trait methods now return `Box<dyn Iterator<Item = &Track> + '_>` so
streaming callers (sidebar population in `select_sidebar_item`, the library
stats pass in `rebuild_album_device_status`, and `add_sidebar_item_to_queue`)
skip the intermediate `Vec`. `find_matching_tracks` still materializes a
`Vec` since it uses `.len()` / `.is_empty()` for user-facing messages and
its public return type is `Vec<&Track>`. The trait stays dyn-safe because
`Box<dyn Iterator>` is object-safe. `tracks_to_info` was generalized to take
`impl IntoIterator<Item = &Track>` so it accepts either the new iterator
form or any other shape without forcing a collect at the call site.

### ~~Dead `#[allow(dead_code)]` fields~~ (done)
`Track.album_artist` was always `None` in practice (lofty doesn't expose it
via `Accessor` and no writer populated it) and had no readers — deleted.
`Track.genre` is now read on the sync path via the new `mtp::TrackMeta`
struct threaded through `DeviceSession::import_track`, eliminating
`NativeSession`'s redundant lofty read when the caller already parsed the
library. The stale `#[allow(dead_code)]` markers on `year`,
`track_number`, `disc_number`, `total_time_ms` were lies — the TUI
(`album_list`, `TrackInfo`, sort keys) already read all four — so the
attributes were dropped. `DeviceTrackInfo.size` was write-only and removed.
`SyncItem` gained `track_number` / `genre` so the TUI sync path carries
the metadata through to the wire.

### ~~Search sidebar filter still allocates per item~~ (done)
Refactored the sidebar into a two-tier cache:
`sidebar_items_full` holds the unfiltered source, `sidebar_lowercase_full`
stores the pre-lowercased search key for each row (albums join
`artist\nalbum` so queries match either side without spanning), and
`sidebar_items` is the filtered view the UI reads. `rebuild_sidebar_source`
does the expensive library/device scan on library/mode changes only;
`apply_sidebar_filter` runs on every `/` keystroke and just walks the
cached lowercase strings — no per-row `to_lowercase()` allocation.
`main.rs` search keystrokes route through `apply_sidebar_filter` instead
of `refresh_sidebar`. Regression tests:
`sidebar_entry_lowercase_key_album_matches_either_side` guards the
separator semantics and `search_filter_reuses_cached_lowercase` guards
the two-tier design.

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

### ~~Dedupe-on-sync prompt~~ (done — reshaped into overwrite-on-sync + explicit dedupe hotkey)
Original plan was a post-hoc modal asking the user which copies to keep.
Reshaped into a simpler two-part design that doesn't need a modal:

1. **Overwrite-on-sync**: `SyncItem` gained an `overwrite_targets: Vec<(String, u64)>`
   field. `App::execute_sync` (and the mid-sync append path) now calls
   `find_device_copies(device, artist, name)` for every queued item and
   stamps the matching `(device_path, object_id)` pairs onto the item.
   The old "skip if already on device" filter is gone — every queued
   track is dispatched. The background worker removes each target via
   `s.rm_by_id` (falling back to `s.rm(path)`) before calling
   `import_track`, emitting `DeviceTrackRemoved` / `DeviceTrackAdded` so
   the in-memory index stays current. Fatal USB cascades during the
   remove phase trigger the same `is_device_gone` abort path as a failed
   upload. Result: re-queuing a track always refreshes it on the device,
   and any pre-existing duplicates matching the queued name get swept as
   a side effect.
2. **Explicit dedupe hotkey**: `U` in Device panel invokes
   `App::dedupe_device`, which scans `device.album_tracks` for groups
   sharing normalized `(artist, album, name)`, keeps the newest
   `object_id` per group, and dispatches the rest via
   `BgCommand::RemoveFromDevice`. No modal — a toast reports either "No
   duplicates found" or "Removing N duplicate copy(ies) from device...".
   Keep-newest was the default we discussed — MTP assigns object IDs
   monotonically so the highest ID is the most recent upload (usually
   the freshest tags/art).

Regression tests cover: `find_device_copies` returning every matching
copy (including pre-existing duplicates), `execute_sync` populating
`overwrite_targets` for duplicates, the mid-sync append path doing the
same, `collect_device_duplicates` keeping-newest-per-group with
multi-album guarding, and the dedupe dispatch/toast paths.

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

## UX bugs

### Device track list: duration column is empty (Zune)
`DeviceEntry` (`src/mtp/parse.rs`) has no duration field, so device-mode
rows always render an empty Duration column. Two-pronged fix, scoped
during investigation (2026-07-11):

1. **MTP enrichment (device truth).** Add `PROP_DURATION: u16 = 0xDC89`
   to `zune-mtp/src/proplist.rs`, an `apply_durations` projection in
   `src/mtp/native/playcount.rs` (skip `0` values — duration 0 is
   meaningless, unlike play_count 0), and a fourth
   `enrich_one_prop(tracks, PROP_DURATION, ...)` call in
   `NativeSession::enrich_with_playcounts`. The bulk
   `GetObjectPropList(0xFFFFFFFF, MP3, prop, 0, 0)` pattern is already
   proven on v1.4 firmware for UseCount/Rating — one extra round trip
   per connect. ZMDB has **no** duration in the audio record (all 28
   fixed bytes are mapped: album/artist/genre/folder refs, size,
   track#, format), so ZMDB rows only pick duration up after the cache
   merge restores their object handles — same limitation playcounts
   already have.
2. **Library fallback (instant coverage).** In `device_tracks_to_info`
   (`src/tui/app.rs`), refactor `resolve_library_id_for_device_track`
   to return the matched `&Track` and use
   `dt.duration_ms.or(lib_track.total_time_ms)` — covers every
   device row that has a library counterpart with zero USB traffic.

Wiring: `DeviceEntry.duration_ms: Option<u32>`, 9th tab-separated
column in `TrackCache` (`splitn(9, ..)`, old caches parse fine),
`DeviceTrackInfo.duration_ms: Option<u64>` threaded through
`add_indexed_track`, and iPod fills it for free from mhit `+40`
(`t.total_time_ms`, lift 0 → `None`) in
`IpodSession::collect_all_tracks`. The UI already renders
`TrackInfo.duration_ms` when present — no render changes needed.

## Future features

### Tag manager: composer / lyricist / performer fields
`build_track_fields` in `src/tag_ops.rs` currently emits all Picard-standard
release-level fields (Title/Artist/Album/Year/Genre/MUSICBRAINZ_ALBUMTYPE/
Media/Country/Status/Packaging/Script/Language/ISRC/Barcode/Catalog#/Label
plus all six MBIDs and Filename), but **track-level credit fields** like
composer, lyricist, conductor, and performers are still absent. MB models
these as recording–artist relationships, not as fields on the recording
itself. Scope:

1. Extend the `lookup_release_full` / `lookup_disc` `inc` set with
   `work-rels+recording-rels+artist-rels` (already requesting
   `+recordings+artist-credits+release-groups+isrcs+labels+genres`).
2. Add a `relations: Vec<Relation>` field to `Recording` in `musicbrainz.rs`,
   where each `Relation` carries the relation type (`composer`/`lyricist`/
   `conductor`/`performer`/`vocal`/`instrument`) and the linked artist's
   name + MBID.
3. Walk each track's `recording.relations` in `build_track_fields` and
   join multi-artist credits with `; ` (Picard's convention) before
   pushing the diff field.
4. Map to `ItemKey::Composer`, `ItemKey::Lyricist`, `ItemKey::Conductor`,
   `ItemKey::Performer` in `apply_field`. Picard also writes per-instrument
   TXXX frames (`Performer:Lead Guitar` etc.) — start with the bare
   `Performer` aggregate, defer per-instrument splits.
5. Add a `Credits` `FieldKind` variant (or reuse `Identity`) so the diff
   UI sections them together. The track-header summary in `ui/mod.rs`
   should not regress for credit-only diffs.

The MB inc-set change is harmless to other call sites (CD import / overlay
rendering both ignore unknown fields), so the work is contained to
`musicbrainz.rs` + `tag_ops.rs` + tests.

### AcoustID lookup as tag-manager fallback (next up)
The tag-manager overlay's resolution chain is currently:
`mb_release_id` → direct lookup, else MB search (Solr-only). For files
with no MBIDs at all, neither path works on a Solr-less mirror — and
search-by-text is unreliable even with Solr. Library tracks already
carry a Chromaprint `acoustic_id` and `total_time_ms` (see
`src/fingerprint.rs`), so we have everything the AcoustID web service
needs to identify a bare audio file from scratch.

Integration point is `App::resolve_known_release_mbid` in
`src/tui/app.rs` — extend it with an async-but-blocking-on-worker call
that, when no library MBID exists, dispatches AcoustID lookup and
treats the returned recording MBID as the starting point for an MB
`/recording/{mbid}?inc=releases` round-trip to pick a release.

Scope:

1. **`src/acoustid.rs` client.** GET `https://api.acoustid.org/v2/lookup`
   (NOT POST — the API takes `client`, `fingerprint`, `duration`,
   `meta` as query params or form-encoded body. GET is simpler and
   their docs are explicit it's supported). `meta=recordings+releases`
   returns enough to skip the follow-up MB call for the common case.
   Rate-limit to 3 req/sec per their ToS. Return
   `Result<Vec<AcoustIdHit>, AcoustIdError>` where each hit carries
   recording MBID + score + optional release IDs.
2. **Config field.** Add `acoustid_app_key: Option<String>` to
   `~/.config/zytunes/config.toml` (parallel to
   `musicbrainz_user_agent`). Without it, the lookup-first chain
   skips AcoustID silently — the feature stays dark until configured.
   Plumb through `App.acoustid_app_key` like `mb_user_agent`.
3. **Disk cache.** Fingerprints are stable input, so cache responses
   keyed on `(acoustic_id, duration_secs_rounded)` at
   `$ZYTUNES_CACHE_DIR/.zytunes-acoustid-cache`. Re-launches reuse
   without re-hitting the API. Match the dirlib cache's logger
   forwarding so warnings land in the TUI sync log rather than stderr.
4. **Worker integration.** New `BgCommand::AcoustIdLookup {
   token, fingerprint, duration_ms, app_key }` and matching
   `BgEvent::AcoustIdResolved { token, result: Result<Vec<AcoustIdHit>,
   String> }`. Token-fenced same as the existing MB requests.
5. **Tag-manager dispatch.** When `resolve_known_release_mbid` returns
   None AND the library track carries `acoustic_id`, dispatch
   `AcoustIdLookup` instead of `MbSearchReleases`. On a single
   high-confidence hit (score > 0.9), auto-advance to
   `MbReleaseDetails` for the first release on the matched recording;
   on ambiguous hits, present them as a `SearchResults`-like list so
   the user picks. The overlay needs a small enum extension to track
   "we resolved via AcoustID" so error recovery (Esc from Error phase
   currently goes to SearchInput) routes back somewhere useful.
6. **Tests.** Live-fixture JSON test for the AcoustID response parser
   (catches struct-shape drift). Mock worker test asserting that an
   empty library MBID + present `acoustic_id` triggers `AcoustIdLookup`
   not `MbSearchReleases`. Cache-hit test (second call with same
   fingerprint doesn't hit the network).

Out of scope: AcoustID submission of new fingerprints (we read, we
don't contribute). Per-track batch lookup for an entire untagged
album — let the user invoke per-track for now; a "scan untagged"
batch mode is its own follow-up.

## Stem-split follow-ups

### ~~Stem cache holds one entry per track, so recipe A/B re-separates every flip~~ (done)
`stem_cache_key` now takes the recipe cache id and produces
`{path_hash}-{sanitised_cache_id}` (slashes in versioned ids like `hq/v1`
mapped to `-`, kept readable rather than hashed), so entries are
per-(track, recipe) and an A/B flip hits both ways. Migration is
lossless: a legacy bare-hash entry's own `meta.model` already records its
cache id, so `migrate_legacy_stem_entries` (run by the worker before
every lookup; no-op readdir after the first sweep) renames it into the
new scheme — no re-separation. A legacy dir whose new-key twin already
exists is removed as dead bytes; metaless dirs are not provably ours and
are left alone. Cap-pressure note added to the README config docs (each
recipe in play holds its own ~150–250 MB per track against the same
`cache_max_gb`). `prune_stem_cache` `keep_key` still protects exactly the
just-stored entry (now one recipe's), covered by the existing prune test.
Tests: `cache_key_embeds_recipe_and_sanitizes_slashes`,
`two_recipes_coexist_for_the_same_track`,
`legacy_bare_hash_entry_migrates_losslessly_and_hits`,
`legacy_migration_leaves_foreign_dirs_and_superseded_copies`.

### ~~Stem config panel in the TUI~~ (done)
`o` opens a Stem Settings modal (`StemPanel` in `src/tui/app.rs`,
`handle_stem_panel_key` in the modal cascade, `draw_stem_panel` in
`src/tui/ui/mod.rs`), following the theme picker's shape. Recipe picker
lists all `ALL_RECIPES` rows with engine, stem count, and a cost hint;
Enter persists via the same `config::update` path (in-memory/persist
split like `cycle_show_player` so tests never write real config; takes
effect on the next `M`). Engine state shows each engine's discovered
binary and its precedence rung via the new
`provision::discover_engine` / `EngineSource` (`[stems] command` → PATH
→ managed install) — the "why am I not being prompted" answer. Pinned
checkpoints render read-only (curated over free-text, per the scope
note). `u` opens a destructive confirm: `y` runs
`BgCommand::UninstallStemEngine` (worker shells `uv tool uninstall`
via `build_uninstall_command`, evicts the audio-separator-owned model
cache, reports reclaimed bytes), `Y` also deletes the stems cache
(optional — cached stems stay playable engine-free). Refused while a
stem job runs. On the `StemEngineUninstalled` event a `[stems] command`
naming the uninstalled engine is cleared (a stale path there is exactly
what suppresses the re-install consent) while the other engine's
command survives. Tests: `stem_panel_opens_on_the_configured_recipe_and_wraps`,
`stem_panel_confirm_updates_recipe_in_memory_and_closes`,
`stem_panel_uninstall_needs_an_installed_engine`,
`stem_panel_uninstall_dispatches_for_the_selected_recipes_engine`,
`stem_panel_uninstall_blocked_while_a_job_runs`,
`uninstall_event_clears_only_a_matching_command`,
`discover_engine_reports_the_precedence_rung`,
`build_uninstall_command_uses_the_bare_package_name`.

### ~~Bulk stem separation for an album~~ (done)
`M` on an album sidebar entry (Library browse) opens a disk/time-honesty
confirm — track count, cached-vs-to-separate split, projected bytes
against the cache cap (`EST_STEM_BYTES_PER_TRACK` estimate; an explicit
over-cap warning line), and the minutes-per-track-on-CPU note — then
dispatches `BgCommand::SeparateStemsBatch`. Queue model: the whole batch
is ONE superseding worker job (`run_stem_batch` — cache-first per track
via the new read-only `stem_cache_contains`/`cached_stems`, skips
counted, one terminal `StemBatchDone`); an interactive `M` supersedes it
worker-side while the app marks the batch state suspended and
re-dispatches the FULL list once the interactive job's terminal event
lands — cache-first skipping makes resume cost only the remainder, so
no worker-side FIFO/resume machinery exists to drift. `M` on an album
entry while a batch runs cancels it (token trip + state drop + gen bump
so late events are ignored); a mid-track cancel reuses the engine
group-kill and does not count the interrupted track as failed.
Aggregate progress (`track 3/12 (40%)`) renders in the footer;
per-track starts and the end summary (separated / already cached /
failed) land in the log and a toast. Batches never emit `StemsReady` —
they pre-warm the cache, never hijack playback. Engine must already be
installed (no consent flow hidden behind a batch keypress). Tests:
`stem_batch_skips_cached_and_stores_the_rest`,
`stem_batch_cancel_reports_cancelled_without_counting_failures`,
`m_on_album_sidebar_entry_opens_bulk_confirm`,
`bulk_accept_requires_an_engine`, `bulk_accept_dispatches_the_batch`,
`m_on_album_cancels_a_running_batch`,
`interactive_split_suspends_the_batch_and_resumes_after`,
`batch_done_clears_state_and_toasts_a_summary`.

### ~~Playing a paused track after splitting it clears stem state~~ (done)
`play_selected_track` now detects when the selected row IS the loaded
track (path comparison against `playing_track_path()`) and routes through
`resume_or_restart_current` instead of the transition path: a paused
session resumes in place (`AudioCommand::Resume`, position and stem mixer
preserved), and an already-playing session keeps the double-click restart
semantics but rewinds via a stem-aware `AudioCommand::Scrub` to 0:00 —
Scrub rebuilds whichever `NowSource` is live over the same gains `Arc`,
so an active split survives with its toggle state. Neither case records a
skip (same session, mirroring the `prev_track` restart distinction).
Regression tests: `enter_on_paused_split_track_resumes_and_keeps_stems`,
`enter_on_playing_split_track_restarts_via_scrub`, and
`enter_on_a_different_track_still_resets_stems` (the full transition path
— skip + stem reset + fresh `Play` — still fires for a different row).

### ~~Model-checkpoint cache has no eviction~~ (done — pinned-set cleanup)
`prune_model_cache(model_dir, pinned, log)` in `src/stems.rs` implements
the pinned-set policy: a `.ckpt` whose filename is absent from
`pinned_model_files()` (derived from the same constants the recipes are
built on, so a checkpoint swap automatically retires the old file) is
provably retired and is deleted along with its same-stem sidecars (the
`.yaml`/`.json` configs the engine downloads alongside). Files with any
other extension are never touched — audio-separator also stores
engine-managed data there (demucs weight segments, registry json) whose
names we don't control, and deleting those would force silent
re-downloads; a belt-and-braces guard also refuses to delete any file
whose full name is pinned. The worker sweeps after every audio-separator
separation (retired files are never referenced by a running engine, so a
cancelled run can't lose anything). Every eviction logs its size. Root
resolution folded into `paths::zytunes_cache_root()`, now shared by
`default_model_file_dir`, `default_stem_cache_dir`, `cache.rs`,
`art_cache.rs`, and `local_plays.rs`. Surfacing reclaimed-bytes in the
stem config panel remains with that panel's todo. Tests:
`pinned_model_files_names_every_recipe_checkpoint`,
`prune_model_cache_removes_retired_ckpts_and_their_sidecars`,
`p
천​국​의 뒷​마​당
by GODSPEED 音
Why do you love this album?

appears in 602 other collections
download
overlove
by Oblique Occasions
Why do you love this album?

appears in 445 other collections
download
BLYAT
by DARK DESIRE
Why do you love this album?

appears in 569 other collections
download
起​​​源​​​不​​​明 (Remastered)
by 𝐺𝑂𝑅𝐸
Why do you love this album?

appears in 676 other collections
download
香り
by slowerpace 音楽
Why do you love this album?

appears in 707 other collections
download
rune_model_cache_tolerates_missing_dir`.


## Quality of life updates
- ~~stem cache dir override (ie make it easier to locate if you want to get the stems~~ (done — `[stems] cache_dir` config, resolved via `StemsConfig::stem_cache_dir()`; the `o` panel now shows the resolved stem-cache path)
- stems export
- dedupe menu
- ~~stem cache delete~~ (done — `c` in the stem settings panel clears the separated-stems cache without touching the engine; routed through `StemJobs` so it can't race a staging separation)
- CD TUI panel with graphic
- delete functionality (always put in trash, NEVER just delete it
- audit command keys and make controls more intuitive
- import from directory / auto sorting







