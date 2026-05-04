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

echo "Building shbot..."
cd daemon
cargo build --release
cd ..

echo "Installing shbot binary to ~/.local/bin..."
mkdir -p ~/.local/bin
cp target/release/shbot ~/.local/bin/shbot

echo "Installing systemd user services..."
mkdir -p ~/.config/systemd/user
cp systemd/shbot.socket ~/.config/systemd/user/
cp systemd/shbot.service ~/.config/systemd/user/

echo "Reloading systemd daemon..."
systemctl --user daemon-reload
systemctl --user enable --now shbot.socket

echo "Checking ~/.zshrc configuration..."
ZSHRC="$HOME/.zshrc"
PLUGIN_SRC="$(pwd)/zsh/shbot.zsh"

if ! grep -q "source .*shbot.zsh" "$ZSHRC" 2>/dev/null; then
    echo "Adding shbot plugin to ~/.zshrc..."
    echo "" >> "$ZSHRC"
    echo "# shbot plugin hook" >> "$ZSHRC"
    echo "source \"$PLUGIN_SRC\"" >> "$ZSHRC"
    echo "Successfully updated ~/.zshrc."
else
    echo "shbot plugin is already configured in ~/.zshrc."
fi

echo ""
echo "Installation complete!"
echo "Please start a new Zsh session or run 'source ~/.zshrc' in your current terminal."
