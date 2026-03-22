#!/bin/bash
set -e

echo "Building zytunes (release)..."
cargo build --release

INSTALL_DIR="/usr/local/bin"
CLI_BINARY="target/release/zytunes"
TUI_BINARY="target/release/zytunes-tui"

if [ ! -f "$CLI_BINARY" ]; then
    echo "Error: build failed — $CLI_BINARY not found"
    exit 1
fi

if [ ! -f "$TUI_BINARY" ]; then
    echo "Error: build failed — $TUI_BINARY not found"
    exit 1
fi

echo "Installing to $INSTALL_DIR..."
sudo cp "$CLI_BINARY" "$INSTALL_DIR/zytunes"
sudo cp "$TUI_BINARY" "$INSTALL_DIR/zytunes-tui"
sudo chmod 755 "$INSTALL_DIR/zytunes" "$INSTALL_DIR/zytunes-tui"

echo "Done."
echo "  zytunes      — CLI (run 'zytunes help')"
echo "  zytunes-tui  — Interactive TUI"
