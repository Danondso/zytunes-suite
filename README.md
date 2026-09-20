# zytunes

[![CI](https://github.com/Danondso/zytunes-suite/actions/workflows/ci.yml/badge.svg)](https://github.com/Danondso/zytunes-suite/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.98%2B-orange.svg)](https://www.rust-lang.org)

A Rust tool for syncing music (and photos/videos on Zune) to a Microsoft Zune or classic iPod from macOS and Linux.

## Status

**Active development.** Core functionality is working: device detection for both Zune and iPod Classic, MTPZ authentication (Zune), iTunesDB reads/writes (iPod), file listing, music push/remove, library sync (by artist, album, or track), and automatic transcoding of unsupported formats.

## What works

- **Multi-device detection** — scans for both Microsoft Zune (VID `0x045e`, classic models + Zune HD PID `0x063e`) and classic iPod via `rusb` plus mounted-volume probing. CLI and TUI iterate registered backends and open a session against whichever device is connected
- **MTPZ authentication (Zune)** — the Zune requires Microsoft's encrypted MTPZ handshake before exposing storage. Handled automatically by `zune-mtp` (IOKit on macOS, libusb on Linux). Credentials are **not shipped**: copy a 5-line hex `.mtpz-data` file to `~/.mtpz-data`, or set `mtpz_data` / `ZYTUNES_MTPZ_DATA`. iPod sync does not use this file
- **iPod Classic sync** — full music sync via a pure-Rust `ipod-db` crate. iTunesDB parser/writer with hash58 signing, ArtworkDB + ITHMB thumbnails, from-scratch libgpod-ported mhit writer for new tracks, and raw blob replay for lossless round-trip of existing tracks
- **File listing** — `ls [path]` enumerates storage and prints the device's directory tree (device browser in the TUI shows human-readable `Artist/Album/Title.ext` paths on the iPod instead of the hashed F-dir filenames)
- **Music push** — `push <files...>` uploads music files to the device with proper metadata
- **Music removal** — `rm <device-paths...>` removes files/folders from the device (leaf-first for directories; firmware `DeleteObject` on a non-empty folder does not cascade)
- **Music library sync** — `sync <type> <name>` syncs tracks by artist, album, or track name by scanning a local music folder. Detects duplicates already on device and skips them. The TUI's queued sync also filters out tracks already on the device before dispatching the sync
- **Library scan** — `scan` pre-warms the directory-library cache without opening the TUI (useful for large libraries)
- **Photo & video sync (Zune)** — `photo-sync [dir]` and `video-sync [dir]` push images and videos to the Zune's Pictures/Video stores. Videos are transcoded to WMV2/WMAv2 via ffmpeg; photos are JPEG-normalised and resized to fit 240×320. iPod photo/video sync is not implemented
- **Auto-transcoding (audio)** — formats the connected device does not play natively are transcoded via pure-Rust symphonia + LAME (MP3) or ffmpeg (FLAC→ALAC on iPod). Zune album art is resized to 200×200 JPEG. The M4A/ALAC→MP3 path trims trailing silence leaked by symphonia's unapplied `elst` edit-list atoms
- **Native-format passthrough** — Zune natives (MP3, WMA, AAC) skip transcoding. iPod natives are broader (MP3, M4A, AAC, ALAC, WAV, AIFF); only OGG/OPUS drop to MP3 on iPod
- **Per-device lossless promotion** — FLAC library tracks pushed to iPod transcode to ALAC (lossless) instead of dropping to MP3. Zune still falls through to MP3 since its firmware has no lossless container. Controlled by `DeviceCapabilities::lossless_target`
- **CD import** — insert an audio CD and zytunes shows a status bar with the disc's artist/album (identified via MusicBrainz; libdiscid reads the disc TOC and our pure-Rust `compute_disc_id` derives the MB disc ID from it). Press `i` to open an import overlay: toggle individual tracks, cycle through alternate release matches, pick a fidelity (FLAC, WAV, or MP3 V2 / V0 / 320 CBR), and optionally auto-eject when done. Files land under `{music_dir}/{Artist}/{Album}/01 - Title.{ext}` with Picard-equivalent tags (identity, numbering, ISRC / barcode / catalog / label, and 6 MusicBrainz IDs — track, recording, release, release-group, release-artist, and track-artist — that the tag manager keys on)
- **Library browsing** — `library [query]` browses/searches your music library
- **Directory scanning** — point zytunes at a music folder. It reads tags via lofty (FLAC, M4A, OGG, WAV, MP3, etc.) and infers metadata from the directory structure (`Artist/Album/Track.ext`) for untagged files. Set `ZYTUNES_MUSIC_DIR` or add `music_dir` to `~/.config/zytunes/config.toml`
- **Interactive TUI** — `zytunes-tui` launches a terminal UI (ratatui) for browsing your music library, connecting to the device, managing a sync queue, and monitoring sync progress. Library scanning runs in the background on startup. The left panel draws Zune-shaped ASCII art (same drawing for iPod) plus firmware / serial / model, a storage bar, loading spinner, track count, and queue count; the sidebar/header labels the connected family (`Zune` / `iPod`). iPod titles use the iTunes-assigned name from `iPod_Control/iTunes/DeviceInfo` when present, otherwise a generation + capacity label composed from SysInfo `ModelNumStr` (libgpod suffix table covering 1G through Classic, plus mini / nano / shuffle), SysInfoExtended `FamilyID`, USB product ID, and storage size
- **Device content browsing** — `v` cycles Library → Device (when connected) → Playlists. Device mode organises tracks by artist/album; the sidebar auto-labels itself `Zune: …` or `iPod: …`. The Duration column is filled from the device (MTP `0xDC89` on Zune, iTunesDB mhit on iPod) with a library-tag fallback so matched tracks aren't blank while handles are still merging
- **Playlists** — a third browse mode for manual and generated playlists (Discover Weekly-style form). Create (`N`), rename (`e`), regenerate (`R`), generate from a library selection (`G`), add a library track (`+`), delete a playlist or drop a track (`d`). Playlists can be queued for device sync with `a`
- **Device track removal** — in Device mode, `a` (and `A` on the track list) queues the current selection for removal; `D` confirms and executes the removal queue; `C` clears it. `U` dedupes copies on the device, keeping the newest. Progress is shown during removal and the device track list auto-refreshes afterward
- **Album-art rendering** — two renderers: unicode `halfblock` (default) and a 10-char luminance ramp `ascii` renderer. Press `T` to toggle; choice persists to `config.toml`. Per-album renderings are cached at `~/.cache/zytunes/art/` keyed by `(artist, album)` with `(mtime, size)` fingerprint invalidation so re-tagging refreshes automatically
- **Audio playback** — in-TUI preview of library tracks via rodio. `Space` play/pause, `n`/`p` next/previous, `<` / `>` (or `,` / `.`) scrub ±5 seconds. Press `P` to cycle the now-playing panel through auto → force-hidden → force-shown. Selected rows marquee-scroll long titles
- **Stem-split playback** — press `M` while a track plays to split it into live-toggleable stems, muted/unmuted with the digit keys from a strip in the now-playing panel. Separation runs in the background — the original keeps playing and swaps into stem playback at the same position when the stems are ready — and results are cached under `~/.cache/zytunes/stems/` (10 GB LRU cap). Four recipes, selected with `recipe` in the `[stems]` table:
  - `demucs` (default) — six stems (vocals / drums / bass / guitar / piano / other) from a single Demucs `htdemucs_6s` pass, keys `1`–`6`
  - `hq` — the same six stems via a two-pass cascade through [`audio-separator`](https://github.com/nomadkaraoke/python-audio-separator): BS-Roformer pulls the vocals (audibly cleaner than demucs), then `htdemucs_6s` separates the band from the devocalized instrumental
  - `sw` — the same six stems from a single jarredou BS-RoFormer-SW pass (stronger guitar/piano than `htdemucs_6s`; slow on CPU)
  - `hq-harmony` — seven stems, keys `1`–`7`: a Mel-Roformer karaoke pass additionally splits the vocals into **lead** and **backing/harmony**

  Engines are Python subprocesses; on first use zytunes offers a one-time managed install via `uv` (CPU-only PyTorch, ~1.5 GB; the Roformer recipes also download model checkpoints of 200 MB–1 GB on first separation) after explicit consent — nothing installs at startup or during the library scan. Heads-up: Roformer inference on CPU is markedly slower than demucs — the Roformer recipes (`hq` / `sw` / `hq-harmony`) really want `gpu = true`. Configure via the `[stems]` table in `~/.config/zytunes/config.toml` (`recipe`, `command`, `package`, `model`, `gpu`, `cache_max_gb`, `cache_dir` to relocate the stem cache off the default `~/.cache/zytunes/stems` — onto a roomier disk, or somewhere easy to grab the separated FLACs by hand — and `provision = "manual"` to opt out of auto-install). Separated stems are cached per **(track, recipe)** so flipping `recipe` back and forth replays instantly instead of re-separating — but each recipe in play keeps its own ~150–250 MB per track against the same `cache_max_gb` (default 10), so comparing recipes across many tracks fills the cap correspondingly faster
- **Stem settings panel** — press `o` to pick the separation recipe in-TUI (engine, stem count, and CPU-cost hints per row; persists to config and takes effect on the next `M`), see which binary each engine resolved to and via which precedence rung (`[stems] command` → PATH → managed install), and see the resolved stem-cache directory and its size. Two maintenance actions from the panel: uninstall an engine (`u`) with its model checkpoints — optionally the stem cache too — or clear just the separated-stems cache (`c`) without touching any engine (the FLACs are derived data — they re-separate on the next split). Both confirm before deleting and report reclaimed space
- **Album stem pre-warming** — press `M` on an album in the sidebar to separate every track into the stem cache in the background (for offline or on-stage use). A confirmation shows track count, how many are already cached, projected disk against the cache cap, and the CPU-time expectation; progress shows in the footer (`3/12 (40%)`). Splitting a single track mid-batch suspends the batch and it resumes automatically afterwards; `M` on the album again cancels
- **Track-info inspector** — press `I` on the track list (Library, Device, or Playlists) to open a centered, scrollable popup. Library tracks show parsed metadata: title/artist/album, composer / conductor / lyricist, ISRC / barcode / catalog number, all seven MusicBrainz IDs (`mb_work_id` is scan-time only — CD rip writes the other six), ReplayGain values, audio properties (sample rate, bit depth, bitrate, channels), file size, encoder, and a lyrics preview. Device-side rows are thinner. Sections are suppressed when empty so lightly-tagged tracks stay terse. Long values (file paths, MB UUIDs) marquee-scroll inside the value column. `j`/`k` or arrows scroll, `g`/`G` top/end, `PageUp`/`PageDown` page, `Esc`/`q`/`I` close
- **Tag manager** — press `m` on a Library album or track to retag against MusicBrainz (search, pick a release, preview a field-level diff, apply). Uses the same `MusicBrainzClient` as CD import; library MBIDs auto-resolve when present
- **Playback-cache clear** — press `X` to confirm-delete the on-disk playback cache (`/tmp/zytunes-playback`)
- **Log export** — press `L` to dump the live log to `/tmp/zytunes-log.txt` and copy the path to the system clipboard
- **USB resilience** — both transports recover from transient pipe stalls with a one-shot retry (IOKit `ClearPipeStall` on macOS, libusb `clear_halt` on Linux). Read timeouts clear stalls on both bulk endpoints so the OUT pipe stays in sync with the device. When a sync/remove cascade indicates the USB session is truly gone (unplug, unrecoverable stall, read timeout) the TUI aborts remaining work, clears the session, and prompts a replug
- **Theming** — 16 built-in color themes (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original) plus user-defined themes via `[themes."Name"]` tables in `~/.config/zytunes/config.toml`. Press `t` to open the theme picker
- **Platform USB transports** — `zune-mtp` talks MTP/MTPZ over IOKit on macOS (libusb's macOS backend fails Zune data-out transfers) and libusb on Linux. Both support listing, import, removal, track collection, and stall recovery. iPod sync is USB mass-storage + iTunesDB, not MTP
- **LAN streaming server** — `zytunes-serve` (the `zytunes-stream` crate) exposes the library over HTTP: browse/search/stream/download tracks with byte-`Range` seeking, plus on-demand stem splits served from the same cache the TUI's `M` key writes to. The Flutter client (`mobile/`) shares the TUI theme catalog (plus Bedfellow Light/Dark). The artist list speed-scrolls like an iPod click wheel: after about 20 names, further dragging ticks A→B→C with a haptic click and a list jump per letter, a large overlay, and a decaying coast after a hard flick. The client records a listen with `POST /tracks/{id}/play` once the iTunes threshold is crossed (50% of duration or 4 minutes), writing the same sidecar as the TUI. Android keeps a foreground notification so backgrounding the app does not kill the stream. Ships with a `Dockerfile` and `docker-compose.yml` that bind-mount the host music directory read-only, so the container reads tracks in place rather than copying them into a volume. See [What works: Streaming server](#streaming-server-zytunes-serve) and [`docs/stream-api.md`](docs/stream-api.md)

## Interactive TUI

<!-- TODO: add screenshot or GIF here -->

Launch the interactive terminal UI:

```
zytunes-tui
# or: cargo run --bin zytunes-tui
```

The TUI reads `music_dir` from `~/.config/zytunes/config.toml`, or falls back to the `ZYTUNES_MUSIC_DIR` env var. Every config key is listed with defaults in [`config.toml.example`](config.toml.example).

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

- **Left column**: Device panel (Zune-shaped ASCII art for both families, plus a storage bar), sync queue, and log
- **Center**: 3-panel browser — sidebar, album list (when applicable), and track list
- **Right column**: Context-sensitive keybinding reference (toggle with `h`)

### Browse modes

`v` cycles through:

- **Library** (default) — browse your music library by Artists (`1`) or Albums (`2`). Digit keys become stem toggles instead while the stem strip is on screen (see Playback)
- **Device** — tracks on the connected device, organised by Artist/Album. Skipped in the cycle when nothing is connected. Sidebar header auto-labels itself "Zune: …" or "iPod: …"
- **Playlists** — manual and generated playlists. Reachable with `v` even with no device plugged in

The album detail view shows a ZIP disk ASCII art with album metadata (artist, album, year, track count, duration) alongside the track table.

### Keybindings

Bindings are context-sensitive. An open modal (search, help, CD import, tag manager, stem settings, confirms) consumes keys first. `1`/`2` are Artists/Albums unless the stem strip is visible and claiming those digits.

**Navigation**

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle between panels |
| `Up` / `Down` | Navigate lists |
| `Left` / `Right` | Letter-group skip in the sidebar; year-group skip on the Albums panel |
| `Enter` | Sidebar: expand. Albums: open tracks. Track list: play. Sync queue: start sync |
| `1` / `2` | Switch sidebar to Artists / Albums (when stems are not claiming the digits) |
| `4` | Jump to sync queue (only if the queue is non-empty) |
| `v` | Cycle Library → Device (if connected) → Playlists |

**Library**

| Key | Action |
|-----|--------|
| `/` | Search sidebar (live filter, `Esc` to cancel) |
| `s` | Cycle sort column (TrackList panel) |
| `I` | Track-info popup (TrackList panel, any browse mode) — `j`/`k` or arrows scroll, `g`/`G` top/end, `PageUp`/`PageDown` page, `Esc`/`q`/`I` close |
| `a` | Add current selection to the sync queue |
| `A` | Add all visible tracks (TrackList panel) |
| `m` | Open the MusicBrainz tag manager (Library Albums or TrackList, library loaded) |
| `+` | Add the selected track to a playlist (Library TrackList) |
| `G` | Open the playlist generation form (seeded by the focused artist/album/track when invoked from Library) |

**Playlists**

| Key | Action |
|-----|--------|
| `N` | New manual playlist |
| `e` | Rename the selected playlist |
| `R` | Regenerate the selected generated playlist |
| `G` | Open the generation form (Discover Weekly defaults) |
| `a` | Add the selected playlist to the sync queue |
| `d` | Delete playlist (sidebar) / drop track from playlist (track list) |

**Device**

| Key | Action |
|-----|--------|
| `c` | Connect (only when disconnected; USB detect, then MTPZ handshake on Zune or volume mount on iPod) |
| `r` | Refresh device track list (when connected) |
| `d` | Disconnect (from any panel that does not claim `d` for dequeue / playlist delete). The iPod volume stays mounted so `c` can reconnect without a replug |
| `a` | Queue the current selection for removal |
| `A` | Same as `a` on the current selection (not "remove all") |
| `D` | Confirm and execute the removal queue |
| `C` | Clear the removal queue |
| `U` | Dedupe copies on the device, keeping the newest |
| `X` | Confirm-clear the playback cache |

**Sync**

| Key | Action |
|-----|--------|
| `S` | Start sync (queue must be non-empty; also focuses the queue panel) |
| `Enter` (in queue) | Start sync |
| `d` (in queue) | Remove the selected queue item |
| `C` (in queue) | Clear the sync queue (Library/Playlists; Device mode `C` clears the removal queue instead) |
| `Esc` | Cancel a running sync |

**Playback & display**

| Key | Action |
|-----|--------|
| `Space` | Play / pause |
| `n` / `p` | Next / previous track |
| `<` / `>` (or `,` / `.`) | Scrub −5s / +5s while something is playing |
| `P` | Cycle now-playing panel (auto → hidden → always on) |
| `M` | Stem mixer — split the playing track into stems (first use offers a one-time engine install); press again to cancel a running job or return to normal playback. On an album in the Library sidebar, pre-warms every track into the stem cache |
| `1`–`7` | Toggle stems only while stem playback is Active **and** the stem strip is on screen (six under `demucs`/`hq`/`sw`; seven under `hq-harmony`). Hidden player (`P`) falls digits through to Artists/Albums |
| `o` | Open the stem settings panel (pick recipe, view resolved engine + cache dir/size, uninstall engine, clear stem cache) |
| `T` | Toggle album-art style (halfblock / ASCII) |
| `L` | Dump log to `/tmp/zytunes-log.txt` and copy path to clipboard |
| `PageUp` / `PageDown` | Scroll the sync log (±10) |

Inside the stem settings panel (`o`):

| Key | Action |
|-----|--------|
| `↑` / `↓` (or `k` / `j`) | Move recipe selection |
| `Enter` | Set the highlighted recipe (takes effect on the next `M`) |
| `u` | Uninstall the selected recipe's engine — `Enter`/`y` removes the engine + model checkpoints, `Y` also deletes the stem cache |
| `c` | Clear the separated-stems cache only (confirm with `Enter`/`y`); no engine is touched |
| `Esc` | Close the panel |

**CD import**

| Key | Action |
|-----|--------|
| `i` | Always responds — opens the import overlay when a disc is identified, otherwise toasts one of: "No optical drive detected" (no drive on the system) / "Insert a CD to import" (drive empty) / "Cannot import — {reason}" (TOC read but MB lookup failed — common reason: `musicbrainz_user_agent` not configured) |
| `c` (during rip) | Cancel the active rip (SIGTERMs ffmpeg, skips remaining tracks). Falls through to `c` for connect when no rip is running |

Inside the import overlay:

| Key | Action |
|-----|--------|
| `Esc` / `q` | Cancel and close |
| `Tab` / `Shift+Tab` | Cycle focused field (Tracks → Fidelity → AlternateMatch → AutoEject) |
| `Up` / `Down` | Move track cursor (when Tracks focused) |
| `Space` | Toggle track include (Tracks) / Toggle eject preference (AutoEject) |
| `a` / `n` | Select all / none (Tracks) |
| `Left` / `Right` | Cycle the focused picker (Fidelity or AlternateMatch) |
| `[` / `]` | Cycle alternate match (any focus) |
| `f` / `F` | Cycle fidelity backward / forward (any focus) |
| `e` | Toggle auto-eject (any focus) |
| `Enter` | Start the rip |

**Tag manager** (`m`)

| Key | Action |
|-----|--------|
| `q` | Close (blocked while an apply is in flight) |
| `Esc` | Back one phase, or close from search |
| `Tab` | Toggle artist/album search fields |
| `Enter` | Search / pick a release / apply the diff |
| `s` | Return to search |
| `Space` / `a` / `n` | Toggle focused field / enable all / enable none (diff preview) |
| `c` / `C` / `X`/`E` | Collapse focused track / collapse all / expand all |
| `j`/`k` or arrows | Move focus |

**General**

| Key | Action |
|-----|--------|
| `q` / `Ctrl+C` | Quit |
| `?` | Full help overlay |
| `h` | Toggle keybinding panel |
| `t` | Theme picker |

### Themes

16 built-in themes, selectable with `t`:

iTunes 2004, Gruvbox Dark, Gruvbox Light, Everforest Dark, Everforest Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original

Selected theme is persisted to `~/.config/zytunes/config.toml`. You can also define your own themes by adding a `[themes."Name"]` table that inherits from a built-in `base` and overrides any colors, modifiers, border type, or accent animation:

```toml
theme = "My Custom"

[themes."My Custom"]
base = "Gruvbox Dark"
selection_bg = "#ff00aa"
progress_bar = "#00ffaa"
accent_anim = "pulse"
```

Custom themes appear in the picker alongside the built-ins. Malformed entries (unknown base, bad color, name collision with a built-in) are skipped with a stderr warning — a bad entry never breaks the picker.

### Terminal compatibility

The TUI works in any EAW-compliant terminal (Alacritty, kitty, wezterm, Zed's embedded terminal, iTerm2, Ghostty, etc.). One known exception:

- **cosmic-term** — CJK (Chinese / Japanese / Korean) text misaligns panel borders and zebra-row highlights. This is an upstream rendering bug in cosmic-term ([#325](https://github.com/pop-os/cosmic-term/issues/325), [#369](https://github.com/pop-os/cosmic-term/issues/369)): it advances the cursor by 1 cell for wide glyphs instead of the 2 cells required by Unicode East Asian Width. Our measurement code (via the `unicode-width` crate) is correct; use a different terminal for libraries with CJK metadata until cosmic-term is fixed upstream.

### Sync workflow

1. Browse your music library and press `a` to add artists, albums, or individual tracks to the sync queue
2. Press `c` to connect to the device (auto-detects via USB, performs MTPZ handshake on Zune / mounts the iPod volume)
3. Press `S` or switch to the queue and press `Enter` to start syncing
4. Formats the device does not play natively are transcoded (Zune → LAME VBR `NearBest` ~V0 MP3 with 200×200 art; iPod keeps WAV/AIFF/M4A/ALAC and promotes FLAC → ALAC)
5. Progress and results appear in the log panel; device track list auto-refreshes on completion. Tracks already on the device are skipped automatically and noted in the log
6. If the device disconnects mid-sync (unplug, unrecoverable stall), the TUI aborts remaining items, drops the session, and tells you to replug

### CD import workflow

1. Insert an audio CD. Within ~5 seconds the top status bar shows `CD {drive name} — {artist} — {album}  [i] import` where *drive name* is the friendly label zytunes builds from libdiscid's default-device path (e.g. `Optical Drive (/dev/disk4)`). The artist/album come from MusicBrainz; libdiscid reads the disc TOC and our pure-Rust `compute_disc_id` derives the MB disc ID locally
2. Press `i` to open the import overlay
3. Toggle which tracks to import (`Space`), pick fidelity (`f` / `F` cycles MP3 V2 → MP3 V0 → MP3 320 CBR → FLAC → WAV — defaults to FLAC for lossless archival), cycle alternate match candidates if MusicBrainz returned more than one (`[` / `]`), and toggle auto-eject (`e`, defaults to on)
4. Press `Enter` to start ripping. The status bar takes over with `CD ripping 3/12: {title}  [c] cancel` and updates per-track
5. Files land at `{music_dir}/{Artist}/{Album}/01 - Title.{ext}` with Picard-equivalent MusicBrainz tags (identity, numbering, ISRC / barcode / catalog / label, plus 6 MBIDs: track, recording, release, release-group, release-artist, and track-artist). A `.part` extension is used while writing so the dirlib scanner never sees a half-formed file
6. On completion the disc ejects automatically (unless you turned that off in the overlay), and the new tracks appear in your library on the next dirlib refresh

Set `musicbrainz_user_agent` in `~/.config/zytunes/config.toml` before first use (the public MusicBrainz host requires it per their ToS — format `app/version (contact)`, e.g. `zytunes/3.0.0-alpha (you@example.com)`). Point `musicbrainz_base_url` at a local mirror (`http://localhost:5000/ws/2`) to skip rate limits.

Other CD-import config knobs (all optional):

```toml
default_fidelity     = "flac"  # overlay default: "mp3-cbr-320" | "mp3-v0" | "mp3-v2" | "flac" | "wav"
cd_auto_eject        = true    # eject after a successful rip (defaults to true)
acoustid_fingerprint = true    # embed an AcoustID fingerprint at rip time (~1–15 s/track)
```

## Streaming server (zytunes-serve)

A separate binary (`zytunes-stream` crate) exposes the library over a LAN
HTTP API — browse/search/stream/download tracks, plus on-demand stem splits
served from the same cache the TUI's `M` key writes to. The Flutter client
in `mobile/` records listens via `POST /tracks/{id}/play` after the iTunes
threshold (50% or 4 minutes). Android playback keeps a foreground
notification so backgrounding the app does not kill the stream. Full API reference:
[`docs/stream-api.md`](docs/stream-api.md).

```bash
zytunes-serve [--bind 0.0.0.0] [--port 9847] --token SECRET [--music-dir PATH]
```

Configurable via CLI flags, `ZYTUNES_STREAM_*` environment variables, or a
`[stream]` table in `~/.config/zytunes/config.toml` (in that precedence
order). A **non-empty** token is required for any bind — including
loopback, which is reachable by other users on a shared host. Empty
`[stream] token` values count as unset. There is no `--allow-insecure`
escape hatch; leftover copies of that flag, `[stream] allow_insecure =
true`, or `ZYTUNES_STREAM_ALLOW_INSECURE` fail the process. Serving is
HTTP; put TLS in front on untrusted networks. The server refuses to start
otherwise, since `POST /tracks/{id}/stems` alone would let any
unauthenticated client trigger CPU-heavy separation jobs.

### Docker

```bash
cp .env.example .env   # set ZYTUNES_MUSIC_DIR and ZYTUNES_STREAM_TOKEN
docker compose up --build
```

`docker-compose.yml` builds `zytunes-stream/Dockerfile` and **bind-mounts**
your music directory read-only into the container — the library is read
straight from the host path, nothing is copied into a Docker volume. A
separate named volume persists the library/art/stem caches across restarts.
See the [Docker section of `docs/stream-api.md`](docs/stream-api.md#docker)
for the plain `docker build`/`docker run` equivalent.

## Claude Code commands

Custom [Claude Code](https://claude.ai/code) slash commands live in `.claude/commands/`:

| Command | What it does |
|---------|--------------|
| `/review` | Diffs the current branch against main, reviews for correctness, Rust idioms, cleanliness, readability, refactoring opportunities, and security. Posts findings as line-level comments on the PR, fixes them in priority order, and replies to each comment with the resolution commit. |
| `/fix-ci` | Finds the PR for the current branch, pulls failing CI check logs, correlates failures with local code, and produces a structured fix plan for approval before making changes. |
| `/triage-ci` | Same CI pull as `/fix-ci`, but stops at a categorized report (build / test / lint / flake / infra) with no code changes. |
| `/debug` | Spawns research agents on a non-trivial bug, aggregates findings into `DEBUG.md`, and ranks hypotheses with validation steps. |

`/review` and `/fix-ci` run `cargo fmt` and `cargo clippy -- -D warnings` as part of their fix workflow, and add meaningful test coverage (regression tests for bugs, edge cases for new logic) without test theatre.

## What's planned

- **Dump command** — `dump` to pull all music off a device to a local directory
- **Submit unknown discs to MusicBrainz** — when a CD's disc-id has no MB match, offer a one-key submission via OAuth (read endpoints don't need auth; writes do — this is the first place we'd need the OAuth flow)
- **Auto-queue ripped tracks to device** — optional toggle to enqueue freshly-ripped files for sync to a connected device, gated on `auto_queue_ripped_to_device` config

## Setup

### Prerequisites

```
brew install libusb            # required
brew install libdiscid         # required for CD detection / import (libdiscid is LGPL, dynamically linked)
brew install ffmpeg            # required for `video-sync` and CD ripping; optional for TUI playback of WMA files
```

On Debian/Ubuntu: `apt install libusb-1.0-0-dev libdiscid-dev ffmpeg`.

Stem-split playback (`M` in the TUI) additionally needs a Python engine: `demucs` for the default recipe, or [`audio-separator`](https://github.com/nomadkaraoke/python-audio-separator) for the `hq`/`sw`/`hq-harmony` recipes. You don't have to preinstall either — on first use zytunes offers a one-time managed install via [`uv`](https://docs.astral.sh/uv/) — but an existing install works too: point `[stems] command` in `~/.config/zytunes/config.toml` at the engine binary. Roformer model checkpoints download automatically on first separation into `~/.cache/zytunes/models/`.

### Zune authentication

Zune sync needs an MTPZ handshake before the session can do useful work.
zytunes does **not** ship the credentials that handshake needs. Obtain a
`.mtpz-data` file in the 5-line hex format used by the existing MTPZ /
libmtp ecosystem, then point zytunes at it (first match wins):

1. **`ZYTUNES_MTPZ_DATA`** — path to the file
2. **`mtpz_data`** in `~/.config/zytunes/config.toml`
3. **`~/.mtpz-data`** — default location

```
# copy into the default location
cp /path/to/.mtpz-data ~/.mtpz-data

# or keep it elsewhere and set the path
# ~/.config/zytunes/config.toml
mtpz_data = "/path/to/.mtpz-data"
```

Without a readable file at the resolved path, Zune connect/sync fails at
the handshake. The iPod backend does not use it. See
[libmtp-zune](https://github.com/kbhomes/libmtp-zune) for protocol notes.

### Config file

Copy [`config.toml.example`](config.toml.example) to `~/.config/zytunes/config.toml` and uncomment what you need. Paths, TUI, scan/fingerprinting, MusicBrainz/AcoustID, CD import, `[stems]`, `[stream]`, and custom `[themes."Name"]` tables are all there. `./install.sh --setup` copies that example (backing up any existing file) and uncomments only `music_dir`.

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
./install.sh                  # CLI + TUI
./install.sh --setup          # write a new ~/.config/zytunes/config.toml, then install
./install.sh --setup --serve  # new config + zytunes-serve
```

This builds release binaries and installs `zytunes` (CLI) and `zytunes-tui` (interactive TUI) to `/usr/local/bin/`. The LAN streaming server (`zytunes-serve`) is optional — pass `--serve` (or `--all`) to build and install it too.

`--setup` is the first-time wizard: it asks for your music library (default `~/Music`, or `xdg-user-dir MUSIC` when that tool is on PATH), then copies [`config.toml.example`](config.toml.example) to `~/.config/zytunes/config.toml` with only `music_dir` uncommented (any existing file is moved to `config.toml.bak`). Photo/video, cache, stems, and MTPZ stay commented — uncomment them in the file when you need them. To uninstall:

```
./uninstall.sh
```

Or build and run directly without installing:

```
cargo build
cargo run
```

Connect a Zune or iPod via USB, then run the tool.

### Debugging

USB-level debug logging can be enabled via environment variables:

```
IOKIT_USB_DEBUG=1 cargo run       # Trace native IOKit USB reads/writes
ZYTUNES_DUMP_ART=1 cargo run      # Dump the exact JPEG bytes sent for album art to /tmp/zytunes-last-art.jpg
```

Zune diagnostic probes (MTP session required):

```
zytunes probe props [format]      # properties the device advertises (default MP3)
zytunes probe playcount [obj]     # UseCount / Rating / SkipCount for an audio object
zytunes probe zmdb-dump <out>     # raw ZMDB binary (firmware 3.0+)
zytunes probe all-props [obj]     # every prop on an object, ignoring the supported list
zytunes probe vendor-op <code> …  # read-only vendor ops (allowlist)
```

## Architecture

### USB architecture

The Zune uses MTPZ — Microsoft's encrypted extension to MTP — which requires a cryptographic handshake before the device exposes any storage. That handshake involves data-out USB operations (host sending data to the device).

**macOS:** USB can go through libusb (`rusb`) or IOKit. Standard MTP ops (OpenSession, GetStorageIDs) work via libusb; MTPZ data-out (`SendWMDRMPDAppRequest` for the certificate exchange) returns `GeneralError (0x2002)` via libusb and succeeds via IOKit. zytunes therefore uses `rusb` only for descriptor-based detection, and `zune-mtp`'s IOKit transport for all MTP/MTPZ I/O.

**Linux:** libusb talks to the kernel USB stack directly, so the same `zune-mtp` session runs on `LibusbTransport`. Stall recovery (`clear_halt` + one-shot retry, dual-endpoint timeout clear, `UsbFatal` on unplug) matches the IOKit backend.

iPod Classic is USB mass-storage with an on-disk iTunesDB — no MTP.

### Native MTP backend (zune-mtp)

The `zune-mtp/` workspace crate provides a Rust MTP/MTPZ stack:
- **transport/iokit.rs** — macOS IOKit USB transport: device/interface discovery via `IOServiceMatching`, bulk pipe read/write via `IOUSBInterfaceInterface` vtables. Automatic `ClearPipeStall` recovery (with a one-shot retry) on recoverable IOKit errors; on read timeouts the transport clears stalls on both bulk endpoints. `read_container_with_timeout(secs)` lets callers supply longer windows for slow operations (e.g. album-art commits to flash)
- **transport/libusb.rs** — Linux libusb transport with the same stall-recovery / `UsbFatal` classification
- **container.rs** — MTP/PTP container format: command, data, and response container building and parsing
- **session.rs** — MTP session management: OpenSession, GetDeviceInfo (best-effort; some firmware returns `0x2006` and we fall back to device property `0xD404` for firmware strings), GetStorageIDs, GetStorageInfo, GetObjectHandles, GetObjectInfo, SendObjectInfo/SendObject, DeleteObject, SetDevicePropValue, GetObjectPropsSupported, GetObjectPropList, SetObjectPropValue, SendObjectPropList, GetObjectReferences, SetObjectReferences. `SetObjectPropValue` uses a 45 s response timeout because the Zune can take many seconds to commit large values (album art) to flash
- **mtpz.rs** — Full MTPZ handshake: SessionInitiatorVersionInfo setup, RSA-1024 signature, AES-128-CBC decryption, CMAC verification, certificate exchange
- **proplist.rs** — MTP ObjectPropList builder for SendObjectPropList: constructs binary property list payloads with string, u16, and u32 property types
- **transport/iokit_ffi.rs** — Raw FFI declarations for IOKit/CoreFoundation

The `NativeSession` in `app/src/mtp/native.rs` wraps `zune-mtp` and implements the `DeviceSession` trait, providing ls, import, rm, and track collection. Device-scoped caches (`track-cache`, library handles, sync progress) default under `$HOME` as `~/.zytunes-*-{serial}` and honor `ZYTUNES_CACHE_DIR` (the workspace `.cargo/config.toml` points it at `./.zytunes-cache`).

## What's known

| Topic | Details |
|---|---|
| USB Vendor ID | `0x045e` (Microsoft) |
| USB Product ID (media mode) | `0x0710` |
| USB Product ID (firmware update) | `0x0711` |
| USB Product ID (MTP alternate) | `0x0712` |
| USB Product ID (Zune HD media) | `0x063e` |
| Transfer protocol | MTPZ (MTP with encrypted Zune extensions) |
| Authentication | RSA-1024 certificate exchange + AES-128-CBC + CMAC |
| macOS USB transport | IOKit required for MTPZ data-out; libusb fails that path |
| Linux USB transport | libusb (`rusb`) with stall recovery |
| Device storage layout | Root dirs: Albums, Music, Podcasts, Series |
| Music organization | Music/{Artist}/{Album}/{Track} hierarchy |
| Play counts / ratings | MTP `0xDC91` UseCount, `0xDC8A` Rating, `0xDC92` SkipCount via bulk `GetObjectPropList` |
| MTP version | 1.0 with 85 supported operations |

## Zune MTP error codes

Error codes encountered during development and what they mean in the Zune context:

| Code | Name | Meaning on Zune |
|---|---|---|
| `0x2001` | OK | Success |
| `0x2002` | GeneralError | Operation rejected. On macOS with libusb: returned for ALL data-out MTP operations (the IOKit gap). Firmware also returns this for `DeleteObject` on a non-empty folder; `rm` walks children first so that path is not the user-facing `rm` behaviour. |
| `0x2006` | ParameterNotSupported | Seen from GetDeviceInfo on some firmware/states. The session still calls it after OpenSession and falls back to device property `0xD404` for the firmware string. |
| `0x2008` | InvalidStorageID | Storage ID doesn't exist (storages are hidden until MTPZ auth completes) |
| `0x200f` | SessionNotOpen | Returned by EnableTrustedFilesOperations when MTPZ confirmation step was not accepted by the device |
| `0x2013` | StoreNotAvailable | Returned for GetObjectHandles on all-storages before MTPZ authentication |
| `0x2016` | InvalidCodeFormat | File format not supported by device. The Zune rejects FLAC, OGG, WAV, OPUS. Only MP3, WMA, and AAC are accepted. |
| `0xa803` | InvalidObjectPropValue | A metadata property value was rejected. Triggered by embedded album art larger than ~200x200px. Resizing art to 200x200 JPEG before import fixes this. |
| `0x201d` | InvalidParameter | Returned by OpenSession when a session is already open. The Zune auto-opens a session on USB connect, so this is normal. |

## What's unknown

- **Firmware-dependent behavior** — product IDs and MTP behavior may vary across firmware versions. SkipCount (`0xDC92`) is in the same neighbourhood as UseCount/Rating; v1.4 may reject it, in which case the merge sees a zero delta

## Project structure

```
app/                 — CLI + TUI (Cargo package name `zytunes`)
  src/
    main.rs          — CLI entry point (ls, push, rm, sync, library, scan, photo-sync, video-sync, probe)
    lib.rs           — public API, sync engine, SyncType enum; re-exports transcode
    transcode.rs     — audio transcode (symphonia + LAME; FLAC→ALAC ffmpeg) and video WMV ffmpeg
    device/
      mod.rs         — DeviceBackend trait, DeviceCapabilities, DeviceFamily
      zune.rs        — ZuneBackend: rusb-based scan, opens NativeSession
      ipod.rs        — IpodBackend: mounted-volume scan, opens IpodSession
    mtp/
      mod.rs         — DeviceSession trait, TrackMeta
      native.rs      — Zune session (NativeSession on zune-mtp)
      ipod_session.rs— iPod session (IpodSession on ipod-db)
      parse.rs       — DeviceEntry struct and parsing utilities
      zmdb.rs        — Zune Metadata Database parser (vendor op 0x9217)
    library.rs       — MusicLibrary trait and shared Track type
    dirlib.rs        — DirectoryLibrary: recursive folder scanner (lofty tags + path fallback)
    cache.rs         — on-disk cache for the directory scanner
    paths.rs         — device-scoped cache paths (ZYTUNES_CACHE_DIR aware)
    cd/              — CD import (discid, drive, rip, metadata tagging)
    musicbrainz.rs   — MusicBrainz WS2 client
    tag_ops.rs       — tag-manager field diffs / apply
    playlist.rs / playlist_store.rs — playlist model + on-disk store
    stems.rs         — recipes, engines (demucs / audio-separator), stem cache (LRU)
    stems/
      process.rs     — shared engine-subprocess driver (drains, cancel, teardown)
      provision.rs   — stem-engine discovery + uv-managed install
    tui/
      main.rs        — TUI entry point (zytunes-tui binary)
      app.rs         — application state, panel navigation
      app/
        keys.rs      — keyboard dispatch (modals then global map)
        events.rs    — background-worker event handlers
      ui/mod.rs      — ratatui widget rendering (layout, panels, overlays, album-art cache)
      background.rs  — background worker thread (device I/O, sync, removal, library loading, album-art load)
      anim.rs        — theme-aware animations
      audio.rs       — local audio playback (rodio), stem playback commands
      audio/
        stem_mix.rs  — stem mixing Source with live per-stem gains
      theme.rs       — Theme struct, built-in presets, user-theme merge
      config.rs      — TOML config file (~/.config/zytunes/config.toml)
zune-mtp/            — native MTP/MTPZ library (workspace crate)
  src/
    lib.rs           — crate root, public API, MtpError
    transport/       — IOKit (macOS) and libusb (Linux) USB transports, ClearPipeStall / clear_halt recovery
    container.rs     — MTP/PTP container format
    session.rs       — MTP session
    mtpz.rs          — MTPZ authentication
    proplist.rs      — MTP ObjectPropList builder
  examples/          — standalone integration tests (connect, ls, tracks, import, MTPZ primitives)
ipod-db/             — pure-Rust iTunesDB parser/writer (workspace crate)
  src/
    lib.rs           — crate root
    itunesdb.rs      — iTunesDB reader
    itunesdb_write.rs— mhit/mhbd writer (libgpod-port), hash58 signing
    artwork/         — ArtworkDB + ITHMB thumbnail writer
    detect.rs        — iPod volume/mount detection
    fs.rs            — iPod_Control path helpers
    hash.rs          — hash58 signing
    encoding.rs      — UTF-16 helpers
zytunes-stream/      — LAN HTTP server (`zytunes-serve` binary)
mobile/              — Flutter LAN client
```

## Reference libraries

| Library | Role | Link |
|---|---|---|
| **libmtp-zune** | Protocol documentation. The `mtpz.md` file has the most detailed public description of the MTPZ handshake. zytunes does not vendor handshake credentials. | [kbhomes/libmtp-zune](https://github.com/kbhomes/libmtp-zune) |

### Other references

- [libmtp](https://github.com/libmtp/libmtp) — open-source MTP library with some Zune-specific code paths
- [OpenZDK](https://github.com/ZuneRedux/openZDK-quick-start-kit) — community effort to reverse-engineer Zune internals (Zune HD focused)
- [USB MTP spec](https://www.usb.org/document-library/media-transfer-protocol-v11-spec-and-mtp-v11-adopters-agreement) — the official MTP 1.1 specification

## License

MIT — see [LICENSE](LICENSE). Third-party notices in [THIRD_PARTY.md](THIRD_PARTY.md).

## Trademarks

"Zune" is a trademark of Microsoft Corporation. "iPod" and "iTunes" are
trademarks of Apple Inc. zytunes is an independent interoperability tool and
is not affiliated with, endorsed by, or sponsored by Microsoft or Apple. All
product names are used under nominative fair use for the sole purpose of
describing compatibility with the named devices.
