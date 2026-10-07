//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFK Runner - Creates bootable disk images and runs them in VirtualBox or QEMU

use std::path::{Path, PathBuf};
use std::process::Command;

mod simplfs_host;
mod uefi_iso;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Firmware {
    Bios,
    Uefi,
}

/// Which USB host controller the emulated keyboard and tablet attach to.
/// The guest's matching driver (xhci/ehci/uhci/ohci) claims the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KbdMode {
    Ps2,
    Xhci,
    Ehci,
    Uhci,
    Ohci,
}

impl KbdMode {
    fn is_usb(self) -> bool {
        self != KbdMode::Ps2
    }

    /// QEMU `-device` string for this controller, with an explicit id so the
    /// bus name is predictable.
    fn hcd_device(self) -> Option<(&'static str, &'static str)> {
        match self {
            KbdMode::Xhci => Some(("qemu-xhci", "xhci")),
            KbdMode::Ehci => Some(("usb-ehci", "ehci")),
            KbdMode::Uhci => Some(("piix3-usb-uhci", "uhci")),
            KbdMode::Ohci => Some(("pci-ohci", "ohci")),
            KbdMode::Ps2 => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            KbdMode::Ps2 => "ps2",
            KbdMode::Xhci => "xHCI",
            KbdMode::Ehci => "EHCI",
            KbdMode::Uhci => "UHCI",
            KbdMode::Ohci => "OHCI",
        }
    }
}

impl Firmware {
    fn name(self) -> &'static str {
        match self {
            Firmware::Bios => "bios",
            Firmware::Uefi => "uefi",
        }
    }
}

#[derive(Debug, Clone)]
struct GpuPassthrough {
    device: String,
    audio: Option<String>,
    rom: Option<String>,
}

fn parse_pci_bdf(value: &str) -> Option<String> {
    let value = value.strip_prefix("0000:").unwrap_or(value);
    let mut parts = value.split(':');
    let bus = parts.next()?;
    let device_function = parts.next()?;
    if parts.next().is_some() || bus.len() != 2 {
        return None;
    }
    let (device, function) = device_function.split_once('.')?;
    if device.len() != 2 || function.len() != 1 {
        return None;
    }
    u16::from_str_radix(bus, 16).ok()?;
    u16::from_str_radix(device, 16).ok()?;
    u16::from_str_radix(function, 16).ok()?;
    Some(value.to_string())
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
        eprintln!("  cargo run -p mfk-runner --bin mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel");
        std::process::exit(1);
    }

    // ------------------------------------------------------------
    // Parse CLI options
    // ------------------------------------------------------------

    let hypervisor = if args.iter().any(|a| a == "--qemu") {
        "qemu"
    } else if args
        .iter()
        .any(|a| a == "--hyperv" || a == "--hyper-v" || a == "--hv")
    {
        "hyperv"
    } else if args.iter().any(|a| a == "--vbox" || a == "--virtualbox") {
        "vbox"
    } else {
        "vbox"
    };

    // Hyper-V Generation 2 VMs are UEFI-only; BIOS is irrelevant there.
    if hypervisor == "hyperv" && args.iter().any(|a| a == "--bios") {
        eprintln!("ERROR: --hyperv is UEFI-only (Generation 2). Drop --bios or use --uefi.");
        std::process::exit(1);
    }

    let firmware = if hypervisor == "hyperv" || args.iter().any(|a| a == "--uefi") {
        Firmware::Uefi
    } else {
        Firmware::Bios
    };

    let no_run = args.iter().any(|a| a == "--no-run");
    let force = args.iter().any(|a| a == "--force");
    // Skip creating the UEFI ISO (`--no-iso`). Everything except Hyper-V
    // DVD boot runs from the raw .img files, so the ISO is optional.
    // Hyper-V DVD boot fails with a clear error when the ISO is missing.
    let no_iso = args.iter().any(|a| a == "--no-iso");
    // Hyper-V options (Windows only, UEFI only).
    let hyperv_switch: Option<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--hyperv-switch="))
        .map(|s| s.to_string());
    let hyperv_mem: u32 = args
        .iter()
        .find_map(|a| a.strip_prefix("--hyperv-mem="))
        .and_then(|s| s.trim_end_matches(['M', 'm']).parse().ok())
        .unwrap_or(512);
    let hyperv_cpus: u32 = args
        .iter()
        .find_map(|a| a.strip_prefix("--hyperv-cpus="))
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    // COM1 named pipe for kernel serial output (Gen2 has no file-backed
    // serial like VirtualBox). `off` disables COM configuration.
    let hyperv_com: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--hyperv-com="))
        .map(|s| s.to_string())
        .unwrap_or_default();
    // DVD boots the generated UEFI ISO; disk boots a VHDX (see --vhdx).
    let hyperv_boot: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--hyperv-boot="))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "dvd".to_string());
    if hypervisor == "hyperv" && hyperv_boot != "dvd" && hyperv_boot != "disk" {
        eprintln!("ERROR: --hyperv-boot must be dvd or disk.");
        std::process::exit(1);
    }
    let vhdx_arg: Option<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--vhdx="))
        .map(|s| s.to_string());
    let gpu_passthrough_arg = args
        .iter()
        .find_map(|a| a.strip_prefix("--gpu-passthrough="));
    let gpu_audio = args.iter().find_map(|a| a.strip_prefix("--gpu-audio="));
    let gpu_rom = args.iter().find_map(|a| a.strip_prefix("--gpu-rom="));
    let gpu_passthrough = gpu_passthrough_arg.and_then(|value| {
        let device = parse_pci_bdf(value)?;
        let audio = match gpu_audio {
            Some(audio) => Some(parse_pci_bdf(audio)?),
            None => None,
        };
        Some(GpuPassthrough {
            device,
            audio,
            rom: gpu_rom.map(ToString::to_string),
        })
    });
    if gpu_passthrough_arg.is_some() && gpu_passthrough.is_none() {
        eprintln!("ERROR: Invalid PCI BDF for --gpu-passthrough.");
        std::process::exit(1);
    }
    if gpu_passthrough.is_some() && cfg!(target_os = "windows") {
        eprintln!("ERROR: GPU passthrough is unavailable on native Windows.");
        eprintln!("Use QEMU's emulated GPU, or assign the GPU to a Hyper-V VM with Discrete Device Assignment.");
        std::process::exit(1);
    }
    if gpu_passthrough.is_none() && (gpu_audio.is_some() || gpu_rom.is_some()) {
        eprintln!("ERROR: --gpu-audio and --gpu-rom require --gpu-passthrough=<BDF>.");
        std::process::exit(1);
    }
    if let Some(rom) = gpu_passthrough.as_ref().and_then(|gpu| gpu.rom.as_deref()) {
        if !Path::new(rom).is_file() {
            eprintln!("ERROR: GPU ROM file not found at: {}", rom);
            std::process::exit(1);
        }
    }
    let bundle = args
        .iter()
        .any(|a| a == "--bundle-apps" || a == "--with-apps");
    // Doom WAD injection into an (extra) disk image:
    //   --wad=<host-wad> [--wad-disk=target/doom.img] [--wad-disk-size=128M]
    //   [--wad-guest=/wad/doom1.wad]
    // The WAD disk stays separate from target/disk.img by design.
    let wad_host: Option<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--wad="))
        .map(|s| s.to_string());
    let wad_disk: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--wad-disk="))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "target/doom.img".to_string());
    let wad_disk_size: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--wad-disk-size="))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "128M".to_string());
    let wad_guest: String = args
        .iter()
        .find_map(|a| a.strip_prefix("--wad-guest="))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "/wad/doom1.wad".to_string());

    // Keyboard transport: --kbd=<ps2|xhci|ehci|uhci|ohci> (from mfk_launch.py)
    // or legacy --xhci-kbd. Each USB mode attaches a matching QEMU host
    // controller plus usb-kbd/usb-tablet; the emulated usb-kbd overrides
    // PS/2 so guest input flows through that controller's driver. Serial
    // stdio stays available as a backdoor.
    let hw_profile = args
        .iter()
        .find_map(|a| a.strip_prefix("--hw-profile="))
        .map(|s| s.to_ascii_lowercase());
    let kbd_arg = args
        .iter()
        .find_map(|a| a.strip_prefix("--kbd=").map(|v| v.to_ascii_lowercase()));
    let kbd = match kbd_arg.as_deref() {
        Some("xhci") => KbdMode::Xhci,
        Some("ehci") => KbdMode::Ehci,
        Some("uhci") => KbdMode::Uhci,
        Some("ohci") => KbdMode::Ohci,
        Some("ps2") => KbdMode::Ps2,
        Some(other) => {
            eprintln!("Warning: unknown --kbd={other} (expected ps2|xhci|ehci|uhci|ohci); using ps2.");
            KbdMode::Ps2
        }
        None => {
            // Real N95: notebook-class PC, UEFI, USB keyboard/mouse over
            // xHCI, legacy PS/2 input path is effectively absent. If the
            // operator asked for the N95 profile, default to xHCI so a
            // QEMU boot mirrors the real IRQ/driver path instead of the
            // emulated i8042 PS/2 one.
            if hw_profile.as_deref() == Some("n95") {
                KbdMode::Xhci
            } else {
                KbdMode::Ps2
            }
        }
    };
    // Legacy alias for the historical xHCI-only flag.
    let kbd = if args.iter().any(|a| a == "--xhci-kbd") {
        KbdMode::Xhci
    } else {
        kbd
    };

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
    let vnc_port: u16 = args
        .iter()
        .find_map(|a| a.strip_prefix("--vnc-port="))
        .and_then(|s| s.parse().ok())
        .unwrap_or(5900);
    let web_ui = args.iter().any(|a| a == "--web-ui");
    let vnc = web_ui || args.iter().any(|a| a == "--vnc");
    let web_ui_port: u16 = args
        .iter()
        .find_map(|a| a.strip_prefix("--web-ui-port="))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8084);

    // Hyper-V (Gen2) ignores QEMU-only devices: warn instead of silently
    // dropping them so `--hyperv --vnc` typos are visible.
    if hypervisor == "hyperv" {
        if vnc || web_ui {
            eprintln!("Warning: --vnc/--web-ui are QEMU-only and are ignored with --hyperv.");
        }
        if kbd.is_usb() || kbd_arg.is_some() {
            eprintln!("Warning: --kbd/--xhci-kbd are QEMU-only and are ignored with --hyperv.");
        }
        if gpu_passthrough.is_some() {
            eprintln!("Warning: --gpu-passthrough is QEMU-only and is ignored with --hyperv.");
            eprintln!("For a physical GPU in Hyper-V, use Discrete Device Assignment (DDA) manually.");
        }
        if boot_extra.is_some() {
            eprintln!("Warning: --boot-extra-disk is QEMU/VBox-legacy only and is ignored with --hyperv.");
            eprintln!("Use --hyperv-boot=dvd|disk to select the Hyper-V boot device.");
        }
    }

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
    // Create the UEFI ISO (El Torito, for Hyper-V Gen2 DVD boot).
    //
    // BIOS is legacy here and intentionally gets no ISO.
    // Skipped with --no-iso (Hyper-V DVD boot then needs the ISO
    // to already exist, otherwise it exits with a clear error).
    // ------------------------------------------------------------

    let iso_path = format!("{}-uefi.iso", args[1]);

    if no_iso {
        println!("Skipping UEFI ISO creation (--no-iso).");
        if hypervisor == "hyperv" && hyperv_boot == "dvd" && !Path::new(&iso_path).exists() {
            eprintln!(
                "ERROR: Hyper-V DVD boot needs the UEFI ISO but it is missing: {}",
                iso_path
            );
            eprintln!("Drop --no-iso to create it, or use --hyperv-boot=disk.");
            std::process::exit(1);
        }
    } else {
        println!("Creating UEFI ISO image...");
        match uefi_iso::create_uefi_iso(Path::new(&uefi_path), Path::new(&iso_path)) {
            Ok(_) => println!("Created UEFI ISO image: {}", iso_path),
            Err(e) => {
                eprintln!("Failed to create UEFI ISO image: {}", e);
                std::process::exit(1);
            }
        }
    }

    // ------------------------------------------------------------
    // Pre-create raw data disks so --bundle-apps works on the first run
    // (QEMU helpers create them lazily inside run_qemu_*; Hyper-V needs
    // the raws now so they can be bundled then converted to VHDX).
    // ------------------------------------------------------------

    if hypervisor == "hyperv" {
        create_qemu_data_disk("target/disk.img", &data_disk_size);
        ensure_extra_disks(&extra_disks);
    }

    // ------------------------------------------------------------
    // Bundle example apps into the raw data disk if requested
    // (Hyper-V converts target/disk.img -> target/disk.vhdx later).
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

            if let Err(e) = simplfs_host::bundle_examples(disk_raw, examples) {
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
    // Bundle a Doom WAD into its own disk image (kept off target/disk.img).
    // Creates the raw image on demand so `--wad` works on first run.
    // ------------------------------------------------------------

    if let Some(wad) = wad_host.as_deref() {
        let wad_src = Path::new(wad);
        if !wad_src.is_file() {
            eprintln!("WAD bundle: host file not found: {}", wad);
            std::process::exit(1);
        }
        if !Path::new(&wad_disk).exists() {
            println!(
                "WAD bundle: creating WAD disk {} ({})...",
                wad_disk, wad_disk_size
            );
            create_qemu_data_disk(&wad_disk, &wad_disk_size);
        }
        if Path::new(&wad_disk).exists() {
            if let Err(e) = simplfs_host::bundle_single_file(
                Path::new(&wad_disk),
                &wad_guest,
                wad_src,
            ) {
                eprintln!("WAD bundle failed: {}", e);
            }
        } else {
            eprintln!(
                "WAD bundle: {} not found (install qemu-img to create it)",
                wad_disk
            );
        }
        // Fully automated setup: attach the WAD disk unless the user
        // already listed it via --extra-disk (avoids double-attach).
        if !extra_disks.iter().any(|(p, _)| p == &wad_disk) {
            if extra_disks.len() >= MAX_EXTRAS {
                eprintln!(
                    "Warning: extra disk slots full ({}); WAD disk {} bundled but not attached.",
                    MAX_EXTRAS, wad_disk
                );
            } else {
                println!("WAD bundle: auto-attaching {} as extra disk.", wad_disk);
                extra_disks.push((wad_disk.clone(), wad_disk_size.clone()));
            }
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
    if no_iso && !Path::new(&iso_path).exists() {
        println!("  UEFI ISO:   (skipped)");
    } else {
        println!("  UEFI ISO:   {}", iso_path);
    }
    if hypervisor == "hyperv" {
        println!("  HV boot:    {}", hyperv_boot);
        println!(
            "  HV switch:  {}",
            hyperv_switch.as_deref().unwrap_or("Default Switch (auto)")
        );
        println!("  HV memory:  {} MB", hyperv_mem);
        println!("  HV CPUs:    {}", hyperv_cpus);
        if let Some(v) = vhdx_arg.as_deref() {
            println!("  HV VHDX:    {}", v);
        }
        if hyperv_com.is_empty() {
            println!("  HV COM:     pipe MFK-<vm>-com1 (use tools/hyperv-serial.ps1)");
        } else if hyperv_com == "off" {
            println!("  HV COM:     disabled");
        } else {
            println!("  HV COM:     pipe {}", hyperv_com);
        }
    }
    if kbd.is_usb() && hypervisor != "hyperv" {
        println!(
            "  Input:      {} USB keyboard + absolute tablet",
            kbd.name()
        );
    }
    if hypervisor == "hyperv" {
        println!(
            "  Data disk:  target/disk.img ({}) -> target/disk.vhdx (SCSI)",
            data_disk_size
        );
        for (i, (p, s)) in extra_disks.iter().enumerate() {
            println!("  Extra #{}:   {} ({}) -> {}.vhdx (SCSI)", i + 1, p, s, p);
        }
    } else {
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
    }
    if (vnc || web_ui) && hypervisor != "hyperv" {
        println!(
            "  VNC:        enabled (TCP port {}, WebSocket port {})",
            qemu_vnc_tcp_port(vnc_port),
            vnc_port
        );
    }
    if web_ui && hypervisor != "hyperv" {
        println!("  Web UI:     enabled (port {})", web_ui_port);
    }
    if let Some(gpu) = &gpu_passthrough {
        if hypervisor == "hyperv" {
            println!("  GPU:        ignored (QEMU-only; use Hyper-V DDA manually)");
        } else {
            println!("  GPU:        vfio-pci {}", gpu.device);
            if let Some(audio) = &gpu.audio {
                println!("  GPU audio:  vfio-pci {}", audio);
            }
            if let Some(rom) = &gpu.rom {
                println!("  GPU ROM:    {}", rom);
            }
        }
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
            eprintln!("Install: sudo apt-get install qemu-system-x86 qemu-utils");
            std::process::exit(1);
        }
    } else if selected_hypervisor == "hyperv" {
        if !cfg!(target_os = "windows") {
            eprintln!("ERROR: --hyperv requires Windows with the Hyper-V role.");
            eprintln!("On Linux/macOS use --vbox or --qemu (the UEFI ISO is still created).");
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
                kbd,
                &data_disk_size,
                &extra_disks,
                vnc,
                vnc_port,
                web_ui,
                web_ui_port,
                gpu_passthrough.as_ref(),
            ),
            Firmware::Uefi => run_qemu_uefi(
                &uefi_path,
                bundle,
                kbd,
                &data_disk_size,
                &extra_disks,
                vnc,
                vnc_port,
                web_ui,
                web_ui_port,
                gpu_passthrough.as_ref(),
            ),
        },

        "vbox" => match firmware {
            Firmware::Bios => run_virtualbox(&bios_path, kernel_path, Firmware::Bios, force),

            Firmware::Uefi => run_virtualbox(&uefi_path, kernel_path, Firmware::Uefi, force),
        },

        "hyperv" => run_hyperv(
            &iso_path,
            &uefi_path,
            kernel_path,
            force,
            hyperv_boot.as_str(),
            hyperv_switch.as_deref(),
            hyperv_mem,
            hyperv_cpus,
            vhdx_arg.as_deref(),
            hyperv_com.as_str(),
            &data_disk_size,
            &extra_disks,
        ),

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

fn qemu_supports_whpx() -> bool {
    Command::new("qemu-system-x86_64")
        .args(["-accel", "help"])
        .output()
        .map(|output| {
            let help = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            help.split_whitespace().any(|item| item == "whpx")
        })
        .unwrap_or(false)
}

fn add_qemu_acceleration(qemu: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        if qemu_supports_whpx() {
            qemu.args(["-accel", "tcg", "-cpu", "max"]);
        } else {
            eprintln!("WHPX is not available; falling back to TCG.");
            qemu.args(["-accel", "tcg", "-cpu", "max"]);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if Path::new("/dev/kvm").exists() {
            qemu.args(["-accel", "kvm", "-cpu", "host"]);
        } else {
            eprintln!("KVM is not available; falling back to TCG.");
            qemu.args(["-accel", "tcg", "-cpu", "max"]);
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        qemu.args(["-accel", "tcg", "-cpu", "max"]);
    }
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

        eprintln!("ERROR: OVMF_CODE was set but does not exist:");
        eprintln!("  {}", path.display());
        std::process::exit(1);
    }

    if cfg!(target_os = "windows") {
        let mut candidates = Vec::new();
        if let Some(root) = std::env::var_os("QEMU_HOME") {
            let root = PathBuf::from(root);
            candidates.push(root.join("share/edk2-x86_64-code.fd"));
            candidates.push(root.join("share/OVMF_CODE.fd"));
        }
        for variable in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
            let Some(root) = std::env::var_os(variable) else {
                continue;
            };
            let root = PathBuf::from(root);
            candidates.push(root.join("qemu/share/edk2-x86_64-code.fd"));
            candidates.push(root.join("qemu/share/OVMF_CODE.fd"));
        }
        for candidate in candidates {
            if candidate.is_file() {
                return candidate;
            }
        }
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

            if path.is_file() && name.starts_with("OVMF_CODE") && name.ends_with(".FD") {
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

fn run_virtualbox(boot_image: &str, kernel_path: &Path, firmware: Firmware, force: bool) {
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
                .args(["closemedium", "disk", &vdi_path, "--delete"])
                .output();

            let _ = std::fs::remove_file(&vdi_path);
        }

        true
    } else if Path::new(&vdi_path).exists() {
        let image_mtime = std::fs::metadata(boot_image)
            .and_then(|m| m.modified())
            .ok();

        let vdi_mtime = std::fs::metadata(&vdi_path).and_then(|m| m.modified()).ok();

        match (image_mtime, vdi_mtime) {
            (Some(image), Some(vdi)) if image > vdi => {
                println!(
                    "{} image newer than VDI - regenerating...",
                    firmware_name.to_uppercase()
                );

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
                    eprintln!("Warning: VBoxManage convertfromraw failed");
                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                    std::process::exit(1);
                }

                println!("Created VDI disk: {}", vdi_path);
            }

            Err(e) => {
                eprintln!("Error: Failed to run VBoxManage: {}", e);
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
        println!("Creating data disk VDI: {}", data_vdi_path);

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
                    eprintln!("Warning: Failed to create data disk VDI");
                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                }
            }

            Err(e) => {
                eprintln!("Warning: Failed to create data disk: {}", e);
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
        match Command::new("VBoxManage").args(args).output() {
            Ok(output) if output.status.success() => true,

            Ok(output) => {
                eprintln!(
                    "Warning: {} failed: {}",
                    desc,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                false
            }

            Err(e) => {
                eprintln!("Warning: {} failed to execute VBoxManage: {}", desc, e);
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
                eprintln!("Error: Failed to run VBoxManage createvm: {}", e);
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
            eprintln!("VM created but memory configuration failed.");
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
            eprintln!("Error: Failed to create IDE controller");
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
            eprintln!("Error: Failed to attach boot disk");
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
                let text = String::from_utf8_lossy(&output.stdout);

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
                        &["modifyvm", &vm_name, "--bridgeadapter1", iface],
                        &format!("set bridgeadapter1 to {}", iface),
                    ) {
                        println!("Using network interface: {} (auto-detected)", iface);

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
                "enp0s3", "enp0s8", "ens33", "enp1s0", "eth0", "eth1", "wlan0", "wlp2s0", "en0",
                "en1",
            ];

            for iface in &fallback {
                if run_vbox(
                    &["modifyvm", &vm_name, "--bridgeadapter1", iface],
                    &format!("set bridgeadapter1 to {}", iface),
                ) {
                    println!("Using network interface: {} (fallback)", iface);

                    bridged_ok = true;
                    break;
                }
            }
        }

        if !bridged_ok {
            eprintln!("Warning: Could not configure bridged adapter.");
            eprintln!("  List adapters: VBoxManage list bridgedifs");
            eprintln!("  Or use QEMU: ./run.sh --qemu");
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
            eprintln!("Warning: Could not configure VirtualBox firmware.");
        }

        // Serial console
        let serial_log = std::env::current_dir()
            .map(|p| p.join("target/mfk-serial.log"))
            .unwrap_or_else(|_| Path::new("target/mfk-serial.log").to_path_buf());

        let serial_str = serial_log.to_string_lossy().to_string();

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
                .args(["showvminfo", &vm_name, "--machinereadable"])
                .output()
            {
                let info_str = String::from_utf8_lossy(&info.stdout);

                if !info_str.contains("disk.vdi") && !info_str.contains(data_vdi_path) {
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
        .unwrap_or_else(|_| Path::new("target/mfk-serial.log").to_path_buf());

    println!();
    println!("Starting VirtualBox VM: {}", vm_name);
    println!("Firmware: {}", firmware_name.to_uppercase());
    println!("Boot image: {}", boot_image);
    println!("Networking: Bridged (E1000/82540EM)");
    println!("Serial console: {}", serial_abs.display());

    let result = Command::new("VBoxManage")
        .args(["startvm", &vm_name, "--type", "gui"])
        .output();

    match result {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);

                eprintln!("Error starting VM: {}", stderr.trim());

                if stderr.contains("is already running") || stderr.contains("is running") {
                    eprintln!("VM appears already running.");

                    eprintln!("Access via VirtualBox GUI or:");

                    eprintln!("  VBoxManage controlvm {} poweroff", vm_name);
                } else if stderr.contains("Host network interface") || stderr.contains("bridged") {
                    eprintln!("Bridged network failed.");

                    eprintln!("  VBoxManage list bridgedifs");

                    eprintln!(
                        "  VBoxManage modifyvm {} \
                         --bridgeadapter1 \"<ifname>\"",
                        vm_name
                    );
                }

                std::process::exit(1);
            }

            println!("VM started successfully!");
            println!("Serial console output: {}", serial_abs.display());
        }

        Err(e) => {
            eprintln!("Error: Failed to start VirtualBox: {}", e);
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------
// Hyper-V (Windows, Generation 2 / UEFI only)
// ------------------------------------------------------------

/// Escape a string for embedding in a PowerShell single-quoted literal.
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Run a PowerShell script, returning trimmed stdout or an error.
fn run_powershell(script: &str) -> Result<String, String> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("Failed to launch powershell: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Convert any raw image to VHDX, reusing the VHDX when it is newer than
/// the raw (same policy as the VirtualBox VDI path).
///
/// Prefers `qemu-img` (cross-platform); falls back to Hyper-V's
/// `Convert-VHD` via PowerShell on Windows. `label` is used in log lines.
fn raw_to_vhdx(raw: &str, vhdx: &str, label: &str, force: bool) -> Result<String, String> {
    let stale = if force {
        true
    } else if Path::new(&vhdx).exists() {
        match (
            std::fs::metadata(raw).and_then(|m| m.modified()).ok(),
            std::fs::metadata(&vhdx).and_then(|m| m.modified()).ok(),
        ) {
            (Some(r), Some(v)) => r > v,
            _ => true,
        }
    } else {
        true
    };

    if !stale {
        println!("Using existing {} VHDX disk: {}", label, vhdx);
        return Ok(vhdx.to_string());
    }

    if Command::new("qemu-img")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&vhdx);
        println!("Converting {} image to VHDX format...", label);
        let output = Command::new("qemu-img")
            .args(["convert", "-f", "raw", "-O", "vhdx", raw, vhdx])
            .output()
            .map_err(|e| format!("Failed to run qemu-img: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "qemu-img convert failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        println!("Created VHDX disk: {}", vhdx);
        return Ok(vhdx.to_string());
    }

    // Fallback on Windows: Hyper-V's Convert-VHD (needs admin + Hyper-V).
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "Convert-VHD -Path {} -DestinationPath {} -VHDType Dynamic; '{}'",
            ps_quote(raw),
            ps_quote(vhdx),
            "CONVERT_OK"
        );
        match run_powershell(&script) {
            Ok(_) => {
                println!("Created VHDX disk via Convert-VHD: {}", vhdx);
                return Ok(vhdx.to_string());
            }
            Err(e) => {
                return Err(format!(
                    "No VHDX at {} and conversion failed (qemu-img missing, Convert-VHD: {}).\n\
                     Convert once with: qemu-img convert -f raw -O vhdx {} {}",
                    vhdx, e, raw, vhdx
                ));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        return Err(format!(
            "No VHDX at {} and qemu-img is missing.\n\
             Convert once with: qemu-img convert -f raw -O vhdx {} {}\n\
             Or boot the ISO instead: --hyperv-boot=dvd (default).",
            vhdx, raw, vhdx
        ));
    }
}

/// Convert the UEFI raw image to VHDX for `--hyperv-boot=disk`.
fn ensure_hyperv_vhdx(uefi_img: &str, vhdx_arg: Option<&str>, force: bool) -> Result<String, String> {
    let vhdx = vhdx_arg
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}.vhdx", uefi_img));
    raw_to_vhdx(uefi_img, &vhdx, "UEFI", force)
}

/// Ensure the Hyper-V data-disk VHDX (`target/disk.vhdx`) from the raw
/// `target/disk.img`. The raw must already exist (created before bundling).
fn ensure_hyperv_data_vhdx(force: bool) -> Result<String, String> {
    let raw = "target/disk.img";
    if !Path::new(raw).exists() {
        return Err(format!(
            "Data disk raw {} missing (qemu-img failed to create it?).",
            raw
        ));
    }
    raw_to_vhdx(raw, "target/disk.vhdx", "data disk", force)
}

/// Ensure one VHDX per `--extra-disk=<raw>` (output: `<raw>.vhdx`).
fn ensure_hyperv_extra_vhdx(extra_disks: &[(String, String)], force: bool) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (raw, _) in extra_disks {
        if !Path::new(raw).exists() {
            return Err(format!("Extra disk raw {} missing.", raw));
        }
        let vhdx = format!("{}.vhdx", raw);
        out.push(raw_to_vhdx(raw, &vhdx, "extra disk", force)?);
    }
    Ok(out)
}

fn absolutize(p: &str) -> String {
    std::env::current_dir()
        .map(|c| c.join(p))
        .unwrap_or_else(|_| Path::new(p).to_path_buf())
        .to_string_lossy()
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn run_hyperv(
    iso_path: &str,
    uefi_img: &str,
    kernel_path: &Path,
    force: bool,
    boot: &str,
    switch: Option<&str>,
    mem_mb: u32,
    cpus: u32,
    vhdx_arg: Option<&str>,
    com: &str,
    _data_disk_size: &str,
    extra_disks: &[(String, String)],
) {
    let vm_name = format!(
        "MFK-{}-uefi",
        kernel_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kernel")
    );

    // Attach as absolute paths: Hyper-V resolves relative to
    // C:\Windows\System32 otherwise.
    let iso_abs = std::env::current_dir()
        .map(|p| p.join(iso_path))
        .unwrap_or_else(|_| Path::new(iso_path).to_path_buf());
    let iso_abs = iso_abs.to_string_lossy().to_string();

    // --------------------------------------------------------
    // Preflight: admin + Hyper-V module
    // --------------------------------------------------------
    let preflight = concat!(
        "$admin = ([Security.Principal.WindowsPrincipal]",
        "[Security.Principal.WindowsIdentity]::GetCurrent())",
        ".IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); ",
        "if (-not $admin) { Write-Error 'NOT_ADMIN'; exit 1 } ",
        "if (-not (Get-Module -ListAvailable -Name Hyper-V)) { Write-Error 'NO_MODULE'; exit 1 } ",
        "if (-not (Get-Command New-VM -ErrorAction SilentlyContinue)) { Write-Error 'NO_CMDLETS'; exit 1 } ",
        "'PREFLIGHT_OK'"
    );
    if let Err(e) = run_powershell(preflight) {
        if e.contains("NOT_ADMIN") {
            eprintln!("ERROR: Hyper-V needs an elevated shell.");
            eprintln!("Right-click PowerShell -> Run as administrator, then retry.");
        } else if e.contains("NO_MODULE") || e.contains("NO_CMDLETS") {
            eprintln!("ERROR: Hyper-V PowerShell module not found.");
            eprintln!("Enable Hyper-V: OptionalFeatures.exe -> Hyper-V, then reboot.");
        } else {
            eprintln!("ERROR: Hyper-V preflight failed: {}", e);
        }
        std::process::exit(1);
    }

    // --------------------------------------------------------
    // VHDX disks: boot image + data disk + extra disks
    //
    // Gen2 VMs have no IDE controller, so every disk is a SCSI VHDX:
    //   boot VHDX  = UEFI image converted (<uefi>.vhdx or --vhdx=)
    //   data VHDX  = target/disk.img  -> target/disk.vhdx
    //   extra VHDX = <raw>            -> <raw>.vhdx
    // Raws are created (qemu-img) before bundling in main(), so the
    // conversions below only need to copy raw -> VHDX.
    // --------------------------------------------------------
    let boot_vhdx_abs: Option<String> = if boot == "disk" {
        match ensure_hyperv_vhdx(uefi_img, vhdx_arg, force) {
            Ok(v) => Some(absolutize(&v)),
            Err(e) => {
                eprintln!("ERROR: {}", e);
                std::process::exit(1);
            }
        }
    } else if let Some(v) = vhdx_arg {
        // Explicit --vhdx with DVD boot: attach it as an extra data disk.
        let abs = absolutize(v);
        if !Path::new(&abs).is_file() {
            eprintln!("ERROR: VHDX not found at: {}", abs);
            std::process::exit(1);
        }
        Some(abs)
    } else {
        None
    };

    let data_vhdx_abs: String = match ensure_hyperv_data_vhdx(force) {
        Ok(v) => absolutize(&v),
        Err(e) => {
            eprintln!("ERROR: {}", e);
            eprintln!("Hint: install qemu-img (qemu-utils) or run with a pre-made target/disk.vhdx.");
            std::process::exit(1);
        }
    };

    let extra_vhdx_abs: Vec<String> = match ensure_hyperv_extra_vhdx(extra_disks, force) {
        Ok(v) => v.into_iter().map(|p| absolutize(&p)).collect(),
        Err(e) => {
            eprintln!("ERROR: {}", e);
            std::process::exit(1);
        }
    };

    // Attach order on the SCSI controller: boot VHDX first (so
    // FirstBootDevice=hard-disk is unambiguous), then data, then extras,
    // then an explicit --vhdx in DVD-boot mode.
    let mut scsi_vhdx: Vec<String> = Vec::new();
    if let Some(v) = boot_vhdx_abs.as_deref() {
        if boot == "disk" {
            scsi_vhdx.push(v.to_string());
        }
    }
    scsi_vhdx.push(data_vhdx_abs.clone());
    scsi_vhdx.extend(extra_vhdx_abs.clone());
    if boot != "disk" {
        if let Some(v) = boot_vhdx_abs.as_deref() {
            scsi_vhdx.push(v.to_string());
        }
    }

    // --------------------------------------------------------
    // Virtual switch
    // --------------------------------------------------------
    let switch_name = if let Some(s) = switch {
        let check = format!(
            "if (-not (Get-VMSwitch -Name {} -ErrorAction SilentlyContinue)) \
             {{ Write-Error 'NO_SWITCH'; exit 1 }} '{}'",
            ps_quote(s),
            "SWITCH_OK"
        );
        if let Err(e) = run_powershell(&check) {
            eprintln!("ERROR: virtual switch not found: {}", s);
            eprintln!("List switches: Get-VMSwitch   ({})", e);
            std::process::exit(1);
        }
        s.to_string()
    } else {
        // Prefer "Default Switch", else the first available switch.
        let find = concat!(
            "$sw = Get-VMSwitch -Name 'Default Switch' -ErrorAction SilentlyContinue; ",
            "if (-not $sw) { $sw = Get-VMSwitch -ErrorAction SilentlyContinue | Select-Object -First 1 } ",
            "if (-not $sw) { Write-Error 'NO_SWITCH'; exit 1 } ",
            "$sw.Name"
        );
        match run_powershell(find) {
            Ok(name) if !name.is_empty() => {
                println!("Using virtual switch: {} (auto-detected)", name);
                name
            }
            _ => {
                eprintln!("ERROR: No virtual switch found.");
                eprintln!("Create one: Hyper-V Manager -> Virtual Switch Manager,");
                eprintln!("or pass one explicitly: --hyperv-switch=\"<name>\"");
                std::process::exit(1);
            }
        }
    };

    // --------------------------------------------------------
    // Existing VM?
    // --------------------------------------------------------
    let vm_exists = run_powershell(&format!(
        "if (Get-VM -Name {} -ErrorAction SilentlyContinue) {{ 'YES' }} else {{ 'NO' }}",
        ps_quote(&vm_name)
    ))
    .map(|s| s == "YES")
    .unwrap_or(false);

    if vm_exists && force {
        println!("--force: removing existing VM {}", vm_name);
        let _ = run_powershell(&format!(
            "Stop-VM -Name {} -TurnOff -Force -ErrorAction SilentlyContinue; \
             Remove-VM -Name {} -Force",
            ps_quote(&vm_name),
            ps_quote(&vm_name)
        ));
    }

    let vm_exists = if vm_exists && force { false } else { vm_exists };

    if !vm_exists {
        println!("Creating Hyper-V VM: {} (Generation 2, UEFI)", vm_name);
        // NoVHD here: the boot device (DVD and/or VHDX) is attached below
        // so FirstBootDevice can point at the real device object.
        let create = format!(
            "New-VM -Name {} -Generation 2 -MemoryStartupBytes {}MB -NoVHD -SwitchName {} | Out-Null; \
             Set-VMProcessor -VMName {} -Count {}; \
             Set-VMFirmware -VMName {} -EnableSecureBoot Off; \
             '{}'",
            ps_quote(&vm_name),
            mem_mb,
            ps_quote(&switch_name),
            ps_quote(&vm_name),
            cpus,
            ps_quote(&vm_name),
            "VM_OK"
        );
        if let Err(e) = run_powershell(&create) {
            eprintln!("ERROR: Failed to create VM {}: {}", vm_name, e);
            std::process::exit(1);
        }
    } else {
        println!("Using existing VM: {}", vm_name);
        // Keep CPU/memory in sync on reuse.
        let _ = run_powershell(&format!(
            "Set-VMProcessor -VMName {} -Count {}; \
             Set-VMMemory -VMName {} -StartupBytes {}MB -ErrorAction SilentlyContinue; \
             Set-VMFirmware -VMName {} -EnableSecureBoot Off; \
             '{}'",
            ps_quote(&vm_name),
            cpus,
            ps_quote(&vm_name),
            mem_mb,
            ps_quote(&vm_name),
            "VM_OK"
        ));
    }

    // --------------------------------------------------------
    // COM1 serial (named pipe; Gen2 has no file-backed serial)
    // --------------------------------------------------------
    let com_pipe: Option<String> = if com == "off" {
        None
    } else if com.is_empty() {
        Some(format!("MFK-{}-com1", vm_name.trim_start_matches("MFK-")))
    } else {
        Some(com.trim_start_matches("\\\\.\\pipe\\").to_string())
    };
    if let Some(pipe) = com_pipe.as_deref() {
        let pipe_path = format!("\\\\.\\pipe\\{}", pipe);
        let com_script = format!(
            "Set-VMComPort -VMName {} -Number 1 -Path {}; '{}'",
            ps_quote(&vm_name),
            ps_quote(&pipe_path),
            "COM_OK"
        );
        if let Err(e) = run_powershell(&com_script) {
            eprintln!("Warning: could not configure COM1 pipe {}: {}", pipe_path, e);
        } else {
            println!("COM1 pipe: {}", pipe_path);
        }
    }

    // --------------------------------------------------------
    // Attach DVD (UEFI ISO) + all SCSI VHDX disks, set boot order
    // --------------------------------------------------------
    let dvd = format!(
        "$dvd = Get-VMDvdDrive -VMName {}; \
         if (-not $dvd) {{ Add-VMDvdDrive -VMName {} -Path {} }} \
         else {{ Set-VMDvdDrive -VMName {} -Path {} }}; \
         '{}'",
        ps_quote(&vm_name),
        ps_quote(&vm_name),
        ps_quote(&iso_abs),
        ps_quote(&vm_name),
        ps_quote(&iso_abs),
        "DVD_OK"
    );
    if let Err(e) = run_powershell(&dvd) {
        eprintln!("ERROR: Failed to attach UEFI ISO: {}", e);
        std::process::exit(1);
    }

    for vhdx in &scsi_vhdx {
        // Idempotent: only Add when this exact Path is not attached yet.
        // (Reused VMs keep old disks; --force recreates the VM above.)
        let disk = format!(
            "$exists = Get-VMHardDiskDrive -VMName {} | Where-Object {{ $_.Path -eq {} }}; \
             if (-not $exists) {{ Add-VMHardDiskDrive -VMName {} -Path {} }}; \
             '{}'",
            ps_quote(&vm_name),
            ps_quote(vhdx),
            ps_quote(&vm_name),
            ps_quote(vhdx),
            "DISK_OK"
        );
        if let Err(e) = run_powershell(&disk) {
            eprintln!("ERROR: Failed to attach VHDX {}: {}", vhdx, e);
            std::process::exit(1);
        }
    }
    println!("Attached {} SCSI VHDX disk(s).", scsi_vhdx.len());

    // Boot order: disk boots the first hard disk, dvd boots the ISO.
    let first = if boot == "disk" {
        format!("Get-VMHardDiskDrive -VMName {}", ps_quote(&vm_name))
    } else {
        format!("Get-VMDvdDrive -VMName {}", ps_quote(&vm_name))
    };
    if let Err(e) = run_powershell(&format!(
        "Set-VMFirmware -VMName {} -FirstBootDevice ({}); '{}'",
        ps_quote(&vm_name),
        first,
        "BOOT_OK"
    )) {
        eprintln!("Warning: could not set Hyper-V boot order: {}", e);
    }

    // --------------------------------------------------------
    // Start
    // --------------------------------------------------------
    println!();
    println!("Starting Hyper-V VM: {}", vm_name);
    println!("Firmware: UEFI (Generation 2, Secure Boot off)");
    println!("Boot:     {} ({})", boot.to_uppercase(), iso_abs);
    println!("Switch:   {}", switch_name);
    println!("Data:     {} + {} extra(s)", data_vhdx_abs, extra_vhdx_abs.len());
    println!("Connect:  Hyper-V Manager or vmconnect.exe");

    match run_powershell(&format!(
        "$vm = Get-VM -Name {}; \
         if ($vm.State -ne 'Running') {{ Start-VM -Name {} }}; \
         '{}'",
        ps_quote(&vm_name),
        ps_quote(&vm_name),
        "START_OK"
    )) {
        Ok(_) => {
            println!("VM started successfully!");
            println!("Stop it with: Stop-VM -Name {}", vm_name);
            if let Some(pipe) = com_pipe.as_deref() {
                println!(
                    "Serial: tools/hyperv-serial.ps1 -VMName {} -PipeName {} (log: target/mfk-hyperv-serial.log)",
                    vm_name, pipe
                );
            }
        }
        Err(e) => {
            eprintln!("ERROR: Failed to start VM: {}", e);
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
    if websocket_port == 5900 {
        5901
    } else {
        5900
    }
}

fn add_gpu_passthrough(qemu: &mut Command, gpu: Option<&GpuPassthrough>) {
    let Some(gpu) = gpu else {
        return;
    };

    qemu.args(["-vga", "none"]);
    let mut device = format!("vfio-pci,host={},x-vga=1", gpu.device);
    if let Some(rom) = &gpu.rom {
        device.push_str(",romfile=");
        device.push_str(rom);
    }
    qemu.args(["-device", &device]);
    if let Some(audio) = &gpu.audio {
        qemu.args(["-device", &format!("vfio-pci,host={}", audio)]);
    }
}

fn run_qemu_bios(
    bios_path: &str,
    bundle: bool,
    kbd: KbdMode,
    data_disk_size: &str,
    extra_disks: &[(String, String)],
    vnc: bool,
    vnc_port: u16,
    web_ui: bool,
    web_ui_port: u16,
    gpu_passthrough: Option<&GpuPassthrough>,
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
    println!("Networking: E1000 + user-mode NAT");
    if kbd.is_usb() {
        println!(
            "Input: {} USB keyboard + absolute tablet (PS/2 overridden)",
            kbd.name()
        );
    }

    let mut qemu = Command::new("qemu-system-x86_64");

    qemu.args([
        "-cpu",
        "max",
        "-drive",
        &format!("file={},format=raw,if=ide,index=0,media=disk", bios_path),
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

    if gpu_passthrough.is_some() {
        qemu.args(["-enable-kvm"]);
    }
    add_gpu_passthrough(&mut qemu, gpu_passthrough);

    if vnc {
        let tcp_port = qemu_vnc_tcp_port(vnc_port);
        let display = tcp_port - 5900;
        qemu.args(["-vnc", &format!(":{},websocket={}", display, vnc_port)]);
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

    attach_usb_input(&mut qemu, kbd);

    // Spawn web proxy before QEMU if web UI is enabled
    let mut web_proxy_child = None;
    if web_ui {
        let proxy_bin = PathBuf::from("target")
            .join("release")
            .join(format!("mfk_web_proxy{}", std::env::consts::EXE_SUFFIX));
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
                    .expect("Failed to start mfk_web_proxy"),
            );
        } else {
            eprintln!("Warning: mfk_web_proxy not found at {}. Run 'cargo build --release -p mfk-runner' to build it.", proxy_bin.display());
        }
    }

    let mut child = qemu.spawn().expect("Failed to start QEMU");

    child.wait().expect("Failed to wait on QEMU");

    // Clean up web proxy when QEMU exits
    if let Some(mut proxy) = web_proxy_child {
        let _ = proxy.kill();
        let  _ = proxy.wait();
    }
}

/// Attaches the emulated USB keyboard and absolute pointer to the host
/// controller selected by `--kbd`.
///
/// The controller gets an explicit id so its bus is predictably named
/// (`<id>.0`), which is what the usb-kbd/usb-tablet devices attach to. The
/// emulated usb-kbd overrides PS/2 in the guest, so input exercises the
/// matching HCD driver.
///
/// `pci-ohci` is a compile-time option in QEMU, so OHCI mode is best-effort:
/// if the device is unavailable QEMU exits with a clear error naming it.
fn attach_usb_input(qemu: &mut Command, kbd: KbdMode) {
    let Some((hcd, id)) = kbd.hcd_device() else {
        return;
    };
    let bus = format!("{id}.0");
    qemu.args(["-device", &format!("{hcd},id={id}")]);
    qemu.args(["-device", &format!("usb-kbd,bus={bus}")]);
    // Absolute pointer: the host cursor maps 1:1, so no GTK grab is needed.
    // The guest claims it like a USB tablet (see the xhci/uhci drivers).
    qemu.args(["-device", &format!("usb-tablet,bus={bus}")]);
}

// ------------------------------------------------------------
// QEMU UEFI
// ------------------------------------------------------------

fn run_qemu_uefi(
    uefi_path: &str,
    bundle: bool,
    kbd: KbdMode,
    data_disk_size: &str,
    extra_disks: &[(String, String)],
    vnc: bool,
    vnc_port: u16,
    web_ui: bool,
    web_ui_port: u16,
    gpu_passthrough: Option<&GpuPassthrough>,
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
    println!("Networking: E1000 + user-mode NAT");
    if kbd.is_usb() {
        println!(
            "Input: {} USB keyboard + absolute tablet ONLY",
            kbd.name()
        );
    } else {
        println!("Input: PS/2 keyboard");
    }

    let mut qemu = Command::new("qemu-system-x86_64");
    let ovmf_drive = format!("if=pflash,format=raw,readonly=on,file={}", ovmf.display());

    qemu.args([
        // Use the legacy PC machine because the kernel's disk driver uses
        // the legacy ATA PIO ports; Q35 exposes AHCI instead.
        "-machine",
        "pc",
        "-m",
        "4G",
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
        &format!("file={},format=raw,if=ide,index=0,media=disk", uefi_path),
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

    add_qemu_acceleration(&mut qemu);
    add_gpu_passthrough(&mut qemu, gpu_passthrough);

    if vnc {
        let tcp_port = qemu_vnc_tcp_port(vnc_port);
        let display = tcp_port - 5900;
        qemu.args(["-vnc", &format!(":{},websocket={}", display, vnc_port)]);
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

    attach_usb_input(&mut qemu, kbd);

    // Spawn web proxy before QEMU if web UI is enabled
    let mut web_proxy_child = None;
    if web_ui {
        let proxy_bin = PathBuf::from("target")
            .join("release")
            .join(format!("mfk_web_proxy{}", std::env::consts::EXE_SUFFIX));
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
                    .expect("Failed to start mfk_web_proxy"),
            );
        } else {
            eprintln!("Warning: mfk_web_proxy not found at {}. Run 'cargo build --release -p mfk-runner' to build it.", proxy_bin.display());
        }
    }

    let mut child = qemu.spawn().expect("Failed to start QEMU with OVMF");

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

fn create_qemu_data_disk(disk_path: &str, size: &str) {
    if !Path::new(disk_path).exists() {
        println!("Creating virtual data disk: {} ({})", disk_path, size);

        let result = Command::new("qemu-img")
            .args(["create", "-f", "raw", disk_path, size])
            .output();

        match result {
            Ok(output) => {
                if !output.status.success() {
                    eprintln!("Warning: Failed to create data disk.");

                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                }
            }

            Err(e) => {
                eprintln!("Warning: Failed to execute qemu-img: {}", e);
            }
        }
    } else {
        println!("Using existing data disk: {}", disk_path);
    }
}

/// Creates missing extra data disks (secondary IDE channel). Existing files
/// are reused regardless of the requested size.
fn ensure_extra_disks(extra_disks: &[(String, String)]) {
    for (i, (path, size)) in extra_disks.iter().enumerate() {
        if Path::new(path).exists() {
            println!("Using existing extra disk #{}: {}", i + 1, path);
            continue;
        }

        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        println!("Creating extra disk #{}: {} ({})", i + 1, path, size);

        match Command::new("qemu-img")
            .args(["create", "-f", "raw", path, size.as_str()])
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
                eprintln!("Warning: Failed to execute qemu-img for {}: {}", path, e);
            }
        }
    }
}

fn bundle_qemu_examples(disk_path: &str) {
    let examples = Path::new("apps/examples");

    if !examples.exists() {
        return;
    }

    println!("Bundling example apps into {}...", disk_path);

    if let Err(e) = simplfs_host::bundle_examples(Path::new(disk_path), examples) {
        eprintln!("Bundle failed: {}", e);
    }
}

// ------------------------------------------------------------
// Usage
// ------------------------------------------------------------

fn print_usage(program: &str) {
    eprintln!("Usage: {} <kernel-binary-path> [OPTIONS]", program);

    eprintln!();

    eprintln!("OPTIONS:");

    eprintln!("  --vbox, --virtualbox     Run in VirtualBox (default)");

    eprintln!("  --qemu                   Run in QEMU");

    eprintln!("  --hyperv, --hyper-v, --hv");
    eprintln!("                           Run in Hyper-V (Windows, Generation 2 / UEFI only)");

    eprintln!("  --hyperv-boot=<dvd|disk>  Hyper-V boot device (default dvd: the UEFI ISO)");

    eprintln!("  --vhdx=<path>            VHDX for --hyperv-boot=disk (auto-converted via");
    eprintln!("                           qemu-img/Convert-VHD when missing) or extra data disk with dvd boot");

    eprintln!("  --hyperv-switch=<name>    Virtual switch (default: Default Switch, auto-detected)");

    eprintln!("  --hyperv-mem=<MB>         VM startup memory in MB (default 512)");

    eprintln!("  --hyperv-cpus=<n>         Virtual CPUs (default 2)");

    eprintln!("  --hyperv-com=<pipe|off>   COM1 named pipe (default MFK-<vm>-com1; off disables)");
    eprintln!("                           Capture with: tools/hyperv-serial.ps1");

    eprintln!("  --bios                   Use BIOS firmware (default)");

    eprintln!("  --uefi                   Use UEFI firmware");

    eprintln!("  --no-run                 Only create disk images");

    eprintln!("  --no-iso                 Skip UEFI ISO creation (Hyper-V DVD boot needs it)");

    eprintln!("  --force                  Force rebuild VDI/VM");

    eprintln!("  --bundle-apps, --with-apps");

    eprintln!("                           Bundle apps/examples into target/disk.img");
    eprintln!("                           (Hyper-V: bundled before raw->VHDX conversion)");

    eprintln!("  --wad=<host-wad>         Inject a Doom WAD into its own disk image");
    eprintln!("  --wad-disk=<img>         WAD disk image (default target/doom.img)");
    eprintln!("  --wad-disk-size=<size>   Size when creating WAD disk (default 128M)");
    eprintln!("  --wad-guest=<path>       Guest path (default /wad/doom1.wad)");
    eprintln!("                           Then attach with --extra-disk=target/doom.img");

    eprintln!("  --kbd=<mode>             QEMU keyboard transport (default ps2)");
    eprintln!("                            ps2 | xhci | ehci | uhci | ohci");
    eprintln!("                            USB modes attach qemu-xhci / usb-ehci /");
    eprintln!("                            piix3-usb-uhci / pci-ohci plus usb-kbd");
    eprintln!("                            and usb-tablet, overriding PS/2.");

    eprintln!("  --xhci-kbd               Attach qemu-xhci + usb-kbd to QEMU");

    eprintln!("  --data-disk-size=<size>   Size for target/disk.img (default 10M)");

    eprintln!(
        "  --extra-disk=<path>       Extra data disk (repeatable, max 8; QEMU: first 2 IDE rest virtio-blk;"
    );
    eprintln!("                           Hyper-V: each raw converted to <raw>.vhdx on SCSI)");

    eprintln!("  --extra-disk-size=<size>  Size for preceding --extra-disk (default 64M)");

    eprintln!("  --boot-extra-disk[=N]     Boot extra disk N (1-based, bare = first)");

    eprintln!("  --vnc                    Enable QEMU VNC server");

    eprintln!("  --vnc-port=<port>         VNC WebSocket port (default 5900)");

    eprintln!("  --web-ui                 Launch noVNC web UI (implies --vnc)");

    eprintln!("  --web-ui-port=<port>      Web UI HTTP port (default 8084)");

    eprintln!("  --gpu-passthrough=<BDF>   Pass through a PCI GPU with vfio-pci");

    eprintln!("  --gpu-audio=<BDF>          Pass through the GPU audio function");

    eprintln!("  --gpu-rom=<path>           Use a verified GPU ROM with vfio-pci");

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
        "  {} target/x86_64-mfk/debug/mfk-kernel --hyperv --uefi",
        program
    );

    eprintln!(
        "  {} target/x86_64-mfk/debug/mfk-kernel --hyperv --hyperv-boot=disk",
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

    eprintln!("  {} target/x86_64-mfk/debug/mfk-kernel --no-run", program);

    eprintln!();

    eprintln!("UEFI:");

    eprintln!("  Install OVMF on Debian/Ubuntu:");

    eprintln!("    sudo apt install ovmf");

    eprintln!("  Or specify firmware manually:");

    eprintln!(
        "    OVMF_CODE=/path/to/OVMF_CODE.fd {} target/x86_64-mfk/debug/mfk-kernel --qemu --uefi",
        program
    );
}
