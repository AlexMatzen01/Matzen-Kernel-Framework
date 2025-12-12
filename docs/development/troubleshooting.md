# Troubleshooting Guide

Solutions for common issues with MFK.

## Build Issues

### "rustc: command not found"

**Cause**: Rust toolchain not installed or not in PATH.

**Solution**:
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Source environment
source $HOME/.cargo/env

# Verify
rustc --version
```

### "error: failed to resolve: use of undeclared crate `xxx`"

**Cause**: Missing dependency or corrupted build.

**Solution**:
```bash
# Clean and rebuild
cargo clean
./build.sh

# Update toolchain
rustup update nightly
```

### "error: could not compile `mfk-kernel`"

**Cause**: Incompatible Rust version or missing components.

**Solution**:
```bash
# Ensure nightly is default
rustup default nightly

# Install required components
rustup component add rust-src llvm-tools-preview --toolchain nightly

# Rebuild
cargo clean
./build.sh
```

### "linking with `cc` failed: exit status: 1"

**Cause**: Linker not found or incompatible toolchain.

**Solution**:
```bash
# Linux: Install build tools
sudo apt-get install build-essential  # Ubuntu
sudo yum install gcc make             # Fedora

# macOS: Install Xcode
xcode-select --install

# Windows (WSL): Install build tools
sudo apt-get install build-essential
```

## Boot Issues

### Kernel doesn't boot / Black screen

**Cause**: QEMU issues or missing disk image.

**Solution**:
```bash
# Verify disk image exists
ls -la target/disk.img

# Recreate if missing
qemu-img create -f raw target/disk.img 10M

# Try rebuilding
./build.sh
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

### "qemu-system-x86_64: not found"

**Cause**: QEMU not installed or not in PATH.

**Solution**:
```bash
# Linux
sudo apt-get install qemu-system-x86  # Ubuntu
sudo yum install qemu-system-x86      # Fedora

# macOS
brew install qemu

# Verify
which qemu-system-x86_64
```

### "KERNEL PANIC! attempt to subtract with overflow"

**Cause**: Address space calculation error.

**Solution**: Usually fixed in recent versions. Update:
```bash
git pull origin main
./build.sh
```

### Kernel hangs at boot

**Cause**: Infinite loop in initialization, network driver hang, or disk I/O issue.

**Solution**:
```bash
# Check boot messages
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | head -50

# Try without network
# (Edit kernel/src/main.rs, comment out e1000::init())

# Try minimal drivers
# (Check which driver is causing hang in serial output)
```

## Runtime Issues

### Shell prompt doesn't appear

**Cause**: Slow boot, network initialization delay, or VGA issue.

**Solution**:
```bash
# Wait longer (5-10 seconds from boot)
# Network initialization can take a few seconds

# Check serial output for progress
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | tail -20

# If VGA not working, check:
# - Bootloader config in kernel/src/main.rs
# - VGA initialization in kernel/src/drivers/vga.rs
```

### Commands not recognized

**Cause**: Typo, case mismatch, or command not implemented.

**Solution**:
```bash
# Check available commands
mfk> help

# Commands are case-sensitive
mfk> Help          # ❌ Error
mfk> help          # ✅ Correct

# Check for typos
mfk> ifconfig      # ✅ Correct
mfk> ifconf        # ❌ Typo
```

### Keyboard input erratic

**Cause**: PS/2 driver not initialized or interrupt disabled.

**Solution**:
```bash
# Check keyboard in help
mfk> help | grep -i keyboard

# Try re-initializing (not possible, requires reboot)

# Verify keyboard driver in:
# kernel/src/drivers/keyboard.rs
```

### Commands are slow

**Cause**: Network processing delays, disk I/O waiting, or polling loop.

**Solution**:
```bash
# Network ARP resolution can take ~100ms first time
# Subsequent pings to same IP should be faster

# File operations are synchronous (blocking)

# Consider reducing network packet processing:
# Edit kernel/src/shell/mod.rs, reduce process_packets() frequency
```

## Network Issues

### Ping doesn't work / No replies

**Cause**: IP not configured, network not initialized, or E1000 driver issue.

**Solution**:
```bash
# Check network configuration
mfk> ifconfig
# Should show: IP Address:  10.0.2.15

# If not configured, set it
mfk> ifconfig 10.0.2.15

# Check network status
mfk> netstat
# Should show Ethernet and IP are Active

# Ping should work now
mfk> ping 10.0.2.2 2

# If still no reply:
# - E1000 driver may not be receiving packets
# - Check kernel logs for E1000 errors
# - Verify QEMU has network enabled
```

### "Pinging X.X.X.X but no replies" after sending packets

**Cause**: Packet transmission works but RX descriptor buffers not properly addressed.

**Solution**:
```bash
# Verify E1000 initialization
# Check: "E1000 initialized" in boot output

# Check MAC address  
mfk> ifconfig
# Should show MAC address like 52:54:00:12:34:56

# If MAC is 00:00:00:00:00:00, E1000 didn't initialize properly

# Check kernel source:
# kernel/src/drivers/e1000.rs for address calculation issues
```

### "Permission denied" on disk access

**Cause**: File system not mounted or permissions issue.

**Solution**:
```bash
# Check filesystem is mounted
mfk> ls
# If error: filesystem not mounted

# Mount filesystem
mfk> mkfs    # Create filesystem
mfk> mount   # Mount it

# Try again
mfk> ls      # Should work now
```

## File System Issues

### "File not found" when file exists

**Cause**: File system not mounted or file in wrong location.

**Solution**:
```bash
# Mount the filesystem
mfk> mount

# List files
mfk> ls

# If still not found, recreate:
mfk> write myfile.txt "content"
```

### Can't write files

**Cause**: File system not mounted, disk full, or disk I/O error.

**Solution**:
```bash
# Mount filesystem first
mfk> mount

# Check disk status
mfk> diskinfo
# Look for available blocks

# Try smaller file
mfk> write test.txt "abc"

# If error persists:
# - Disk image may be corrupted
# - Recreate: qemu-img create -f raw target/disk.img 10M
# - Reformat: mkfs
```

### Files disappear after reboot

**Cause**: Each QEMU run creates fresh disk image if not persistent.

**Solution**:
```bash
# Disk image should persist in target/disk.img
# If files disappear:

# 1. Check disk image exists
ls -la target/disk.img

# 2. Ensure mkfs and mount on each boot
mfk> mkfs
mfk> mount

# 3. Or manually keep persistent disk
# (See documentation on persistent storage)
```

## Performance Issues

### System is very slow

**Cause**: Polling-based architecture, no optimization, or debug build.

**Solution**:
```bash
# Use release build (faster)
./build.sh                              # Builds release by default
cargo run -p mfk-runner --release ...

# Network processing is slow (polling every loop)
# Disk I/O is synchronous and blocking
# This is normal for educational kernel

# Don't run too many operations simultaneously
```

### High CPU usage in QEMU

**Cause**: Main loop spinning without halt, constant polling.

**Solution**:
```bash
# This is normal for an OS without proper scheduling

# To reduce slightly, consider:
# - Increasing polling interval
# - Adding hlt() instructions in idle loop
# - Implementing proper interrupts (currently done)

# Should use ~20-30% CPU on idle in QEMU
```

## Memory Issues

### Heap exhaustion / Out of memory

**Cause**: Allocations exceed heap size or memory leak.

**Solution**:
```bash
# Kernel has limited heap
# Default: 100KB heap allocation

# Check usage
mfk> memory

# If running out:
# - Reduce network buffers in e1000.rs
# - Limit file system caches
# - Reduce shell command history

# Or increase heap:
# kernel/src/allocator.rs: HEAP_SIZE = ...
```

### "Heap not initialized"

**Cause**: Accessing heap before allocator init.

**Solution**:
- Should not happen in normal boot
- Check kernel/src/main.rs initialization order
- allocator::init() must come very early

## Debugging Methods

### Enable Serial Output
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1
```

Capture all serial/debug messages.

### Check Boot Progress
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | grep "initialized\|Starting"
```

See what's initializing and where it stops.

### Add Debug Logs
```rust
// In your code
crate::serial_println!("DEBUG: variable = {}", variable);
```

Rebuild and boot to see debug output.

### Use Minimal Config
Comment out non-essential drivers:
```rust
// In kernel/src/main.rs
// if let Err(e) = drivers::ata::init() { }  // Skip ATA
if let Err(e) = drivers::e1000::init(phys_mem_offset) { }  // Skip E1000
```

Narrow down which component fails.

## Getting Help

1. **Check this guide** first for common issues
2. **Read boot messages** for clues
3. **Enable serial output** for debug info
4. **Simplify setup** by disabling drivers
5. **Check GitHub Issues** for similar problems
6. **Ask in Discussions** or open new issue

## Next Steps

- **[Development Setup](setup.md)** — Configure IDE
- **[Building Guide](building.md)** — Build process details
- **[Testing Guide](testing.md)** — How to test changes
