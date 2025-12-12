# Memory Management

Deep dive into MFK's memory system.

## Memory Layout

### Address Space Overview

```
Virtual Address Space (x86_64)
────────────────────────────────────────────

0xFFFFFFFF80000000  ┌─────────────────────┐
                    │  Kernel Code/Data   │
                    │  (Identity mapped)   │
                    │  512MB              │
                    │                     │
0xFFFFFFFF80000000 + 512MB
                    │  Heap (100KB-1MB)   │
                    │                     │
Lower Virtual       │  Stack              │
Addresses           │  (grows down)       │
                    │                     │
0x00000000          │  User space/BIOS    │
                    │  (not used yet)     │
                    └─────────────────────┘

Physical Address Space
────────────────────────────────────────────

0x0000000000000000  ┌─────────────────────┐
                    │  BIOS/Real mode     │
                    │  (first 1MB)        │
                    │                     │
0x0000000000100000  │  Bootloader         │
                    │                     │
0x0000000000200000  │  Kernel ELF         │
                    │  (loaded by boot)   │
                    │                     │
0x0000000010000000  │  Free RAM           │
                    │  (available for     │
                    │   heap/DMA)         │
                    │                     │
                    └─────────────────────┘
```

### Physical Memory Offset

The bootloader provides a **physical memory offset** that allows kernel code to access any physical address:

```rust
// From bootloader
let phys_mem_offset = VirtAddr::new(0xFFFFFFFF80000000);

// Access physical address:
let physical = PhysAddr::new(0x1000);
let virtual_addr = phys_mem_offset + physical.as_u64();
let reference = &*(virtual_addr.as_ptr() as *const u32);
```

In practice, **MFK uses identity mapping**: Physical address 0x1000 → Virtual address 0x1000. This means no conversion is needed.

## Allocator Design

### BumpAllocator

MFK uses a simple bump allocator for initial memory allocation:

```rust
pub struct BumpAllocator {
    heap_start: usize,
    heap_end: usize,
    next: usize,
}

impl BumpAllocator {
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        // Align to requested alignment
        let aligned = align_up(self.next, layout.align());
        
        // Check bounds
        if aligned + layout.size() > self.heap_end {
            return core::ptr::null_mut();
        }
        
        // Allocate
        self.next = aligned + layout.size();
        aligned as *mut u8
    }
    
    pub fn deallocate(&mut self, ptr: *mut u8, layout: Layout) {
        // Bump allocator doesn't support deallocation
        // (except via reset)
    }
}
```

**Characteristics:**
- ✅ Very fast (O(1) allocation)
- ✅ No fragmentation
- ✅ Low overhead
- ❌ Can't free individual allocations
- ❌ Wastes space if some allocations are freed

**When to use:** Best for systems that mostly allocate at startup and never free.

### Heap Setup

The heap is initialized early in boot:

```rust
// kernel/src/main.rs

unsafe {
    allocator::init_heap(&mut HEAP, HEAP_START, HEAP_SIZE)
        .expect("Heap initialization failed");
}
```

**Default heap:**
- **Size:** 100 KB (configurable in allocator.rs)
- **Start:** `0xABCD0000` (defined in allocator.rs)
- **Growth:** Static, no dynamic expansion

### Allocation Strategy

**Typical allocation workflow:**
```
1. Request Vec::new()
2. GlobalAlloc::alloc() called
3. BumpAllocator.allocate() bumps pointer
4. Memory initialized with zeros
5. Pointer returned to Vec
```

**Deallocations:**
```
1. Variable dropped (implicit or explicit)
2. Drop trait called
3. Memory no longer accessible
4. BumpAllocator: memory wasted until reset
```

## Memory Safety

### Alignment

Hardware requires certain alignments:
- `u32`: 4-byte aligned
- `u64`: 8-byte aligned  
- Structures: Aligned to largest field

```rust
// Good: Fields naturally aligned
#[repr(C)]
struct Good {
    a: u8,
    b: u32,     // Padded to 4-byte boundary
    c: u64,     // At 8-byte boundary
}

// Bad: Manual packing wastes space
#[repr(C, packed)]
struct Bad {
    a: u8,
    b: u32,     // Unaligned! CPU penalty
    c: u64,
}
```

### DMA Buffer Alignment

DMA buffers must be aligned for hardware:

```rust
#[repr(C, align(4096))]  // 4KB page alignment
pub struct DmaBuffer {
    rx_descriptors: [RxDescriptor; 32],
    tx_descriptors: [TxDescriptor; 32],
    rx_buffers: [[u8; 2048]; 32],
    tx_buffers: [[u8; 2048]; 32],
}
```

This ensures DMA controller can access buffers correctly.

### Virtual vs Physical Addresses

**Key insight:** Most kernel code uses virtual addresses. But hardware (like NIC) needs physical addresses.

```rust
// Virtual address: kernel code uses this
let virt = &some_data as *const _ as VirtAddr;

// Physical address: hardware uses this
let phys = match virt_to_phys(virt) {
    Ok(p) => p,
    Err(e) => { /* handle */ }
};

// For DMA:
device.set_buffer_address(phys);  // Hardware understands this
```

## Memory Regions

### Code Section

- **Virtual:** 0xFFFFFFFF80000000 onwards
- **Physical:** 0x200000 onwards  
- **Permissions:** Read + Execute (initially)
- **Visibility:** All modules

### Data Section

- **Virtual:** After code (varies)
- **Physical:** After code
- **Permissions:** Read + Write
- **Visibility:** All modules

### Heap

- **Virtual:** 0xABCD0000 onwards
- **Physical:** Depends on allocations
- **Permissions:** Read + Write
- **Visibility:** Through Allocator trait

### Stack

- **Virtual:** Below kernel code (grows down)
- **Physical:** Various
- **Permissions:** Read + Write
- **Visibility:** Local to functions

### Static Data

Global variables live in the data section:

```rust
pub static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
pub static COMMAND_BUFFER: Mutex<VecDeque<char>> = 
    Mutex::new(VecDeque::new());
```

## Memory Pressure

### Heap Exhaustion

If heap fills up:

```rust
pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
    if aligned + layout.size() > self.heap_end {
        // Heap full!
        return core::ptr::null_mut();  // Null indicates failure
    }
    // ...
}
```

**Allocation failure handling:**
```rust
let vec: Result<Vec<u8>, _> = vec.try_reserve(1000);
match vec {
    Ok(v) => { /* use vector */ }
    Err(_) => { /* handle out of memory */ }
}
```

### Monitoring Memory

Check current heap usage:

```rust
// In shell: memory command
pub fn handle_memory() {
    let used = current_heap_used();
    let total = HEAP_SIZE;
    serial_println!("Heap: {}/{} bytes", used, total);
}
```

## Unsafe Memory Access

Unsafe is needed for:
- Hardware register access
- Buffer descriptors (physical addresses)
- Raw pointers to hardware memory

**Safe pattern:**
```rust
pub fn safe_hardware_access() {
    // SAFETY: This register is memory-mapped by bootloader
    // and valid for entire kernel lifetime.
    unsafe {
        let reg = &*(0xDEADBEEF as *const u32);
        serial_println!("Value: {}", reg);
    }
}
```

**Dangerous pattern:**
```rust
// WRONG: Unvalidated pointer from untrusted source
unsafe {
    let ptr = user_input_as_ptr();  // Where does it point?
    *ptr = 42;  // Could write anywhere!
}
```

## Memory Debugging

### Checking Alignment

```rust
fn check_alignment<T>() {
    let align = core::mem::align_of::<T>();
    serial_println!("Alignment of {}: {}", 
        core::any::type_name::<T>(), 
        align);
}
```

### Validating Pointers

```rust
fn is_valid_pointer(ptr: *const u8) -> bool {
    // Check if pointer is in kernel space
    let addr = ptr as usize;
    addr >= KERNEL_START && addr < KERNEL_END
}
```

### Memory Dumps

```rust
fn dump_memory(addr: *const u8, len: usize) {
    for i in 0..len {
        unsafe {
            serial_print!("{:02X} ", *addr.add(i));
            if (i + 1) % 16 == 0 {
                serial_println!();
            }
        }
    }
}
```

## Future Enhancements

### Better Allocator

Consider implementing:
- **Buddy Allocator** — Balance between speed and fragmentation
- **Slab Allocator** — Fast allocation of fixed-size objects
- **Paging** — Virtual memory for more address space

### Memory Protection

Add later:
- **Write protection** — Mark code sections read-only
- **Bounds checking** — Runtime verification
- **Canaries** — Detect buffer overflows

### Automatic Cleanup

Future work:
- **Reference counting** — Automatic deallocation
- **Garbage collection** — Managed memory
- **RAII** — Resource acquisition as initialization

## Next Steps

- **[Interrupts Reference](interrupts.md)** — How exceptions handled
- **[Drivers Reference](drivers.md)** — Hardware access patterns
- **[Architecture Overview](architecture.md)** — System organization
