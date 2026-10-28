#!/usr/bin/env bash
# Adds PDF Editor to the desktop menu. The menu entry runs the release
# build through a symlink, so every `cargo build --release` updates it.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"
target="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
cargo build --release
icons=~/.local/share/icons/hicolor
mkdir -p ~/.local/bin ~/.local/share/applications "$icons/256x256/apps" "$icons/128x128/apps"
ln -sf "$target/release/pdf-editor" ~/.local/bin/pdf-editor
ln -sf "$target/release/pdf-editor-cli" ~/.local/bin/pdf-editor-cli
cp assets/icon.png "$icons/256x256/apps/pdf-editor.png"
cp assets/icon-128.png "$icons/128x128/apps/pdf-editor.png"
cat > ~/.local/share/applications/pdf-editor.desktop <<DESKTOP
[Desktop Entry]
Type=Application
Name=PDF Editor
Comment=Clean up, compress and OCR scanned books
Exec=$HOME/.local/bin/pdf-editor %F
Icon=pdf-editor
Terminal=false
Categories=Office;
MimeType=application/pdf;
StartupWMClass=pdf-editor
DESKTOP
command -v update-desktop-database >/dev/null && update-desktop-database ~/.local/share/applications || true
# The user hicolor folder has no index.theme: without -t the update fails and a stale cache hides new icons.
if [ -e "$icons/icon-theme.cache" ] || command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t -q "$icons"
fi
echo "Installed: menu entry 'PDF Editor', commands pdf-editor and pdf-editor-cli in ~/.local/bin"
