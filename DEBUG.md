# iPod Sync Debug — libgpod Investigation

## Current Status

**Working:**
- Read existing iTunesDB → parse to `IpodDatabase`
- Pure roundtrip (parse → serialize → sign) — all existing tracks play
- File copy to `/iPod_Control/Music/F00..F49/`
- Raw blob replay preserves iTunes-written mhit/mhod bytes verbatim
- hash58 signing with FirewireGuid

**Broken:**
- New track added via any method (from-scratch mhit via libgpod port,
  OR cloned raw blob from working track with metadata patched) does not
  appear on device OR appears but skips immediately on play.

**Proven:**
- The source file plays fine when swapped into a working track's
  F-dir slot (DB untouched). So the audio file is iPod-compatible.
- Pure roundtrip with `hash58` re-sign works — all 3883 existing tracks play.
- A byte-identical clone of a working track's mhit blob, with only metadata
  strings and per-track numerics patched, added to master playlist mhips,
  sort indexes, and letter indexes — still doesn't appear on device.

This means something OUTSIDE iTunesDB is needed OR our approach to track IDs
is fundamentally wrong.

---

## libgpod Investigation Findings

### 1. Track IDs are REASSIGNED on every write (`prepare_itdb_for_write`)

Location: `itdb_itunesdb.c:5900-5921`

```c
fexp->next_id = FIRST_IPOD_ID;  // = 52
for (gl=itdb->tracks; gl; gl=gl->next) {
    track->id = fexp->next_id++;
    ...
}
```

**libgpod does NOT preserve existing track IDs.** Every write reassigns them
sequentially starting from 52. Album/artist/composer IDs get the same
treatment via hash tables.

This means:
- Every mhip in every playlist gets the new track IDs
- Every album_id/artist_id cross-reference is rebuilt
- Track IDs are DENSE (52, 53, 54, ...) with no gaps

**Our implementation preserves track IDs from parse** (via raw blob replay).
When we add a new track we assign `max(existing)+1 = 398548`, which creates
a huge gap. The firmware may validate that track IDs are contiguous.

**Hypothesis:** The iPod Classic firmware requires dense track IDs. Our
sparse ID scheme (534, 537, 539, ..., 398547, 398548) might cause the
firmware to stop iterating at the first large gap.

### 2. Dataset order is 1, 3, 2, 4, 8, 6, 10, 5 (done)

Types 6, 8, 10 are required. We now write all 8.

### 3. mhbd version is hardcoded to 0x30 (iTunes 9.2)

Location: `itdb_itunesdb.c:3853`

```c
fexp->itdb->version = 0x30;  // OVERWRITES caller's version
```

libgpod ALWAYS writes `db_version = 0x30`. Our golden DB has `db_version = 0x75`
(iTunes 10.x). The iPod Classic reads both. Probably not the issue.

### 4. db_type is 1 for iPod Classic, 2 for iPhone/Nano 5G

Location: `itdb_itunesdb.c:3821-3828`

Controlled by `itdb_device_supports_compressed_itunesdb`.
- iPod Classic 1/2/3: `db_type = 1`
- iPhone/Nano 5G+: `db_type = 2`

We write `db_type = 1` (correct). Golden shows `db_type = 0` (strange — but
that's what the parsed `db_type` field says at mhbd+12). libgpod writes 1.

### 5. Checksum: hash58 is correct for iPod Classic

Location: `itdb_device.c:1938-1944`

CLASSIC_1/2/3 → `ITDB_CHECKSUM_HASH58`. We use hash58. Correct.

### 6. ArtworkDB is OPTIONAL for tracks without art

Location: `db-artwork-writer.c:1035`

Only tracks with `itdb_track_has_thumbnails(track) == TRUE` get mhii entries.
Tracks without artwork are NOT referenced in ArtworkDB. So ArtworkDB is NOT
gating track visibility.

### 7. Extras.itdb / Dynamic.itdb / Genius.itdb / Locations.itdb

These SQLite databases are ONLY generated for:
- iPod Nano 5G+
- iPod Touch 1-4
- iPhone 1-4
- iPad 1

`itdb_device_supports_sqlite_db` returns FALSE for CLASSIC_1/2/3.
So these files are not needed on our iPod.

### 8. Play Counts file gets DELETED after iTunesDB write

Location: `itdb_itunesdb.c:6135` → `playcounts_reset`

libgpod deletes Play Counts, iTunesStats, PlayCounts.plist after successful
write. Not related to track visibility.

### 9. mhbd has optional hash72 (+0x72) and hashAB (+0xab) bytes

Location: `itdb_itunesdb.c:3891-3902`

For Classic (HASH58): all these byte ranges are zero. Consistent with golden
and our output.

---

## Open Questions

1. **Does track ID density matter?** Our new track gets ID 398548 (gap of
   ~400k from existing). libgpod always reassigns starting from 52. Test:
   serialize with sequential IDs starting from 52 and see if our track
   appears.

2. **What's at mhbd +0x50/+0x54?** libgpod calls them `unk_0x50`/`unk_0x54`.
   Golden values: needs inspection. These are device-specific constants
   that we might need to preserve.

3. **iTunesPrefs contents** — we never modify it. Should we?

4. **Extras.itdb on iPod Classic** — Classic doesn't use SQLite per libgpod,
   but Classic DOES have an `Extras.itdb` file on this particular iPod.
   Maybe iTunes still writes a non-SQLite format and the firmware reads it?

---

## Next Experiments

**Priority 1: Reassign ALL track IDs sequentially before serialize.**
This is what libgpod does. We currently preserve the original IDs.
Test:
1. Parse golden DB (existing tracks keep their raw blobs)
2. On serialize, patch all mhit track_ids to sequential starting from 52
3. Also patch all mhip track_id refs to match
4. If new track now shows up and plays, track ID density was the issue

**Priority 2: Preserve mhbd unknown fields (+0x50, +0x54, +0x22, +0xa4)**
These vary per device. The golden mhbd header has values we should replay
verbatim when available (we already do partial preservation).

**Priority 3: Check Extras.itdb contents**
Binary dump of the file to see if it has a track list or counter we'd need
to update.

---

## Timeline

- 2026-04-16: Investigation started. Documented libgpod flow.
  Key finding: libgpod reassigns track IDs on every write (we don't).
- 2026-04-16 (later): Ran track ID reassignment experiment. Still skips.
  Added total_tracks, total_discs fields. Fixed id_0x24 at +0x124 (was
  writing db_id from mhbd+0x18, correct is id_0x24 from mhbd+0x24). Made
  all mhit headers uniform 624 bytes. Still skips.

## Experiments Run (all FAILED to make new track playable)

1. From-scratch mhit with libgpod-ported layout ❌
2. 8-dataset output (types 1/3/2/4/8/6/10/5) ❌
3. Clone working track's raw blob, patch metadata ❌
4. Clone + add to sort indexes + add to letter indexes ❌
5. Track ID reassignment (dense 52..N) ❌
6. Uniform 624-byte headers ❌
7. id_0x24 from mhbd+0x24 (not db_id from mhbd+0x18) ❌
8. total_tracks, total_discs populated ❌

## Remaining differences (new mhit vs golden working AAC mhit)

These are still unfixed but may not matter:

- `+0x080 artwork_size`: 0 (no artwork) vs real bytes (has artwork) — expected
- `+0x0B8 pregap` / `+0x0BC samplecount` / `+0x0C8 postgap` — gapless info
  we don't extract from the file
- `+0x0CC unk204`: libgpod writes 0 for AAC, golden has `0x02000003`
- `+0x134/0x138` mystery: libgpod `0x808080808080`, golden `0x00008080_03038080`
- `+0x160 mhii_link`: 0 (no ArtworkDB entry) vs ArtworkDB id
- `+0x1E0 artist_id`: 0 vs `0x218`
- `+0x1F4 composer_id`: 0
- `+0x20C` mystery byte

## Play Counts Hypothesis DISPROVEN (2026-04-16)

Play Counts file has exactly 3883 entries (matching track count, one per
track, 28 bytes each). Adding a track creates a count mismatch (DB says
3884, Play Counts says 3883). libgpod deletes Play Counts after every
write so the firmware regenerates it.

Tested: delete Play Counts after import. Still skips.
Conclusion: Play Counts count mismatch is NOT the filter.

## ArtworkDB Hypothesis DISPROVEN (2026-04-16)

Inspected the golden ArtworkDB (7,276 bytes) and found only **8 mhii
entries** out of 3,883 tracks. 3,875 tracks play perfectly fine without
any ArtworkDB reference. So the firmware does NOT require an mhii entry
per track. Our new track lacking one cannot be the cause.

This matches libgpod's behavior (skips tracks without thumbnails).

## New Hypothesis: It's Not the Database

We've ported libgpod faithfully for the parts that matter (mhit fields,
mhods, datasets, checksum). We've tried binary cloning from working tracks.
Nothing makes a NEW track play, even though the file itself plays fine when
swapped into a working slot.

Possible explanations left:
1. **Incomplete ArtworkDB** — even for tracks without artwork, maybe the
   firmware requires some entry. libgpod explicitly skips tracks without
   artwork in `db-artwork-writer.c:1035`, but this is the one thing we
   haven't tried.
2. **Extras.itdb has a track manifest** we're not updating
3. **Firmware track limit on this particular iPod** — maybe disk is full
   or some internal counter overflowed
4. **The iPod caches the DB** in a way that survives reboots and our DB
   writes aren't invalidating the cache

## Remaining Leads

### `iTunesControl` is 156MB

File size `156,237,824` bytes. Not documented in libgpod. Could be:
- Pre-built index the firmware uses to avoid scanning F-dirs (if so,
  new files at new paths get ignored)
- Log / manifest of valid content

**Inspect structure of iTunesControl** next — might be a magic-prefixed
binary format, might be a concatenation of track data, etc.

### `Extras.itdb` is a SQLite database

Starts with "SQLite format 3". libgpod says Classic doesn't use SQLite,
but the file exists. Maybe iTunes writes it and the firmware reads it.

### The swap test proved it's NOT the file

Replacing a working track's file (`F31/XXXX.m4a`) with the new
audio showed the iPod playing the new audio under the existing library entry. So:
- The file is iPod-compatible
- The firmware DOES scan and play files at paths referenced by working
  tracks

**But** when we add a NEW track pointing to a NEW F-dir path, it doesn't
work. This suggests the firmware has a "trusted paths" list (possibly
iTunesControl) that doesn't include our new path.

### Test with gtkpod as control

If gtkpod can add a track to this specific iPod, the issue is our code.
If gtkpod fails too, the iPod itself has some state issue.

## Summary of What We've Tried

| Experiment | Result |
|-----------|--------|
| From-scratch mhit, libgpod layout | ❌ Skip |
| 8 datasets (1/3/2/4/8/6/10/5) | ❌ Skip |
| Clone working track's raw blob | ❌ Skip |
| Clone + sort/letter index updates | ❌ Skip |
| Track ID reassignment (dense 52..N) | ❌ Skip |
| Uniform 624-byte mhit headers | ❌ Skip |
| Correct id_0x24 from mhbd+0x24 | ❌ Skip |
| total_tracks, total_discs populated | ❌ Skip |
| Play Counts deletion | ❌ Skip |

Ruled out:
- Sort indexes (cosmetic only per libgpod)
- ArtworkDB (3875/3883 golden tracks have no mhii and play fine)
- File format (swap test — file plays in working track's slot)
- hash58 (pure roundtrip works)

## Final Status

Pure roundtrip: ✅ All existing tracks play
New track sync: ❌ Track appears but skips on playback

The gap is narrow and very specific — we write a bit-perfect database
compared to what a working clone would be, but the firmware still won't
play a track pointing to a new F-dir path. Leading theory: `iTunesControl`
is a path manifest we're not updating.
