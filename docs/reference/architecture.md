# Architecture Overview

High-level design and structure of the Matzen Kernel Framework.

## System Architecture

```
┌─────────────────────────────────────────────┐
│            Shell (Interactive CLI)          │
├─────────────────────────────────────────────┤
│                Kernel Core                  │
├──────────────┬──────────────┬───────────────┤
│   Network    │   File       │   Drivers     │
│   Stack      │   System     │   (VGA, KB,   │
│              │              │    ATA, ...)  │
├──────────────┴──────────────┴───────────────┤
│           Interrupt Handler (IDT)           │
├──────────────┬──────────────┬───────────────┤
│    Memory    │  Allocator   │   Paging      │
│  Management  │              │   (Bootloader)│
├─────────────────────────────────────────────┤
│         Hardware & Bootloader                │
└─────────────────────────────────────────────┘
```

## Boot Process

1. **Bootloader** (Bootloader v0.11) loads kernel and maps physical memory
2. **Kernel Entry** (kernel_main) initializes core systems
3. **Serial Init** enables early debug output
4. **VGA Init** enables terminal output
5. **Memory Setup** initializes heap allocator
6. **Interrupt Setup** creates IDT and enables exception handling
7. **PIC Init** configures programmable interrupt controller
8. **Driver Init** initializes hardware drivers (keyboard, disk, network)
9. **Network Init** enables network stack
10. **Shell Start** begins interactive command loop

## Core Modules

### `main.rs` — Kernel Entry Point
```
├── kernel_main()        # Boot sequence
└── panic_handler()      # Panic handling
```

**Responsibilities:**
- Initialize bootloader config
- Set up memory mapping
- Initialize all subsystems in order
- Start shell loop

### `allocator.rs` — Memory Allocation
```
├── init()               # Initialize heap
├── ALLOCATOR            # Global heap allocator (LinkedListAllocator)
└── HEAP_SIZE            # Heap size constant
```

**Responsibilities:**
- Initialize linked-list heap allocator
- Manage dynamic memory for Vec, String, etc.

### `interrupts.rs` — Exception & Interrupt Handling
```
├── init_idt()           # Create interrupt descriptor table
├── set_idt_entry()      # Configure IDT entry
├── exception handlers   # Divide by zero, page fault, etc.
└── IrqHandlers          # IRQ interrupt handlers
```

**Responsibilities:**
- Define and register all 256 exception/interrupt handlers
- Handle CPU exceptions (divide by zero, page fault, etc.)
- Route hardware interrupts to drivers

### `pic.rs` — Programmable Interrupt Controller
```
├── ChainedPics          # Master + Slave PIC
├── initialize()         # Set up and remap PIC
├── notify_eoi()         # End of interrupt
└── set_mask()           # Enable/disable IRQs
```

**Responsibilities:**
- Initialize and program 8259 PIC chips
- Remap interrupts to 0x20+ (avoid CPU exceptions)
- Manage interrupt masks

## Driver Layer

Located in `drivers/`:

### `vga.rs` — Video Output
Terminal output to 80×25 VGA text buffer at 0xB8000.

### `keyboard.rs` — Input
PS/2 keyboard via IRQ1, scancodes converted to ASCII.

### `serial.rs` — Debug Serial
Serial port at 0x3F8 for debug output.

### `ata.rs` — Disk Drives
ATA/IDE disk controller, supports up to 4 drives.

### `e1000.rs` — Network Interface
Intel E1000 NIC via PCI, DMA packet transfer.

### `pci.rs` — PCI Bus
PCI configuration space scanning for device detection.

### `rtc.rs` — Real-time Clock
CMOS RTC for date/time.

### `block.rs` — Block Device Interface
Abstract trait for disk-like devices.

## Network Stack

Located in `net/`:

```
User Application (Shell Commands)
         ↓
┌─────────────────────┐
│  ICMP (ping)        │  Layer 3 (network)
│  UDP               │
└─────────────────────┘
         ↓
┌─────────────────────┐
│  IP (IPv4)          │  Layer 3 (network)
└─────────────────────┘
         ↓
┌─────────────────────┐
│  ARP                │  Layer 2.5 (address resolution)
└─────────────────────┘
         ↓
┌─────────────────────┐
│  Ethernet           │  Layer 2 (data link)
└─────────────────────┘
         ↓
Hardware (E1000 NIC)
```

### Key Structures

**Ethernet Frame** (`ethernet.rs`)
```
Destination MAC (6B) | Source MAC (6B) | EtherType (2B) | Payload | CRC (4B)
```

**IPv4 Header** (`ip.rs`)
```
Version/IHL | DSCP/ECN | Total Length | ID | Flags/Fragment | TTL | Protocol | Checksum | Source IP | Dest IP | Options
```

**ICMP Header** (`icmp.rs`)
```
Type | Code | Checksum | Identifier | Sequence | Payload
```

## File System

Located in `fs/`:

**SimpleFS** — Basic block-based filesystem:
- Inode-based structure
- Directory and file support
- Read/write operations

Mounted on ATA disk when `mount` command is issued.

## Shell

Located in `shell/`:

Command parser that tokenizes input and dispatches to:
- System commands (help, about, version)
- Device commands (ifconfig, diskinfo)
- File commands (ls, cat, write)
- Network commands (ping, netstat)
- Control commands (halt, reboot)

## Memory Layout

```
Virtual Address Space (x86_64, 64-bit)
┌─────────────────────────────────┐
│     Kernel Code (0x200000)      │
├─────────────────────────────────┤
│     Kernel Data                 │
├─────────────────────────────────┤
│     Kernel BSS                  │
├─────────────────────────────────┤
│     Heap (grows up)             │
│                                 │
│                                 │
├─────────────────────────────────┤
│     Stack (grows down)          │
│                                 │
│                                 │
└─────────────────────────────────┘

Physical Address Space
┌─────────────────────────────────┐
│   Kernel @ 0x1000000            │
│   (matches virtual via boot)    │
│   ...                           │
│   Identity-mapped via           │
│   bootloader's physical memory  │
│   offset (0x20000000000)        │
└─────────────────────────────────┘
```

## Initialization Sequence

```rust
fn kernel_main(boot_info) {
    // Phase 1: Early Init
    serial::init()              // Debug output
    vga::init()                 // Terminal output
    allocator::init()           // Heap allocation
    
    // Phase 2: Interrupts
    interrupts::init_idt()      // Exception handlers
    pic::PICS.initialize()      // Interrupt controller
    
    // Phase 3: Drivers
    keyboard::init()            // Input
    ata::init()                 // Disk
    e1000::init()               // Network
    
    // Phase 4: High-Level
    net::init()                 // Network stack
    interrupts::enable()        // Start handling interrupts
    
    // Phase 5: User Interaction
    shell::run()                // Interactive shell loop
}
```

## Key Design Decisions

### Rust for Safety
- No C code
- Memory safety enforced by compiler
- No unsafe in hot paths (except hardware access)

### No_std Kernel
- No standard library (small binary)
- Custom allocator and panic handler
- Minimal dependencies

### Dynamic Dispatch
- Traits for drivers (BlockDevice)
- Lazy_static for singletons (drivers, filesystem)
- Spin mutexes for synchronization

### Identity Mapping for DMA
- Static buffers for DMA operations
- Virtual = Physical addresses in low memory
- Simplifies hardware access

### Simple Interrupt Handler
- All interrupts use same priority
- No preemption or scheduling
- Simple polling for tasks

## Performance Characteristics

| Operation | Latency | Notes |
|-----------|---------|-------|
| Keyboard Input | ~100ms | Polling every loop iteration |
| Network RX/TX | ~1ms | Processed in main loop |
| Disk Read/Write | ~10ms | Synchronous ATA operations |
| Memory Allocation | <1μs | Linked-list allocator |
| Context Switch | N/A | No multi-tasking |
| Interrupt Latency | ~10μs | IDT handler dispatch |

## Limitations

- **Single-threaded** — No process scheduling or context switching
- **No Virtual Memory** — Physical = Virtual addressing (mostly)
- **No Protection** — No privilege levels or memory isolation
- **Limited Drivers** — Only Intel E1000, ATA, VGA, PS/2
- **No TCP** — Only ICMP/UDP (ping and basic network)
- **Cooperative Shell** — No background tasks

## Extension Points

To add features:

1. **New Device** → Create `drivers/newdevice.rs`
2. **New Protocol** → Add to `net/` module
3. **New Shell Command** → Add case to `shell::execute_command()`
4. **New Filesystem** → Implement `BlockDevice` trait
5. **New Interrupt** → Register in `interrupts::init_idt()`

## Next Steps

- **[Memory Management](memory.md)** — Heap and address details
- **[Interrupt Handling](interrupts.md)** — IDT and exceptions
- **[Network Stack](networking.md)** — Protocol implementation
- **[Kernel API](api.md)** — Public kernel interfaces
