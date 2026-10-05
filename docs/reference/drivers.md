# Drivers Reference

Hardware drivers and interface documentation.

## Overview

MFK includes drivers for common hardware:

| Driver | Module | Purpose |
|--------|--------|---------|
| VGA | `vga.rs` | Display output |
| Keyboard | `keyboard.rs` | User input |
| E1000 | `e1000.rs` | Network interface |
| Serial | `serial.rs` | Debug console |
| PIC | `pic.rs` | Interrupt controller |
| RTC | `rtc.rs` | Real-time clock |
| ATA | `ata.rs` | Disk interface |

## VGA Driver

### Purpose

Provides text-mode display output on VGA-compatible monitors.

### Initialization

```rust
pub fn init() -> Result<(), &'static str> {
    // Clear screen
    for col in 0..80 {
        for row in 0..25 {
            write_char(col, row, ' ', DEFAULT_COLOR);
        }
    }
    Ok(())
}
```

### Writing Text

```rust
pub fn write(data: &str, color: Color) {
    for char in data.chars() {
        write_char(CURSOR_COL, CURSOR_ROW, char, color);
        advance_cursor();
    }
}
```

### Memory Layout

VGA memory is at `0xB8000` (physical) and contains:

```
Offset  Content
────────────────────────
0       'A' (char)
1       0x0F (color byte)
2       'B' (char)
3       0x0F (color byte)
...
```

Each character position takes 2 bytes (character + color).

### Colors

```rust
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    // ... etc
}
```

## Keyboard Driver

### Purpose

Reads keyboard input. The PS/2 controller on I/O port 0x60 is always
available; USB HID keyboards are claimed by the host-controller drivers
below and pushed in via `keyboard::push_usb_report()`. PS/2 is never
disabled, so it remains a fallback when a USB device fails to enumerate.

### Port 0x60 (Keyboard Data)

```
Bit  Meaning
────────────────
7    Key release (1) or press (0)
6-0  Scan code
```

### Initialization

```rust
pub fn init() -> Result<(), &'static str> {
    // Enable keyboard interrupt (IRQ1)
    unsafe {
        PICS.lock().enable_interrupt(1);
    }
    Ok(())
}
```

### Reading Input

```rust
pub fn read_key() -> Option<char> {
    unsafe {
        let mut port: Port<u8> = Port::new(0x60);
        let scancode: u8 = port.read();
        
        // Convert scancode to ASCII
        match scancode {
            0x1E => Some('a'),
            0x30 => Some('b'),
            // ... mapping table
            _ => None,
        }
    }
}
```

### Scan Codes

Common scan codes:

```
Scan  Key
──────────
0x1E  'A'
0x2E  'C'
0x2D  'X'
0x20  'D'
0x12  'E'
0x2F  'V'
0x1C  Enter
0x0E  Backspace
0x0F  Tab
```

### Key event tracing

`keyboard::KEY_TRACE` (`drivers/keyboard.rs`) turns on a serial trace of
every key event, tagged with the source that produced it. It exists to tell
apart the three places a key can be lost:

| Tag | Emitted by | Meaning |
| --- | --- | --- |
| `[key] xhci claimed slot=.. ep=.. maxpacket=.. binterval=.. xfer_len=..` | `xhci::claim_hid_keyboards` | The HID keyboard endpoint was claimed at all |
| `[key] xhci kb slot=.. len=.. <hex>` | `xhci::handle_hid_completion` | Bytes as they came off the bus, before the 8-byte boot slice |
| `[key] usb raw = <hex>` | `keyboard::push_usb_report` | Boot-protocol report on entry, before any filtering |
| `[key] usb suppress = <hex>` | `keyboard::push_usb_report` | Usage dropped by the `LAST_HID_REPORT` press-edge filter (held key) |
| `[key] usb unmapped = <hex>` | `keyboard::push_usb_report` | Usage matched neither `hid_to_ascii` nor `hid_to_key` |
| `[key] usb -> <Key>` | `keyboard::push_usb_report` | Event decoded and queued |
| `[key] ps2 sc = <hex>` | `keyboard::handle_interrupt` | Raw scancode from IRQ1 |
| `[key] ps2 -> <Key>` | `keyboard::handle_interrupt` | Event decoded and queued |

An HID usage that never reaches a `-> <Key>` line is dropped somewhere in the
transport; a usage that reaches `-> <Key>` but does nothing is a dispatch bug
in the consumer.

Enable it by flipping `KEY_TRACE` in `drivers/keyboard.rs`, then build and
boot as usual (the guest console is already on `-serial stdio`):

```console
$ cargo build -p mfk-kernel --target targets/x86_64-mfk.json \
    -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

Reproduce the key slowly and paste the serial log.

> **Leave it off while typing quickly.** Each line is a blocking write at
> 115200 baud, while USB reports are drained only once per
> shell/desktop/editor loop iteration. Tracing fast input can overflow the xHCI
> event ring and swallow the very reports under investigation, producing a
> misleading log. Turn it back off once you have what you need.

## USB Host Controller Drivers

### Purpose

Claim USB host controllers, enumerate the devices on their root ports, and
feed HID boot reports into the keyboard and mouse subsystems.

Four controllers are supported, each in its own module. All are
**polling-only**: no IRQ, no MSI, and no interrupt-driven completion.
`usb::poll()` is called from the shell loop and checks descriptor status.

| Driver | Module | PCI class/subclass/prog-if | Bus |
| --- | --- | --- | --- |
| EHCI | `drivers/usb.rs` | `0c/03/20` | MMIO, high-speed |
| xHCI | `drivers/xhci.rs` | `0c/03/30` | MMIO, USB 2.0 + 3.x |
| UHCI | `drivers/uhci.rs` | `0c/03/00` | I/O, full/low-speed |
| OHCI | `drivers/ohci.rs` | `0c/03/10` | I/O, full/low-speed |

### Shared descriptor parsing

The config-descriptor walkers are pure `&[u8]` functions in `drivers/usb.rs`
and are re-exported to the other drivers, so all four agree on what a device
is:

```rust
pub(crate) fn find_hid_keyboard(cfg: &[u8]) -> Option<(u8, u8, u16)>;   // iface, ep, maxpacket
pub(crate) fn find_hid_mouse(cfg: &[u8]) -> Option<(u8, u8, u16)>;
pub(crate) fn find_hid_pointer(cfg: &[u8]) -> Option<(u8, u8, u16, HidPointerKind)>;
pub(crate) fn hid_binterval(cfg: &[u8], ep_addr: u8) -> u8;
pub(crate) fn parse_tablet_layout(desc: &[u8]) -> Option<TabletLayout>;
pub(crate) fn setup_packet(bm, req, value, index, len) -> [u8; 8];
```

Reports are handed to the subsystems by one shared function, so a keyboard
behaves identically regardless of which controller claimed it:

```rust
pub(crate) fn dispatch_report(kind: StreamKind, layout: Option<TabletLayout>, data: &[u8]);
```

### UHCI: frame list

UHCI schedules from a 1024-entry frame list of TD pointers (exactly one 4 KiB
page). The async schedule lives in entry 1023 and is selected by SOFMOD bit
2, so control transfers publish their TD chain there and switch SOFMOD.

Interrupt endpoints spread several identical TDs across frame-list slots:

```
stride = clamp(2^(bInterval-1), 2, 16)   // frames
slots  = min(1024 / stride, 16)
```

The clamp matters. A bInterval of 10 means 2^9 = 512 ms if taken literally,
and the frame list wraps every 1024 ms, so a single TD per endpoint would
give roughly one poll per second. bInterval is a maximum latency, not a floor,
and devices must tolerate faster polling, so the stride is capped at 16 ms.

TD layout is 32 bytes: link pointer, control/status, token, and four buffer
pointers. A SETUP stage uses MaxLen 11, matching Linux `uhci-hcd`.

### OHCI: ED lists and the HCCA

OHCI has no frame list. The controller walks endpoint-descriptor lists, and
**periodic (interrupt) EDs are reachable only through the 32-entry schedule
table inside the HCCA**, which is 256 bytes and must be programmed after
`HcCommandStatus.HCR` because a reset clears `HcHCCA`.

```text
ED (32B):  hwINFO | hwTailP | hwHeadP | hwNextED
TD (16B):  hwINFO | hwCBP    | hwNextTD| hwBE
```

Two details are easy to get wrong:

* **The tail is the terminator, not the last real TD.** The controller runs
  `while (head != tail)`, so a chain is published by filling the TDs and then
  writing `hwTailP` once, pointing at an inert dummy TD. Descriptor contents
  must be visible before that single write.
* **A TD's length comes from `hwCBP`/`hwBE`, not a length field**, and a
  zero-length transfer is encoded as `hwCBP == 0 && hwBE == 0`.

Completion is detected by the condition-code field: TDs are armed with
`TD_NOTACCESSED` (0xF) and the controller overwrites it, so `CC != 0xF` means
the transfer retired. `HcDoneHead` is written back by the controller and must
be cleared by the driver, or the retired-TD chain grows without bound.

For a control transfer the TDs are SETUP (`TD_T_DATA0`), DATA (`TD_T_DATA1`
plus `TD_R` so a short IN does not halt the ED), and STATUS (zero length,
opposite direction). Control direction lives in the TDs, not the ED.

### I/O vs MMIO BARs

UHCI and OHCI decode **I/O** BARs, unlike EHCI and xHCI which are MMIO.
`pci::bar_base()` deliberately returns `None` when bit 0 of the BAR is set, so
those drivers use `pci::bar_io_base()` and the `io_r*`/`io_w*` helpers in
`drivers/usb.rs`.

Only firmware-assigned bases are accepted. If a BAR is zero — a hot-added or
firmware-less controller — the driver logs the address it would have needed
and skips the controller rather than guessing a port window that could
collide with other ranges.

### Companion handshake with EHCI

On real chipsets an EHCI root port is shared with a UHCI/OHCI companion, and
EHCI's `PORTSC` owner bit selects which half may drive it. When a companion
driver is present, `EhciController::reset_port` reads that bit instead of
clearing it, so the two halves do not both reset and enable the same physical
port. With no companion loaded the historical steal-the-port behavior is kept,
so machines with only EHCI are unaffected.

This is what makes low-speed devices work on real hardware: EHCI cannot carry
full/low-speed traffic at all, so a keyboard behind a companion that nothing
drives has no path to the host.

### Verifying in QEMU

Pick the controller with the runner flag and check the result in the guest
with the `usb` shell command, which prints every controller found on the bus
and the live driver status:

```
usb                                          # PS/2: exercise the machine-default UHCIs
cargo run -p mfk-runner -- --qemu --kbd=uhci
cargo run -p mfk-runner -- --qemu --kbd=ehci
cargo run -p mfk-runner -- --qemu --kbd=ohci # needs pci-ohci in your QEMU
```

`pci-ohci` is a QEMU build option. Confirm it is available with
`qemu-system-x86_64 -device help` before relying on `--kbd=ohci`.

## E1000 Driver

### Purpose

Intel E1000 network interface controller (NIC) driver.

### Hardware Interface

The E1000 is controlled via memory-mapped I/O registers:

```rust
const E1000_IOBASE: u64 = 0xFE000000;  // Physical address

// Important registers:
const REG_CTRL:    u32 = 0x0000;       // Device control
const REG_STATUS:  u32 = 0x0008;       // Device status
const REG_EEPROM:  u32 = 0x0014;       // EEPROM
const REG_MDIC:    u32 = 0x0020;       // MDI control
const REG_ICR:     u32 = 0x00C0;       // Interrupt cause
const REG_IMS:     u32 = 0x00D0;       // Interrupt mask
const REG_RCTL:    u32 = 0x0100;       // Receive control
const REG_RDBAL:   u32 = 0x2800;       // RX descriptors low
const REG_RDBAH:   u32 = 0x2804;       // RX descriptors high
const REG_RDLEN:   u32 = 0x2808;       // RX descriptors length
const REG_RDH:     u32 = 0x2810;       // RX head
const REG_RDT:     u32 = 0x2818;       // RX tail
const REG_TCTL:    u32 = 0x0400;       // Transmit control
const REG_TDBAL:   u32 = 0x3800;       // TX descriptors low
const REG_TDBAH:   u32 = 0x3804;       // TX descriptors high
const REG_TDLEN:   u32 = 0x3808;       // TX descriptors length
const REG_TDH:     u32 = 0x3810;       // TX head
const REG_TDT:     u32 = 0x3818;       // TX tail
```

### Initialization Steps

```rust
pub fn init(phys_mem_offset: VirtAddr) -> Result<(), &'static str> {
    // 1. Reset device
    reset()?;
    
    // 2. Read MAC address from EEPROM
    let mac = read_mac_address()?;
    
    // 3. Setup receive descriptors
    setup_rx_descriptors()?;
    
    // 4. Setup transmit descriptors
    setup_tx_descriptors()?;
    
    // 5. Initialize statistics
    init_statistics()?;
    
    // 6. Setup filtering
    setup_multicast()?;
    
    // 7. Enable receive/transmit
    enable_rx()?;
    enable_tx()?;

    // 8. Leave the interrupt mask clear: the card may have latched causes
    //    during setup, and unmasking with a stale bit set fires once for work
    //    already handled. The mask is enabled after the driver is published.
    Ok(())
}
```

### Interrupts

`init` reads the assigned INTx pin and line from PCI config offset 0x3C, which
is where firmware records them. If the line is one the IDT has a handler for
(IRQ10 or IRQ11 under the legacy `pc` machine), the cause register is unmasked
and the PIC line is opened; otherwise the driver stays in polling mode and
reports so, rather than unmasking a cause with no vector behind it.

The handler acknowledges only. It reads ICR — which clears the latched cause and
lets the line deassert — records the cause, sets a pending flag, and sends the
PIC EOI. It takes no lock and allocates nothing, because on a uniprocessor it
can interrupt `receive_packet` mid-update: taking the driver spin lock there
would deadlock against the code that raised the interrupt. The ring is drained
by the pump instead.

### Receive (RX) Descriptors

```rust
#[repr(C)]
pub struct RxDescriptor {
    pub buffer_addr: u64,    // Physical address of buffer
    pub length: u16,         // Length
    pub checksum: u16,       // Checksum
    pub status: u8,          // Status
    pub errors: u8,          // Errors
    pub vlan: u16,           // VLAN tag
}
```

The driver maintains a ring of RX descriptors:

```
Device fills buffer → Sets status → Tail pointer advances

RX Ring Buffer:
[desc0] [desc1] [desc2] [desc3]
  ↑ Head              ↑ Tail
  (driver reads)      (device writes)
```

### Transmit (TX) Descriptors

```rust
#[repr(C)]
pub struct TxDescriptor {
    pub buffer_addr: u64,    // Physical address of data
    pub length: u16,         // Data length
    pub cso: u8,             // Checksum offset
    pub cmd: u8,             // Command
    pub status: u8,          // Status
    pub css: u8,             // Checksum start
    pub special: u16,        // Special
}
```

The driver writes packets to TX ring:

```
Driver fills buffer → Sets command → Tail pointer advances

TX Ring Buffer:
[desc0] [desc1] [desc2] [desc3]
        ↑ Head               ↑ Tail
        (device reads)       (driver writes)
```

### MAC Address

Read from EEPROM:

```rust
pub fn read_mac_address() -> Result<[u8; 6], &'static str> {
    let mut mac = [0u8; 6];
    
    for i in 0..3 {
        let word = read_eeprom_word(i)?;
        mac[i * 2] = (word & 0xFF) as u8;
        mac[i * 2 + 1] = ((word >> 8) & 0xFF) as u8;
    }
    
    Ok(mac)
}
```

## Serial Driver

### Purpose

Debug console via COM1 (serial port 0).

### I/O Ports

```
Port    Purpose
─────────────────────
0x3F8   Data
0x3F9   Interrupt enable
0x3FA   Interrupt ID
0x3FB   Line control (setup)
0x3FC   Modem control
0x3FD   Line status
0x3FE   Modem status
```

### Initialization

```rust
pub fn init() -> Result<(), &'static str> {
    unsafe {
        let mut cmd: Port<u8> = Port::new(0x3FB);
        cmd.write(0x80);  // Enable DLAB
        
        // Set baud rate: 115200
        let mut data: Port<u8> = Port::new(0x3F8);
        data.write(1);  // Divisor low
        
        let mut ier: Port<u8> = Port::new(0x3F9);
        ier.write(0);   // Divisor high
        
        cmd.write(0x03); // 8 bits, 1 stop, no parity
        
        let mut fcr: Port<u8> = Port::new(0x3FA);
        fcr.write(0xC7); // FIFO control
        
        let mut mcr: Port<u8> = Port::new(0x3FC);
        mcr.write(0x0B); // Enable interrupts
    }
    Ok(())
}
```

### Writing Output

```rust
pub fn write_byte(byte: u8) {
    unsafe {
        loop {
            let lsr: Port<u8> = Port::new(0x3FD);
            if (lsr.read() & 0x20) != 0 {  // Is TX ready?
                let data: Port<u8> = Port::new(0x3F8);
                data.write(byte);
                return;
            }
        }
    }
}
```

## PIC Driver

### Purpose

Programmable Interrupt Controller manages hardware interrupts.

### Initialization

See [Interrupts Reference](interrupts.md) for details.

### Enable/Disable IRQs

```rust
pub fn enable_irq(irq: u8) {
    match irq {
        0..=7 => {
            // Master PIC
            unsafe {
                let current = MASTER_DATA.read();
                MASTER_DATA.write(current & !(1 << irq));
            }
        }
        8..=15 => {
            // Slave PIC
            unsafe {
                let current = SLAVE_DATA.read();
                SLAVE_DATA.write(current & !(1 << (irq - 8)));
            }
        }
        _ => { /* invalid */ }
    }
}
```

## RTC Driver

### Purpose

Real-Time Clock provides date/time.

### I/O Ports

```
Port    Purpose
──────────────────
0x70    Index
0x71    Data
```

### Reading Time

```rust
pub fn read_time() -> Time {
    unsafe {
        let seconds = read_cmos(0x00);
        let minutes = read_cmos(0x02);
        let hours = read_cmos(0x04);
        let day = read_cmos(0x07);
        let month = read_cmos(0x08);
        let year = read_cmos(0x09);
        
        Time { seconds, minutes, hours, day, month, year }
    }
}

unsafe fn read_cmos(index: u8) -> u8 {
    let mut idx_port: Port<u8> = Port::new(0x70);
    let mut data_port: Port<u8> = Port::new(0x71);
    
    idx_port.write(index);
    data_port.read()
}
```

## ATA Driver

### Purpose

Advanced Technology Attachment (IDE) disk access.

### Initialization

```rust
pub fn init() -> Result<(), &'static str> {
    // Detect connected drives
    probe_drives()?;
    
    // Setup interrupts
    setup_interrupts()?;
    
    Ok(())
}
```

### Reading Sectors

```rust
pub fn read_sector(lba: u32, buffer: &mut [u8]) -> Result<(), &'static str> {
    if buffer.len() < 512 {
        return Err("Buffer too small");
    }
    
    // 1. Send read command to drive
    issue_read_command(lba)?;
    
    // 2. Wait for data ready
    wait_for_data_ready()?;
    
    // 3. Read 512 bytes from data port
    for i in 0..256 {  // 512 bytes = 256 words
        let word = read_data_word()?;
        buffer[i * 2] = (word & 0xFF) as u8;
        buffer[i * 2 + 1] = ((word >> 8) & 0xFF) as u8;
    }
    
    Ok(())
}
```

## Driver Architecture Patterns

### Initialization Pattern

```rust
pub fn init() -> Result<(), &'static str> {
    // 1. Enable interrupts
    // 2. Reset hardware
    // 3. Configure registers
    // 4. Start operations
    Ok(())
}
```

### Interrupt Handler Pattern

```rust
extern "x86-interrupt" fn device_handler(_stack_frame: InterruptStackFrame) {
    // 1. Read device status
    // 2. Process data
    // 3. Clear interrupt
    // 4. Send EOI to PIC
}
```

### Read/Write Pattern

```rust
pub fn read_data() -> Result<Data, &'static str> {
    // 1. Check device status
    // 2. Initiate read
    // 3. Wait for completion
    // 4. Read data
    Ok(data)
}
```

## Performance Tips

### Interrupt Handlers

Keep handlers fast:
- ✅ Read device status
- ✅ Copy data to buffer
- ✅ Send EOI
- ❌ Don't allocate memory
- ❌ Don't print unless necessary

### Memory Access

```rust
// Good: Volatile ensures memory access
unsafe {
    let value = *(0xDEADBEEF as *const u32);  // Read from device
}

// Bad: Compiler might optimize away the read
let value = read_device_memory();
```

### I/O Ports

```rust
// Good: Direct port I/O
unsafe {
    let mut port: Port<u8> = Port::new(0x60);
    let data = port.read();
}

// Bad: Buffered/cached access
let data = get_cached_value();
```

## Next Steps

- **[Networking Reference](networking.md)** — Network stack
- **[Interrupts Reference](interrupts.md)** — Interrupt handling
- **[Architecture Overview](architecture.md)** — System design
