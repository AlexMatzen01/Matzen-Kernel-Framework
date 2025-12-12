# Interrupt Handling

MFK's interrupt and exception system.

## Overview

Interrupts are signals from hardware (or software) that pause normal execution to handle events.

**Types:**
- **Exceptions** — CPU-generated (divide by zero, page fault)
- **Hardware Interrupts** — Device-generated (keyboard, timer)
- **Software Interrupts** — Application-generated (syscalls)

## Exception Handling

### CPU Exceptions

The CPU generates exceptions for error conditions:

| Exception | Code | Cause |
|-----------|------|-------|
| Divide Error | 0 | Division by zero |
| Debug | 1 | Debugger breakpoint |
| Page Fault | 14 | Invalid memory access |
| General Protection | 13 | Invalid memory/segment |

**Example: Page Fault**
```
User code tries to access 0xDEADBEEF
↓
CPU checks if address valid
↓
Address not mapped → Page Fault exception
↓
CPU pushes context (RIP, RFlags, RSP)
↓
IDT points to page_fault_handler
↓
Handler executes with saved context
```

### Handler Implementation

```rust
// kernel/src/interrupts.rs

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame, 
    error_code: PageFaultErrorCode,
) {
    // Save registers here (compiler does this automatically)
    
    // Handle the fault
    match error_code {
        code if code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) => {
            panic!("Protection violation at {:?}", Cr2::read());
        }
        code if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) => {
            panic!("Write fault at {:?}", Cr2::read());
        }
        _ => {
            panic!("Page fault at {:?}", Cr2::read());
        }
    }
}

pub fn init_idt() {
    let mut idt = InterruptDescriptorTable::new();
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.load();
}
```

## Interrupt Descriptor Table (IDT)

The IDT is a table that maps interrupt numbers to handlers:

```
Interrupt Vector  │  Handler
──────────────────┼──────────────────
0                 │  divide_error_handler
1                 │  debug_handler
2                 │  nmi_handler
...               │  ...
14                │  page_fault_handler
...               │  ...
32                │  timer_handler (first hardware)
33                │  keyboard_handler
...               │  ...
255               │  last_possible_handler
```

### Loading the IDT

```rust
// kernel/src/interrupts.rs

pub fn init_interrupts() {
    let mut idt = InterruptDescriptorTable::new();
    
    // CPU exceptions (0-31)
    idt.divide_error.set_handler_fn(divide_error_handler);
    idt.debug.set_handler_fn(debug_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    
    // Hardware interrupts (32+)
    idt[32].set_handler_fn(timer_handler);
    idt[33].set_handler_fn(keyboard_handler);
    
    idt.load();
}
```

## Hardware Interrupts

### Programmable Interrupt Controller (PIC)

The PIC manages hardware interrupts from devices:

```
Devices → PIC → IRQ → CPU
         (on pin 2)
```

**Interrupt mapping:**
- IRQ 0 → Vector 32 (Timer)
- IRQ 1 → Vector 33 (Keyboard)
- IRQ 2 → Vector 34 (Cascade/Slave)
- IRQ 3 → Vector 35 (COM2/COM4)
- IRQ 4 → Vector 36 (COM1/COM3)
- IRQ 5 → Vector 37 (Parallel port)
- IRQ 6 → Vector 38 (Floppy disk)
- IRQ 7 → Vector 39 (Parallel port)

### PIC Initialization

```rust
// kernel/src/pic.rs

pub struct ChainedPics {
    pics: [Pic; 2],
}

impl ChainedPics {
    pub unsafe fn init(&mut self) {
        // ICW1: Initialize
        self.pics[0].command.write(ICW1_INIT | ICW1_ICW4);
        self.pics[1].command.write(ICW1_INIT | ICW1_ICW4);
        
        // ICW2: Vector offsets
        self.pics[0].data.write(MASTER_OFFSET);   // Start at 32
        self.pics[1].data.write(SLAVE_OFFSET);    // Start at 40
        
        // ICW3: Cascade
        self.pics[0].data.write(4);               // Slave on IRQ2
        self.pics[1].data.write(2);               // Connected to IRQ2
        
        // ICW4: Environment
        self.pics[0].data.write(ICW4_8086);
        self.pics[1].data.write(ICW4_8086);
        
        // Mask all (disable for now)
        self.pics[0].data.write(0xFF);
        self.pics[1].data.write(0xFF);
    }
    
    pub unsafe fn enable_interrupt(&self, irq: u8) {
        if irq < 8 {
            self.pics[0].enable(irq);
        } else {
            self.pics[1].enable(irq - 8);
            self.pics[0].enable(2);  // Enable slave
        }
    }
}
```

### Handling Hardware Interrupts

```rust
extern "x86-interrupt" fn timer_handler(stack_frame: InterruptStackFrame) {
    serial_print!(".");  // Show activity
    
    // Acknowledge interrupt
    unsafe {
        PICS.lock().notify_end_of_interrupt(0);
    }
}

extern "x86-interrupt" fn keyboard_handler(stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    
    // Process scancode
    handle_keyboard(scancode);
    
    // Acknowledge
    unsafe {
        PICS.lock().notify_end_of_interrupt(1);
    }
}
```

## Interrupt Flow

### Step-by-Step Example: Keyboard Press

```
1. User presses 'A' key
   ↓
2. Keyboard device detects press
   ↓
3. Device pulls IRQ1 line low
   ↓
4. PIC reads IRQ1
   ↓
5. PIC sends interrupt signal to CPU
   ↓
6. CPU completes current instruction
   ↓
7. CPU checks IDT[33] (IRQ1 → Vector 33)
   ↓
8. CPU pushes current context:
   - Return address (RIP)
   - Flags (RFLAGS)
   - Stack pointer (RSP)
   ↓
9. CPU jumps to keyboard_handler
   ↓
10. Handler reads port 0x60
    → Gets scancode (0x1E for 'A')
    ↓
11. Handler processes scancode
    → Adds to key buffer
    ↓
12. Handler sends EOI (End of Interrupt)
    → Tells PIC interrupt is handled
    ↓
13. Handler executes `iret` instruction
    ↓
14. CPU pops context
    ↓
15. Normal execution resumes
```

## Interrupt Context

When an interrupt occurs, the CPU saves the context:

```rust
pub struct InterruptStackFrame {
    pub instruction_pointer: VirtAddr,  // Where we were
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: VirtAddr,
    pub stack_segment: u64,
}
```

**Important:** The stack frame is provided **by the CPU**, not by our code.

## Interrupt Priorities

Hardware interrupts have priorities:

```
Priority (Highest)
    │
    ├─ Timer (IRQ0)
    ├─ Keyboard (IRQ1)  
    ├─ PIC Cascade (IRQ2)
    ├─ Serial (IRQ3, 4)
    ├─ Parallel (IRQ5, 7)
    └─ Floppy (IRQ6)
    │
Priority (Lowest)
```

Generally, handle interrupts quickly and return.

## Exception Recovery

Some exceptions can be recovered from:

```rust
extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!("General Protection Fault!");
    serial_println!("At: {:#x}", stack_frame.instruction_pointer);
    
    // If recoverable, could modify stack_frame to continue
    // Most cases: terminate process or kernel
    
    loop {
        x86_64::instructions::hlt();
    }
}
```

## Timer Interrupt

The timer generates periodic interrupts:

```rust
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Called periodically (typically 1ms intervals)
    
    // This is the heartbeat of the OS
    // Could use for:
    // - Time tracking
    // - Process scheduling
    // - Timeout enforcement
    // - Periodic cleanup
    
    unsafe {
        PICS.lock().notify_end_of_interrupt(0);
    }
}
```

## Disabling/Enabling Interrupts

Sometimes you need to disable interrupts (for atomic operations):

```rust
use x86_64::instructions::interrupts;

// Disable interrupts
let flags_before = interrupts::read_flags();
interrupts::disable();
// Critical section here - no interrupts!
interrupts::set_flags(flags_before);

// Or use helper:
interrupts::without_interrupts(|| {
    // This block runs with interrupts disabled
    // They're automatically re-enabled after
});
```

## Common Exceptions

### Divide by Zero

```rust
fn bad_divide() {
    let x = 1;
    let y = 0;
    let result = x / y;  // ← CPU generates exception here
}
```

**Handling:**
```rust
extern "x86-interrupt" fn divide_error_handler(
    stack_frame: InterruptStackFrame,
) {
    panic!("Divide by zero at {:?}", stack_frame);
}
```

### Invalid Memory Access

```rust
fn bad_access() {
    let ptr = 0xDEADBEEF as *const u32;
    let value = unsafe { *ptr };  // ← Page fault here
}
```

**Handling:** Page fault handler can potentially fix (e.g., demand paging in future).

### Stack Overflow

```rust
fn stack_overflow() {
    let mut arr = [0u8; 1_000_000];  // Huge stack allocation
    stack_overflow();  // Recursive call
    // Eventually: stack exhausted → page fault
}
```

**Handling:** Very difficult to recover; usually fatal.

## Debugging Interrupts

### Enabling Interrupt Logging

```rust
extern "x86-interrupt" fn generic_handler(stack_frame: InterruptStackFrame) {
    serial_println!("Interrupt at {:#x}", 
        stack_frame.instruction_pointer);
}
```

### Checking if Interrupts Enabled

```rust
let enabled = x86_64::instructions::interrupts::are_enabled();
if enabled {
    serial_println!("Interrupts are enabled");
} else {
    serial_println!("Interrupts are disabled");
}
```

## Next Steps

- **[Memory Reference](memory.md)** — Memory layout and allocation
- **[Drivers Reference](drivers.md)** — Hardware interaction
- **[Architecture Overview](architecture.md)** — System design
