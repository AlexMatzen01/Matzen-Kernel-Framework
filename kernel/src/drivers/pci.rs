//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Basic PCI device enumeration
//!
//! Provides simple PCI configuration space access

use x86_64::instructions::port::Port;

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

/// Maps an MMIO region into the address space.
///
/// The bootloader maps all physical memory at `phys_offset`
/// (`Mapping::Dynamic`), exactly like the E1000 driver relies on, so the
/// virtual base is simply `phys_offset + phys`. No page-table work is
/// needed; `_alloc` is accepted for signature compatibility with the USB
/// drivers. Returns `None` for a zero physical base.
pub fn map_mmio_region(
    phys: u64,
    _size: usize,
    phys_offset: u64,
    _alloc: &mut crate::memory::frame_allocator::GlobalFrameAllocator,
) -> Option<usize> {
    if phys == 0 {
        return None;
    }
    Some(phys_offset.wrapping_add(phys) as usize)
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
