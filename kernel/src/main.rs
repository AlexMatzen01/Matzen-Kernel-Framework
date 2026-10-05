//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Matzen Kernel Framework - A simple terminal OS
//!
//! This kernel provides a basic terminal interface that runs on bare metal x86_64.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod allocator;
mod app;
mod archive;
mod desktop;
mod doom;
mod drivers;
mod entropy;
mod editor;
mod fs;
mod install;
mod interrupts;
mod memory;
mod net;
mod pic;
mod shell;
mod sysinfo;
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
    // Single place the direct-map arithmetic lives; drivers read it instead of
    // re-deriving the offset inline.
    memory::addr::set_direct_map_offset(phys_mem_offset);

    // Record the bootloader's physical-memory direct map so PCI drivers can
    // tell a BAR that is already reachable at `offset + phys` from one that
    // needs fresh page tables (QEMU/OVMF puts xHCI at 56 TiB, far outside the
    // mapped range).
    //
    // The mapped range is `max(highest region end, 4 GiB)`: the bootloader
    // guarantees at least 4 GiB so MMIO regions stay reachable even on a
    // small guest (bootloader_api `BootloaderConfig::mappings.physical_memory`
    // docs). Using the region maximum alone makes every sub-4 GiB BAR look
    // unmapped on a 128 MiB guest, so each one takes the page-table path and
    // leaks page-table frames for a mapping that already exists.
    let phys_map_end = boot_info
        .memory_regions
        .iter()
        .map(|r| r.end)
        .max()
        .unwrap_or(0)
        .max(memory::memmap::DMA_PHYS_LIMIT);
    drivers::pci::set_phys_window(phys_mem_offset, phys_map_end);

    // Initialize VGA text mode with proper memory mapping
    drivers::vga::init_with_offset(phys_mem_offset);
    serial_println!("VGA initialized");

    // Initialize heap allocator (required before fb init: fb uses alloc).
    // Carve the heap from the memory map instead of storing it in .bss to
    // keep the kernel ELF compact and the carved range physically mapped.
    let (heap_phys, heap_size) =
        allocator::carve_heap_run(&boot_info.memory_regions, allocator::HEAP_TOTAL_SIZE as u64)
            .unwrap_or((0, 0));
    // Minimum carved heap worth using; below this, boot on the 1 MiB
    // static fallback instead (large images then fail cleanly via caps).
    const MIN_CARVED_HEAP: u64 = 4 * 1024 * 1024;
    if heap_size >= MIN_CARVED_HEAP {
        let heap_virt = phys_mem_offset.wrapping_add(heap_phys) as *mut u8;
        allocator::init(heap_virt, heap_size as usize);
        serial_println!(
            "Heap allocator initialized: {} KiB at phys {:#x} ({} KiB emergency reserve)",
            heap_size / 1024,
            heap_phys,
            allocator::HEAP_RESERVE_SIZE / 1024
        );
        if heap_size < allocator::HEAP_TOTAL_SIZE as u64 {
            serial_println!("Warning: heap below full budget; large allocations may fail");
        }
    } else {
        allocator::init_fallback();
        serial_println!("Warning: no large usable run below 4 GiB; 1 MiB fallback heap in use");
    }

    // Interpret the firmware map: classify every region and subtract the
    // spans the kernel itself occupies. This needs the heap (the layout owns
    // Vecs), so it comes after allocator init.
    //
    // Reservations recorded here are the ones the firmware does *not* mark
    // reserved for us: the kernel ELF and its `.bss`, a ramdisk, and the
    // linear framebuffer. The bootloader stack and its own page tables are
    // already tagged `Bootloader`, and the heap is in `.bss`-adjacent
    // firmware-visible memory we just carved, so both are added explicitly.
    let mut reservations: [memory::memmap::Reservation; 4] = [memory::memmap::Reservation {
        start: 0,
        end: 0,
        kind: memory::memmap::ResKind::Manual,
    }; 4];
    let mut reservation_count = 0usize;
    if let Some(r) = memory::memmap::Reservation::new(
        boot_info.kernel_addr,
        boot_info.kernel_addr.saturating_add(boot_info.kernel_len),
        memory::memmap::ResKind::KernelImage,
    ) {
        reservations[reservation_count] = r;
        reservation_count += 1;
    }
    if boot_info.ramdisk_len > 0 {
        if let Some(addr) = boot_info.ramdisk_addr.into_option() {
            if let Some(r) = memory::memmap::Reservation::new(
                addr,
                addr.saturating_add(boot_info.ramdisk_len),
                memory::memmap::ResKind::RamDisk,
            ) {
                reservations[reservation_count] = r;
                reservation_count += 1;
            }
        }
    }
    // Framebuffer extent. `FrameBuffer` exposes no physical-base accessor,
    // but `buffer()` is a slice over exactly that span, so its pointer is the
    // base and `info().byte_len()` is the size.
    let fb_extent = boot_info.framebuffer.as_ref().and_then(|fb| {
        memory::memmap::Reservation::new(
            fb.buffer().as_ptr() as u64,
            (fb.buffer().as_ptr() as u64).saturating_add(fb.info().byte_len as u64),
            memory::memmap::ResKind::FrameBuffer,
        )
    });
    if let Some(r) = fb_extent {
        reservations[reservation_count] = r;
        reservation_count += 1;
    }
    if heap_size >= MIN_CARVED_HEAP {
        if let Some(r) = memory::memmap::Reservation::new(
            heap_phys,
            heap_phys.saturating_add(heap_size),
            memory::memmap::ResKind::Heap,
        ) {
            reservations[reservation_count] = r;
            reservation_count += 1;
        }
    }
    let reservations = &reservations[..reservation_count];
    let layout = memory::memmap::build(&boot_info.memory_regions, reservations);
    serial_println!(
        "[mem] Map: {} MiB total, {} MiB allocatable ({} MiB below 4 GiB), {} MiB kernel-owned, {}{}",
        layout.total_bytes / (1024 * 1024),
        layout.allocatable_bytes / (1024 * 1024),
        layout.allocatable_low_bytes / (1024 * 1024),
        layout.boot_owned_bytes / (1024 * 1024),
        layout.regions.len(),
        if layout.truncated { " (list truncated)" } else { "" }
    );
    sysinfo::stash_memory_layout(&layout);

    // Physical frames come from the reservation-free spans, never from the
    // raw firmware map: handing out the raw map would let a MMIO window
    // map a page table over the kernel image or the heap.
    let spans = memory::memmap::allocatable_spans(&layout);
    memory::frame_allocator::init_from_spans(&spans);
    serial_println!(
        "Frame allocator initialized: {} free frames",
        memory::frame_allocator::free_frames()
    );

    // Record how wide physical addressing this CPU actually implements.
    // Nothing else in the kernel knows the ceiling, so a 64-bit PCI BAR above
    // it has no way to be detected as unreachable.
    match crate::sysinfo::cpu_max_phys_addr_bits() {
        Some(bits) => serial_println!(
            "Physical address width: {} bits (max address {:#x})",
            bits,
            crate::sysinfo::cpu_max_physical_address().unwrap_or(0)
        ),
        None => serial_println!(
            "Physical address width: unreported, assuming 40 bits (max address {:#x})",
            crate::sysinfo::cpu_phys_addr_limit()
        ),
    }

    // SMBIOS inventory: what the RAM physically *is*, which the memory map
    // alone cannot say. The entry point is searched for the way the SMBIOS
    // specification prescribes rather than assumed to live in the RSDP.
    let rsdp = boot_info.rsdp_addr.into_option();
    let address_limit = crate::sysinfo::cpu_phys_addr_limit();
    match drivers::smbios::locate(phys_mem_offset, rsdp, address_limit) {
        Ok((inv, where_)) => {
            serial_println!(
                "[smbios] found via {}: {} array(s), {} slot(s), {} MiB installed{}",
                where_.label(),
                inv.arrays.len(),
                inv.devices.len(),
                inv.installed_mb(),
                if inv.truncated { ", table truncated" } else { "" }
            );
            drivers::smbios::stash(inv);
        }
        Err(e) => {
            serial_println!("[smbios] {}", e);
            drivers::smbios::stash(drivers::smbios::MemoryInventory::default());
        }
    }

    // Attach UEFI GOP framebuffer when present so `println!` reaches the
    // display. On BIOS there is no framebuffer and VGA text mode is used.
    // Must run after allocator init; take() leaves `Optional::None` behind.
    if let Some(fb) = boot_info.framebuffer.take() {
        drivers::fb::init(fb);
        serial_println!("Framebuffer console initialized");
        // Give the desktop an off-screen buffer so window updates are published
        // in one pass instead of streaming into live scanout (which made every
        // repaint visibly sweep from the top of the damage rect downwards).
        // Needs the heap, so it must come after `fb::init`. If the allocation
        // fails we log it and drawing stays direct â€” degraded, not broken.
        if drivers::fb_gfx::init_back_buffer() {
            serial_println!("Desktop back buffer active (single-copy present)");
        } else {
            serial_println!("Warning: no back buffer; window updates may tear");
        }
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

    macro_rules! boot {
        ($($arg:tt)*) => { println!("[boot] {}", format_args!($($arg)*)); };
    }
    boot!("welcome printed");

    serial_println!("Welcome message printed");

    // Initialize interrupt handling
    interrupts::init_idt();
    serial_println!("IDT initialized");
    boot!("IDT initialized");

    // Start the monotonic kernel clock and 100 Hz PIT before enabling IRQ0.
    time::init();
    drivers::pit::init_default();
    boot!("time + PIT initialized");

    // Select the unified time/delay source (PIT/PM-Timer/TSC) before any
    // driver can call delay_ms(); the xHCI init path relies on it.
    drivers::time_source::select_at_boot(phys_mem_offset);

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
    boot!("PIC initialized");

    // Initialize keyboard driver (PS/2 + serial)
    boot!("keyboard init start");
    drivers::keyboard::init();
    serial_println!("Keyboard initialized");
    boot!("keyboard initialized");

    // Initialize PS/2 mouse (fail-open: logs and continues when absent).
    boot!("mouse init start");
    drivers::mouse::init();
    if drivers::mouse::is_present() {
        serial_println!("Mouse initialized (PS/2, IRQ12)");
    } else {
        serial_println!("Mouse absent (desktop cursor will not move)");
    }
    boot!("mouse init done");

    // Initialize USB (EHCI/xHCI) for USB keyboards, mice and tablets,
    // including QEMU `qemu-xhci + usb-kbd + usb-tablet`. Bounded init:
    // never hangs when absent.
    boot!("usb init start");
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
    boot!("usb init done");

    // Initialize ATA disk driver
    boot!("ata init start");
    drivers::ata::init();
    boot!("ata init done");

    // Initialize virtio-blk disks (extras beyond the 4 IDE slots land on
    // unified drive indices 4+; needs the `usb` feature for DMA helpers).
    #[cfg(feature = "usb")]
    drivers::virtio_blk::init();
    #[cfg(not(feature = "usb"))]
    serial_println!("virtio-blk not compiled in (ATA-only drives)");
    boot!("virtio-blk init done");

    // Initialize networking (E1000 NIC)
    boot!("e1000 init start");
    if let Err(e) = drivers::e1000::init(phys_mem_offset) {
        serial_println!("Warning: E1000 initialization failed: {}", e);
    } else {
        net::init();
    }
    boot!("e1000 init done");

    boot!("nvidia probe start");
    drivers::nvidia::probe(phys_mem_offset);
    boot!("nvidia probe done");

    // Enable interrupts
    x86_64::instructions::interrupts::enable();
    serial_println!("Interrupts enabled");
    boot!("interrupts enabled");

    // Start the shell
    serial_println!("Starting shell...");
    boot!("starting shell");
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
