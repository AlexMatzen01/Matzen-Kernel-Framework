//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! SMBIOS physical-memory inventory (types 16 and 17).
//!
//! The firmware memory map says *how much* RAM the machine has; SMBIOS says
//! *what it is* â€” module sizes, form factor, ECC, manufacturer part numbers.
//! That is the difference between `mem` reporting `488 MiB allocatable` and
//! `2 x 8 GiB DDR4 non-ECC, 3200 MT/s`.
//!
//! Entry-point discovery walks ACPI's RSDP for either the SMBIOS3 (`SMBIOS3`)
//! or the legacy (`_SM_`) table, so BIOS and UEFI boots both work. The whole
//! parser is bounds-checked over a `&[u8]` and, crucially, split into pure
//! functions so the structure decoders can be unit-tested against
//! hand-built byte fixtures without any firmware present.
//!
//! References: SMBIOS 3.6 specification --5 (Physical Memory Array) and --6
//! (Physical Memory Device).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// SMBIOS structure 16 â€” Physical Memory Array.
pub const TYPE_MEMORY_ARRAY: u8 = 16;
/// SMBIOS structure 17 â€” Physical Memory Device.
pub const TYPE_MEMORY_DEVICE: u8 = 17;


/// Guard against a table whose structure lengths are nonsense, which would
/// otherwise turn the walk into an unbounded loop.
const MAX_STRUCTURES: usize = 512;
/// A single structure longer than this is treated as a malformed table.
const MAX_STRUCTURE_LEN: usize = 4096;

/// Bytes read from the RSDP when looking for an entry point. The structure is
/// at most 128 bytes; reading a full page costs nothing and tolerates firmware
/// that pads it.
pub const RSDP_READ_LEN: usize = 64;

/// A SMBIOS string reference (an index into the structure's string pool).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StrRef(pub u16);

/// Memory technology, decoded from type 17 field 0x1B.
pub fn technology_name(tech: u8) -> &'static str {
    match tech {
        1 => "DRAM",
        2 => "NVDIMM-N",
        3 => "NVDIMM-F",
        4 => "NVDIMM-P",
        5 => "Intel Optane DC persistent",
        6 => "LPDDR",
        7 => "LPDDR2",
        8 => "LPDDR3",
        9 => "LPDDR4",
        10 => "LPDDR5",
        11 => "HBM",
        12 => "HBM2",
        13 => "DDR",
        14 => "DDR2",
        15 => "DDR3",
        16 => "DDR4",
        17 => "DDR5",
        18 => "DDR5 (LM)",
        19 => "HBM3",
        0xFF => "unknown",
        _ => "other",
    }
}

/// Memory form factor, decoded from type 17 field 0x14.
pub fn form_factor_name(form: u8) -> &'static str {
    match form {
        1 => "other",
        2 => "unknown",
        3 => "SIMM",
        4 => "SIP",
        5 => "Chip",
        6 => "DIP",
        7 => "ZIP",
        8 => "proprietary card",
        9 => "proprietary",
        10 => "SOIMM",
        11 => "RIMM",
        12 => "SODIMM",
        13 => "SRIMM",
        14 => "FB-DIMM",
        17 => "DIMM",
        18 => "TSOP",
        19 => "row-of-chips",
        20 => "R-SIMM",
        21 => "S-SIMM",
        22 => "SOP",
        23 => "SOP stacked",
        24 => "MASP",
        25 => "small outline (STAMP)",
        26 => "M.2",
        0xFF => "unknown",
        _ => "other",
    }
}

/// One SMBIOS type 17 record.
#[derive(Clone, Debug, Default)]
pub struct MemoryDevice {
    /// Handle of the containing array (type 16).
    pub array_handle: u16,
    /// Installed size in mebibytes. `0` when unknown or not installed.
    pub size_mb: u64,
    /// True when the slot is populated with usable memory.
    pub installed: bool,
    pub form_factor: u8,
    pub memory_type: u8,
    pub technology: u8,
    /// Total bus width in bits (`0` when unreported).
    pub total_width_bits: u32,
    /// Data width in bits (`0` when unreported).
    pub data_width_bits: u32,
    /// Configured speed in MT/s, when the module reports one.
    pub speed_mts: Option<u32>,
    pub locator: String,
    pub bank: String,
    pub manufacturer: String,
    pub part_number: String,
    pub serial: String,
}

/// One SMBIOS type 16 record.
#[derive(Clone, Debug, Default)]
pub struct MemoryArray {
    pub handle: u16,
    /// Maximum capacity of the array in mebibytes, when reported.
    pub max_size_mb: Option<u64>,
    /// Number of device slots the array reports.
    pub device_count: u16,
    /// ECC method bitmask from field 0x0E.
    pub ecc_methods: u8,
}

/// Full memory inventory assembled from the SMBIOS table.
#[derive(Clone, Debug, Default)]
pub struct MemoryInventory {
    pub arrays: Vec<MemoryArray>,
    pub devices: Vec<MemoryDevice>,
    /// Number of structure records walked.
    pub structure_count: usize,
    /// True when the walk stopped early at the table length rather than at a
    /// type 127 end marker, or hit a bound.
    pub truncated: bool,
}

impl MemoryInventory {
    /// Total installed, populated memory in mebibytes.
    pub fn installed_mb(&self) -> u64 {
        self.devices
            .iter()
            .filter(|d| d.installed)
            .map(|d| d.size_mb)
            .sum()
    }

    /// Slots that are present but not populated.
    pub fn empty_slots(&self) -> usize {
        self.devices.iter().filter(|d| !d.installed).count()
    }

    /// True when any populated module reports ECC.
    pub fn has_ecc(&self) -> bool {
        self.devices
            .iter()
            .filter(|d| d.installed)
            .any(|d| d.total_width_bits > 0 && d.total_width_bits > d.data_width_bits)
            || self.arrays.iter().any(|a| a.ecc_methods != 0)
    }
}

// â”€â”€ entry point discovery â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Offset from the start of the RSDP to the SMBIOS3 entry point.
///
/// The legacy and SMBIOS3 entry points live in the RSDP's tail, and their
/// positions differ between ACPI revisions, so [`find_table`] scans for the
/// signatures instead of trusting a fixed offset. This constant is the
/// smallest RSDP that can still contain a legacy entry point.
const RSDP_MIN_LEN: usize = 36;

/// Find `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Locate the SMBIOS table referenced by a raw ACPI RSDP.
///
/// Scans for the `SMBIOS3` entry point first and falls back to the legacy
/// `_SM_` one, so a BIOS machine (which has no SMBIOS3) still resolves. The
/// signatures are located by search rather than by a hardcoded RSDP offset:
/// the two entry points sit in the RSDP tail and their offsets differ between
/// ACPI revisions and firmware, so a fixed offset is a silent failure mode.
///
/// Returns the table's physical address and declared length.
pub fn find_table(rsdp: &[u8]) -> Option<(u64, usize)> {
    if rsdp.len() < RSDP_MIN_LEN {
        return None;
    }
    // SMBIOS3 entry point: table address at sig+16, length at sig+24.
    if let Some(i) = find_subslice(rsdp, b"SMBIOS3") {
        if i + 26 <= rsdp.len() {
            let table =
                u64::from_le_bytes(rsdp[i + 16..i + 24].try_into().ok()?) as *mut u8 as u64;
            let length = u16::from_le_bytes(rsdp[i + 24..i + 26].try_into().ok()?) as usize;
            if length > 0 {
                return Some((table, length));
            }
        }
    }
    // Legacy `_SM_` entry point: table address at sig+10, length at sig+14.
    if let Some(i) = find_subslice(rsdp, b"_SM_") {
        if i + 16 <= rsdp.len() {
            let table = u32::from_le_bytes(rsdp[i + 10..i + 14].try_into().ok()?) as u64;
            let length = u16::from_le_bytes(rsdp[i + 14..i + 16].try_into().ok()?) as usize;
            if length > 0 {
                return Some((table, length));
            }
        }
    }
    None
}

// â”€â”€ structure decoding â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn read_string(data: &[u8], strings: &[u8], index: u8) -> String {
    if index == 0 {
        return String::new();
    }
    // Walk to the requested NUL-terminated string in the pool.
    let mut current = 1u8;
    let mut pos = 0usize;
    while pos < strings.len() && current < index {
        match strings[pos..].iter().position(|&b| b == 0) {
            Some(i) => pos += i + 1,
            None => return String::new(),
        }
        current += 1;
    }
    if pos >= strings.len() {
        return String::new();
    }
    let end = strings[pos..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| pos + i)
        .unwrap_or(strings.len());
    core::str::from_utf8(&strings[pos..end])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn u8_at(d: &[u8], off: usize) -> Option<u8> {
    d.get(off).copied()
}

fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Decode one type 17 structure.
///
/// `structure` is the whole record including the 4-byte header, so field
/// offsets match the SMBIOS specification directly with no shifting to get
/// wrong. `strings` is the record's string pool.
pub fn decode_memory_device(structure: &[u8], strings: &[u8]) -> Option<MemoryDevice> {
    // Type 17 is at least 0x11 bytes; 0x22 is needed for the 2.x string
    // fields and 0x24 for the SMBIOS 3.x configured-speed field.
    if structure.len() < 0x11 {
        return None;
    }
    let size_raw = u32_at(structure, 0x10)?;
    // 0xFFFFFFFF = size unknown; 0x7FFFFFFF = slot not populated.
    let (size_mb, installed) = match size_raw {
        0xFFFF_FFFF | 0x7FFF_FFFF | 0 => (0u64, false),
        mb => (mb as u64, true),
    };
    let (locator, bank) = if structure.len() >= 0x18 {
        (
            read_string(structure, strings, u8_at(structure, 0x16)?),
            read_string(structure, strings, u8_at(structure, 0x17)?),
        )
    } else {
        (String::new(), String::new())
    };
    let (manufacturer, serial, part_number) = if structure.len() >= 0x22 {
        (
            read_string(structure, strings, u8_at(structure, 0x1E)?),
            read_string(structure, strings, u8_at(structure, 0x1F)?),
            read_string(structure, strings, u8_at(structure, 0x21)?),
        )
    } else {
        (String::new(), String::new(), String::new())
    };
    // Configured speed (SMBIOS 3.x, DWORD at 0x20).
    let speed_mts = if structure.len() >= 0x24 {
        match u32_at(structure, 0x20)? {
            0 | 0xFFFF_FFFF => None,
            mts => Some(mts),
        }
    } else {
        None
    };
    Some(MemoryDevice {
        array_handle: u16_at(structure, 0x04)?,
        size_mb,
        installed,
        form_factor: structure.get(0x14).copied().unwrap_or(0xFF),
        memory_type: structure.get(0x18).copied().unwrap_or(0),
        technology: structure.get(0x1B).copied().unwrap_or(0xFF),
        total_width_bits: u32_at(structure, 0x08).unwrap_or(0),
        data_width_bits: u32_at(structure, 0x0C).unwrap_or(0),
        speed_mts,
        locator,
        bank,
        manufacturer,
        part_number,
        serial,
    })
}

/// A declared maximum capacity in KiB that could plausibly be real.
///
/// `0` means "not stated", `0xFFFFFFFF` means "unknown". Anything larger than
/// the machine's whole address space cannot be a memory capacity either, which
/// is how a misaligned read of the wrong field is caught.
fn plausible_capacity_kb(kb: u32, addressable_bytes: u64) -> Option<u64> {
    if kb == 0 || kb == 0xFFFF_FFFF {
        return None;
    }
    let bytes = (kb as u64).checked_mul(1024)?;
    if bytes > addressable_bytes {
        return None;
    }
    Some(bytes / 1024 / 1024) // KiB -> MiB
}

/// Decode one type 16 structure (full record, header included).
///
/// Type 16 has **two published layouts** and the only reliable discriminator
/// is the record's own length byte â€” which is what the length byte is for:
///
/// ```text
/// offset  SMBIOS 2.x (len 0x0F)   SMBIOS 3.x (len >= 0x17)
///  0x02   array handle (WORD)      array handle (WORD)
///  0x04   error handle (BYTE)      array max capacity (DWORD, KiB)
///  0x05   device count (WORD)      -
///  0x07   device max size (MB)     error handle (BYTE)
///  0x09   -                        device count (WORD)
///  0x0B   ECC methods (BYTE)       device max size (MB)
///  0x0F   -                        ECC methods (BYTE)
/// ```
///
/// Reading a 3.x record at 2.x offsets yields a plausible-looking but wrong
/// handle and slot count, so the layout is chosen from the declared length.
/// A length matching neither revision is not decoded at all: guessing is worse
/// than omitting the record.
pub fn decode_memory_array(structure: &[u8], addressable_bytes: u64) -> Option<MemoryArray> {
    let length = *structure.get(1)? as usize;
    let (count_off, ecc_off, capacity_off) = if length == 0x0F {
        (0x05usize, 0x0Busize, None)
    } else if length >= 0x17 {
        (0x09usize, 0x0Fusize, Some(0x04usize))
    } else {
        return None;
    };
    let max_size_mb = capacity_off
        .and_then(|off| u32_at(structure, off))
        .and_then(|kb| plausible_capacity_kb(kb, addressable_bytes));
    Some(MemoryArray {
        // The array handle is at 0x02 in both revisions.
        handle: u16_at(structure, 0x02).unwrap_or(0),
        max_size_mb,
        device_count: u16_at(structure, count_off).unwrap_or(0),
        ecc_methods: structure.get(ecc_off).copied().unwrap_or(0),
    })
}
/// Walk a SMBIOS table image and collect types 16 and 17.
///
/// Stops at the declared `length`, at a type 127 end-of-table record, at a
/// structure with an impossible length, or after [`MAX_STRUCTURES`]
/// structures â€” whichever comes first. Never panics on malformed input.
///
/// `addressable_bytes` bounds the array-capacity sanity check; pass the
/// machine's physical address limit (see `sysinfo::cpu_phys_addr_limit`).
pub fn parse_table(table: &[u8], addressable_bytes: u64) -> MemoryInventory {
    let mut inv = MemoryInventory::default();
    let mut pos = 0usize;
    while pos + 4 <= table.len() && inv.structure_count < MAX_STRUCTURES {
        let structure_type = table[pos];
        let length = table[pos + 1] as usize;
        if length < 4 || length > MAX_STRUCTURE_LEN || pos + length > table.len() {
            inv.truncated = true;
            break;
        }
        let body = &table[pos..pos + length];
        // The string pool follows the formatted section, terminated by two
        // consecutive NUL bytes.
        let pool_end = find_struct_end(table, pos, length).unwrap_or(table.len());
        let strings = &table[pos + length..pool_end];

        match structure_type {
            TYPE_MEMORY_ARRAY => {
                if let Some(array) = decode_memory_array(body, addressable_bytes) {
                    inv.arrays.push(array);
                }
            }
            TYPE_MEMORY_DEVICE => {
                if let Some(device) = decode_memory_device(body, strings) {
                    inv.devices.push(device);
                }
            }
            // Type 127 ends the table.
            127 => break,
            _ => {}
        }
        inv.structure_count += 1;
        if structure_type == 127 {
            break;
        }
        pos = pool_end.max(pos + length);
    }
    if pos < table.len() && inv.structure_count >= MAX_STRUCTURES {
        inv.truncated = true;
    }
    inv
}

/// Offset just past a structure's string pool.
///
/// A structure is `header + formatted section + unformatted section`, where
/// the unformatted section is each string NUL-terminated, followed by a double
/// NUL marking the end of the record.
///
/// The terminator cannot be found by scanning for two adjacent NULs: the last
/// string's own terminator is itself followed by the double NUL, so a naive
/// scan stops one byte early and desynchronises every subsequent record â€” which
/// silently reduces a three-device table to one device. Strings are therefore
/// consumed one at a time, and the terminator is recognised only at a string
/// *boundary*.
fn find_struct_end(table: &[u8], pos: usize, length: usize) -> Option<usize> {
    let mut i = pos + length;
    loop {
        if i >= table.len() {
            return None;
        }
        if table[i] == 0 {
            // At a string boundary with an empty string: this is where the
            // double NUL must start.
            return if i + 1 < table.len() && table[i + 1] == 0 {
                Some(i + 2)
            } else {
                None
            };
        }
        match table[i..].iter().position(|&b| b == 0) {
            Some(k) => i += k + 1,
            None => return None,
        }
    }
}

// â”€â”€ global inventory â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

static INVENTORY: Mutex<Option<MemoryInventory>> = Mutex::new(None);
static PARSED: AtomicBool = AtomicBool::new(false);

/// Where an entry point was found, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// The ACPI RSDP supplied by the bootloader.
    RsdpPage,
    /// The BIOS data / EBDA area below 0x500.
    LowBda,
    /// The extended BIOS ROM area 0xE0000..0xFFFFF.
    BiosRom,
}

impl Found {
    pub fn label(self) -> &'static str {
        match self {
            Found::RsdpPage => "RSDP page",
            Found::LowBda => "BIOS data area",
            Found::BiosRom => "BIOS ROM area",
        }
    }
}

/// Scan a physical range for an SMBIOS entry point, in page-sized windows.
///
/// `phys_offset` translates physical addresses into the bootloader's direct
/// map. Windows overlap by one page so an entry point straddling a boundary is
/// still found. Returns the table's physical address and declared length.
fn scan_physical(phys_offset: u64, start: u64, size: u64) -> Option<(u64, usize)> {
    const WINDOW: usize = 4096;
    /// A range larger than this is not worth scanning at boot.
    const MAX_SCAN: u64 = 256 * 1024;
    let size = size.min(MAX_SCAN);
    if size < 16 {
        return None;
    }
    let mut offset = 0u64;
    while offset + 16 <= size {
        let len = core::cmp::min(WINDOW as u64, size - offset) as usize;
        let addr = phys_offset.wrapping_add(start + offset);
        // SAFETY: `addr` is `phys_offset` plus a physical address inside a
        // firmware-owned range the bootloader mapped via `Mapping::Dynamic`.
        // The window is bounded and the parser bounds-checks every read, so a
        // range that is not real RAM yields data that simply fails to match.
        let window = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
        if let Some(found) = find_table(window) {
            return Some(found);
        }
        if len == WINDOW {
            // Overlap one page so a split entry point is still seen.
            offset += (WINDOW as u64) - 16;
        } else {
            break;
        }
    }
    None
}

/// Locate and parse the SMBIOS table.
///
/// The entry point is searched for in the order the specification recommends,
/// because no single method is reliable across both BIOS and UEFI firmware:
///
/// 1. The page holding the bootloader's ACPI RSDP, which on many firmwares
///    carries the `SMBIOS3` entry point.
/// 2. The BIOS data area, where a legacy `_SM_` entry point is usually parked.
/// 3. The extended BIOS ROM area 0xE0000..0xFFFFF.
///
/// A UEFI-only firmware may have no entry point at all in low memory, in which
/// case this returns `Err` and the caller records an empty inventory â€” the
/// memory map alone remains authoritative.
pub fn locate(
    phys_offset: u64,
    rsdp_addr: Option<u64>,
    addressable_bytes: u64,
) -> Result<(MemoryInventory, Found), &'static str> {
    // No one-off scan: firmware that installs no guest-reachable entry point
    // (OVMF among them) is reported honestly rather than swept for at boot.
    let candidates: [(Found, u64, u64); 3] = [
        (Found::RsdpPage, rsdp_addr.unwrap_or(1), 4096),
        (Found::LowBda, 0x400, 0x100),
        (Found::BiosRom, 0xE0000, 0x20000),
    ];
    for (where_, start, size) in candidates.iter() {
        if *start == 1 || *start > addressable_bytes {
            continue;
        }
        let (table_phys, len) = match scan_physical(phys_offset, *start, *size) {
            Some(found) => found,
            None => continue,
        };
        let len = len.min(MAX_STRUCTURE_LEN * MAX_STRUCTURES);
        if len < 4 || table_phys == 0 || table_phys > addressable_bytes {
            continue;
        }
        // SAFETY: as in `scan_physical`; `table_phys` came from an entry point
        // inside the window just scanned, and is range-checked above.
        let bytes =
            unsafe { core::slice::from_raw_parts(phys_offset.wrapping_add(table_phys) as *const u8, len) };
        let inv = parse_table(bytes, addressable_bytes);
        if inv.devices.is_empty() && inv.arrays.is_empty() {
            continue;
        }
        return Ok((inv, *where_));
    }
    Err("smbios: firmware exposes no SMBIOS entry point (searched RSDP page, BIOS data area, ROM area)")
}

/// Store the inventory for the `mem` command.
pub fn stash(inventory: MemoryInventory) {
    *INVENTORY.lock() = Some(inventory);
    PARSED.store(true, Ordering::Release);
}

/// The parsed inventory, if SMBIOS was found and parsed.
pub fn inventory() -> Option<MemoryInventory> {
    INVENTORY.lock().clone()
}

/// True once a SMBIOS parse has been attempted, successfully or not.
pub fn parse_attempted() -> bool {
    PARSED.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// 64 GiB address space, used as the plausibility bound for array capacity.
    const ADDRESSABLE: u64 = 64 * 1024 * 1024 * 1024;

    /// Build a full SMBIOS structure: header + formatted body + string pool
    /// + double-NUL terminator. `fill` receives the record with the length
    /// byte already set, so it writes at specification offsets.
    fn structure<F>(structure_type: u8, body_len: usize, strings: &[&str], fill: F) -> Vec<u8>
    where
        F: FnOnce(&mut [u8]),
    {
        let mut out = vec![0u8; 4 + body_len];
        out[0] = structure_type;
        out[1] = (4 + body_len) as u8;
        out[2] = 0;
        out[3] = 1;
        fill(&mut out);
        for s in strings {
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        }
        out.push(0);
        out.push(0);
        out
    }

    /// A type 17 record at the SMBIOS 3.x length (0x24).
    fn device(size_mb: u32, locator_idx: u8, bank_idx: u8, tech: u8, form: u8) -> Vec<u8> {
        structure(
            TYPE_MEMORY_DEVICE,
            0x20,
            &[],
            |s| {
                s[0x04..0x06].copy_from_slice(&0x0001u16.to_le_bytes()); // array handle
                s[0x08..0x0C].copy_from_slice(&64u32.to_le_bytes()); // total width
                s[0x0C..0x10].copy_from_slice(&64u32.to_le_bytes()); // data width
                s[0x10..0x14].copy_from_slice(&size_mb.to_le_bytes());
                s[0x14] = form;
                s[0x16] = locator_idx;
                s[0x17] = bank_idx;
                s[0x1B] = tech;
                s[0x20..0x24].copy_from_slice(&3200u32.to_le_bytes()); // speed MT/s
            },
        )
    }

    /// A SMBIOS 3.x type 16 record (length 0x17): handle WORD at 0x02,
    /// capacity DWORD (KiB) at 0x04, slot count WORD at 0x09, ECC BYTE at 0x0F.
    fn array(capacity_kb: Option<u32>, slots: u16, ecc: u8) -> Vec<u8> {
        structure(TYPE_MEMORY_ARRAY, 0x13, &[], |s| {
            s[0x02..0x04].copy_from_slice(&0x0001u16.to_le_bytes()); // handle
            if let Some(kb) = capacity_kb {
                s[0x04..0x08].copy_from_slice(&kb.to_le_bytes());
            }
            s[0x09..0x0B].copy_from_slice(&slots.to_le_bytes());
            s[0x0F] = ecc;
        })
    }

    /// A SMBIOS 2.x type 16 record (length 0x0F): handle WORD at 0x02,
    /// slot count WORD at 0x05, ECC BYTE at 0x0B, and no capacity field.
    fn array_2x(slots: u16, ecc: u8) -> Vec<u8> {
        structure(TYPE_MEMORY_ARRAY, 0x0B, &[], |s| {
            s[0x02..0x04].copy_from_slice(&0x0007u16.to_le_bytes()); // handle
            s[0x05..0x07].copy_from_slice(&slots.to_le_bytes());
            s[0x0B] = ecc;
        })
    }

    /// Replace a structure's trailing double-NUL with a new string pool.
    fn with_strings(mut s: Vec<u8>, strings: &[&str]) -> Vec<u8> {
        s.truncate(s.len() - 2);
        for text in strings {
            s.extend_from_slice(text.as_bytes());
            s.push(0);
        }
        s.push(0);
        s.push(0);
        s
    }

    fn pool_of(s: &[u8]) -> Vec<u8> {
        let len = s[1] as usize;
        s[len..].to_vec()
    }

    #[test]
    fn decodes_a_populated_device() {
        let s = with_strings(device(8192, 1, 2, 0x10, 0x11), &["DIMM_A1", "BANK 0"]);
        let d = decode_memory_device(&s, &pool_of(&s)).expect("device");
        assert!(d.installed);
        assert_eq!(d.size_mb, 8192);
        assert_eq!(d.locator, "DIMM_A1");
        assert_eq!(d.bank, "BANK 0");
        assert_eq!(d.technology, 0x10);
        assert_eq!(technology_name(d.technology), "DDR4");
        assert_eq!(form_factor_name(d.form_factor), "DIMM");
        assert_eq!(d.total_width_bits, 64);
        assert_eq!(d.data_width_bits, 64);
        assert_eq!(d.speed_mts, Some(3200));
        assert_eq!(d.array_handle, 1);
    }

    #[test]
    fn treats_sentinels_as_not_installed() {
        for sentinel in [0xFFFF_FFFFu32, 0x7FFF_FFFFu32, 0u32] {
            let s = device(sentinel, 0, 0, 0xFF, 0xFF);
            let d = decode_memory_device(&s, &pool_of(&s)).expect("device");
            assert!(!d.installed, "sentinel {:#x} must read as empty", sentinel);
            assert_eq!(d.size_mb, 0);
        }
    }

    #[test]
    fn decodes_string_index_zero_as_empty() {
        let s = with_strings(device(1024, 0, 0, 0xFF, 0xFF), &["only"]);
        let d = decode_memory_device(&s, &pool_of(&s)).expect("device");
        assert_eq!(d.locator, "");
        assert_eq!(d.bank, "");
    }

    #[test]
    fn out_of_range_string_index_is_empty_not_a_panic() {
        let s = with_strings(device(1024, 9, 99, 0xFF, 0xFF), &["a", "b"]);
        let d = decode_memory_device(&s, &pool_of(&s)).expect("device");
        assert_eq!(d.locator, "");
        assert_eq!(d.bank, "");
    }

    #[test]
    fn short_device_structure_is_rejected() {
        let short = vec![TYPE_MEMORY_DEVICE, 8, 0, 1, 0, 0, 0, 0];
        assert!(decode_memory_device(&short, &[]).is_none());
    }

    #[test]
    fn decodes_an_array() {
        let a = decode_memory_array(&array(Some(64 * 1024 * 1024), 4, 0), ADDRESSABLE)
            .expect("array");
        assert_eq!(a.handle, 1);
        assert_eq!(a.device_count, 4);
        assert_eq!(a.max_size_mb, Some(65536));
        assert_eq!(a.ecc_methods, 0);
    }

    #[test]
    fn array_without_max_size_is_reported_as_unknown() {
        let a = decode_memory_array(&array(Some(0xFFFF_FFFF), 2, 0), ADDRESSABLE)
            .expect("array");
        assert_eq!(a.max_size_mb, None);
        assert_eq!(a.device_count, 2);
    }

    #[test]
    fn legacy_length_array_is_decoded_with_its_own_offsets() {
        // A 2.x type 16 record is 0x0F bytes: handle at 0x02, slot count at
        // 0x05, ECC at 0x0B, and no capacity field. Reading it with 3.x
        // offsets returns a plausible but wrong handle and slot count.
        let a = decode_memory_array(&array_2x(4, 0x03), ADDRESSABLE).expect("array");
        assert_eq!(a.handle, 7);
        assert_eq!(a.device_count, 4);
        assert_eq!(a.ecc_methods, 0x03);
        assert_eq!(a.max_size_mb, None);
    }

    #[test]
    fn two_revisions_with_the_same_payload_decode_differently() {
        let v2 = decode_memory_array(&array_2x(4, 0), ADDRESSABLE).expect("2.x");
        let v3 = decode_memory_array(&array(None, 4, 0), ADDRESSABLE).expect("3.x");
        // Both report 4 slots, but the handle offset differs between
        // revisions and each must be read from its own position.
        assert_eq!(v2.handle, 7);
        assert_eq!(v3.handle, 1);
        assert_eq!(v2.device_count, 4);
        assert_eq!(v3.device_count, 4);
    }

    #[test]
    fn unknown_revision_is_skipped_rather_than_guessed() {
        // A type 16 record whose length matches neither the 2.x nor the 3.x
        // layout is not decoded: reporting a wrong handle or slot count is
        // worse than reporting no array at all.
        for body_len in [0x02usize, 0x06, 0x0A, 0x10, 0x11] {
            let odd = structure(TYPE_MEMORY_ARRAY, body_len, &[], |s| {
                let end = core::cmp::min(8, s.len());
                for b in s[4..end].iter_mut() {
                    *b = 0xFF;
                }
            });
            assert!(
                decode_memory_array(&odd, ADDRESSABLE).is_none(),
                "length {:#x} must not be decoded",
                4 + body_len
            );
        }
        // Both published revisions are accepted, and a 3.x record that a later
        // specification extended is still read: the fields we use sit before
        // the appended data.
        assert!(decode_memory_array(&array_2x(2, 0), ADDRESSABLE).is_some());
        assert!(decode_memory_array(&array(None, 2, 0), ADDRESSABLE).is_some());
        let longer = structure(TYPE_MEMORY_ARRAY, 0x17, &[], |s| {
            s[0x02..0x04].copy_from_slice(&1u16.to_le_bytes());
            s[0x09..0x0B].copy_from_slice(&6u16.to_le_bytes());
        });
        let a = decode_memory_array(&longer, ADDRESSABLE).expect("3.x+");
        assert_eq!(a.device_count, 6);
        assert_eq!(a.handle, 1);
    }

    #[test]
    fn capacity_sanity_is_bounded_by_the_address_space() {
        // 64 GiB expressed in KiB is the whole assumed address space and is
        // accepted; twice that is not a real capacity.
        assert_eq!(
            plausible_capacity_kb((ADDRESSABLE / 1024) as u32, ADDRESSABLE),
            Some(65536)
        );
        assert_eq!(plausible_capacity_kb(0, ADDRESSABLE), None);
        assert_eq!(plausible_capacity_kb(0xFFFF_FFFF, ADDRESSABLE), None);
        assert_eq!(plausible_capacity_kb(u32::MAX / 2, ADDRESSABLE), None);
    }

    #[test]
    fn walks_a_table_of_devices_and_sums_installed_memory() {
        let mut table = Vec::new();
        table.extend_from_slice(&with_strings(
            device(8192, 1, 2, 0x10, 0x11),
            &["DIMM_A1", "BANK 0"],
        ));
        table.extend_from_slice(&with_strings(
            device(8192, 1, 2, 0x10, 0x11),
            &["DIMM_B1", "BANK 1"],
        ));
        table.extend_from_slice(&device(0x7FFF_FFFF, 0, 0, 0xFF, 0x11));
        table.extend_from_slice(&[127, 4, 0, 1, 0, 0]);

        let inv = parse_table(&table, ADDRESSABLE);
        assert_eq!(inv.devices.len(), 3);
        assert_eq!(inv.installed_mb(), 16384);
        assert_eq!(inv.empty_slots(), 1);
        assert_eq!(inv.devices[0].locator, "DIMM_A1");
        assert_eq!(inv.devices[1].locator, "DIMM_B1");
        assert!(!inv.truncated);
    }

    #[test]
    fn walks_arrays_alongside_devices_and_skips_other_types() {
        let mut table = Vec::new();
        table.extend_from_slice(&array(Some(128 * 1024), 2, 0));
        // An unrelated structure type between the two memory records.
        table.extend_from_slice(&structure(7, 0x10, &["Intel"], |s| {
            s[0x04..0x06].copy_from_slice(&2u16.to_le_bytes());
        }));
        table.extend_from_slice(&with_strings(
            device(4096, 1, 1, 0x0F, 0x11),
            &["A1", "B0"],
        ));
        table.extend_from_slice(&[127, 4, 0, 1, 0, 0]);
        let inv = parse_table(&table, ADDRESSABLE);
        assert_eq!(inv.arrays.len(), 1);
        assert_eq!(inv.devices.len(), 1);
        assert_eq!(inv.installed_mb(), 4096);
        assert_eq!(inv.devices[0].technology, 0x0F);
        assert_eq!(technology_name(0x0F), "DDR3");
    }

    #[test]
    fn stops_at_end_of_table_marker() {
        let mut table = Vec::new();
        table.extend_from_slice(&device(1024, 0, 0, 0xFF, 0xFF));
        // A device record after the terminator must not be seen.
        table.extend_from_slice(&[127, 4, 0, 1, 0, 0]);
        table.extend_from_slice(&device(2048, 0, 0, 0xFF, 0xFF));
        let inv = parse_table(&table, ADDRESSABLE);
        assert_eq!(inv.devices.len(), 1);
        assert_eq!(inv.installed_mb(), 1024);
    }

    #[test]
    fn malformed_structure_length_terminates_the_walk() {
        // A zero length byte would loop forever if not bounded.
        let inv = parse_table(&[TYPE_MEMORY_DEVICE, 0, 0, 1, 0, 0], ADDRESSABLE);
        assert!(inv.truncated);
        assert!(inv.devices.is_empty());

        // A length running past the end of the table is also refused.
        let inv = parse_table(&[TYPE_MEMORY_DEVICE, 200, 0, 1, 0, 0], ADDRESSABLE);
        assert!(inv.truncated);
        assert!(inv.devices.is_empty());
    }

    #[test]
    fn truncated_table_does_not_panic() {
        for len in 0..64usize {
            let table = alloc::vec![0u8; len];
            let _ = parse_table(&table, ADDRESSABLE);
        }
        let mut table = with_strings(device(1024, 1, 1, 0xFF, 0xFF), &["A"]);
        table.truncate(table.len() - 3);
        let inv = parse_table(&table, ADDRESSABLE);
        assert!(inv.devices.len() <= 1);
    }

    /// An RSDP image carrying only a SMBIOS3 entry point at `at`.
    fn rsdp_smbios3(at: usize, table: u64, length: u16) -> Vec<u8> {
        let mut r = vec![0u8; 128];
        r[0..8].copy_from_slice(b"RSD PTR ");
        r[at..at + 7].copy_from_slice(b"SMBIOS3");
        r[at + 16..at + 24].copy_from_slice(&table.to_le_bytes());
        r[at + 24..at + 26].copy_from_slice(&length.to_le_bytes());
        r
    }

    /// An RSDP image carrying only a legacy `_SM_` entry point at `at`.
    fn rsdp_legacy(at: usize, table: u32, length: u16) -> Vec<u8> {
        let mut r = vec![0u8; 128];
        r[0..8].copy_from_slice(b"RSD PTR ");
        r[at..at + 4].copy_from_slice(b"_SM_");
        r[at + 10..at + 14].copy_from_slice(&table.to_le_bytes());
        r[at + 14..at + 16].copy_from_slice(&length.to_le_bytes());
        r
    }

    #[test]
    fn finds_smbios3_entry_point_at_any_offset() {
        // The signature is located by search, so its RSDP offset does not
        // matter: real firmware and ACPI revisions disagree about it.
        for at in [16usize, 24, 32, 40, 56] {
            let rsdp = rsdp_smbios3(at, 0x0010_0000, 128);
            assert_eq!(
                find_table(&rsdp),
                Some((0x0010_0000, 128)),
                "signature at offset {}",
                at
            );
        }
    }

    #[test]
    fn finds_legacy_entry_point_at_any_offset() {
        for at in [16usize, 24, 40, 56] {
            let rsdp = rsdp_legacy(at, 0x000F_0000, 96);
            assert_eq!(
                find_table(&rsdp),
                Some((0x000F_0000, 96)),
                "signature at offset {}",
                at
            );
        }
    }

    #[test]
    fn prefers_smbios3_over_legacy() {
        // Both entry points present, as real firmware provides.
        let mut rsdp = rsdp_smbios3(40, 0x0010_0000, 128);
        let legacy = rsdp_legacy(16, 0x000F_0000, 96);
        rsdp[16..32].copy_from_slice(&legacy[16..32]);
        assert_eq!(find_table(&rsdp), Some((0x0010_0000, 128)));
    }

    #[test]
    fn rejects_rsdp_without_an_entry_point() {
        assert!(find_table(&[0u8; 128]).is_none());
        assert!(find_table(&[]).is_none());
        assert!(find_table(&[0u8; 8]).is_none());
        assert!(find_table(&[0u8; RSDP_MIN_LEN - 1]).is_none());
        // A near-miss signature must not match.
        let mut rsdp = rsdp_smbios3(40, 0x0010_0000, 128);
        rsdp[40..47].copy_from_slice(b"NOTSMBI");
        assert!(find_table(&rsdp).is_none());
    }

    #[test]
    fn zero_length_table_is_rejected() {
        assert!(find_table(&rsdp_smbios3(40, 0x1000, 0)).is_none());
        assert!(find_table(&rsdp_legacy(24, 0x1000, 0)).is_none());
    }

    #[test]
    fn truncated_signature_is_not_matched() {
        // The signature is present but its fields would run past the buffer.
        let mut rsdp = vec![0u8; 32];
        rsdp[0..8].copy_from_slice(b"RSD PTR ");
        rsdp[20..27].copy_from_slice(b"SMBIOS3");
        assert!(find_table(&rsdp).is_none());
    }

    #[test]
    fn struct_end_is_found_at_a_string_boundary() {
        // A naive "two adjacent NULs" scan stops one byte early here, because
        // the last string's own terminator is followed by the double NUL. That
        // desynchronises every following record, so the boundary-aware walk is
        // what keeps multi-record tables intact.
        let mut table = Vec::new();
        table.extend_from_slice(&with_strings(device(1024, 1, 2, 0x10, 0x11), &["A", "B"]));
        let first_len = 4 + 0x20;
        // The unformatted section is "A\0B\0" plus the record's double NUL:
        // 6 bytes after the formatted section.
        assert_eq!(
            find_struct_end(&table, 0, first_len),
            Some(first_len + 6),
            "record must end after the double NUL, not at the string's NUL"
        );

        // A record with no strings ends immediately after the double NUL.
        let bare = device(1024, 0, 0, 0xFF, 0xFF);
        assert_eq!(find_struct_end(&bare, 0, 4 + 0x20), Some(4 + 0x20 + 2));
    }

    #[test]
    fn struct_end_returns_none_when_truncated() {
        // Non-zero bytes with no NUL anywhere: no terminator exists.
        let table = [1u8; 10];
        assert!(find_struct_end(&table, 0, 8).is_none());
        // A buffer that runs out mid-string.
        let mut partial = alloc::vec![0u8; 10];
        partial[8] = b'A';
        partial[9] = 0;
        assert!(find_struct_end(&partial, 0, 8).is_none());
    }

    #[test]
    fn string_pool_walking_skips_unused_entries() {
        // Index 1 unused, index 2 = "BANK", index 3 = "DIMM".
        let s = with_strings(device(1024, 3, 2, 0xFF, 0xFF), &["unused", "BANK", "DIMM"]);
        let d = decode_memory_device(&s, &pool_of(&s)).expect("device");
        assert_eq!(d.locator, "DIMM");
        assert_eq!(d.bank, "BANK");
    }

    #[test]
    fn unterminated_string_pool_yields_what_is_available() {
        let body = device(1024, 1, 0, 0xFF, 0xFF);
        let strings = b"PARTIAL".to_vec();
        let d = decode_memory_device(&body, &strings).expect("device");
        assert_eq!(d.locator, "PARTIAL");
    }

    #[test]
    fn inventory_reports_ecc_when_widths_differ() {
        let plain = device(8192, 0, 0, 0xFF, 0x11);
        let mut ecc = plain.clone();
        ecc[0x08..0x0C].copy_from_slice(&72u32.to_le_bytes()); // total width 72
        ecc[0x0C..0x10].copy_from_slice(&64u32.to_le_bytes()); // data width 64
        let d = decode_memory_device(&ecc, &[]).expect("device");
        assert_eq!(d.total_width_bits, 72);
        assert_eq!(d.data_width_bits, 64);
        let p = decode_memory_device(&plain, &[]).expect("device");
        let inv = MemoryInventory {
            devices: alloc::vec![d, p],
            ..Default::default()
        };
        assert!(inv.has_ecc());
    }
}
