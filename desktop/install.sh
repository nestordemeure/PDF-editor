#!/usr/bin/env bash
# Adds PDF Editor to the desktop menu. The menu entry runs the release
# build through a symlink, so every `cargo build --release` updates it.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"
target="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
cargo build --release
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons/hicolor/256x256/apps
ln -sf "$target/release/pdf-editor" ~/.local/bin/pdf-editor
ln -sf "$target/release/pdf-editor-cli" ~/.local/bin/pdf-editor-cli
cp assets/icon.png ~/.local/share/icons/hicolor/256x256/apps/pdf-editor.png
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
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q ~/.local/share/icons/hicolor 2>/dev/null || true
echo "Installed: menu entry 'PDF Editor', commands pdf-editor and pdf-editor-cli in ~/.local/bin"
