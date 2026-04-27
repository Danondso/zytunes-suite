# TODO

## Next up

- **Device playlist browsing and removal** — MTP playlists as first-class objects in the device view (currently we browse artists/albums/tracks only). Would need to list playlist objects, show their track references, and support playlist-level removal. Separate from rodio in-memory queue.

## Future

- **Diagnostics: route playlist/listen-log writes through the project Logger** — `src/playlist_store.rs` and `src/listen_log.rs` use `eprintln!` for failure paths instead of the project's `Logger = Arc<dyn Fn(&str) + Send + Sync>` pattern (the same pattern `src/cache.rs` already uses to avoid stderr corrupting the rendered TUI). Both files run from the TUI hot path on every play/skip and every playlist save, so a stray write at the wrong moment can scribble through the ratatui frame. Goal: thread an optional `Logger` into `PlaylistStore::{load_from, save_to}` and `ListenLog::{append, append_to_disk, save_to}`, defaulting to `default_logger` (stderr) for the CLI and a closure-routed-into-`SyncMessage` for the TUI. Drop the module-level `eprintln!` calls. Out of scope for the playlists/recommender PR — touches every diagnostic call site and deserves its own pass.
- **Diagnostics: tighten the existing-playlist lookup error path** — `NativeSession::import_playlist` swallows transient `get_object_handles` / `get_object_info` errors via `.ok()` when scanning for an existing `<name>.zpl` to update in place. Now that the filename match is correct (commit 8a517a2), a transient stall during this scan still falls through to the create branch — which now correctly hits the case-insensitive existence check on the *next* sync. The blast radius is small (one extra `<name>.zpl` survives until the next successful scan), but propagating the handle-list error rather than silencing it would surface USB issues sooner. Out of scope for this PR; small enough to bundle into a future MTP-error-handling sweep.
- **Configurable transcode quality** — currently hardcoded to symphonia → LAME `NearBest` VBR (~190kbps) in `src/lib.rs:326`. Add CLI flag and config key for bitrate/quality.
- **Theming (remaining)**
  - Theme preview screenshots in docs
- **Player Support (remaining)**
  - Soundbar/waveform visualizer effect (research terminal audio visualizers — rodio already exposes the sample stream)
- **Radio Support**
  - Research a terminal radio source we can pipe into the rodio player.
- **Scrobbling** — hook into TUI player track-change events to submit listens
  - Last.fm (auth handshake, now-playing + scrobble submit)
  - ListenBrainz (token auth, listen submission + feedback)
  - Config in `~/.config/zytunes/config.toml`, opt-in per service
  - Respect scrobble rules (≥50% played or ≥4min, ≥30s track length)
- **UX Audit**
  - Commands have been made organically
  - Audit mappings
  - Suggest improvements / redundant / confusing
- **One Offs**
  - perf: batch transcoding — transcoding is sequential; `rayon` is already a dependency but unused in the sync loop.
- **Unify cache implementations** — four device-scoped caches plus the album-art cache are hand-rolled with no shared abstraction. The dirlib library scan cache stays out of this work: it lives at `$HOME/.cache/zytunes` precisely so sibling worktrees pointed at the same `~/Music` reuse one scan, which is the entire reason `ZYTUNES_CACHE_DIR` exists (it isolates device caches per-worktree without dragging the library scan along). Goal: one cache layer for everything else, with on-disk formats designed for **export** (a user can bundle their cache, ship it to another machine or back it up, and reimport it).
  - **In scope:**
    - `src/art_cache.rs` — album art, raw JPEG + `.meta.json` sidecar, `$HOME/.cache/zytunes/art/{hash(artist,album)}.jpg`, source-file `(mtime,size)` fingerprint. Worth exporting (regenerating across a large library is expensive).
    - `src/mtp/native.rs::TrackCache` (lines 168–330) — custom tab-separated text with `#free_bytes:` header, `{device_cache_base()}/.zytunes-track-cache-{serial}`, **fast-path header rewrite** in `update_free_bytes` (258–277) is load-bearing.
    - `src/mtp/native.rs::DeviceLibrary` (lines 71–150) — custom tab-separated text (`HDR`/`ART`/`ALB`), `{device_cache_base()}/.zytunes-library-cache-{serial}`, targeted invalidation via `invalidate_library_for_path` (1615–1642).
    - `src/mtp/native.rs::sync_progress` (lines 1349–1367, 1669–1714) — raw 530-byte binary blob from vendor op `0x9217`, `{device_cache_base()}/.zytunes-sync-progress-{serial}`.
  - **Out of scope (do not touch):**
    - `src/cache.rs` (dirlib library scan) — keep as-is. Its deliberate `ZYTUNES_CACHE_DIR` opt-out is the load-bearing behaviour. Test in `cache.rs:192-207` must continue to pass; the unification work must not import or replace this module.
  - **Export constraint shapes the design:**
    - Every cache file in scope must be self-describing: a fixed magic header, a `u32` schema version, and a typed payload. No more "what does the second field on this `ART` line mean?" — the format itself answers.
    - One canonical on-disk layout per cache so `zytunes cache export <path>` can `tar.zst` the relevant files (album art bundle for `Scope::Shared`, per-device bundle for `Scope::Device`) and `zytunes cache import <path>` can validate magic + version and place files back. Per-device bundles key on the device serial baked into the file header, not the filename, so importing onto a different host still works.
    - Pick **bincode + serde** for the typed caches (track list, device library, art metadata sidecar). It is compact, self-versioning when paired with an outer envelope, and serde-derive lets us evolve schemas without hand-writing parsers. Track-cache and library-cache custom text formats get retired as part of the port — easier than retrofitting export into them.
    - Sync progress stays a raw blob (it is opaque vendor data) but gets wrapped in the same magic+version envelope so the export tool can identify it.
    - Album art JPEGs stay as-is on disk (the file IS the cached artifact); only the `.meta.json` sidecar gets the envelope treatment, switching to bincode at the same time.
  - **Design sketch:**
    - New `src/cache/mod.rs`:
      - `CacheEnvelope<T>` — magic bytes (`b"ZYTC"`), `schema_version: u32`, `created_unix: u64`, `payload: T`. Single `read`/`write` pair handles framing + atomic write (`.tmp` + rename).
      - `Scope::Shared` → `$HOME/.cache/zytunes/...` (album art only, in this work).
      - `Scope::Device { serial }` → `paths::device_cache_base()/...{serial}` honouring `ZYTUNES_CACHE_DIR`.
      - `PartialUpdate` trait — opt-in for caches that need surgical rewrites without rehydrating the full payload. `TrackCache::update_free_bytes` becomes an `impl PartialUpdate` that knows where in the bincode stream the free-bytes field sits (or moves the field into a separate small file in the same envelope so we don't have to seek into a serialized blob).
      - `Fingerprint { mtime: u64, size: u64 }` — shared helper, currently duplicated between `cache.rs` and `art_cache.rs`. Lift it out so the art cache can use it; dirlib keeps its own copy unchanged.
    - New `src/cache/export.rs` — `pack(scope) -> tar.zst` and `unpack(path) -> ()`. Validates envelope magic + version on each entry. Refuses to import device-scope bundles whose serial does not match the connected device unless `--force` is passed.
    - New CLI subcommands (`src/main.rs`): `zytunes cache export <path>` and `zytunes cache import <path>`. Per-device export defaults to "currently connected device"; shared export grabs the art bundle.
  - **Migration order:**
    1. Land `cache::CacheEnvelope` + `Scope` + atomic-write helper + `PartialUpdate` trait. No callers yet.
    2. Port `art_cache.rs` — simplest case, exercises `Scope::Shared` and the `Fingerprint` helper. Keep JPEGs raw, switch sidecar to enveloped bincode.
    3. Port `sync_progress` — trivial raw-bytes case wrapped in the envelope, exercises `Scope::Device`.
    4. Port `DeviceLibrary` — bincode payload, exercises targeted invalidation through the new API. Migration code reads the old text format once if present, rewrites as bincode, deletes the legacy file.
    5. Port `TrackCache` last — has the fast-path constraint. Decide between (a) `PartialUpdate` seeking into bincode or (b) splitting free-bytes into a sibling file under the same envelope; pick whichever benchmarks cleanly. Same legacy-format migration step as `DeviceLibrary`.
    6. Wire up `zytunes cache export` / `import` and document the bundle format.
  - **Test gates:**
    - `cache.rs:192-207` (worktree-shared dirlib) keeps passing untouched.
    - New: `ZYTUNES_CACHE_DIR=/tmp/foo` redirects all four in-scope caches but leaves dirlib + art untouched (art is `Scope::Shared` by design).
    - New: round-trip export → wipe → import reproduces the cache byte-for-byte (excluding timestamps in the envelope).
    - New: importing a bundle written with `schema_version - 1` fails cleanly with a "run an older zytunes to read this" error rather than a bincode panic.
    - New: `TrackCache::update_free_bytes` benchmark stays within ~2x of the current line-rewrite cost; if the seek-into-bincode approach loses, fall back to the split-file design.
- **Code Audit**
  - Rule of threes should be observed, what code is duplicated > 3 times or two even if the code block is large
  - Rust best practices
  - files too big? 
- **iPod Support (remaining)** — core music sync + delete work, non-music features left
  - **Photo sync** — iPod Classic has a Photos database (separate from iTunesDB) at `iPod_Control/Photos/Photo Database`. Uses `mhfd` container format (same as ArtworkDB). Needs ITHMB generation for iPod screen thumbnail + main preview sizes. Zune has `cmd_photo_sync` + `session.import_photo` as reference. libgpod has a photo writer in `db-artwork-writer.c::ipod_write_photo_db` we can port.
  - **Video sync** — iPod Classic plays MP4/M4V with specific constraints (320x240/640x480, H.264 baseline). Videos go in `iPod_Control/Music/F*/` alongside audio (not a separate directory). iTunesDB entries use `mediatype = 0x02` at mhit +0xD0 (we hardcode `1` for audio). Zune has `cmd_video_sync` + `transcode_and_import_video` as reference. Need to transcode to iPod-compatible MP4 via ffmpeg, set `mediatype = 2`, set video-specific mhod types.
  - **User playlist sync from iTunes XML** — currently we only preserve the master playlist via raw blob replay. iTunes user playlists (type 2 mhsd, non-master mhyps) could be added from iTunes Library.xml parsing. Would need mhip writing per playlist member.
  - **Storage/model detection** — mirror of `zune_model_from_storage` in `src/device/zune.rs:40`, but for iPod Classic (30GB / 60GB / 80GB / 120GB / 160GB). Surface in TUI alongside the detected model.
  - **Track rating write-back** — read `Play Counts` on connect, apply ratings/play counts back into the iTunesDB on next sync (currently we delete it like libgpod does, losing the data).
- **Philips GoGear support** — user has a couple of units, worth attempting
  - Identify which models (VID/PID, firmware generation — SA/HDD vs Vibe vs Ariaz etc.)
  - Transport: most GoGears are UMS/MSC (plain mass storage) — no MTPZ/iTunesDB lift needed, just file copy + folder conventions
  - Some later models use MTP (non-encrypted); `zune-mtp` transport is reusable, auth path is not
  - Check if any model needs a proprietary DB (SA52xx songdb.dat) vs pure tag-based playback
  - Slot into `DeviceSession` trait once scoped

## Done

- **Library-side "on device" indicator** — library track list prefixes `✓` for tracks already on the connected device (`src/tui/ui.rs:887`), and the sidebar shows `✓`/`◐` for full/partial coverage (`src/tui/ui.rs:405`).
- **Scrubber / seek controls** — `AudioCommand::Scrub { delta_ms }` with ±5s bindings (`src/tui/audio.rs:26`, `src/tui/main.rs:347`).
- **Bespoke album art panel** — panel resizes horizontally to art width with right-anchored layout and custom border junctions (`src/tui/ui.rs:664`).
- **Toggleable ASCII art album covers** — `AlbumArtStyle` enum + `toggle_album_art_style()` switches between halfblock and ASCII rendering (`src/tui/app.rs:153`, `:482`).
- **Log scrolling + clipboard** — PageUp/PageDown scroll the log 10 lines at a time (`src/tui/app.rs:322`). `L` dumps the log to `/tmp/zytunes-log.txt` and copies the path to the clipboard via `arboard`.
- **User-toggleable player panel** — `P` cycles between auto / force-hidden / force-shown. Preference persists to `config.toml`; `App::should_show_player` is the single source of truth consumed by both `LayoutMetrics` and the pre-render pass.
- **Marquee scroll for long names** — the selected row in the sidebar, album track list, and main track table now scrolls its name when it exceeds the column width. Uses the shared `marquee` helper in `src/tui/ui.rs` (12-frame head pause, 4-frame step).
- **iPod Classic music sync** — pure Rust iTunesDB parser/writer (`ipod-db` crate) with raw blob replay for lossless roundtrip of existing tracks + libgpod-ported from-scratch mhit writer for new tracks. Handles hash58 signing, ArtworkDB + ITHMB thumbnails for album art, 8-dataset output (types 1/3/2/4/8/6/10/5), sort indexes with libgpod-style tiebreakers, and case-sensitive extension normalization. `IpodBackend` + `IpodSession` integrate via `DeviceBackend` / `DeviceSession` traits — CLI and TUI auto-detect and connect to iPods alongside Zunes.
- **Local audio playback** — play/pause/skip for local tracks via rodio (`tui/audio.rs`). Basic queue support integrated into TUI.
- **Native IOKit USB backend** — replaced libusb/aft-mtp-cli with Apple's native IOKit for direct MTP/MTPZ communication via the `zune-mtp` workspace crate. Eliminates all external MTP tool dependencies.
- **Device content view** — toggle between Library and Device browse modes (`v` key). Reuses artist/album/track panels for device content. Device tracks parsed from `/Music/Artist/Album/track` directory structure. `a` key removes tracks in device mode. Supports artist, album, and track-level removal.
- **Sync engine** — diffs local music against device contents, pushes only new tracks, deduplicates, supports playlist syncing.
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely.
- **TUI sync status on Zune art** — loading spinner, track count, syncing spinner, and queue count all displayed on the Zune ASCII art screen.
- **Disable sync until device connected** — `execute_sync()` guards against syncing when no device is present.
- **Batch import performance** — `TrackCache` in `NativeSession` caches the device library to `~/.zytunes-track-cache-{serial}`, avoiding full reload over USB 1.1.
- **Proper rm for special characters** — path escaping handled natively in `zune-mtp` session operations.
- **Theming** — 16 built-in theme presets (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original). Live preview picker (`t` key). Config persisted to `~/.config/zytunes/config.toml`. User-defined custom themes via `[themes."Name"]` TOML tables — inherits from a `base` built-in and overrides any colors (`#rrggbb`/`#rgb`), modifiers, border type, or accent animation.
- **Album art on import** — the Zune rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
