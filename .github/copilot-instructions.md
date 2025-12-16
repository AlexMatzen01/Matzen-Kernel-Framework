<!-- Copilot instructions for AI coding agents working on Matzen Kernel Framework -->
# Matzen Kernel Framework — Copilot Instructions

Purpose: get an AI coding agent productive quickly in this repo. Focus on concrete, discoverable patterns.

- **Big picture:** This repository contains a tiny educational OS written in Rust:
  - `kernel/` — `mfk-kernel` (no_std, kernel binary). Entry: `kernel/src/main.rs` via `bootloader_api::entry_point!`.
  - `tools/` — `mfk-runner` (creates bootable images and runs QEMU).
  - `targets/x86_64-mfk.json` — custom target spec used for building the kernel.

- **Build & run (exact commands to use):**
  - Install helper (sets up nightly + components): `./install.sh`
  - Full build: `./build.sh`
  - Manual kernel build (required flags):
    `cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem`
  - Build runner: `cargo build -p mfk-runner --release`
  - Create and run image (runner):
    `cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel`
  - Helper runner script: `./run.sh target/x86_64-mfk/debug/mfk-kernel`

- **Key runtime behaviors and conventions to preserve:**
  - Kernel is `#![no_std]` and uses `alloc` after `allocator::init()`; avoid allocating before the allocator is initialized.
  - Early debugging prints go to serial using `serial_println!()`; VGA output uses `println!()` after VGA is set up.
  - Panic handler prints to serial first, then VGA. Prefer serial for early/low-level debugging.
  - `BOOTLOADER_CONFIG` in `kernel/src/main.rs` maps physical memory (`Mapping::Dynamic`) — many drivers rely on physical mappings.
  - Interrupts and PIC are set up early; driver init order matters (serial → VGA → allocator → IDT → PIC → drivers → enable interrupts → shell).

- **Important files to inspect when making changes:**
  - [kernel/src/main.rs](kernel/src/main.rs) — boot sequence and feature flags (`abi_x86_interrupt`).
  - [kernel/Cargo.toml](kernel/Cargo.toml) — crate name `mfk-kernel` and core deps.
  - [kernel/src/allocator.rs](kernel/src/allocator.rs) — heap setup and allocation constraints.
  - [kernel/src/drivers/mod.rs](kernel/src/drivers/mod.rs) and individual drivers (e.g. drivers/e1000.rs, ata.rs, serial.rs).
  - [kernel/src/net/](kernel/src/net/) — network stack layering: `ethernet.rs`, `arp.rs`, `ip.rs`, `icmp.rs`, `udp.rs`.
  - [tools/](tools/) — `mfk-runner` behavior (image creation and QEMU flags), and the top-level `run.sh`/`build.sh` scripts.
  - [targets/x86_64-mfk.json](targets/x86_64-mfk.json) — custom target used during kernel builds.

- **Patterns and idioms observed (use these in new code):**
  - Prefer explicit init functions for subsystems (e.g., `drivers::serial::init()`), and call them from `main.rs` in the existing order.
  - Use serial prints for low-level logs and avoid reliance on VGA until `drivers::vga::init_with_offset()` has been called.
  - Keep kernel changes minimal and portable within the no_std constraints; add new Cargo deps only when necessary and ensure no_std compatibility.
  - Driver errors are propagated as `Result` where possible; initialization failures should be logged via `serial_println!()` and should not panic the whole system unless unavoidable.

- **Debugging tips:**
  - QEMU launched by `mfk-runner` redirects serial to stdio — read console output in the terminal.
  - Use `serial_println!()` in early init to get output even when VGA is unavailable.
  - If changing memory mapping or bootloader config, check `BOOTLOADER_CONFIG` in `kernel/src/main.rs`.
  - **Known issue:** QEMU's user-mode networking has limited ICMP support. Ping may not receive replies due to SLIRP limitations, not kernel bugs. Use TAP networking for proper ICMP testing (see `ICMP_STATUS.md`).

- **Tests & manual checks:**
  - There are no automated unit tests for the kernel; use `test_commands.txt` for manual shell smoke tests (filesystem, network commands).
  - Run the built image in QEMU via the runner to exercise changes end-to-end.

- **When editing build config / targets:**
  - Update `targets/x86_64-mfk.json` and the `build.sh` helper together. Note the `-Zbuild-std` flags required for linking `core`/`alloc` into kernel builds.

- **PR guidance for contributors/agents:**
  - Keep changes small and self-contained; prefer adding a short `docs/` note for non-obvious behavior.
  - If adding dependencies, confirm they work in `no_std` contexts and add rationale in the PR description.

If anything is unclear or you want me to expand examples for a specific subsystem (network, driver, or build flow), tell me which area to expand.
