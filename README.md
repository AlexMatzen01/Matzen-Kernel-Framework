# Matzen Kernel Framework (MFK)

A simple terminal OS written in Rust for the x86_64 architecture.
````markdown
# Matzen Kernel Framework (MFK)

A small educational x86_64 hobby OS written in Rust. MFK demonstrates a bare-metal kernel with a simple shell, VGA text-mode output, PS/2 keyboard support, and a small set of drivers useful for experimentation and teaching.

**Status**: Experimental — actively developed, suitable for learning and demos.

**Repository**: `alexmatzen01/Matzen-Kernel-Framework`

**Top-level crates**:
- `mfk-kernel` (kernel crate, located in `kernel/`)
- `mfk-runner` (tooling for creating bootable images and running QEMU, located in `tools/`)

**Test commands**: See `test_commands.txt` for example shell interactions.

**Requirements**
- **Rust**: nightly toolchain (see `rust-toolchain.toml`).
- **Components**: `rust-src`, `llvm-tools-preview` (used by `cargo` when building `core`/`alloc`).
- **QEMU**: for running and testing the image locally.

**Quickstart**

1. Install the toolchain and components (recommended to follow `rust-toolchain.toml`):

```bash
rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly
```

2. Build the kernel (uses a custom target spec in `targets/x86_64-mfk.json`):

```bash
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

3. Build the runner tool (creates disk images and can run QEMU):

```bash
cargo build -p mfk-runner --release
```

4. Create a disk image and run in QEMU (runner will locate the kernel artifact):

```bash
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel
```

5. To create the disk image without launching QEMU:

```bash
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run
```

Notes:
- The `-Zbuild-std` flags are required because the kernel builds `core`/`alloc` for the custom target.
- Paths shown (e.g. `target/x86_64-mfk/debug/mfk-kernel`) reflect the workspace layout when building in debug mode.

**Available Shell Commands**
- **help**: Display available commands.
- **clear/cls**: Clear the screen.
- **echo <text>**: Print text to the screen.
- **about**: Display information about MFK.
- **uptime**: Show system uptime (simulated).
- **memory/mem**: Display memory information.
- **reboot**: Reboot the system (simulated by runner/QEMU).
- **halt/shutdown**: Halt the system.
- **whoami**: Display current user.

Some commands may be placeholders or partially implemented; consult `kernel/src/shell` for the current implementation and to add new commands.

**Project Layout (high level)**

```
`Cargo.toml`            # Workspace config
`kernel/`               # Kernel crate (`mfk-kernel`)
    `Cargo.toml`
    `src/`                # Kernel sources (entry: `src/main.rs`)
        `drivers/`          # Drivers (VGA, keyboard, serial, ATA, RTC...)
        `fs/`               # Filesystem code
        `shell/`            # Shell implementation
`tools/`                # Runner / image creation (`mfk-runner`)
    `Cargo.toml`
    `src/main.rs`         # Disk image creator and QEMU runner
`targets/x86_64-mfk.json`  # Custom target spec used for building kernel
`test_commands.txt`     # Example shell commands to exercise basic features
```

**Development notes**
- The repository uses a workspace with `kernel` and `tools` members — build either crate individually using `-p <name>` or build the whole workspace with `cargo build`.
- Kernel development targets bare metal; expect to use QEMU for emulation rather than running on real hardware.
- To add drivers or shell commands, modify the code under `kernel/src/drivers` and `kernel/src/shell`.
- Use `rustfmt` and `cargo clippy` (on supported parts) to keep code consistent.

**Testing**
- Use `tools` runner to run images in QEMU for manual testing.
- Example interaction scripts are in `test_commands.txt`.

**Troubleshooting**
- If builds fail with missing components, ensure `rust-src` and `llvm-tools-preview` are installed for the nightly toolchain.
- If `cargo` flags change across nightly versions, consult `rust-toolchain.toml` and update components accordingly.

**Contributing**
- Fork, create a branch, and open a PR. Keep changes focused and add tests where applicable.

**License**
- MIT License (see `LICENSE`)

````
