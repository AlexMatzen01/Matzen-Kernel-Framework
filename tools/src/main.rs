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
        eprintln!("  ./build.sh");
        eprintln!("  # or: cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem");
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
    let force = args.iter().any(|a| a == "--force");

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
            "vbox" => run_virtualbox(&bios_path, kernel_path, force),
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
    eprintln!("  --force               Force rebuild VDI/VM (fixes stale kernel)");
    eprintln!();
    eprintln!("Example:");
    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --vbox", program);
    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --qemu", program);
    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --force  # purge stale VDI", program);
}

fn run_virtualbox(bios_path: &str, kernel_path: &Path, force: bool) {
    // Create VDI disk from BIOS image - always regenerate to avoid stale kernel boot
    let vdi_path = format!("{}.vdi", bios_path);
    
    // If --force or BIOS newer than VDI, delete stale VDI
    let needs_convert = if force {
        if Path::new(&vdi_path).exists() {
            println!("--force: removing stale VDI {}", vdi_path);
            let _ = Command::new("VBoxManage")
                .args(["closemedium", "disk", &vdi_path, "--delete"])
                .output();
            let _ = std::fs::remove_file(&vdi_path);
        }
        true
    } else if Path::new(&vdi_path).exists() {
        // Check timestamps: if BIOS newer than VDI, regenerate
        let bios_mtime = std::fs::metadata(bios_path).and_then(|m| m.modified()).ok();
        let vdi_mtime = std::fs::metadata(&vdi_path).and_then(|m| m.modified()).ok();
        match (bios_mtime, vdi_mtime) {
            (Some(b), Some(v)) if b > v => {
                println!("BIOS image newer than VDI - regenerating...");
                let _ = Command::new("VBoxManage")
                    .args(["closemedium", "disk", &vdi_path, "--delete"])
                    .output();
                let _ = std::fs::remove_file(&vdi_path);
                true
            }
            _ => false,
        }
    } else {
        true
    };

    if needs_convert {
        println!("Converting BIOS image to VDI format...");
        // Ensure any old medium registration is closed
        if Path::new(&vdi_path).exists() {
            let _ = std::fs::remove_file(&vdi_path);
        }
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

    // Helper to run VBoxManage with error checking
    let run_vbox = |args: &[&str], desc: &str| -> bool {
        let result = Command::new("VBoxManage").args(args).output();
        match result {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                eprintln!("Warning: {} failed: {}", desc, String::from_utf8_lossy(&output.stderr).trim());
                false
            }
            Err(e) => {
                eprintln!("Warning: {} failed to execute VBoxManage: {}", desc, e);
                false
            }
        }
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
                eprintln!("Error: Failed to create VM: {}", String::from_utf8_lossy(&output.stderr));
                std::process::exit(1);
            }
        } else if let Err(e) = result {
            eprintln!("Error: Failed to run VBoxManage createvm: {}", e);
            std::process::exit(1);
        }

        // Configure VM memory - fail loudly if can't configure
        if !run_vbox(&["modifyvm", &vm_name, "--memory", "256", "--cpus", "2", "--rtcuseutc", "on", "--vram", "16"], "modifyvm memory") {
            eprintln!("VM created but memory config failed - VM may not boot correctly");
        }

        // Create IDE controller
        if !run_vbox(&["storagectl", &vm_name, "--name", "IDE", "--add", "ide", "--controller", "PIIX4"], "create IDE controller") {
            eprintln!("Error: Failed to create IDE controller");
            std::process::exit(1);
        }

        // Attach boot disk
        if !run_vbox(&["storageattach", &vm_name, "--storagectl", "IDE", "--port", "0", "--device", "0", "--type", "hdd", "--medium", &vdi_path], "attach boot disk") {
            eprintln!("Error: Failed to attach boot disk");
            std::process::exit(1);
        }

        // Attach data disk
        if Path::new(data_vdi_path).exists() {
            run_vbox(&["storageattach", &vm_name, "--storagectl", "IDE", "--port", "0", "--device", "1", "--type", "hdd", "--medium", data_vdi_path], "attach data disk");
        }

        // Configure network - Bridged for unrestricted networking
        run_vbox(&["modifyvm", &vm_name, "--nic1", "bridged", "--nictype1", "82540EM"], "configure bridged NIC");

        // Detect and set host interface - try dynamic detection first (Debian: enp*, ens*, wlp*, etc.)
        let mut bridged_ok = false;
        // First, query VBoxManage list bridgedifs for available interfaces (handles Debian predictable names)
        if let Ok(output) = Command::new("VBoxManage").args(["list", "bridgedifs"]).output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let mut discovered: Vec<String> = Vec::new();
                for line in text.lines() {
                    if let Some(name) = line.strip_prefix("Name:") {
                        let iface = name.trim().to_string();
                        if !iface.is_empty() {
                            discovered.push(iface);
                        }
                    }
                }
                // Prefer Up interfaces first (check Status: Up)
                // For simplicity, try all discovered in order
                for iface in &discovered {
                    if run_vbox(&["modifyvm", &vm_name, "--bridgeadapter1", iface], &format!("set bridgeadapter1 to {}", iface)) {
                        println!("Using network interface: {} (auto-detected)", iface);
                        bridged_ok = true;
                        break;
                    }
                }
                if !bridged_ok && !discovered.is_empty() {
                    eprintln!("Warning: Failed to set any auto-detected bridged interface: {:?}", discovered);
                }
            }
        }
        // Fallback to legacy hard-coded list if auto-detection failed
        if !bridged_ok {
            let fallback = ["enp0s3", "enp0s8", "ens33", "enp1s0", "eth0", "eth1", "wlan0", "wlp2s0", "en0", "en1"];
            for iface in &fallback {
                if run_vbox(&["modifyvm", &vm_name, "--bridgeadapter1", iface], &format!("set bridgeadapter1 to {}", iface)) {
                    println!("Using network interface: {} (fallback)", iface);
                    bridged_ok = true;
                    break;
                }
            }
        }
        if !bridged_ok {
            eprintln!("Warning: Could not configure bridged adapter - VM will have no network.");
            eprintln!("  Fix manually: VBoxManage modifyvm {} --bridgeadapter1 \"<your-ifname>\"", vm_name);
            eprintln!("  List adapters: VBoxManage list bridgedifs");
            eprintln!("  Or use: ./run.sh --qemu");
        }

        // Boot order + firmware
        run_vbox(&["modifyvm", &vm_name, "--boot1", "disk", "--boot2", "none", "--firmware", "bios"], "set boot order");

        // Configure serial port for console output - use absolute path so file is always at workspace target/mfk-serial.log
        let serial_log = std::env::current_dir()
            .map(|p| p.join("target/mfk-serial.log"))
            .unwrap_or_else(|_| Path::new("target/mfk-serial.log").to_path_buf());
        let serial_str = serial_log.to_string_lossy().to_string();
        run_vbox(&["modifyvm", &vm_name, "--uart1", "0x3F8", "4", "--uartmode1", "file", &serial_str], "configure serial port");
        println!("Serial log: {}", serial_str);
    } else {
        println!("Using existing VM: {}", vm_name);
        // On reuse, ensure boot disk is fresh VDI (fixes stale kernel boot)
        if needs_convert {
            println!("Updating VM boot disk to new VDI...");
            // Detach old then attach new
            let _ = Command::new("VBoxManage")
                .args(["storageattach", &vm_name, "--storagectl", "IDE", "--port", "0", "--device", "0", "--medium", "none"])
                .output();
            if !run_vbox(&["storageattach", &vm_name, "--storagectl", "IDE", "--port", "0", "--device", "0", "--type", "hdd", "--medium", &vdi_path], "re-attach boot disk") {
                eprintln!("Warning: Failed to update boot disk in existing VM. Try:");
                eprintln!("  VBoxManage unregistervm {} --delete && ./run.sh", vm_name);
            } else {
                println!("✓ Boot disk updated");
            }
        }
        // Also refresh data disk if missing
        if Path::new(data_vdi_path).exists() {
            // Check if data disk already attached
            if let Ok(info) = Command::new("VBoxManage").args(["showvminfo", &vm_name, "--machinereadable"]).output() {
                let info_str = String::from_utf8_lossy(&info.stdout);
                if !info_str.contains("disk.vdi") && !info_str.contains(data_vdi_path) {
                    run_vbox(&["storageattach", &vm_name, "--storagectl", "IDE", "--port", "0", "--device", "1", "--type", "hdd", "--medium", data_vdi_path], "re-attach data disk");
                }
            }
        }
    }

    // Start the VM
    let serial_abs = std::env::current_dir()
        .map(|p| p.join("target/mfk-serial.log"))
        .unwrap_or_else(|_| Path::new("target/mfk-serial.log").to_path_buf());
    println!("Starting VirtualBox VM: {}", vm_name);
    println!("Networking: Bridged (unrestricted, full Layer 2 access)");
    println!("Serial console: {}", serial_abs.display());
    
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
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("Error starting VM: {}", stderr.trim());
                if stderr.contains("is already running") || stderr.contains("is running") {
                    eprintln!("VM appears already running. Access via VirtualBox GUI or:");
                    eprintln!("  VBoxManage controlvm {} poweroff && ./run.sh", vm_name);
                } else if stderr.contains("Host network interface") || stderr.contains("bridged") {
                    eprintln!("Bridged network failed. Try:");
                    eprintln!("  VBoxManage list bridgedifs");
                    eprintln!("  VBoxManage modifyvm {} --bridgeadapter1 \"<ifname>\"", vm_name);
                    eprintln!("  # Or use QEMU: ./run.sh --qemu");
                }
                std::process::exit(1);
            }
            println!("VM started successfully!");
            println!("Serial console output will be written to: {}", serial_abs.display());
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