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
- ** Code Audit**
  - Rule of threes should be observed, what code is duplicated > 3 times or two even if the code block is large
  - Rust best practices
  - files too big? 
- ** iPod Support **
  - What's it take? I bet we can, pure rust like the zune is gonna be the route first
  - 
- **Philips GoGear support** — user has a couple of units, worth attempting
  - Identify which models (VID/PID, firmware generation — SA/HDD vs Vibe vs Ariaz etc.)
  - Transport: most GoGears are UMS/MSC (plain mass storage) — no MTPZ/iTunesDB lift needed, just file copy + folder conventions
  - Some later models use MTP (non-encrypted); `zune-mtp` transport is reusable, auth path is not
  - Check if any model needs a proprietary DB (SA52xx songdb.dat) vs pure tag-based playback
  - Slot into `DeviceSession` trait once scoped

## Done

- **Local audio playback** — play/pause/skip for local tracks via rodio (`tui/audio.rs`). Basic queue support integrated into TUI.
- **Native IOKit USB backend** — replaced libusb/aft-mtp-cli with Apple's native IOKit for direct MTP/MTPZ communication via the `zune-mtp` workspace crate. Eliminates all external MTP tool dependencies.
- **Device content view** — toggle between Library and Device browse modes (`v` key). Reuses artist/album/track panels for device content. Device tracks parsed from `/Music/Artist/Album/track` directory structure. `a` key removes tracks in device mode. Supports artist, album, and track-level removal.
- **Sync engine** — diffs local music against device contents, pushes only new tracks, deduplicates, supports playlist syncing.
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely.
- **TUI sync status on Zune art** — loading spinner, track count, syncing spinner, and queue count all displayed on the Zune ASCII art screen.
- **Disable sync until device connected** — `execute_sync()` guards against syncing when no device is present.
- **Batch import performance** — `TrackCache` in `NativeSession` caches the device library to `~/.zytunes-track-cache-{serial}`, avoiding full reload over USB 1.1.
- **Proper rm for special characters** — path escaping handled natively in `zune-mtp` session operations.
- **Theming** — 11 built-in theme presets (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Miami Nights, IBM Mainframe, Windows 95, System 7, BIOS, Red Sands). Live preview picker (`t` key). Config persisted to `~/.config/zytunes/config.toml`.
- **Album art on import** — the Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
