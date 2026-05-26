#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo ""
echo "  μCAS Installer"
echo "  =============="
echo ""

if [ ! -f "$SCRIPT_DIR/mucas" ]; then
    echo "  ERROR: mucas binary not found next to install.sh"
    echo "  Please extract the full zip archive before running this installer."
    exit 1
fi

# Install binary
if [ -w /usr/local/bin ]; then
    install -m 755 "$SCRIPT_DIR/mucas" /usr/local/bin/mucas
    MUCAS_BIN=/usr/local/bin/mucas
else
    mkdir -p "$HOME/.local/bin"
    install -m 755 "$SCRIPT_DIR/mucas" "$HOME/.local/bin/mucas"
    MUCAS_BIN="$HOME/.local/bin/mucas"
    # Ensure ~/.local/bin is on PATH (add to .bashrc/.zshrc if missing)
    for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
        if [ -f "$rc" ] && ! grep -q '\.local/bin' "$rc"; then
            echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$rc"
            echo "  Added ~/.local/bin to PATH in $rc"
        fi
    done
fi

# Install Nautilus (GNOME Files) right-click scripts
NAUTILUS_SCRIPTS="$HOME/.local/share/nautilus/scripts"
if command -v nautilus &>/dev/null || [ -d "$NAUTILUS_SCRIPTS" ]; then
    mkdir -p "$NAUTILUS_SCRIPTS"

    # Detect terminal emulator
    if command -v gnome-terminal &>/dev/null; then
        TERM_CMD="gnome-terminal -- bash -c"
    elif command -v konsole &>/dev/null; then
        TERM_CMD="konsole -e bash -c"
    elif command -v xterm &>/dev/null; then
        TERM_CMD="xterm -e bash -c"
    else
        TERM_CMD="bash -c"
    fi

    cat > "$NAUTILUS_SCRIPTS/Pack with μCAS" <<EOF
#!/bin/bash
IFS=\$'\n'
for f in \$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS; do
    $TERM_CMD "$MUCAS_BIN pack \"\$f\"; echo; read -p 'Done! Press Enter to close...' _; exit"
done
EOF
    chmod +x "$NAUTILUS_SCRIPTS/Pack with μCAS"

    cat > "$NAUTILUS_SCRIPTS/Unpack μCAS archive" <<EOF
#!/bin/bash
IFS=\$'\n'
for f in \$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS; do
    $TERM_CMD "$MUCAS_BIN unpack \"\$f\"; echo; read -p 'Done! Press Enter to close...' _; exit"
done
EOF
    chmod +x "$NAUTILUS_SCRIPTS/Unpack μCAS archive"

    echo "  Nautilus scripts installed."
    echo "  In Files (Nautilus): right-click -> Scripts -> \"Pack with μCAS\""
    echo "  In Files (Nautilus): right-click -> Scripts -> \"Unpack μCAS archive\""
fi

# Install .desktop file for .mcar file association (optional, needs update-desktop-database)
APPS_DIR="$HOME/.local/share/applications"
mkdir -p "$APPS_DIR"
cat > "$APPS_DIR/mucas.desktop" <<EOF
[Desktop Entry]
Name=μCAS Archive Tool
Comment=Pack and unpack μCAS archives
Exec=$MUCAS_BIN unpack %f
Icon=application-x-archive
Type=Application
MimeType=application/x-mcar;
NoDisplay=true
EOF

# Register .mcar MIME type
mkdir -p "$HOME/.local/share/mime/packages"
cat > "$HOME/.local/share/mime/packages/mucas.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-mcar">
    <comment>μCAS Archive</comment>
    <magic priority="50">
      <match type="string" offset="0" value="MCAR"/>
    </magic>
    <glob pattern="*.mcar"/>
  </mime-type>
</mime-info>
EOF

update-mime-database "$HOME/.local/share/mime" 2>/dev/null || true
update-desktop-database "$APPS_DIR" 2>/dev/null || true

echo ""
echo "  Installed mucas to: $MUCAS_BIN"
echo ""
echo "  KDE (Dolphin) users: go to Settings -> Configure Dolphin -> Services"
echo "  to add a service menu, or simply call mucas from the terminal."
echo ""
