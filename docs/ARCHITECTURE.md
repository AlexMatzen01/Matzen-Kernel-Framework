# MFK Architecture Overview

The Matzen Kernel Framework (MFK) is intentionally minimal yet structured so new subsystems can be slotted in without modifying unrelated code. This document summarizes the top-level components and how they interact.

## Module tree

```
kernel
├── arch
│   └── x86_64
│       ├── gdt.rs          # CPU privilege levels and task state
│       ├── interrupts.rs   # IDT, ISR stubs, and hardware interrupt handlers
│       ├── pic.rs          # Programmable Interrupt Controller driver
│       └── mod.rs          # Architecture facade + init entry
├── core
│   ├── boot.rs             # Cold-boot sequencing + health checks
│   └── runtime.rs          # Idle loop + future scheduler hooks
├── drivers
│   ├── keyboard.rs         # PS/2 keyboard driver with US QWERTY layout
│   └── vga.rs              # VGA text mode writer
├── terminal.rs             # Interactive shell with command parsing
├── logger.rs               # Simple logging facade backed by drivers
├── memory
│   ├── layout.rs           # Memory region placeholders (bootloader-managed)
│   └── mod.rs              # Init hooks + future paging/alloc placeholder
└── panic.rs                # Panic handler + fail-fast shutdown path
```

## Boot flow

1. **Reset -> Bootloader**: The `bootloader` crate loads `bootimage-mfk-kernel.bin`, switches to 64-bit long mode, and jumps to `_start`.
2. **Custom entry point**: `_start` invokes `kernel_entry` (see `kernel/src/lib.rs`) with a `BootInfo` pointer.
3. **Early services**:
   - VGA writer initialized for deterministic logging.
   - GDT + IDT configured, including basic exception vectors and hardware interrupt handlers.
   - PIC initialized with keyboard interrupt (IRQ1) enabled.
4. **Memory layout validation**: `memory::init` reports boot info and memory regions.
5. **Terminal phase**: Control passes to `terminal::run`, which displays a welcome banner and enters an interactive command loop.

## Terminal OS

The kernel boots into an interactive terminal shell that supports:

- **Keyboard input**: PS/2 keyboard driver with US QWERTY layout, supporting shift, caps lock, and special keys (backspace, enter).
- **Built-in commands**:
  - `help` - List available commands
  - `clear` - Clear the screen
  - `echo <text>` - Print text to screen
  - `about` - Show kernel information
  - `version` - Show version info
  - `uptime` - System uptime (placeholder)
  - `mem` - Memory information
  - `reboot` - Reboot via keyboard controller reset
  - `halt` - Halt the CPU

## Memory map

- **1 MiB identity mapping**: kernel is linked to load at `0x0010_0000` (1 MiB) to keep BIOS area untouched.
- **`.text/.rodata/.data/.bss`** order enforced by `kernel/linker.ld` with 4 KiB alignment.
- **Bootloader framebuffer**: Exposed via `BootInfo` for future graphics drivers.

## Extension points

- **Architectures**: Add a new module under `kernel/src/arch/<arch>` and register it in `arch/mod.rs`. Each arch module exposes `fn init(boot_info: &BootInfo)` for symmetry.
- **Drivers**: Place hardware-specific implementations under `kernel/src/drivers`. The `logger` module already abstracts the console, so new backends (UART, framebuffer) can plug in without touching call sites.
- **Terminal commands**: Add new commands in `terminal.rs` by extending the `execute_command` match statement.
- **Memory management**: `memory::layout` centralizes all symbols exported by the linker. Higher-level allocators, paging structures, and mapping policies can be layered on top without changing the entry flow.
- **Virtualization**: Launch scripts live in `scripts/` so CI/CD pipelines or custom dashboards can wrap them easily. QEMU and VirtualBox templates reside under `virtualization/`.

## Design principles

- **Fail fast**: panic paths immediately print context and triple-fault (via `hlt` loop) to avoid undefined states.
- **Layered boundaries**: `arch` code hides ISA-specific details from `core`. The memory module owns all linker interactions. The driver layer owns IO specifics.
- **Comment the why**: Complex sections (interrupt descriptors, linker script, target spec) include rationale inline. This makes it easier to evolve the kernel without institutional knowledge.
