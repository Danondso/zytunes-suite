# zytunes

A Rust CLI tool for syncing music to a Microsoft Zune 30 from macOS.

## Status

**Active development.** Core functionality is working: device detection, MTPZ authentication, file listing, music push/remove, iTunes library sync (by artist, album, playlist, or track), and automatic transcoding of unsupported formats.

## What works

- **USB detection** — scans connected USB devices via `rusb` and identifies a Zune 30 by Microsoft vendor ID `0x045e` and known product IDs
- **MTPZ authentication** — the Zune requires Microsoft's encrypted MTPZ handshake before exposing storage. Handled automatically via `aft-mtp-cli`
- **File listing** — `ls [path]` enumerates storage and prints the device's directory tree
- **Music push** — `push <files...>` uploads music files to the Zune with proper metadata via `zune-import`
- **Music removal** — `rm <device-paths...>` removes files/folders from the device (leaf-first for directories)
- **iTunes library sync** — `sync <type> <name>` syncs tracks from an iTunes Library.xml by artist, album, playlist, or track name. Detects duplicates already on device and skips them
- **Playlist creation** — `sync playlist <name>` imports tracks and creates the playlist on the device
- **Auto-transcoding** — non-native formats (FLAC, OGG, WAV, M4A, OPUS, ALAC, AIFF) are transcoded to MP3 via ffmpeg with album art resized to 200x200 (Zune 30 constraint)
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely
- **Library browsing** — `library [xml] [query]` browses/searches an iTunes Library.xml
- **Library caching** — vendored `aft-mtp-cli` fork caches the device's artist/album library to `~/.aft-library-cache`, eliminating the ~5 min load time on subsequent sessions. Cache auto-updates on imports; use `zune-refresh` to force a full reload
- **Interactive TUI** — `zytunes-tui` launches a terminal UI (ratatui) for browsing your iTunes library, connecting to the device, managing a sync queue, and monitoring sync progress. Library parsing runs in the background on startup. TUI displays sync status on the Zune ASCII art screen including loading spinner, track count, syncing spinner, and queue count
- **Device content browsing** — the TUI can browse tracks on the connected Zune organized by artist/album, toggled with `v`. The device library is indexed from the device's Music directory structure (`Artist/Album/Track`)
- **Device track removal** — in device view mode, `a`/`A` removes selected tracks, albums, or artists from the device. Progress is shown during removal and the device track list auto-refreshes afterward

## What's planned

- **Dump command** — `dump` to pull all music off a Zune to a local directory

## Setup

### Prerequisites

```
brew install libusb
```

### Build aft-mtp-cli

zytunes includes a vendored fork of [android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux) in `aft/` with library caching support (see [Architecture](#architecture) for why). Build it from source:

```
brew install cmake openssl taglib
cd aft && mkdir build && cd build
cmake .. -DBUILD_QT_UI=OFF -DBUILD_MTPZ=ON -DBUILD_FUSE=OFF
make aft-mtp-cli
```

zytunes automatically finds the binary at `aft/build/cli/aft-mtp-cli`. You can also set `AFT_MTP_CLI` to override:

```
export AFT_MTP_CLI=/path/to/aft-mtp-cli
```

### MTPZ keys

The Zune requires MTPZ authentication. Place the keys file in your home directory:

```
cp mtpz-data.example ~/.mtpz-data
```

These keys originate from the [libmtp-zune](https://github.com/kbhomes/libmtp-zune) project.

### iTunes library path

By default, zytunes looks for your iTunes/Music library at `~/Music/Music/Library.xml`. To use a different location, set the `ZYTUNES_LIBRARY` environment variable:

```
export ZYTUNES_LIBRARY="/path/to/Library.xml"
```

You can also pass `--library <path>` to the `sync` and `library` commands.

### Install

```
./install.sh
```

This builds a release binary and installs it to `/usr/local/bin/zytunes`. To uninstall:

```
./uninstall.sh
```

Or build and run directly without installing:

```
cargo build
cargo run
```

Connect your Zune 30 via USB, then run the tool.

## Architecture

### Why aft-mtp-cli?

The Zune 30 uses MTPZ — Microsoft's encrypted extension to MTP — which requires a cryptographic handshake before the device exposes any storage. This handshake involves data-out USB operations (host sending data to device) that are incompatible with libusb on macOS.

**The libusb/IOKit gap on macOS:** On macOS, USB communication can go through either:
- **libusb** (used by the `rusb` Rust crate) — a cross-platform userspace USB library
- **IOKit** — Apple's native USB framework

The Zune 30's MTPZ data-out operations work correctly through IOKit but fail through libusb. Specifically:
- Standard MTP operations (GetDeviceInfo, OpenSession, GetStorageIDs) work fine via libusb
- Data-out operations (SendWMDRMPDAppRequest for the MTPZ certificate exchange) consistently return `GeneralError (0x2002)` via libusb, regardless of payload content
- The same operations succeed immediately via IOKit (as used by android-file-transfer-linux)

This appears to be a fundamental incompatibility between libusb's macOS backend and the Zune's USB implementation for data-out transfers. The `android-file-transfer-linux` project uses IOKit directly on macOS, which handles the Zune's USB quirks correctly.

**Our approach:** zytunes uses `rusb` for fast device detection (which only requires USB descriptor reads and works fine) and delegates all MTP/MTPZ communication to `aft-mtp-cli` as a subprocess. This gives us a working tool now while preserving the option to implement a native IOKit backend in the future.

### Native MTP implementation (preserved)

The `src/mtp_native/` directory contains our original pure-Rust MTP/MTPZ implementation built on `rusb`. It includes:
- Full MTP container format (PTP/MTP packet serialization)
- MTP session management (OpenSession, GetObjectHandles, GetObjectInfo, etc.)
- Complete MTPZ handshake implementation (RSA-1024, AES-128-CBC, CMAC, SHA-1)
- USB bulk pipe transport

This code works correctly for all MTP operations except data-out on macOS. It is preserved for reference and could be revived with a native IOKit USB backend.

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
| `0x2002` | GeneralError | Operation rejected. On macOS with libusb: returned for ALL data-out MTP operations (the IOKit gap). Via aft-mtp-cli: returned when trying to `rm` a non-empty folder. |
| `0x2006` | ParameterNotSupported | Returned by GetDeviceInfo when called with wrong transaction ID (must be tid=0 outside a session) |
| `0x2008` | InvalidStorageID | Storage ID doesn't exist (storages are hidden until MTPZ auth completes) |
| `0x2013` | StoreNotAvailable | Returned for GetObjectHandles on all-storages before MTPZ authentication |
| `0x2016` | InvalidCodeFormat | File format not supported by device. The Zune 30 rejects FLAC, OGG, WAV, OPUS. Only MP3, WMA, and AAC are accepted. |
| `0xa803` | InvalidObjectPropValue | A metadata property value was rejected. Triggered by embedded album art larger than ~200x200px. Resizing art to 200x200 JPEG before import fixes this. |
| `0x201d` | InvalidParameter | Returned by OpenSession when a session is already open. The Zune auto-opens a session on USB connect, so this is normal. |

## What's unknown

- **Firmware-dependent behavior** — product IDs and MTP behavior may vary across firmware versions
- **Ratings / play counts** — how ratings and play counts are represented on-device (playlists are now working via `create-playlist`)

## Project structure

```
aft/                 — vendored fork of android-file-transfer-linux (LGPL-2.1)
  cli/Session.cpp    — CLI commands including zune-init, zune-import, zune-refresh
  mtp/metadata/
    Library.h/cpp    — Zune media library with disk cache support
src/
  main.rs            — CLI entry point, commands (ls, push, rm, sync, library), sync engine
  device.rs          — USB scanning and Zune identification (rusb)
  mtp/
    mod.rs           — module root, DeviceSession trait
    aft.rs           — aft-mtp-cli subprocess wrapper (MTPZ + MTP)
    parse.rs         — output parsing for aft-mtp-cli commands
  mtp_native/        — original pure-Rust MTP/MTPZ (preserved for reference)
    mod.rs
    transport.rs     — USB bulk pipe read/write
    container.rs     — MTP/PTP container format and operation codes
    session.rs       — MTP session management and operations
    mtpz.rs          — MTPZ handshake (keys, crypto, authentication)
  library.rs         — iTunes Library.xml parser (tracks, playlists, artists)
  tui/
    main.rs          — TUI entry point (zytunes-tui binary)
    app.rs           — application state, panel navigation, event handling
    ui.rs            — ratatui widget rendering (layout, panels, overlays)
    background.rs    — background worker thread (device I/O, sync, removal, library loading)
    theme.rs         — color and style definitions
```

## Reference libraries

| Library | Role | Link |
|---|---|---|
| **android-file-transfer-linux** | MTP/MTPZ backend via `aft-mtp-cli`. Uses IOKit on macOS. Has complete MTPZ auth, Zune library management, and dedicated Zune CLI commands. Vendored in `aft/` with library caching additions. Upstream: v4.6, 2025. | [whoozle/android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux) |
| **libmtp-zune** | Protocol documentation. The `mtpz.md` file has the most detailed public description of the MTPZ handshake. Source of the `.mtpz-data` keys. | [kbhomes/libmtp-zune](https://github.com/kbhomes/libmtp-zune) |

### Other references

- [libmtp](https://github.com/libmtp/libmtp) — open-source MTP library with some Zune-specific code paths
- [OpenZDK](https://github.com/ZuneRedux/openZDK-quick-start-kit) — community effort to reverse-engineer Zune internals (Zune HD focused)
- [USB MTP spec](https://www.usb.org/document-library/media-transfer-protocol-v11-spec-and-mtp-v11-adopters-agreement) — the official MTP 1.1 specification
