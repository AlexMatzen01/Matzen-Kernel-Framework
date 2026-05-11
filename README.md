# Matzen Kernel Framework (MFK)

A small educational terminal OS written in Rust for x86_64. MFK contains a tiny kernel (`mfk-kernel`) with VGA driver, PS/2 keyboard support, ATA disk driver, filesystem, networking (E1000), and a built-in shell. The `mfk-runner` tool builds bootable disk images and runs them in **VirtualBox** (or optionally QEMU).

**Status:** active development — use for experimentation and learning.

**Platform Support:**
- ✅ **Windows:** PowerShell (`.ps1`) or Batch (`.bat`) scripts
- ✅ **Linux/macOS:** Bash (`.sh`) scripts
- ✅ **All platforms:** Direct `cargo` commands

## Features
- VGA text mode output
- PS/2 keyboard input
- ATA PIO disk driver
- SimplFS filesystem (custom implementation)
- Intel E1000 network driver
- Network stack: Ethernet, ARP, IPv4, ICMP, UDP, TCP
- Interactive shell with file and network commands

**Repository layout (important files):**

- `Cargo.toml`              : Workspace configuration (members: `kernel`, `tools`)
- `kernel/`                 : Kernel crate (`mfk-kernel`)
- `tools/`                  : Runner tool (`mfk-runner`) — creates disk images and runs VirtualBox/QEMU
- `targets/x86_64-mfk.json` : Custom target specification used for building the kernel
- `build.sh` / `build.ps1` / `build.bat` : Build scripts (choose your OS)
- `run.sh` / `run.ps1` / `run.bat`       : Run scripts (choose your OS)
- `install.sh` / `install.ps1` / `install.bat` : Setup scripts (choose your OS)
- `test_commands.txt`       : Example shell commands to exercise the kernel's filesystem/shell
- `NETWORKING.md`           : Networking documentation and usage guide
- `VIRTUALBOX_SETUP.md`     : VirtualBox setup and configuration guide
- `WINDOWS_SUPPORT.md`      : Complete Windows support guide (batch + PowerShell)

## Prerequisites

### Core Requirements
- Rust (nightly) and build tools
- **VirtualBox** (default hypervisor, provides unrestricted networking)
  - Alternative: QEMU (use `./run.sh --qemu`)

### Installation by Platform

**Linux:**
```bash
# Install VirtualBox
sudo apt-get install virtualbox virtualbox-dkms  # Debian/Ubuntu
sudo dnf install virtualbox                       # Fedora/RHEL
```

**macOS:**
```bash
brew install virtualbox
```

**Windows:**
```bash
choco install virtualbox
# or download from https://www.virtualbox.org/wiki/Downloads
```

**Verify Installation:**
```bash
VBoxManage --version
```

## Quick Start

### On Windows (Choose One)

#### Option 1: Command Prompt (Simplest)

```batch
install.bat
build.bat
run.bat
```

See [CMD_QUICKSTART.md](CMD_QUICKSTART.md) for details.

#### Option 2: PowerShell (More Features)

```powershell
.\install.ps1
.\build.ps1
.\run.ps1
```

See [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md) for details.

**Note:** If PowerShell gives a script execution error, run first:
```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

#### Option 3: Direct Cargo Commands (Advanced)

```batch
# Install prerequisites
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly

# Build
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
cargo build -p mfk-runner --release

# Run
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

### On Linux/macOS (Bash)

```bash
./install.sh
./build.sh
./run.sh
```

The runner will automatically:
- Create bootable disk images
- Convert to VirtualBox format (VDI)
- Create a new VM with bridged networking
- Boot the kernel

4. Inside the kernel shell, configure networking:

```bash
# DHCP (recommended)
dhclient eth0

# Or static IP
ifconfig eth0 192.168.1.100/24
route add default 192.168.1.1

# Test connectivity
ping 8.8.8.8
ping google.com
```

For more details, see [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md).

## Switching Between Hypervisors

VirtualBox is the default for better networking support. To use QEMU:

**PowerShell:**
```powershell
.\run.ps1 --qemu
```

**Bash:**
```bash
./run.sh --qemu
```

Both use the same kernel binary and disk images.

## Manual Build Steps

**PowerShell (Windows):**
```powershell
# Build kernel
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Build runner
cargo build -p mfk-runner --release
```

**Bash (Linux/macOS):**
```bash
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

cargo build -p mfk-runner --release
```

## Using the Runner

The runner creates BIOS and UEFI bootable disk images and runs them in VirtualBox (or QEMU).

**PowerShell (Windows):**
```powershell
# Run in VirtualBox (default)
.\run.ps1

# Run with QEMU
.\run.ps1 --qemu

# Custom kernel path
.\run.ps1 target/x86_64-mfk/release/mfk-kernel

# Only create images, don't run
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run
```

**Bash (Linux/macOS):**
```bash
# Run in VirtualBox (default)
./run.sh

# Run with QEMU
./run.sh --qemu

# Custom kernel path
./run.sh target/x86_64-mfk/release/mfk-kernel

# Only create images, don't run
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run
```

## What `mfk-runner` Does

- Creates UEFI and BIOS disk images named like `<kernel-path>-uefi.img` and `<kernel-path>-bios.img`.
- Converts BIOS image to VirtualBox VDI format (`<kernel-path>-bios.img.vdi`).
- Creates/uses a small `target/disk.vdi` (10MB) for data storage.
- When not passed `--no-run`, `mfk-runner` launches the VM:
  - **VirtualBox (default):** Creates a new VM with bridged networking, full Layer 2 access, and unrestricted ICMP/ping support.
  - **QEMU (with `--qemu`):** Uses user-mode networking with limited ICMP support; useful for comparison testing.
- Serial output is logged to `target/mfk-serial.log` (VirtualBox) or redirected to stdio (QEMU).

## Running and Testing the Kernel

1. **Boot the VM in VirtualBox** (see [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) for detailed networking setup):
   ```bash
   ./run.sh
   ```

2. **Configure networking inside the kernel:**
   ```bash
   dhclient eth0    # Get IP via DHCP
   ping 8.8.8.8     # Test connectivity
   ```

3. **Use the shell** to test filesystem, network, and system commands.

See [test_commands.txt](test_commands.txt) for quick smoke tests and [NETWORKING.md](NETWORKING.md) for protocol details.

## Available Commands

**Filesystem commands:**
- `mkfs`       : Format disk with SimplFS
- `mount`      : Mount the filesystem
- `ls`/`dir`   : List files
- `touch <f>`  : Create a file
- `cat <f>`    : Display file contents
- `write <f> <text>`: Write to a file
- `rm <f>`     : Delete a file

**Network commands** (see [NETWORKING.md](NETWORKING.md) for details):
- `ifconfig [ip]`: Configure/display network interface
- `ping <ip>`  : Send ICMP echo request
- `netstat`    : Display network status

**Shell commands:**
- `help`       : Show available shell commands
- `clear`/`cls`: Clear the screen
- `echo <x>`   : Print text
- `about`      : Show project info
- `uptime`     : Simulated uptime
- `mem`/`memory`: Show memory info
- `reboot`, `halt`, `shutdown`: System control commands

## Development Architecture

- The `kernel` crate (`mfk-kernel`) depends on `bootloader_api`, configured as binary `mfk-kernel` (see [kernel/Cargo.toml](kernel/Cargo.toml)).
- The `tools` crate (`mfk-runner`) depends on `bootloader` and implements disk image creation + VM launch logic.
- For architecture details, see [docs/reference/architecture.md](docs/reference/architecture.md).

## Contributing

- Fork the repository, create a branch, and open a pull request.
- Keep changes small and focused.
- For questions or issues, check [docs/development/troubleshooting.md](docs/development/troubleshooting.md).

## License

- MIT License — see [LICENSE](LICENSE) for details.

## Contact / Author
- Alexander Matzen — repository owner

If anything in this README is out of date, please open an issue or send a PR with suggested corrections.
