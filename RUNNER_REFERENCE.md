# MFK Runner — Command Reference

The `mfk-runner` tool creates bootable disk images and launches them in VirtualBox, QEMU, or Hyper-V.

## Basic Usage

```bash
# Build everything (kernel + runner)
./build.sh

# Run in VirtualBox (default)
./run.sh

# Run in QEMU
./run.sh --qemu

# Run in Hyper-V (Windows, Generation 2 / UEFI, boots the UEFI ISO via DVD)
./run.sh --hyperv --uefi

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
  --hyperv, --hyper-v, --hv
                        Run in Hyper-V (Windows, Generation 2 / UEFI only)
  --hyperv-boot=<dvd|disk>
                        Hyper-V boot device (default dvd: the UEFI ISO)
  --vhdx=<path>         VHDX for --hyperv-boot=disk (auto-converted via
                        qemu-img/Convert-VHD when missing) or extra data disk with dvd boot
  --hyperv-switch=<name>
                        Virtual switch (default: Default Switch, auto-detected)
  --hyperv-mem=<MB>     VM startup memory in MB (default 512)
  --hyperv-cpus=<n>     Virtual CPUs (default 2)
  --hyperv-com=<pipe|off>
                        COM1 named pipe (default MFK-<vm>-com1; capture with tools/hyperv-serial.ps1)
  --uefi                Use UEFI firmware (implied by --hyperv)
  --kbd=<mode>          QEMU keyboard transport (default ps2; QEMU only)
                        ps2  = PS/2 keyboard, no USB controller attached
                        xhci = qemu-xhci, exercised by the xHCI driver
                        ehci = usb-ehci, exercised by the EHCI driver
                        uhci = piix3-usb-uhci, exercised by the UHCI driver
                        ohci = pci-ohci, exercised by the OHCI driver
                        USB modes attach usb-kbd + usb-tablet, which override
                        PS/2 in the guest. `pci-ohci` is a QEMU build option:
                        check `qemu-system-x86_64 -device help` before using
                        `--kbd=ohci`.
  --xhci-kbd            Legacy alias for --kbd=xhci
  --no-run              Only create disk images, don't launch
```

### USB controller selection

The kernel claims a host controller by PCI class code and programming
interface, so the QEMU device must match the driver you want to test:

| `--kbd=` | QEMU `-device` | USB controller | Kernel driver |
| --- | --- | --- | --- |
| `xhci` | `qemu-xhci` | USB 3.x | `kernel/src/drivers/xhci.rs` |
| `ehci` | `usb-ehci` | USB 2.0 high-speed | `kernel/src/drivers/usb.rs` (EHCI) |
| `uhci` | `piix3-usb-uhci` | USB 1.1 | `kernel/src/drivers/uhci.rs` |
| `ohci` | `pci-ohci` | USB 1.0 | `kernel/src/drivers/ohci.rs` |

On `-machine pc` (both the BIOS and UEFI QEMU paths) QEMU already provides
two PIIX3 UHCI controllers on bus `usb-bus.0`. `--kbd=uhci` adds a third with
an explicit id, so the bus under test is predictably named `uhci.0`; run the
`usb` shell command in the guest to see every controller the kernel found and
which were claimed.

## What Gets Created

### Disk Images

For kernel at `target/x86_64-mfk/debug/mfk-kernel`:

- `target/x86_64-mfk/debug/mfk-kernel-uefi.img` — UEFI bootable image
- `target/x86_64-mfk/debug/mfk-kernel-uefi.iso` — UEFI bootable ISO (El Torito,
  for Hyper-V Gen2 DVD boot; created on every run, no external tools needed)
- `target/x86_64-mfk/debug/mfk-kernel-uefi.img.vhdx` — Hyper-V boot disk, only with
  `--hyperv --hyperv-boot=disk` (converted via `qemu-img`/`Convert-VHD` when missing)
- `target/disk.vhdx` — Hyper-V data disk (SCSI, converted from `target/disk.img`)
- `<extra>.vhdx` — one per `--extra-disk=<raw>` (SCSI)
- `target/x86_64-mfk/debug/mfk-kernel-bios.img` — BIOS bootable image
- `target/x86_64-mfk/debug/mfk-kernel-bios.img.vdi` — VirtualBox disk (auto-converted from BIOS image)
- `target/disk.vdi` — Data disk (10 MB, created once)

### Virtual Machine (VirtualBox only)

- **Name:** `MFK-mfk-kernel` (or `MFK-<kernel-name>`)
- **Memory:** 256 MB
- **CPUs:** 2
- **Networking:** Bridged (auto-detects host interface)
- **Serial Console:** Logged to `target/mfk-serial.log`

## Hyper-V Setup (Windows)

Requirements: Windows with the Hyper-V role, an elevated shell, and a
virtual switch (the `Default Switch` is used when present).
Full guide: `HYPERV_SETUP.md`. Preflight: `.\setup-hyperv-windows.ps1`.

```powershell
# Run as Administrator
.\run.ps1 --hyperv --uefi

# With explicit switch / resources / disk boot
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --hyperv `
  --hyperv-switch="Default Switch" --hyperv-mem=512 --hyperv-cpus=2

# Boot from VHDX instead of the ISO (converted via qemu-img/Convert-VHD)
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --hyperv `
  --hyperv-boot=disk
```

Details:

- VM name: `MFK-<kernel>-uefi`, Generation 2, Secure Boot off (the MFK
  loader is unsigned), DVD drive with the UEFI ISO attached.
- `--hyperv-boot=dvd` (default) boots the ISO; `--hyperv-boot=disk` boots
  the auto-converted `.vhdx` (or the file given via `--vhdx=`).
- Data disk `target/disk.img` → `target/disk.vhdx` (SCSI) + each
  `--extra-disk=<raw>` → `<raw>.vhdx` (SCSI); `--bundle-apps` is applied
  before conversion. `--force` rebuilds VHDX + VM.
- Serial: COM1 → named pipe (default `MFK-<vm>-com1`, `--hyperv-com=<pipe|off>`).
  Capture with `.\tools\hyperv-serial.ps1` (log: `target/mfk-hyperv-serial.log`);
  GUI via Hyper-V Manager (`vmconnect.exe`).
- Manage with: `Get-VM MFK-*-uefi`, `Stop-VM -Name <vm>`,
  `Remove-VM -Name <vm> -Force` (with `--force` the runner recreates it).

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
| Native Windows virtualization (UEFI) | Hyper-V |
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
