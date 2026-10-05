//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Interrupt Handling
//!
//! This module sets up the Interrupt Descriptor Table (IDT) and handles
//! hardware and software interrupts.

use crate::serial_println;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // CPU Exception Handlers
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.double_fault.set_handler_fn(double_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);

        // Hardware Interrupt Handlers (PIC IRQs)
        idt[InterruptIndex::Timer.as_u8()]
            .set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()]
            .set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Com1.as_u8()]
            .set_handler_fn(serial_interrupt_handler);
        idt[InterruptIndex::Mouse.as_u8()]
            .set_handler_fn(mouse_interrupt_handler);

        // The E1000 takes whichever of IRQ10/IRQ11 the firmware gave it (see
        // `NIC_IRQ_LINES`). Both vectors call into the driver; the one the card
        // is not on stays masked, so it never fires.
        idt[InterruptIndex::E1000A.as_u8()].set_handler_fn(e1000_interrupt_handler);
        idt[InterruptIndex::E1000B.as_u8()].set_handler_fn(e1000_interrupt_handler);

        // Remaining PIC IRQs: acknowledge only, so a masked or unused line
        // cannot escalate into a fault.
        idt[34].set_handler_fn(irq2_handler);
        idt[35].set_handler_fn(irq3_handler);
        idt[37].set_handler_fn(irq5_handler);
        idt[38].set_handler_fn(irq6_handler);
        idt[39].set_handler_fn(irq7_handler);
        idt[40].set_handler_fn(irq8_handler);
        idt[41].set_handler_fn(irq9_handler);
        idt[45].set_handler_fn(irq13_handler);
        idt[46].set_handler_fn(irq14_handler);
        idt[47].set_handler_fn(irq15_handler);

        idt
    };
}

/// Initializes the IDT
pub fn init_idt() {
    IDT.load();
}

/// First PIC interrupt vector. Matches the remap in `pic::PICS`.
pub const PIC_BASE: u8 = 32;

// PIC IRQ lines owned by the core devices and the E1000.
pub const IRQ_TIMER: u8 = 0;
pub const IRQ_KEYBOARD: u8 = 1;
pub const IRQ_COM1: u8 = 4;
pub const IRQ_MOUSE: u8 = 12;

/// PIC lines the E1000 may be assigned, with a handler installed on each.
///
/// QEMU's legacy `pc` machine places the e1000 at device 3, which lands on
/// IRQ11 here; other machines and firmware configurations put it on IRQ10. Both
/// vectors are wired to the driver, and the driver unmasks only the line it was
/// actually given, so the unused one stays quiet.
pub const NIC_IRQ_LINES: [u8; 2] = [10, 11];

/// Whether `irq` has a handler installed for the E1000.
pub fn is_nic_irq(irq: u8) -> bool {
    NIC_IRQ_LINES.contains(&irq)
}

/// Hardware interrupt indices (mapped by PIC)
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = 32,     // PIC1 base + 0 (IRQ0)
    Keyboard = 33,  // PIC1 base + 1 (IRQ1)
    Com1 = 36,      // PIC1 base + 4 (IRQ4)
    E1000A = 42,    // PIC1 base + 10 (IRQ10)
    E1000B = 43,    // PIC1 base + 11 (IRQ11)
    Mouse = 44,     // PIC1 base + 12 (IRQ12)
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

// Exception Handlers

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
    panic!("EXCEPTION: INVALID OPCODE");
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: GENERAL PROTECTION FAULT (code {:#x})\n{:#?}",
        error_code,
        stack_frame,
    );
    panic!("EXCEPTION: GENERAL PROTECTION FAULT");
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: SEGMENT NOT PRESENT (code {:#x})\n{:#?}",
        error_code,
        stack_frame,
    );
    panic!("EXCEPTION: SEGMENT NOT PRESENT");
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: STACK SEGMENT FAULT (code {:#x})\n{:#?}",
        error_code,
        stack_frame,
    );
    panic!("EXCEPTION: STACK SEGMENT FAULT");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    crate::println!();
    crate::println!("EXCEPTION: PAGE FAULT");
    crate::println!("Accessed Address: {:?}", Cr2::read());
    crate::println!("Error Code: {:?}", error_code);
    crate::println!("{:#?}", stack_frame);
    serial_println!("EXCEPTION: PAGE FAULT");
    serial_println!("Accessed Address: {:?}", Cr2::read());
    serial_println!("Error Code: {:?}", error_code);
    serial_println!("{:#?}", stack_frame);
    panic!("Page fault");
}

// Hardware Interrupt Handlers

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::time::tick();
    crate::shell::timer_tick();
    unsafe {
        super::pic::PICS
            .lock()
            .notify_end_of_interrupt(PIC_BASE + IRQ_TIMER);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Read scancode from keyboard
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    // Pass to keyboard driver
    crate::drivers::keyboard::handle_interrupt(scancode);

    // Send End of Interrupt to PIC
    unsafe {
        super::pic::PICS
            .lock()
            .notify_end_of_interrupt(PIC_BASE + IRQ_KEYBOARD);
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Drain all pending i8042 output bytes. Bit 5 of the status register
    // marks aux (mouse) data; anything else is routed to the keyboard
    // driver so shared-controller bytes are never lost.
    for _ in 0..8 {
        let mut status = Port::<u8>::new(0x64);
        let st: u8 = unsafe { status.read() };
        if st & 0x01 == 0 {
            break;
        }
        let mut data = Port::<u8>::new(0x60);
        let byte: u8 = unsafe { data.read() };
        if st & 0x20 != 0 {
            crate::drivers::mouse::handle_byte(byte);
        } else {
            crate::drivers::keyboard::handle_interrupt(byte);
        }
    }

    unsafe {
        super::pic::PICS
            .lock()
            .notify_end_of_interrupt(PIC_BASE + IRQ_MOUSE);
    }
}

extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Handle serial port interrupt - read data from COM1
    crate::drivers::serial::handle_interrupt();

    // Send End of Interrupt to PIC
    unsafe {
        super::pic::PICS
            .lock()
            .notify_end_of_interrupt(PIC_BASE + IRQ_COM1);
    }
}

extern "x86-interrupt" fn e1000_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::drivers::e1000::handle_interrupt();
}

// Unhandled or spurious IRQs (PIC remapped to 32-47). These simply acknowledge
// the interrupt so that masked or unused lines don't escalate into faults.
macro_rules! unused_irq_handler {
    ($name:ident, $irq_number:expr) => {
        extern "x86-interrupt" fn $name(_stack_frame: InterruptStackFrame) {
            unsafe {
                super::pic::PICS.lock().notify_end_of_interrupt($irq_number);
            }
        }
    };
}

unused_irq_handler!(irq2_handler, 34);
unused_irq_handler!(irq3_handler, 35);
unused_irq_handler!(irq5_handler, 37);
unused_irq_handler!(irq6_handler, 38);
unused_irq_handler!(irq7_handler, 39);
unused_irq_handler!(irq8_handler, 40);
unused_irq_handler!(irq9_handler, 41);
unused_irq_handler!(irq11_handler, 43);
unused_irq_handler!(irq13_handler, 45);
unused_irq_handler!(irq14_handler, 46);
unused_irq_handler!(irq15_handler, 47);
