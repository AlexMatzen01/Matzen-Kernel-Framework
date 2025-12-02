# Matzen Kernel Framework (MFK)

The Matzen Kernel Framework is a forward-looking Rust micro-kernel playground designed for experimentation on bare-metal targets. The project favors clarity and modular growth over premature optimization, making it easy to extend and customize for new architectures, drivers, or services.

## Highlights

- **Rust-first** `no_std`, `no_main` kernel that boots via the [`bootloader`](https://github.com/rust-osdev/bootloader) crate.
- **Enterprise-grade layout**: clearly separated `arch`, `core`, `memory`, and `drivers` modules with extensive inline documentation.
- **Custom entry point** with predictable memory map, linker script, and early exception/interrupt scaffolding.
- **Deterministic builds**: nightly Rust toolchain pinning, workspace-level `Cargo.toml`, and reproducible target specification via `targets/x86_64-mfk.json`.
- **Early logging** through a lightweight VGA text writer and composable logger API for future framebuffer work.
- **Turn-key virtualization**: QEMU launch scripts (bash + PowerShell) and VirtualBox configuration templates for rapid iteration without host reboots.

## Quick Start

```bash
# 1. Install rustup nightly per rust-toolchain.toml (automatic on first build)
# 2. Install cargo bootimage helper once
cargo install bootimage

# 3. Build a bootable image (runs clippy/tests disabled by design)
cargo bootimage -p mfk-kernel

# 4. Launch in QEMU with one command
make run          # Linux/macOS/WSL
# or
./scripts/run-qemu.ps1  # Windows PowerShell
```

> **Heads-up:** The first `cargo bootimage` invocation builds the Rust standard library for the custom `x86_64-mfk` target; expect a longer compilation time.

## Repository layout

```
├── Cargo.toml                # Workspace definition
├── kernel/                   # Primary kernel crate (lib + entrypoint)
│   ├── linker.ld             # Memory layout script
│   └── src/
│       ├── arch/             # Architecture-specific boot + interrupt code
│       ├── core/             # Scheduler/runtime glue
│       ├── drivers/          # Early drivers (VGA, timers, etc.)
│       ├── logger.rs         # Logger facade + macros
│       ├── memory/           # Memory map + allocator placeholders
│       └── panic.rs          # Panic + shutdown handling
├── targets/x86_64-mfk.json   # Custom compilation target spec
├── scripts/                  # Helper scripts (QEMU launch, artifact prep)
└── virtualization/           # QEMU & VirtualBox configs/documentation
```

For a deeper architectural overview and extension points, read [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
