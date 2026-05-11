# PowerShell Quick Start — MFK on Windows

This guide walks you through getting the Matzen Kernel Framework running on Windows using PowerShell.

## Prerequisites

1. **PowerShell 5.1 or higher** (built into Windows 10/11)
   - Check: `$PSVersionTable.PSVersion`

2. **Rust** — with nightly toolchain
   - Install from: https://rustup.rs/
   - Or: `choco install rust` (if using Chocolatey)

3. **VirtualBox** (recommended for networking)
   - Install from: https://www.virtualbox.org/wiki/Downloads
   - Or: `choco install virtualbox`

## One-Time Setup

### Step 1: Open PowerShell

**Windows 10/11:**
- Press `Win + X`, select "Windows PowerShell (Admin)" or "Terminal (Admin)"
- Or search for "PowerShell" and run as Administrator

**Note:** Some scripts may require Administrator privileges for VirtualBox setup.

### Step 2: Enable Script Execution

If you get an error about scripts not being allowed to run, enable execution:

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

This allows locally-created scripts to run while maintaining security.

### Step 3: Run Installation Script

```powershell
# Navigate to the project directory
cd C:\path\to\Matzen-Kernel-Framework

# Run the installation script
.\install.ps1
```

This will:
- ✓ Verify Rust is installed
- ✓ Install nightly toolchain
- ✓ Add required components (`rust-src`, `llvm-tools-preview`)
- ✓ Check VirtualBox installation

### Step 4: Verify VirtualBox Setup

```powershell
.\setup-vbox-windows.ps1
```

This will:
- ✓ Verify VirtualBox and VBoxManage are accessible
- ✓ List available network interfaces
- ✓ Show any existing MFK VMs

## Build and Run

### Quick Build

```powershell
.\build.ps1
```

Output:
```
Building MFK Kernel Framework...

[1/2] Building kernel...
✓ Kernel build complete

[2/2] Building runner...
✓ Runner build complete

======================================
Build Complete!
======================================

Next, run the kernel:
  .\run.ps1
```

### Quick Run

```powershell
.\run.ps1
```

The runner will:
1. Create bootable BIOS/UEFI disk images
2. Convert to VirtualBox VDI format
3. Create a new VM with bridged networking
4. Boot the kernel in VirtualBox GUI

**Note:** The first run takes longer (VM creation). Subsequent runs reuse the VM.

## Inside the Kernel

Once the kernel boots and you see the shell prompt:

### Configure Network

```bash
# Get IP via DHCP
dhclient eth0

# Or set static IP
ifconfig eth0 192.168.1.100/24
route add default 192.168.1.1
```

### Test Network

```bash
# Ping a host (now works perfectly with VirtualBox!)
ping 8.8.8.8
ping google.com

# Show network status
netstat

# Show interface info
ifconfig
```

### Try Filesystem Commands

```bash
# Format filesystem
mkfs

# Mount filesystem
mount

# Create a file
touch myfile.txt

# Write to file
write myfile.txt "Hello from MFK!"

# Read file
cat myfile.txt

# List files
ls
```

### System Commands

```bash
# Show help
help

# Show uptime
uptime

# Show memory info
mem

# Clear screen
clear

# Reboot
reboot
```

## Common Tasks

### Run with QEMU Instead

```powershell
.\run.ps1 --qemu
```

### Use a Different Kernel Build

```powershell
.\run.ps1 target/x86_64-mfk/release/mfk-kernel
```

### Only Create Images (Don't Run)

```powershell
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run
```

### Rebuild Everything

```powershell
# Clean builds
cargo clean
.\build.ps1

# Then run
.\run.ps1
```

### View Serial Output

VirtualBox logs kernel output to `target/mfk-serial.log`:

```powershell
# View the log
type target/mfk-serial.log

# Watch in real-time (needs Get-Content loop)
Get-Content target/mfk-serial.log -Wait
```

### Delete VM and Start Fresh

```powershell
# Unregister and delete the VM
VBoxManage unregistervm MFK-mfk-kernel --delete

# Rebuild
.\build.ps1

# Run (new VM will be created)
.\run.ps1
```

## Troubleshooting

### PowerShell Won't Run Scripts

```powershell
# Enable script execution for current user
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser

# Then try again
.\install.ps1
```

### "VBoxManage" Not Found

VirtualBox may not be in PATH. Verify:

```powershell
VBoxManage --version
```

If that fails:
1. **Install VirtualBox:** https://www.virtualbox.org/wiki/Downloads
2. **Or via Chocolatey:** `choco install virtualbox`
3. **Restart PowerShell after installation**

### Build Fails with Toolchain Error

```powershell
# Update Rust
rustup update

# Reinstall nightly
rustup toolchain uninstall nightly
rustup toolchain install nightly

# Then rebuild
.\build.ps1
```

### Cargo Command Not Found

Rust wasn't added to PATH. Close PowerShell and reopen, or:

```powershell
# Manually source rustup
. $env:USERPROFILE\.cargo\env
cargo --version
```

### VM Creation Fails

```powershell
# Check VirtualBox status
VBoxManage showvminfo MFK-mfk-kernel

# Delete and retry
VBoxManage unregistervm MFK-mfk-kernel --delete
.\build.ps1
.\run.ps1
```

## Available Commands Reference

### Build Commands

```powershell
.\build.ps1                           # Build kernel + runner
.\build.ps1 -Verbose                  # With verbose output (if added)
```

### Run Commands

```powershell
.\run.ps1                             # Run in VirtualBox (default)
.\run.ps1 --qemu                      # Run in QEMU
.\run.ps1 target/x86_64-mfk/release   # Use release build
.\run.ps1 --vbox                      # Explicitly use VirtualBox
```

### Setup/Install Commands

```powershell
.\install.ps1                         # Verify Rust setup
.\setup-vbox-windows.ps1              # Verify VirtualBox setup
```

### Manual Commands

```powershell
# Build kernel manually
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Build runner manually
cargo build -p mfk-runner --release

# Run runner manually
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## Workflow Tips

### Fast Iteration

```powershell
# Build once
.\build.ps1

# Run multiple times to test different scenarios
.\run.ps1              # First run
# ... test kernel ...
# Stop VM (close window or Ctrl+C)

.\run.ps1              # Second run (reuses existing VM)
# ... test again ...
```

### Keep Serial Log Open

While developing, keep another PowerShell window showing the log:

**Terminal 1:**
```powershell
.\run.ps1
```

**Terminal 2:**
```powershell
Get-Content target/mfk-serial.log -Wait
```

### Use `cargo` Directly for Speed

For rapid development, use cargo directly (faster than the script):

```powershell
# Faster iteration (skip script overhead)
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Then run
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## More Information

- **README.md** — Project overview and architecture
- **VIRTUALBOX_SETUP.md** — Advanced VirtualBox configuration
- **NETWORKING.md** — Network stack documentation
- **MIGRATION_QEMU_TO_VBOX.md** — Switching from QEMU
- **RUNNER_REFERENCE.md** — Runner tool details

## Next Steps

1. ✅ Run `.\install.ps1` to set up Rust
2. ✅ Run `.\setup-vbox-windows.ps1` to verify VirtualBox
3. ✅ Run `.\build.ps1` to build the project
4. ✅ Run `.\run.ps1` to boot the kernel
5. Inside kernel: `dhclient eth0` then `ping 8.8.8.8`

Good luck, and happy kernel development! 🚀
