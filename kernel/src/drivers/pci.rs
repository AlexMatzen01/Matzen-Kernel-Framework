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

pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bar0: u32,
    pub bar1: u32,
    pub irq_line: u8,
}

impl PciDevice {
    pub fn read_config(&self, offset: u8) -> u32 {
        let address = 0x80000000u32
            | ((self.bus as u32) << 16)
            | ((self.device as u32) << 11)
            | ((self.function as u32) << 8)
            | ((offset as u32) & 0xFC);

        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);

            addr_port.write(address);
            data_port.read()
        }
    }
}

pub fn enumerate_devices() -> alloc::vec::Vec<PciDevice> {
    let mut devices = alloc::vec::Vec::new();

    for bus in 0..256u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = 0x80000000u32
                    | ((bus as u32) << 16)
                    | ((device as u32) << 11)
                    | ((function as u32) << 8);

                let vendor_device = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address);
                    data_port.read()
                };

                let vendor_id = (vendor_device & 0xFFFF) as u16;
                let device_id = (vendor_device >> 16) as u16;

                // Check if device exists (vendor ID != 0xFFFF)
                if vendor_id == 0xFFFF {
                    continue;
                }

                // Read class code
                let class_info = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address | 0x08);
                    data_port.read()
                };

                let class_code = ((class_info >> 24) & 0xFF) as u8;
                let subclass = ((class_info >> 16) & 0xFF) as u8;
                let prog_if = ((class_info >> 8) & 0xFF) as u8;

                // Read BAR0
                let bar0 = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address | 0x10);
                    data_port.read()
                };

                // Read BAR1
                let bar1 = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address | 0x14);
                    data_port.read()
                };

                // Read IRQ line
                let irq_line = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address | 0x3C);
                    (data_port.read() & 0xFF) as u8
                };

                devices.push(PciDevice {
                    bus: bus as u8,
                    device,
                    function,
                    vendor_id,
                    device_id,
                    class_code,
                    subclass,
                    prog_if,
                    bar0,
                    bar1,
                    irq_line,
                });
            }
        }
    }

    devices
}

pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    // Only scan first few buses to avoid slow boot
    for bus in 0..8u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = 0x80000000u32
                    | ((bus as u32) << 16)
                    | ((device as u32) << 11)
                    | ((function as u32) << 8);

                let vendor_device = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);

                    addr_port.write(address);
                    data_port.read()
                };

                let vid = (vendor_device & 0xFFFF) as u16;
                let did = (vendor_device >> 16) as u16;

                if vid == vendor_id && did == device_id {
                    // Read class code
                    let class_info = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);

                        addr_port.write(address | 0x08);
                        data_port.read()
                    };

                    let class_code = ((class_info >> 24) & 0xFF) as u8;
                    let subclass = ((class_info >> 16) & 0xFF) as u8;
                    let prog_if = ((class_info >> 8) & 0xFF) as u8;

                    // Read BAR0
                    let bar0 = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);

                        addr_port.write(address | 0x10);
                        data_port.read()
                    };

                    // Read BAR1
                    let bar1 = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);

                        addr_port.write(address | 0x14);
                        data_port.read()
                    };

                    // Read IRQ line
                    let irq_line = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);

                        addr_port.write(address | 0x3C);
                        (data_port.read() & 0xFF) as u8
                    };

                    return Some(PciDevice {
                        bus: bus as u8,
                        device,
                        function,
                        vendor_id: vid,
                        device_id: did,
                        class_code,
                        subclass,
                        prog_if,
                        bar0,
                        bar1,
                        irq_line,
                    });
                }
            }
        }
    }

    None
}

/// Returns true for USB xHCI controllers (class 0x0C, subclass 0x03, prog-if 0x30).
impl PciDevice {
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

    /// BAR0 MMIO base as a 64-bit physical address. Handles 64-bit BARs via BAR1.
    pub fn mmio_base(&self) -> Option<u64> {
        if self.bar0 & 0x01 != 0 {
            return None; // I/O space, not memory
        }
        if self.bar0 & 0x06 == 0x04 {
            // 64-bit BAR: upper half in BAR1.
            let lo = (self.bar0 & 0xFFFF_FFF0) as u64;
            let hi = (self.bar1 & 0xFFFF_FFFF) as u64;
            Some(lo | (hi << 32))
        } else {
            Some((self.bar0 & 0xFFFF_FFF0) as u64)
        }
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
