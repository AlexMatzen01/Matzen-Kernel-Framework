//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFK Runner - Creates bootable disk images and runs them in VirtualBox or QEMU

use std::path::Path;
use std::process::Command;

fn main() {
    // Get the kernel binary path from arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
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

    // Parse CLI options
    let hypervisor = if args.iter().any(|a| a == "--qemu") {
        "qemu"
    } else if args.iter().any(|a| a == "--vbox" || a == "--virtualbox") {
        "vbox"
    } else {
        "vbox" // Default to VirtualBox
    };

    let no_run = args.iter().any(|a| a == "--no-run");

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

    if !no_run {
        match hypervisor {
            "vbox" => run_virtualbox(&bios_path, kernel_path),
            "qemu" => run_qemu(&bios_path),
            _ => {
                eprintln!("Unknown hypervisor: {}", hypervisor);
                std::process::exit(1);
            }
        }
    }
}

fn print_usage(program: &str) {
    eprintln!("Usage: {} <kernel-binary-path> [OPTIONS]", program);
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --vbox, --virtualbox  Run in VirtualBox (default)");
    eprintln!("  --qemu                Run in QEMU");
    eprintln!("  --no-run              Only create disk images, don't run");
    eprintln!();
    eprintln!("Example:");
    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --vbox", program);
    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --qemu", program);
}

fn run_virtualbox(bios_path: &str, kernel_path: &Path) {
    // Create VDI disk from BIOS image
    let vdi_path = format!("{}.vdi", bios_path);
    
    if !Path::new(&vdi_path).exists() {
        println!("Converting BIOS image to VDI format...");
        let result = Command::new("VBoxManage")
            .args([
                "convertfromraw",
                bios_path,
                &vdi_path,
                "--format",
                "VDI",
                "--variant",
                "Standard",
            ])
            .output();
        
        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!("Warning: VBoxManage convertfromraw failed");
                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                    eprintln!("Make sure VirtualBox is installed and VBoxManage is in PATH");
                    std::process::exit(1);
                }
                println!("Created VDI disk: {}", vdi_path);
            }
            Err(e) => {
                eprintln!("Error: Failed to run VBoxManage: {}", e);
                eprintln!("Make sure VirtualBox is installed and VBoxManage is in PATH");
                std::process::exit(1);
            }
        }
    } else {
        println!("Using existing VDI disk: {}", vdi_path);
    }

    // Create data disk VDI if it doesn't exist
    let data_vdi_path = "target/disk.vdi";
    if !Path::new(data_vdi_path).exists() {
        println!("Creating data disk VDI: {}", data_vdi_path);
        let result = Command::new("VBoxManage")
            .args([
                "createmedium",
                "disk",
                "--filename",
                data_vdi_path,
                "--size",
                "10240", // 10 MB in MB
                "--format",
                "VDI",
                "--variant",
                "Standard",
            ])
            .output();
        
        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!("Warning: Failed to create data disk VDI");
                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to create data disk: {}", e);
            }
        }
    }

    // VM name based on kernel binary
    let vm_name = format!("MFK-{}", 
        kernel_path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kernel")
    );

    // Check if VM already exists
    let check_vm = Command::new("VBoxManage")
        .args(["list", "vms"])
        .output();

    let vm_exists = if let Ok(output) = check_vm {
        let vms_list = String::from_utf8_lossy(&output.stdout);
        vms_list.contains(&format!("\"{}\"", vm_name))
    } else {
        false
    };

    if !vm_exists {
        println!("Creating VirtualBox VM: {}", vm_name);
        
        // Create VM
        let result = Command::new("VBoxManage")
            .args([
                "createvm",
                "--name",
                &vm_name,
                "--ostype",
                "Linux_64",
                "--register",
            ])
            .output();

        if let Ok(output) = result {
            if !output.status.success() {
                eprintln!("Warning: Failed to create VM");
                eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
        }

        // Configure VM memory
        let _ = Command::new("VBoxManage")
            .args([
                "modifyvm",
                &vm_name,
                "--memory",
                "256",
                "--cpus",
                "2",
                "--rtcuseutc",
                "on",
                "--vram",
                "16",
            ])
            .output();

        // Create IDE controller
        let _ = Command::new("VBoxManage")
            .args([
                "storagectl",
                &vm_name,
                "--name",
                "IDE",
                "--add",
                "ide",
                "--controller",
                "PIIX4",
            ])
            .output();

        // Attach boot disk
        let _ = Command::new("VBoxManage")
            .args([
                "storageattach",
                &vm_name,
                "--storagectl",
                "IDE",
                "--port",
                "0",
                "--device",
                "0",
                "--type",
                "hdd",
                "--medium",
                &vdi_path,
            ])
            .output();

        // Attach data disk
        if Path::new(data_vdi_path).exists() {
            let _ = Command::new("VBoxManage")
                .args([
                    "storageattach",
                    &vm_name,
                    "--storagectl",
                    "IDE",
                    "--port",
                    "0",
                    "--device",
                    "1",
                    "--type",
                    "hdd",
                    "--medium",
                    data_vdi_path,
                ])
                .output();
        }

        // Configure network - Bridged for unrestricted networking
        let _ = Command::new("VBoxManage")
            .args([
                "modifyvm",
                &vm_name,
                "--nic1",
                "bridged",
            ])
            .output();

        // Detect and set host interface (try common names)
        let interfaces = ["eth0", "eth1", "wlan0", "wlan1", "en0", "en1"];
        for iface in &interfaces {
            let result = Command::new("VBoxManage")
                .args([
                    "modifyvm",
                    &vm_name,
                    "--bridgeadapter1",
                    iface,
                ])
                .output();
            
            if let Ok(output) = result {
                if output.status.success() {
                    println!("Using network interface: {}", iface);
                    break;
                }
            }
        }

        // Configure serial port for console output
        let _ = Command::new("VBoxManage")
            .args([
                "modifyvm",
                &vm_name,
                "--uart1",
                "0x3F8",
                "4",
                "--uartmode1",
                "file",
                "target/mfk-serial.log",
            ])
            .output();
    } else {
        println!("Using existing VM: {}", vm_name);
    }

    // Start the VM
    println!("Starting VirtualBox VM: {}", vm_name);
    println!("Networking: Bridged (unrestricted, full Layer 2 access)");
    println!("Serial console: target/mfk-serial.log");
    
    let result = Command::new("VBoxManage")
        .args([
            "startvm",
            &vm_name,
            "--type",
            "gui",
        ])
        .output();

    match result {
        Ok(output) => {
            if !output.status.success() {
                eprintln!("Error starting VM:");
                eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                std::process::exit(1);
            }
            println!("VM started successfully!");
            println!("Serial console output will be written to: target/mfk-serial.log");
        }
        Err(e) => {
            eprintln!("Error: Failed to start VM: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_qemu(bios_path: &str) {
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
    println!("Running in QEMU...");
    println!("Networking: user-mode NAT (unrestricted, limited ICMP support)");
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
            "user,id=net0,restrict=off,hostfwd=udp::5555-:5555,hostfwd=tcp::49152-:49152",
        ])
        .spawn()
        .expect("Failed to start QEMU");

    qemu.wait().expect("Failed to wait on QEMU");
}