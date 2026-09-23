//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Matzen Kernel Framework - A simple terminal OS
//!
//! This kernel provides a basic terminal interface that runs on bare metal x86_64.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod allocator;
mod app;
mod desktop;
mod drivers;
mod editor;
mod fs;
mod install;
mod interrupts;
mod memory;
mod net;
mod pic;
mod shell;
mod time;

use bootloader_api::config::Mapping;
use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all physical memory so we can access VGA buffer at 0xb8000
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

#[cfg(not(test))]
entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Main entry point for the kernel
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Initialize serial port first for early debugging
    drivers::serial::init();
    serial_println!("Serial port initialized");

    // Get physical memory offset for VGA buffer access
    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("Physical memory offset not available");
    serial_println!("Physical memory offset: {:#x}", phys_mem_offset);

    // Initialize VGA text mode with proper memory mapping
    drivers::vga::init_with_offset(phys_mem_offset);
    serial_println!("VGA initialized");

    // Initialize heap allocator (required before fb init: fb uses alloc)
    allocator::init();
    serial_println!("Heap allocator initialized");

    // Frame allocator from the bootloader memory map. Powers double
    // buffering and any future page-table work. Heap (above) is a static
    // array, so there is no conflict with these regions.
    memory::frame_allocator::init_from_memory_map(&boot_info.memory_regions);
    serial_println!("Frame allocator initialized");

    // Attach UEFI GOP framebuffer when present so `println!` reaches the
    // display. On BIOS there is no framebuffer and VGA text mode is used.
    // Must run after allocator init; take() leaves `Optional::None` behind.
    if let Some(fb) = boot_info.framebuffer.take() {
        drivers::fb::init(fb);
        serial_println!("Framebuffer console initialized");
        // The legacy frame-allocator double buffer remains disabled because
        // its scattered pages aren't contiguous. The desktop draws directly
        // to GOP with damage clipping.
        serial_println!("Legacy physical-frame double buffer disabled");
    } else {
        serial_println!("No framebuffer (VGA text mode)");
    }

    // Print welcome message
    println!("======================================");
    println!("  Matzen Kernel Framework v0.1.0");
    println!("  Terminal OS Ready!");
    println!("======================================");
    println!();
    println!("Type 'help' for available commands.");
    println!();

    serial_println!("Welcome message printed");

    // Initialize interrupt handling
    interrupts::init_idt();
    serial_println!("IDT initialized");

    // Start the monotonic kernel clock and 100 Hz PIT before enabling IRQ0.
    time::init();
    drivers::pit::init_default();

    // Initialize and configure PIC
    unsafe {
        pic::PICS.lock().initialize();
        // Unmask the PIT timer for monotonic clock and bounded I/O timeouts.
        pic::PICS.lock().set_mask(0, false);
        // Unmask keyboard interrupt (IRQ1)
        pic::PICS.lock().set_mask(1, false);
        // Unmask cascade (IRQ2) + PS/2 mouse (IRQ12, slave)
        pic::PICS.lock().set_mask(2, false);
        pic::PICS.lock().set_mask(12, false);
        // Unmask COM1 serial interrupt (IRQ4)
        pic::PICS.lock().set_mask(4, false);
    }
    serial_println!("PIC initialized and configured");

    // Initialize keyboard driver (PS/2 + serial)
    drivers::keyboard::init();
    serial_println!("Keyboard initialized");

    // Initialize PS/2 mouse (fail-open: logs and continues when absent).
    drivers::mouse::init();
    if drivers::mouse::is_present() {
        serial_println!("Mouse initialized (PS/2, IRQ12)");
    } else {
        serial_println!("Mouse absent (desktop cursor will not move)");
    }

    // Initialize USB (EHCI/xHCI) for USB keyboards, mice and tablets,
    // including QEMU `qemu-xhci + usb-kbd + usb-tablet`. Bounded init:
    // never hangs when absent.
    #[cfg(feature = "usb")]
    {
        drivers::usb::init(phys_mem_offset);
        serial_println!(
            "USB initialized ({} HID keyboard(s), {} HID pointer(s))",
            drivers::usb::hid_keyboard_count(),
            drivers::usb::hid_pointer_count()
        );
    }
    #[cfg(not(feature = "usb"))]
    {
        serial_println!("USB support not compiled in");
    }

    // Initialize ATA disk driver
    drivers::ata::init();

    // Initialize networking (E1000 NIC)
    if let Err(e) = drivers::e1000::init(phys_mem_offset) {
        serial_println!("Warning: E1000 initialization failed: {}", e);
    } else {
        net::init();
    }

    // Enable interrupts
    x86_64::instructions::interrupts::enable();
    serial_println!("Interrupts enabled");

    // Start the shell
    serial_println!("Starting shell...");
    shell::run();
}

/// Panic handler - prints error message and halts
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Try to print to serial first (always works)
    serial_println!();
    serial_println!("KERNEL PANIC!");
    serial_println!("{}", info);

    // Also try VGA (may not work if panic is early)
    println!();
    println!("KERNEL PANIC!");
    println!("{}", info);

    loop {
        x86_64::instructions::hlt();
    }
}
