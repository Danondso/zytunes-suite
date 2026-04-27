# Zune playlist sync — protocol & format research

> **Update 2026-04-26**: protocol validated end-to-end on real Zune 30 v1.4 hardware via the `mtp-probe playlist-push` binary. The wire format and call sequence in this doc are correct.
>
> **Update 2026-04-27**: TUI-driven sync now works end-to-end on v1.4. The bug was a `GetObjectPropsSupported(0xBA05)` call wedging v1.4 firmware before the create — see [`zune-playlist-known-issues.md`](./zune-playlist-known-issues.md) for the post-mortem.

Scope: how the Microsoft Zune 30 (PID `0x0710`, fw v1.4 baseline, 3.x target) accepts playlists over MTP/MTPZ, and how third-party tools historically built them. Goal is to wire `import_playlist` in `src/mtp/native.rs` (currently a stub returning a "not implemented" error — see `src/mtp/native.rs:1467-1492`).

## TL;DR — recommended implementation path

**Flow A (libmtp-style), zero-byte file body.** The `.zpl` payload doesn't need to be a real SMIL document — the Zune builds on-device playlist membership from MTP object references, not from the file body. libmtp ships exactly this path for the Zune (`create_new_abstract_list` → `SendObjectPropList` → `SendObject(empty)` → `SetObjectReferences`), and the Zune is registered with `DEVICE_FLAG_NONE` — no playlist-related quirks ([music-players.h L437](https://github.com/kbhomes/libmtp-zune/blob/master/src/music-players.h), [libmtp.c L7469-7787](https://github.com/kbhomes/libmtp-zune/blob/master/src/libmtp.c)).

Call sequence for a 2-track playlist named `Roadtrip`:

1. `SendObjectPropList (0x9808)`: `storage_id`=music storage, `parent`=`0x00000000` (or music root handle), `format`=`0xBA05`, `object_size`=`0`, props `[dc07 ObjectFileName="Roadtrip.zpl", dc44 Name="Roadtrip"]`. Per v1.4 detect log those are the only writable string props on `0xBA05` for fw 1.4 — do **not** add `AlbumArtist` / `Composer` / `Genre` / `DateModified`, the prop-desc lookup will fail. Zune HD has more — capability-gate.
2. `SendObject (0x100D)` with zero-length payload. Required even for empty bodies; libmtp settled on `ptp_sendobject(NULL, 0)` after experimenting with a placeholder byte (L7686-7700, comment).
3. `SetObjectReferences (0x9810)` with the new playlist handle plus the ordered list of member track handles (same handles already in `DeviceLibrary`). Zune preserves order.

No vendor op needed. Flow C unnecessary; A and B are equivalent on-device, but A is what libmtp validates against working hardware ([libmtp.c L8048-8060](https://github.com/kbhomes/libmtp-zune/blob/master/src/libmtp.c)).

## 1. ZPL on-disk byte format

ZPL is **WPL with a renamed PI**. WPL is Microsoft's Windows Media Player playlist file: SMIL-derived XML, UTF-8, no XML-decl, the Microsoft-custom `<?wpl version="1.0"?>` processing instruction in place of `<?xml ...?>`. ZPL substitutes `<?zpl version="2.0"?>`. Confirmed by `ross-kahn/make-wpl`, which converts ZPL→WPL by gsub-ing the leading `<?zpl.*` line: `zpl` → `wpl`, `2` → `1`. ([make_wpl.rb](https://github.com/ross-kahn/make-wpl/blob/master/make_wpl.rb), [Drale write-up](https://blog.drale.com/convert-zune-playlist-zpl-to-m3u-or-wpl/))

Other vs WPL: ZPL uses **absolute** paths in `<media src="...">`; WPL uses relative. ([fileinfo.com](https://fileinfo.com/extension/zpl))

Minimal ZPL that the Zune desktop emits, modeled on the [WPL sample on tenforums](https://www.tenforums.com/sound-audio/186624-windows-media-playlist-wpl-computer-usb-2.html) and the [`<media>` schema](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/wmp/media-element):

```xml
<?zpl version="2.0"?>
<smil>
<head>
<meta name="Generator" content="Microsoft Windows Media Player -- 12.0.19041.1266"/>
<meta name="ItemCount" content="2"/>
<title>Roadtrip</title>
</head>
<body>
<seq>
<media src="C:\Users\me\Music\AC DC\01 Riff Raff.mp3" albumTitle="If You Want Blood" albumArtist="AC/DC" trackTitle="Riff Raff" trackArtist="AC/DC" duration="338000"/>
<media src="C:\Users\me\Music\AC DC\02 Hell Ain't a Bad Place to Be.mp3" albumTitle="If You Want Blood" albumArtist="AC/DC" trackTitle="Hell Ain't a Bad Place to Be" trackArtist="AC/DC" duration="252000"/>
</seq>
</body>
</smil>
```

UTF-8, no BOM, CRLF or LF, no XML namespaces. `<media>` attribute set is the union of standard WMP (`src`, `cid`, `tid` per [MS Learn media Element](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/wmp/media-element)) and Zune-extended (`albumTitle`, `albumArtist`, `trackTitle`, `trackArtist`, `duration` ms) per [Microsoft's Zune→Excel article](https://learn.microsoft.com/en-us/previous-versions/office/developer/officetalk2007/cc835493(v=office.11)) using XPath `/smil/body/seq/media/@trackTitle`.

**Key observation:** the Zune builds the on-device playlist from MTP object references (§4), not by parsing the `<media src="">` attribute. Any payload — including zero bytes — will work as the `SendObject` body. Recommend ship empty for v1, ship a real ZPL later for desktop round-trip.

## 2. How the Zune embeds track references

**The `.zpl` body's `<media src="...">` is documentation for the desktop client only.** On-device, member tracks are stored as PTP object references attached to the playlist handle via `SetObjectReferences (0x9811)`, read back via `GetObjectReferences (0x9810)` ([libmtp.c L7777, L7987](https://github.com/kbhomes/libmtp-zune/blob/master/src/libmtp.c)). Both ops are advertised by the Zune 30 v1.4 ([detect log](https://github.com/kbhomes/libmtp-zune/blob/master/logs/mtp-detect-zune.txt)).

Identifier shape: 32-bit MTP object handles — the same ones `SendObjectPropList` returns at track-import time. Persistent across sessions for the same content; already cached in `DeviceLibrary` (`~/.zytunes-library-cache-{serial}`).

## 3. Where playlists live on the Zune filesystem

Zune 30 v1.4 reports `Default playlist folder: 0x00000000` ([detect log L515](https://github.com/kbhomes/libmtp-zune/blob/master/logs/mtp-detect-zune.txt)); Zune HD same ([HD log](https://github.com/yifanlu/libMTP/blob/master/logs/mtp-detect-microsoft-zune-hd.txt)). The device does not nominate a fixed playlist folder. libmtp falls through to `default_music_folder` when this is null ([libmtp.c L8035-8040](https://github.com/kbhomes/libmtp-zune/blob/master/src/libmtp.c)). Default music folder on v1.4 is `0x05000001` (storage root).

Recommendation: try `parent=0x00000000` first; if `0x2009 InvalidParent`, retry against the cached music root handle. Either way the playlist appears in the device's "Playlists" UI category — that view is built from all `0xBA05` objects regardless of tree position. There is no `\Internal Storage\Playlists\` style fixed path; the Zune isn't UMS and MTP hierarchies are arbitrary. Anything `Playlists\foo.zpl`-shaped lives only on the host PC (`My Music\Zune\Playlists\`).

## 4. MTP wire flow — picking A / B / C

**Flow A.** Annotated trace (mirrors libmtp's `create_new_abstract_list`):

```
→ 0x9808 SendObjectPropList  storage=<music> parent=<0 or music_root>
                             format=0xBA05 size=0
                             props=[ ObjectFileName="Roadtrip.zpl",
                                     Name="Roadtrip",
                                     ProtectionStatus=0, NonConsumable=0 ]
← 0x2001 OK + new_handle
→ 0x100D SendObject           data=<empty>
← 0x2001 OK
→ 0x9811 SetObjectReferences  object=<new_handle>
                              refs=[track_a, track_b, ...]
← 0x2001 OK
```

Zune v1.4 advertises `9808 SendObjectPropList`; use that branch. The `SendObjectInfo` fallback in libmtp (L7663+) is for devices missing `0x9808` — not us.

Flow B (rich `.zpl` body, no SetObjectReferences): never observed; Zune doesn't parse `<media src=>` as a track resolver. Skip.

Flow C (vendor op): none of the Zune vendor ops map to playlist creation. `0x9214/0x9215` are Enable/Disable trusted files. `0x9217` is `GetZuneMetadataDatabase` (read-only). `0x9220–0x9232` are sync-state machinery; `9104 GetSyncList` returns the Zune-software-managed sync set, not a user-playlist write path. No third-party tool uses a vendor op for playlist push.

## 5. Prior art

Useful (port from / cite):
- [libmtp-zune libmtp.c create_new_abstract_list L7469-7787](https://github.com/kbhomes/libmtp-zune/blob/master/src/libmtp.c) — **primary reference**. Generic MTP playlist creator, used unmodified for Zune. The `SendObjectPropList` branch (L7538-7661) is what to port.
- [libmtp-zune music-players.h L437](https://github.com/kbhomes/libmtp-zune/blob/master/src/music-players.h) — Zune (PID `0x0710`) registered `DEVICE_FLAG_NONE`; no playlist quirks expected.
- [libmtp-zune logs/mtp-detect-zune.txt](https://github.com/kbhomes/libmtp-zune/blob/master/logs/mtp-detect-zune.txt) — real Zune 30 v1.4 capability dump. `0xBA05` props writable on v1.4 are minimal (`dc44 Name`, `dc07 ObjectFileName` only).
- [yifanlu/libMTP logs/mtp-detect-microsoft-zune-hd.txt](https://github.com/yifanlu/libMTP/blob/master/logs/mtp-detect-microsoft-zune-hd.txt) — Zune HD `0xBA05` is richer (incl. `0xDD60 URL Source`). Don't conflate with Zune 30.
- [Microsoft Zune→Excel article](https://learn.microsoft.com/en-us/previous-versions/office/developer/officetalk2007/cc835493(v=office.11)) — first-party confirmation of `.zpl` schema (`/smil/body/seq/media/@trackTitle` + siblings).
- [ross-kahn/make-wpl make_wpl.rb](https://github.com/ross-kahn/make-wpl/blob/master/make_wpl.rb) — one-line proof that `.zpl` differs from `.wpl` only in `<?zpl version="2.0"?>` vs `<?wpl version="1.0"?>`.

Dead ends checked: ZenseMe ([dumbie](https://github.com/dumbie/ZenseMe), [mope-life](https://github.com/mope-life/ZenseMe)) is a Last.fm scrobbler, read-only. Banshee/Rhythmbox never landed Zune support. Zboard/OpenZDK is on-device app SDK, not desktop sync. [KritZu](http://vishaljoshi.blogspot.com/2008/12/kritzu-zune-playlist-generator-based-on.html) emits `.zpl` host-side and lets the official Zune client push it — confirms `.zpl` schema, doesn't show the MTP wire flow.

## 6. Known landmines specific to playlists

- **`0xBA05` writable prop set on v1.4 is tiny:** `dc07 ObjectFileName`, `dc44 Name` only. Any other writable (e.g. `0xDC4A AlbumArtist`) will fail. Capability-gate every prop on `get_object_prop_desc(...).GetSet`, the way libmtp does. The Zune HD's set is richer — don't hardcode either. ([logs/mtp-detect-zune.txt L405-419](https://github.com/kbhomes/libmtp-zune/blob/master/logs/mtp-detect-zune.txt))
- **MTPZ data-out separation applies** to all three writes (proplist, empty SendObject, references payload). The existing `send_object_prop_list` / `send_object` / `set_object_references` paths in `zune-mtp/src/session.rs` already split header + payload correctly — reuse them.
- **`SendObjectPropList` returns `0x0000` "unparseable input" on v1.4 when the proplist payload is malformed** (per `MEMORY/project_v14_mtp_fuzz_baseline`). If the playlist call returns `0x0000`, suspect proplist serialisation — particularly UTF-16LE NUL-termination on string props — before suspecting the device.
- **`SendObject(&[])` payload shape:** libmtp settled on `ptp_sendobject(params, NULL, 0)` (an empty payload, not a one-byte `\0\0`). Mirror that. Our `Session::send_object` should accept `&[]` without dropping to a wrong wire shape — verify the data container builder handles zero length.
- **No 45 s commit pause expected** on `SetObjectReferences` — it's a DB write, not a flash write. Default 30 s response timeout is fine.
- **Stale handles in `DeviceLibrary` will silently corrupt the playlist.** Filter the queue against `DeviceState::track_set` before calling `SetObjectReferences`, or re-resolve handles immediately before push. Same pattern we already use for the sync queue.
- **`MoveObject (0x1019)` is supported on v1.4** as a fallback if `parent=0` is rejected: create at music root, then `MoveObject`.
- **Update-in-place > delete-and-recreate.** libmtp's `update_abstract_list` re-runs `SetObjectReferences` against the existing handle (L7987). For our `import_playlist`'s "replaced atomically" contract: locate the existing `0xBA05` whose `Name` matches, call `SetObjectReferences` with the new list, optionally `SetObjectPropValue (0x9804)` on `dc44 Name` to rename. No `DeleteObject` needed in the happy path. The folder-delete cascade gotcha from `findings.md` does not apply — a playlist is a leaf, not an `ASSOCIATION_FORMAT`.

## What to probe first on real hardware

Run in order against a connected Zune 30 (3.0+ preferred; v1.4 should work with smaller prop set). Suggest a hidden `cargo run -- playlist-probe` subcommand or `#[ignore]` test gated on `ZYTUNES_PROBE_DEVICE=1`.

1. **`get_object_props_supported(0xBA05)`.** Success: non-empty Vec including `0xDC44` (Name) and `0xDC07` (ObjectFileName). Failure `0x200B InvalidObjectFormatCode` → device rejects playlists, bail.
2. **Empty playlist at storage root.** `SendObjectPropList(storage=<music>, parent=0, format=0xBA05, size=0, props=[ObjectFileName="zytunes-probe.zpl", Name="zytunes-probe"])`. Success: `0x2001 OK` + new handle. `0x2009 InvalidParent` → retry with `parent=<default_music_folder>`. `0x2014 InvalidObjectPropFormat` → drop the unsupported prop. `0x0000` → proplist serialisation bug.
3. **`SendObject(&[])`.** Success: `0x2001 OK`. If device hangs / returns `0x2007 IncompleteTransfer`, try single `\0` byte (libmtp's old behaviour).
4. **`SetObjectReferences(new_handle, &[track_a, track_b])`** with two existing handles from `DeviceLibrary`. Success: `0x2001 OK`. Disconnect + verify "zytunes-probe" appears in the Playlists UI with the two tracks in order.
5. **Round-trip.** Reconnect, `GetObjectReferences(new_handle)` returns the same handles in the same order, then `DeleteObject(new_handle)` to clean up (do NOT delete member tracks — references, not ownership).

All five pass → wire `import_playlist` for real, replace stub at `src/mtp/native.rs:1483`.

## Open questions

- **Does `parent=0` surface in the device UI** or is a named "Playlists" folder required? Probe step 2 + UI inspection answers this.
- **Does fw 3.x advertise more writable `0xBA05` props than v1.4?** (`AlbumArtist`, `Composer`, `Genre`?) Cheap probe: `get_object_props_supported(0xBA05)` on a 3.x unit.
- **Do v1.4 quirk gates apply to playlists?** If `SendObjectPropList(format=0xBA05)` returns `0x0000` on v1.4 specifically, fall back to the `SendObjectInfo` branch.
- **Does the Zune persist playlist order across reboots?** Object-reference order should be DB-persisted; verify with probe step 5 + a power cycle.
- **Is there a Zune vendor "create playlist" op the desktop client uses?** Nothing in v1.4's ops list maps to it (`MEMORY/project_v14_vendor_ops_survey`). One passive USB capture against the official Zune software would settle it.
- **Does the `.zpl` body matter at all on-device?** Hypothesis: no, references authoritative. Confirm by writing a 4-byte garbage body and checking the playlist still renders. If true, we can ship a portable real ZPL later without device-parser risk.
