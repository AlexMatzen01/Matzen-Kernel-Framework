# Matzen Kernel Framework (MFK)

A small educational terminal OS written in Rust for x86_64. MFK contains a tiny kernel (`mfk-kernel`) with VGA driver, PS/2 keyboard support, ATA disk driver, filesystem, networking (E1000), and a built-in shell. The `mfk-runner` tool builds bootable disk images and runs them in QEMU.

**Status:** active development — use for experimentation and learning.

## Features
- VGA text mode output
- PS/2 keyboard input
- ATA PIO disk driver
- SimplFS filesystem (custom implementation)
- Intel E1000 network driver
- Network stack: Ethernet, ARP, IPv4, ICMP, UDP, TCP
- Interactive shell with file and network commands

**Repository layout (important files):**

- `Cargo.toml`           : Workspace configuration (members: `kernel`, `tools`)
- `kernel/`              : Kernel crate (`mfk-kernel`)
- `tools/`               : Runner tool (`mfk-runner`) — creates disk images and runs QEMU
- `targets/x86_64-mfk.json` : Custom target specification used for building the kernel
- `build.sh`             : Convenience script to build kernel + runner
- `run.sh`               : Convenience script to run `mfk-runner` with a kernel path
- `install.sh`           : Convenience script to install Rust/nightly components
- `test_commands.txt`    : Example shell commands to exercise the kernel's filesystem/shell
- `NETWORKING.md`        : Networking documentation and usage guide

Prerequisites
- Linux or macOS (QEMU required for running the image)
- Rust (we use nightly for building the kernel)
- `qemu-system-x86_64` and `qemu-img` available in `PATH`

Quick setup
1. Install the nightly toolchain and required components (or run the helper):

```bash
./install.sh
```

2. Build everything (kernel + runner):

```bash
./build.sh
```

Manual build steps

- Build the kernel (required flags for no-std build):

```bash
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

- Build the runner tool (release recommended):

```bash
cargo build -p mfk-runner --release
```

Using the runner (`mfk-runner`)

The runner creates both UEFI and BIOS bootable disk images from a kernel binary and — by default — will launch QEMU to run the BIOS image. The runner usage is:

```bash
cargo run -p mfk-runner --release -- <path-to-kernel-binary> [--no-run]
```

Examples (after building the kernel):

```bash
# Run and boot the kernel in QEMU (uses target/x86_64-mfk/debug/mfk-kernel by default path shown in examples)
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel

# Only create disk images, don't start QEMU:
cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --no-run

# Helper script (runs the above):
./run.sh target/x86_64-mfk/debug/mfk-kernel
```

What `mfk-runner` does
- Creates UEFI and BIOS disk images named like `<kernel-path>-uefi.img` and `<kernel-path>-bios.img`.
- Creates/uses a small `target/disk.img` (10MB) for data; if the file doesn't exist the runner will attempt to create it using `qemu-img`.
- When not passed `--no-run`, `mfk-runner` launches QEMU with the BIOS disk image attached as the primary drive and the `disk.img` attached as a secondary disk. QEMU is invoked with serial redirected to stdio.

Running and testing the kernel
- Boot the image in QEMU (see example above). The kerne

Filesystem commands
- `mkfs`       : Format disk with SimplFS
- `mount`      : Mount the filesystem
- `ls`/`dir`   : List files
- `touch <f>`  : Create a file
- `cat <f>`    : Display file contents
- `write <f> <text>`: Write to a file
- `rm <f>`     : Delete a file

Network commands (see NETWORKING.md for details)
- `ifconfig [ip]`: Configure/display network interface
- `ping <ip>`  : Send ICMP echo request
- `netstat`    : Display network statusl prints to the serial/VGA and exposes a simple shell.
- Use `test_commands.txt` for quick filesystem/shell smoke tests (examples: `mkfs`, `mount`, `write`, `cat`, `ls`).

Shell commands (common)
- `help`       : Show available shell commands
- `clear`/`cls`: Clear the screen
- `echo <x>`   : Print text
- `about`      : Show project info
- `uptime`     : Simulated uptime
- `mem`/`memory`: Show memory info
- `reboot`, `halt`, `shutdown`: System control commands

Development notes
- The `kernel` crate (`mfk-kernel`) depends on `bootloader_api`, and is configured as the binary named `mfk-kernel` (see `kernel/Cargo.toml`).
- The `tools` crate (`mfk-runner`) depends on `bootloader` and implements helpers to create UEFI/BIOS disk images and launch QEMU.

Contributing
- Fork the repository, create a branch, and open a pull request. Keep changes small and focused.

License
- MIT License — see `LICENSE` for details.

Contact / author
- Alexander Matzen — repository owner

If anything in this README is out of date, please open an issue or send a PR with suggested corrections.
