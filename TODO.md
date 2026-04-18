# TODO

## Next up

- **Device playlist browsing and removal** — MTP playlists as first-class objects in the device view (currently we browse artists/albums/tracks only). Would need to list playlist objects, show their track references, and support playlist-level removal. Separate from rodio in-memory queue.

## Future

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
- **Album art on import** — the Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
