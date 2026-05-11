# Migration Guide: QEMU to VirtualBox

This guide helps users transition from QEMU to VirtualBox for running the Matzen Kernel Framework.

## What Changed?

- **Default hypervisor** is now **VirtualBox** instead of QEMU
- **Networking** is dramatically improved with bridged networking support
- **ICMP/ping** now works reliably without workarounds
- **QEMU is still supported** as an alternative with a CLI flag
- **No changes to kernel code** — completely backwards compatible

## Quick Migration

### 1. Install VirtualBox

If you haven't already:

**Windows:**
```bash
choco install virtualbox
```

**macOS:**
```bash
brew install virtualbox
```

**Linux:**
```bash
sudo apt-get install virtualbox virtualbox-dkms  # Debian/Ubuntu
sudo dnf install virtualbox                       # Fedora
```

Verify: `VBoxManage --version`

### 2. Rebuild the Kernel

```bash
./build.sh
```

Nothing special needed — the new runner will be built automatically.

### 3. Run in VirtualBox

```bash
./run.sh
```

That's it! The runner will:
- Create disk images (same as before)
- Convert to VirtualBox VDI format
- Create a VM with bridged networking
- Launch it in the GUI

### 4. Configure Networking

Inside the kernel shell:

```bash
dhclient eth0          # Get IP from DHCP
ping 8.8.8.8           # Now works reliably!
```

## Going Back to QEMU

If you need to use QEMU for any reason:

```bash
./run.sh --qemu
```

Both hypervisors use the same kernel binary and disk images.

## What's Better in VirtualBox?

| Issue | QEMU | VirtualBox |
|-------|------|-----------|
| **Ping/ICMP** | ❌ Limited (SLIRP) | ✅ Full support |
| **Network Access** | ❌ NAT only | ✅ Bridged + NAT + host-only |
| **Performance** | Moderate | Good |
| **VM Management** | CLI-only | GUI + VBoxManage CLI |
| **Snapshots** | Not supported | ✅ Full snapshot support |
| **Monitoring** | Terminal output | ✅ GUI console + serial log |

## Why This Matters for Networking

### Before (QEMU):
```
Kernel → E1000 → SLIRP NAT → Host → Internet
         (ICMP often fails due to SLIRP limitation)
```

### After (VirtualBox):
```
Kernel → E1000 → Bridged adapter → Host network → Internet
         (Full Layer 2 access, ICMP works perfectly)
```

## Common Tasks

### View Serial Console

QEMU output went to `stdio`. VirtualBox logs to `target/mfk-serial.log`:

```bash
tail -f target/mfk-serial.log
```

### Stop/Delete VM

```bash
# Power off
VBoxManage controlvm MFK-mfk-kernel poweroff

# Delete
VBoxManage unregistervm MFK-mfk-kernel --delete
```

### SSH to VM

Once networking is configured:

```bash
ssh root@<vm-ip>
```

## Troubleshooting Migration

### Error: "VBoxManage not found"

Make sure VirtualBox is in PATH:

**Windows:** Add `C:\Program Files\Oracle\VirtualBox` to PATH

**Linux/macOS:** `which VBoxManage` should find it; if not, reinstall VirtualBox

### Error: "Failed to convert image"

The runner tried to convert BIOS image to VDI but VBoxManage failed. Check:

```bash
VBoxManage --version  # Should work
```

If it fails, VirtualBox may not be properly installed.

### VM Won't Start

Delete and recreate:

```bash
VBoxManage unregistervm MFK-mfk-kernel --delete
./build.sh
./run.sh
```

### No Network Access

Inside the kernel:

```bash
ifconfig                    # Check if eth0 is present
route -n                    # Check routing
ping -c 1 192.168.1.1      # Ping gateway
```

If no eth0, the network card may not be recognized. Check [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) for bridge configuration.

## Performance Comparison

- **VirtualBox:** Better overall performance, especially for network ops
- **QEMU:** Lighter weight, no GUI overhead (useful for CI/headless testing)

Both are viable; VirtualBox is recommended for interactive development.

## Staying with QEMU

If you prefer QEMU (e.g., for CI/CD pipelines):

```bash
# Every run
./run.sh --qemu

# Or update your scripts
```

The kernel doesn't care which hypervisor runs it.

## FAQ

**Q: Do I need to rebuild the kernel?**  
A: No, the same binary works with both hypervisors. Just rebuild the runner: `./build.sh`

**Q: Can I run both at the same time?**  
A: Yes, but each needs unique resources. The runner creates separate VMs/images.

**Q: Will my old disk images work?**  
A: The old QEMU `.img` files are still there. VirtualBox creates separate `.vdi` versions.

**Q: How much disk space do I need?**  
A: BIOS image (~100KB) + VDI conversion (~10MB data disk) = ~10MB per kernel.

**Q: Can I share the network interface?**  
A: The runner auto-detects the interface. For multiple VMs, ensure your bridge supports it.

## Next Steps

- Read [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) for advanced networking
- Check [NETWORKING.md](NETWORKING.md) for network stack details
- See [test_commands.txt](test_commands.txt) for testing examples

## Need Help?

- Check [docs/development/troubleshooting.md](../docs/development/troubleshooting.md)
- Review [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) networking section
- See copilot-instructions.md for development workflows
