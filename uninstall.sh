#!/bin/bash
set -e

INSTALL_PATH="/usr/local/bin/zytunes"

if [ ! -f "$INSTALL_PATH" ]; then
    echo "zytunes is not installed at $INSTALL_PATH"
    exit 0
fi

echo "Removing $INSTALL_PATH..."
sudo rm "$INSTALL_PATH"
echo "Done. zytunes has been uninstalled."
