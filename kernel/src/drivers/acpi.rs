//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ACPI table parsing for FADT (PM Timer) discovery.
//!
//! Locates RSDP → RSDT/XSDT → FADT to get the PM Timer I/O port address
//! from the FADT's PM_TIMER_BLOCK field. Provides fallback addresses
//! for platforms where ACPI tables are unavailable.

use core::mem;
use core::ptr;
use core::slice;

use crate::serial_println;

/// RSDP (Root System Description Pointer) structure.
#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],      // "RSD PTR "
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_addr: u32,          // Physical address of RSDT
    length: u32,             // Length of XSDT (revision >= 2)
    xsdt_addr: u64,          // Physical address of XSDT (revision >= 2)
    extended_checksum: u8,
    reserved: [u8; 3],
}

/// Generic SDT (System Description Table) header.
#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

/// FADT (Fixed ACPI Description Table) - subset we care about.
#[repr(C, packed)]
struct Fadt {
    header: SdtHeader,
    firmware_ctrl: u32,
    dsdt: u32,
    _reserved0: u8,
    preferred_pm_profile: u8,
    sci_int: u16,
    smi_cmd: u32,
    acpi_enable: u8,
    acpi_disable: u8,
    s4bios_req: u8,
    pstate_cnt: u8,
    pm1a_evt_blk: u32,
    pm1b_evt_blk: u32,
    pm1a_cnt_blk: u32,
    pm1b_cnt_blk: u32,
    pm2_cnt_blk: u32,
    pm_tmr_blk: u32,         // PM_TIMER_BLOCK - what we need!
    gpe0_blk: u32,
    gpe1_blk: u32,
    pm1_evt_len: u8,
    pm1_cnt_len: u8,
    pm2_cnt_len: u8,
    pm_tmr_len: u8,          // 4 = 32-bit, else 24-bit
    gpe0_blk_len: u8,
    gpe1_blk_len: u8,
    gpe1_base: u8,
    cst_cnt: u8,
    p_lvl2_lat: u16,
    p_lvl3_lat: u16,
    flush_size: u16,
    flush_stride: u16,
    duty_offset: u8,
    duty_width: u8,
    day_alrm: u8,
    mon_alrm: u8,
    century: u8,
    iapc_boot_arch: u16,
    _reserved1: u8,
    flags: u32,
    // ... rest not needed
}

/// Parsed FADT info we care about.
#[derive(Debug, Clone, Copy)]
pub struct FadtInfo {
    pub pm_timer_addr: u16,
    pub pm_timer_len: u8,    // 4 = 32-bit, else 24-bit
    pub flags: u32,
}

/// Common fallback PM timer I/O addresses (covers 99% of UEFI platforms).
const FALLBACK_PM_TIMER_ADDRS: &[u16] = &[
    0x408,   // Standard
    0x4008,  // Common on Intel PCH
    0x808,   // Some older chipsets
    0x1008,  // Some AMD
    0x1808,  // Some server platforms
    0x2008,  // Rare
    0x4000,  // Additional Intel PCH variant
    0x4408,  // Additional Intel PCH variant
];

/// Reads a physical address via the bootloader's physical memory offset.
fn read_phys_u8(phys_addr: u64, phys_offset: u64) -> u8 {
    let virt = phys_offset + phys_addr;
    unsafe { ptr::read_volatile(virt as *const u8) }
}

fn read_phys_u32(phys_addr: u64, phys_offset: u64) -> u32 {
    let virt = phys_offset + phys_addr;
    unsafe { ptr::read_volatile(virt as *const u32) }
}

fn read_phys_u64(phys_addr: u64, phys_offset: u64) -> u64 {
    let virt = phys_offset + phys_addr;
    unsafe { ptr::read_volatile(virt as *const u64) }
}

/// Validates an ACPI table checksum.
fn validate_checksum(base: u64, length: u32, phys_offset: u64) -> bool {
    let mut sum: u8 = 0;
    for i in 0..length {
        sum = sum.wrapping_add(read_phys_u8(base + i as u64, phys_offset));
    }
    sum == 0
}

/// Searches for RSDP in EBDA and BIOS ROM area.
fn find_rsdp(phys_offset: u64) -> Option<u64> {
    // 1. EBDA (Extended BIOS Data Area) at 0x40:0x0E (physical 0x40E)
    // EBDA segment is at 0x40E, segment * 16 = physical address
    let ebda_seg = read_phys_u16(0x40E, phys_offset);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;
        if let Some(addr) = scan_region_for_rsdp(ebda_base, 1024, phys_offset) {
            return Some(addr);
        }
    }

    // 2. BIOS ROM area: 0xE0000 - 0xFFFFF (16-byte aligned)
    if let Some(addr) = scan_region_for_rsdp(0xE0000, 0x20000, phys_offset) {
        return Some(addr);
    }

    None
}

fn read_phys_u16(phys_addr: u64, phys_offset: u64) -> u16 {
    let virt = phys_offset + phys_addr;
    unsafe { ptr::read_volatile(virt as *const u16) }
}

/// Scans a memory region for RSDP signature ("RSD PTR ").
fn scan_region_for_rsdp(start: u64, size: u64, phys_offset: u64) -> Option<u64> {
    let mut addr = start;
    let end = start + size;
    while addr + 16 <= end {
        // Check signature
        let sig = read_phys_u64(addr, phys_offset);
        if sig == u64::from_le_bytes(*b"RSD PTR ") {
            // Verify checksum (first 20 bytes for rev 0, full for rev 2)
            let revision = read_phys_u8(addr + 15, phys_offset);
            let len = if revision >= 2 { 36 } else { 20 };
            if validate_checksum(addr, len, phys_offset) {
                return Some(addr);
            }
        }
        addr += 16;
    }
    None
}

/// Parses the RSDP to get RSDT/XSDT address.
fn parse_rsdp(rsdp_addr: u64, phys_offset: u64) -> Option<(u64, bool)> {
    let revision = read_phys_u8(rsdp_addr + 15, phys_offset);
    if revision >= 2 {
        // XSDT (64-bit)
        let xsdt_addr = read_phys_u64(rsdp_addr + 24, phys_offset);
        Some((xsdt_addr, true))
    } else {
        // RSDT (32-bit)
        let rsdt_addr = read_phys_u32(rsdp_addr + 16, phys_offset) as u64;
        Some((rsdt_addr, false))
    }
}

/// Searches RSDT/XSDT for FADT (signature "FACP").
fn find_fadt(sdt_addr: u64, is_xsdt: bool, phys_offset: u64) -> Option<u64> {
    let header = unsafe {
        ptr::read_volatile((phys_offset + sdt_addr) as *const SdtHeader)
    };
    if &header.signature != b"RSDT" && &header.signature != b"XSDT" {
        return None;
    }
    if !validate_checksum(sdt_addr, header.length, phys_offset) {
        return None;
    }

    let entry_size = if is_xsdt { 8 } else { 4 };
    let entries = (header.length - mem::size_of::<SdtHeader>() as u32) / entry_size;
    let entries_start = sdt_addr + mem::size_of::<SdtHeader>() as u64;

    for i in 0..entries {
        let entry_addr = entries_start + (i as u64 * entry_size as u64);
        let table_addr = if is_xsdt {
            read_phys_u64(entry_addr, phys_offset)
        } else {
            read_phys_u32(entry_addr, phys_offset) as u64
        };

        // Check signature at table_addr
        let sig = read_phys_u32(table_addr, phys_offset);
        if sig == u32::from_le_bytes(*b"FACP") {
            return Some(table_addr);
        }
    }
    None
}

/// Parses FADT at the given physical address.
fn parse_fadt(fadt_addr: u64, phys_offset: u64) -> Option<FadtInfo> {
    let fadt = unsafe {
        ptr::read_volatile((phys_offset + fadt_addr) as *const Fadt)
    };

    // Validate signature and checksum
    if &fadt.header.signature != b"FACP" {
        return None;
    }
    if !validate_checksum(fadt_addr, fadt.header.length, phys_offset) {
        return None;
    }

    let pm_timer_addr = fadt.pm_tmr_blk as u16;
    let pm_timer_len = fadt.pm_tmr_len;

    if pm_timer_addr == 0 {
        return None;
    }

    Some(FadtInfo {
        pm_timer_addr,
        pm_timer_len,
        flags: fadt.flags,
    })
}

/// Public entry point: finds and parses FADT, returns PM timer info.
/// Returns None if ACPI tables unavailable or FADT missing.
pub fn find_pm_timer_info(phys_offset: u64) -> Option<FadtInfo> {
    let rsdp_addr = find_rsdp(phys_offset)?;
    let (sdt_addr, is_xsdt) = parse_rsdp(rsdp_addr, phys_offset)?;
    let fadt_addr = find_fadt(sdt_addr, is_xsdt, phys_offset)?;
    parse_fadt(fadt_addr, phys_offset)
}

/// Tries fallback PM timer addresses.
/// Returns first address that responds (reads non-zero, non-0xFFFFFFFF twice with delay).
pub fn try_fallback_pm_timer_addrs() -> Option<u16> {
    use x86_64::instructions::port::Port;

    for &addr in FALLBACK_PM_TIMER_ADDRS {
        let mut port = Port::<u32>::new(addr);
        let v1 = unsafe { port.read() };
        // Small delay
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        let v2 = unsafe { port.read() };

        if v1 != 0 && v1 != 0xFFFFFFFF && v2 != 0 && v2 != 0xFFFFFFFF {
            crate::serial_println!("[acpi] PM timer found at fallback address 0x{:04x}", addr);
            return Some(addr);
        }
    }
    None
}