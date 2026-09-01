#!/usr/bin/env bash
# Run kernel with TAP networking for proper ICMP support (Debian/Linux)
# TAP requires root for network setup; QEMU runs as user via /dev/net/tun
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

KERNEL_PATH="${1:-target/x86_64-mfk/debug/mfk-kernel}"
TAP_INTERFACE="${TAP_INTERFACE:-tap0}"
TAP_IP="${TAP_IP:-192.168.100.1}"
GUEST_IP="${GUEST_IP:-192.168.100.2}"

# --- Preflight checks (Debian packages) ---
if ! command -v ip >/dev/null 2>&1; then
    echo "ERROR: 'ip' not found. Install iproute2:" >&2
    echo "  sudo apt-get install iproute2" >&2
    exit 1
fi
if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "ERROR: qemu-system-x86_64 not found." >&2
    echo "  sudo apt-get install qemu-system-x86 qemu-utils" >&2
    exit 1
fi
if ! command -v qemu-img >/dev/null 2>&1; then
    echo "WARNING: qemu-img not found - disk creation may fail" >&2
fi

# Check for /dev/net/tun (required for TAP)
if [[ ! -c /dev/net/tun ]]; then
    echo "ERROR: /dev/net/tun not found. Load tun module:" >&2
    echo "  sudo modprobe tun && sudo chmod 666 /dev/net/tun" >&2
    exit 1
fi

echo "Setting up TAP networking..."
echo "  Interface: $TAP_INTERFACE  Host: $TAP_IP  Guest: $GUEST_IP"
echo "  This may require sudo password for network setup."
echo ""

# Early sudo check - fail fast if we can't sudo
if ! sudo -n true 2>/dev/null; then
    echo "Requesting sudo credentials..."
    sudo -v || { echo "ERROR: sudo required for TAP setup" >&2; exit 1; }
    # Keep sudo alive in background
    while true; do sudo -n true; sleep 60; kill -0 "$$" || exit; done 2>/dev/null &
    SUDO_KEEPALIVE_PID=$!
    trap 'kill $SUDO_KEEPALIVE_PID 2>/dev/null || true' EXIT
fi

# Create TAP interface if it doesn't exist
if ! ip link show "$TAP_INTERFACE" &>/dev/null; then
    echo "Creating TAP interface $TAP_INTERFACE..."
    sudo ip tuntap add dev "$TAP_INTERFACE" mode tap user "$USER"
    sudo ip link set "$TAP_INTERFACE" up
    sudo ip addr add "$TAP_IP/24" dev "$TAP_INTERFACE"
    echo "✓ TAP interface created: $TAP_INTERFACE ($TAP_IP)"
    # Enable IP forwarding for guest<->host connectivity
    echo "  Enabling IP forwarding..."
    sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null || true
else
    echo "Using existing TAP interface: $TAP_INTERFACE"
    # Ensure it's up and has IP
    sudo ip link set "$TAP_INTERFACE" up 2>/dev/null || true
    if ! ip addr show "$TAP_INTERFACE" 2>/dev/null | grep -q "$TAP_IP"; then
        echo "  Adding IP $TAP_IP to existing interface..."
        sudo ip addr add "$TAP_IP/24" dev "$TAP_INTERFACE" 2>/dev/null || true
    fi
    ip addr show "$TAP_INTERFACE" 2>/dev/null | sed 's/^/  /' || true
fi

echo ""

# Build if needed
if [[ ! -f "$KERNEL_PATH" ]]; then
    echo "Kernel not found at $KERNEL_PATH - building..."
    if ! ./build.sh; then
        echo "ERROR: Build failed" >&2
        exit 1
    fi
fi

if [[ ! -f "$KERNEL_PATH" ]]; then
    echo "ERROR: Kernel still not found at $KERNEL_PATH after build" >&2
    exit 1
fi

# Create disk images (via runner --no-run)
echo "Creating disk images..."
if ! cargo run -p mfk-runner --release -- "$KERNEL_PATH" --no-run; then
    echo "ERROR: Failed to create disk images" >&2
    exit 1
fi

BIOS_PATH="${KERNEL_PATH}-bios.img"
DISK_PATH="target/disk.img"

if [[ ! -f "$BIOS_PATH" ]]; then
    echo "ERROR: BIOS image not created at $BIOS_PATH" >&2
    exit 1
fi

# Ensure data disk exists
if [[ ! -f "$DISK_PATH" ]]; then
    echo "Creating data disk $DISK_PATH..."
    qemu-img create -f raw "$DISK_PATH" 10M
fi

echo ""
echo "====================================="
echo "  TAP Networking Configuration"
echo "====================================="
echo "Host TAP IP:   $TAP_IP"
echo "Guest IP:      $GUEST_IP  (set with 'ifconfig $GUEST_IP' in kernel)"
echo ""
echo "After kernel boots:"
echo "  1. ifconfig $GUEST_IP"
echo "  2. ping $TAP_IP    (ping host from guest)"
echo ""
echo "From another terminal on host:"
echo "  ping $GUEST_IP    (after ifconfig in guest)"
echo "  # To remove TAP later: sudo ip link del $TAP_INTERFACE"
echo "====================================="
echo ""
echo "Starting QEMU with TAP networking..."
echo "  BIOS: $BIOS_PATH"
echo "  Disk: $DISK_PATH"
echo "  TAP:  $TAP_INTERFACE"
echo ""

exec qemu-system-x86_64 \
    -drive "file=$BIOS_PATH,format=raw,if=ide,index=0,media=disk" \
    -drive "file=$DISK_PATH,format=raw,if=ide,index=1,media=disk,cache=none,readonly=off" \
    -device e1000,netdev=net0 \
    -netdev tap,id=net0,ifname="$TAP_INTERFACE",script=no,downscript=no \
    -serial stdio \
    -display none \
    -no-reboot \
    -no-shutdown \
    -m 128M
