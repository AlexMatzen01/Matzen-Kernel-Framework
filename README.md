# Matzen Kernel Framework (MFK)

A simple terminal OS written in Rust for the x86_64 architecture.

## Features

- VGA text mode output (80x25 characters)
- PS/2 keyboard input support
- Built-in command shell with various commands
- Bootable via BIOS or UEFI

## Requirements

- Rust nightly toolchain
- QEMU (for testing)

## Building

First, ensure you have the nightly Rust toolchain installed:

```bash
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

Build the kernel:

```bash
cargo build -p mfk-kernel --target x86_64-unknown-none -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

Build the runner tool:

```bash
cargo build -p mfk-runner --release
```

## Running in QEMU

Create a bootable disk image and run in QEMU:

```bash
cargo run -p mfk-runner --release -- target/x86_64-unknown-none/debug/mfk-kernel
```

Or create only the disk image (without running QEMU):

```bash
cargo run -p mfk-runner --release -- target/x86_64-unknown-none/debug/mfk-kernel --no-run
```

## Available Shell Commands

Once the kernel boots, you'll see a terminal prompt. Available commands:

| Command | Description |
|---------|-------------|
| `help` | Display available commands |
| `clear` or `cls` | Clear the screen |
| `echo <text>` | Print text to the screen |
| `about` | Display information about MFK |
| `uptime` | Show system uptime (simulated) |
| `memory` or `mem` | Display memory information |
| `reboot` | Reboot the system |
| `halt` or `shutdown` | Halt the system |
| `date` | Display current date (not implemented) |
| `whoami` | Display current user |

## Project Structure

```
.
├── Cargo.toml           # Workspace configuration
├── kernel/              # Kernel crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs      # Kernel entry point
│       ├── drivers/     # Hardware drivers
│       │   ├── mod.rs
│       │   ├── vga.rs   # VGA text buffer driver
│       │   └── keyboard.rs  # PS/2 keyboard driver
│       └── shell/       # Shell implementation
│           └── mod.rs
├── tools/               # Build/runner tools
│   ├── Cargo.toml
│   └── src/
│       └── main.rs      # Disk image creator & QEMU runner
└── targets/             # Custom target specifications
    └── x86_64-mfk.json
```

## Architecture

The kernel uses the `bootloader` crate for booting and provides:

1. **VGA Driver**: Direct memory-mapped access to the VGA text buffer at `0xb8000`
2. **Keyboard Driver**: PS/2 keyboard input via port I/O
3. **Shell**: Simple command-line interface with built-in commands

## License

MIT License
