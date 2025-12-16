#!/bin/bash
# Quick TCP connection test

echo "Building kernel with TCP support..."
./build.sh > /dev/null 2>&1

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

./run.sh target/x86_64-mfk/debug/mfk-kernel
