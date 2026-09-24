# Quick Start Guide

Get MFK running in 5 minutes.

## Prerequisites

- Linux, macOS, or Windows (WSL)
- QEMU installed
- ~2GB disk space

## Installation & Boot (5 minutes)

### 1. Install Rust (if needed)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

### 2. Clone & Navigate
```bash
git clone https://github.com/AlexMatzen01/Matzen-Kernel-Framework.git
cd Matzen-Kernel-Framework
```

### 3. Build
```bash
./build.sh
```

This creates:
- `target/x86_64-mfk/debug/mfk-kernel` — Kernel binary
- `target/x86_64-mfk/debug/mfk-kernel-bios.img` — BIOS boot image
- `target/x86_64-mfk/debug/mfk-kernel-uefi.img` — UEFI boot image

### 4. Boot in QEMU
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

You should see:
```
======================================
  Matzen Kernel Framework v0.1.0
  Terminal OS Ready!
======================================

Type 'help' for available commands.

mfk>
```

## First Commands

```bash
# Show help
mfk> help

# Configure network (auto-configured, but you can check)
mfk> ifconfig

# List files
mfk> ls

# Print text
mfk> echo "Hello, MFK!"

# Check memory
mfk> memory

# Show system info
mfk> about

# Shutdown
mfk> halt
```

## Network Testing

Once booted, the network is auto-configured with IP `10.0.2.15`:

```bash
# Ping QEMU gateway
mfk> ping 10.0.2.2 4

# Check network status
mfk> netstat
```

## File System

Format and mount the virtual disk:

```bash
# Format disk (drive 1; --yes confirms the erase)
mfk> mkfs 1 --yes

# Mount filesystem
mfk> mount 1

# Create files
mfk> write myfile.txt "Hello, World!"

# Read files
mfk> cat myfile.txt

# List files
mfk> ls

# Delete files
mfk> rm myfile.txt
```

## Exit QEMU

Inside the kernel:
```bash
mfk> halt
```

Or press `Ctrl+A` then `X` in QEMU if it hangs.

## What's Next?

- **Learn more**: Read [Architecture Overview](../reference/architecture.md)
- **Build from source**: See [Building Guide](../development/building.md)
- **Extend the kernel**: Check [Extending Guide](../development/extending.md)
- **Troubleshoot**: Visit [Troubleshooting Guide](../development/troubleshooting.md)

## Common Issues

**Kernel doesn't boot?**
- Ensure you ran `./build.sh` successfully
- Check that `qemu-system-x86_64` is in your PATH

**Network doesn't work?**
- Network auto-configures at boot; run `ifconfig` to confirm IP is set
- Ping relies on ARP resolution which can take a few seconds

**Commands not recognized?**
- Type `help` to see all available commands
- Note: some commands may be case-sensitive

See [Troubleshooting Guide](../development/troubleshooting.md) for more help.
