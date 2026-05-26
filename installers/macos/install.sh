#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ARCH="$(uname -m)"

echo ""
echo "  μCAS Installer"
echo "  =============="
echo ""

# Pick correct binary for this architecture
if [ "$ARCH" = "arm64" ] && [ -f "$SCRIPT_DIR/mucas-arm64" ]; then
    SRC="$SCRIPT_DIR/mucas-arm64"
    echo "  Detected: Apple Silicon (arm64)"
elif [ -f "$SCRIPT_DIR/mucas" ]; then
    SRC="$SCRIPT_DIR/mucas"
    echo "  Detected: Intel (x86_64)"
else
    echo "  ERROR: No mucas binary found next to install.sh"
    echo "  Please extract the full zip archive before running this installer."
    exit 1
fi

# Install binary
if [ -w /usr/local/bin ]; then
    cp "$SRC" /usr/local/bin/mucas
    chmod +x /usr/local/bin/mucas
    MUCAS_BIN=/usr/local/bin/mucas
else
    mkdir -p "$HOME/.local/bin"
    cp "$SRC" "$HOME/.local/bin/mucas"
    chmod +x "$HOME/.local/bin/mucas"
    MUCAS_BIN="$HOME/.local/bin/mucas"
    echo "  Installed to ~/.local/bin (add to PATH if not already there)"
fi

# Install Quick Actions to ~/Library/Services/
SERVICES_DIR="$HOME/Library/Services"
mkdir -p "$SERVICES_DIR"

for wf in "Pack with μCAS.workflow" "Unpack μCAS archive.workflow"; do
    if [ -d "$SCRIPT_DIR/$wf" ]; then
        rm -rf "$SERVICES_DIR/$wf"
        cp -R "$SCRIPT_DIR/$wf" "$SERVICES_DIR/$wf"
        # Patch binary path in workflow (default is /usr/local/bin/mucas)
        if [ "$MUCAS_BIN" != "/usr/local/bin/mucas" ]; then
            sed -i '' "s|/usr/local/bin/mucas|$MUCAS_BIN|g" \
                "$SERVICES_DIR/$wf/Contents/document.wflow" 2>/dev/null || true
        fi
    fi
done

# Reload the Services menu
/System/Library/CoreServices/pbs -update 2>/dev/null || true

echo ""
echo "  Installed mucas to: $MUCAS_BIN"
echo ""
echo "  In Finder: right-click any folder -> Quick Actions -> \"Pack with μCAS\""
echo "  In Finder: right-click any .mcar  -> Quick Actions -> \"Unpack μCAS archive\""
echo ""
echo "  NOTE: On first use, you may need to enable the Quick Actions in:"
echo "  System Settings -> Privacy & Security -> Extensions -> Finder"
echo ""
