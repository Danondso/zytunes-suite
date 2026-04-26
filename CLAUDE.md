# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Rust (zytunes workspace)
cargo build                   # Debug build (all workspace crates)
cargo build --release         # Release build
cargo check                   # Fast type-check without building
cargo run -- <command>        # Run CLI with arguments (ls, push, rm, sync, library, photo-sync, video-sync, help)
cargo run --bin zytunes-tui   # Run interactive TUI
cargo test                    # Run tests (all workspace crates)
cargo test -p zune-mtp        # Run zune-mtp crate tests only
cargo test -p ipod-db         # Run ipod-db crate tests only
cargo fmt                     # Format code
cargo clippy                  # Lint
./install.sh                  # Build release + install both zytunes and zytunes-tui to /usr/local/bin
./uninstall.sh                # Remove both zytunes and zytunes-tui from /usr/local/bin

```

## What This Is

A Rust tool for syncing music (and, on the Zune, photos/videos) to a Microsoft Zune 30 or classic iPod from macOS and Linux. Detects the device over USB and speaks whichever protocol the device needs: MTPZ (encrypted MTP) for the Zune, raw mass-storage iTunesDB writes for the iPod.

CLI commands: `ls [path]`, `push <files...>`, `rm <paths...>`, `sync <type> <name>`, `library [query]`, `photo-sync [dir]`, `video-sync [dir]`, `help`.

**TUI** (`zytunes-tui`): Interactive terminal UI for browsing the music library (directory scan) and device content, connecting to the device, managing a sync queue, removing tracks from the device, and monitoring sync progress. Library scanning runs asynchronously on a background thread at startup. The TUI supports two browse modes: Library and Device, toggled with `v` (the sidebar/device panel auto-labels itself "Zune" or "iPod" based on what's connected). Theme picker on `t`, album-art render style (`halfblock`/`ascii`) toggled with `T`, now-playing panel visibility cycled with `P`, and `L` dumps the log to `/tmp/zytunes-log.txt` and copies that path to the system clipboard via `arboard`.

## Architecture

**Workspace layout:** The project is a Cargo workspace with three members: the root `zytunes` crate, the `zune-mtp` MTPZ library crate, and the `ipod-db` iTunesDB parser/writer crate. All workspace crates specify `rust-version = "1.94"`. A `tools/mtp-probe` directory (gitignored) contains the MTP vendor operation probe tool used for reverse engineering.

**Device abstraction:** `src/device/` defines `DeviceBackend` (detect + open session) and `DeviceCapabilities` (family, supported formats, transcode target, music root, max art dims). `ZuneBackend` (MTPZ-over-IOKit) and `IpodBackend` (USB mass storage + iTunesDB) both implement it, so CLI and TUI iterate backends instead of hardcoding Zune. `DetectedDevice` carries a type-erased `backend_data: Box<dyn Any>` that the backend downcasts when opening a session.

**USB strategy:** `rusb` handles device detection (descriptor reads only). For Zune MTP/MTPZ communication, the `zune-mtp` crate provides direct IOKit FFI, bypassing libusb (which fails on data-out USB operations that the Zune's MTPZ handshake requires). iPods mount as normal USB mass-storage and are driven through the filesystem — no MTP involved.

**Native IOKit backend (`zune-mtp/`):** A workspace crate providing direct MTP/MTPZ communication via Apple's IOKit framework:
- `transport.rs` — IOKit USB transport: device discovery via `IOServiceMatching` with vendor/product ID filtering, interface claiming, bulk pipe endpoint discovery, read/write operations via `IOUSBInterfaceInterface` COM-like vtables. Debug logging via `IOKIT_USB_DEBUG` env var. `WritePipe`/`ReadPipe` automatically call `ClearPipeStall` and retry once on recoverable IOKit errors (top 16 bits `0xe000`, covering `kIOUSBPipeStalled`, `kIOReturnNotResponding`, etc.) — transient stalls from mid-sync cascades or cable jostles no longer surface as fatal. Read-timeout path aborts the IN pipe and clears stalls on BOTH bulk endpoints so the OUT pipe does not desync from the device. `read_container_with_timeout(secs)` exposes a caller-supplied timeout for ops where 30s is too tight
- `container.rs` — MTP/PTP container format: building command/data containers, parsing response headers, operation and response code enums. Operation codes: `EnableTrustedFilesOperations` = `0x9214`, `DisableTrustedFilesOperations` = `0x9215`. Vendor operations: `GetZuneMetadataDatabase` = `0x9217`, `GetAcquiredItems` = `0x9219`, `SetDeviceSyncProgress` = `0x922a`, `GetDeviceSyncProgress` = `0x922f`
- `transport/tcp.rs` — TCP transport for MTP/IP (PTP over IP) wireless communication. SSDP discovery on `239.255.255.250:1900`, PTP/IP session init handshake, MTP container wrapping/unwrapping for PTP/IP packet types
- `transport/libusb.rs` — libusb fallback transport for non-macOS hosts. Has `read_container_with_timeout` parity with the IOKit backend, but does NOT yet implement `clear_halt`-based stall recovery (see `todos.md` — "Linux transport parity")
- `lib.rs` — Crate root, exports `MtpError` enum with variants: `Usb`, `Protocol`, `Crypto`, `KeyLoad`, `Io`. All crate functions return `Result<T, MtpError>` instead of `Result<T, String>`
- `session.rs` — MTP session management: OpenSession, GetDeviceInfo, GetStorageIDs, GetStorageInfo, GetObjectHandles, GetObjectInfo, SendObjectInfo/SendObject, DeleteObject, SetDevicePropValue (MTP string properties), GetObjectPropsSupported, GetObjectPropList, SetObjectPropValue, SendObjectPropList, GetObjectReferences, SetObjectReferences. Data-out operations send header and payload as separate bulk writes (Microsoft/Zune requirement). `SetObjectPropValue` uses a 45 s response timeout (album art commits to flash can take tens of seconds under load; the default 30 s cascaded into wedged sessions). Includes MTP string and ObjectInfo dataset parsing. Uses `le_u16`/`le_u32`/`le_u64` helpers for binary parsing
- `mtpz.rs` — Full MTPZ authentication: sets SessionInitiatorVersionInfo (`0xD406`) before handshake, RSA-1024 raw signing, PSS-like certificate message generation, AES-128-CBC decryption, CMAC key extraction from device response, session confirmation with data-only fallback, EnableTrustedFilesOperations
- `proplist.rs` — MTP ObjectPropList builder for SendObjectPropList (0x9808): constructs binary property list payloads with string, u16, and u32 property types. Property constants for object filename, name, artist, track, genre, artist ID, date authored, and representative sample data
- `iokit_ffi.rs` — Raw FFI: IOUSBDeviceInterface and IOUSBInterfaceInterface vtable structs, CoreFoundation helpers (CFString, CFUUID, CFNumber), IOKit service matching functions

**Key modules:**
- `device/mod.rs` — `DeviceBackend` trait, `DeviceCapabilities`, `DeviceFamily` (`Zune` | `Ipod`), `DetectedDevice` with type-erased `backend_data`
- `device/zune.rs` — `ZuneBackend` / `ZuneDevice` / `ZuneDeviceData`. rusb-based scan for VID `0x045e`, opens a `NativeSession` over IOKit
- `device/ipod.rs` — `IpodBackend` / `IpodDeviceData`. Detects a mounted classic iPod's `iPod_Control/` root, opens an `IpodSession`
- `lib.rs` — Crate root, exports `SyncType` enum (`Artist`, `Album`, `Track`) with `FromStr`/`Display` impls, plus the backend traits. Also exposes `check_ffmpeg_available()` and `transcode_and_import_video()` — video-sync shells out to ffmpeg for the WMV2/WMAv2 transcode the Zune expects
- `mtp/mod.rs` — `DeviceSession` trait abstracting device operations (ls, import_track, rm, collect_all_tracks, save_sync_progress, `prewarm_library`, `refresh_storage_cache`, import_photo, import_video, collect_all_videos) for testability. `prewarm_library` is called once at connect so the first import pays no artist/album mapping setup cost; `refresh_storage_cache` is called after sync/remove to bump the cached `#free_bytes:` header and avoid invalidating track/library caches on next reconnect. `TrackMeta` carries pre-parsed lofty tags from the library so the session does not re-read the file. All trait methods have doc comments
- `mtp/native.rs` — `NativeSession` implements `DeviceSession` using `zune-mtp`: opens IOKit USB transport, performs MTPZ handshake, provides ls/import/rm/collect_all_tracks. Resolves device paths by walking object handles. `TrackCache` struct handles per-device track caching to `~/.zytunes-track-cache-{serial}` with an `update_free_bytes()` fast path that rewrites only the header line (preserves track entries without re-serialising). `DeviceLibrary` struct caches artist/album MTP handles to `~/.zytunes-library-cache-{serial}` for fast reconnects. Sync progress cached to `~/.zytunes-sync-progress-{serial}`. `save_sync_progress` routes through the firmware-guarded `get_sync_progress` so pre-3.0 Zunes that reject the vendor op stay silent instead of emitting a user-visible warning every sync. Tracks an `art_disabled` session flag: after a single `SetObjectPropValue` failure on album art (one bad JPEG can wedge the MTP session), subsequent tracks skip art entirely for the remainder of the session to avoid cascading 45 s read timeouts. `extract_album_art` normalises output through a defensive path — explicit RGB8, `JpegEncoder` at quality 85, no ICC/EXIF carry-over, SOI/EOI validation — to reduce the probability of hitting the Zune's JPEG decoder bug. Setting `ZYTUNES_DUMP_ART=1` writes the last art payload to `/tmp/zytunes-last-art.jpg` for inspection. `MtpResultExt` trait converts `MtpError` to `String` at the `DeviceSession` boundary
- `mtp/ipod_session.rs` — `IpodSession` implements `DeviceSession` for classic iPods. Backed by the `ipod-db` crate: mhit/mhbd writes via `itunesdb_write`, hash58 signing, ArtworkDB + ITHMB thumbnails. Copies files under `iPod_Control/Music/F00..F49/` with lowercased extensions (firmware matches `.mp3`/`.m4a` case-sensitively). Reassigns track IDs from `FIRST_IPOD_ID = 52`, deletes `Play Counts` after import (libgpod-parity), and exposes device paths as `Artist/Album/Title.ext` in the TUI even though on-disk they live in hashed F-dirs
- `mtp/parse.rs` — `DeviceEntry` struct and parsing utilities
- `mtp/zmdb.rs` — ZMDB (Zune Metadata Database) parser. Parses the proprietary binary format returned by vendor op `0x9217` into tracks, albums, artists, genres. Used as the fast path in `collect_all_tracks` (single MTP call instead of hundreds)
- `library.rs` — `MusicLibrary` trait abstracting library queries (artists, albums, track lookups) and the shared `Track` type. `Track` carries the always-present identity fields (id, name, artist, album) plus a wide bag of `Option` extended-metadata fields populated by the dirlib scanner: `album_artist`, `composer`, `conductor`, `lyricist`, BPM, initial key / mood / language, ISRC / barcode / catalog number / publisher / copyright, encoder + encoder settings, original_*, lyrics, all seven MusicBrainz IDs (`mb_track_id`, `mb_recording_id`, `mb_release_id`, `mb_release_group_id`, `mb_artist_id`, `mb_release_artist_id`, `mb_work_id`), four ReplayGain values, plus audio properties (`sample_rate`, `channels`, `bit_depth`, `audio_bitrate_kbps`, `overall_bitrate_kbps`, `file_size_bytes`). Every `Option` field uses `#[serde(default, skip_serializing_if = "Option::is_none")]` so the JSON cache is compact and forward-compatible — old caches missing newly-added fields still load
- `dirlib.rs` — `DirectoryLibrary` implements `MusicLibrary` by scanning a folder recursively for audio files. Reads metadata from all audio formats via lofty (FLAC, M4A, OGG, WAV, MP3, etc.), populating the full extended `Track` metadata bag in one tagged-file probe (`tagged.properties()` for audio props, `ItemKey::*` for tag fields). BPM uses `Bpm` → `IntegerBpm` fallback for ID3v2; publisher uses `Publisher` → `Label` fallback. Falls back to `Artist/Album/Track.ext` path structure for untagged files. Generates stable track IDs via path hashing
- `cache.rs` — on-disk metadata cache for `DirectoryLibrary`, keyed by a hash of the scan-root path. Deliberately lives under `$HOME/.cache/zytunes` and does NOT honor `ZYTUNES_CACHE_DIR` so sibling worktrees pointed at the same `~/Music` share one cached scan. Carries a `CACHE_SCHEMA_VERSION` constant — bumped whenever the on-disk shape of `Track` changes such that stale cache entries would silently sit at `None` for newly-added fields. On version mismatch the cache is discarded and a re-scan is forced (one wart beats per-file `None` rot until each mtime ages out). All cache operations route diagnostics through a `Logger` (`Arc<dyn Fn(&str) + Send + Sync>`); CLI uses `default_logger` (stderr), the TUI passes a closure that forwards to the `SyncMessage` channel so cache warnings don't corrupt the rendered terminal
- `paths.rs` — resolves device-scoped cache locations. `ZYTUNES_CACHE_DIR` (when set) redirects device caches (track list, library handles, sync progress) into one directory for per-worktree isolation; the workspace `.cargo/config.toml` points it at `./.zytunes-cache`. The dirlib cache explicitly opts out of this override — see `cache.rs`
- `main.rs` — CLI entry point, `run()` dispatcher, `cmd_sync`/`cmd_push`/`cmd_rm`/`cmd_ls`/`cmd_library`/`cmd_photo_sync`/`cmd_video_sync` commands, `sync_to_device()` engine, `find_matching_tracks()` (takes `SyncType`), audio transcoding via symphonia + LAME, video transcoding via ffmpeg shell-out
- `tui/main.rs` — TUI entry point (`zytunes-tui` binary), event loop (50ms poll), terminal setup/teardown
- `tui/app.rs` — TUI application state (`App`), input handling, panel navigation. Panels: `Library` (sidebar), `Albums`, `TrackList`, `Device`, `SyncQueue` — cycled via Tab. Two browse modes: `BrowseMode::Library` and `BrowseMode::Device` (toggled with `v`). Two sidebar modes: `Artists`, `Albums` (keys `1`/`2`). Sidebar selection positions saved per browse-mode × sidebar-mode pair. Device mode builds in-memory artist/album/track index from `{Artist}/{Album}/{Track}` paths via `DeviceState::add_indexed_track` / `remove_indexed_track` — incremental binary-search inserts and artist-scoped lookup-set rebuilds, so sync deltas are O(log N) per track instead of O(N²) full resorts. `build_device_index` is retained for initial load and drains `tracks` into the incremental path. `flush_device_index` now only runs UI re-derivations (`rebuild_artist_device_status`, sidebar/track retag) since the index itself is kept live. `execute_sync` filters queued items against `device.track_set` before dispatching so already-on-device tracks are skipped (with a log entry), and shows a toast if the queue was entirely duplicates. `App` uses `DeviceState` sub-struct (holds device status, name, family, firmware, serial, storage, tracks, etc.) and `SyncState` sub-struct (holds sync queue, status, current track, and log). `should_show_player()` is the single source of truth for the now-playing panel, consulted by both `LayoutMetrics` and the pre-render pass; `P` cycles auto → force-hidden → force-shown and `cycle_show_player_in_memory` persists the choice to config. Track-info popup state lives on three `App` fields: `show_track_info`, `track_info_scroll`, and `track_info_lib` — the last is a cloned library `Track` resolved once in `open_track_info` so the renderer reads it directly each frame instead of re-running `tracks_by_name` (would be O(N) per ~50 ms tick on the directory library). `close_track_info` clears all three. `cycle_panel`, `cycle_panel_back`, and the `move_up` / `move_down` track-list selection paths all dismiss the popup so it never persists across selection changes. State machines: `DeviceStatus` (Disconnected → Detecting → Connecting → Connected), `SyncStatus` (Idle → Running)
- `tui/background.rs` — background worker thread communicating via `mpsc` channels. Commands (`BgCommand`): `LoadLibrary`, `LoadAlbumArt` (with explicit artist/album fields so the per-album art cache keys on structured data), `Connect`, `LoadDeviceTracks`, `Disconnect`, `ExecuteSyncQueue`, `RemoveFromDevice`, `CancelSync`. Events (`BgEvent`): `LibraryLoaded`, `AlbumArtLoaded`, `DeviceDetected`, `SessionReady`, `SessionFailed`, `DeviceTracksLoaded`, `SyncProgress`, `SyncTrackDone`, `SyncComplete`, `RemoveProgress`, `RemoveComplete`, `StorageUpdated`, `Error`, `SyncMessage`. On `Connect`, iterates registered backends (`ZuneBackend` then `IpodBackend`) and opens the first that detects hardware; calls `prewarm_library` so the first sync doesn't pause for library scan. After successful sync/remove, calls `refresh_storage_cache(free_bytes)` so the next reconnect sees a small free-space delta. Uses an `is_device_gone()` predicate to detect fatal USB failure strings (`0xe00002c0`, `0xe00002ed`, `retry after ClearPipeStall`, `ReadPipe timed out`); when it trips mid-sync or mid-remove, the worker aborts the remaining queue, skips storage refresh, drops the session, and emits `SessionFailed("Device disconnected from USB")` so the TUI flips back to Disconnected
- `tui/anim.rs` — animation data for connection and sync progress
- `tui/audio.rs` — local audio playback via rodio. Calls `device_sink.log_on_drop(false)` immediately after construction — rodio 0.22's `DeviceSink` writes a raw `eprintln!` on drop that would otherwise corrupt the rendered TUI when the audio thread exits
- `tui/ui.rs` — ratatui rendering. Layout: left column (device panel with device-specific ASCII art + storage bar, sync queue, log), center (sidebar + albums + track list + footer), right (toggleable keys reference via `h`). Album detail view shows ZIP disk ASCII art with metadata alongside track table. Track table supports sort cycling (`s`). `AlbumArtCache` enum holds both `Halfblock` and `Ascii` variants — `T` toggles the style and invalidates the inactive variant. Selected rows marquee-scroll when the name overflows the column (12-frame head pause, 4-frame step); non-selected rows stay stable. Overlays: help (`?`), search (`/`, live-filters sidebar), theme picker (`t`), track-info popup (`I`). Toast notifications (auto-dismiss 5s, green/red borders). Footer shows context-sensitive track count and action hints, adapts to Library vs Device mode. Track-info popup is rendered by `draw_track_info_overlay`: 70%-of-terminal sizing clamped to `[40,90] × [8,30]` cells with a `width < 30` guard that falls back to the title-only block. Body is built by `format_metadata_pairs(track, lib_track)` returning `Vec<MetadataRow>` (`Section(name)` divider rows + `Field { key, value }` rows), grouped into Identity / Classification / Credits / Identifiers / MusicBrainz / ReplayGain / Audio / File / Notes / Device sections. Empty sections are suppressed so lightly-tagged tracks stay terse. Each `Field` value is run through the existing `marquee()` helper sized to the value column width (`inner.width - 21`), so overflowing values scroll on the same 12-frame-pause / 4-frame-step rhythm as the main track-list selection while short values stay stable. The popup is gated to Library browse mode in the dispatcher — device-side `TrackInfo` lacks the extended metadata, so pressing `I` in Device mode shows a toast instead
- `tui/theme.rs` — `Theme` struct with color/style fields (sidebar, selection, main/alt row, border, footer, header, dim, error, success, progress). 16 built-in presets: iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original. `find_theme_index()` for name-based lookup. `Theme::active_border()` applies BOLD + `selection_bg` to the focused panel's frame so it pops against the inactive border colour regardless of theme. `init_themes()` runs at TUI startup and merges user-defined `[themes."Name"]` config tables (inheriting from a named `base` built-in) into a `OnceLock<Vec<&'static Theme>>`; malformed user themes log to stderr and are skipped
- `tui/config.rs` — TOML config file at `~/.config/zytunes/config.toml` (serde + toml). Stored fields: `theme`, `music_dir`, `photo_dir`, `video_dir`, `album_art_style` (`ascii` | `halfblock`), `show_player` (`None` = auto, `Some(bool)` = forced), and user-defined `[themes."Name"]` tables via `UserTheme`. `update_contents()` refuses to write if the existing file is unparseable — prevents a stray keypress from clobbering hand-edited fields

**Configuration:** Music library is resolved by `load_library()`: (1) `ZYTUNES_MUSIC_DIR` env var, (2) `music_dir` from config.toml. Both point at a folder that is recursively scanned for audio files. TUI config (theme, `music_dir`, `photo_dir`, `video_dir`, `album_art_style`, `show_player`, custom `[themes."Name"]` tables) is stored in `~/.config/zytunes/config.toml`.

**Cache locations:**
- Device-scoped caches (`~/.zytunes-track-cache-{serial}`, `~/.zytunes-library-cache-{serial}`, `~/.zytunes-sync-progress-{serial}`) are redirected by `ZYTUNES_CACHE_DIR` — set automatically per-worktree via `.cargo/config.toml` to `./.zytunes-cache`.
- The dirlib metadata cache at `$HOME/.cache/zytunes` intentionally ignores that override so worktrees sharing a `~/Music` root reuse one lofty scan.
- The TUI album-art cache at `$HOME/.cache/zytunes/art/` is keyed on `(artist, album)` and fingerprinted by `(mtime, size)` so re-tagging a source file invalidates the cached rendering automatically. Repeat views of the same album skip tag parsing entirely.

**Licensing:** MIT license (`LICENSE`). Third-party attribution in `THIRD_PARTY.md` (MTPZ keys from libmtp-zune, mhit writer ported from libgpod).

**External tool dependencies:** `libusb` (via rusb). Optional: `ffmpeg` — **required** for `video-sync` (wmv2/wmav2 transcode to the Zune's native video format), optional for TUI playback of WMA files. Audio sync uses pure-Rust transcoding and does not need ffmpeg.

**Audio transcoding:** Non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are automatically transcoded to MP3 using pure Rust libraries (symphonia for decoding, mp3lame-encoder for encoding, lofty for metadata). Native formats (MP3, WMA, AAC) skip transcoding entirely. Album art is resized to 200x200 JPEG via the image crate (Zune 30 rejects larger art with error `0xa803`). The M4A/ALAC path trims trailing silence leaked by symphonia's isomp4 demuxer: edit-list (`elst`) atoms parse but never apply, so the trimmed region decodes to bit-exact zeros and LAME would re-encode it as real silence. The transcoder holds back zero-valued frames during encoding and drops them at EOF; they only reach LAME once a later non-zero sample proves they were mid-track, preserving intentional silence between audio regions. Also: `FlushGap` (not `FlushNoGap`) on standalone-track flush, and mono sources route through `MonoPcm` instead of the stereo-hardcoded `InterleavedPcm`.

## Claude Code Slash Commands

Custom slash commands in `.claude/commands/`:

- `/review` — local code review of the current branch diff. Reviews for correctness, Rust best practices, cleanliness, readability, refactoring opportunities, and security. Posts findings as line-level PR comments via `gh api`, fixes issues in priority order, then replies to each comment with the resolution. Falls back to terminal output if no PR exists.
- `/fix-ci` — diagnoses failing CI checks on the current branch's PR. Fetches check statuses and failure logs via `gh`, correlates with local source, then enters plan mode with a structured fix plan for approval.
- `/triage-ci` — pulls CI check status for the current branch's PR and triages failures into a categorized report (build / test / lint / flake / infra / external regression / config drift / unknown), with severity and recommended action per failure. Stops at triage — does not fix.
- `/debug` — spawns research agents on a problem, aggregates findings into `DEBUG.md`, then forms ranked hypotheses with validation steps. Use for non-trivial bugs spanning multiple angles (code path, git history, protocol/spec knowledge, prior incidents).

`/review` and `/fix-ci` run `cargo fmt` and `cargo clippy -- -D warnings` as part of their fix workflow to catch formatting and lint issues before code is pushed. They also add meaningful test coverage for changes — regression tests for bugs, edge case tests for new logic — without test theatre or trivial assertions.

## Boundaries

Do not search, read, or list files outside the project directory (`~/GitHub/zytunes`) without asking first. If you need a file from the user's home directory, `~/Music`, or any other personal path, describe what you need and ask the user to provide it or confirm the path. This includes `find`/`glob`/`ls` sweeps of `$HOME`.

## Code Health

Leave the codebase better than you found it. Prefer the maintainable solution over the quick fix — even if it takes longer. When you encounter something that needs attention while working on a task, handle it appropriately:

- **Small improvements** (dead imports, a clearer variable name, a missing error case) — fix them inline as part of the current work.
- **Larger issues** (deprecated dependencies, structural refactors, API redesigns) — don't let these block the current task. Create a plan, commit it to a dedicated branch, and note it for the user. These deserve their own focused effort, not a drive-by half-fix.

When choosing between approaches, bias toward the one that makes the next change easier, not just the one that closes the current task fastest. Avoid papering over problems with workarounds that will need to be undone later.

## Commit Convention

This project uses **Conventional Commits** to drive automatic releases. The release workflow (`.github/workflows/release.yml`) analyzes commit messages on `main` to determine version bumps. Always use the correct prefix:

| Prefix | Bump | Example |
|--------|------|---------|
| `feat:` | minor | `feat: add playlist sync support` |
| `fix:` | patch | `fix: handle empty album art gracefully` |
| `perf:` | patch | `perf: reduce memory usage during transcoding` |
| `feat!:` or `BREAKING CHANGE` | major | `feat!: redesign device session API` |
| `docs:`, `chore:`, `refactor:`, `ci:`, `test:`, `style:` | no release | `chore: update dependencies` |

Scopes are optional: `feat(tui): add theme picker` is fine. Commits that don't match a release prefix (`docs:`, `chore:`, `refactor:`, `ci:`, `test:`, `style:`) will not trigger a release.

## Known Limitations

- **cosmic-term + CJK** — CJK text misaligns panel borders in Pop!_OS's cosmic-term because the terminal renders wide glyphs as 1 cell instead of 2 (upstream bugs [pop-os/cosmic-term#325](https://github.com/pop-os/cosmic-term/issues/325) and [#369](https://github.com/pop-os/cosmic-term/issues/369)). Our `unicode-width` measurement in `src/tui/ui.rs` is correct per Unicode EAW. Do **not** work around this by halving CJK widths or sniffing `$TERM_PROGRAM` — it would break every compliant terminal. Fix belongs upstream; recommend users switch terminals (Alacritty, kitty, wezterm, Zed) for CJK libraries.
- **Linux transport lacks stall recovery** — `zune-mtp/src/transport/libusb.rs` does not yet call `clear_halt` on transient pipe errors the way the IOKit backend does. Transient stalls surface as fatal `rusb::Error`s and require a replug. Tracked in `todos.md`.

## iPod Classic Constraints

- Device must be formatted for Windows/FAT (macOS HFS+ iPods are not supported — no backend-side HFS driver)
- Firmware matches `.mp3`/`.m4a` case-sensitively in some code paths; all files written under `iPod_Control/Music/F00..F49/` use lowercased extensions
- New tracks get IDs from `FIRST_IPOD_ID = 52`, uniform mhit size, iTunes-matching mystery constants. Existing tracks round-trip losslessly via raw mhit blob replay (avoids losing unknown fields)
- `id_0x24` must come from `mhbd+0x24`, not `db_id` — getting this wrong produces a database the iPod silently discards
- `Play Counts` is deleted after import to match libgpod behaviour — otherwise stale entries accumulate
- See `DEBUG.md` for the full iPod bring-up investigation; port is from-scratch against libgpod, not a wrapping

## Zune 30 Constraints

- Only accepts MP3, WMA, AAC formats
- MTPZ keys must exist at `~/.mtpz-data`
- Device auto-opens MTP session on USB connect (OpenSession returns `0x201d` — this is normal)
- GetDeviceInfo is not supported by the Zune (returns `0x2006`) — skip it and go straight to OpenSession
- MTPZ handshake requires SessionInitiatorVersionInfo (`0xD406`) to be set before beginning authentication
- Data-out MTP operations require separate bulk writes for the container header and payload (Microsoft/Zune requirement)
- `SetObjectPropValue` commits (especially album art to flash) can take tens of seconds; `zune-mtp` uses a 45 s response timeout on this op. A single failing art payload can poison the MTP session, so `NativeSession` disables art for the remainder of the session after the first failure
- Pre-3.0 firmware rejects the `GetDeviceSyncProgress` vendor op; `save_sync_progress` must stay silent in that case rather than logging a user-visible warning every sync
- Fatal USB cascades (`0xe00002c0`, `0xe00002ed`, a failed `ClearPipeStall` retry, or a `ReadPipe` timeout) mean the session is dead until physical replug — the TUI detects this via `is_device_gone` in `tui/background.rs` and aborts remaining work
- Folder deletes do not cascade: `DeleteObject` on an `ASSOCIATION_FORMAT` handle leaves children orphaned on flash. `rm` must walk the hierarchy post-order (`delete_recursive`) AND invalidate the library cache for the deleted path (`invalidate_library_for_path`) — stale cached artist/album handles fed into `send_object_prop_list` halt the OUT pipe on v1.4 and cascade every queued track. See `findings.md` for the full incident writeup
