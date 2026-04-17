# TODO

## Next up

- **Device content view (remaining)** — remaining items from the device browsing feature
  - Asterisk or indicator in library browser for tracks already on device
  - Device playlist browsing and removal

## Future

- **Configurable transcode quality** — currently hardcoded to `-q:a 2` (~190kbps VBR). Add CLI flag for bitrate/quality.
- **Theming (remaining)** — additional theme features
  - Custom user-defined themes via config file
  - Theme preview screenshots in docs
- **Player Support (remaining)** — play/pause and basic queue are implemented via rodio
  - Scrubber / seek controls
  - Soundbar visualizer effect (research terminal audio visualizers)
  - Toggleable player panel with minimal controls
- **Radio Support**
  - Is there a terminal thing for radio we can pipe into the player?
- **Scrobbling** — hook into TUI player track-change events to submit listens
  - Last.fm (auth handshake, now-playing + scrobble submit)
  - ListenBrainz (token auth, listen submission + feedback)
  - Config in `~/.config/zytunes/config.toml`, opt-in per service
  - Respect scrobble rules (≥50% played or ≥4min, ≥30s track length)
- **UX Audit**
  - Commands have been made organically
  - Audit mappings
  - Suggest improvements / redundant / confusing
- ** One Offs **
  - Initiating sync clears device
  - logs truncate so you can read whole line, can't be scrolled, should copy log dump location to clipboard
  - perf: batch transcoding
  - UI: more bespoke panel for album art, have it resize instead of clip when it's responsive
  - ascii art album covers again, make it toggleable via a key
  - marquee scroll long artist/track names in the sidebar and track table so truncated text is still fully readable
- ** Code Audit**
  - Rule of threes should be observed, what code is duplicated > 3 times or two even if the code block is large
  - Rust best practices
  - files too big? 
- **iPod Support (remaining)** — core music sync works, non-music features left
  - **Track deletion broken** — `IpodSession::rm` looks up tracks by their colon-separated `ipod_path` (e.g. `:iPod_Control:Music:F00:abcd.mp3`), but the TUI and CLI pass display-format paths like `Artist/Album/Song` (returned by `collect_all_tracks` as the entry `name`). The lookup fails, so the track stays in the DB. Need to change `rm` to accept the display-format path (or the `dbid`/`track_id` stored in the `DeviceEntry.object_id` — we already set that to `dbid`) and delete via that.
  - **Photo sync** — iPod Classic has a Photos database (separate from iTunesDB) at `iPod_Control/Photos/Photo Database`. Uses `mhfd` container format (same as ArtworkDB). Needs ITHMB generation for iPod screen thumbnail + main preview sizes. Zune's `import_photo` is a reference for the public API (takes filename + JPEG bytes). libgpod has a photo writer in `db-artwork-writer.c::ipod_write_photo_db` we can port.
  - **Video sync** — iPod Classic plays MP4/M4V with specific constraints (320x240/640x480, H.264 baseline). Videos go in `iPod_Control/Music/F*/` alongside audio (not a separate directory). iTunesDB entries use `mediatype = 0x02` at mhit +0xD0 (we hardcode `1` for audio). Need to: transcode to iPod-compatible MP4 via ffmpeg (zytunes already has `transcode_to_wmv` for Zune — similar pattern), set `mediatype = 2`, set video-specific mhod types.
  - **User playlist sync from iTunes XML** — currently we only preserve the master playlist via raw blob replay. iTunes user playlists (type 2 mhsd, non-master mhyps) could be added from iTunes Library.xml parsing. Would need mhip writing per playlist member.
  - **Storage/model detection** — like `zune_model_from_storage`, map storage capacity → iPod Classic model name (30GB / 60GB / 80GB / 120GB / 160GB).
  - **Track rating write-back** — read `Play Counts` on connect, apply ratings/play counts back into the iTunesDB on next sync (currently we delete it like libgpod does, losing the data).
- **Philips GoGear support** — user has a couple of units, worth attempting
  - Identify which models (VID/PID, firmware generation — SA/HDD vs Vibe vs Ariaz etc.)
  - Transport: most GoGears are UMS/MSC (plain mass storage) — no MTPZ/iTunesDB lift needed, just file copy + folder conventions
  - Some later models use MTP (non-encrypted); `zune-mtp` transport is reusable, auth path is not
  - Check if any model needs a proprietary DB (SA52xx songdb.dat) vs pure tag-based playback
  - Slot into `DeviceSession` trait once scoped

## Done

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
- **Theming** — 16 built-in theme presets (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Tokyo Night, IBM Mainframe, Amber CRT, Windows 95, System 7, BIOS, Red Sands, Newport Lights, NeXTSTEP, WinAmp Classic, Zune Original). Live preview picker (`t` key). Config persisted to `~/.config/zytunes/config.toml`.
- **Album art on import** — the Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
