#!/bin/sh
# naTiVeFS Linux installer
# Usage: sh install.sh

set -e

BINARY_URL="https://github.com/TomasekValla/naTiVeFS/releases/latest/download/nativefs-linux-x64"
INSTALL_DIR="$HOME/.local/bin"
BINARY="$INSTALL_DIR/nativefs"
DESKTOP="$HOME/.local/share/applications/nativefs.desktop"

echo "Installing naTiVeFS..."

mkdir -p "$INSTALL_DIR"
mkdir -p "$(dirname "$DESKTOP")"

echo "Downloading binary..."
curl -fsSL "$BINARY_URL" -o "$BINARY"
chmod +x "$BINARY"

echo "Creating .desktop entry..."
cat > "$DESKTOP" << EOF
[Desktop Entry]
Name=naTiVeFS
Comment=Upload files to TVFS
Exec=$BINARY %F
Icon=nativefs
MimeType=*/*
Type=Application
Categories=Network;FileTransfer;
StartupNotify=false
EOF

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

echo ""
echo "naTiVeFS installed to $BINARY"
echo "You can now:"
echo "  - Run: nativefs"
echo "  - Right-click any file → Open With → naTiVeFS"
