# Building Guide

In-depth guide to the MFK build process.

## Overview

MFK uses a multi-stage build:
1. **Bootloader** — Compiled for x86_64, handles boot initialization
2. **Kernel** — Compiled for custom `x86_64-mfk` target with no_std
3. **Runner** — Launcher tool that starts QEMU with proper arguments

## Build Scripts

### `./build.sh`

Main build script that orchestrates the entire process.

**What it does:**
```bash
#!/bin/bash
# 1. Compile kernel in release mode
cargo build -p mfk-kernel --release --target targets/x86_64-mfk.json -Zjson-target-spec

# 2. Create disk image if missing
qemu-img create -f raw target/disk.img 10M 2>/dev/null || true

# 3. Build runner tool
cargo build -p mfk-runner --release

# 4. Copy kernel to expected location
cp target/x86_64-mfk/release/mfk-kernel target/kernel.elf
```

**Usage:**
```bash
./build.sh                    # Full release build
cargo build -p mfk-kernel    # Debug build instead
```

### `./run.sh`

Starts the built kernel in QEMU.

**Usage:**
```bash
./run.sh                                           # Use default kernel
./run.sh target/x86_64-mfk/debug/mfk-kernel     # Specific kernel
./run.sh target/x86_64-mfk/release/mfk-kernel   # Release build
```

**What it does:**
```bash
# Executes mfk-runner with kernel argument
cargo run -p mfk-runner --release -- "$@"
```

### `./test_commands.txt`

Commands to automatically run in QEMU.

**Format:**
```
command1
command2
command3
```

Each line becomes a command sent to the kernel after boot.

## Custom Target: `x86_64-mfk.json`

The kernel requires a custom Rust target because it runs in ring 0 with no OS.

**Location**: `targets/x86_64-mfk.json`

**Key settings:**
```json
{
  "llvm-target": "x86_64-unknown-none",
  "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
  "arch": "x86_64",
  "target-pointer-width": "64",
  "target-c-int-width": "32",
  "os": "none",
  "env": "",
  "vendor": "",
  "abi": "",
  "panic": "abort",
  "disable-redzone": true,
  "features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-3dnow,-3dnowa,-avx,-avx2,+soft-float",
  "precompiled-libraries": false,
  "llvm-libunwind": "none"
}
```

**Explanation:**
- `panic: abort` — No unwinding, just terminate
- `disable-redzone` — x86-64 red zone forbidden in interrupt handlers
- `-sse/-mmx/etc` — Disable SIMD, we don't preserve these in interrupts
- `soft-float` — Use soft float library instead of FPU

## Build Profile Tuning

### Release Build (Production)

Default settings optimize for speed:
```toml
# Cargo.toml [profile.release]
opt-level = 3        # Maximum optimization
lto = true          # Link-time optimization
codegen-units = 1   # Single unit for better optimization
```

**Benefits:** Faster execution, smaller binary, better loop optimization

**Drawbacks:** Longer compile time, harder to debug

### Debug Build (Development)

Default settings optimize for build speed:
```toml
# Cargo.toml [profile.dev]
opt-level = 0       # No optimization
debug = true        # Keep debug symbols
```

**Benefits:** Instant build, full debug symbols

**Drawbacks:** Slow execution, large binary

**Build time:**
- Debug: ~20 seconds
- Release: ~45 seconds (first time) / ~5 seconds (incremental)

## Dependencies

### Workspace Structure

```
Cargo.toml (workspace root)
├── kernel/                    # Main kernel binary
│   └── Cargo.toml
└── tools/                     # mfk-runner tool
    └── Cargo.toml
```

### Key Crates

**Bootloader**: `0.11.x`
```toml
[dependencies]
bootloader = { version = "0.11", features = ["map_physical_memory"] }
```

Provides boot environment and physical memory mapping.

**x86_64**: `0.14.x`
```toml
x86_64 = "0.14"
```

CPU operations: descriptor tables, paging, port I/O, instructions.

**Spin**: `0.9.x`
```toml
spin = "0.9"
```

Spinlocks for kernel synchronization.

**Volatile**: `0.4.x`
```toml
volatile = "0.4"
```

Volatile reads/writes for hardware registers.

## Cargo Workspace

The project uses Cargo workspaces to manage multiple binaries.

**Structure:**
```toml
[workspace]
members = ["kernel", "tools"]
resolver = "2"

[workspace.package]
version = "0.1.0"
authors = ["MFK Contributors"]
```

**Benefits:**
- Share dependencies across projects
- Single unified build
- Common version numbers

**Build command:**
```bash
cargo build --workspace          # Build all
cargo build -p mfk-kernel       # Build just kernel
cargo build -p mfk-runner       # Build just runner
```

## Compilation Details

### No-std Environment

The kernel compiles with `#![no_std]` for bare metal.

**What this means:**
- No heap allocator (initially) — we implement custom one
- No File I/O system (we implement SimpleFS)
- No threading (we implement cooperative multitasking)
- No standard library functions

**But we have:**
- Core library (iterators, options, results, etc)
- Volatile accesses for hardware
- Unsafe blocks for privileged operations

### Linker Script

Implicitly handled by bootloader crate.

The bootloader provides its own linker script that:
- Sets entry point to `_start` (bootloader sets up)
- Maps kernel at `0xFFFFFFFF80000000` (actually at `0x400000` due to identity mapping)
- Provides physical memory offset

### Compilation Phases

```
Source Code (Rust)
       ↓
   rustc (LLVM)
       ↓
   Intermediate LLVM-IR
       ↓
   LLVM Optimizer
       ↓
   Assembly (x86-64)
       ↓
   Assembler (LLVM)
       ↓
   Object Files
       ↓
   Linker (lld)
       ↓
   ELF Binary (kernel.elf)
       ↓
   Boot Loader
       ↓
   Running Kernel
```

**Time breakdown (release):**
- rustc: 30s
- LLVM optimization: 10s
- Linking: 2s
- Total: 42s (initial) / 3s (incremental with nothing changed)

## Incremental Builds

Cargo tracks dependencies to rebuild only what changed.

**Clean dependencies:**
```bash
cargo clean                           # Remove all build artifacts
cargo clean -p mfk-kernel           # Remove just kernel artifacts
cargo clean -release                # Remove release artifacts only
```

**Rebuild specific module:**
```bash
touch kernel/src/drivers/e1000.rs   # Touch file to mark changed
cargo build                         # Only recompiles that module
```

## Debug Symbols

**Include debug symbols:**
```bash
cargo build               # Debug build has symbols
cargo build --release    # Release build: no symbols by default
```

**To add symbols to release:**
```toml
[profile.release]
debug = true   # Keep debug symbols even in release
```

**Symbol usage:**
- Enable GDB debugging in QEMU
- Better panic messages
- Larger binary size (+5MB typical)

## Cross-Compilation Notes

MFK targets `x86_64` and uses a custom target specification. The build works on:
- Linux (x86_64)
- macOS (x86_64 and ARM64)
- Windows (WSL2 or MSYS2)

**Key requirement:** QEMU x86_64 emulation must be available.

```bash
# Linux
sudo apt-get install qemu-system-x86

# macOS
brew install qemu

# Windows (WSL2)
apt-get install qemu-system-x86
```

## Common Build Issues

### "error: linker `cc` not found"

The Rust toolchain tries to use C compiler for linking. Usually not needed.

**Solution:**
```bash
# On Linux
sudo apt-get install build-essential

# On macOS
xcode-select --install

# On Windows (WSL2)
apt-get install build-essential
```

### "error[E0514]: found crate mismatch"

Mismatched crate versions between dependencies.

**Solution:**
```bash
cargo update                # Update all dependencies
cargo clean                 # Clean artifacts
cargo build                 # Rebuild
```

### "LLVM ERROR: Unsupported architecture"

LLVM tools not available for target.

**Solution:**
```bash
rustup component add rust-src llvm-tools-preview --toolchain nightly
cargo build
```

### Build is slow

**Causes:**
- Release build with optimizations (40+ seconds normal)
- Computer load too high
- Disk I/O bottleneck

**Solutions:**
```bash
# Use debug for faster iteration
cargo build -p mfk-kernel          # Debug: 15s
cargo build -p mfk-kernel --release  # Release: 40s

# Use sccache for incremental builds
cargo install sccache
RUSTC_WRAPPER=sccache cargo build  # Uses caching
```

## Build Customization

### Feature Flags

Define optional features in `kernel/Cargo.toml`:

```toml
[features]
default = ["serial-debug"]
serial-debug = []       # Enable serial debug output
disk-logging = []       # Log disk operations
```

Build with features:
```bash
cargo build --features serial-debug,disk-logging
```

### Optimization Levels

Tuning in `Cargo.toml`:

```toml
[profile.release]
opt-level = 2          # Medium optimization (faster build, still fast)
opt-level = 3          # Maximum (default, slow compile)
opt-level = "z"        # Minimize size
```

### LTO (Link-Time Optimization)

```toml
[profile.release]
lto = true      # Full LTO (slowest compile, fastest binary)
lto = "thin"    # Thin LTO (compromise)
lto = false     # No LTO (fastest compile)
```

## Continuous Integration

The project can be built in CI/CD:

```bash
#!/bin/bash
set -e
./build.sh
cargo test -p mfk-kernel --lib
./run.sh < test_commands.txt
```

**GitHub Actions example:**
```yaml
name: Build & Test
on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          profile: minimal
          toolchain: nightly
      - run: rustup component add rust-src llvm-tools-preview
      - run: sudo apt-get install qemu-system-x86
      - run: ./build.sh
```

## Performance Profiling

### Measuring Build Time

```bash
# Unix
time ./build.sh

# Windows (PowerShell)
Measure-Command { .\build.sh }

# Cargo built-in
cargo build -p mfk-kernel --timings
```

### Flamegraph (Runtime Performance)

Not directly applicable since we run in QEMU, but concepts apply:

```bash
# Measure kernel execution time
time ./run.sh target/x86_64-mfk/release/mfk-kernel < test_commands.txt
```

## Next Steps

- **[Testing Guide](testing.md)** — How to test your changes
- **[Extending Guide](extending.md)** — Add new features
- **[Architecture Reference](../reference/architecture.md)** — System design
