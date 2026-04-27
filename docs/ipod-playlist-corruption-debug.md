# iPod Classic playlist write — corruption root-cause debug

> Status: investigation only, no fix applied. Device-side iPod playlist sync is
> still gated behind `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1` (commit `18d5f7c`).
> Captured 2026-04-27.

## Failure surface

`feat: playlists, Discover Weekly recommender, listen log, iPod playlist sync`
(`ba43e1b`) shipped `IpodSession::import_playlist` →
`upsert_playlist` (`src/mtp/ipod_session.rs:431`,`449`) → `IpodDatabase::flush`
→ `itunesdb_write::serialize` → hash58 sign. Users hit visible-on-iPod
corruption (the writeup in `RECOMMENDER_DESIGN.md` lines 698–760 records
"music vanished from the device UI"). The follow-up commit `18d5f7c` did
**not** patch the broken code — it only added the
`ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC` env-var gate at
`src/tui/app.rs:893,1193,3859,3872` so the broken path stays dormant by
default. Re-enabling for testing: `export ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1`
before launching the TUI.

## Method

Two parallel investigations:

1. Static audit of the playlist write path in this repo (`upsert_playlist`,
   `write_mhyp`, `write_mhip`, parser at `parse_mhyp`).
2. Format research against libgpod (`itdb_itunesdb.c` `mk_mhip` /
   `write_playlist` / `mk_long_mhod_id_playlist`) and the ipodlinux wiki
   ITunesDB page, looking up byte-exact field positions.

`ba43e1b` and `18d5f7c` diffs read directly via `git show`.

## Ranked hypotheses

### H1 — mhyp byte layout doesn't match iPod firmware spec **(highest confidence)**

Our `write_mhyp` (`ipod-db/src/itunesdb_write.rs:673-750`) emits a header
that diverges from libgpod's `write_playlist`/`mk_mhyp` and the ipodlinux
wiki layout in two large ways:

| Offset | What firmware expects (libgpod) | What we write |
|--------|---------------------------------|---------------|
| `+4` `header_len` | **108** (`0x6C`) | **184** (`0xB8`) — line 681 |
| `+24` timestamp | u32, mac-epoch | u64 = 0 — line 738 |
| `+28` `playlist_id` (u64) | u64 persistent ID | upper 4 bytes of our zero-timestamp + lower 4 bytes of our playlist_id (we put playlist_id at `+32`) — line 739 |
| `+36` `unk3` u32 | 0 | upper 4 bytes of our playlist_id |
| `+40` string mhod count u16 | hardcoded `1` | upper bytes of our playlist_id (effectively garbage when `playlist_id > 0xFFFF_FFFF`) |
| `+42` podcastflag u16 | 0 | continued garbage |
| `+44` sortorder u32 | 0 | continued garbage |

The cascading effect is most acute at `+40 string mhod count`. Firmware
reads that u16 to know how many string-typed mhods to walk before
encountering mhip records. Our writer always emits exactly one title mhod
(line 688) plus column-info mhods 100 and 102 (lines 693–696). If the
firmware reads anything other than `1` at `+40`, it walks a wrong number
of mhods and aligns on the wrong byte for the first mhip — the rest of
the parse desyncs and the playlist (or, depending on parser strictness,
the entire iTunesDB) is rejected. For the master playlist with
`playlist_id = 1` the upper bytes are zero so this is benign; for any
later playlist where the persistent ID doesn't fit in 32 bits, the parser
goes off the rails.

In addition, libgpod ships a hardcoded **648-byte type-100 prefs mhod**
(`mk_long_mhod_id_playlist` in `itdb_itunesdb.c:4990`) on every playlist
(`itunesdb.c:5499`). Without it, iTunes silently drops tracks (libgpod
comment lines 4982–4988) — the iPod Classic firmware appears to share
that behavior. We emit a 44-byte type-100 column-info mhod
(`write_column_info_100`) instead, which is structurally different.

**Symptom match:** "music vanished from device UI" matches a database
the firmware silently rejects on mount.

**Validation:**

1. Capture the iTunesDB written by an experimental sync run
   (`cp /mount/iPod_Control/iTunes/iTunesDB /tmp/zy.db` while the device
   is still connected after the failing sync).
2. Run `gtkpod --check /tmp/zy.db` or equivalent libgpod CLI — expect
   format errors flagged on the playlist mhsd dataset.
3. Cross-check against a known-good iTunesDB taken from before the
   first experimental sync: hex-diff `+4..+48` of every `mhyp` chunk.
   Our writes will differ at `+4`, `+24`, `+28`, `+40`.
4. Record the iTunes-emitted mhyp from a real iPod and parse with
   `examples/itdb_structure.rs` — confirm the 108-byte header layout
   end-to-end before patching `write_mhyp`.

### H2 — Hash58 signature signs garbage mhyp bytes (downstream of H1)

`itdb_device_write_checksum` (libgpod) and our `hash::sign` run over the
final iTunesDB byte buffer. Any mhyp layout drift changes the hash —
firmware rejects mismatched signatures silently (no UI error). H1 is the
upstream cause; if we fix H1, H2 disappears. Listed separately so we
remember to **re-validate the signature path after the fix**, not just
the layout.

**Validation:** after fixing H1, take the resulting iTunesDB and confirm
the iPod mounts it without recovery prompts.

### H3 — Master playlist invariants partly preserved, partly not

`upsert_playlist` (`src/mtp/ipod_session.rs:486-508`) correctly skips
index 0 and only mutates `playlists[1..]`, preserving the master at
`playlists[0]`. `IpodDatabase::add_track` at `lib.rs:286-301` pushes new
tracks' dbids into the master. So the *vector positions* are right.

But there are two soft invariants the master also has to satisfy:

- **Master flag at mhyp+20**. We write the `is_master` bool as u32
  (1 or 0) at `itunesdb_write.rs:736`. libgpod writes `pl->type` as u8
  followed by 3 single-byte flags. The byte values for `is_master=true`
  collide (low byte = 1, others = 0), so this is currently benign — but
  it stops being benign if anyone ever sets `flag1`/`flag2`/`flag3`.

- **Master gets sort/letter index mhods (types 52/53)**. We do produce
  these at `itunesdb_write.rs:702-715` for `is_master`, gated on
  `!tracks.is_empty()`. Without them the browse UI can show "no music"
  even when tracks load — the libgpod comment in `write_mhsd_playlists`
  documents this for Classic-class devices. So the path is correct in
  principle; verify on hardware after H1 is fixed.

**Validation:** after H1 fix, confirm the master playlist still shows
all tracks in the browse UI; if it doesn't, audit the type-52/53
mhod payloads for byte-exact correctness against an iTunes-written
reference DB.

### H4 — Parser/writer dbid round-trip (DISMISSED on read)

Earlier hypothesis: the parser stores mhip's `track_id` directly into
`IpodPlaylist.track_ids` and the writer treats those as `dbid`s, so the
round-trip silently zeros every playlist. **Read of the actual code
disproves this.** Parser at `ipod-db/src/itunesdb.rs:482-493` translates
mhip `track_id` → `dbid` via the track list **before** populating
`playlist.track_ids`. Writer at `itunesdb_write.rs:720-721` correctly
does the inverse lookup. Round-trip is sound at the ID-handling layer.

Ruling this out matters because the first investigation pass cited it as
the smoking gun; if a future agent reads only the parser snippet at
lines 380–384 they'll repeat the mistake. The translation step further
down is where the truth lives.

### H5 — Persistent playlist_id collision with preserved smart playlist

`itunesdb_write.rs:953` (caller of `write_mhyp`) assigns persistent
`playlist_id = (i + 1) as u64` from enumeration order. We preserve a
prior smart-playlist blob via `raw_smart_playlists` (`itunesdb.rs:466`).
If iTunes' smart playlist references a persistent ID via that blob, and
we re-enumerate user playlists with overlapping IDs, the firmware could
follow the smart playlist into the wrong target.

Low confidence — we'd need to dump `raw_smart_playlists` and parse the
embedded persistent IDs to know if there's a real collision risk. Treat
as a known unknown until H1 is fixed and we re-test.

**Validation:** after H1, write a known persistent-ID-using smart
playlist from iTunes, sync user playlists from us, then check the smart
playlist still resolves the correct tracks.

## Determinism

H1 corrupts every mhyp the writer emits. The damage is **deterministic**
on the byte layout and **not load-bearing on input** — any non-empty
playlist write produces the same broken mhyp shape. The reason it didn't
trip every CI test is that no test parses the produced bytes through a
real iPod firmware (or libgpod's checker) — `upsert_playlist` tests at
`src/mtp/ipod_session.rs:579-704` only assert in-memory invariants, and
no test takes the writer's output and runs it through `parse()` on real
data.

This explains why the round-trip tests at `ipod-db/src/itunesdb.rs:589`
and `:662` pass: they parse what they wrote, and the parser is internally
consistent with the writer's bad layout. The iPod firmware isn't, and
that's where the corruption surfaces.

## Smallest fix path

1. Rewrite `write_mhyp` against libgpod's reference layout
   (`itdb_itunesdb.c` `write_playlist` / `mk_mhyp`). Spec from agent
   research:
   - `header_len = 108` at `+4`
   - `type` u8 at `+20`, three single-byte flags at `+21..+23`
   - `timestamp` u32 mac-epoch at `+24`
   - `playlist_id` u64 at `+28` (not `+32`)
   - `unk3 = 0` u32 at `+36`
   - **`string_mhod_count = 1`** u16 at `+40` (the field whose garbage
     value cascades into desync)
   - `podcastflag = 0` u16 at `+42`
   - `sortorder = 0` u32 at `+44`
   - 60 bytes of zero padding to total 108
2. Replace the 44-byte `write_column_info_100` with the libgpod
   648-byte (0x288) `mk_long_mhod_id_playlist` blob. The 28 magic
   constants there are non-optional; copy them byte-for-byte.
3. Add a parse-after-write integration test using a checked-in
   reference iTunesDB byte sample that walks every mhyp/mhip with the
   layout offsets above, so any future drift fails CI without needing
   real hardware.
4. Hardware test on a re-formatted iPod (Windows/FAT) before flipping
   the experimental flag default-on.

Out of scope for this fix path: H3 master flag bit-width and H5
persistent ID collisions. Both are speculative until we have a
post-fix database to inspect.

## File references

- Playlist write entrypoint: `src/mtp/ipod_session.rs:431`
- Database mutation: `src/mtp/ipod_session.rs:449-515`
- Master playlist push on `add_track`: `ipod-db/src/lib.rs:286-301`
- Track ID reassignment: `ipod-db/src/lib.rs:325-338`
- mhyp writer (broken): `ipod-db/src/itunesdb_write.rs:673-750`
- mhip writer (looks correct): `ipod-db/src/itunesdb_write.rs:393-432`
- mhyp parser: `ipod-db/src/itunesdb.rs:347-397`
- Parser dbid translation: `ipod-db/src/itunesdb.rs:482-493`
- Opt-in gate field: `src/tui/app.rs:893,1193,3859,3872`
- Existing playlist tests: `src/mtp/ipod_session.rs:579-704`,
  `ipod-db/src/itunesdb.rs:589,662`
