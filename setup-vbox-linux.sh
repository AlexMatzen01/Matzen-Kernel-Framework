#!/usr/bin/env bash
# VirtualBox Setup Helper for Debian / Linux
# Diagnoses and guides VirtualBox installation on Debian 11/12
set -euo pipefail

echo "======================================"
echo "MFK VirtualBox Setup Helper (Linux/Debian)"
echo "======================================"
echo ""

# Step 1: Check VirtualBox Installation
echo "[1/5] Checking VirtualBox installation..."

if ! command -v VBoxManage >/dev/null 2>&1; then
    echo "ERROR: VirtualBox not found" >&2
    echo ""
    echo "Install on Debian:"
    echo "  sudo apt-get update"
    echo "  sudo apt-get install virtualbox virtualbox-dkms linux-headers-\$(uname -r) dkms"
    echo "  # For VirtualBox Extension Pack (optional but recommended):"
    echo "  # sudo apt-get install virtualbox-ext-pack"
    echo ""
    echo "  # If 'virtualbox' not found, enable contrib/non-free:"
    echo "  #   echo 'deb http://deb.debian.org/debian bookworm main contrib non-free non-free-firmware' | sudo tee /etc/apt/sources.list.d/debian-contrib.list"
    echo "  #   sudo apt-get update && sudo apt-get install virtualbox"
    echo ""
    echo "Or download .deb from: https://www.virtualbox.org/wiki/Downloads"
    echo ""
    echo "Fedora/RHEL:"
    echo "  sudo dnf install virtualbox"
    echo ""
    echo "Arch:"
    echo "  sudo pacman -S virtualbox"
    echo ""
    exit 1
fi

VBOX_VERSION="$(VBoxManage --version 2>/dev/null || echo "unknown")"
echo "✓ VirtualBox found: $VBOX_VERSION"
# Check for kernel headers mismatch (common Debian DKMS failure)
if command -v dkms >/dev/null 2>&1; then
    if dkms status 2>/dev/null | grep -qi "virtualbox.*added"; then
        echo "  ⚠ VirtualBox DKMS modules show 'added' not 'installed' - headers may be missing:"
        echo "    sudo apt-get install linux-headers-\$(uname -r) && sudo dkms autoinstall"
    fi
fi
if command -v mokutil >/dev/null 2>&1 && mokutil --sb-state 2>/dev/null | grep -qi "enabled"; then
    echo "  ⚠ Secure Boot is ENABLED - VirtualBox modules may fail to load until you enroll MOK or disable Secure Boot."
fi
echo ""

# Step 2: Check kernel modules
echo "[2/5] Checking VirtualBox kernel modules..."

if lsmod 2>/dev/null | grep -q "vboxdrv"; then
    echo "✓ VirtualBox kernel modules loaded"
    lsmod | grep vbox | sed 's/^/  /' || true
else
    echo "⚠ VirtualBox kernel modules not loaded"
    echo ""
    echo "To load them:"
    echo "  sudo modprobe vboxdrv"
    echo "  sudo modprobe vboxnetflt"
    echo "  sudo modprobe vboxnetadp"
    echo "  sudo modprobe vboxpci  # optional"
    echo ""
    echo "If modprobe fails (Debian):"
    echo "  sudo apt-get install --reinstall virtualbox-dkms linux-headers-\$(uname -r)"
    echo "  sudo dkms autoinstall && sudo modprobe vboxdrv"
    echo ""
fi
echo ""

# Step 3: List available network interfaces (Debian predictable names: enp*, ens*, wlp*)
echo "[3/5] Available network interfaces (bridged):"

if VBoxManage list bridgedifs >/dev/null 2>&1; then
    # Robust parse: handle names with spaces, include status + IP
    BRIDGED_OUTPUT="$(VBoxManage list bridgedifs 2>/dev/null || true)"
    # Extract Name lines safely
    IFACES="$(echo "$BRIDGED_OUTPUT" | awk -F': ' '/^Name:/ {print substr($0, index($0,": ")+2)}' || true)"

    if [ -z "${IFACES:-}" ]; then
        echo "  ERROR: No bridgeable network interfaces found" >&2
        echo ""
        echo "Ensure your network interface is active:"
        echo "  ip link show"
        echo "  ip addr show"
        echo "  VBoxManage list bridgedifs  # verbose"
    else
        i=1
        # Use while read to handle spaces in names
        echo "$IFACES" | while IFS= read -r iface; do
            [ -z "$iface" ] && continue
            # Try to get status for this iface
            STATUS="$(echo "$BRIDGED_OUTPUT" | awk -v name="$iface" '
                $0 == "Name:            " name {found=1; next}
                found && /^Status:/ {print substr($0,index($0,": ")+2); exit}
            ' 2>/dev/null || echo "unknown")"
            IPADDR="$(echo "$BRIDGED_OUTPUT" | awk -v name="$iface" '
                $0 == "Name:            " name {found=1; next}
                found && /^IPAddress:/ {print substr($0,index($0,": ")+2); exit}
            ' 2>/dev/null || echo "")"
            if [ -n "$IPADDR" ] && [ "$IPADDR" != "0.0.0.0" ]; then
                echo "  $i. $iface  [$STATUS]  $IPADDR"
            else
                echo "  $i. $iface  [$STATUS]"
            fi
            i=$((i + 1))
        done
        # Note: i is in subshell, but informational only
    fi
else
    echo "  Could not query network interfaces (VBoxManage list bridgedifs failed)"
    echo "  Try: VBoxManage list bridgedifs  # to see error"
fi

# Also show host's actual interfaces (Debian iproute2)
echo ""
echo "  Host interfaces (ip link):"
if command -v ip >/dev/null 2>&1; then
    ip -o link show 2>/dev/null | awk -F': ' '{print "    - " $2}' | head -20 || ip link show | head -30
else
    echo "    (ip not found - install iproute2: sudo apt-get install iproute2)"
fi
echo ""

# Step 4: Check for existing MFK VMs
echo "[4/5] Existing MFK VMs:"

if VBoxManage list vms >/dev/null 2>&1; then
    VMSCOUNT="$(VBoxManage list vms 2>/dev/null | grep -c "MFK-" || true)"
    if [ "$VMSCOUNT" -eq 0 ]; then
        echo "  No MFK VMs found (they will be created on first ./run.sh)"
    else
        VBoxManage list vms 2>/dev/null | grep "MFK-" | while IFS= read -r line; do
            echo "  • $line"
        done
        echo ""
        echo "  To delete stale VM (common fix for 'kernel not loading' after rebuild):"
        echo "    VBoxManage controlvm MFK-mfk-kernel poweroff 2>/dev/null || true"
        echo "    VBoxManage unregistervm MFK-mfk-kernel --delete"
        echo "    rm -f target/*.vdi target/x86_64-mfk/debug/*.img target/x86_64-mfk/release/*.img"
    fi
else
    echo "  Could not query VMs (is VirtualBox running?)"
fi
echo ""

# Step 5: Check QEMU fallback
echo "[5/5] QEMU fallback check:"
if command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "  ✓ QEMU present: $(qemu-system-x86_64 --version 2>/dev/null | head -n1)"
    echo "    You can run without VirtualBox: ./run.sh --qemu"
else
    echo "  ⚠ QEMU not found - install for fallback:"
    echo "    sudo apt-get install qemu-system-x86 qemu-utils"
fi
if command -v qemu-img >/dev/null 2>&1; then
    echo "  ✓ qemu-img present"
fi
echo ""

# Bonus: User permissions (robust: avoid substring match)
echo "[Bonus] User permissions:"

if id -nG "$USER" 2>/dev/null | tr ' ' '\n' | grep -qx "vboxusers"; then
    echo "✓ User '$USER' is in 'vboxusers' group"
else
    echo "⚠ User '$USER' NOT in 'vboxusers' group"
    echo ""
    echo "Add your user to the group:"
    echo "  sudo usermod -aG vboxusers \$USER"
    echo "  newgrp vboxusers   # apply without logout, or logout/login"
    echo ""
fi

# Check for build toolchain (Debian specific)
echo ""
echo "[Bonus] Build toolchain:"
if command -v rustc >/dev/null 2>&1; then
    echo "  ✓ rustc: $(rustc --version 2>/dev/null | head -n1)"
else
    echo "  ✗ rustc missing - run ./install.sh"
fi
if rustup toolchain list 2>/dev/null | grep -q "^nightly"; then
    echo "  ✓ nightly: $(rustup run nightly rustc --version 2>/dev/null | head -n1)"
else
    echo "  ✗ nightly missing - run ./install.sh or: rustup toolchain install nightly"
fi
if rustup component list --toolchain nightly 2>/dev/null | grep -q "^rust-src.*(installed)"; then
    echo "  ✓ rust-src installed"
else
    echo "  ✗ rust-src missing - run ./install.sh"
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
echo "   # or fallback: ./run.sh --qemu"
echo ""
echo "3. Inside the kernel, configure network:"
echo "   ifconfig 10.0.2.15    # or: dhclient eth0  (bridged DHCP if supported)"
echo "   ping 10.0.2.2         # QEMU host; for VirtualBox ping your router e.g. 192.168.1.1"
echo "   ping 8.8.8.8"
echo ""
echo "Stale VM fix (if kernel appears to not update after rebuild):"
echo "  VBoxManage unregistervm MFK-mfk-kernel --delete 2>/dev/null || true"
echo "  rm -f target/*.vdi target/x86_64-mfk/debug/*.img target/x86_64-mfk/release/*.img"
echo "  ./run.sh"
echo ""
echo "For more information, see VIRTUALBOX_SETUP.md"
echo ""
