# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Rust (zytunes workspace)
cargo build                   # Debug build (all workspace crates)
cargo build --release         # Release build
cargo check                   # Fast type-check without building
cargo run -- <command>        # Run CLI with arguments (ls, push, rm, sync, library, help)
cargo run --bin zytunes-tui   # Run interactive TUI
cargo test                    # Run tests (all workspace crates)
cargo test -p zune-mtp        # Run zune-mtp crate tests only
cargo fmt                     # Format code
cargo clippy                  # Lint
./install.sh                  # Build release + install both zytunes and zytunes-tui to /usr/local/bin
./uninstall.sh                # Remove both zytunes and zytunes-tui from /usr/local/bin

```

## What This Is

A Rust CLI tool for syncing music to a Microsoft Zune 30 from macOS and Linux. Detects the Zune over USB, authenticates via MTPZ (encrypted MTP), and manages music files on the device.

CLI commands: `ls [path]`, `push <files...>`, `rm <paths...>`, `sync <type> <name>`, `library [query]`, `help`.

**TUI** (`zytunes-tui`): Interactive terminal UI for browsing the music library (directory scan) and device content, connecting to the device, managing a sync queue, removing tracks from the device, and monitoring sync progress. Library scanning runs asynchronously on a background thread at startup. The TUI supports two browse modes: Library and Device (Zune), toggled with `v`. Theme picker accessible with `t`.

## Architecture

**Workspace layout:** The project is a Cargo workspace with two members: the root `zytunes` crate and the `zune-mtp` library crate. Both crates specify `rust-version = "1.87"`. A `tools/mtp-probe` directory (gitignored) contains the MTP vendor operation probe tool used for reverse engineering.

**USB strategy:** `rusb` handles device detection (descriptor reads only). For MTP/MTPZ communication, the `zune-mtp` crate provides direct IOKit FFI, bypassing libusb (which fails on data-out USB operations that the Zune's MTPZ handshake requires).

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
- `device.rs` — USB scanning via rusb, identifies Zune by VID `0x045e`
- `lib.rs` — Crate root, exports `SyncType` enum (`Artist`, `Album`, `Track`) with `FromStr`/`Display` impls
- `mtp/mod.rs` — `DeviceSession` trait abstracting device operations (ls, zune_import, rm, collect_all_tracks, save_sync_progress, `prewarm_library`, `refresh_storage_cache`, import_photo) for testability. `prewarm_library` is called once at connect so the first import pays no artist/album mapping setup cost; `refresh_storage_cache` is called after sync/remove to bump the cached `#free_bytes:` header and avoid invalidating track/library caches on next reconnect. All trait methods have doc comments
- `mtp/native.rs` — `NativeSession` implements `DeviceSession` using `zune-mtp`: opens IOKit USB transport, performs MTPZ handshake, provides ls/import/rm/collect_all_tracks. Resolves device paths by walking object handles. `TrackCache` struct handles per-device track caching to `~/.zytunes-track-cache-{serial}` with an `update_free_bytes()` fast path that rewrites only the header line (preserves track entries without re-serialising). `DeviceLibrary` struct caches artist/album MTP handles to `~/.zytunes-library-cache-{serial}` for fast reconnects. Sync progress cached to `~/.zytunes-sync-progress-{serial}`. `save_sync_progress` routes through the firmware-guarded `get_sync_progress` so pre-3.0 Zunes that reject the vendor op stay silent instead of emitting a user-visible warning every sync. Tracks an `art_disabled` session flag: after a single `SetObjectPropValue` failure on album art (one bad JPEG can wedge the MTP session), subsequent tracks skip art entirely for the remainder of the session to avoid cascading 45 s read timeouts. `extract_album_art` normalises output through a defensive path — explicit RGB8, `JpegEncoder` at quality 85, no ICC/EXIF carry-over, SOI/EOI validation — to reduce the probability of hitting the Zune's JPEG decoder bug. Setting `ZYTUNES_DUMP_ART=1` writes the last art payload to `/tmp/zytunes-last-art.jpg` for inspection. `MtpResultExt` trait converts `MtpError` to `String` at the `DeviceSession` boundary
- `mtp/parse.rs` — `DeviceEntry` struct and parsing utilities
- `mtp/zmdb.rs` — ZMDB (Zune Metadata Database) parser. Parses the proprietary binary format returned by vendor op `0x9217` into tracks, albums, artists, genres. Used as the fast path in `collect_all_tracks` (single MTP call instead of hundreds)
- `library.rs` — `MusicLibrary` trait abstracting library queries (artists, albums, track lookups) and the shared `Track` type
- `dirlib.rs` — `DirectoryLibrary` implements `MusicLibrary` by scanning a folder recursively for audio files. Reads metadata from all audio formats via lofty (FLAC, M4A, OGG, WAV, MP3, etc.), falls back to `Artist/Album/Track.ext` path structure for untagged files. Generates stable track IDs via path hashing
- `cache.rs` — on-disk metadata cache for `DirectoryLibrary`, keyed by a hash of the scan-root path. Deliberately lives under `$HOME/.cache/zytunes` and does NOT honor `ZYTUNES_CACHE_DIR` so sibling worktrees pointed at the same `~/Music` share one cached scan
- `paths.rs` — resolves device-scoped cache locations. `ZYTUNES_CACHE_DIR` (when set) redirects device caches (track list, library handles, sync progress) into one directory for per-worktree isolation; the workspace `.cargo/config.toml` points it at `./.zytunes-cache`. The dirlib cache explicitly opts out of this override — see `cache.rs`
- `main.rs` — CLI entry point, `run()` dispatcher, `cmd_sync`/`cmd_push`/`cmd_rm`/`cmd_ls`/`cmd_library` commands, `sync_to_device()` engine, `find_matching_tracks()` (takes `SyncType`), transcoding via symphonia + LAME
- `tui/main.rs` — TUI entry point (`zytunes-tui` binary), event loop (50ms poll), terminal setup/teardown
- `tui/app.rs` — TUI application state (`App`), input handling, panel navigation. Panels: `Library` (sidebar), `Albums`, `TrackList`, `Device`, `SyncQueue` — cycled via Tab. Two browse modes: `BrowseMode::Library` and `BrowseMode::Device` (toggled with `v`). Two sidebar modes: `Artists`, `Albums` (keys `1`/`2`). Sidebar selection positions saved per browse-mode × sidebar-mode pair. Device mode builds in-memory artist/album/track index from `Music/{Artist}/{Album}/{Track}` paths via `DeviceState::add_indexed_track` / `remove_indexed_track` — incremental binary-search inserts and artist-scoped lookup-set rebuilds, so sync deltas are O(log N) per track instead of O(N²) full resorts. `build_device_index` is retained for initial load and drains `tracks` into the incremental path. `flush_device_index` now only runs UI re-derivations (`rebuild_artist_device_status`, sidebar/track retag) since the index itself is kept live. `execute_sync` filters queued items against `device.track_set` before dispatching so already-on-device tracks are skipped (with a log entry), and shows a toast if the queue was entirely duplicates. `App` uses `DeviceState` sub-struct (holds device status, name, firmware, serial, storage, tracks, etc.) and `SyncState` sub-struct (holds sync queue, status, current track, and log). State machines: `DeviceStatus` (Disconnected → Detecting → Connecting → Connected), `SyncStatus` (Idle → Running)
- `tui/background.rs` — background worker thread communicating via `mpsc` channels. Commands (`BgCommand`): `LoadLibrary`, `Connect`, `LoadDeviceTracks`, `Disconnect`, `ExecuteSyncQueue`, `RemoveFromDevice`, `CancelSync`. Events (`BgEvent`): `LibraryLoaded`, `DeviceDetected`, `SessionReady`, `SessionFailed`, `DeviceTracksLoaded`, `SyncProgress`, `SyncTrackDone`, `SyncComplete`, `RemoveProgress`, `RemoveComplete`, `StorageUpdated`, `Error`, `SyncMessage`. On `Connect`, calls `prewarm_library` so the first sync doesn't pause for library scan. After successful sync/remove, calls `refresh_storage_cache(free_bytes)` so the next reconnect sees a small free-space delta. Uses an `is_device_gone()` predicate to detect fatal USB failure strings (`0xe00002c0`, `0xe00002ed`, `retry after ClearPipeStall`, `ReadPipe timed out`); when it trips mid-sync or mid-remove, the worker aborts the remaining queue, skips storage refresh, drops the session, and emits `SessionFailed("Device disconnected from USB")` so the TUI flips back to Disconnected. `DeviceInfo` struct does not include `mtp_version`. Uses NativeSession exclusively
- `tui/anim.rs` — animation data for connection and sync progress
- `tui/audio.rs` — local audio playback via rodio, play/pause/skip controls
- `tui/ui.rs` — ratatui rendering. Layout: left column (device panel with Zune ASCII art + storage bar, sync queue, log), center (sidebar + albums + track list + footer), right (toggleable keys reference via `h`). Album detail view shows ZIP disk ASCII art with metadata alongside track table. Track table supports sort cycling (`s`). Overlays: help (`?`), search (`/`, live-filters sidebar), theme picker (`t`). Toast notifications (auto-dismiss 5s, green/red borders). Footer shows context-sensitive track count and action hints, adapts to Library vs Device mode
- `tui/theme.rs` — `Theme` struct with color/style fields (sidebar, selection, main/alt row, border, footer, header, dim, error, success, progress). 16 built-in presets: iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original. `find_theme_index()` for name-based lookup
- `tui/config.rs` — TOML config file at `~/.config/zytunes/config.toml` (serde + toml). Stores selected theme name and optional `music_dir` for directory scanning backend

**Configuration:** Music library is resolved by `load_library()`: (1) `ZYTUNES_MUSIC_DIR` env var, (2) `music_dir` from config.toml. Both point at a folder that is recursively scanned for audio files. TUI config (theme, `music_dir`) is stored in `~/.config/zytunes/config.toml`.

**Cache locations:** Device-scoped caches (`~/.zytunes-track-cache-{serial}`, `~/.zytunes-library-cache-{serial}`, `~/.zytunes-sync-progress-{serial}`) are redirected by `ZYTUNES_CACHE_DIR` — set automatically per-worktree via `.cargo/config.toml` to `./.zytunes-cache`. The dirlib metadata cache at `$HOME/.cache/zytunes` intentionally ignores this override so worktrees that share a `~/Music` root reuse one lofty scan.

**Licensing:** MIT license (`LICENSE`). Third-party attribution in `THIRD_PARTY.md` (MTPZ keys from libmtp-zune).

**External tool dependencies:** `libusb` (via rusb). Optional: `ffmpeg` for TUI playback of WMA files.

**Transcoding:** Non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are automatically transcoded to MP3 using pure Rust libraries (symphonia for decoding, mp3lame-encoder for encoding, lofty for metadata). Native formats (MP3, WMA, AAC) skip transcoding entirely. Album art is resized to 200x200 JPEG via the image crate (Zune 30 rejects larger art with error `0xa803`).

## Claude Code Skills

Custom skills in `.claude/skills/`:

- `/review` — local code review of the current branch diff. Reviews for correctness, Rust best practices, cleanliness, readability, refactoring opportunities, and security. Posts findings as line-level PR comments via `gh api`, fixes issues in priority order, then replies to each comment with the resolution. Falls back to terminal output if no PR exists.
- `/fix-ci` — diagnoses failing CI checks on the current branch's PR. Fetches check statuses and failure logs via `gh`, correlates with local source, then enters plan mode with a structured fix plan for approval.

Both skills run `cargo fmt` and `cargo clippy -- -D warnings` as part of their fix workflow to catch formatting and lint issues before code is pushed. They also add meaningful test coverage for changes — regression tests for bugs, edge case tests for new logic — without test theatre or trivial assertions.

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
