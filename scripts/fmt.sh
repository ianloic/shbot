#!/usr/bin/env bash
set -e

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)"
BIN_DIR="$REPO_ROOT/.bin"

mkdir -p "$BIN_DIR"

echo "==> Formatting Rust"
cd "$REPO_ROOT/daemon" && cargo fmt

echo "==> Formatting Zsh Scripts"
cd "$REPO_ROOT"
if ! command -v shfmt &> /dev/null; then
    if [ ! -f "$BIN_DIR/shfmt" ]; then
        echo "Installing shfmt to .bin/shfmt..."
        wget -qO "$BIN_DIR/shfmt" "https://github.com/mvdan/sh/releases/download/v3.7.0/shfmt_v3.7.0_linux_amd64"
        chmod +x "$BIN_DIR/shfmt"
    fi
    SHFMT="$BIN_DIR/shfmt"
else
    SHFMT="shfmt"
fi
"$SHFMT" -w -ln bash zsh/*.zsh

echo "==> Formatting Markdown"
if ! command -v dprint &> /dev/null; then
    if [ ! -f "$BIN_DIR/dprint" ]; then
        echo "Installing dprint to .bin/dprint..."
        wget -qO "$BIN_DIR/dprint.zip" "https://github.com/dprint/dprint/releases/download/0.45.0/dprint-x86_64-unknown-linux-gnu.zip"
        unzip -q "$BIN_DIR/dprint.zip" -d "$BIN_DIR/"
        rm "$BIN_DIR/dprint.zip"
    fi
    DPRINT="$BIN_DIR/dprint"
else
    DPRINT="dprint"
fi
"$DPRINT" fmt

echo "==> Formatting Complete!"
