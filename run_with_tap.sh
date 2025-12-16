#!/bin/bash
# Run kernel with TAP networking for proper ICMP support

KERNEL_PATH="${1:-target/x86_64-mfk/debug/mfk-kernel}"
TAP_INTERFACE="tap0"
TAP_IP="192.168.100.1"
GUEST_IP="192.168.100.2"

echo "Setting up TAP networking..."
echo "This may require sudo password for network setup."

# Create TAP interface if it doesn't exist
if ! ip link show "$TAP_INTERFACE" &>/dev/null; then
    echo "Creating TAP interface $TAP_INTERFACE..."
    sudo ip tuntap add dev "$TAP_INTERFACE" mode tap user "$USER"
    sudo ip link set "$TAP_INTERFACE" up
    sudo ip addr add "$TAP_IP/24" dev "$TAP_INTERFACE"
    echo "TAP interface created: $TAP_INTERFACE ($TAP_IP)"
else
    echo "Using existing TAP interface: $TAP_INTERFACE"
fi

# Build if needed
if [ ! -f "$KERNEL_PATH" ]; then
    echo "Building kernel..."
    ./build.sh
fi

# Create disk images
cargo run -p mfk-runner --release -- "$KERNEL_PATH" --no-run

BIOS_PATH="${KERNEL_PATH}-bios.img"
DISK_PATH="target/disk.img"

echo ""
echo "====================================="
echo "  TAP Networking Configuration"
echo "====================================="
echo "Host TAP IP:   $TAP_IP"
echo "Guest IP:      $GUEST_IP (set with 'ifconfig $GUEST_IP' in kernel)"
echo ""
echo "After kernel boots:"
echo "  1. ifconfig $GUEST_IP"
echo "  2. ping $TAP_IP    (ping host from guest)"
echo ""
echo "From another terminal on host:"
echo "  ping $GUEST_IP    (ping guest from host - after ifconfig)"
echo "====================================="
echo ""
echo "Starting QEMU with TAP networking..."

qemu-system-x86_64 \
    -drive "file=$BIOS_PATH,format=raw,if=ide,index=0,media=disk" \
    -drive "file=$DISK_PATH,format=raw,if=ide,index=1,media=disk,cache=none,readonly=off" \
    -device e1000,netdev=net0 \
    -netdev tap,id=net0,ifname="$TAP_INTERFACE",script=no,downscript=no \
    -serial stdio \
    -display none \
    -no-reboot \
    -no-shutdown \
    -m 128M

echo ""
echo "QEMU exited."
