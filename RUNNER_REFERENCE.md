# MFK Runner — Command Reference

The `mfk-runner` tool creates bootable disk images and launches them in VirtualBox or QEMU.

## Basic Usage

```bash
# Build everything (kernel + runner)
./build.sh

# Run in VirtualBox (default)
./run.sh

# Run in QEMU
./run.sh --qemu

# With custom kernel path
./run.sh target/x86_64-mfk/release/mfk-kernel

# Only create disk images, don't launch
cargo run -p mfk-runner --release -- <kernel-path> --no-run
```

## Runner Options

```bash
cargo run -p mfk-runner --release -- <kernel-binary-path> [OPTIONS]

OPTIONS:
  --vbox, --virtualbox  Run in VirtualBox (default)
  --qemu                Run in QEMU
  --no-run              Only create disk images, don't launch
```

## What Gets Created

### Disk Images

For kernel at `target/x86_64-mfk/debug/mfk-kernel`:

- `target/x86_64-mfk/debug/mfk-kernel-uefi.img` — UEFI bootable image
- `target/x86_64-mfk/debug/mfk-kernel-bios.img` — BIOS bootable image
- `target/x86_64-mfk/debug/mfk-kernel-bios.img.vdi` — VirtualBox disk (auto-converted from BIOS image)
- `target/disk.vdi` — Data disk (10 MB, created once)

### Virtual Machine (VirtualBox only)

- **Name:** `MFK-mfk-kernel` (or `MFK-<kernel-name>`)
- **Memory:** 256 MB
- **CPUs:** 2
- **Networking:** Bridged (auto-detects host interface)
- **Serial Console:** Logged to `target/mfk-serial.log`

## VirtualBox Setup (Windows)

```powershell
# Check VirtualBox prerequisites
.\setup-vbox-windows.ps1
```

## VirtualBox Setup (Linux)

```bash
# Check VirtualBox prerequisites
bash setup-vbox-linux.sh
```

## VirtualBox Setup (macOS)

```bash
# Check VirtualBox prerequisites
bash setup-vbox-macos.sh
```

## Managing VMs

```bash
# List all VMs
VBoxManage list vms

# Get VM details
VBoxManage showvminfo MFK-mfk-kernel

# Start VM (GUI mode)
VBoxManage startvm MFK-mfk-kernel --type gui

# Stop VM gracefully
VBoxManage controlvm MFK-mfk-kernel acpipowerbutton

# Force stop VM
VBoxManage controlvm MFK-mfk-kernel poweroff

# Delete VM
VBoxManage unregistervm MFK-mfk-kernel --delete

# Modify VM (increase memory to 512 MB)
VBoxManage modifyvm MFK-mfk-kernel --memory 512

# Configure networking
VBoxManage modifyvm MFK-mfk-kernel --nic1 bridged
VBoxManage modifyvm MFK-mfk-kernel --bridgeadapter1 eth0
```

## Networking

### VirtualBox (Bridged — Recommended)

**Pros:**
- Full Layer 2 access
- ICMP/ping works perfectly
- Direct host network access
- Best for protocol testing

**Configuration:**
```bash
# Inside kernel
dhclient eth0
ping 8.8.8.8
```

### QEMU (User-mode NAT)

**Pros:**
- No setup needed
- Good for CI/CD

**Cons:**
- ICMP/ping unreliable (SLIRP limitation)

**Configuration:**
```bash
./run.sh --qemu
# Inside kernel
dhclient eth0
```

## Serial Console Output

### VirtualBox

Serial output is logged to `target/mfk-serial.log`:

```bash
# View log
cat target/mfk-serial.log

# Watch in real-time
tail -f target/mfk-serial.log
```

### QEMU

Serial output goes to stdout:

```bash
./run.sh --qemu 2>&1 | tee qemu-output.log
```

## Troubleshooting

### VBoxManage Not Found

```bash
# Windows: Add to PATH
C:\Program Files\Oracle\VirtualBox

# Linux: Install VirtualBox
sudo apt-get install virtualbox

# macOS: Install VirtualBox
brew install virtualbox
```

### VM Won't Start

```bash
# Check VM configuration
VBoxManage showvminfo MFK-mfk-kernel

# Delete and recreate
VBoxManage unregistervm MFK-mfk-kernel --delete
./build.sh
./run.sh
```

### No Network Connectivity

Inside the kernel:

```bash
# Check if interface is up
ifconfig

# Try DHCP
dhclient eth0

# Try static IP
ifconfig eth0 192.168.1.100/24
route add default 192.168.1.1

# Verify routing
route -n
```

### Ping/ICMP Not Working

- Verify bridged networking: `VBoxManage showvminfo MFK-mfk-kernel | grep "NIC 1"`
- Check host firewall (may block ICMP)
- Verify host interface is up: `ip link show` (Linux) or `ipconfig` (Windows)

### Disk Space Issues

```bash
# Clean up old VDI files
rm -f target/x86_64-mfk/debug/*.vdi
rm -f target/disk.vdi

# Rebuild
./build.sh
./run.sh
```

## Performance Tuning

### Increase VM Resources

```bash
# Adjust memory (MB)
VBoxManage modifyvm MFK-mfk-kernel --memory 512

# Adjust CPU count
VBoxManage modifyvm MFK-mfk-kernel --cpus 4

# Enable 3D acceleration (optional)
VBoxManage modifyvm MFK-mfk-kernel --accelerate3d on
```

### Increase I/O Performance

```bash
# Use better disk cache strategy
VBoxManage storageattach MFK-mfk-kernel \
  --storagectl IDE \
  --port 0 \
  --device 0 \
  --cachemode wt  # Write-through
```

## Advanced: Snapshots

```bash
# Create snapshot
VBoxManage snapshot MFK-mfk-kernel take my-snapshot

# Restore to snapshot
VBoxManage snapshot MFK-mfk-kernel restore my-snapshot

# List snapshots
VBoxManage snapshot MFK-mfk-kernel list

# Delete snapshot
VBoxManage snapshot MFK-mfk-kernel delete my-snapshot
```

## Comparing with QEMU

### Build Once, Run on Both

```bash
./build.sh

# Test on VirtualBox
./run.sh

# Later, test on QEMU for comparison
./run.sh --qemu
```

### When to Use Each

| Use Case | Hypervisor |
|----------|-----------|
| Interactive development | VirtualBox |
| Network protocol testing | VirtualBox |
| ICMP/ping testing | VirtualBox |
| CI/CD pipelines | QEMU |
| Lightweight headless | QEMU |
| Legacy testing | QEMU |

## More Information

- [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) — Detailed VirtualBox configuration
- [MIGRATION_QEMU_TO_VBOX.md](MIGRATION_QEMU_TO_VBOX.md) — Migration guide from QEMU
- [NETWORKING.md](NETWORKING.md) — Network protocol details
- [README.md](README.md) — Project overview
