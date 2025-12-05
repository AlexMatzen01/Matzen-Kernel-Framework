//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.


//! Programmable Interrupt Controller (PIC) Driver
//!
//! Manages the 8259 PIC chips to route hardware interrupts to the CPU.

use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Command port for master PIC
const PIC1_COMMAND: u16 = 0x20;
/// Data port for master PIC
const PIC1_DATA: u16 = 0x21;
/// Command port for slave PIC
const PIC2_COMMAND: u16 = 0xA0;
/// Data port for slave PIC
const PIC2_DATA: u16 = 0xA1;

/// End of Interrupt command
const PIC_EOI: u8 = 0x20;

/// PIC initialization command
const ICW1_INIT: u8 = 0x11;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;

lazy_static! {
    pub static ref PICS: Mutex<ChainedPics> = {
        Mutex::new(unsafe { ChainedPics::new(32, 40) })
    };
}

/// A pair of chained PICs
pub struct ChainedPics {
    master: Pic,
    slave: Pic,
}

impl ChainedPics {
    /// Creates a new ChainedPics with the given offsets
    ///
    /// # Safety
    /// The offsets must not overlap with CPU exceptions (0-31)
    pub const unsafe fn new(offset1: u8, offset2: u8) -> ChainedPics {
        ChainedPics {
            master: Pic::new(offset1, PIC1_COMMAND, PIC1_DATA),
            slave: Pic::new(offset2, PIC2_COMMAND, PIC2_DATA),
        }
    }

    /// Initializes both PICs
    pub unsafe fn initialize(&mut self) {
        let mut wait_port: Port<u8> = Port::new(0x80);
        let mut wait = || wait_port.write(0);

        // Save masks
        let mask1 = self.master.data.read();
        let mask2 = self.slave.data.read();

        // Start initialization sequence
        self.master.command.write(ICW1_INIT | ICW1_ICW4);
        wait();
        self.slave.command.write(ICW1_INIT | ICW1_ICW4);
        wait();

        // Set vector offsets
        self.master.data.write(self.master.offset);
        wait();
        self.slave.data.write(self.slave.offset);
        wait();

        // Configure chaining
        self.master.data.write(4); // Slave on IRQ2
        wait();
        self.slave.data.write(2); // Cascade identity
        wait();

        // Set mode
        self.master.data.write(ICW4_8086);
        wait();
        self.slave.data.write(ICW4_8086);
        wait();

        // Restore masks
        self.master.data.write(mask1);
        self.slave.data.write(mask2);
    }

    /// Notifies the PICs that an interrupt has been handled
    pub unsafe fn notify_end_of_interrupt(&mut self, interrupt_id: u8) {
        if interrupt_id >= self.slave.offset {
            self.slave.command.write(PIC_EOI);
        }
        self.master.command.write(PIC_EOI);
    }

    /// Disables both PICs
    pub unsafe fn disable(&mut self) {
        self.master.data.write(0xff);
        self.slave.data.write(0xff);
    }

    /// Sets the interrupt mask for a specific IRQ
    pub unsafe fn set_mask(&mut self, irq: u8, masked: bool) {
        let (pic, bit) = if irq < 8 {
            (&mut self.master, irq)
        } else {
            (&mut self.slave, irq - 8)
        };

        let mut mask = pic.data.read();
        if masked {
            mask |= 1 << bit;
        } else {
            mask &= !(1 << bit);
        }
        pic.data.write(mask);
    }
}

/// A single PIC chip
struct Pic {
    offset: u8,
    command: Port<u8>,
    data: Port<u8>,
}

impl Pic {
    const fn new(offset: u8, command_port: u16, data_port: u16) -> Pic {
        Pic {
            offset,
            command: Port::new(command_port),
            data: Port::new(data_port),
        }
    }
}
