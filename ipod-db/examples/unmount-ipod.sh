#!/bin/bash
# Unmount helper for iPod Classic testing.
# Cleans up all mounts on the mountpoint.
#
# Usage:
#   sudo ./ipod-db/examples/unmount-ipod.sh

set -e

MOUNTPOINT="/mnt/ipod-classic"

while mountpoint -q "$MOUNTPOINT" 2>/dev/null; do
    echo "Unmounting $MOUNTPOINT..."
    umount "$MOUNTPOINT" 2>/dev/null || break
done

echo "Done. Safe to disconnect."
