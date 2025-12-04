//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.


//! Matzen Kernel Framework - A simple terminal OS
//!
//! This kernel provides a basic terminal interface that runs on bare metal x86_64.

#![no_std]
#![no_main]

mod drivers;
mod shell;

use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use bootloader_api::config::Mapping;
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all physical memory so we can access VGA buffer at 0xb8000
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Main entry point for the kernel
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Initialize serial port first for early debugging
    drivers::serial::init();
    serial_println!("Serial port initialized");
    
    // Get physical memory offset for VGA buffer access
    let phys_mem_offset = boot_info.physical_memory_offset.into_option()
        .expect("Physical memory offset not available");
    serial_println!("Physical memory offset: {:#x}", phys_mem_offset);
    
    // Initialize VGA text mode with proper memory mapping
    drivers::vga::init_with_offset(phys_mem_offset);
    serial_println!("VGA initialized");
    
    // Print welcome message
    println!("======================================");
    println!("  Matzen Kernel Framework v0.1.0");
    println!("  Terminal OS Ready!");
    println!("======================================");
    println!();
    println!("Type 'help' for available commands.");
    println!();
    
    serial_println!("Welcome message printed");
    
    // Initialize keyboard
    drivers::keyboard::init();
    serial_println!("Keyboard initialized");
    
    // Start the shell
    serial_println!("Starting shell...");
    shell::run();
}

/// Panic handler - prints error message and halts
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
