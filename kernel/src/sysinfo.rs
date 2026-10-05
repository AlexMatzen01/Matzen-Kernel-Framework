//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! System information: CPU identity (vendor/brand/features/address width),
//! the interpreted physical memory layout, and last-app accounting.
//!
//! All helpers are `no_std` + `alloc` compatible. CPUID uses the same
//! `push rbx` / `pop rbx` pattern as the shell so PIC builds stay safe.
//!
//! The memory half is a thin snapshot layer over
//! [`crate::memory::memmap::MemoryLayout`]: `kernel_main` builds the layout
//! once (classifying every firmware region and subtracting kernel-owned
//! spans) and hands it to [`stash_memory_layout`], which keeps it available
//! for the `mem` command. Nothing here re-derives totals, so what the shell
//! reports and what the frame allocator was given cannot drift apart.

use alloc::string::String;
use spin::Mutex;

use crate::memory::memmap;

/// Snapshot of the interpreted memory map.
static MEM_LAYOUT: Mutex<Option<memmap::MemoryLayout>> = Mutex::new(None);

/// Store the interpreted memory layout. Called once from `kernel_main`
/// before the shell runs; later calls replace the snapshot.
pub fn stash_memory_layout(layout: &memmap::MemoryLayout) {
    *MEM_LAYOUT.lock() = Some(layout.clone());
}

/// The stashed layout, if boot produced one.
pub fn memory_layout() -> Option<memmap::MemoryLayout> {
    MEM_LAYOUT.lock().clone()
}

/// Aggregate totals, or all-zero before boot stashes the layout.
pub fn memory_summary() -> memmap::MemoryLayout {
    MEM_LAYOUT.lock().clone().unwrap_or_default()
}

/// Aggregate physical memory totals computed at boot.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySummary {
    /// Bytes described by the firmware map.
    pub total_bytes: u64,
    /// Bytes that are RAM rather than a PCI MMIO / port-space window.
    pub ram_bytes: u64,
    /// Bytes in memory-mapped I/O windows (excluded from `ram_bytes`).
    pub mmio_bytes: u64,
    /// Bytes the kernel may allocate from, before reservations.
    pub usable_bytes: u64,
    /// Allocatable bytes a 32-bit DMA controller can reach.
    pub usable_low_bytes: u64,
    /// Allocatable bytes above the 32-bit DMA limit.
    pub usable_high_bytes: u64,
    /// Bytes the kernel reserves for itself.
    pub reserved_bytes: u64,
    /// Allocatable bytes after reservations.
    pub allocatable_bytes: u64,
    /// Retained regions listed.
    pub region_count: usize,
    /// Regions dropped by the [`memmap::MAX_REGIONS`] cap.
    pub truncated: bool,
}

impl From<&memmap::MemoryLayout> for MemorySummary {
    fn from(l: &memmap::MemoryLayout) -> Self {
        Self {
            total_bytes: l.total_bytes,
            ram_bytes: l.ram_bytes(),
            mmio_bytes: l.mmio_bytes,
            usable_bytes: l.allocatable_bytes,
            usable_low_bytes: l.allocatable_low_bytes,
            usable_high_bytes: l.allocatable_high_bytes,
            reserved_bytes: l.boot_owned_bytes,
            allocatable_bytes: l.allocatable_after_reservations,
            region_count: l.regions.len(),
            truncated: l.truncated,
        }
    }
}

/// Summary of the stashed layout, if boot stashed one.
pub fn memory_summary_opt() -> Option<MemorySummary> {
    MEM_LAYOUT.lock().as_ref().map(MemorySummary::from)
}

/// Calls `f` with the retained region list.
pub fn with_regions<R>(f: impl FnOnce(&[memmap::LayoutRegion]) -> R) -> R {
    let guard = MEM_LAYOUT.lock();
    match guard.as_ref() {
        Some(l) => f(&l.regions),
        None => f(&[]),
    }
}

/// Calls `f` with the kernel-owned reservations.
pub fn with_reservations<R>(f: impl FnOnce(&[memmap::Reservation]) -> R) -> R {
    let guard = MEM_LAYOUT.lock();
    match guard.as_ref() {
        Some(l) => f(&l.reservations),
        None => f(&[]),
    }
}

/// Short display name for a region class, including the firmware tag when
/// the region arrived as an `Unknown*` kind.
pub fn region_kind_name(
    class: memmap::RegionClass,
    firmware_tag: u32,
    buf: &mut [u8; 32],
) -> &str {
    if firmware_tag == 0 {
        return class.label();
    }
    let base = class.label().as_bytes();
    let mut len = 0usize;
    for &b in base {
        if len < buf.len() {
            buf[len] = b;
            len += 1;
        }
    }
    // Append a compact "(0xNN)" tag; truncation keeps the last digit rather
    // than silently cutting mid-number.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digits = if firmware_tag > 0xFF {
        4
    } else if firmware_tag > 0xF {
        3
    } else {
        2
    };
    let mut suffix = [0u8; 8];
    let mut n = 0usize;
    suffix[n] = b'(';
    n += 1;
    suffix[n] = b'0';
    n += 1;
    suffix[n] = b'x';
    n += 1;
    for i in (0..digits).rev() {
        suffix[n] = HEX[((firmware_tag >> (i * 4)) & 0xF) as usize];
        n += 1;
    }
    suffix[n] = b')';
    n += 1;
    for &b in suffix[..n].iter() {
        if len < buf.len() {
            buf[len] = b;
            len += 1;
        }
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("unknown")
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

/// Physical address width reported by the CPU.
///
/// `CPUID.0x80000008:EAX[7:0]` is `MAXPHYADDR`, the number of bits of
/// physical address the CPU implements; `[8:15]` is
/// `PHYSICAL_ADDRESS_BITS_EXTENSION`, set when the width exceeds 52 bits.
///
/// This is the ceiling every physical address in the kernel must respect: a
/// 64-bit PCI BAR whose base exceeds this width cannot be addressed at all,
/// and the MMIO mapper has no way to express it. Without it nothing in the
/// kernel knows how wide physical memory actually is.
///
/// `None` when the extended leaf is unavailable, in which case the x86-64
/// baseline of 40 bits (1 TiB) applies.
pub fn cpu_max_phys_addr_bits() -> Option<u32> {
    if cpu_max_extended() < 0x8000_0008 {
        return None;
    }
    let (eax, _, _, _) = cpuid(0x8000_0008);
    let bits = eax & 0xFF;
    if bits == 0 {
        return None;
    }
    let extended = ((eax >> 8) & 0xFF) != 0;
    Some(if extended { bits + 32 } else { bits })
}

/// Highest physical address the CPU can address, or `None` when unknown.
pub fn cpu_max_physical_address() -> Option<u64> {
    cpu_max_phys_addr_bits()
        .and_then(|bits| {
            if bits >= 64 {
                Some(u64::MAX)
            } else {
                Some((1u64 << bits) - 1)
            }
        })
}

/// Physical address width, defaulting to the x86-64 baseline when the CPU
/// does not report one. Always safe to use for range checks.
pub fn cpu_phys_addr_limit() -> u64 {
    cpu_max_physical_address().unwrap_or((1u64 << 40) - 1)
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
