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

usage() {
    cat <<'EOF'
Usage: ./install.sh [options]

Always builds and installs:
  zytunes        CLI
  zytunes-tui    Interactive TUI

Optional pieces:
  --setup        First-time wizard: copy config.toml.example to
                 ~/.config/zytunes/config.toml (backs up any existing
                 file) and uncomment music_dir only. Default ~/Music, or
                 xdg-user-dir MUSIC when that tool is on PATH.
  --serve        Also build and install zytunes-serve (LAN streaming)
  --all          Install every optional piece (currently just --serve)
  -h, --help     Show this help

Examples:
  ./install.sh                  # CLI + TUI
  ./install.sh --setup          # write a new config, then install CLI + TUI
  ./install.sh --setup --serve  # new config + streaming server
EOF
}

toml_quote() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '"%s"' "$s"
}

# Absolute path even if the target does not exist yet (the wizard mkdir -p's
# afterwards). GNU realpath -m does this in one shot; BSD realpath (macOS)
# has no -m and treats unknown flags as fatal.
expand_path() {
    local p="$1"
    p="${p/#\~/$HOME}"

    if command -v realpath >/dev/null 2>&1 && realpath -m / >/dev/null 2>&1; then
        realpath -m "$p"
        return
    fi

    case "$p" in
        /*) ;;
        *) p="${PWD}/${p}" ;;
    esac

    # Walk up to the longest prefix that exists, canonicalize that, then
    # append the missing tail.
    local prefix="$p" tail="" base
    while [ -n "$prefix" ] && [ "$prefix" != "/" ] && [ ! -e "$prefix" ]; do
        base=$(basename "$prefix")
        prefix=$(dirname "$prefix")
        if [ -n "$tail" ]; then
            tail="${base}/${tail}"
        else
            tail="$base"
        fi
    done

    if [ -e "$prefix" ] && command -v realpath >/dev/null 2>&1; then
        prefix=$(realpath -q "$prefix")
    fi
    if [ -n "$tail" ]; then
        printf '%s/%s\n' "$prefix" "$tail"
    else
        printf '%s\n' "$prefix"
    fi
}

# XDG user dir when the helper exists and returns a real folder (not $HOME).
xdg_user_dir() {
    local key="$1" d
    command -v xdg-user-dir >/dev/null 2>&1 || return 1
    d=$(xdg-user-dir "$key" 2>/dev/null) || return 1
    [ -n "$d" ] && [ "$d" != "$HOME" ] || return 1
    printf '%s\n' "$d"
}

# Prefer xdg-user-dir, then the first candidate that already exists, else the
# first candidate. Extra candidates cover macOS Movies vs Linux Videos.
default_user_dir() {
    local xdg_key="$1"
    shift
    local d c
    if d=$(xdg_user_dir "$xdg_key"); then
        printf '%s\n' "$d"
        return
    fi
    for c in "$@"; do
        if [ -d "$c" ]; then
            printf '%s\n' "$c"
            return
        fi
    done
    printf '%s\n' "$1"
}

prompt_path() {
    local label="$1"
    local default="$2"
    local required="$3"
    local reply
    if [ -n "$default" ]; then
        read -r -p "$label [$default]: " reply || exit 1
        reply="${reply:-$default}"
    else
        read -r -p "$label (optional, Enter to skip): " reply || exit 1
    fi
    reply="${reply#"${reply%%[![:space:]]*}"}"
    reply="${reply%"${reply##*[![:space:]]}"}"
    # "-" skips an optional path even when a default is shown.
    if [ -z "$reply" ] || [ "$reply" = "-" ]; then
        if [ "$required" = "1" ]; then
            echo "This path is required." >&2
            exit 1
        fi
        return 0
    fi
    expand_path "$reply"
}

write_new_config() {
    local dest="$1" music_dir="$2" example="$3"
    local quoted
    quoted=$(toml_quote "$music_dir")

    mkdir -p "$(dirname "$dest")"
    # Copy the example and uncomment only music_dir. Everything else
    # (photo/video, cache, stems, stream token, …) stays commented.
    awk -v val="$quoted" '
        /^# music_dir = / { print "music_dir = " val; next }
        { print }
    ' "$example" >"$dest"
    grep -q '^music_dir = ' "$dest" || {
        echo "Error: config.toml.example has no commented music_dir line to uncomment." >&2
        exit 1
    }
}

run_setup() {
    if [ ! -t 0 ]; then
        echo "Error: --setup needs an interactive terminal." >&2
        exit 1
    fi

    local script_dir example config_dir config_path default_music music_dir
    script_dir=$(cd "$(dirname "$0")" && pwd)
    example="${script_dir}/config.toml.example"
    if [ ! -f "$example" ]; then
        echo "Error: config.toml.example not found next to install.sh (${example})" >&2
        exit 1
    fi

    config_dir="${HOME}/.config/zytunes"
    config_path="${config_dir}/config.toml"
    default_music=$(default_user_dir MUSIC "${HOME}/Music")

    echo
    echo "First-time setup — copies config.toml.example to ${config_path}"
    echo "Only music_dir is uncommented; uncomment other keys as needed."
    echo "Press Enter to accept the default in [brackets]."
    echo

    music_dir=$(prompt_path "Music library" "$default_music" 1)
    mkdir -p "$music_dir"

    if [ -f "$config_path" ]; then
        local bak="${config_path}.bak"
        echo "Existing config found — moving it to ${bak}"
        mv "$config_path" "$bak"
    fi

    write_new_config "$config_path" "$music_dir" "$example"
    echo "Wrote ${config_path}"
    echo
}

INSTALL_SERVE=0
RUN_SETUP=0
for arg in "$@"; do
    case "$arg" in
        --setup)
            RUN_SETUP=1
            ;;
        --serve | --all)
            INSTALL_SERVE=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $arg" >&2
            echo >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [ "$RUN_SETUP" -eq 1 ]; then
    run_setup
fi

clean_stale_native_deps

INSTALL_DIR="/usr/local/bin"
CLI_BINARY="target/release/zytunes"
TUI_BINARY="target/release/zytunes-tui"
SERVE_BINARY="target/release/zytunes-serve"

# default-members is just `app` (CLI + TUI). zytunes-serve lives in
# zytunes-stream and is only built when requested.
if [ "$INSTALL_SERVE" -eq 1 ]; then
    echo "Building zytunes (release, CLI + TUI + serve)..."
    cargo build --release -p zytunes -p zytunes-stream
else
    echo "Building zytunes (release, CLI + TUI)..."
    cargo build --release -p zytunes
fi

if [ ! -f "$CLI_BINARY" ]; then
    echo "Error: build failed — $CLI_BINARY not found"
    exit 1
fi

if [ ! -f "$TUI_BINARY" ]; then
    echo "Error: build failed — $TUI_BINARY not found"
    exit 1
fi

if [ "$INSTALL_SERVE" -eq 1 ] && [ ! -f "$SERVE_BINARY" ]; then
    echo "Error: build failed — $SERVE_BINARY not found"
    exit 1
fi

echo "Installing to $INSTALL_DIR..."
# `sudo cp` leaves the Mach-O owned by root. macOS SIGKILLs a root-owned
# ad-hoc-signed binary that links AppKit (rodio/cpal on the TUI) — zsh
# reports that as "killed". Install as the invoking user and re-sign.
install_bin() {
    local src="$1" dest="$2"
    sudo cp "$src" "$dest"
    sudo chmod 755 "$dest"
    if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
        sudo chown "$SUDO_USER" "$dest"
    fi
    if command -v codesign >/dev/null 2>&1; then
        sudo codesign --force --sign - "$dest"
    fi
}
install_bin "$CLI_BINARY" "$INSTALL_DIR/zytunes"
install_bin "$TUI_BINARY" "$INSTALL_DIR/zytunes-tui"

if [ "$INSTALL_SERVE" -eq 1 ]; then
    install_bin "$SERVE_BINARY" "$INSTALL_DIR/zytunes-serve"
fi

echo "Done."
echo "  zytunes        — CLI (run 'zytunes help')"
echo "  zytunes-tui    — Interactive TUI"
if [ "$INSTALL_SERVE" -eq 1 ]; then
    echo "  zytunes-serve  — Library HTTP streaming server"
else
    echo
    echo "Skipped zytunes-serve. Re-run with --serve to install it."
fi
