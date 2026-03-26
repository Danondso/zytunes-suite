# TODO

## Next up

- **Device content view (remaining)** — remaining items from the device browsing feature
  - Asterisk or indicator in library browser for tracks already on device
  - Device playlist browsing and removal

## Future

- **Native IOKit USB backend** — replace libusb with Apple's native IOKit for direct MTP/MTPZ communication (eliminates aft-mtp-cli dependency). See README Architecture section for the libusb/IOKit gap details.
- **Configurable transcode quality** — currently hardcoded to `-q:a 2` (~190kbps VBR). Add CLI flag for bitrate/quality.
- **Theming (remaining)** — additional theme features
  - Custom user-defined themes via config file
  - Theme preview screenshots in docs
- **Player Support**
  - play / pause / queue / scrubber if possible
  - player TUI with soundbar effect (do research to find a visualizer for this)
  - toggleable player view with minimal controls
- **Radio Support**
  - Is there a terminal thing for radio we can pipe into the player?
- **UX Audit**
  - Commands have been made organically
  - audit mappings
  - suggest improvements / redundant / confusing

## Done

- **Device content view** — toggle between Library and Device browse modes (`v` key). Reuses artist/album/track panels for device content. Device tracks parsed from `/Music/Artist/Album/track` directory structure. `a` key removes tracks in device mode. Supports artist, album, and track-level removal. Fixed aft-mtp-cli hanging on empty devices (Zune Flash) via channel-based stderr monitoring.
- **Sync engine** — diffs local music against device contents, pushes only new tracks, deduplicates, supports playlist syncing.
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely.
- **TUI sync status on Zune art** — loading spinner, track count, syncing spinner, and queue count all displayed on the Zune ASCII art screen.
- **Disable sync until device connected** — `execute_sync()` guards against syncing when no device is present.
- **Batch import performance** — vendored aft-mtp-cli fork caches the device library to `~/.aft-library-cache`, avoiding the ~5 min full reload over USB 1.1.
- **Proper rm for special characters** — `aft_quote()` escapes quotes, strips newlines, and wraps paths for safe subprocess communication.
- **Theming** — 11 built-in theme presets (iTunes 2004, Gruvbox Dark/Light, Everforest Dark/Light, Miami Nights, IBM Mainframe, Windows 95, System 7, BIOS, Red Sands). Live preview picker (`t` key). Config persisted to `~/.config/zytunes/config.toml`.
- **Album art on import** — the Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
