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
  --setup        First-time wizard: write a new ~/.config/zytunes/config.toml
                 (backs up any existing file) before installing.
                 Defaults: ~/Music, ~/Pictures, ~/Videos (or ~/Movies on
                 macOS), ~/.cache/zytunes, ~/.mtpz-data — or xdg-user-dir
                 when that tool is on PATH.
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

expand_path() {
    local p="$1"
    p="${p/#\~/$HOME}"
    if command -v realpath >/dev/null 2>&1; then
        realpath -m "$p"
    else
        printf '%s\n' "$p"
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
    local dest="$1"
    shift
    local music_dir="$1" cache_dir="$2" stems_dir="$3" photo_dir="$4" video_dir="$5" mtpz_data="$6"

    mkdir -p "$(dirname "$dest")"
    {
        echo "# Written by ./install.sh --setup"
        echo "music_dir = $(toml_quote "$music_dir")"
        if [ -n "$cache_dir" ]; then
            echo "cache_dir = $(toml_quote "$cache_dir")"
        fi
        if [ -n "$photo_dir" ]; then
            echo "photo_dir = $(toml_quote "$photo_dir")"
        fi
        if [ -n "$video_dir" ]; then
            echo "video_dir = $(toml_quote "$video_dir")"
        fi
        if [ -n "$mtpz_data" ]; then
            echo "mtpz_data = $(toml_quote "$mtpz_data")"
        fi
        if [ -n "$stems_dir" ]; then
            echo
            echo "[stems]"
            echo "cache_dir = $(toml_quote "$stems_dir")"
        fi
    } >"$dest"
}

run_setup() {
    if [ ! -t 0 ]; then
        echo "Error: --setup needs an interactive terminal." >&2
        exit 1
    fi

    local config_dir="${HOME}/.config/zytunes"
    local config_path="${config_dir}/config.toml"
    local default_music default_photo default_video default_cache default_stems default_mtpz
    default_music=$(default_user_dir MUSIC "${HOME}/Music")
    default_photo=$(default_user_dir PICTURES "${HOME}/Pictures")
    if [ "$(uname -s)" = Darwin ]; then
        default_video=$(default_user_dir VIDEOS "${HOME}/Movies" "${HOME}/Videos")
    else
        default_video=$(default_user_dir VIDEOS "${HOME}/Videos" "${HOME}/Movies")
    fi
    if [ -n "${XDG_CACHE_HOME:-}" ]; then
        default_cache="${XDG_CACHE_HOME}/zytunes"
    else
        default_cache="${HOME}/.cache/zytunes"
    fi
    default_stems="${default_cache}/stems"
    default_mtpz="${HOME}/.mtpz-data"

    echo
    echo "First-time setup — this writes a new ${config_path}"
    echo "Press Enter to accept the default in [brackets]."
    echo "Type - and Enter to skip an optional path."
    echo

    local music_dir cache_dir stems_dir photo_dir video_dir mtpz_data
    music_dir=$(prompt_path "Music library" "$default_music" 1)
    cache_dir=$(prompt_path "Shared cache (art, models, play history)" "$default_cache" 0)
    local stems_default="$default_stems"
    if [ -n "$cache_dir" ]; then
        stems_default="${cache_dir}/stems"
    fi
    stems_dir=$(prompt_path "Stem cache" "$stems_default" 0)
    photo_dir=$(prompt_path "Photo sync folder" "$default_photo" 0)
    video_dir=$(prompt_path "Video sync folder" "$default_video" 0)
    mtpz_data=$(prompt_path "MTPZ handshake file (Zune)" "$default_mtpz" 0)

    # Only persist cache/stems/mtpz when they differ from built-in defaults
    # (or when the shared cache moved, which relocates the default stem dir).
    if [ "$cache_dir" = "$default_cache" ]; then
        cache_dir=""
    fi
    if [ -z "$cache_dir" ] && [ "$stems_dir" = "$default_stems" ]; then
        stems_dir=""
    fi
    if [ -n "$cache_dir" ] && [ "$stems_dir" = "${cache_dir}/stems" ]; then
        stems_dir=""
    fi
    if [ "$mtpz_data" = "$default_mtpz" ]; then
        mtpz_data=""
    fi

    mkdir -p "$music_dir"
    [ -n "$cache_dir" ] && mkdir -p "$cache_dir"
    [ -n "$stems_dir" ] && mkdir -p "$stems_dir"
    [ -n "$photo_dir" ] && mkdir -p "$photo_dir"
    [ -n "$video_dir" ] && mkdir -p "$video_dir"
    if [ -n "$mtpz_data" ]; then
        mkdir -p "$(dirname "$mtpz_data")"
    fi

    if [ -f "$config_path" ]; then
        local bak="${config_path}.bak"
        echo "Existing config found — moving it to ${bak}"
        mv "$config_path" "$bak"
    fi

    write_new_config "$config_path" "$music_dir" "$cache_dir" "$stems_dir" "$photo_dir" "$video_dir" "$mtpz_data"
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
