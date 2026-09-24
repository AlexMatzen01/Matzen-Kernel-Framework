//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFK Runner - Creates bootable disk images and runs them in VirtualBox or QEMU

use std::path::{Path, PathBuf};
use std::process::Command;

mod simplfs_host;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Firmware {
    Bios,
    Uefi,
}

impl Firmware {
    fn name(self) -> &'static str {
        match self {
            Firmware::Bios => "bios",
            Firmware::Uefi => "uefi",
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let kernel_path = Path::new(&args[1]);

    if !kernel_path.exists() {
        eprintln!("Error: Kernel binary not found at: {}", args[1]);
        eprintln!();
        eprintln!("Make sure you've built the kernel first:");
        eprintln!("  ./build.sh");
        eprintln!("  # or:");
        eprintln!("  cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem");
        eprintln!();
        eprintln!("Then run with the correct path:");
        eprintln!("  cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel");
        std::process::exit(1);
    }

    // ------------------------------------------------------------
    // Parse CLI options
    // ------------------------------------------------------------

    let hypervisor = if args.iter().any(|a| a == "--qemu") {
        "qemu"
    } else if args.iter().any(|a| a == "--vbox" || a == "--virtualbox") {
        "vbox"
    } else {
        "vbox"
    };

    let firmware = if args.iter().any(|a| a == "--uefi") {
        Firmware::Uefi
    } else {
        Firmware::Bios
    };

    let no_run = args.iter().any(|a| a == "--no-run");
    let force = args.iter().any(|a| a == "--force");
    let bundle = args.iter().any(|a| {
        a == "--bundle-apps" || a == "--with-apps"
    });

    // Keyboard transport: --kbd=<ps2|xhci|ehci|uhci> (from mfk_launch.py)
    // or legacy --xhci-kbd. xhci attaches qemu-xhci + usb-kbd; the emulated
    // usb-kbd overrides PS/2 so guest input flows through the xHCI driver.
    // Serial stdio stays available as a backdoor.
    let kbd_arg = args.iter().find_map(|a| {
        a.strip_prefix("--kbd=")
            .map(|v| v.to_ascii_lowercase())
    });
    let xhci_kbd = args.iter().any(|a| a == "--xhci-kbd")
        || kbd_arg.as_deref() == Some("xhci");

    // Extra drives from mfk_launch.py: --data-disk-size=10M plus repeatable
    // --extra-disk=<path> --extra-disk-size=<size> pairs in order, with an
    // optional --boot-extra-disk[=N] (1-based among extras, bare = first).
    // The first 2 extras use legacy IDE slots (index 2/secondary-master and
    // index 3/secondary-slave); extras beyond that attach as virtio-blk-pci
    // devices and appear in the guest as unified drive indices 4+ (needs the
    // kernel virtio-blk driver, compiled in with the default `usb` feature).
    const MAX_EXTRA_IDE: usize = 2;
    const MAX_EXTRAS: usize = 8;
    let data_disk_size: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--data-disk-size="))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "10M".to_string());
    let mut extra_paths: Vec<String> = Vec::new();
    let mut extra_sizes: Vec<String> = Vec::new();
    for a in args.iter() {
        if let Some(p) = a.strip_prefix("--extra-disk=") {
            extra_paths.push(p.to_string());
        } else if let Some(s) = a.strip_prefix("--extra-disk-size=") {
            extra_sizes.push(s.to_string());
        }
    }
    let mut extra_disks: Vec<(String, String)> = extra_paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let s = extra_sizes
                .get(i)
                .cloned()
                .unwrap_or_else(|| "64M".to_string());
            (p, s)
        })
        .collect();
    if extra_disks.len() > MAX_EXTRAS {
        eprintln!(
            "Warning: {} extra disks given, max {} (2 IDE + 6 virtio); ignoring the rest.",
            extra_disks.len(),
            MAX_EXTRAS
        );
        extra_disks.truncate(MAX_EXTRAS);
    }
    let boot_extra: Option<usize> = args.iter().find_map(|a| {
        if a == "--boot-extra-disk" {
            Some(0)
        } else if let Some(n) = a.strip_prefix("--boot-extra-disk=") {
            n.parse::<usize>().ok().map(|v| v.saturating_sub(1))
        } else {
            None
        }
    });

    // VNC / Web UI
    let vnc_port: u16 = args.iter().find_map(|a| a.strip_prefix("--vnc-port="))
        .and_then(|s| s.parse().ok())
        .unwrap_or(5900);
    let web_ui = args.iter().any(|a| a == "--web-ui");
    let vnc = web_ui || args.iter().any(|a| a == "--vnc");
    let web_ui_port: u16 = args.iter().find_map(|a| a.strip_prefix("--web-ui-port="))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8084);

    // ------------------------------------------------------------
    // Create disk image paths
    // ------------------------------------------------------------

    let uefi_path = format!("{}-uefi.img", args[1]);
    let bios_path = format!("{}-bios.img", args[1]);

    // ------------------------------------------------------------
    // Create both boot images
    //
    // We intentionally create both even when only one is being
    // tested. This preserves the existing behavior of the runner.
    // ------------------------------------------------------------

    println!("Creating boot images...");

    let uefi_builder = bootloader::UefiBoot::new(kernel_path);
    let bios_builder = bootloader::BiosBoot::new(kernel_path);

    match uefi_builder.create_disk_image(Path::new(&uefi_path)) {
        Ok(_) => println!("Created UEFI disk image: {}", uefi_path),
        Err(e) => {
            eprintln!("Failed to create UEFI disk image: {}", e);
            std::process::exit(1);
        }
    }

    match bios_builder.create_disk_image(Path::new(&bios_path)) {
        Ok(_) => println!("Created BIOS disk image: {}", bios_path),
        Err(e) => {
            eprintln!("Failed to create BIOS disk image: {}", e);
            std::process::exit(1);
        }
    }

    // ------------------------------------------------------------
    // Bundle example apps into QEMU raw data disk if requested
    // ------------------------------------------------------------

    if bundle {
        let disk_raw = Path::new("target/disk.img");
        let examples = Path::new("apps/examples");

        if disk_raw.exists() && examples.exists() {
            println!(
                "Bundling example apps from {} into {}...",
                examples.display(),
                disk_raw.display()
            );

            if let Err(e) =
                simplfs_host::bundle_examples(disk_raw, examples)
            {
                eprintln!("Bundle failed: {}", e);
            }
        } else if !disk_raw.exists() {
            eprintln!(
                "Bundle: target/disk.img not found \
                 (run will create it, retry with --bundle-apps after first run)"
            );
        }
    }

    // ------------------------------------------------------------
    // Select the actual boot image
    // ------------------------------------------------------------

    let boot_image = match firmware {
        Firmware::Bios => &bios_path,
        Firmware::Uefi => &uefi_path,
    };

    println!();
    println!("MFK Runner configuration:");
    println!("  Kernel:     {}", kernel_path.display());
    println!("  Hypervisor: {}", hypervisor);
    println!("  Firmware:   {}", firmware.name());
    println!("  Boot image: {}", boot_image);
    if xhci_kbd {
        println!("  Input:      xHCI USB keyboard + absolute tablet");
    }
    println!("  Data disk:  target/disk.img ({})", data_disk_size);
    for (i, (p, s)) in extra_disks.iter().enumerate() {
        let bus = if i < MAX_EXTRA_IDE { "IDE" } else { "virtio" };
        println!(
            "  Extra #{}:   {} ({}) [{}]{}",
            i + 1,
            p,
            s,
            bus,
            if boot_extra == Some(i) { " [boot]" } else { "" }
        );
    }
    if vnc || web_ui {
        println!(
            "  VNC:        enabled (TCP port {}, WebSocket port {})",
            qemu_vnc_tcp_port(vnc_port),
            vnc_port
        );
    }
    if web_ui {
        println!("  Web UI:     enabled (port {})", web_ui_port);
    }
    println!();

    if no_run {
        println!("--no-run specified; images created but VM will not start.");
        return;
    }

    // ------------------------------------------------------------
    // Automatically use QEMU if VirtualBox isn't installed
    // ------------------------------------------------------------

    let mut selected_hypervisor = hypervisor;

    if selected_hypervisor == "vbox"
        && Command::new("VBoxManage")
            .arg("--version")
            .output()
            .is_err()
    {
        if Command::new("qemu-system-x86_64")
            .arg("--version")
            .output()
            .is_ok()
        {
            println!("ℹ VirtualBox not found, using QEMU.");
            selected_hypervisor = "qemu";
        }
    }

    // ------------------------------------------------------------
    // Validate selected hypervisor
    // ------------------------------------------------------------

    if selected_hypervisor == "qemu" {
        if !command_exists("qemu-system-x86_64") {
            eprintln!("ERROR: qemu-system-x86_64 not found.");
            eprintln!(
                "Install: sudo apt-get install qemu-system-x86 qemu-utils"
            );
            std::process::exit(1);
        }
    } else if !command_exists("VBoxManage") {
        eprintln!("ERROR: VBoxManage not found.");
        std::process::exit(1);
    }

    // ------------------------------------------------------------
    // Run selected environment
    // ------------------------------------------------------------

    match selected_hypervisor {
        "qemu" => match firmware {
            Firmware::Bios => run_qemu_bios(
                &bios_path,
                bundle,
                xhci_kbd,
                &data_disk_size,
                &extra_disks,
                vnc,
                vnc_port,
                web_ui,
                web_ui_port,
            ),
            Firmware::Uefi => run_qemu_uefi(
                &uefi_path,
                bundle,
                xhci_kbd,
                &data_disk_size,
                &extra_disks,
                vnc,
                vnc_port,
                web_ui,
                web_ui_port,
            ),
        },

        "vbox" => match firmware {
            Firmware::Bios => {
                run_virtualbox(&bios_path, kernel_path, Firmware::Bios, force)
            }

            Firmware::Uefi => {
                run_virtualbox(&uefi_path, kernel_path, Firmware::Uefi, force)
            }
        },

        _ => {
            eprintln!("Unknown hypervisor: {}", selected_hypervisor);
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------
// Utility functions
// ------------------------------------------------------------

fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

// ------------------------------------------------------------
// OVMF detection
// ------------------------------------------------------------

fn find_ovmf() -> PathBuf {
    // Allow explicit override:
    //
    // OVMF_CODE=/path/to/OVMF_CODE.fd ./run.sh --qemu --uefi
    //
    if let Ok(path) = std::env::var("OVMF_CODE") {
        let path = PathBuf::from(path);

        if path.is_file() {
            return path;
        }

        eprintln!(
            "ERROR: OVMF_CODE was set but does not exist:"
        );
        eprintln!("  {}", path.display());
        std::process::exit(1);
    }

    let candidates = [
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
        "/usr/share/OVMF/OVMF.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE_4M.fd",
        "/usr/share/edk2/ovmf/x64/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/x64/OVMF_CODE_4M.fd",
        "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd",
        "/usr/share/qemu/OVMF_CODE.fd",
        "/usr/share/qemu/OVMF_CODE_4M.fd",
        "/usr/share/edk2/x64/OVMF_CODE.fd",
    ];

    for candidate in candidates {
        let path = Path::new(candidate);

        if path.is_file() {
            return path.to_path_buf();
        }
    }

    // Package layouts vary between Linux distributions. Look for a firmware
    // file in the usual directories after checking the known exact paths.
    let search_dirs = [
        "/usr/share/OVMF",
        "/usr/share/edk2/ovmf",
        "/usr/share/edk2/ovmf/x64",
        "/usr/share/edk2-ovmf",
        "/usr/share/edk2-ovmf/x64",
        "/usr/share/edk2/x64",
        "/usr/share/qemu",
    ];

    for directory in search_dirs {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_uppercase();

            if path.is_file()
                && name.starts_with("OVMF_CODE")
                && name.ends_with(".FD")
            {
                return path;
            }
        }
    }

    eprintln!("ERROR: OVMF/EDK2 UEFI firmware was not found.");
    eprintln!();
    eprintln!("On Debian/Ubuntu, try:");
    eprintln!("  sudo apt install ovmf");
    eprintln!();
    eprintln!("Then check:");
    eprintln!("  find /usr/share -iname 'OVMF_CODE*.fd' 2>/dev/null");
    eprintln!();
    eprintln!("BIOS boot does not require OVMF:");
    eprintln!("  ./run.sh --qemu --bios");
    eprintln!();
    eprintln!("Or explicitly specify it:");
    eprintln!("  OVMF_CODE=/path/to/OVMF_CODE.fd ./run.sh --qemu --uefi");

    std::process::exit(1);
}

// ------------------------------------------------------------
// VirtualBox
// ------------------------------------------------------------

fn run_virtualbox(
    boot_image: &str,
    kernel_path: &Path,
    firmware: Firmware,
    force: bool,
) {
    let firmware_name = firmware.name();

    // Use a different VDI and VM for BIOS vs UEFI.
    //
    // This prevents VirtualBox from retaining a BIOS firmware
    // configuration when the user switches to UEFI.
    let vdi_path = format!("{}.vdi", boot_image);

    let vm_name = format!(
        "MFK-{}-{}",
        kernel_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kernel"),
        firmware_name
    );

    // --------------------------------------------------------
    // Rebuild VDI if necessary
    // --------------------------------------------------------

    let needs_convert = if force {
        if Path::new(&vdi_path).exists() {
            println!("--force: removing stale VDI {}", vdi_path);

            let _ = Command::new("VBoxManage")
                .args([
                    "closemedium",
                    "disk",
                    &vdi_path,
                    "--delete",
                ])
                .output();

            let _ = std::fs::remove_file(&vdi_path);
        }

        true
    } else if Path::new(&vdi_path).exists() {
        let image_mtime = std::fs::metadata(boot_image)
            .and_then(|m| m.modified())
            .ok();

        let vdi_mtime = std::fs::metadata(&vdi_path)
            .and_then(|m| m.modified())
            .ok();

        match (image_mtime, vdi_mtime) {
            (Some(image), Some(vdi)) if image > vdi => {
                println!(
                    "{} image newer than VDI - regenerating...",
                    firmware_name.to_uppercase()
                );

                let _ = Command::new("VBoxManage")
                    .args([
                        "closemedium",
                        "disk",
                        &vdi_path,
                        "--delete",
                    ])
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
        println!(
            "Converting {} image to VDI format...",
            firmware_name.to_uppercase()
        );

        if Path::new(&vdi_path).exists() {
            let _ = std::fs::remove_file(&vdi_path);
        }

        let result = Command::new("VBoxManage")
            .args([
                "convertfromraw",
                boot_image,
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
                    eprintln!(
                        "Warning: VBoxManage convertfromraw failed"
                    );
                    eprintln!(
                        "stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    std::process::exit(1);
                }

                println!("Created VDI disk: {}", vdi_path);
            }

            Err(e) => {
                eprintln!(
                    "Error: Failed to run VBoxManage: {}",
                    e
                );
                std::process::exit(1);
            }
        }
    } else {
        println!("Using existing VDI disk: {}", vdi_path);
    }

    // --------------------------------------------------------
    // Data disk
    // --------------------------------------------------------

    let data_vdi_path = "target/disk.vdi";

    if !Path::new(data_vdi_path).exists() {
        println!(
            "Creating data disk VDI: {}",
            data_vdi_path
        );

        let result = Command::new("VBoxManage")
            .args([
                "createmedium",
                "disk",
                "--filename",
                data_vdi_path,
                "--size",
                "10240",
                "--format",
                "VDI",
                "--variant",
                "Standard",
            ])
            .output();

        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!(
                        "Warning: Failed to create data disk VDI"
                    );
                    eprintln!(
                        "stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }

            Err(e) => {
                eprintln!(
                    "Warning: Failed to create data disk: {}",
                    e
                );
            }
        }
    }

    // --------------------------------------------------------
    // Check if VM already exists
    // --------------------------------------------------------

    let vm_exists = Command::new("VBoxManage")
        .args(["list", "vms"])
        .output()
        .map(|output| {
            let vms = String::from_utf8_lossy(&output.stdout);

            vms.contains(&format!("\"{}\"", vm_name))
        })
        .unwrap_or(false);

    // --------------------------------------------------------
    // VBoxManage helper
    // --------------------------------------------------------

    let run_vbox = |args: &[&str], desc: &str| -> bool {
        match Command::new("VBoxManage")
            .args(args)
            .output()
        {
            Ok(output) if output.status.success() => true,

            Ok(output) => {
                eprintln!(
                    "Warning: {} failed: {}",
                    desc,
                    String::from_utf8_lossy(&output.stderr)
                        .trim()
                );
                false
            }

            Err(e) => {
                eprintln!(
                    "Warning: {} failed to execute VBoxManage: {}",
                    desc,
                    e
                );
                false
            }
        }
    };

    // --------------------------------------------------------
    // Create VM
    // --------------------------------------------------------

    if !vm_exists {
        println!("Creating VirtualBox VM: {}", vm_name);

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

        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!(
                        "Error: Failed to create VM: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    std::process::exit(1);
                }
            }

            Err(e) => {
                eprintln!(
                    "Error: Failed to run VBoxManage createvm: {}",
                    e
                );
                std::process::exit(1);
            }
        }

        // Memory / CPUs
        if !run_vbox(
            &[
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
            ],
            "modifyvm memory",
        ) {
            eprintln!(
                "VM created but memory configuration failed."
            );
        }

        // IDE controller
        if !run_vbox(
            &[
                "storagectl",
                &vm_name,
                "--name",
                "IDE",
                "--add",
                "ide",
                "--controller",
                "PIIX4",
            ],
            "create IDE controller",
        ) {
            eprintln!(
                "Error: Failed to create IDE controller"
            );
            std::process::exit(1);
        }

        // Boot disk
        if !run_vbox(
            &[
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
            ],
            "attach boot disk",
        ) {
            eprintln!(
                "Error: Failed to attach boot disk"
            );
            std::process::exit(1);
        }

        // Data disk
        if Path::new(data_vdi_path).exists() {
            run_vbox(
                &[
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
                ],
                "attach data disk",
            );
        }

        // ----------------------------------------------------
        // Networking
        // ----------------------------------------------------

        run_vbox(
            &[
                "modifyvm",
                &vm_name,
                "--nic1",
                "bridged",
                "--nictype1",
                "82540EM",
            ],
            "configure bridged NIC",
        );

        let mut bridged_ok = false;

        if let Ok(output) = Command::new("VBoxManage")
            .args(["list", "bridgedifs"])
            .output()
        {
            if output.status.success() {
                let text =
                    String::from_utf8_lossy(&output.stdout);

                let mut discovered = Vec::new();

                for line in text.lines() {
                    if let Some(name) = line.strip_prefix("Name:") {
                        let iface = name.trim().to_string();

                        if !iface.is_empty() {
                            discovered.push(iface);
                        }
                    }
                }

                for iface in &discovered {
                    if run_vbox(
                        &[
                            "modifyvm",
                            &vm_name,
                            "--bridgeadapter1",
                            iface,
                        ],
                        &format!(
                            "set bridgeadapter1 to {}",
                            iface
                        ),
                    ) {
                        println!(
                            "Using network interface: {} (auto-detected)",
                            iface
                        );

                        bridged_ok = true;
                        break;
                    }
                }

                if !bridged_ok && !discovered.is_empty() {
                    eprintln!(
                        "Warning: Failed to set any \
                         auto-detected bridged interface: {:?}",
                        discovered
                    );
                }
            }
        }

        // Fallback interface names
        if !bridged_ok {
            let fallback = [
                "enp0s3",
                "enp0s8",
                "ens33",
                "enp1s0",
                "eth0",
                "eth1",
                "wlan0",
                "wlp2s0",
                "en0",
                "en1",
            ];

            for iface in &fallback {
                if run_vbox(
                    &[
                        "modifyvm",
                        &vm_name,
                        "--bridgeadapter1",
                        iface,
                    ],
                    &format!(
                        "set bridgeadapter1 to {}",
                        iface
                    ),
                ) {
                    println!(
                        "Using network interface: {} (fallback)",
                        iface
                    );

                    bridged_ok = true;
                    break;
                }
            }
        }

        if !bridged_ok {
            eprintln!(
                "Warning: Could not configure bridged adapter."
            );
            eprintln!(
                "  List adapters: VBoxManage list bridgedifs"
            );
            eprintln!(
                "  Or use QEMU: ./run.sh --qemu"
            );
        }

        // ----------------------------------------------------
        // Boot order + firmware
        // ----------------------------------------------------

        let vbox_firmware = match firmware {
            Firmware::Bios => "bios",
            Firmware::Uefi => "efi64",
        };

        if !run_vbox(
            &[
                "modifyvm",
                &vm_name,
                "--boot1",
                "disk",
                "--boot2",
                "none",
                "--firmware",
                vbox_firmware,
            ],
            "set boot order and firmware",
        ) {
            eprintln!(
                "Warning: Could not configure VirtualBox firmware."
            );
        }

        // Serial console
        let serial_log = std::env::current_dir()
            .map(|p| p.join("target/mfk-serial.log"))
            .unwrap_or_else(|_| {
                Path::new("target/mfk-serial.log")
                    .to_path_buf()
            });

        let serial_str =
            serial_log.to_string_lossy().to_string();

        run_vbox(
            &[
                "modifyvm",
                &vm_name,
                "--uart1",
                "0x3F8",
                "4",
                "--uartmode1",
                "file",
                &serial_str,
            ],
            "configure serial port",
        );

        println!("Serial log: {}", serial_str);
    } else {
        println!("Using existing VM: {}", vm_name);

        // ----------------------------------------------------
        // Refresh boot disk if the image changed
        // ----------------------------------------------------

        if needs_convert {
            println!(
                "Updating VM boot disk to new {} VDI...",
                firmware_name.to_uppercase()
            );

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
                    "--medium",
                    "none",
                ])
                .output();

            if !run_vbox(
                &[
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
                ],
                "re-attach boot disk",
            ) {
                eprintln!(
                    "Warning: Failed to update boot disk."
                );
                eprintln!(
                    "Try: VBoxManage unregistervm {} --delete",
                    vm_name
                );
            } else {
                println!("✓ Boot disk updated");
            }
        }

        // ----------------------------------------------------
        // Make sure firmware remains correct on reused VM
        // ----------------------------------------------------

        let vbox_firmware = match firmware {
            Firmware::Bios => "bios",
            Firmware::Uefi => "efi64",
        };

        run_vbox(
            &[
                "modifyvm",
                &vm_name,
                "--boot1",
                "disk",
                "--boot2",
                "none",
                "--firmware",
                vbox_firmware,
            ],
            "refresh firmware configuration",
        );

        // ----------------------------------------------------
        // Make sure data disk is attached
        // ----------------------------------------------------

        if Path::new(data_vdi_path).exists() {
            if let Ok(info) = Command::new("VBoxManage")
                .args([
                    "showvminfo",
                    &vm_name,
                    "--machinereadable",
                ])
                .output()
            {
                let info_str =
                    String::from_utf8_lossy(&info.stdout);

                if !info_str.contains("disk.vdi")
                    && !info_str.contains(data_vdi_path)
                {
                    run_vbox(
                        &[
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
                        ],
                        "re-attach data disk",
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------
    // Start VM
    // ------------------------------------------------------------

    let serial_abs = std::env::current_dir()
        .map(|p| p.join("target/mfk-serial.log"))
        .unwrap_or_else(|_| {
            Path::new("target/mfk-serial.log")
                .to_path_buf()
        });

    println!();
    println!("Starting VirtualBox VM: {}", vm_name);
    println!(
        "Firmware: {}",
        firmware_name.to_uppercase()
    );
    println!(
        "Boot image: {}",
        boot_image
    );
    println!(
        "Networking: Bridged (E1000/82540EM)"
    );
    println!(
        "Serial console: {}",
        serial_abs.display()
    );

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
                let stderr =
                    String::from_utf8_lossy(&output.stderr);

                eprintln!(
                    "Error starting VM: {}",
                    stderr.trim()
                );

                if stderr.contains("is already running")
                    || stderr.contains("is running")
                {
                    eprintln!(
                        "VM appears already running."
                    );

                    eprintln!(
                        "Access via VirtualBox GUI or:"
                    );

                    eprintln!(
                        "  VBoxManage controlvm {} poweroff",
                        vm_name
                    );
                } else if stderr.contains("Host network interface")
                    || stderr.contains("bridged")
                {
                    eprintln!(
                        "Bridged network failed."
                    );

                    eprintln!(
                        "  VBoxManage list bridgedifs"
                    );

                    eprintln!(
                        "  VBoxManage modifyvm {} \
                         --bridgeadapter1 \"<ifname>\"",
                        vm_name
                    );
                }

                std::process::exit(1);
            }

            println!("VM started successfully!");
            println!(
                "Serial console output: {}",
                serial_abs.display()
            );
        }

        Err(e) => {
            eprintln!(
                "Error: Failed to start VirtualBox: {}",
                e
            );
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------
// QEMU BIOS
// ------------------------------------------------------------

/// Select the other default VNC port so the TCP and WebSocket listeners do
/// not attempt to bind the same port when the WebSocket port is 5900/5901.
fn qemu_vnc_tcp_port(websocket_port: u16) -> u16 {
    if websocket_port == 5900 { 5901 } else { 5900 }
}

fn run_qemu_bios(
    bios_path: &str,
    bundle: bool,
    xhci_kbd: bool,
    data_disk_size: &str,
    extra_disks: &[(String, String)],
    vnc: bool,
    vnc_port: u16,
    web_ui: bool,
    web_ui_port: u16,
) {
    let disk_path = "target/disk.img";

    create_qemu_data_disk(disk_path, data_disk_size);
    ensure_extra_disks(extra_disks);

    if bundle {
        bundle_qemu_examples(disk_path);
    }

    println!();
    println!("Running MFK in QEMU...");
    println!("Firmware: BIOS");
    println!("Boot image: {}", bios_path);
    println!(
        "Networking: E1000 + user-mode NAT"
    );
    if xhci_kbd {
        println!("Input: xHCI USB keyboard + absolute tablet (PS/2 overridden)");
    }

    let mut qemu = Command::new("qemu-system-x86_64");

    qemu.args([
        "-cpu",
        "max",

        "-drive",
        &format!(
            "file={},format=raw,if=ide,index=0,media=disk",
            bios_path
        ),

        "-drive",
        &format!(
            "file={},format=raw,if=ide,index=1,media=disk,cache=none,readonly=off",
            disk_path
        ),

        "-serial",
        "stdio",

        "-display",
        "gtk",

        "-no-reboot",
        "-no-shutdown",

        "-m",
        "128M",

        // Intel E1000 emulation.
        "-device",
        "e1000,netdev=net0",

        "-netdev",
        "user,id=net0,restrict=off,\
         hostfwd=udp::5555-:5555,\
         hostfwd=tcp::49152-:49152",
    ]);

    if vnc {
        let tcp_port = qemu_vnc_tcp_port(vnc_port);
        let display = tcp_port - 5900;
        qemu.args([
            "-vnc",
            &format!(":{},websocket={}", display, vnc_port),
        ]);
    }

    // Extra data disks: first 2 on the secondary IDE channel (index 2/3),
    // the rest as virtio-blk-pci devices (guest drive indices 4+).
    for (i, (path, _)) in extra_disks.iter().enumerate() {
        if i < 2 {
            qemu.args([
                "-drive",
                &format!(
                    "file={},format=raw,if=ide,index={},media=disk,cache=none,readonly=off",
                    path,
                    2 + i
                ),
            ]);
        } else {
            let id = format!("vd{}", i - 2);
            qemu.args([
                "-drive",
                &format!(
                    "file={},format=raw,if=none,id={},cache=none,readonly=off",
                    path, id
                ),
            ]);
            qemu.args(["-device", &format!("virtio-blk-pci,drive={}", id)]);
        }
    }

    if xhci_kbd {
        qemu.args([
            "-device",
            "qemu-xhci,id=xhci",
            "-device",
            "usb-kbd,bus=xhci.0",
            // Absolute pointer: host cursor maps 1:1, no GTK grab needed.
            // The guest claims it like a USB tablet (see xhci/usb drivers).
            "-device",
            "usb-tablet,bus=xhci.0",
        ]);
    }

    // Spawn web proxy before QEMU if web UI is enabled
    let mut web_proxy_child = None;
    if web_ui {
        let proxy_bin = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target/release/mfk_web_proxy");
        if proxy_bin.exists() {
            println!(
                "Starting web proxy on port {} (QEMU VNC websocket: 127.0.0.1:{}, raw VNC TCP: {})...",
                web_ui_port,
                vnc_port,
                qemu_vnc_tcp_port(vnc_port)
            );
            web_proxy_child = Some(
                Command::new(&proxy_bin)
                    .arg(web_ui_port.to_string())
                    .arg(vnc_port.to_string())
                    .spawn()
                    .expect("Failed to start mfk_web_proxy")
            );
        } else {
            eprintln!("Warning: mfk_web_proxy not found at {}. Run 'cargo build --release -p mfk-runner' to build it.", proxy_bin.display());
        }
    }

    let mut child = qemu
        .spawn()
        .expect("Failed to start QEMU");

    child.wait().expect("Failed to wait on QEMU");

    // Clean up web proxy when QEMU exits
    if let Some(mut proxy) = web_proxy_child {
        let _ = proxy.kill();
        let _ = proxy.wait();
    }
}

// ------------------------------------------------------------
// QEMU UEFI
// ------------------------------------------------------------

fn run_qemu_uefi(
    uefi_path: &str,
    bundle: bool,
    xhci_kbd: bool,
    data_disk_size: &str,
    extra_disks: &[(String, String)],
    vnc: bool,
    vnc_port: u16,
    web_ui: bool,
    web_ui_port: u16,
) {
    let ovmf = find_ovmf();

    let disk_path = "target/disk.img";

    create_qemu_data_disk(disk_path, data_disk_size);
    ensure_extra_disks(extra_disks);

    if bundle {
        bundle_qemu_examples(disk_path);
    }

    println!();
    println!("Running MFK in QEMU...");
    println!("Firmware: UEFI / OVMF");
    println!("OVMF: {}", ovmf.display());
    println!("Boot image: {}", uefi_path);
    println!(
        "Networking: E1000 + user-mode NAT"
    );
    if xhci_kbd {
        println!("Input: xHCI USB keyboard + absolute tablet ONLY");
    } else {
        println!("Input: PS/2 keyboard");
    }

    let mut qemu = Command::new("qemu-system-x86_64");
    let ovmf_drive = format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    );

    qemu.args([
        "-cpu",
        "host",
        "-enable-kvm",

        // Use the legacy PC machine because the kernel's disk driver uses
        // the legacy ATA PIO ports; Q35 exposes AHCI instead.
        "-machine",
        "pc",

        "-m",
        "128M",

        // IMPORTANT:
        //
        // The UEFI image itself is attached here.
        //
        // We are NOT passing the kernel to QEMU with -kernel.
        // OVMF must discover and execute:
        //
        // EFI/BOOT/BOOTX64.EFI
        //
        "-drive",
        &format!(
            "file={},format=raw,if=ide,index=0,media=disk",
            uefi_path
        ),

        "-drive",
        &format!(
            "file={},format=raw,if=ide,index=1,media=disk,cache=none,readonly=off",
            disk_path
        ),

        // OVMF_CODE is a flash image, not a legacy PC BIOS image.
        "-drive",
        &ovmf_drive,

        "-serial",
        "stdio",

        "-display",
        "gtk",

        "-no-reboot",
        "-no-shutdown",

        // Intel E1000.
        "-device",
        "e1000,netdev=net0",

        "-netdev",
        "user,id=net0,restrict=off,\
         hostfwd=udp::5555-:5555,\
         hostfwd=tcp::49152-:49152",
    ]);

    if vnc {
        let tcp_port = qemu_vnc_tcp_port(vnc_port);
        let display = tcp_port - 5900;
        qemu.args([
            "-vnc",
            &format!(":{},websocket={}", display, vnc_port),
        ]);
    }

    // Extra data disks: first 2 on the secondary IDE channel (index 2/3),
    // the rest as virtio-blk-pci devices (guest drive indices 4+).
    for (i, (path, _)) in extra_disks.iter().enumerate() {
        if i < 2 {
            qemu.args([
                "-drive",
                &format!(
                    "file={},format=raw,if=ide,index={},media=disk,cache=none,readonly=off",
                    path,
                    2 + i
                ),
            ]);
        } else {
            let id = format!("vd{}", i - 2);
            qemu.args([
                "-drive",
                &format!(
                    "file={},format=raw,if=none,id={},cache=none,readonly=off",
                    path, id
                ),
            ]);
            qemu.args(["-device", &format!("virtio-blk-pci,drive={}", id)]);
        }
    }

    if xhci_kbd {
        qemu.args([
            "-device",
            "qemu-xhci,id=xhci",
            "-device",
            "usb-kbd,bus=xhci.0",
            // Absolute pointer: host cursor maps 1:1, no GTK grab needed.
            "-device",
            "usb-tablet,bus=xhci.0",
        ]);
    }

    // Spawn web proxy before QEMU if web UI is enabled
    let mut web_proxy_child = None;
    if web_ui {
        let proxy_bin = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target/release/mfk_web_proxy");
        if proxy_bin.exists() {
            println!(
                "Starting web proxy on port {} (QEMU VNC websocket: 127.0.0.1:{}, raw VNC TCP: {})...",
                web_ui_port,
                vnc_port,
                qemu_vnc_tcp_port(vnc_port)
            );
            web_proxy_child = Some(
                Command::new(&proxy_bin)
                    .arg(web_ui_port.to_string())
                    .arg(vnc_port.to_string())
                    .spawn()
                    .expect("Failed to start mfk_web_proxy")
            );
        } else {
            eprintln!("Warning: mfk_web_proxy not found at {}. Run 'cargo build --release -p mfk-runner' to build it.", proxy_bin.display());
        }
    }

    let mut child = qemu
        .spawn()
        .expect("Failed to start QEMU with OVMF");

    child.wait().expect("Failed to wait on QEMU");

    // Clean up web proxy when QEMU exits
    if let Some(mut proxy) = web_proxy_child {
        let _ = proxy.kill();
        let _ = proxy.wait();
    }
}

// ------------------------------------------------------------
// QEMU data disk
// ------------------------------------------------------------

fn create_qemu_data_disk(
    disk_path: &str,
    size: &str,
) {
    if !Path::new(disk_path).exists() {
        println!(
            "Creating virtual data disk: {} ({})",
            disk_path,
            size
        );

        let result = Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "raw",
                disk_path,
                size,
            ])
            .output();

        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!(
                        "Warning: Failed to create data disk."
                    );

                    eprintln!(
                        "stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }

            Err(e) => {
                eprintln!(
                    "Warning: Failed to execute qemu-img: {}",
                    e
                );
            }
        }
    } else {
        println!(
            "Using existing data disk: {}",
            disk_path
        );
    }
}

/// Creates missing extra data disks (secondary IDE channel). Existing files
/// are reused regardless of the requested size.
fn ensure_extra_disks(extra_disks: &[(String, String)]) {
    for (i, (path, size)) in extra_disks.iter().enumerate() {
        if Path::new(path).exists() {
            println!(
                "Using existing extra disk #{}: {}",
                i + 1,
                path
            );
            continue;
        }

        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        println!(
            "Creating extra disk #{}: {} ({})",
            i + 1,
            path,
            size
        );

        match Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "raw",
                path,
                size.as_str(),
            ])
            .output()
        {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!(
                        "Warning: Failed to create extra disk {}: {}",
                        path,
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
            }

            Err(e) => {
                eprintln!(
                    "Warning: Failed to execute qemu-img for {}: {}",
                    path,
                    e
                );
            }
        }
    }
}

fn bundle_qemu_examples(
    disk_path: &str,
) {
    let examples = Path::new("apps/examples");

    if !examples.exists() {
        return;
    }

    println!(
        "Bundling example apps into {}...",
        disk_path
    );

    if let Err(e) = simplfs_host::bundle_examples(
        Path::new(disk_path),
        examples,
    ) {
        eprintln!("Bundle failed: {}", e);
    }
}

// ------------------------------------------------------------
// Usage
// ------------------------------------------------------------

fn print_usage(program: &str) {
    eprintln!(
        "Usage: {} <kernel-binary-path> [OPTIONS]",
        program
    );

    eprintln!();

    eprintln!("OPTIONS:");

    eprintln!(
        "  --vbox, --virtualbox     Run in VirtualBox (default)"
    );

    eprintln!(
        "  --qemu                   Run in QEMU"
    );

    eprintln!(
        "  --bios                   Use BIOS firmware (default)"
    );

    eprintln!(
        "  --uefi                   Use UEFI firmware"
    );

    eprintln!(
        "  --no-run                 Only create disk images"
    );

    eprintln!(
        "  --force                  Force rebuild VDI/VM"
    );

    eprintln!(
        "  --bundle-apps, --with-apps"
    );

    eprintln!(
        "                           Bundle apps/examples into target/disk.img"
    );

    eprintln!(
        "  --kbd=<ps2|xhci>          QEMU keyboard transport (default ps2)"
    );

    eprintln!(
        "  --xhci-kbd               Attach qemu-xhci + usb-kbd to QEMU"
    );

    eprintln!(
        "  --data-disk-size=<size>   Size for target/disk.img (default 10M)"
    );

    eprintln!(
        "  --extra-disk=<path>       Extra data disk (repeatable, max 8: first 2 IDE, rest virtio-blk)"
    );

    eprintln!(
        "  --extra-disk-size=<size>  Size for preceding --extra-disk (default 64M)"
    );

    eprintln!(
        "  --boot-extra-disk[=N]     Boot extra disk N (1-based, bare = first)"
    );

    eprintln!(
        "  --vnc                    Enable QEMU VNC server"
    );

    eprintln!(
        "  --vnc-port=<port>         VNC WebSocket port (default 5900)"
    );

    eprintln!(
        "  --web-ui                 Launch noVNC web UI (implies --vnc)"
    );

    eprintln!(
        "  --web-ui-port=<port>      Web UI HTTP port (default 8084)"
    );

    eprintln!();

    eprintln!("Examples:");

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --qemu --bios",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --vbox --bios",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --vbox --uefi",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi --bundle-apps",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi --kbd=xhci",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi --vnc --web-ui",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --no-run",
        program
    );

    eprintln!();

    eprintln!("UEFI:");

    eprintln!(
        "  Install OVMF on Debian/Ubuntu:"
    );

    eprintln!(
        "    sudo apt install ovmf"
    );

    eprintln!(
        "  Or specify firmware manually:"
    );

    eprintln!(
        "    OVMF_CODE=/path/to/OVMF_CODE.fd {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi",
        program
    );
}
