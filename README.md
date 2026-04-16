# zytunes

[![CI](https://github.com/Danondso/zytunes/actions/workflows/ci.yml/badge.svg)](https://github.com/Danondso/zytunes/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](https://www.rust-lang.org)

A Rust CLI tool for syncing music to a Microsoft Zune 30 from macOS and Linux.

## Status

**Active development.** Core functionality is working: device detection, MTPZ authentication, file listing, music push/remove, library sync (by artist, album, or track), and automatic transcoding of unsupported formats.

## What works

- **USB detection** — scans connected USB devices via `rusb` and identifies a Zune 30 by Microsoft vendor ID `0x045e` and known product IDs
- **MTPZ authentication** — the Zune requires Microsoft's encrypted MTPZ handshake before exposing storage. Handled automatically via the native IOKit backend (`zune-mtp`)
- **File listing** — `ls [path]` enumerates storage and prints the device's directory tree
- **Music push** — `push <files...>` uploads music files to the Zune with proper metadata via `zune-import`
- **Music removal** — `rm <device-paths...>` removes files/folders from the device (leaf-first for directories)
- **Music library sync** — `sync <type> <name>` syncs tracks by artist, album, or track name by scanning a local music folder. Detects duplicates already on device and skips them
- **Auto-transcoding** — non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are transcoded to MP3 via ffmpeg with album art resized to 200x200 (Zune 30 constraint)
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely
- **Library browsing** — `library [query]` browses/searches your music library
- **Directory scanning** — point zytunes at a music folder. It reads tags via lofty (FLAC, M4A, OGG, WAV, MP3, etc.) and infers metadata from the directory structure (`Artist/Album/Track.ext`) for untagged files. Set `ZYTUNES_MUSIC_DIR` or add `music_dir` to `~/.config/zytunes/config.toml`
- **Interactive TUI** — `zytunes-tui` launches a terminal UI (ratatui) for browsing your music library, connecting to the device, managing a sync queue, and monitoring sync progress. Library scanning runs in the background on startup. TUI displays sync status on the Zune ASCII art screen including loading spinner, track count, syncing spinner, and queue count
- **Device content browsing** — the TUI can browse tracks on the connected Zune organized by artist/album, toggled with `v`. The device library is indexed from the device's Music directory structure (`Artist/Album/Track`)
- **Device track removal** — in device view mode, `a`/`A` removes selected tracks, albums, or artists from the device. Progress is shown during removal and the device track list auto-refreshes afterward
- **Theming** — the TUI includes 16 built-in color themes (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original). Press `t` to open the theme picker. Selected theme is persisted to `~/.config/zytunes/config.toml`
- **Native IOKit USB backend** — the `zune-mtp` crate provides direct MTP/MTPZ communication via Apple's IOKit framework, bypassing libusb. This is the sole backend for all device operations, supporting listing, import, removal, and track collection

## Interactive TUI

<!-- TODO: add screenshot or GIF here -->

Launch the interactive terminal UI:

```
zytunes-tui
# or: cargo run --bin zytunes-tui
```

The TUI reads `music_dir` from `~/.config/zytunes/config.toml`, or falls back to the `ZYTUNES_MUSIC_DIR` env var.

### Layout

```
┌──────────────────┬──────────────────────────────────┬──────────────┐
│  Device          │  Sidebar  │  Albums  │  Tracks   │  Keys        │
│  (Zune art,      │  (artists │  (per    │  (table   │  (context-   │
│   storage info)  │   albums  │  artist) │   or album│   sensitive  │
│                  │   or      │          │   detail  │   reference) │
├──────────────────┤   lists)  │          │   view)   │              │
│  Sync Queue      │           │          │           │              │
│                  │           │          │           │              │
├──────────────────┤           │          │           │              │
│  Log             ├──────────────────────────────────┤              │
│                  │  Footer (track count + hints)    │              │
└──────────────────┴──────────────────────────────────┴──────────────┘
```

- **Left column**: Device info with Zune ASCII art and storage bar, sync queue, and log
- **Center**: 3-panel browser — sidebar, album list (when applicable), and track list
- **Right column**: Context-sensitive keybinding reference (toggle with `h`)

### Browse modes

- **Library mode** (default) — browse your music library by Artists (`1`) or Albums (`2`)
- **Device mode** — browse tracks on the connected Zune, organized by Artist/Album from the device filesystem. Toggle with `v`

The album detail view shows a ZIP disk ASCII art with album metadata (artist, album, year, track count, duration) alongside the track table.

### Keybindings

**Navigation**

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle between panels |
| `Up` / `Down` | Navigate lists |
| `Left` / `Right` | Skip to next letter group in sidebar |
| `Enter` | Select / expand |
| `1` / `2` | Switch to Artists / Albums |
| `4` | Jump to sync queue |

**Library & Browsing**

| Key | Action |
|-----|--------|
| `/` | Search sidebar (live filter, `Esc` to cancel) |
| `s` | Cycle sort column (track list) |
| `a` | Add selection to sync queue (library) / remove from device (device mode) |
| `A` | Add all visible tracks / remove all visible |
| `v` | Toggle Library / Device browse mode |

**Device**

| Key | Action |
|-----|--------|
| `c` | Connect to Zune (USB detect + MTPZ handshake) |
| `r` | Refresh device track list |
| `d` | Disconnect |

**Sync**

| Key | Action |
|-----|--------|
| `S` or `Enter` (in queue) | Start sync |
| `d` (in queue) | Remove selected item |
| `C` | Clear entire queue |
| `Esc` | Cancel running sync |

**General**

| Key | Action |
|-----|--------|
| `q` | Quit |
| `?` | Full help overlay |
| `h` | Toggle keybinding panel |
| `t` | Theme picker |

### Themes

16 built-in themes, selectable with `t`:

iTunes 2004, Gruvbox Dark, Gruvbox Light, Everforest Dark, Everforest Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original

Selected theme is persisted to `~/.config/zytunes/config.toml`.

### Terminal compatibility

The TUI works in any EAW-compliant terminal (Alacritty, kitty, wezterm, Zed's embedded terminal, iTerm2, Ghostty, etc.). One known exception:

- **cosmic-term** — CJK (Chinese / Japanese / Korean) text misaligns panel borders and zebra-row highlights. This is an upstream rendering bug in cosmic-term ([#325](https://github.com/pop-os/cosmic-term/issues/325), [#369](https://github.com/pop-os/cosmic-term/issues/369)): it advances the cursor by 1 cell for wide glyphs instead of the 2 cells required by Unicode East Asian Width. Our measurement code (via the `unicode-width` crate) is correct; use a different terminal for libraries with CJK metadata until cosmic-term is fixed upstream.

### Sync workflow

1. Browse your music library and press `a` to add artists, albums, or individual tracks to the sync queue
2. Press `c` to connect to the Zune (auto-detects via USB, performs MTPZ handshake)
3. Press `S` or switch to the queue and press `Enter` to start syncing
4. Non-native formats are auto-transcoded to MP3, album art resized to 200x200
5. Progress and results appear in the log panel; device track list auto-refreshes on completion

## Claude Code Skills

This project includes custom [Claude Code](https://claude.ai/code) skills in `.claude/skills/`:

| Skill | Command | What it does |
|-------|---------|--------------|
| **Review** | `/review` | Diffs the current branch against main, reviews for correctness, Rust idioms, cleanliness, readability, refactoring opportunities, and security. Posts findings as line-level comments on the PR, fixes them in priority order, and replies to each comment with the resolution commit. |
| **Fix CI** | `/fix-ci` | Finds the PR for the current branch, pulls failing CI check logs, correlates failures with local code, and produces a structured fix plan for approval before making changes. |

Both skills run `cargo fmt` and `cargo clippy -- -D warnings` as part of their fix workflow, and add meaningful test coverage (regression tests for bugs, edge cases for new logic) without test theatre.

## What's planned

- **Dump command** — `dump` to pull all music off a Zune to a local directory

## Setup

### Prerequisites

```
brew install libusb
```

### MTPZ keys

The Zune requires MTPZ authentication. Place the keys file in your home directory:

```
cp mtpz-data.example ~/.mtpz-data
```

These keys originate from the [libmtp-zune](https://github.com/kbhomes/libmtp-zune) project.

### Music library

Point zytunes at a folder of audio files:

1. **`ZYTUNES_MUSIC_DIR`** env var — highest priority
2. **`music_dir`** in `~/.config/zytunes/config.toml` — persistent fallback

```
export ZYTUNES_MUSIC_DIR="$HOME/Music"
```

The scanner reads tags via [lofty](https://crates.io/crates/lofty) for all common audio formats (MP3, FLAC, M4A, OGG, WAV, etc.) and falls back to the path structure (`Artist/Album/Track.ext`) when tags are missing.

### Install

```
./install.sh
```

This builds a release binary and installs both `zytunes` (CLI) and `zytunes-tui` (interactive TUI) to `/usr/local/bin/`. To uninstall:

```
./uninstall.sh
```

Or build and run directly without installing:

```
cargo build
cargo run
```

Connect your Zune 30 via USB, then run the tool.

### Debugging

USB-level debug logging can be enabled via environment variables:

```
IOKIT_USB_DEBUG=1 cargo run       # Trace native IOKit USB reads/writes
```

## Architecture

### USB architecture

The Zune 30 uses MTPZ — Microsoft's encrypted extension to MTP — which requires a cryptographic handshake before the device exposes any storage. This handshake involves data-out USB operations (host sending data to device) that are incompatible with libusb on macOS.

**The libusb/IOKit gap on macOS:** On macOS, USB communication can go through either:
- **libusb** (used by the `rusb` Rust crate) — a cross-platform userspace USB library
- **IOKit** — Apple's native USB framework

The Zune 30's MTPZ data-out operations work correctly through IOKit but fail through libusb. Specifically:
- Standard MTP operations (GetDeviceInfo, OpenSession, GetStorageIDs) work fine via libusb
- Data-out operations (SendWMDRMPDAppRequest for the MTPZ certificate exchange) consistently return `GeneralError (0x2002)` via libusb, regardless of payload content
- The same operations succeed immediately via IOKit

This appears to be a fundamental incompatibility between libusb's macOS backend and the Zune's USB implementation for data-out transfers.

**Our approach:** zytunes uses `rusb` for fast device detection (which only requires USB descriptor reads and works fine). For all MTP/MTPZ communication, zytunes uses the `zune-mtp` crate, which talks directly to IOKit via Rust FFI. There is no fallback — IOKit is the sole transport for device operations.

### Native IOKit backend (zune-mtp)

The `zune-mtp/` workspace crate provides a pure-Rust native IOKit MTP/MTPZ implementation:
- **transport.rs** — IOKit USB transport: device/interface discovery via `IOServiceMatching`, bulk pipe read/write via `IOUSBInterfaceInterface` vtables
- **container.rs** — MTP/PTP container format: command, data, and response container building and parsing
- **session.rs** — MTP session management: OpenSession, GetDeviceInfo, GetStorageIDs, GetStorageInfo, GetObjectHandles, GetObjectInfo, SendObjectInfo/SendObject, DeleteObject, SetDevicePropValue, GetObjectPropsSupported, GetObjectPropList, SetObjectPropValue, SendObjectPropList, GetObjectReferences, SetObjectReferences
- **mtpz.rs** — Full MTPZ handshake: SessionInitiatorVersionInfo setup, RSA-1024 signature, AES-128-CBC decryption, CMAC verification, certificate exchange
- **proplist.rs** — MTP ObjectPropList builder for SendObjectPropList: constructs binary property list payloads with string, u16, and u32 property types
- **iokit_ffi.rs** — Raw FFI declarations for IOKit/CoreFoundation (IOUSBDeviceInterface, IOUSBInterfaceInterface vtables)

The `NativeSession` in `src/mtp/native.rs` wraps `zune-mtp` and implements the `DeviceSession` trait, providing ls, import, rm, and track collection. Track cache is stored at `~/.zytunes-track-cache-{serial}`.

## What's known

| Topic | Details |
|---|---|
| USB Vendor ID | `0x045e` (Microsoft) |
| USB Product ID (media mode) | `0x0710` |
| USB Product ID (firmware update) | `0x0711` |
| USB Product ID (MTP alternate) | `0x0712` |
| Transfer protocol | MTPZ (MTP with encrypted Zune extensions) |
| Authentication | RSA-1024 certificate exchange + AES-128-CBC + CMAC |
| macOS USB transport | IOKit required for MTPZ data-out; libusb fails |
| Device storage layout | Root dirs: Albums, Music, Podcasts, Series |
| Music organization | Music/{Artist}/{Album}/{Track} hierarchy |
| MTP version | 1.0 with 85 supported operations |

## Zune 30 MTP error codes

Error codes encountered during development and what they mean in the Zune context:

| Code | Name | Meaning on Zune 30 |
|---|---|---|
| `0x2001` | OK | Success |
| `0x2002` | GeneralError | Operation rejected. On macOS with libusb: returned for ALL data-out MTP operations (the IOKit gap). Also returned when trying to `rm` a non-empty folder. |
| `0x2006` | ParameterNotSupported | Returned by GetDeviceInfo (Zune does not support this operation) |
| `0x2008` | InvalidStorageID | Storage ID doesn't exist (storages are hidden until MTPZ auth completes) |
| `0x200f` | SessionNotOpen | Returned by EnableTrustedFilesOperations when MTPZ confirmation step was not accepted by the device |
| `0x2013` | StoreNotAvailable | Returned for GetObjectHandles on all-storages before MTPZ authentication |
| `0x2016` | InvalidCodeFormat | File format not supported by device. The Zune 30 rejects FLAC, OGG, WAV, OPUS. Only MP3, WMA, and AAC are accepted. |
| `0xa803` | InvalidObjectPropValue | A metadata property value was rejected. Triggered by embedded album art larger than ~200x200px. Resizing art to 200x200 JPEG before import fixes this. |
| `0x201d` | InvalidParameter | Returned by OpenSession when a session is already open. The Zune auto-opens a session on USB connect, so this is normal. |

## What's unknown

- **Firmware-dependent behavior** — product IDs and MTP behavior may vary across firmware versions
- **Ratings / play counts** — how ratings and play counts are represented on-device

## Project structure

```
zune-mtp/            — native IOKit MTP/MTPZ library (workspace crate)
  src/
    lib.rs           — crate root, public API
    transport.rs     — IOKit USB transport (device open, bulk read/write)
    container.rs     — MTP/PTP container format (build/parse commands, data, responses)
    session.rs       — MTP session (open, object operations, property lists, object references)
    mtpz.rs          — MTPZ authentication (RSA, AES-CBC, CMAC, certificate exchange)
    proplist.rs      — MTP ObjectPropList builder (SendObjectPropList payloads)
    iokit_ffi.rs     — raw FFI declarations for IOKit and CoreFoundation
  examples/
    test_connect.rs  — end-to-end connection test
    test_cmac.rs     — AES-CMAC verification against RFC 4493
    test_sign.rs     — CMAC signature verification against aft-mtp-cli trace output
    test_rsa.rs      — RSA key roundtrip verification
    test_ls.rs       — device directory listing test
    test_tracks.rs   — full Music tree walk and track enumeration
    test_import.rs   — file upload via SendObjectPropList
src/
  main.rs            — CLI entry point, commands (ls, push, rm, sync, library, photo-sync, video-sync)
  lib.rs             — public API, sync engine, SyncType enum, transcoding
  device.rs          — USB scanning and Zune identification (rusb)
  mtp/
    mod.rs           — module root, DeviceSession trait
    native.rs        — native IOKit backend (NativeSession implementing DeviceSession)
    parse.rs         — DeviceEntry struct and parsing utilities
  library.rs         — MusicLibrary trait and shared Track type
  dirlib.rs          — DirectoryLibrary: recursive folder scanner (lofty tags + path fallback)
  cache.rs           — on-disk cache for the directory scanner
  tui/
    main.rs          — TUI entry point (zytunes-tui binary)
    app.rs           — application state, panel navigation, event handling
    ui.rs            — ratatui widget rendering (layout, panels, overlays)
    background.rs    — background worker thread (device I/O, sync, removal, library loading)
    anim.rs          — theme-aware animations
    audio.rs         — local audio playback
    theme.rs         — theme struct and built-in theme presets
    config.rs        — TOML config file loading/saving (~/.config/zytunes/config.toml)
```

## Reference libraries

| Library | Role | Link |
|---|---|---|
| **libmtp-zune** | Protocol documentation. The `mtpz.md` file has the most detailed public description of the MTPZ handshake. Source of the `.mtpz-data` keys. | [kbhomes/libmtp-zune](https://github.com/kbhomes/libmtp-zune) |

### Other references

- [libmtp](https://github.com/libmtp/libmtp) — open-source MTP library with some Zune-specific code paths
- [OpenZDK](https://github.com/ZuneRedux/openZDK-quick-start-kit) — community effort to reverse-engineer Zune internals (Zune HD focused)
- [USB MTP spec](https://www.usb.org/document-library/media-transfer-protocol-v11-spec-and-mtp-v11-adopters-agreement) — the official MTP 1.1 specification

## License

MIT — see [LICENSE](LICENSE). Third-party notices in [THIRD_PARTY.md](THIRD_PARTY.md).
