# Matzen Kernel Framework Documentation

Welcome to the comprehensive documentation for the **Matzen Kernel Framework (MFK)** — a small educational terminal OS written in Rust for x86_64.

## Quick Navigation

### 🚀 Getting Started
- **[Quick Start Guide](guide/quick-start.md)** — Set up and run the kernel in 5 minutes
- **[Installation Guide](guide/installation.md)** — Detailed setup instructions
- **[Running the Kernel](guide/running.md)** — Boot and interact with MFK

### 📚 Core Documentation
- **[Architecture Overview](reference/architecture.md)** — System design and components
- **[Kernel API Reference](reference/api.md)** — Core kernel functions and structures
- **[Memory Management](reference/memory.md)** — Heap allocation and memory layout
- **[Interrupt Handling](reference/interrupts.md)** — IDT, PIC, and exception handling

### 🌐 Features & Subsystems
- **[Network Stack](reference/networking.md)** — Ethernet, IP, ARP, ICMP, and networking
- **[File System](reference/filesystem.md)** — SimpleFS implementation and usage
- **[Shell Commands](reference/shell-commands.md)** — Built-in terminal commands
- **[Hardware Drivers](reference/drivers.md)** — VGA, Keyboard, ATA, E1000, Serial, RTC, PIC

### 🔧 Development
- **[Development Setup](development/setup.md)** — Build environment configuration
- **[Building the Kernel](development/building.md)** — Compilation flags and process
- **[Testing Guide](development/testing.md)** — Running tests and debugging
- **[Extending MFK](development/extending.md)** — Adding new features and modules
- **[Troubleshooting](development/troubleshooting.md)** — Common issues and fixes

### 💡 Advanced Topics
- **[Network Implementation Details](reference/networking-deep-dive.md)** — Protocol stack internals
- **[Shell Implementation](reference/shell-internals.md)** — Command parsing and execution
- **[Contributing Guide](development/contributing.md)** — How to contribute to the project

---

## What is MFK?

The **Matzen Kernel Framework** is an educational operating system kernel written entirely in Rust. It provides:

- **VGA Text Mode Output** — 80×25 character terminal
- **PS/2 Keyboard Input** — Real-time keyboard support
- **TCP/IP Network Stack** — Partial networking with Ethernet, IPv4, ARP, ICMP
- **SimpleFS File System** — Basic disk I/O and file management
- **Built-in Shell** — Interactive command-line interface
- **BIOS/UEFI Boot** — Bootable via multiple firmware types
- **Hardware Drivers** — VGA, ATA, E1000 NIC, Serial, RTC
- **Interrupt Handling** — IDT, PIC, keyboard, serial interrupts

---

## Key Features

### 🎯 Pure Rust
MFK is written entirely in Rust with no C dependencies, leveraging Rust's memory safety to prevent common kernel bugs.

### 📖 Educational Focus
Clean, well-documented code designed to teach OS concepts:
- Memory management and heap allocation
- Interrupt handling and exception processing
- Network protocol implementation
- File system design
- Shell command parsing

### 🔌 Hardware Support
- Intel E1000 NIC (network)
- ATA disk drives (storage)
- PS/2 keyboard (input)
- VGA text mode (output)
- Serial port (debugging)
- Real-time clock (timing)

### 🚀 Real Hardware Capable
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
├── kernel/              # Main kernel crate
│   ├── src/
│   │   ├── main.rs      # Kernel entry point
│   │   ├── allocator.rs # Heap allocator
│   │   ├── interrupts.rs# IDT and exceptions
│   │   ├── drivers/     # Hardware drivers
│   │   ├── net/         # Network stack
│   │   ├── fs/          # File system
│   │   └── shell/       # Terminal shell
│   └── Cargo.toml
├── tools/               # Build tools
│   ├── src/main.rs      # mfk-runner (disk image creator)
│   └── Cargo.toml
├── targets/             # Custom target specs
│   └── x86_64-mfk.json
├── docs/                # This documentation
├── build.sh             # Build script
├── run.sh               # Run script
└── README.md            # Main project README
```

---

## Documentation Conventions

- **Code examples** use `mfk>` for shell prompts
- **File paths** are relative to repository root
- **API references** follow Rust standard documentation format
- **Configuration** sections use TOML and Rust syntax

---

## For Different Audiences

### 👨‍💻 Users
Start with [Quick Start Guide](guide/quick-start.md) and [Shell Commands Reference](reference/shell-commands.md).

### 📚 Students & Learners
Read [Architecture Overview](reference/architecture.md) then [Development Setup](development/setup.md).

### 🔧 Developers & Contributors
Check [Building the Kernel](development/building.md) and [Extending MFK](development/extending.md).

### 🐛 Debuggers & Troubleshooters
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
