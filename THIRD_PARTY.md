# Third-Party Notices

## MTPZ handshake credentials

The Zune requires an MTPZ handshake (`~/.mtpz-data`) before a session
can do useful work. Those credentials are **not distributed** with
zytunes — the file is not in this repository, and we will not add it.
See `README.md` for where the process looks. The iPod backend does not
use this file.

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
  `zune-mtp/src/mtpz.rs`.

## Dynamically linked LGPL libraries

zytunes dynamically links against the following LGPL libraries. No LGPL
source is included or modified in the zytunes tree. Per LGPL §6, dynamic
linking imposes no copyleft on the calling application, and users can
substitute their own builds of these libraries by setting `PKG_CONFIG_PATH`
or `LD_LIBRARY_PATH` (Linux) / `DYLD_LIBRARY_PATH` (macOS) at runtime.

- **[libdiscid](https://musicbrainz.org/doc/libdiscid)** (LGPL-2.1+) — used
  by `app/src/cd/drive.rs` via the [`discid`](https://crates.io/crates/discid)
  Rust crate for reading audio-CD tables of contents. Install via
  `brew install libdiscid` (macOS) or `apt install libdiscid-dev` (Debian/
  Ubuntu). Upstream source:
  <https://github.com/metabrainz/libdiscid>.

## External engines (subprocess, user-installed)

Stem-split playback shells out to external engines run as subprocesses.
None of their code is included in or linked into zytunes; they are
installed separately (optionally via zytunes' consented one-time `uv`
managed install) and invoked by path.

- **[demucs](https://github.com/adefossez/demucs)** (MIT) — the default
  stem-separation engine, including the `htdemucs_6s` model weights it
  downloads on first use.
- **[python-audio-separator](https://github.com/nomadkaraoke/python-audio-separator)**
  (MIT) — the engine behind the `hq`/`hq-harmony` recipes; runs Roformer
  checkpoints and demucs models behind one CLI.
- **[uv](https://github.com/astral-sh/uv)** (MIT OR Apache-2.0) — the
  managed-install vehicle. Bootstrapped from Astral's official install
  script only after explicit user consent in the TUI.

The Roformer model checkpoints the `hq` recipes reference (BS-Roformer
`ep_317` by viperx; the Mel-Roformer karaoke model by aufr33/viperx) are
community-trained weights fetched by audio-separator from its model
registry on first separation — zytunes never distributes them, and every
download happens behind the same explicit consent as the engine install.
If a checkpoint's license terms matter for your use, review them at the
model registry before enabling those recipes.

## Trademarks

"Zune" is a trademark of Microsoft Corporation. "iPod" and "iTunes" are
trademarks of Apple Inc. zytunes is an independent interoperability tool
and is not affiliated with, endorsed by, or sponsored by Microsoft or Apple.
All product names are used under nominative fair use for the sole purpose
of describing compatibility with the named devices.
