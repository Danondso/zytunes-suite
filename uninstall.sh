#!/bin/bash
set -e

INSTALL_DIR="/usr/local/bin"
removed=0

for bin in zytunes zytunes-tui; do
    if [ -f "$INSTALL_DIR/$bin" ]; then
        echo "Removing $INSTALL_DIR/$bin..."
        sudo rm "$INSTALL_DIR/$bin"
        removed=1
    fi
done

if [ "$removed" -eq 0 ]; then
    echo "zytunes is not installed at $INSTALL_DIR"
else
    echo "Done. zytunes has been uninstalled."
fi
