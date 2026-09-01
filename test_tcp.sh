#!/usr/bin/env bash
# Quick TCP connection test (Debian/Linux)
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

echo ""
echo "========================================="
echo "  TCP Connection Test"
echo "========================================="
echo ""
echo "Starting kernel. After boot, try these commands:"
echo ""
echo "  1. Configure IP:"
echo "     ifconfig 10.0.2.15"
echo ""
echo "  2. Test TCP connection to QEMU host:"
echo "     tcpconnect 10.0.2.2 80"
echo ""
echo "  3. Send HTTP request (if connected):"
echo "     tcpsend <port> GET / HTTP/1.0"
echo ""
echo "  4. Close connection:"
echo "     tcpclose <port>"
echo ""
echo "Note: QEMU user-mode networking has limitations."
echo "      Use './run_with_tap.sh' for full TCP testing."
echo ""
echo "========================================="
echo ""

# Auto-fallback to QEMU for test (piping not suitable for VBox GUI)
exec ./run.sh "$KERNEL_BIN" --qemu
