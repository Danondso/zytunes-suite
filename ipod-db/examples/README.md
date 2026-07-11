# ipod-db examples

These are **manual bring-up and forensics tools**, not test coverage or API
demos. They were written during the from-scratch iPod Classic port (see the
workspace `DEBUG.md` for the full investigation) to answer questions like
"which dataset does the firmware reject?" and "does our hash58 match
iTunes'?" — most need a real mounted iPod and several deliberately write to
it. The crate's actual test coverage lives in `src/` unit tests.

Run any of them with:

```bash
cargo run -p ipod-db --example <name> --release -- <args>
```

Each file's `//!` header documents its exact usage and safety notes.

## Read-only diagnostics (safe)

| Example | Purpose |
|---|---|
| `validate_ipod` | Parse a real iTunesDB from a connected iPod and exercise the parser |
| `verify_hash` | Check our hash58 implementation against an iTunes-signed DB |
| `itdb_structure` | Walk and compare chunk headers between two iTunesDB files |
| `mhit_field_dump` | Dump non-zero mhit extended-region fields from a reference DB |
| `compare_artworkdb` | Byte-diff two ArtworkDB files |
| `roundtrip_test` | Offline parse → serialize → re-sign → compare (no device writes) |

## Device-writing tests (use on a sacrificial/backed-up iPod)

| Example | Purpose |
|---|---|
| `write_test` | Parse a real DB, serialize with our writer, write back |
| `write_roundtrip` | Round-trip the DB back to the device for firmware validation |
| `import_track` | Copy one audio file into an F-dir and update iTunesDB |
| `artwork_test` | Add artwork for existing tracks; write ArtworkDB + `.ithmb` |
| `patch_artwork_inplace` | Patch artwork flags in-place on the original binary |
| `sign_db` | Re-sign an iTunesDB in-place with hash58 |
| `hash_only_test` | Isolation: original DB with only the hash recomputed |
| `isolate_test` | Isolation: swap one dataset at a time to find what firmware rejects |
| `dataset_swap` | Build per-dataset swap candidates in `/tmp/swap-candidates/` |

## Helper scripts

`mount-ipod.sh` / `unmount-ipod.sh` mount and cleanly eject the iPod's USB
mass-storage volume so the examples can reach `iPod_Control/`.
