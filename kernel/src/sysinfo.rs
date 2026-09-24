//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! System information: CPU identity (vendor/brand/features), physical
//! memory summary stashed from the bootloader memory map, and last-app
//! accounting shared by `ps` and `top`.
//!
//! All helpers are `no_std` + `alloc` compatible. CPUID uses the same
//! `push rbx` / `pop rbx` pattern as the shell so PIC builds stay safe.

use alloc::string::String;
use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use spin::Mutex;

/// Maximum bootloader memory regions copied at boot.
pub const MAX_REGIONS: usize = 64;

/// Region-kind tags (mirrors `MemoryRegionKind` in bootloader_api 0.11).
pub const KIND_USABLE: u8 = 0;
pub const KIND_BOOTLOADER: u8 = 1;
pub const KIND_UNKNOWN_UEFI: u8 = 2;
pub const KIND_UNKNOWN_BIOS: u8 = 3;

/// Copied physical memory region (start inclusive, end exclusive).
#[derive(Clone, Copy)]
pub struct MemRegion {
    pub start: u64,
    pub end: u64,
    pub kind_tag: u8,
    pub kind_extra: u32,
}

impl MemRegion {
    pub const fn empty() -> Self {
        Self {
            start: 0,
            end: 0,
            kind_tag: KIND_BOOTLOADER,
            kind_extra: 0,
        }
    }

    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Aggregate physical memory totals computed at boot.
#[derive(Clone, Copy, Default)]
pub struct MemorySummary {
    pub total_bytes: u64,
    pub usable_bytes: u64,
    pub bootloader_bytes: u64,
    pub unknown_bytes: u64,
    pub region_count: usize,
    pub truncated: bool,
}

static MEM_SUMMARY: Mutex<Option<MemorySummary>> = Mutex::new(None);
static MEM_REGIONS: Mutex<([MemRegion; MAX_REGIONS], usize)> =
    Mutex::new(([MemRegion::empty(); MAX_REGIONS], 0));

fn tag_of(kind: &MemoryRegionKind) -> (u8, u32) {
    match *kind {
        MemoryRegionKind::Usable => (KIND_USABLE, 0),
        MemoryRegionKind::Bootloader => (KIND_BOOTLOADER, 0),
        MemoryRegionKind::UnknownUefi(v) => (KIND_UNKNOWN_UEFI, v),
        MemoryRegionKind::UnknownBios(v) => (KIND_UNKNOWN_BIOS, v),
        _ => (KIND_UNKNOWN_BIOS, 0xFFFF_FFFF),
    }
}

/// Copy the bootloader memory map into static storage.
///
/// Must be called once from `kernel_main` before the shell runs. Later
/// calls overwrite the previous snapshot.
pub fn stash_memory_map(regions: &MemoryRegions) {
    let mut total = 0u64;
    let mut usable = 0u64;
    let mut bootloader = 0u64;
    let mut unknown = 0u64;
    let mut guard = MEM_REGIONS.lock();
    let mut count = 0usize;
    let mut truncated = false;
    for r in regions.iter() {
        let len = r.end.saturating_sub(r.start);
        total = total.saturating_add(len);
        let (tag, extra) = tag_of(&r.kind);
        match tag {
            KIND_USABLE => usable = usable.saturating_add(len),
            KIND_BOOTLOADER => bootloader = bootloader.saturating_add(len),
            _ => unknown = unknown.saturating_add(len),
        }
        if count < MAX_REGIONS {
            guard.0[count] = MemRegion {
                start: r.start,
                end: r.end,
                kind_tag: tag,
                kind_extra: extra,
            };
            count += 1;
        } else {
            truncated = true;
        }
    }
    let full_count = regions.iter().count();
    guard.1 = count;
    *MEM_SUMMARY.lock() = Some(MemorySummary {
        total_bytes: total,
        usable_bytes: usable,
        bootloader_bytes: bootloader,
        unknown_bytes: unknown,
        region_count: full_count,
        truncated,
    });
}

/// Returns the stashed memory summary, if boot stashed it.
pub fn memory_summary() -> Option<MemorySummary> {
    *MEM_SUMMARY.lock()
}

/// Calls `f` with the copied region list.
pub fn with_regions<R>(f: impl FnOnce(&[MemRegion]) -> R) -> R {
    let guard = MEM_REGIONS.lock();
    f(&guard.0[..guard.1])
}

/// Short display name for a region-kind tag.
pub fn region_kind_name(tag: u8, extra: u32, buf: &mut [u8; 32]) -> &str {
    let s: &str = match tag {
        KIND_USABLE => "Usable",
        KIND_BOOTLOADER => "Bootloader",
        KIND_UNKNOWN_UEFI => "UnknownUefi",
        KIND_UNKNOWN_BIOS => "UnknownBios",
        _ => "Unknown",
    };
    if tag == KIND_UNKNOWN_UEFI || tag == KIND_UNKNOWN_BIOS {
        let mut len = 0usize;
        for &b in s.as_bytes() {
            if len < buf.len() {
                buf[len] = b;
                len += 1;
            }
        }
        // Append "(0xNN)" suffix.
        let hex = b"0123456789ABCDEF";
        let suffix: [u8; 12] = [
            b'(',
            b'0',
            b'x',
            hex[((extra >> 28) & 0xF) as usize],
            hex[((extra >> 24) & 0xF) as usize],
            hex[((extra >> 20) & 0xF) as usize],
            hex[((extra >> 16) & 0xF) as usize],
            hex[((extra >> 12) & 0xF) as usize],
            hex[((extra >> 8) & 0xF) as usize],
            hex[((extra >> 4) & 0xF) as usize],
            hex[(extra & 0xF) as usize],
            b')',
        ];
        for &b in suffix.iter() {
            if len < buf.len() {
                buf[len] = b;
                len += 1;
            }
        }
        core::str::from_utf8(&buf[..len]).unwrap_or("Unknown")
    } else {
        s
    }
}

// ── CPUID helpers ────────────────────────────────────────────

fn cpuid_subleaf(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let eax: u32;
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx:e}, ebx",
            "pop rbx",
            inout("eax") leaf => eax,
            ebx = out(reg) ebx,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
        );
    }
    (eax, ebx, ecx, edx)
}

/// Raw CPUID with subleaf 0. Returns (eax, ebx, ecx, edx).
fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    cpuid_subleaf(leaf, 0)
}

/// 12 raw vendor bytes in CPUID order (EBX, EDX, ECX).
pub fn cpu_vendor_bytes() -> [u8; 12] {
    let (_, ebx, ecx, edx) = cpuid(0);
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&ebx.to_le_bytes());
    out[4..8].copy_from_slice(&edx.to_le_bytes());
    out[8..12].copy_from_slice(&ecx.to_le_bytes());
    out
}

/// Vendor as printable string (non-printable bytes become `?`).
pub fn cpu_vendor_string() -> String {
    let raw = cpu_vendor_bytes();
    let mut s = String::with_capacity(12);
    for &b in raw.iter() {
        if (0x20..=0x7e).contains(&b) {
            s.push(b as char);
        } else {
            s.push('?');
        }
    }
    s
}

/// Maximum basic CPUID leaf (EAX from leaf 0).
pub fn cpu_max_basic() -> u32 {
    cpuid(0).0
}

/// Maximum extended CPUID leaf (EAX from leaf 0x80000000).
pub fn cpu_max_extended() -> u32 {
    cpuid(0x8000_0000).0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuTopology {
    pub logical_threads: u32,
    pub physical_cores: Option<u32>,
}

fn topology_from_counts(
    logical_threads: u32,
    has_htt: bool,
    threads_per_core: Option<u32>,
) -> CpuTopology {
    let logical_threads = if has_htt { logical_threads.max(1) } else { 1 };
    let physical_cores = if !has_htt {
        Some(1)
    } else {
        threads_per_core.and_then(|threads| {
            if threads == 0 || logical_threads % threads != 0 {
                None
            } else {
                Some(logical_threads / threads)
            }
        })
    };
    CpuTopology {
        logical_threads,
        physical_cores,
    }
}

pub fn cpu_topology() -> CpuTopology {
    let max_basic = cpu_max_basic();
    if max_basic < 1 {
        return CpuTopology {
            logical_threads: 1,
            physical_cores: Some(1),
        };
    }

    let (_, ebx, _, edx) = cpuid(1);
    let logical_threads = ((ebx >> 16) & 0xff) + 1;
    let has_htt = edx & (1 << 28) != 0;
    let threads_per_core = if max_basic >= 0x0000_000b {
        let (eax, ebx, _, _) = cpuid_subleaf(0x0000_000b, 0);
        if eax != 0 && eax & 0x1f == 1 {
            let count = ebx & 0xffff;
            if count == 0 {
                None
            } else {
                Some(count)
            }
        } else {
            None
        }
    } else {
        None
    };
    topology_from_counts(logical_threads, has_htt, threads_per_core)
}

/// 48-byte CPU brand string (leaves 0x80000002..04).
///
/// Returns `(bytes, display_len, present)`. `display_len` is trimmed of
/// trailing NULs/spaces and leading spaces so `&bytes[..len]` prints well.
/// When the CPU does not support the brand leaves, `present` is false.
pub fn cpu_brand() -> ([u8; 48], usize, bool) {
    let mut out = [0u8; 48];
    if cpu_max_extended() < 0x8000_0004 {
        return (out, 0, false);
    }
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
        .iter()
        .enumerate()
    {
        let (eax, ebx, ecx, edx) = cpuid(*leaf);
        out[i * 16..i * 16 + 4].copy_from_slice(&eax.to_le_bytes());
        out[i * 16 + 4..i * 16 + 8].copy_from_slice(&ebx.to_le_bytes());
        out[i * 16 + 8..i * 16 + 12].copy_from_slice(&ecx.to_le_bytes());
        out[i * 16 + 12..i * 16 + 16].copy_from_slice(&edx.to_le_bytes());
    }
    // Trim at first NUL, then trailing spaces, then leading spaces.
    let mut end = out.len();
    for (i, &b) in out.iter().enumerate() {
        if b == 0 {
            end = i;
            break;
        }
    }
    while end > 0 && (out[end - 1] == b' ' || out[end - 1] == 0) {
        end -= 1;
    }
    let mut start = 0;
    while start < end && out[start] == b' ' {
        start += 1;
    }
    if start > 0 {
        out.copy_within(start..end, 0);
        end -= start;
    }
    // Sanitize non-printable bytes in the display range.
    for b in out[..end].iter_mut() {
        if !((0x20..=0x7e).contains(b)) {
            *b = b'?';
        }
    }
    (out, end, true)
}

/// Decoded CPU signature with proper extended family/model handling.
///
/// Returns `(family, model, stepping, base_family, base_model, max_basic, max_ext)`.
pub fn cpu_signature() -> (u32, u32, u32, u32, u32, u32, u32) {
    let max_basic = cpu_max_basic();
    let max_ext = cpu_max_extended();
    let (eax, _, _, _) = cpuid(1);
    let stepping = eax & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let base_family = (eax >> 8) & 0xF;
    let ext_model = (eax >> 16) & 0xF;
    let ext_family = (eax >> 20) & 0xFF;
    let family = if base_family == 0xF {
        base_family + ext_family
    } else {
        base_family
    };
    let model = if base_family == 0x6 || base_family == 0xF {
        (ext_model << 4) | base_model
    } else {
        base_model
    };
    (
        family,
        model,
        stepping,
        base_family,
        base_model,
        max_basic,
        max_ext,
    )
}

/// Space-separated feature list from CPUID leaf 1 (EDX + ECX).
pub fn cpu_features() -> String {
    let (_, _, ecx, edx) = cpuid(1);
    let mut s = String::new();
    let mut first = true;
    let mut push = |name: &str, on: bool| {
        if on {
            if !first {
                s.push(' ');
            }
            s.push_str(name);
            first = false;
        }
    };
    // EDX bits.
    push("FPU", edx & (1 << 0) != 0);
    push("VME", edx & (1 << 1) != 0);
    push("DE", edx & (1 << 2) != 0);
    push("PSE", edx & (1 << 3) != 0);
    push("TSC", edx & (1 << 4) != 0);
    push("MSR", edx & (1 << 5) != 0);
    push("PAE", edx & (1 << 6) != 0);
    push("MCE", edx & (1 << 7) != 0);
    push("CX8", edx & (1 << 8) != 0);
    push("APIC", edx & (1 << 9) != 0);
    push("SEP", edx & (1 << 11) != 0);
    push("MTRR", edx & (1 << 12) != 0);
    push("PGE", edx & (1 << 13) != 0);
    push("MCA", edx & (1 << 14) != 0);
    push("CMOV", edx & (1 << 15) != 0);
    push("PAT", edx & (1 << 16) != 0);
    push("PSE36", edx & (1 << 17) != 0);
    push("CLFSH", edx & (1 << 19) != 0);
    push("MMX", edx & (1 << 23) != 0);
    push("FXSR", edx & (1 << 24) != 0);
    push("SSE", edx & (1 << 25) != 0);
    push("SSE2", edx & (1 << 26) != 0);
    push("HTT", edx & (1 << 28) != 0);
    // ECX bits.
    push("SSE3", ecx & (1 << 0) != 0);
    push("PCLMUL", ecx & (1 << 1) != 0);
    push("SSSE3", ecx & (1 << 9) != 0);
    push("SSE4.1", ecx & (1 << 19) != 0);
    push("SSE4.2", ecx & (1 << 20) != 0);
    push("AVX", ecx & (1 << 28) != 0);
    push("RDRAND", ecx & (1 << 30) != 0);
    if !first {
        s.push(' ');
    }
    s.push_str("x86_64");
    s
}

// ── Last-app accounting (shared by ps/top) ───────────────────

const LAST_PATH_MAX: usize = 64;

/// Snapshot of the most recently executed app.
#[derive(Clone)]
pub struct LastApp {
    pub path: [u8; LAST_PATH_MAX],
    pub path_len: usize,
    pub exit_code: i32,
    pub elapsed_ms: u64,
}

impl LastApp {
    pub const fn empty() -> Self {
        Self {
            path: [0; LAST_PATH_MAX],
            path_len: 0,
            exit_code: 0,
            elapsed_ms: 0,
        }
    }

    pub fn path_str(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("?")
    }
}

static LAST_APP: Mutex<Option<LastApp>> = Mutex::new(None);

/// Record an app exit (called from `app::run` for both MFKE and scripts).
pub fn record_app_exit(path: &str, exit_code: i32, elapsed_ms: u64) {
    let bytes = path.as_bytes();
    let n = core::cmp::min(bytes.len(), LAST_PATH_MAX);
    let mut app = LastApp::empty();
    app.path[..n].copy_from_slice(&bytes[..n]);
    app.path_len = n;
    app.exit_code = exit_code;
    app.elapsed_ms = elapsed_ms;
    *LAST_APP.lock() = Some(app);
}

/// Returns a copy of the last-app snapshot, if any app has run.
pub fn last_app() -> Option<LastApp> {
    LAST_APP.lock().clone()
}

#[cfg(test)]
mod tests {
    use super::topology_from_counts;

    #[test]
    fn topology_reports_single_threaded_cpu() {
        assert_eq!(
            topology_from_counts(1, false, None),
            super::CpuTopology {
                logical_threads: 1,
                physical_cores: Some(1),
            }
        );
    }

    #[test]
    fn topology_divides_threads_by_smt_width() {
        assert_eq!(
            topology_from_counts(8, true, Some(2)),
            super::CpuTopology {
                logical_threads: 8,
                physical_cores: Some(4),
            }
        );
    }

    #[test]
    fn topology_rejects_zero_smt_width() {
        assert_eq!(topology_from_counts(8, true, Some(0)).physical_cores, None);
    }

    #[test]
    fn topology_rejects_uneven_thread_count() {
        assert_eq!(topology_from_counts(7, true, Some(2)).physical_cores, None);
    }
}
