#!/usr/bin/env bash
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
