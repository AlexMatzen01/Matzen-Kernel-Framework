//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFK Runner - Creates bootable disk images and runs them in QEMU

use std::path::Path;
use std::process::Command;

fn main() {
    // Get the kernel binary path from arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <kernel-binary-path>", args[0]);
        eprintln!("Example: {} target/x86_64-mfk/debug/mfk-kernel", args[0]);
        std::process::exit(1);
    }

    let kernel_path = Path::new(&args[1]);

    // Check if kernel binary exists
    if !kernel_path.exists() {
        eprintln!("Error: Kernel binary not found at: {}", args[1]);
        eprintln!();
        eprintln!("Make sure you've built the kernel first:");
        eprintln!("  cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem");
        eprintln!();
        eprintln!("Then run with the correct path:");
        eprintln!("  cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel");
        std::process::exit(1);
    }

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
        // Create a virtual disk image for testing (10MB) - only if it doesn't exist
        let disk_path = "target/disk.img";
        if !Path::new(disk_path).exists() {
            println!("Creating virtual disk image: {}", disk_path);
            let result = Command::new("qemu-img")
                .args(["create", "-f", "raw", disk_path, "10M"])
                .output();
            if let Err(e) = result {
                eprintln!("Warning: Failed to create disk image: {}", e);
            }
        } else {
            println!("Using existing disk image: {}", disk_path);
        }

        // Run in QEMU
        // Boot disk on IDE0 master (hda), data disk on IDE0 slave (hdb)
        println!("Running in QEMU...");
        let mut qemu = Command::new("qemu-system-x86_64")
            .args([
                "-drive",
                &format!("file={},format=raw,if=ide,index=0,media=disk", bios_path),
                "-drive",
                &format!("file={},format=raw,if=ide,index=1,media=disk,cache=none,readonly=off", disk_path),
                "-serial",
                "stdio",
                "-display",
                "none",
                "-no-reboot",
                "-no-shutdown",
                "-m",
                "128M",
                // Add E1000 network card
                "-device",
                "e1000,netdev=net0",
                "-netdev",
                "user,id=net0,hostfwd=udp::5555-:5555",
            ])
            .spawn()
            .expect("Failed to start QEMU");

        qemu.wait().expect("Failed to wait on QEMU");
    }
}