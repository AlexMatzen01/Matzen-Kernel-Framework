# Matzen Kernel Framework Documentation

Welcome to the comprehensive documentation for the **Matzen Kernel Framework (MFK)** â€” a small educational terminal OS written in Rust for x86_64.

## Quick Navigation

### ðŸš€ Getting Started
- **[Quick Start Guide](guide/quick-start.md)** â€” Set up and run the kernel in 5 minutes
- **[Installation Guide](guide/installation.md)** â€” Detailed setup instructions
- **[Running the Kernel](guide/running.md)** â€” Boot and interact with MFK

### ðŸ“š Core Documentation
- **[Architecture Overview](reference/architecture.md)** â€” System design and components
- **[Kernel API Reference](reference/api.md)** â€” Core kernel functions and structures
- **[Memory Management](reference/memory.md)** â€” Heap allocation and memory layout
- **[Interrupt Handling](reference/interrupts.md)** â€” IDT, PIC, and exception handling

### ðŸŒ Features & Subsystems
- **[Network Stack](reference/networking.md)** â€” Ethernet, IP, ARP, ICMP, and networking
- **[File System](reference/filesystem.md)** â€” SimpleFS implementation and usage
- **[Shell Commands](reference/shell-commands.md)** â€” Built-in terminal commands
- **[Hardware Drivers](reference/drivers.md)** â€” VGA, Keyboard, USB (EHCI/xHCI/UHCI/OHCI), ATA, E1000, Serial, RTC, PIC

### ðŸ”§ Development
- **[Development Setup](development/setup.md)** â€” Build environment configuration
- **[Building the Kernel](development/building.md)** â€” Compilation flags and process
- **[Testing Guide](development/testing.md)** â€” Running tests and debugging
- **[Extending MFK](development/extending.md)** â€” Adding new features and modules
- **[Troubleshooting](development/troubleshooting.md)** â€” Common issues and fixes

### ðŸ’¡ Advanced Topics
- **[Network Implementation Details](reference/networking-deep-dive.md)** â€” Protocol stack internals
- **[Shell Implementation](reference/shell-internals.md)** â€” Command parsing and execution
- **[Contributing Guide](development/contributing.md)** â€” How to contribute to the project

---

## What is MFK?

The **Matzen Kernel Framework** is an educational operating system kernel written entirely in Rust. It provides:

- **VGA Text Mode Output** â€” 80Ã—25 character terminal
- **PS/2 + USB HID Keyboard Input** - Real-time keyboard support over PS/2 or any USB host controller
- **TCP/IP Network Stack** â€” Partial networking with Ethernet, IPv4, ARP, ICMP
- **SimpleFS File System** â€” Basic disk I/O and file management
- **Built-in Shell** â€” Interactive command-line interface
- **BIOS/UEFI Boot** â€” Bootable via multiple firmware types
- **Hardware Drivers** â€” VGA, ATA, E1000 NIC, Serial, RTC
- **Interrupt Handling** â€” IDT, PIC, keyboard, serial interrupts

---

## Key Features

### ðŸŽ¯ Pure Rust
MFK is written entirely in Rust with no C dependencies, leveraging Rust's memory safety to prevent common kernel bugs.

### ðŸ“– Educational Focus
Clean, well-documented code designed to teach OS concepts:
- Memory management and heap allocation
- Interrupt handling and exception processing
- Network protocol implementation
- File system design
- Shell command parsing

### ðŸ”Œ Hardware Support
- Intel E1000 NIC (network)
- ATA disk drives (storage)
- PS/2 keyboard and USB HID keyboards via EHCI/xHCI/UHCI/OHCI (input)
- VGA text mode (output)
- Serial port (debugging)
- Real-time clock (timing)

### ðŸš€ Real Hardware Capable
Boots on real x86_64 systems and virtual machines (QEMU, Hyper-V, VirtualBox).

---

## System Requirements

**To Run MFK:**
- Linux, macOS, or Windows with WSL
- QEMU with x86_64 support (or real x86_64 hardware)
- 128MB+ RAM (for QEMU)

**To Build MFK:**
- Rust nightly toolchain
- `cargo` package manager
- `qemu-system-x86_64` and `qemu-img` tools

---

## Quick Start

### 1. Install Rust Nightly
```bash
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

### 2. Clone Repository
```bash
git clone https://github.com/AlexMatzen01/Matzen-Kernel-Framework.git
cd Matzen-Kernel-Framework
```

### 3. Build and Run
```bash
./build.sh                                          # Build kernel + runner
./run.sh target/x86_64-mfk/debug/mfk-kernel       # Boot in QEMU
```

### 4. Interact with Shell
```
mfk> help              # Show available commands
mfk> ifconfig          # Configure network
mfk> ping 10.0.2.2     # Test network connectivity
mfk> ls                # List files
mfk> halt              # Shutdown
```

---

## Repository Structure

```
Matzen-Kernel-Framework/
â”œâ”€â”€ kernel/              # Main kernel crate
â”‚   â”œâ”€â”€ src/
â”‚   â”‚   â”œâ”€â”€ main.rs      # Kernel entry point
â”‚   â”‚   â”œâ”€â”€ allocator.rs # Heap allocator
â”‚   â”‚   â”œâ”€â”€ interrupts.rs# IDT and exceptions
â”‚   â”‚   â”œâ”€â”€ drivers/     # Hardware drivers
â”‚   â”‚   â”œâ”€â”€ net/         # Network stack
â”‚   â”‚   â”œâ”€â”€ fs/          # File system
â”‚   â”‚   â””â”€â”€ shell/       # Terminal shell
â”‚   â””â”€â”€ Cargo.toml
â”œâ”€â”€ tools/               # Build tools
â”‚   â”œâ”€â”€ src/main.rs      # mfk-runner (disk image creator)
â”‚   â””â”€â”€ Cargo.toml
â”œâ”€â”€ targets/             # Custom target specs
â”‚   â””â”€â”€ x86_64-mfk.json
â”œâ”€â”€ docs/                # This documentation
â”œâ”€â”€ build.sh             # Build script
â”œâ”€â”€ run.sh               # Run script
â””â”€â”€ README.md            # Main project README
```

---

## Documentation Conventions

- **Code examples** use `mfk>` for shell prompts
- **File paths** are relative to repository root
- **API references** follow Rust standard documentation format
- **Configuration** sections use TOML and Rust syntax

---

## For Different Audiences

### ðŸ‘¨â€ðŸ’» Users
Start with [Quick Start Guide](guide/quick-start.md) and [Shell Commands Reference](reference/shell-commands.md).

### ðŸ“š Students & Learners
Read [Architecture Overview](reference/architecture.md) then [Development Setup](development/setup.md).

### ðŸ”§ Developers & Contributors
Check [Building the Kernel](development/building.md) and [Extending MFK](development/extending.md).

### ðŸ› Debuggers & Troubleshooters
See [Troubleshooting Guide](development/troubleshooting.md) and [Testing Guide](development/testing.md).

---

## Resources

- **GitHub Repository**: https://github.com/AlexMatzen01/Matzen-Kernel-Framework
- **Rust Official Docs**: https://doc.rust-lang.org/
- **OSDev Wiki**: https://wiki.osdev.org/
- **x86_64 Crate Docs**: https://docs.rs/x86_64/

---

## License

MFK is released under the **MIT License**. See [LICENSE](../LICENSE) for details.

---

## Questions & Contributions

- **Report Issues**: Open an issue on GitHub
- **Contribute**: See [Contributing Guide](development/contributing.md)
- **Discuss**: Check GitHub Discussions

---

**Last Updated**: December 2025  
**Version**: 0.1.0
