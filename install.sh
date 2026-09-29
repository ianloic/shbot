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

set -euo pipefail

echo "Installing shbot binary to ~/.cargo/bin..."
cargo install --path daemon

echo "Installing systemd user services..."
mkdir -p ~/.config/systemd/user
cp systemd/shbot.socket ~/.config/systemd/user/
cp systemd/shbot.service ~/.config/systemd/user/

echo "Reloading systemd daemon..."
systemctl --user daemon-reload
systemctl --user enable --now shbot.socket

echo "Checking ~/.zshrc configuration..."
ZSHRC="$HOME/.zshrc"
PLUGIN_SRC="$(pwd)/zsh/shbot.plugin.zsh"

if ! grep -q "source .*shbot.plugin.zsh" "$ZSHRC" 2>/dev/null; then
	echo "Adding shbot plugin to ~/.zshrc..."
	# Shell-quote the path so it evaluates safely as zsh code even when the
	# checkout directory contains spaces, quotes, or other metacharacters.
	ESCAPED_PLUGIN_SRC="'"$(printf '%s' "$PLUGIN_SRC" | sed "s/'/'\\\\''/g")"'"
	echo "" >>"$ZSHRC"
	echo "# shbot plugin hook" >>"$ZSHRC"
	echo "source $ESCAPED_PLUGIN_SRC" >>"$ZSHRC"
	echo "Successfully updated ~/.zshrc."
else
	echo "shbot plugin is already configured in ~/.zshrc."
fi

echo ""
echo "Installation complete!"
echo "Please start a new Zsh session or run 'source ~/.zshrc' in your current terminal."
