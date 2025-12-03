//! Matzen Kernel Framework - A simple terminal OS
//!
//! This kernel provides a basic terminal interface that runs on bare metal x86_64.

#![no_std]
#![no_main]

mod drivers;
mod shell;

use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let config = BootloaderConfig::new_default();
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Main entry point for the kernel
fn kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    // Initialize serial port first for early debugging
    drivers::serial::init();
    serial_println!("Serial port initialized");
    
    // Initialize VGA text mode
    drivers::vga::init();
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
    println!();
    println!("KERNEL PANIC!");
    println!("{}", info);
    loop {
        x86_64::instructions::hlt();
    }
}
