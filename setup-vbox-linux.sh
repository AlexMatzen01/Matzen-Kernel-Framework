#!/bin/bash
# VirtualBox Setup Helper for Linux

set -euo pipefail

echo "======================================"
echo "MFK VirtualBox Setup Helper (Linux)"
echo "======================================"
echo ""

# Step 1: Check VirtualBox Installation
echo "[1/4] Checking VirtualBox installation..."

if ! command -v VBoxManage &> /dev/null; then
    echo "ERROR: VirtualBox not found" >&2
    echo ""
    echo "Please install VirtualBox:"
    echo "  Ubuntu/Debian:"
    echo "    sudo apt-get update"
    echo "    sudo apt-get install virtualbox virtualbox-dkms"
    echo ""
    echo "  Fedora/RHEL:"
    echo "    sudo dnf install virtualbox"
    echo ""
    echo "  Arch:"
    echo "    sudo pacman -S virtualbox"
    echo ""
    exit 1
fi

VBOX_VERSION=$(VBoxManage --version)
echo "✓ VirtualBox found: $VBOX_VERSION"
echo ""

# Step 2: Check kernel modules
echo "[2/4] Checking VirtualBox kernel modules..."

if lsmod | grep -q "vboxdrv"; then
    echo "✓ VirtualBox kernel modules loaded"
else
    echo "⚠ VirtualBox kernel modules not loaded"
    echo ""
    echo "To load them:"
    echo "  sudo modprobe vboxdrv"
    echo "  sudo modprobe vboxnetflt"
    echo ""
fi
echo ""

# Step 3: List available network interfaces
echo "[3/4] Available network interfaces:"

if VBoxManage list bridgedifs > /dev/null 2>&1; then
    IFACES=$(VBoxManage list bridgedifs | grep "^Name:" | cut -d' ' -f2-)
    
    if [ -z "$IFACES" ]; then
        echo "  ERROR: No bridgeable network interfaces found" >&2
        echo ""
        echo "Ensure your network interface is active:"
        echo "  ip link show"
    else
        IFS=$'\n'
        i=1
        for iface in $IFACES; do
            echo "  $i. $iface"
            i=$((i + 1))
        done
    fi
else
    echo "  Could not query network interfaces"
fi
echo ""

# Step 4: Check for existing MFK VMs
echo "[4/4] Existing MFK VMs:"

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

# Check user permissions
echo "[Bonus] User permissions:"

if groups | grep -q "vboxusers"; then
    echo "✓ User is in 'vboxusers' group (can use VirtualBox)"
else
    echo "⚠ User NOT in 'vboxusers' group"
    echo ""
    echo "Add your user to the group:"
    echo "  sudo usermod -aG vboxusers \$USER"
    echo "  newgrp vboxusers"
    echo ""
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
echo "   dhclient eth0    # or set static IP"
echo "   ping 8.8.8.8"
echo ""
echo "For more information, see VIRTUALBOX_SETUP.md"
echo ""
