#!/usr/bin/env bash

# Copyright (C) 2026 Ian McKellar
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

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
