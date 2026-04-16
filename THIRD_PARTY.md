# Third-Party Notices

## MTPZ keys

The Zune 30 requires MTPZ authentication keys (`~/.mtpz-data`) to complete
its handshake. These keys are **not distributed** with zytunes. Obtain them
from the [libmtp-zune](https://github.com/kbhomes/libmtp-zune) project, which
reverse-engineered the Zune MTPZ protocol. See `README.md` for setup
instructions.

## Reference sources

No code from the projects below is included in zytunes. They are listed
because their public documentation, reverse-engineering notes, or source
comments informed the implementation of reverse-engineered binary formats in
the `ipod-db` crate. File format constants (header sizes, magic values, field
offsets) and algorithmic descriptions are facts, not copyrightable expression.

- **[libgpod](https://sourceforge.net/projects/gtkpod/)** (LGPL-2.1) —
  `db-artwork-writer.c` documented the ArtworkDB header sizes used in
  `ipod-db/src/artwork/artworkdb.rs`. `itdb_itunesdb.c` documented mhit field
  layouts used in `ipod-db/examples/mhit_field_dump.rs` and sort-index
  behaviour discussed in `ipod-db/SORT_INDEX_FINDINGS.md`.
- **[iPodLinux wiki](https://web.archive.org/web/*/ipodlinux.org)** — mhit
  field documentation cross-referenced against libgpod.
- **[libmtp-zune](https://github.com/kbhomes/libmtp-zune)** — MTPZ protocol
  documentation (`mtpz.md`) informed the handshake implementation in
  `zune-mtp/src/mtpz.rs`. Source of the `.mtpz-data` key file format.

## Trademarks

"Zune" is a trademark of Microsoft Corporation. "iPod" and "iTunes" are
trademarks of Apple Inc. zytunes is an independent interoperability tool
and is not affiliated with, endorsed by, or sponsored by Microsoft or Apple.
All product names are used under nominative fair use for the sole purpose
of describing compatibility with the named devices.
