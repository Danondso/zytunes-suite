#!/bin/bash
# Mount helper for iPod Classic testing.
# Cleans up stale mounts, finds the current device, mounts cleanly.
#
# Usage:
#   sudo ./ipod-db/examples/mount-ipod.sh [ro|rw]
#   Default: ro (read-only)
#
# Override the label with IPOD_LABEL=<label> if the device has been renamed
# (e.g. IPOD_LABEL="GOLDEN BOI").

set -e

MOUNTPOINT="/mnt/ipod-classic"
MODE="${1:-ro}"
LABEL="${IPOD_LABEL:-iPod}"

# 1. Unmount all stale mounts on this mountpoint.
while mountpoint -q "$MOUNTPOINT" 2>/dev/null; do
    echo "Unmounting stale mount on $MOUNTPOINT..."
    umount "$MOUNTPOINT" 2>/dev/null || break
done

# 2. Find the current device — prefer the configured label, then fall back to
# the first hfsplus partition on an Apple iPod block device (factory-default
# label is "iPod", but firmware restores and iTunes rename can change it).
DEV=$(blkid -o device -t LABEL="$LABEL" 2>/dev/null | head -1)

if [ -z "$DEV" ]; then
    DEV=$(lsblk -rno NAME,VENDOR,MODEL,FSTYPE | awk '$2=="Apple" && $3=="iPod" && $4=="hfsplus" { print "/dev/"$1; exit }')
fi

if [ -z "$DEV" ]; then
    echo "ERROR: No iPod found (label '$LABEL' not present, no Apple iPod hfsplus partition)."
    echo "Is the iPod connected?"
    blkid | grep -iE "ipod|hfsplus" || true
    exit 1
fi

echo "Found iPod at $DEV (label lookup: '$LABEL')"

# 3. Mount.
mkdir -p "$MOUNTPOINT"
mount -o "$MODE" "$DEV" "$MOUNTPOINT"
echo "Mounted $DEV on $MOUNTPOINT ($MODE)"
ls "$MOUNTPOINT/iPod_Control/iTunes/iTunesDB" 2>/dev/null && echo "iTunesDB: OK" || echo "WARNING: no iTunesDB found"
