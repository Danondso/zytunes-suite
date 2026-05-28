#!/usr/bin/env python3
"""
Walk a directory and report MP4/M4A files whose `moov` atom is positioned
after `mdat` (non-faststart layout). Symphonia's isomp4 demuxer can fail to
decode these even when ffmpeg/QuickTime play them fine.

Usage:
    ./find-non-faststart-m4a.py <directory>
    ./find-non-faststart-m4a.py <directory> --fix    # rewrite in place via ffmpeg -movflags +faststart
"""

import argparse
import struct
import subprocess
import sys
from pathlib import Path

EXTS = {".m4a", ".m4b", ".mp4"}


def atom_order(path: Path) -> tuple[str, ...] | None:
    """Return the sequence of top-level atom types, or None if unreadable."""
    types: list[str] = []
    try:
        with path.open("rb") as f:
            size_total = path.stat().st_size
            offset = 0
            while offset < size_total:
                f.seek(offset)
                header = f.read(8)
                if len(header) < 8:
                    break
                size, atom_type = struct.unpack(">I4s", header)
                try:
                    type_str = atom_type.decode("ascii")
                except UnicodeDecodeError:
                    return None
                types.append(type_str)
                if size == 1:
                    ext = f.read(8)
                    if len(ext) < 8:
                        break
                    size = struct.unpack(">Q", ext)[0]
                elif size == 0:
                    break
                if size < 8:
                    return None
                offset += size
    except OSError:
        return None
    return tuple(types)


def is_non_faststart(types: tuple[str, ...]) -> bool:
    if "moov" not in types or "mdat" not in types:
        return False
    return types.index("moov") > types.index("mdat")


def fix_file(path: Path) -> bool:
    # Keep the original extension on the temp file so ffmpeg can infer the muxer.
    tmp = path.with_name(path.stem + ".faststart-tmp" + path.suffix)
    result = subprocess.run(
        ["ffmpeg", "-y", "-i", str(path), "-c", "copy", "-movflags", "+faststart", str(tmp)],
        capture_output=True,
    )
    if result.returncode != 0:
        if tmp.exists():
            tmp.unlink()
        sys.stderr.write(f"  ffmpeg failed: {result.stderr.decode('utf-8', 'replace').splitlines()[-1] if result.stderr else 'unknown'}\n")
        return False
    tmp.replace(path)
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="Directory to scan")
    parser.add_argument("--fix", action="store_true", help="Rewrite affected files in place via ffmpeg")
    args = parser.parse_args()

    if not args.root.exists():
        sys.stderr.write(f"error: {args.root} does not exist\n")
        return 2

    found = 0
    scanned = 0
    for path in sorted(args.root.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in EXTS:
            continue
        scanned += 1
        types = atom_order(path)
        if types is None:
            continue
        if not is_non_faststart(types):
            continue
        found += 1
        print(path)
        if args.fix:
            if fix_file(path):
                print("  -> fixed")
            else:
                print("  -> FIX FAILED")

    sys.stderr.write(f"\nScanned {scanned} files, {found} non-faststart\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
