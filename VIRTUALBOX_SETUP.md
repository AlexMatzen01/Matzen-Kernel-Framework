# VirtualBox Setup for Matzen Kernel Framework

This guide explains how to run the Matzen Kernel Framework in **VirtualBox** instead of QEMU. VirtualBox provides superior networking capabilities, making it ideal for testing ICMP (ping), TCP, UDP, and other network protocols without the limitations of QEMU's user-mode networking.

## Why VirtualBox?

| Feature | QEMU User-Mode | VirtualBox Bridged |
|---------|-----------------|-------------------|
| ICMP/Ping | Limited (SLIRP limitation) | ✅ Full support |
| TCP/UDP | ✅ Works | ✅ Works |
| Bridged Networking | ❌ No | ✅ Yes |
| Direct Host Access | ❌ Requires forwarding | ✅ Direct |
| Network Performance | Moderate | Good |
| Setup Complexity | Low | Medium |
| Platform Support | Cross-platform | Windows, Linux, macOS |

## Prerequisites

### Installation

**Windows:**
```bash
# Using Chocolatey (recommended)
choco install virtualbox

# Or download from https://www.virtualbox.org/wiki/Downloads
```

**macOS:**
```bash
brew install virtualbox
```

**Linux:**
```bash
sudo apt-get install virtualbox virtualbox-dkms
# or
sudo dnf install virtualbox
```

### Verify Installation

```bash
VBoxManage --version
```

You should see version output like `7.0.0r...`

## Quick Start

### 1. Build the Kernel

```bash
./build.sh
```

### 2. Run in VirtualBox (Default)

```bash
./run.sh

# Or explicitly:
./run.sh target/x86_64-mfk/debug/mfk-kernel --vbox

# To use QEMU instead:
./run.sh target/x86_64-mfk/debug/mfk-kernel --qemu
```

The runner will automatically:
1. Create disk images from the kernel binary
2. Convert them to VirtualBox VDI format
3. Create a VirtualBox VM named `MFK-mfk-kernel`
4. Configure bridged networking
5. Launch the VM in the GUI

### 3. Configure Networking in the Kernel

Inside the running kernel, configure the network interface:

```bash
# Get IP via DHCP (if your network supports it)
dhclient eth0

# Or set a static IP
ifconfig eth0 192.168.1.100/24
route add default 192.168.1.1

# Test connectivity
ping 8.8.8.8
ping google.com
```

## Networking Configuration

### Bridged Networking (Default)

Bridged networking allows the kernel VM to appear as a device on your physical network:

- **VM gets its own IP** from your router's DHCP
- **Full Layer 2 access** to the physical network
- **ICMP/ping works** without limitations
- **TCP/UDP connections** to any host on the network

```
┌─────────────────────────────────────────┐
│ Host Machine                            │
│  ┌─────────────────────────────────┐   │
│  │ VirtualBox VM (Bridged)         │   │
│  │  eth0 ↔ Bridge ↔ Host eth0     │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
        │
        ↓ (Direct network access)
    Physical Network / Router
```

### Automatic Interface Detection

The runner tries to automatically detect your primary network interface. It checks:
1. `eth0`, `eth1` (Linux)
2. `wlan0`, `wlan1` (Linux wireless)
3. `en0`, `en1` (macOS)

If automatic detection fails, manually set the interface:

```bash
VBoxManage modifyvm MFK-mfk-kernel --bridgeadapter1 "Your Interface Name"
```

To list available interfaces:

**Windows:**
```bash
VBoxManage list bridgedifs
```

**Linux/macOS:**
```bash
ip link show        # Linux
ifconfig            # macOS
```

### Manual Network Configuration

If you need more control, modify the VM after creation:

```bash
# Use NAT networking instead (less recommended)
VBoxManage modifyvm MFK-mfk-kernel --nic1 nat

# Use host-only networking (isolated from host)
VBoxManage modifyvm MFK-mfk-kernel --nic1 hostonly
VBoxManage modifyvm MFK-mfk-kernel --hostonlyadapter1 "vboxnet0"

# Back to bridged
VBoxManage modifyvm MFK-mfk-kernel --nic1 bridged
VBoxManage modifyvm MFK-mfk-kernel --bridgeadapter1 "eth0"
```

## Common Tasks

### View Serial Console Output

The kernel's serial output is logged to `target/mfk-serial.log`:

```bash
tail -f target/mfk-serial.log
```

Or pipe it to the console during boot (requires GUI to be active).

### Stop the VM

```bash
VBoxManage controlvm MFK-mfk-kernel poweroff

# Gracefully shut down
VBoxManage controlvm MFK-mfk-kernel acpipowerbutton
```

### Delete the VM

```bash
# First power off
VBoxManage controlvm MFK-mfk-kernel poweroff 2>/dev/null || true

# Unregister and delete
VBoxManage unregistervm MFK-mfk-kernel --delete
```

### Rebuild the VM

```bash
# Delete old VM
VBoxManage unregistervm MFK-mfk-kernel --delete

# Rebuild kernel
./build.sh

# Run (new VM will be created)
./run.sh
```

### Access VM via SSH

Once networking is configured inside the kernel:

```bash
# From host
ssh root@<vm-ip-address>

# Or via host name (if your network supports it)
ssh root@mfk-kernel.local
```

## Troubleshooting

### VBoxManage Not Found

**Windows:**
- Make sure VirtualBox is installed
- Add `C:\Program Files\Oracle\VirtualBox` to PATH
- Restart terminal/VS Code

**Linux:**
- Install: `sudo apt-get install virtualbox`

**macOS:**
- Install: `brew install virtualbox`

### VM Fails to Start

Check error logs:
```bash
VBoxManage showvminfo MFK-mfk-kernel
```

Delete and recreate:
```bash
VBoxManage unregistervm MFK-mfk-kernel --delete
./run.sh
```

### No Network Connectivity

1. **Inside the VM:** Check interface is up
   ```bash
   ifconfig
   route -n
   ping 8.8.8.8
   ```

2. **Check Bridge Configuration:**
   ```bash
   VBoxManage showvminfo MFK-mfk-kernel | grep -A2 "NIC"
   ```

3. **Verify Host Interface:**
   ```bash
   VBoxManage list bridgedifs
   ```

4. **Reconfigure Bridge:**
   ```bash
   VBoxManage modifyvm MFK-mfk-kernel --bridgeadapter1 "eth0"
   ```

### Ping (ICMP) Not Working

- Verify bridged networking is active
- Check host firewall allows ICMP
- Ensure kernel ICMP implementation is enabled

Test with UDP instead (more reliable):
```bash
# In kernel shell
nc -lu -p 5555 &
nc -u <host-ip> 5555
```

### VM GUI Not Responding

Force stop and check VirtualBox processes:

```bash
# Windows
taskkill /F /IM VirtualBoxVM.exe 2>nul || true

# Linux/macOS
pkill -9 VirtualBoxVM || true
```

## Advanced Configuration

### Increase VM Resources

For better performance:

```bash
# Increase memory (adjust size in MB)
VBoxManage modifyvm MFK-mfk-kernel --memory 512

# Add more CPUs
VBoxManage modifyvm MFK-mfk-kernel --cpus 4

# Increase VRAM for graphics
VBoxManage modifyvm MFK-mfk-kernel --vram 32
```

### Enable 3D Acceleration (Optional)

```bash
VBoxManage modifyvm MFK-mfk-kernel --accelerate3d on
```

### Shared Folders (Optional)

Mount a host folder in the VM:

```bash
# Create shared folder
VBoxManage sharedfolder add MFK-mfk-kernel \
  --name host_share \
  --hostpath /path/to/folder \
  --automount

# In VM, mount it:
mkdir /mnt/share
mount -t vboxsf host_share /mnt/share
```

### Snapshots

Save VM state:

```bash
# Create snapshot
VBoxManage snapshot MFK-mfk-kernel take my-snapshot

# Restore snapshot
VBoxManage snapshot MFK-mfk-kernel restore my-snapshot

# List snapshots
VBoxManage snapshot MFK-mfk-kernel list
```

## Comparison: QEMU vs VirtualBox

### Using QEMU (Alternative)

```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel --qemu
```

**Pros:**
- Lighter weight
- No GUI overhead
- Good for CI/CD

**Cons:**
- User-mode networking limitations
- ICMP/ping doesn't work reliably
- No bridged networking

### Switching Between Hypervisors

It's easy to switch between them:

```bash
# Build once
./build.sh

# Test in VirtualBox
./run.sh --vbox

# Later, test in QEMU
./run.sh --qemu
```

Both use the same kernel binary and disk images.

## Additional Resources

- [VirtualBox Documentation](https://www.virtualbox.org/wiki/Documentation)
- [VBoxManage Command Reference](https://www.virtualbox.org/manual/ch08.html)
- [Matzen Kernel Framework Networking Guide](NETWORKING.md)
- [Kernel Development Guide](docs/development/development.md)

## Migrating from QEMU

If you were previously using QEMU:

1. **No changes needed** to kernel code - it's compatible with both
2. **Run the new runner:**
   ```bash
   ./run.sh  # Uses VirtualBox by default now
   ```
3. **To revert to QEMU:**
   ```bash
   ./run.sh --qemu
   ```
4. **VM resources** are created fresh each time automatically

## Next Steps

- Follow the [Networking Guide](NETWORKING.md) to configure TCP/UDP/ICMP
- Check [Shell Commands](docs/reference/shell-commands.md) for kernel shell usage
- See [Architecture](docs/reference/architecture.md) for network stack details
