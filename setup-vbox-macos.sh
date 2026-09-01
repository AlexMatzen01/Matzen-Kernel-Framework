#!/bin/bash
# VirtualBox Setup Helper for macOS

set -euo pipefail

echo "======================================"
echo "MFK VirtualBox Setup Helper (macOS)"
echo "======================================"
echo ""

# Step 1: Check VirtualBox Installation
echo "[1/3] Checking VirtualBox installation..."

if ! command -v VBoxManage &> /dev/null; then
    echo "ERROR: VirtualBox not found" >&2
    echo ""
    echo "Please install VirtualBox:"
    echo "  Using Homebrew:"
    echo "    brew install virtualbox"
    echo ""
    echo "  Or download from:"
    echo "    https://www.virtualbox.org/wiki/Downloads"
    echo ""
    echo "Note: You may need to grant system extensions permission in System Preferences"
    echo ""
    exit 1
fi

VBOX_VERSION=$(VBoxManage --version)
echo "✓ VirtualBox found: $VBOX_VERSION"
echo ""

# Step 2: List available network interfaces
echo "[2/3] Available network interfaces:"

if VBoxManage list bridgedifs > /dev/null 2>&1; then
    IFACES=$(VBoxManage list bridgedifs | grep "^Name:" | cut -d' ' -f2-)
    
    if [ -z "$IFACES" ]; then
        echo "  ERROR: No bridgeable network interfaces found" >&2
        echo ""
        echo "Check available interfaces:"
        echo "  ifconfig"
        echo ""
    else
        IFS=$'\n'
        i=1
        for iface in $IFACES; do
            STATUS=$(VBoxManage list bridgedifs | grep -A1 "^Name: $iface" | grep "Status:" | cut -d' ' -f2-)
            echo "  $i. $iface ($STATUS)"
            i=$((i + 1))
        done
    fi
else
    echo "  Could not query network interfaces"
fi
echo ""

# Step 3: Check for existing MFK VMs
echo "[3/3] Existing MFK VMs:"

if VBoxManage list vms > /dev/null 2>&1; then
    VMSCOUNT=$(VBoxManage list vms | grep -c "MFK-" || true)
    if [ "$VMSCOUNT" -eq 0 ]; then
        echo "  No MFK VMs found (they will be created on first run)"
    else
        VBoxManage list vms | grep "MFK-" | while read line; do
            echo "  • $line"
        done
    fi
fi
echo ""

echo "======================================"
echo "Setup Complete!"
echo "======================================"
echo ""
echo "Next steps:"
echo "1. Build the kernel:"
echo "   ./build.sh"
echo ""
echo "2. Run in VirtualBox:"
echo "   ./run.sh"
echo ""
echo "3. Inside the kernel, configure network:"
echo "   dhclient en0     # or set static IP"
echo "   ping 8.8.8.8"
echo ""
echo "For more information, see VIRTUALBOX_SETUP.md"
echo ""
