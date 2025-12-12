# Extending the Kernel

Guide to adding new features to MFK.

## Overview

MFK is designed to be extensible. You can add:
- New shell commands
- New device drivers
- Network protocols
- File system features
- Hardware support

This guide shows how.

## Adding Shell Commands

### 1. Implement Command Logic

Create a new function in the shell module:

```rust
// kernel/src/shell/mod.rs

pub fn handle_command(command: &str, args: &[&str]) -> Result<(), &'static str> {
    match command {
        // ... existing commands ...
        "mycommand" => {
            handle_mycommand(args)?;
        }
        _ => return Err("Unknown command"),
    }
    Ok(())
}

fn handle_mycommand(args: &[&str]) -> Result<(), &'static str> {
    if args.is_empty() {
        return Err("Usage: mycommand <arg>");
    }
    
    let arg = args[0];
    serial_println!("My command received: {}", arg);
    Ok(())
}
```

### 2. Add to Help Text

Update the help command to include your command:

```rust
"help" => {
    serial_println!("Available commands:");
    serial_println!("  help              - Show this message");
    // ... other commands ...
    serial_println!("  mycommand <arg>   - My new command");
}
```

### 3. Test

```bash
./build.sh
./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF
mycommand hello
halt
EOF
```

## Adding Device Drivers

### 1. Create Driver Module

```rust
// kernel/src/drivers/mydevice.rs

use volatile::Volatile;
use x86_64::PhysAddr;

const DEVICE_IO_BASE: u16 = 0x1000;

pub struct MyDevice {
    base_address: u16,
}

impl MyDevice {
    pub fn new(base_address: u16) -> Self {
        MyDevice { base_address }
    }
    
    pub fn read_register(&self, offset: u16) -> u32 {
        // Read from I/O port
        unsafe {
            x86_64::instructions::port::Port::new(self.base_address + offset).read()
        }
    }
    
    pub fn write_register(&self, offset: u16, value: u32) {
        // Write to I/O port
        unsafe {
            x86_64::instructions::port::Port::new(self.base_address + offset).write(value);
        }
    }
    
    pub fn init(&mut self) -> Result<(), &'static str> {
        // Initialize device
        self.write_register(0, 0x1);  // Enable
        Ok(())
    }
}
```

### 2. Register in Module

```rust
// kernel/src/drivers/mod.rs

pub mod mydevice;

pub fn init_all(phys_mem_offset: VirtAddr) -> Result<(), &'static str> {
    // ... existing drivers ...
    mydevice::init()?;
    Ok(())
}
```

### 3. Initialize in Main

```rust
// kernel/src/main.rs

// In kernel_main()
drivers::init_all(phys_mem_offset)?;
```

## Adding Network Protocols

### 1. Create Protocol Module

```rust
// kernel/src/net/myprotocol.rs

pub struct MyProtocolPacket {
    pub header: [u8; 8],
    pub payload: Vec<u8>,
}

impl MyProtocolPacket {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 8 {
            return Err("Packet too short");
        }
        
        let mut header = [0u8; 8];
        header.copy_from_slice(&bytes[0..8]);
        
        let payload = bytes[8..].to_vec();
        
        Ok(MyProtocolPacket { header, payload })
    }
    
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.header);
        bytes.extend_from_slice(&self.payload);
        bytes
    }
}

pub fn handle_packet(packet: &MyProtocolPacket) {
    serial_println!("Received myprotocol packet");
}
```

### 2. Integrate into Network Stack

```rust
// kernel/src/net/mod.rs

mod myprotocol;

pub fn process_packets() {
    // ... existing processing ...
    
    // Process myprotocol packets
    if let Some(packet) = receive_myprotocol_packet() {
        myprotocol::handle_packet(&packet);
    }
}
```

## Adding File System Features

### 1. Extend Inode Structure

```rust
// kernel/src/fs/mod.rs

#[repr(C, packed)]
pub struct Inode {
    // Existing fields
    pub size: u32,
    pub blocks: [u32; 12],
    
    // New fields for metadata
    pub created_at: u32,      // Unix timestamp
    pub modified_at: u32,
    pub permissions: u16,     // Unix permissions
}

impl Inode {
    pub fn new() -> Self {
        let now = get_current_timestamp();
        Inode {
            size: 0,
            blocks: [0; 12],
            created_at: now,
            modified_at: now,
            permissions: 0o644,
        }
    }
    
    pub fn is_writable(&self) -> bool {
        (self.permissions & 0o200) != 0
    }
}

fn get_current_timestamp() -> u32 {
    // Get from RTC or kernel timer
    0
}
```

### 2. Add File Operations

```rust
pub fn get_file_info(inode: &Inode) -> FileInfo {
    FileInfo {
        size: inode.size,
        created: inode.created_at,
        modified: inode.modified_at,
        readable: true,
        writable: inode.is_writable(),
    }
}
```

## Architecture Patterns

### Module Organization

```
kernel/src/
├── main.rs           # Entry and initialization
├── allocator.rs      # Memory allocation
├── drivers/
│   ├── mod.rs        # Driver initialization
│   ├── vga.rs        # Display
│   ├── keyboard.rs   # Input
│   ├── e1000.rs      # Network (existing)
│   └── mydevice.rs   # New driver
├── net/
│   ├── mod.rs        # Network init
│   ├── arp.rs
│   ├── ip.rs
│   └── myprotocol.rs # New protocol
├── fs/
│   └── mod.rs        # File system
├── shell/
│   └── mod.rs        # Command shell
└── interrupts.rs     # Interrupt handling
```

### Error Handling Pattern

Use `Result<T, &'static str>` throughout:

```rust
pub fn init_subsystem() -> Result<(), &'static str> {
    subsystem1::init()?;         // ? propagates errors
    subsystem2::init()?;
    Ok(())
}

// In kernel_main()
if let Err(e) = init_subsystem() {
    panic!("Failed to initialize: {}", e);
}
```

### Unsafe Blocks

Minimize unsafe code and document it:

```rust
pub fn unsafe_operation() {
    // SAFETY: Device memory is identity-mapped by bootloader
    // and never deallocated, making this pointer valid.
    unsafe {
        let ptr = 0xDEADBEEF as *mut u32;
        *ptr = 42;  // Write to device register
    }
}
```

## Performance Considerations

### Allocations

Minimize heap allocations in hot paths:

```rust
// Bad: allocates on every call
pub fn process_packet(bytes: &[u8]) {
    let data = bytes.to_vec();  // Allocation!
    // ...
}

// Good: use references
pub fn process_packet(bytes: &[u8]) {
    for &b in bytes {
        // Process each byte
    }
}
```

### Polling vs Interrupts

For drivers, prefer interrupts over polling:

```rust
// Polling: wastes CPU
loop {
    if device.has_data() {
        handle_data();
    }
}

// Interrupts: efficient
device.set_interrupt_handler(handle_data);
```

### Memory Layout

Be mindful of cache lines and alignment:

```rust
// Good: aligned for cache efficiency
#[repr(C, align(64))]
struct CacheAligned {
    data: u32,
}

// Bad: poor cache behavior
struct Unaligned {
    a: u8,
    b: u32,  // Misaligned!
    c: u16,
}
```

## Testing Your Changes

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_mycommand() {
        let result = handle_mycommand(&["arg"]);
        assert!(result.is_ok());
    }
}
```

### Integration Tests

```bash
./run.sh target/x86_64-mfk/release/mfk-kernel <<EOF
mycommand arg
help
halt
EOF
```

### Regression Tests

```bash
./regression_tests.sh  # Ensure nothing broke
```

## Documentation

### Code Comments

```rust
/// Initializes the E1000 network device.
///
/// # Arguments
/// * `phys_mem_offset` - Physical memory offset for address translation
///
/// # Returns
/// Ok(()) if initialization succeeded, Err with message otherwise
///
/// # Safety
/// Accesses memory-mapped I/O registers.
pub fn init(phys_mem_offset: VirtAddr) -> Result<(), &'static str> {
    // ...
}
```

### Module Documentation

```rust
//! The E1000 driver provides network functionality.
//!
//! # Usage
//!
//! Initialize the driver and send/receive packets:
//!
//! ```ignore
//! e1000::init(phys_mem_offset)?;
//! let packet = e1000::receive_packet()?;
//! ```

pub mod e1000 {
    // ...
}
```

## Common Patterns

### Initialization Pattern

```rust
pub struct Subsystem {
    initialized: bool,
}

impl Subsystem {
    pub fn init(&mut self) -> Result<(), &'static str> {
        if self.initialized {
            return Err("Already initialized");
        }
        // Do initialization
        self.initialized = true;
        Ok(())
    }
}
```

### Resource Cleanup Pattern

```rust
pub struct Resource {
    // ...
}

impl Drop for Resource {
    fn drop(&mut self) {
        // Cleanup code
    }
}
```

### State Machine Pattern

```rust
#[derive(Debug, Clone, Copy)]
pub enum DeviceState {
    Uninitialized,
    Initializing,
    Ready,
    Error,
}

impl Device {
    pub fn transition(&mut self, state: DeviceState) {
        self.state = state;
    }
}
```

## Debugging Extensions

### Adding Debug Logging

```rust
pub fn my_function() {
    serial_println!("DEBUG: my_function called");
    serial_println!("DEBUG: state = {:?}", self.state);
}
```

### Adding Assertions

```rust
pub fn process_data(data: &[u8]) {
    assert!(!data.is_empty(), "Data cannot be empty");
    assert!(data.len() < 4096, "Data too large");
    // ...
}
```

## Performance Profiling

### Measuring Execution Time

```rust
let start = x86_64::instructions::rdtsc();
expensive_operation();
let end = x86_64::instructions::rdtsc();
serial_println!("Took {} cycles", end - start);
```

## Common Mistakes

### ❌ Not checking initialization

```rust
// WRONG: Assumes initialized
unsafe { 
    DEVICE.write_register(0, 42);
}

// RIGHT: Check first
if !device.is_initialized() {
    device.init()?;
}
device.write_register(0, 42)?;
```

### ❌ Excessive allocations

```rust
// WRONG: Allocates Vec on every call
fn process(data: &[u8]) {
    let vec = data.to_vec();
    // ...
}

// RIGHT: Use references
fn process(data: &[u8]) {
    // Use data directly
}
```

### ❌ Forgetting error handling

```rust
// WRONG: Unwrap silently fails
device.init().unwrap();

// RIGHT: Handle error
device.init()?;
```

## Next Steps

- **[Testing Guide](testing.md)** — Test your extensions
- **[Building Guide](building.md)** — Understand the build system
- **[Architecture Reference](../reference/architecture.md)** — System design

## Resources

- **[Rust Book](https://doc.rust-lang.org/book/)** — Rust language guide
- **[x86_64 Crate Docs](https://docs.rs/x86_64/)** — CPU operations
- **[OSDev Wiki](https://wiki.osdev.org/)** — OS development reference
