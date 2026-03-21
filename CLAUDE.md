# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Rust (zytunes)
cargo build                   # Debug build
cargo build --release         # Release build
cargo check                   # Fast type-check without building
cargo run -- <command>        # Run CLI with arguments (ls, push, rm, sync, library, help)
cargo run --bin zytunes-tui   # Run interactive TUI
cargo test                    # Run tests (42 unit + integration tests)
cargo fmt                     # Format code
cargo clippy                  # Lint
./install.sh                  # Build release + install to /usr/local/bin/zytunes
./uninstall.sh                # Remove from /usr/local/bin

# C++ (aft-mtp-cli, vendored in aft/)
cd aft && mkdir -p build && cd build
cmake .. -DBUILD_QT_UI=OFF -DBUILD_MTPZ=ON -DBUILD_FUSE=OFF
make aft-mtp-cli              # Binary output: aft/build/cli/aft-mtp-cli
```

## What This Is

A Rust CLI tool for syncing music to a Microsoft Zune 30 from macOS. Detects the Zune over USB, authenticates via MTPZ (encrypted MTP), and manages music files on the device.

CLI commands: `ls [path]`, `push <files...>`, `rm <paths...>`, `sync <type> <name>`, `library [xml] [query]`, `help`.

**TUI** (`zytunes-tui`): Interactive terminal UI for browsing the iTunes library, connecting to the device, managing a sync queue, and monitoring sync progress. Library parsing runs asynchronously on a background thread at startup.

## Architecture

**Two-layer USB strategy:** `rusb` handles device detection (descriptor reads only), while `aft-mtp-cli` (a subprocess) handles all MTP/MTPZ communication. This split exists because libusb's macOS backend fails on data-out USB operations that the Zune's MTPZ handshake requires — IOKit (used by aft-mtp-cli) works.

**Vendored aft-mtp-cli fork:** `aft/` contains a fork of [android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux) (LGPL-2.1) with library caching. The upstream CLI has no caching — every `zune-init` loads the entire artist/album library from the device over USB 1.1 (~5 min). The fork adds:
- `Library::SaveCache()` / `TryLoadFromCache()` — serializes the artist/album maps to `~/.aft-library-cache` (text format, keyed by device serial)
- Auto-save on mutations (CreateArtist, CreateAlbum, AddTrack)
- `zune-refresh` CLI command to clear cache and reload from device
- Modified files: `aft/mtp/metadata/Library.h`, `aft/mtp/metadata/Library.cpp`, `aft/cli/Session.h`, `aft/cli/Session.cpp`

**Key modules:**
- `device.rs` — USB scanning via rusb, identifies Zune by VID `0x045e`
- `mtp/mod.rs` — `DeviceSession` trait abstracting device operations (ls, zune_import, rm, collect_all_tracks, create_playlist) for testability
- `mtp/aft.rs` — `AftSession` implements `DeviceSession`: spawns and communicates with aft-mtp-cli subprocess. Uses `aft_quote()` to sanitize all user-controlled strings sent to the subprocess. Binary discovery: `AFT_MTP_CLI` env var → `aft/build/cli/aft-mtp-cli` → PATH → `/tmp/aft/build/cli/aft-mtp-cli`
- `mtp/parse.rs` — Parses aft-mtp-cli text output into `DeviceEntry` structs
- `mtp_native/` — Preserved pure-Rust MTP/MTPZ implementation (works except data-out on macOS). Not actively used but kept for a future IOKit backend
- `library.rs` — iTunes Library.xml plist parser. Fully integrated — used by `sync` and `library` commands
- `main.rs` — CLI entry point, `run()` dispatcher, `cmd_sync`/`cmd_push`/`cmd_rm`/`cmd_ls`/`cmd_library` commands, `sync_to_device()` engine, transcoding via ffmpeg
- `tui/main.rs` — TUI entry point (`zytunes-tui` binary), event loop, terminal setup/teardown
- `tui/app.rs` — TUI application state (`App`), panel navigation, background event handling
- `tui/background.rs` — background worker thread: device detection, MTP session, sync execution, library loading
- `tui/ui.rs` — ratatui rendering: layout (3-column with device left panel), startup screen, Zune ASCII art, panels, overlays
- `tui/theme.rs` — color and style constants

**Configuration:** iTunes library path defaults to `~/Music/Music/Library.xml`. Override with `ZYTUNES_LIBRARY` env var or `--library <path>` flag.

**External tool dependencies:** `ffmpeg`/`ffprobe` (transcoding), `libusb` (via rusb). `aft-mtp-cli` is vendored in `aft/`.

**Transcoding:** Non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are automatically transcoded to MP3 via ffmpeg. Album art is resized to 200x200 JPEG (Zune 30 rejects larger art with error `0xa803`).

## Zune 30 Constraints

- Only accepts MP3, WMA, AAC formats
- MTPZ keys must exist at `~/.mtpz-data`
- Device auto-opens MTP session on USB connect (OpenSession returns `0x201d` — this is normal)
- Special characters in device paths: mitigated by `aft_quote()` which escapes `"`, strips newlines, and wraps in double quotes before sending to aft-mtp-cli
