# TODO

## Next up

- **Device content view** — browse and manage tracks on the device from the TUI
  - Render the device tracks list in the Device panel (data is loaded, UI rendering not yet wired up)
  - Asterisk or indicator in library browser for tracks already on device
  - Device view where searchable items are what's on device
  - Adding device items to queue means removal — UI should represent add vs remove (check mark vs x)
  - Allow playlist, track, artist, album removals

## Future

- **Native IOKit USB backend** — replace libusb with Apple's native IOKit for direct MTP/MTPZ communication (eliminates aft-mtp-cli dependency). See README Architecture section for the libusb/IOKit gap details.
- **Configurable transcode quality** — currently hardcoded to `-q:a 2` (~190kbps VBR). Add CLI flag for bitrate/quality.

## Done

- **Sync engine** — diffs local music against device contents, pushes only new tracks, deduplicates, supports playlist syncing.
- **MP3 passthrough** — native formats (MP3, WMA, AAC) skip transcoding entirely.
- **TUI sync status on Zune art** — loading spinner, track count, syncing spinner, and queue count all displayed on the Zune ASCII art screen.
- **Disable sync until device connected** — `execute_sync()` guards against syncing when no device is present.
- **Batch import performance** — vendored aft-mtp-cli fork caches the device library to `~/.aft-library-cache`, avoiding the ~5 min full reload over USB 1.1.
- **Proper rm for special characters** — `aft_quote()` escapes quotes, strips newlines, and wraps paths for safe subprocess communication.
- **Album art on import** — the Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
