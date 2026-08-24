#!/bin/bash
set -e

# Homebrew's .pc files embed version-pinned Cellar paths
# (e.g. -L/opt/homebrew/Cellar/libusb/1.0.29/lib). Cargo caches each -sys
# crate's build-script output and only re-runs it when a declared input
# changes — and `brew upgrade libusb` changes nothing cargo tracks. The stale
# -L path gets replayed to the linker, which then fails with
# "library 'usb-1.0' not found". Linux keeps these libs at stable paths
# (/usr/lib/...), so it never hits this.
#
# Detect build-script output referencing link-search dirs that no longer exist
# and clean just those packages, forcing a fresh pkg-config resolution.
clean_stale_native_deps() {
    [ -d target ] || return 0

    local stale
    stale=$(
        grep -ho 'link-search=native=[^ ]*' target/*/build/*/output 2>/dev/null |
            sed 's/^link-search=native=//' |
            while read -r dir; do
                [ -d "$dir" ] && continue
                # Map the output path back to a package name by stripping the
                # trailing -<hash> from its build dir.
                grep -l "link-search=native=$dir" target/*/build/*/output 2>/dev/null |
                    sed -E 's|.*/build/(.*)-[0-9a-f]+/output$|\1|'
            done | sort -u
    )

    [ -n "$stale" ] || return 0

    echo "Stale native library paths detected (Homebrew upgrade?). Cleaning:"
    while read -r pkg; do
        [ -n "$pkg" ] || continue
        echo "  - $pkg"
        # `cargo clean -p` is a no-op for build-script artifacts (it reports
        # "Removed 0 files"), so drop the build output and fingerprint dirs
        # directly. That forces cargo to re-run the build script, which
        # re-queries pkg-config and picks up the current Cellar path.
        rm -rf target/*/build/"$pkg"-* target/*/.fingerprint/"$pkg"-*
    done <<<"$stale"
}

clean_stale_native_deps

echo "Building zytunes (release)..."
# --workspace: default-members is just `app`, so a bare `cargo build`
# skips zytunes-stream (zytunes-serve) and the install would fail at
# the binary check below.
cargo build --release --workspace

INSTALL_DIR="/usr/local/bin"
CLI_BINARY="target/release/zytunes"
TUI_BINARY="target/release/zytunes-tui"
SERVE_BINARY="target/release/zytunes-serve"

if [ ! -f "$CLI_BINARY" ]; then
    echo "Error: build failed — $CLI_BINARY not found"
    exit 1
fi

if [ ! -f "$TUI_BINARY" ]; then
    echo "Error: build failed — $TUI_BINARY not found"
    exit 1
fi

if [ ! -f "$SERVE_BINARY" ]; then
    echo "Error: build failed — $SERVE_BINARY not found"
    exit 1
fi

echo "Installing to $INSTALL_DIR..."
sudo cp "$CLI_BINARY" "$INSTALL_DIR/zytunes"
sudo cp "$TUI_BINARY" "$INSTALL_DIR/zytunes-tui"
sudo cp "$SERVE_BINARY" "$INSTALL_DIR/zytunes-serve"
sudo chmod 755 "$INSTALL_DIR/zytunes" "$INSTALL_DIR/zytunes-tui" "$INSTALL_DIR/zytunes-serve"

echo "Done."
echo "  zytunes        — CLI (run 'zytunes help')"
echo "  zytunes-tui    — Interactive TUI"
echo "  zytunes-serve  — Library HTTP streaming server"
