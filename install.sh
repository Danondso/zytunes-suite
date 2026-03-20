#!/bin/bash
set -e

echo "Building zytunes (release)..."
cargo build --release

INSTALL_DIR="/usr/local/bin"
BINARY="target/release/zytunes"

if [ ! -f "$BINARY" ]; then
    echo "Error: build failed — $BINARY not found"
    exit 1
fi

echo "Installing to $INSTALL_DIR/zytunes..."
sudo cp "$BINARY" "$INSTALL_DIR/zytunes"
sudo chmod 755 "$INSTALL_DIR/zytunes"

echo "Done. Run 'zytunes help' to get started."
