//! Programmable Interrupt Controller (PIC) driver
//!
//! This module provides initialization and control for the 8259 PIC chips.

use spin::Mutex;
use x86_64::instructions::port::Port;

/// Command port for PIC 1 (master)
const PIC1_COMMAND: u16 = 0x20;
/// Data port for PIC 1 (master)
const PIC1_DATA: u16 = 0x21;
/// Command port for PIC 2 (slave)
const PIC2_COMMAND: u16 = 0xA0;
/// Data port for PIC 2 (slave)
const PIC2_DATA: u16 = 0xA1;

/// End of interrupt command
const PIC_EOI: u8 = 0x20;

/// ICW1 - Initialization control word 1
const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;

/// ICW4 - 8086 mode
const ICW4_8086: u8 = 0x01;

/// Offset for master PIC interrupts
pub const PIC1_OFFSET: u8 = 32;
/// Offset for slave PIC interrupts
pub const PIC2_OFFSET: u8 = 40;

/// Hardware interrupt numbers
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC1_OFFSET,
    Keyboard = PIC1_OFFSET + 1,
}

impl InterruptIndex {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

/// The PIC controller
pub struct Pics {
    pics: [Pic; 2],
}

struct Pic {
    offset: u8,
    command: Port<u8>,
    data: Port<u8>,
}

impl Pic {
    const fn new(offset: u8, command: u16, data: u16) -> Self {
        Self {
            offset,
            command: Port::new(command),
            data: Port::new(data),
        }
    }

    fn handles_interrupt(&self, interrupt_id: u8) -> bool {
        self.offset <= interrupt_id && interrupt_id < self.offset + 8
    }

    unsafe fn end_of_interrupt(&mut self) {
        self.command.write(PIC_EOI);
    }
}

impl Pics {
    pub const fn new() -> Self {
        Self {
            pics: [
                Pic::new(PIC1_OFFSET, PIC1_COMMAND, PIC1_DATA),
                Pic::new(PIC2_OFFSET, PIC2_COMMAND, PIC2_DATA),
            ],
        }
    }

    /// Initialize both PICs
    pub unsafe fn initialize(&mut self) {
        // Save masks
        let mask1 = self.pics[0].data.read();
        let mask2 = self.pics[1].data.read();

        // Start initialization sequence (ICW1)
        self.pics[0].command.write(ICW1_INIT | ICW1_ICW4);
        io_wait();
        self.pics[1].command.write(ICW1_INIT | ICW1_ICW4);
        io_wait();

        // ICW2: Set vector offsets
        self.pics[0].data.write(PIC1_OFFSET);
        io_wait();
        self.pics[1].data.write(PIC2_OFFSET);
        io_wait();

        // ICW3: Configure cascading
        self.pics[0].data.write(4); // Slave on IRQ2
        io_wait();
        self.pics[1].data.write(2); // Cascade identity
        io_wait();

        // ICW4: Set mode
        self.pics[0].data.write(ICW4_8086);
        io_wait();
        self.pics[1].data.write(ICW4_8086);
        io_wait();

        // Restore masks (but enable keyboard and timer)
        self.pics[0].data.write(mask1);
        self.pics[1].data.write(mask2);
    }

    /// Send end of interrupt signal
    pub unsafe fn notify_end_of_interrupt(&mut self, interrupt_id: u8) {
        if self.pics[1].handles_interrupt(interrupt_id) {
            self.pics[1].end_of_interrupt();
        }
        if self.pics[0].handles_interrupt(interrupt_id) {
            self.pics[0].end_of_interrupt();
        }
    }

    /// Enable a specific IRQ
    pub unsafe fn enable_irq(&mut self, irq: u8) {
        if irq < 8 {
            let mask = self.pics[0].data.read();
            self.pics[0].data.write(mask & !(1 << irq));
        } else {
            let mask = self.pics[1].data.read();
            self.pics[1].data.write(mask & !(1 << (irq - 8)));
        }
    }

    /// Disable all IRQs
    pub unsafe fn disable_all(&mut self) {
        self.pics[0].data.write(0xFF);
        self.pics[1].data.write(0xFF);
    }
}

/// Wait for I/O operation to complete
fn io_wait() {
    unsafe {
        // Write to an unused port for a small delay
        Port::<u8>::new(0x80).write(0);
    }
}

/// Global PIC instance
pub static PICS: Mutex<Pics> = Mutex::new(Pics::new());

/// Initialize the PICs
pub fn init() {
    unsafe {
        let mut pics = PICS.lock();
        pics.disable_all();
        pics.initialize();
        // Enable keyboard interrupt (IRQ1)
        pics.enable_irq(1);
    }
    log::info!("PIC initialized");
}

/// Send end of interrupt signal
pub fn end_of_interrupt(interrupt_id: u8) {
    unsafe {
        PICS.lock().notify_end_of_interrupt(interrupt_id);
    }
}
