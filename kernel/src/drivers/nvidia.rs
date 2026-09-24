use crate::drivers::pci::{PciDevice, PCI_VENDOR_NVIDIA};

const DEVICE_ID: u16 = 0x2488;
const SUBSYSTEM_VENDOR_ID: u16 = 0x1043;
const SUBSYSTEM_ID: u16 = 0x8825;
const NV_PMC_BOOT_0: usize = 0x0000;
const NV_PMC_BOOT_42: usize = 0x0A00;
const BAR0_PROBE_SIZE: usize = 0x1000;

fn read_mmio32(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

fn print_capabilities(pci: &PciDevice) {
    for (id, name) in [(0x05, "MSI"), (0x10, "PCIe"), (0x11, "MSI-X")] {
        if let Some(capability) = pci.capability(id) {
            crate::serial_println!("[nvidia] capability {} at {:#04x}", name, capability.offset);
        } else {
            crate::serial_println!("[nvidia] capability {} absent", name);
        }
    }
}

fn print_bars(pci: &PciDevice) {
    let mut index = 0;
    while index < pci.bars.len() {
        let raw = pci.bar(index).unwrap_or(0);
        if raw == 0 {
            index += 1;
            continue;
        }
        let base = pci.bar_base(index).unwrap_or(0);
        let kind = if pci.bar_is_io(index) { "io" } else { "mem" };
        crate::serial_println!(
            "[nvidia] BAR{} raw={:#010x} base={:#018x} type={} 64bit={} prefetch={}",
            index,
            raw,
            base,
            kind,
            pci.bar_is_64bit(index),
            pci.bar_is_prefetchable(index)
        );
        index += if pci.bar_is_64bit(index) { 2 } else { 1 };
    }
}

fn print_boot_registers(base: usize) {
    let boot0 = read_mmio32(base, NV_PMC_BOOT_0);
    let boot42 = read_mmio32(base, NV_PMC_BOOT_42);
    crate::serial_println!("[nvidia] BOOT0={:#010x}", boot0);
    crate::serial_println!("[nvidia] BOOT42={:#010x}", boot42);
}

pub fn probe(phys_mem_offset: u64) {
    let devices = crate::drivers::pci::enumerate_devices();
    let mut candidates = 0;
    let mut exact = 0;

    for pci in devices.iter() {
        if pci.vendor_id != PCI_VENDOR_NVIDIA || pci.device_id != DEVICE_ID {
            continue;
        }
        candidates += 1;
        crate::serial_println!(
            "[nvidia] candidate {:02x}:{:02x}.{} revision={:02x} class={:02x}{:02x}{:02x}",
            pci.bus,
            pci.device,
            pci.function,
            pci.revision_id,
            pci.class_code,
            pci.subclass,
            pci.prog_if
        );
        crate::serial_println!(
            "[nvidia] subsystem {:04x}:{:04x}",
            pci.subsystem_vendor_id,
            pci.subsystem_id
        );

        if pci.subsystem_vendor_id != SUBSYSTEM_VENDOR_ID || pci.subsystem_id != SUBSYSTEM_ID {
            crate::serial_println!("[nvidia] subsystem does not match RTX 3070 target");
            continue;
        }

        exact += 1;
        print_capabilities(pci);
        print_bars(pci);

        let Some(bar0) = pci.bar_base(0) else {
            crate::serial_println!("[nvidia] BAR0 is not a memory BAR");
            continue;
        };
        if bar0 == 0 {
            crate::serial_println!("[nvidia] BAR0 is unassigned");
            continue;
        }

        let Some(virt) = crate::drivers::pci::map_mmio_region(
            bar0,
            BAR0_PROBE_SIZE,
            phys_mem_offset,
            &mut crate::memory::frame_allocator::frame_allocator(),
        ) else {
            crate::serial_println!("[nvidia] BAR0 mapping failed");
            continue;
        };

        print_boot_registers(virt);
    }

    if candidates == 0 {
        crate::serial_println!("[nvidia] RTX 3070 target not found");
    } else if exact == 0 {
        crate::serial_println!("[nvidia] no matching RTX 3070 subsystem");
    } else {
        crate::serial_println!("[nvidia] read-only probe passed");
    }
}
