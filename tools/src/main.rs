//! MFK Runner - Creates bootable disk images and runs them in QEMU

use std::path::Path;
use std::process::Command;

fn main() {
    // Get the kernel binary path from arguments
    let args: Vec<String> = std::env::args().collect();
    
    if args.len() < 2 {
        eprintln!("Usage: {} <kernel-binary-path>", args[0]);
        eprintln!("Example: {} target/x86_64-unknown-none/debug/mfk-kernel", args[0]);
        std::process::exit(1);
    }
    
    let kernel_path = Path::new(&args[1]);
    
    // Create disk image paths
    let uefi_path = format!("{}-uefi.img", args[1]);
    let bios_path = format!("{}-bios.img", args[1]);
    
    // Create the disk images
    let uefi_builder = bootloader::UefiBoot::new(kernel_path);
    let bios_builder = bootloader::BiosBoot::new(kernel_path);
    
    match uefi_builder.create_disk_image(Path::new(&uefi_path)) {
        Ok(_) => println!("Created UEFI disk image: {}", uefi_path),
        Err(e) => {
            eprintln!("Failed to create UEFI disk image: {}", e);
        }
    }
    
    match bios_builder.create_disk_image(Path::new(&bios_path)) {
        Ok(_) => println!("Created BIOS disk image: {}", bios_path),
        Err(e) => {
            eprintln!("Failed to create BIOS disk image: {}", e);
            std::process::exit(1);
        }
    }
    
    // Check if --no-run flag is passed
    let no_run = args.iter().any(|a| a == "--no-run");
    
    if !no_run {
        // Run in QEMU
        println!("Running in QEMU...");
        let mut qemu = Command::new("qemu-system-x86_64")
            .args([
                "-drive", &format!("format=raw,file={}", bios_path),
                "-serial", "stdio",
                "-display", "none",
                "-no-reboot",
            ])
            .spawn()
            .expect("Failed to start QEMU");
        
        qemu.wait().expect("Failed to wait on QEMU");
    }
}
