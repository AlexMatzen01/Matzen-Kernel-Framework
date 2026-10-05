//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Basic PCI device enumeration
//!
//! Provides simple PCI configuration space access

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::mapper::{MappedPageTable, Mapper, PageTableFrameMapping};
use x86_64::structures::paging::{
    Page, PageTable, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::VirtAddr;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

pub const PCI_VENDOR_INTEL: u16 = 0x8086;
pub const PCI_VENDOR_NVIDIA: u16 = 0x10DE;

pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub revision_id: u8,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bar0: u32,
    pub bar1: u32,
    pub bars: [u32; 6],
    pub irq_line: u8,
}

fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x80000000u32
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn read_config_at(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    unsafe {
        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
        let mut data_port = Port::<u32>::new(CONFIG_DATA);
        addr_port.write(config_address(bus, device, function, offset));
        data_port.read()
    }
}

impl PciDevice {
    pub fn read_config(&self, offset: u8) -> u32 {
        read_config_at(self.bus, self.device, self.function, offset)
    }

    pub fn from_address(bus: u8, device: u8, function: u8) -> Option<Self> {
        let vendor_device = read_config_at(bus, device, function, 0x00);
        let vendor_id = (vendor_device & 0xFFFF) as u16;
        if vendor_id == 0xFFFF {
            return None;
        }

        let class_info = read_config_at(bus, device, function, 0x08);
        let subsystem_info = read_config_at(bus, device, function, 0x2C);
        let mut bars = [0u32; 6];
        for (index, bar) in bars.iter_mut().enumerate() {
            *bar = read_config_at(bus, device, function, 0x10 + (index as u8 * 4));
        }

        Some(Self {
            bus,
            device,
            function,
            vendor_id,
            device_id: (vendor_device >> 16) as u16,
            subsystem_vendor_id: (subsystem_info & 0xFFFF) as u16,
            subsystem_id: (subsystem_info >> 16) as u16,
            revision_id: (class_info & 0xFF) as u8,
            class_code: ((class_info >> 24) & 0xFF) as u8,
            subclass: ((class_info >> 16) & 0xFF) as u8,
            prog_if: ((class_info >> 8) & 0xFF) as u8,
            bar0: bars[0],
            bar1: bars[1],
            bars,
            irq_line: (read_config_at(bus, device, function, 0x3C) & 0xFF) as u8,
        })
    }
}

pub fn enumerate_devices() -> alloc::vec::Vec<PciDevice> {
    let mut devices = alloc::vec::Vec::new();

    for bus in 0..=255u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                if let Some(pci) = PciDevice::from_address(bus, device, function) {
                    devices.push(pci);
                }
            }
        }
    }

    devices
}

/// Enumerate all PCI functions on a single bus (used by virtio-blk
/// hot-add detection; see `ata::rescan_silent`).
pub fn enumerate_bus(bus: u8) -> alloc::vec::Vec<PciDevice> {
    let mut devices = alloc::vec::Vec::new();
    for device in 0..32u8 {
        for function in 0..8u8 {
            if let Some(pci) = PciDevice::from_address(bus, device, function) {
                devices.push(pci);
            }
        }
    }
    devices
}

pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                if let Some(pci) = PciDevice::from_address(bus, device, function) {
                    if pci.vendor_id == vendor_id && pci.device_id == device_id {
                        return Some(pci);
                    }
                }
            }
        }
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciCapability {
    pub id: u8,
    pub offset: u8,
}

impl PciDevice {
    pub fn bar(&self, index: usize) -> Option<u32> {
        self.bars.get(index).copied()
    }

    pub fn bar_base(&self, index: usize) -> Option<u64> {
        let raw = self.bar(index)?;
        if raw & 0x01 != 0 {
            return None;
        }
        if raw & 0x06 == 0x04 {
            let high = self.bar(index + 1)?;
            Some(((raw & 0xFFFF_FFF0) as u64) | ((high as u64) << 32))
        } else {
            Some((raw & 0xFFFF_FFF0) as u64)
        }
    }

    pub fn bar_is_io(&self, index: usize) -> bool {
        self.bar(index).map(|raw| raw & 0x01 != 0).unwrap_or(true)
    }

    pub fn bar_is_64bit(&self, index: usize) -> bool {
        self.bar(index)
            .map(|raw| raw & 0x06 == 0x04)
            .unwrap_or(false)
    }

    pub fn bar_is_prefetchable(&self, index: usize) -> bool {
        self.bar(index).map(|raw| raw & 0x08 != 0).unwrap_or(false)
    }

    pub fn capability(&self, id: u8) -> Option<PciCapability> {
        let status = self.read_config(0x06);
        if status & (1 << 20) == 0 {
            return None;
        }

        let mut offset = (self.read_config(0x34) & 0xFF) as u8;
        let mut visited = 0;
        while offset >= 0x40 && visited < 48 {
            visited += 1;
            let value = self.read_config(offset);
            let shift = (offset & 0x03) * 8;
            let capability_id = ((value >> shift) & 0xFF) as u8;
            let next = ((value >> (shift + 8)) & 0xFF) as u8;
            if capability_id == id {
                return Some(PciCapability {
                    id: capability_id,
                    offset,
                });
            }
            if next == offset || next < 0x40 {
                break;
            }
            offset = next;
        }
        None
    }

    pub fn is_xhci(&self) -> bool {
        self.class_code == 0x0c && self.subclass == 0x03 && self.prog_if == 0x30
    }

    /// Returns true for USB EHCI controllers (prog-if 0x20).
    pub fn is_ehci(&self) -> bool {
        self.class_code == 0x0c && self.subclass == 0x03 && self.prog_if == 0x20
    }

    /// Returns true for USB UHCI controllers (prog-if 0x00).
    pub fn is_uhci(&self) -> bool {
        self.class_code == 0x0c && self.subclass == 0x03 && self.prog_if == 0x00
    }

    /// Returns true for USB OHCI controllers (prog-if 0x10).
    pub fn is_ohci(&self) -> bool {
        self.class_code == 0x0c && self.subclass == 0x03 && self.prog_if == 0x10
    }

    /// Base of an I/O-space BAR, if it is one. UHCI/OHCI are I/O-space
    /// controllers, so `bar_base` deliberately rejects them (it requires
    /// bit 0 clear, i.e. memory space). Returns the dword-aligned port
    /// window. `None` for memory BARs and for unprogrammed (base zero) BARs.
    pub fn bar_io_base(&self, index: usize) -> Option<u16> {
        let raw = self.bar(index)?;
        if raw & 0x01 == 0 {
            return None; // memory BAR
        }
        let base = (raw & 0xFFFF_FFFC) as u16;
        if base == 0 {
            return None; // firmware left it unassigned
        }
        Some(base)
    }

    /// Raw I/O BAR base including the low unaligned bits, for diagnostics.
    pub fn bar_io_raw(&self, index: usize) -> Option<u32> {
        let raw = self.bar(index)?;
        if raw & 0x01 == 0 {
            return None;
        }
        Some(raw)
    }

    pub fn mmio_base(&self) -> Option<u64> {
        self.bar_base(0)
    }

    /// Enable I/O space, memory space, and bus mastering in PCI COMMAND.
    pub fn enable_bus_mastering(&self) {
        let cmd = self.read_config(0x04);
        self.write_config(0x04, cmd | 0x07);
    }

    /// Re-read the vendor ID from hardware (fresh check, not the cached copy).
    pub fn reread_vendor_id(&self) -> u16 {
        (self.read_config(0x00) & 0xFFFF) as u16
    }

    /// Legacy INTx interrupt pin and line assigned by the firmware.
    ///
    /// Offset 0x3C holds the interrupt pin (INTA#..INTD#, 1-based) in bits 15:8
    /// and the interrupt line in bits 7:0. A pin of 0 means the function has no
    /// INTx pin wired, which is what a device routed to MSI-X reports.
    ///
    /// Returns `(pin, line)` with the pin still 1-based, matching the register.
    pub fn interrupt_pin_line(&self) -> Option<(u8, u8)> {
        let value = self.read_config(0x3C);
        let pin = ((value >> 8) & 0xFF) as u8;
        let line = (value & 0xFF) as u8;
        if pin == 0 || line == 0 || line >= 32 {
            return None;
        }
        Some((pin, line))
    }

    /// Write to PCI config space.
    pub fn write_config(&self, offset: u8, val: u32) {
        let address = 0x80000000u32
            | ((self.bus as u32) << 16)
            | ((self.device as u32) << 11)
            | ((self.function as u32) << 8)
            | ((offset as u32) & 0xFC);

        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);

            addr_port.write(address);
            data_port.write(val);
        }
    }

    /// Power-management info: walks the capability list for the PM cap (ID 0x01)
    /// and returns `(cap_offset, power_state_D0-D3)`.
    pub fn pm_info(&self) -> Option<(u8, u8)> {
        let status = self.read_config(0x06);
        if status & (1 << 20) == 0 {
            return None; // No capability list.
        }
        let mut cap = (self.read_config(0x34) & 0xFF) as u8;
        let mut guard = 0;
        while cap != 0 && guard < 16 {
            guard += 1;
            let (id, next) = {
                let v = self.read_config(cap & 0xFC);
                let shift = (cap & 0x03) * 8;
                (
                    ((v >> shift) & 0xFF) as u8,
                    ((v >> (shift + 8)) & 0xFF) as u8,
                )
            };
            if id == 0x01 {
                let pmcs = self.read_config((cap + 4) & 0xFC);
                let shift = ((cap + 4) & 0x03) * 8;
                return Some((cap, ((pmcs >> shift) & 0x03) as u8));
            }
            cap = next;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// MMIO mapping
// ---------------------------------------------------------------------------

/// Offset the bootloader direct-maps physical memory at (`Mapping::Dynamic`).
static PHYS_MAP_OFFSET: AtomicU64 = AtomicU64::new(0);
/// End of the physical range the bootloader actually mapped at that offset.
///
/// The mapped range is `[0, max(max_region_end, 4 GiB))`: the bootloader
/// guarantees at least 4 GiB is mapped so MMIO regions (local APIC, I/O APIC,
/// PCI BARs) stay reachable even when the machine has less RAM than that (see
/// `BootloaderConfig::mappings.physical_memory`). Using the region maximum
/// alone makes every sub-4 GiB BAR look unmapped on a small guest, so each one
/// takes the page-table path and leaks page-table frames for a mapping that
/// already exists.
///
/// A PCI BAR can still sit far above this (QEMU+OVMF hand the xHCI controller
/// a 64-bit BAR at 56 TiB), so this bound is what decides whether the direct
/// map can be reused or new page tables are required.
static PHYS_MAP_END: AtomicU64 = AtomicU64::new(0);

/// Serializes page-table edits. Boot is effectively single-threaded, but the
/// level-4 scan plus entry install must not interleave with another mapper
/// (the frame allocator locks on its own; this covers the table walk).
static MMIO_MAP_LOCK: Mutex<()> = Mutex::new(());

/// Leaf flags for device MMIO.
///
/// `NO_EXECUTE` is safe because `bootloader-x86_64_common` calls
/// `enable_nxe_bit()` before handing control to the kernel. `WRITE_THROUGH |
/// NO_CACHE` selects PAT slot PA7 = UC under the default PAT MSR that the
/// bootloader leaves untouched, which is the correct memory type for device
/// registers (uncacheable on real silicon; ignored by QEMU's TCG).
const MMIO_LEAF_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::WRITE_THROUGH)
    .union(PageTableFlags::NO_CACHE)
    .union(PageTableFlags::NO_EXECUTE);

/// Flags for the intermediate level-3/2/1 tables holding MMIO mappings.
const MMIO_TABLE_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::NO_EXECUTE);

/// Records the bootloader's physical-memory direct map so [`map_mmio_region`]
/// can tell "already reachable as `offset + phys`" from "needs page tables".
///
/// Call once from `kernel_main` with the offset taken from `BootInfo` and the
/// highest end address of `BootInfo::memory_regions`. An over-estimate is
/// safe (it only costs a redundant mapping); an under-estimate would send a
/// reachable BAR down the page-table path, which is still correct.
pub fn set_phys_window(offset: u64, max_end: u64) {
    PHYS_MAP_OFFSET.store(offset, Ordering::Relaxed);
    PHYS_MAP_END.store(max_end, Ordering::Relaxed);
    crate::serial_println!(
        "[mmio] direct map: phys [{:#x}, {:#x}) at virt offset {:#x}",
        0,
        max_end,
        offset
    );
}

/// Translates a physical page-table frame into a usable pointer.
///
/// Sound because the frame allocator only hands out frames from
/// `MemoryRegionKind::Usable` RAM, and the bootloader direct-mapped physical
/// memory at `PHYS_MAP_OFFSET`, so `offset + frame` is a valid virtual
/// address for every frame this mapper can obtain.
struct PhysOffsetFrameMap {
    offset: u64,
}

unsafe impl PageTableFrameMapping for PhysOffsetFrameMap {
    fn frame_to_pointer(&self, frame: PhysFrame) -> *mut PageTable {
        (self.offset.wrapping_add(frame.start_address().as_u64())) as *mut PageTable
    }
}

/// Returns an unused level-4 index for a new MMIO window.
///
/// Only slots that are currently unused are considered: overwriting a live
/// entry would unmap the UEFI identity map (index 0), the RAM direct map, the
/// kernel image, or the GOP framebuffer. Indices 256..512 are tried first so
/// the window lands in the upper half, away from the low mappings above. Each
/// slot spans 512 GiB, so one is always enough for a register window.
fn pick_mmio_p4_slot(p4: &PageTable) -> Option<u64> {
    let free = |i: usize| p4[i].is_unused();
    for i in 256..512 {
        if free(i) {
            return Some(i as u64);
        }
    }
    for i in 1..256 {
        if free(i) {
            return Some(i as u64);
        }
    }
    None
}

/// Maps `[phys, phys + size)` as device MMIO and returns the virtual base.
///
/// Two paths:
/// 1. Inside the bootloader's direct map (`phys + size <= PHYS_MAP_END`):
///    `offset + phys`, the identity the E1000/VGA/ACPI code already assumes.
/// 2. Anywhere else: allocate real page tables in a fresh level-4 slot and
///    map every 4 KiB page.
///
/// Returns `None` for a zero/size-less physical range or when page-table
/// allocation fails, so callers surface their normal "init failed" path
/// instead of touching an unmapped address and taking a page fault.
pub fn map_mmio_region(
    phys: u64,
    size: usize,
    phys_offset: u64,
    alloc: &mut crate::memory::frame_allocator::GlobalFrameAllocator,
) -> Option<usize> {
    if phys == 0 || size == 0 {
        return None;
    }
    // Prefer the recorded window; fall back to the offset handed in by the
    // caller so a missing set_phys_window() degrades to today's behaviour
    // rather than rebuilding tables for a BAR that is already reachable.
    let map_end = PHYS_MAP_END.load(Ordering::Relaxed);
    let direct = phys
        .checked_add(size as u64)
        .map(|end| end <= map_end)
        .unwrap_or(false);
    if direct {
        let virt = phys_offset.wrapping_add(phys);
        crate::println!(
            "[mmio] BAR {:#x} +{:#x} -> virt {:#x} (direct map)",
            phys,
            size,
            virt
        );
        return Some(virt as usize);
    }

    let _guard = MMIO_MAP_LOCK.lock();

    use x86_64::registers::control::Cr3;

    // Page-table frames must be reachable to walk them. The caller passes the
    // same offset the rest of the kernel uses for physical memory.
    let frame_map = PhysOffsetFrameMap { offset: phys_offset };
    let (cr3_frame, cr3_flags) = Cr3::read();
    let p4_ptr = frame_map.frame_to_pointer(cr3_frame);
    let mut mapper = unsafe { MappedPageTable::new(&mut *p4_ptr, frame_map) };

    let p4_index = match pick_mmio_p4_slot(mapper.level_4_table()) {
        Some(idx) => idx,
        None => {
            crate::serial_println!("[mmio] no free level-4 slot for phys {:#x}", phys);
            return None;
        }
    };
    // Level-4 index 256 and up sets bit 47, so the address must be
    // sign-extended through bit 63 or the CPU rejects it as non-canonical.
    let sign_extend = if p4_index & 0x100 != 0 {
        0xFFFF_0000_0000_0000u64
    } else {
        0
    };
    let virt_base = (p4_index << 39) | sign_extend;

    let flags = MMIO_LEAF_FLAGS;
    let table_flags = MMIO_TABLE_FLAGS;
    for offset in (0..size as u64).step_by(4096) {
        let vaddr = virt_base + offset;
        let paddr = x86_64::PhysAddr::new(phys + offset);
        let Ok(vpage) = Page::<Size4KiB>::from_start_address(VirtAddr::new(vaddr)) else {
            crate::serial_println!("[mmio] unaligned window page {:#x}", vaddr);
            return None;
        };
        let pframe = PhysFrame::containing_address(paddr);
        let mapped = unsafe {
            mapper
                .map_to_with_table_flags(vpage, pframe, flags, table_flags, alloc)
                .is_ok()
        };
        if !mapped {
            crate::serial_println!(
                "[mmio] map failed: virt {:#x} -> phys {:#x}",
                vaddr,
                paddr.as_u64()
            );
            return None;
        }
    }

    // Flush non-global TLB entries so the fresh level-4 entry is visible. Safe
    // because only new mappings are added: the running kernel keeps executing
    // from the mappings it already had.
    unsafe { Cr3::write(cr3_frame, cr3_flags) };

    if mapper.translate_addr(VirtAddr::new(virt_base)).is_none() {
        crate::serial_println!(
            "[mmio] phys {:#x} window {:#x} absent after CR3 reload",
            phys,
            virt_base
        );
        return None;
    }

    crate::serial_println!(
        "[mmio] phys {:#x}..{:#x} -> virt {:#x}..{:#x} (level-4 slot {})",
        phys,
        phys + size as u64,
        virt_base,
        virt_base + size as u64,
        p4_index
    );
    crate::println!(
        "[mmio] BAR {:#x} +{:#x} -> virt {:#x} (p4 slot {})",
        phys,
        size,
        virt_base,
        p4_index
    );
    Some(virt_base as usize)
}

/// True when `virt` currently has a page-table entry.
///
/// Drivers call this on a freshly mapped window before the first register
/// access: an absent entry is a clean `Err` instead of a page fault that takes
/// down the kernel. Addresses inside the bootloader's direct map answer `true`
/// without a walk, so this can never be the reason a working BAR stops
/// working. `phys_offset` is only used to reach the page tables.
pub fn is_mapped(virt: usize, phys_offset: u64) -> bool {
    use x86_64::registers::control::Cr3;

    let addr = virt as u64;
    let offset = PHYS_MAP_OFFSET.load(Ordering::Relaxed);
    let map_end = PHYS_MAP_END.load(Ordering::Relaxed);
    if addr >= offset && addr.wrapping_sub(offset) < map_end {
        return true;
    }

    let virt_addr = VirtAddr::new(addr);
    let _guard = MMIO_MAP_LOCK.lock();
    let (cr3_frame, _) = Cr3::read();
    let frame_map = PhysOffsetFrameMap { offset: phys_offset };
    let p4_ptr = frame_map.frame_to_pointer(cr3_frame);
    let mapper = unsafe { MappedPageTable::new(&mut *p4_ptr, frame_map) };
    mapper.translate_addr(virt_addr).is_some()
}

/// Intel PCH USB2 port-routing quirk: switch switchable EHCI ports to xHCI.
///
/// Currently a conservative no-op returning `false` (no ports re-routed, so
/// the caller skips its post-mux settle delay). Rationale: non-Intel
/// controllers (QEMU, AMD, ASMedia) must never see PCH mux accesses, and the
/// standard xHCI bring-up enumerates its root ports directly. A real Intel
/// quirk can be implemented here later behind a vendor check.
pub fn claim_intel_xhci_ports(_devices: &[PciDevice]) -> bool {
    false
}
