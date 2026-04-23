# Development Setup

Setting up your environment for MFK kernel development.

## Prerequisites

See [Installation Guide](../guide/installation.md) for full system setup.

Quick checklist:
- ✅ Rust nightly toolchain
- ✅ QEMU x86_64 system emulator
- ✅ Build tools (gcc, make)
- ✅ Git
- ✅ 2GB+ disk space

## IDE Setup

### Visual Studio Code (Recommended)

#### 1. Install Extensions
```bash
code --install-extension rust-lang.rust-analyzer
code --install-extension vadimcn.vscode-lldb
code --install-extension serayuzgur.crates
code --install-extension eamodio.gitlens
```

#### 2. Configure Settings
Create `.vscode/settings.json`:

```json
{
  "rust-analyzer.checkOnSave.command": "clippy",
  "rust-analyzer.checkOnSave.extraArgs": [
    "--all-targets",
    "--",
    "-D",
    "warnings"
  ],
  "editor.formatOnSave": true,
  "[rust]": {
    "editor.defaultFormatter": "rust-lang.rust-analyzer"
  },
  "files.exclude": {
    "**/target": true
  }
}
```

#### 3. Create Launch Config
Create `.vscode/launch.json`:

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "Run kernel (debug)",
      "type": "lldb",
      "request": "launch",
      "program": "${workspaceFolder}/target/x86_64-mfk/debug/mfk-kernel",
      "args": [],
      "cwd": "${workspaceFolder}",
      "stopOnEntry": false,
      "console": "integratedTerminal"
    }
  ]
}
```

### JetBrains CLion

1. Open project
2. Set toolchain to `rustup: nightly`
3. Enable Rust plugin
4. Configure run configurations for build.sh and run.sh

### Vim/Neovim

Required plugins:
- `vim-rust-lang/rust.vim` — Rust syntax
- `rust-lang/rust.analyzer` — LSP integration
- `dense-analysis/ale` — Linting (optional)

## Project Structure for Development

```
Matzen-Kernel-Framework/
├── kernel/
│   ├── src/
│   │   ├── main.rs           # Edit here for core changes
│   │   ├── drivers/          # Add drivers here
│   │   ├── net/              # Edit for network changes
│   │   ├── fs/               # Filesystem modifications
│   │   └── shell/            # Shell command changes
│   └── Cargo.toml            # Kernel dependencies
├── tools/
│   ├── src/main.rs           # mfk-runner changes
│   └── Cargo.toml
├── docs/                      # This documentation
└── tests/                     # Integration tests (if added)
```

## Common Development Tasks

### Adding a New Driver

```rust
// kernel/src/drivers/newdevice.rs
pub fn init() -> Result<(), &'static str> {
    crate::serial_println!("Initializing NewDevice...");
    // Implementation
    Ok(())
}

pub fn some_operation() {
    // Public interface
}
```

Then register in `kernel/src/drivers/mod.rs`:

```rust
pub mod newdevice;

pub fn init_all_drivers() {
    // ...
    newdevice::init();
}
```

And call from `kernel/src/main.rs`.

### Adding a Shell Command

In `kernel/src/shell/mod.rs`, find `execute_command()`:

```rust
match parts.0 {
    // ... existing commands ...
    "mycommand" => cmd_mycommand(parts.1),
    _ => { /* unknown command */ }
}

fn cmd_mycommand(args: &str) {
    println!("My command executed with args: {}", args);
}
```

### Modifying Network Stack

Network code is in `kernel/src/net/`:
- `mod.rs` — Network initialization and packet processing
- `ethernet.rs` — Ethernet frame handling
- `ip.rs` — IPv4 implementation
- `icmp.rs` — ICMP (ping) implementation
- `arp.rs` — ARP protocol
- `udp.rs` — UDP protocol (basic)

Example: Adding ICMP Echo Reply logging:

```rust
// In kernel/src/net/icmp.rs
pub fn process_packet(packet: &[u8], src_ip: [u8; 4]) {
    // ... existing code ...
    
    if icmp_header.icmp_type == ICMP_ECHO_REPLY {
        crate::serial_println!(
            "🎯 Ping reply from {}.{}.{}.{}",
            src_ip[0], src_ip[1], src_ip[2], src_ip[3]
        );
    }
}
```

### Debugging with Logs

Use `crate::serial_println!()` for debug output:

```rust
crate::serial_println!("Debug message: {}", variable);
crate::serial_println!("Hex value: {:#x}", value);
crate::serial_println!("Array: {:?}", array);
```

Output appears on serial port, visible in QEMU stdout.

## Testing Workflow

### 1. Make Changes
Edit kernel source files.

### 2. Build
```bash
./build.sh
```

Fix any compiler errors.

### 3. Boot & Test
```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

Test changes interactively in shell.

### 4. Automated Testing
```bash
timeout 20 bash << 'EOF'
(
  sleep 4
  echo "help"
  sleep 1
  echo "halt"
) | cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
EOF
```

### 5. Release Build
```bash
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zjson-target-spec -Zbuild-std=core,alloc --release
```

## Code Style

### Formatting
```bash
cargo fmt -p mfk-kernel
cargo fmt -p mfk-runner
```

### Linting
```bash
cargo clippy -p mfk-kernel -- -D warnings
cargo clippy -p mfk-runner -- -D warnings
```

### Documentation
```rust
/// Brief description
/// 
/// Longer explanation with examples.
///
/// # Example
/// ```
/// let result = my_function();
/// ```
pub fn my_function() { }
```

Generate docs:
```bash
cargo doc -p mfk-kernel --open
```

## Performance Profiling

### Using `perf` (Linux)
```bash
perf record -p <qemu-pid> -e cycles,instructions
perf report
```

### Using Flamegraph (Linux)
```bash
cargo install flamegraph
cargo flamegraph --bin mfk-kernel
```

### Timing Operations
```rust
let start = crate::shell::get_tick_count();
// ... operation ...
let elapsed = crate::shell::get_tick_count() - start;
crate::serial_println!("Operation took {}ms", elapsed);
```

## Debugging Tips

### Serial Output
All `serial_println!()` calls appear in QEMU output:

```bash
./run.sh target/x86_64-mfk/debug/mfk-kernel 2>&1 | grep "Debug\|Serial"
```

### Panic Messages
Panics print to both serial and VGA:

```
KERNEL PANIC!
panicked at kernel/src/main.rs:42:5
attempt to divide by zero
```

### Memory Issues
Use `MIRI` for undefined behavior detection:

```bash
cargo +nightly miri test -p mfk-kernel
```

### Network Debugging
Add logging to network stack:

```rust
crate::serial_println!("RX packet: {} bytes", packet.len());
crate::serial_println!("TX to {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
```

## Cargo Commands

| Command | Purpose |
|---------|---------|
| `cargo check` | Quick syntax check |
| `cargo build` | Build debug binary |
| `cargo build --release` | Optimized build |
| `cargo clippy` | Lint for style issues |
| `cargo fmt` | Auto-format code |
| `cargo test` | Run tests (if any) |
| `cargo doc` | Generate documentation |
| `cargo clean` | Remove build artifacts |

## Troubleshooting Development

**Compiler errors after changes?**
```bash
cargo clean && ./build.sh
```

**Code formatting issues?**
```bash
cargo fmt -p mfk-kernel
```

**Linter warnings?**
```bash
cargo clippy -p mfk-kernel -- -D warnings
```

**Can't find symbol?**
Use `cargo doc` to generate and search documentation.

## Next Steps

- **[Building Guide](building.md)** — Detailed build process
- **[Testing Guide](testing.md)** — Testing strategies
- **[Extending MFK](extending.md)** — Add new features
