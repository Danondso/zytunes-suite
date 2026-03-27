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

# C++ (aft-mtp-cli, vendored in aft/)
cd aft && mkdir -p build && cd build
cmake .. -DBUILD_QT_UI=OFF -DBUILD_MTPZ=ON -DBUILD_FUSE=OFF
make aft-mtp-cli              # Binary output: aft/build/cli/aft-mtp-cli
```

## What This Is

A Rust CLI tool for syncing music to a Microsoft Zune 30 from macOS. Detects the Zune over USB, authenticates via MTPZ (encrypted MTP), and manages music files on the device.

CLI commands: `ls [path]`, `push <files...>`, `rm <paths...>`, `sync <type> <name>`, `library [xml] [query]`, `help`.

**TUI** (`zytunes-tui`): Interactive terminal UI for browsing the iTunes library and device content, connecting to the device, managing a sync queue, removing tracks from the device, and monitoring sync progress. Library parsing runs asynchronously on a background thread at startup. The TUI supports two browse modes: Library (iTunes) and Device (Zune), toggled with `v`. Theme picker accessible with `t`.

## Architecture

**Workspace layout:** The project is a Cargo workspace with two members: the root `zytunes` crate and the `zune-mtp` library crate.

**Two-layer USB strategy with native backend:** `rusb` handles device detection (descriptor reads only). For MTP/MTPZ communication, the TUI tries the native `zune-mtp` backend first (direct IOKit FFI) and falls back to `aft-mtp-cli` (a subprocess) if it fails. This split exists because libusb's macOS backend fails on data-out USB operations that the Zune's MTPZ handshake requires — IOKit works.

**Native IOKit backend (`zune-mtp/`):** A workspace crate providing direct MTP/MTPZ communication via Apple's IOKit framework:
- `transport.rs` — IOKit USB transport: device discovery via `IOServiceMatching` with vendor/product ID filtering, interface claiming, bulk pipe endpoint discovery, read/write operations via `IOUSBInterfaceInterface` COM-like vtables. Debug logging via `IOKIT_USB_DEBUG` env var
- `container.rs` — MTP/PTP container format: building command/data containers, parsing response headers, operation and response code enums. Operation codes: `EnableTrustedFilesOperations` = `0x9214`, `DisableTrustedFilesOperations` = `0x9215`
- `session.rs` — MTP session management: OpenSession, GetDeviceInfo, GetStorageIDs, GetStorageInfo, GetObjectHandles, GetObjectInfo, SendObjectInfo/SendObject, DeleteObject, SetDevicePropValue (MTP string properties), GetObjectPropsSupported, GetObjectPropList, SetObjectPropValue, SendObjectPropList, GetObjectReferences, SetObjectReferences. Data-out operations send header and payload as separate bulk writes (Microsoft/Zune requirement). Includes MTP string and ObjectInfo dataset parsing
- `mtpz.rs` — Full MTPZ authentication: sets SessionInitiatorVersionInfo (`0xD406`) before handshake, RSA-1024 raw signing, PSS-like certificate message generation, AES-128-CBC decryption, CMAC key extraction from device response, session confirmation with data-only fallback, EnableTrustedFilesOperations
- `proplist.rs` — MTP ObjectPropList builder for SendObjectPropList (0x9808): constructs binary property list payloads with string, u16, and u32 property types. Property constants for object filename, name, artist, track, genre, artist ID, date authored, and representative sample data
- `iokit_ffi.rs` — Raw FFI: IOUSBDeviceInterface and IOUSBInterfaceInterface vtable structs, CoreFoundation helpers (CFString, CFUUID, CFNumber), IOKit service matching functions

**Vendored aft-mtp-cli fork:** `aft/` contains a fork of [android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux) (LGPL-2.1) with library caching. The upstream CLI has no caching — every `zune-init` loads the entire artist/album library from the device over USB 1.1 (~5 min). The fork adds:
- `Library::SaveCache()` / `TryLoadFromCache()` — serializes the artist/album maps to `~/.aft-library-cache` (text format, keyed by device serial)
- Auto-save on mutations (CreateArtist, CreateAlbum, AddTrack)
- `zune-refresh` CLI command to clear cache and reload from device
- Debug USB tracing via `AFT_USB_DEBUG` env var (logs read/write hex to stderr)
- Modified files: `aft/mtp/metadata/Library.h`, `aft/mtp/metadata/Library.cpp`, `aft/cli/Session.h`, `aft/cli/Session.cpp`, `aft/mtp/backend/darwin/usb/Device.cpp`, `aft/mtp/mtpz/TrustedApp.cpp`

**Key modules:**
- `device.rs` — USB scanning via rusb, identifies Zune by VID `0x045e`
- `mtp/mod.rs` — `DeviceSession` trait abstracting device operations (ls, zune_import, rm, collect_all_tracks, create_playlist) for testability
- `mtp/aft.rs` — `AftSession` implements `DeviceSession`: spawns and communicates with aft-mtp-cli subprocess via channel-based I/O (stdout and stderr each read by dedicated threads, lines delivered via `mpsc` channels). Monitors stderr for error lines that aft-mtp-cli emits without a `:done` marker on stdout. Uses `aft_quote()` to sanitize all user-controlled strings sent to the subprocess. `collect_all_tracks` returns an empty list for non-existent paths (e.g., fresh devices). Binary discovery: `AFT_MTP_CLI` env var → `aft/build/cli/aft-mtp-cli` → PATH → `/tmp/aft/build/cli/aft-mtp-cli`
- `mtp/native.rs` — `NativeSession` implements `DeviceSession` using `zune-mtp`: opens IOKit USB transport, performs MTPZ handshake, provides ls/import/rm/collect_all_tracks. Playlist creation not yet supported. Resolves device paths by walking object handles
- `mtp/parse.rs` — Parses aft-mtp-cli text output into `DeviceEntry` structs
- `mtp_native/` — Earlier pure-Rust MTP/MTPZ implementation built on rusb (works except data-out on macOS). Preserved for reference, superseded by `zune-mtp`
- `library.rs` — iTunes Library.xml plist parser. Fully integrated — used by `sync` and `library` commands
- `main.rs` — CLI entry point, `run()` dispatcher, `cmd_sync`/`cmd_push`/`cmd_rm`/`cmd_ls`/`cmd_library` commands, `sync_to_device()` engine, transcoding via ffmpeg
- `tui/main.rs` — TUI entry point (`zytunes-tui` binary), event loop (50ms poll), terminal setup/teardown
- `tui/app.rs` — TUI application state (`App`), input handling, panel navigation. Panels: `Library` (sidebar), `Albums`, `TrackList`, `Device`, `SyncQueue` — cycled via Tab. Two browse modes: `BrowseMode::Library` and `BrowseMode::Device` (toggled with `v`). Three sidebar modes: `Artists`, `Albums`, `Playlists` (keys `1`/`2`/`3`). Sidebar selection positions saved per browse-mode × sidebar-mode pair. Device mode builds in-memory artist/album/track index from `Music/{Artist}/{Album}/{Track}` paths. State machines: `DeviceStatus` (Disconnected → Detecting → Connecting → Connected), `SyncStatus` (Idle → Running → Complete)
- `tui/background.rs` — background worker thread communicating via `mpsc` channels. Commands (`BgCommand`): `LoadLibrary`, `Connect`, `LoadDeviceTracks`, `Disconnect`, `ExecuteSyncQueue`, `RemoveFromDevice`, `CancelSync`. Events (`BgEvent`): `LibraryLoaded`, `DeviceDetected`, `SessionReady`, `SessionFailed`, `DeviceTracksLoaded`, `SyncProgress`, `SyncTrackDone`, `SyncComplete`, `RemoveProgress`, `RemoveComplete`, `Error`, `SyncMessage`. Connection tries NativeSession first, falls back to AftSession
- `tui/ui.rs` — ratatui rendering. Layout: left column (device panel with Zune ASCII art + storage bar, sync queue, log), center (sidebar + albums + track list + footer), right (toggleable keys reference via `h`). Album detail view shows ZIP disk ASCII art with metadata alongside track table. Track table supports sort cycling (`s`). Overlays: help (`?`), search (`/`, live-filters sidebar), theme picker (`t`). Toast notifications (auto-dismiss 5s, green/red borders). Footer shows context-sensitive track count and action hints, adapts to Library vs Device mode
- `tui/theme.rs` — `Theme` struct with color/style fields (sidebar, selection, main/alt row, border, footer, header, dim, error, success, progress). 11 built-in presets: iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Miami Nights, IBM Mainframe, Windows 95, System 7, BIOS, Red Sands. `find_theme_index()` for name-based lookup
- `tui/config.rs` — TOML config file at `~/.config/zytunes/config.toml` (serde + toml). Currently stores selected theme name

**Configuration:** iTunes library path is resolved by `library_xml_path()`: checks `ZYTUNES_LIBRARY` env var first, then falls back to `$HOME/Music/Music/Library.xml`. Can also be overridden per-command with `--library <path>`. TUI config (theme selection) is stored in `~/.config/zytunes/config.toml`.

**Licensing:** MIT license (`LICENSE`). Third-party attribution in `THIRD_PARTY.md` (vendored aft is LGPL-2.1, MTPZ keys from libmtp-zune).

**External tool dependencies:** `ffmpeg`/`ffprobe` (transcoding), `libusb` (via rusb). `aft-mtp-cli` is vendored in `aft/` (used as fallback when native IOKit backend fails).

**Transcoding:** Non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are automatically transcoded to MP3 via ffmpeg. Native formats (MP3, WMA, AAC) skip transcoding entirely. Album art is resized to 200x200 JPEG (Zune 30 rejects larger art with error `0xa803`).

## Zune 30 Constraints

- Only accepts MP3, WMA, AAC formats
- MTPZ keys must exist at `~/.mtpz-data`
- Device auto-opens MTP session on USB connect (OpenSession returns `0x201d` — this is normal)
- GetDeviceInfo is not supported by the Zune (returns `0x2006`) — skip it and go straight to OpenSession
- MTPZ handshake requires SessionInitiatorVersionInfo (`0xD406`) to be set before beginning authentication
- Data-out MTP operations require separate bulk writes for the container header and payload (Microsoft/Zune requirement)
- Special characters in device paths: mitigated by `aft_quote()` which escapes `"`, strips newlines, and wraps in double quotes before sending to aft-mtp-cli
