# File Index — MFK Project Structure

This document lists all important files in the Matzen Kernel Framework and their purposes.

## Quick Navigation

- **Start Here:** [README.md](README.md)
- **Windows Users:** [WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md) or [CMD_QUICKSTART.md](CMD_QUICKSTART.md) or [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md)
- **Linux/macOS Users:** [README.md](README.md) (Quick Start section)
- **Networking:** [NETWORKING.md](NETWORKING.md)
- **VirtualBox:** [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md)

## Build & Run Scripts

| File | OS | Shell | Purpose |
|------|-----|-------|---------|
| `build.sh` | Linux/macOS | Bash | Build kernel + runner |
| `run.sh` | Linux/macOS | Bash | Run kernel in VirtualBox/QEMU |
| `install.sh` | Linux/macOS | Bash | Install Rust toolchain |
| `build.ps1` | Windows | PowerShell | Build kernel + runner |
| `run.ps1` | Windows | PowerShell | Run kernel in VirtualBox/QEMU |
| `install.ps1` | Windows | PowerShell | Install Rust toolchain |
| `build.bat` | Windows | Batch/CMD | Build kernel + runner |
| `run.bat` | Windows | Batch/CMD | Run kernel in VirtualBox/QEMU |
| `install.bat` | Windows | Batch/CMD | Install Rust toolchain |

## Setup Helpers

| File | OS | Shell | Purpose |
|------|-----|-------|---------|
| `setup-vbox-windows.ps1` | Windows | PowerShell | Verify VirtualBox prerequisites |
| `setup-vbox-linux.sh` | Linux | Bash | Verify VirtualBox prerequisites |
| `setup-vbox-macos.sh` | macOS | Bash | Verify VirtualBox prerequisites |

## Documentation

### Main Guides

| File | Audience | Purpose |
|------|----------|---------|
| [README.md](README.md) | Everyone | Project overview, quick start |
| [WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md) | Windows Users | Complete Windows support guide |
| [CMD_QUICKSTART.md](CMD_QUICKSTART.md) | Windows Users | Command Prompt quick start |
| [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md) | Windows Users | PowerShell quick start |

### Feature Guides

| File | Purpose |
|------|---------|
| [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md) | VirtualBox configuration and troubleshooting |
| [NETWORKING.md](NETWORKING.md) | Network protocol stack documentation |
| [MIGRATION_QEMU_TO_VBOX.md](MIGRATION_QEMU_TO_VBOX.md) | Migrating from QEMU to VirtualBox |
| [ICMP_STATUS.md](ICMP_STATUS.md) | ICMP/ping status and issues |
| [TCP_GUIDE.md](TCP_GUIDE.md) | TCP implementation guide |
| [ASYNC_RECEIVE.md](ASYNC_RECEIVE.md) | Async receive documentation |
| [CTRL_C_INTERRUPT.md](CTRL_C_INTERRUPT.md) | Ctrl+C interrupt handling |

### Reference

| File | Purpose |
|------|---------|
| [RUNNER_REFERENCE.md](RUNNER_REFERENCE.md) | Detailed runner tool command reference |
| [docs/reference/architecture.md](docs/reference/architecture.md) | Kernel architecture overview |
| [docs/reference/drivers.md](docs/reference/drivers.md) | Driver documentation |
| [docs/reference/interrupts.md](docs/reference/interrupts.md) | Interrupt handling |
| [docs/reference/memory.md](docs/reference/memory.md) | Memory management |
| [docs/reference/networking.md](docs/reference/networking.md) | Network stack reference |
| [docs/reference/shell-commands.md](docs/reference/shell-commands.md) | Shell command reference |

### Development

| File | Purpose |
|------|---------|
| [docs/development/building.md](docs/development/building.md) | Building the project |
| [docs/development/contributing.md](docs/development/contributing.md) | Contributing guidelines |
| [docs/development/extending.md](docs/development/extending.md) | Extending the kernel |
| [docs/development/setup.md](docs/development/setup.md) | Development setup |
| [docs/development/testing.md](docs/development/testing.md) | Testing guide |
| [docs/development/troubleshooting.md](docs/development/troubleshooting.md) | Troubleshooting |

### Getting Started

| File | Purpose |
|------|---------|
| [docs/guide/installation.md](docs/guide/installation.md) | Installation guide |
| [docs/guide/quick-start.md](docs/guide/quick-start.md) | Quick start guide |
| [docs/guide/running.md](docs/guide/running.md) | Running the kernel |
| [docs/index.md](docs/index.md) | Documentation index |

## Test Files

| File | Purpose |
|------|---------|
| [test_commands.txt](test_commands.txt) | Example shell commands for testing |
| [tcp_test_commands.txt](tcp_test_commands.txt) | TCP test commands |
| [test_ping.sh](test_ping.sh) | Ping testing script |
| [test_tcp.sh](test_tcp.sh) | TCP testing script |
| [network_test.txt](network_test.txt) | Network test commands |

## Source Code

| Directory | Purpose |
|-----------|---------|
| [kernel/](kernel/) | Main kernel source (`mfk-kernel` crate) |
| `kernel/src/main.rs` | Kernel entry point |
| `kernel/src/allocator.rs` | Memory allocator |
| `kernel/src/drivers/` | Device drivers (VGA, serial, ATA, E1000, etc.) |
| `kernel/src/fs/` | Filesystem implementation |
| `kernel/src/net/` | Network stack (Ethernet, ARP, IP, ICMP, UDP, TCP) |
| `kernel/src/shell/` | Interactive shell |
| [tools/](tools/) | Runner tool source (`mfk-runner` crate) |
| `tools/src/main.rs` | Runner entry point (disk image creation, VM launch) |

## Configuration Files

| File | Purpose |
|------|---------|
| [Cargo.toml](Cargo.toml) | Workspace configuration |
| [kernel/Cargo.toml](kernel/Cargo.toml) | Kernel crate configuration |
| [tools/Cargo.toml](tools/Cargo.toml) | Runner tool crate configuration |
| [targets/x86_64-mfk.json](targets/x86_64-mfk.json) | Custom Rust target specification |
| [rust-toolchain.toml](rust-toolchain.toml) | Rust version configuration |
| [.github/copilot-instructions.md](.github/copilot-instructions.md) | AI coding guidelines |

## Build Artifacts

| Location | Purpose |
|----------|---------|
| `target/x86_64-mfk/debug/` | Debug kernel binary and images |
| `target/x86_64-mfk/release/` | Release kernel binary and images |
| `target/release/mfk-runner` | Compiled runner tool |
| `target/disk.vdi` | Data disk image |
| `target/mfk-serial.log` | Kernel serial output (VirtualBox) |

## Choosing Your Starting Point

### "I want to build and run the kernel"

1. Start: [README.md](README.md) → Quick Start section
2. If on Windows: [WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md)
3. If on Linux/macOS: Continue with bash scripts

### "I want to understand the architecture"

1. [docs/reference/architecture.md](docs/reference/architecture.md)
2. [docs/reference/drivers.md](docs/reference/drivers.md)
3. [docs/reference/networking.md](docs/reference/networking.md)

### "I want to test networking"

1. [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md)
2. [NETWORKING.md](NETWORKING.md)
3. [test_commands.txt](test_commands.txt) (for shell commands)

### "I want to contribute"

1. [docs/development/contributing.md](docs/development/contributing.md)
2. [docs/development/extending.md](docs/development/extending.md)
3. [.github/copilot-instructions.md](.github/copilot-instructions.md) (development patterns)

### "I'm having problems"

1. [docs/development/troubleshooting.md](docs/development/troubleshooting.md)
2. Platform-specific:
   - Windows: [WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md) or [CMD_QUICKSTART.md](CMD_QUICKSTART.md)
   - Network: [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md)
   - VirtualBox: [ICMP_STATUS.md](ICMP_STATUS.md)

## File Organization

```
Matzen-Kernel-Framework/
├── Kernel source
│   ├── kernel/
│   │   └── src/
│   │       ├── main.rs
│   │       ├── allocator.rs
│   │       ├── drivers/
│   │       ├── fs/
│   │       ├── net/
│   │       └── shell/
│   └── Cargo.toml
│
├── Build & run tools
│   ├── tools/
│   │   └── src/
│   │       └── main.rs
│   ├── Cargo.toml
│   ├── build.sh / build.ps1 / build.bat
│   ├── run.sh / run.ps1 / run.bat
│   └── install.sh / install.ps1 / install.bat
│
├── Documentation
│   ├── README.md (START HERE)
│   ├── WINDOWS_SUPPORT.md
│   ├── CMD_QUICKSTART.md
│   ├── POWERSHELL_QUICKSTART.md
│   ├── VIRTUALBOX_SETUP.md
│   ├── NETWORKING.md
│   ├── docs/
│   │   ├── guide/
│   │   ├── reference/
│   │   └── development/
│   └── [other guides]
│
├── Testing
│   ├── test_commands.txt
│   ├── tcp_test_commands.txt
│   └── [test scripts]
│
└── Configuration
    ├── Cargo.toml
    ├── targets/x86_64-mfk.json
    ├── rust-toolchain.toml
    └── .github/copilot-instructions.md
```

## File Size Notes

- Kernel binary: ~50-100 KB (debug), smaller (release)
- UEFI image: ~200 KB
- BIOS image: ~100 KB
- VDI image: ~10 MB (data disk)
- Runner binary: ~5-10 MB (debug), 3-5 MB (release)

## Quick Commands Reference

```bash
# Linux/macOS
./install.sh && ./build.sh && ./run.sh

# Windows (Batch)
install.bat && build.bat && run.bat

# Windows (PowerShell)
.\install.ps1; .\build.ps1; .\run.ps1

# Manual (any platform)
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
cargo build -p mfk-runner --release
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

## Still Lost?

Every major guide starts with a "Quick Start" section. Pick your platform and follow it:

- **Windows Command Prompt:** [CMD_QUICKSTART.md](CMD_QUICKSTART.md)
- **Windows PowerShell:** [POWERSHELL_QUICKSTART.md](POWERSHELL_QUICKSTART.md)
- **Linux/macOS:** [README.md](README.md) → Quick Start
- **Networking Issues:** [VIRTUALBOX_SETUP.md](VIRTUALBOX_SETUP.md)
- **General Issues:** [docs/development/troubleshooting.md](docs/development/troubleshooting.md)

Good luck! 🚀
