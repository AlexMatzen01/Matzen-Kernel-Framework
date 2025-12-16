#!/bin/bash
# Test script for ping functionality

echo "Building kernel..."
./build.sh > /dev/null 2>&1

echo "Starting QEMU and sending test commands..."
echo "Commands to test:"
echo "  1. ifconfig 10.0.2.15"
echo "  2. ping 10.0.2.2"
echo ""

# Create command file
cat > /tmp/test_commands.txt <<EOF
ifconfig 10.0.2.15
ping 10.0.2.2 1
EOF

echo "Starting kernel (will run for 10 seconds)..."
timeout 10 ./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | grep -E "(Serial port|E1000|IP:|ICMP:|Ethernet:|RX:|Reply from|Pinging|waiting for)" | head -50

echo ""
echo "Test complete. Check the output above for ICMP traffic."
