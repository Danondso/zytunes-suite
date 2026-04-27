# Zune playlist sync — post-mortem (resolved 2026-04-27)

End-to-end Zune playlist sync was broken from the day it shipped
(`feat(zune): wire NativeSession::import_playlist`, `0422650`) until
2026-04-27. This doc records what was wrong, what we initially
suspected, and how we got from one to the other — kept around so the
same false trail isn't followed again.

## Symptom

User triggered a playlist sync from the TUI. Tracks uploaded
successfully. The playlist record itself never appeared in the device
UI, and `mtp-probe playlists` listing confirmed **no `0xBA05` record
existed on the device** post-sync. The `mtp-probe playlist-push --keep`
probe, which exercises the same wire flow, produced a visible playlist
with references — proving libmtp Flow A worked on the hardware.

## Initial (incorrect) hypothesis: handle resolver

`NativeSession::import_playlist` resolved `(artist, album, title)`
tuples via `collect_all_tracks`, which goes through ZMDB. ZMDB returns
`object_id = 0` for entries it synthesises from device metadata. The
on-disk track cache restored real IDs by name-matching against any
prior cache entries — but freshly-uploaded tracks have no prior cache
entry, so they stayed at `object_id = 0`, the resolver filtered them
out, and `SetObjectReferences` was called with an empty list. Empty
playlists don't render in the device UI.

This story matched the symptom (tracks present, playlist not visible)
but didn't explain the listing data: even an empty playlist would have
shown up in `mtp-probe playlists` with `REFS = 0`. Nothing was there.

## Actual root cause

`import_playlist` ran a Step-0 capability check at the top of the
function:

```rust
let supported = self.session.get_object_props_supported(0xBA05)?;
```

**v1.4 firmware silently drops `GetObjectPropsSupported(0xBA05)`.**
The device never replies, our 30 s `ReadPipe` timeout fires, the bulk
pipe is left in a half-response state, and `is_device_gone` trips in
the bg worker. `SessionFailed` is emitted; remaining work is dropped;
the user sees a session disconnect. The `SendObjectPropList(0xBA05)`
create call **never ran**.

The `mtp-probe playlist-push` binary discovered this empirically and
gates the same op behind `--probe-caps` (see comment block at
`tools/mtp-probe/src/playlist_push.rs:411-419` describing the wedge).
Production code didn't apply the same gate.

## Fix

Two changes shipped in `fix(zune): firmware-gate 0xBA05 cap check +
session-scoped handle cache`:

1. **Firmware-gate the cap check.** v1.4 (and any device where
   `supports_modern_vendor_ops()` is false) skips
   `GetObjectPropsSupported` and proceeds straight to the create —
   `SendObjectPropList(0xBA05)` with `[ObjectFileName, Name]` is known
   to work on v1.4 hw per the 2026-04-26 probe. Firmware 3.0+ still
   runs the query for diagnostic logging.

2. **Session-scoped `recent_imports` handle cache.** Even with the cap
   check fixed, the resolver couldn't surface freshly-uploaded handles
   from ZMDB. `NativeSession.recent_imports` records the
   `(artist, album, title) → handle` mapping at `import_track` time
   and `import_playlist` consults it before falling back to
   `collect_all_tracks`. Not strictly required to unwedge the bug, but
   the original analysis was correct that this resolver path was
   fragile — fix it while we're here, with tests.

Hardware-validated 2026-04-27: 25-track playlist created end-to-end,
record persists across replug, all 25 references intact.

## Lessons

- **Listings tell the truth.** Once `mtp-probe playlists` showed zero
  records on-device after a "successful" TUI sync, the resolver
  hypothesis was disproven — the bug had to be upstream of the create.
  Reach for hardware introspection before refining theory.
- **v1.4 wedges silently.** Any new MTP op added to a Zune write path
  needs hardware testing on v1.4 OR firmware-gating via
  `supports_modern_vendor_ops()`. The probe binary's source is the
  current best registry of which ops are wedge-prone — read its
  comments before adding new device-facing calls.
- **Empty playlists are a real failure mode** but distinct from "no
  playlist at all". If you see one, you're past the create call; if
  you see none, you're stuck before it.

## Tooling shipped alongside

`mtp-probe playlists` — lists every `0xBA05` record on the device with
handle, parent, ref count, and filename. `--rm 0xH[,0xH...]` deletes
specific handles; `--rm-all --yes` wipes them all. Member tracks are
never touched.
