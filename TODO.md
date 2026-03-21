# TODO

## Next up

- **Sync engine** — diff local music directory against device contents, push new tracks, optionally remove deleted ones
- **MP3 passthrough** — skip transcoding for files already in Zune-native formats (MP3, WMA). Currently all non-native formats are transcoded.
- Adding syncing status to the zune ascii art
  - loading spinner should be on the screen
  - once loaded, show track count on the screen (3333 tracks)
  - show a syncing spinner when the queue is processing
  - show updated queue count

## Future

- **Native IOKit USB backend** — replace libusb with Apple's native IOKit for direct MTP/MTPZ communication (eliminates aft-mtp-cli dependency). See README Architecture section for the libusb/IOKit gap details.
- **Batch import performance** — `zune-import` loads the entire media library on first call (~5 min for large collections over USB 1.1). Investigate caching or incremental loading.
- **Configurable transcode quality** — currently hardcoded to `-q:a 2` (~190kbps VBR). Add CLI flag for bitrate/quality.
- **Proper rm for paths with special characters** — apostrophes and Unicode in device paths can break aft-mtp-cli's command parser. The `rm-id` workaround works but `rm` by path needs quoting fixes.

## Done

- **Album art on import** — resolved. The Zune 30 rejects embedded art larger than ~200x200px (`InvalidObjectPropValue 0xa803`). Fixed by resizing art to 200x200 JPEG during transcoding.
