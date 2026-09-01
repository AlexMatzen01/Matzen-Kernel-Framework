#!/usr/bin/env bash
# Test script for ping functionality (Debian/Linux)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building kernel..."
if ! ./build.sh; then
    echo "ERROR: Build failed" >&2
    exit 1
fi

KERNEL_BIN="target/x86_64-mfk/debug/mfk-kernel"
if [[ ! -f "$KERNEL_BIN" ]]; then
    echo "ERROR: Kernel not found at $KERNEL_BIN" >&2
    exit 1
fi

# Check for qemu or VBox
if ! command -v qemu-system-x86_64 >/dev/null 2>&1 && ! command -v VBoxManage >/dev/null 2>&1; then
    echo "ERROR: Neither qemu-system-x86_64 nor VBoxManage found" >&2
    echo "Install: sudo apt-get install qemu-system-x86 virtualbox" >&2
    exit 1
fi

if ! command -v timeout >/dev/null 2>&1; then
    echo "WARNING: 'timeout' not found (coreutils). Running without timeout." >&2
    TIMEOUT_CMD=""
else
    TIMEOUT_CMD="timeout 10"
fi

echo "Starting QEMU and sending test commands..."
echo "Commands to test:"
echo "  1. ifconfig 10.0.2.15"
echo "  2. ping 10.0.2.2"
echo ""

# Create command file (informational, runner uses stdio)
cat > /tmp/mfk_test_commands.txt <<'EOF'
ifconfig 10.0.2.15
ping 10.0.2.2 1
EOF
echo "Created /tmp/mfk_test_commands.txt"

echo "Starting kernel (will run for 10 seconds)..."
# Use qemu explicitly for test; VBox GUI not suitable for piped test
if [[ -n "$TIMEOUT_CMD" ]]; then
    $TIMEOUT_CMD ./run.sh "$KERNEL_BIN" --qemu 2>&1 | grep -E "(Serial port|E1000|IP:|ICMP:|Ethernet:|RX:|Reply from|Pinging|waiting for)" | head -50 || true
else
    ./run.sh "$KERNEL_BIN" --qemu 2>&1 | grep -E "(Serial port|E1000|IP:|ICMP:|Ethernet:|RX:|Reply from|Pinging|waiting for)" | head -50 || true
fi

echo ""
echo "Test complete. Check the output above for ICMP traffic."
echo "Note: QEMU user-mode ICMP is limited; for full test use ./run_with_tap.sh"
