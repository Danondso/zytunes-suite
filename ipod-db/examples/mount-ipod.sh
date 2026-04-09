#!/bin/bash
# Mount helper for iPod Classic testing.
# Cleans up stale mounts, finds the current device, mounts cleanly.
#
# Usage:
#   sudo ./ipod-db/examples/mount-ipod.sh [ro|rw]
#   Default: ro (read-only)

set -e

MOUNTPOINT="/mnt/ipod-classic"
MODE="${1:-ro}"
LABEL="GOLDEN BOI"

# 1. Unmount all stale mounts on this mountpoint.
while mountpoint -q "$MOUNTPOINT" 2>/dev/null; do
    echo "Unmounting stale mount on $MOUNTPOINT..."
    umount "$MOUNTPOINT" 2>/dev/null || break
done

# 2. Find the current device by label.
DEV=$(blkid -o device -t LABEL="$LABEL" 2>/dev/null | head -1)

if [ -z "$DEV" ]; then
    echo "ERROR: No device with label '$LABEL' found."
    echo "Is the iPod connected?"
    blkid | grep -i ipod || true
    exit 1
fi

echo "Found $LABEL at $DEV"

# 3. Mount.
mkdir -p "$MOUNTPOINT"
mount -o "$MODE" "$DEV" "$MOUNTPOINT"
echo "Mounted $DEV on $MOUNTPOINT ($MODE)"
ls "$MOUNTPOINT/iPod_Control/iTunes/iTunesDB" 2>/dev/null && echo "iTunesDB: OK" || echo "WARNING: no iTunesDB found"
