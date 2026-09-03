# Running the Kernel

How to boot and interact with MFK in QEMU and other environments.

## Quick Boot

```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

The kernel will boot and present a shell prompt:

```
mfk>
```

## Detailed Boot Steps

### 1. Build (if not already built)
```bash
./build.sh
```

### 2. Create Disk Image (one-time)
```bash
qemu-img create -f raw target/disk.img 10M
```

The kernel comes with tooling that auto-creates this if missing.

### 3. Boot Options

#### Option A: Use Helper Script (Recommended)
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

#### Option B: Direct mfk-runner
```bash
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

#### Option C: Create Image Only (No Boot)
```bash
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run
```

This creates:
- `target/x86_64-mfk/debug/mfk-kernel-bios.img` — BIOS bootable
- `target/x86_64-mfk/debug/mfk-kernel-uefi.img` — UEFI bootable

## Boot Process

The kernel goes through these initialization stages:

```
QEMU Bootloader
    ↓
Bootloader Maps Physical Memory
    ↓
Kernel Entry Point (kernel_main)
    ↓
Serial Port Init → Serial Logging Available
    ↓
VGA Init → Terminal Output
    ↓
Heap Allocator Init
    ↓
Interrupt Handlers (IDT, PIC)
    ↓
Drivers (Keyboard, Disk, Network)
    ↓
Network Stack Init → Network Available
    ↓
Shell Ready → Interactive Prompt
```

## Boot Messages

You'll see output like:

```
Serial port initialized
Physical memory offset: 0x20000000000
VGA initialized
Heap allocator initialized
======================================
  Matzen Kernel Framework v0.1.0
  Terminal OS Ready!
======================================

Type 'help' for available commands.

Welcome message printed
IDT initialized
PIC initialized and configured
Keyboard driver initialized
Keyboard initialized
ATA driver initialized (2 drive(s) found)
E1000: Starting initialization...
E1000: Device found!
E1000 initialized
  MAC Address: 52:54:00:12:34:56
Network stack initialized
IP address set to 10.0.2.15
Network configured: IP 10.0.2.15
Interrupts enabled
Starting shell...
mfk>
```

## Shell Interaction

### Entering Commands

Type commands at the `mfk>` prompt:

```
mfk> help
mfk> echo Hello World
mfk> ifconfig
```

### Special Keys

- **Enter** — Execute command
- **Backspace** — Delete character
- **Ctrl+C** — Cancel current operation
- **Ctrl+A then X** — Exit QEMU (if stuck)

### Command Examples

```bash
# System information
help                    # Show available commands
about                   # Project information
version                 # Kernel version
cpuinfo                 # CPU information
memory                  # Memory layout
uptime                  # System uptime

# Terminal control
clear                   # Clear screen
color green             # Change text color

# Network
ifconfig                # Show/set IP address
ping 10.0.2.2 4         # Test connectivity
netstat                 # Network status

# File system
mkfs                    # Format disk
mount                   # Mount filesystem
ls                      # List files
write file.txt content  # Create/write file
cat file.txt            # Read file
touch file.txt          # Create empty file
rm file.txt             # Delete file

# System control
reboot                  # Reboot
halt                    # Shutdown
```

## QEMU Configuration

The kernel runs with these QEMU parameters:

```
-m 128M                                    # 128MB RAM
-drive file=...-bios.img                   # Boot disk
-drive file=disk.img                       # Data disk
-serial stdio                              # Serial to stdout
-display none                              # No GUI
-no-reboot                                 # Exit on reboot
-no-shutdown                               # Keep running
```

### Custom QEMU Launch

For more control, you can boot manually:

```bash
# Build first
./build.sh

# Manual QEMU boot
qemu-system-x86_64 \
  -drive file=target/x86_64-mfk/debug/mfk-kernel-bios.img,format=raw,if=ide,index=0 \
  -drive file=target/disk.img,format=raw,if=ide,index=1 \
  -serial stdio \
  -m 128M \
  -no-reboot \
  -no-shutdown
```

## Multi-Boot Support

MFK can boot via multiple methods:

### BIOS Boot (Default)
```bash
qemu-system-x86_64 -drive file=mfk-kernel-bios.img,format=raw
```

### UEFI Boot
```bash
# Use the OVMF_CODE path installed by your distribution.
qemu-system-x86_64 \
  -drive file=mfk-kernel-uefi.img,format=raw \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE.fd
```

If OVMF is installed at a different path, set `OVMF_CODE` when using
`./run.sh --qemu --uefi`, or replace the path above with the result of
`find /usr/share -iname 'OVMF_CODE*.fd' 2>/dev/null`.

### Real Hardware
You can write the BIOS image to a USB stick:

```bash
dd if=target/x86_64-mfk/debug/mfk-kernel-bios.img of=/dev/sdX bs=4M
```

Replace `/dev/sdX` with your USB device (be careful!).

## Network Configuration

The kernel auto-configures with:
- **IP Address**: `10.0.2.15`
- **Gateway**: `10.0.2.2`
- **Subnet**: `255.255.255.0`

This assumes QEMU's default TAP/user network mode.

### Custom Network Setup

Inside the kernel, you can reconfigure:

```bash
mfk> ifconfig 192.168.1.100
mfk> ping 192.168.1.1
```

## Exit Options

### Clean Shutdown
```bash
mfk> halt
```

This gracefully halts the system and exits QEMU.

### Force Exit
- Press `Ctrl+C` in the terminal
- Press `Ctrl+A` then `X` in QEMU
- Send SIGTERM to QEMU process

## Debugging Output

The kernel sends debug output to serial port. You may see boot messages on stderr:

```
Serial port initialized
Physical memory offset: 0x20000000000
...
```

To redirect to a file:

```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel 2> boot.log
```

## Performance Notes

- First boot may be slow (descriptor table setup, ARP resolution)
- Keyboard input has ~100ms latency (polling in main loop)
- Network packets process asynchronously at ~1000 loop iterations/second
- File operations are synchronous (may block other tasks)

## Next Steps

- **[Shell Commands Reference](../reference/shell-commands.md)** — All available commands
- **[Network Testing](../reference/networking.md)** — How to test networking
- **[File System Usage](../reference/filesystem.md)** — Create and manage files
- **[Troubleshooting](../development/troubleshooting.md)** — Common issues
