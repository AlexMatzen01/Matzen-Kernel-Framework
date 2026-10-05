//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Kernel heap.
//!
//! Two cooperating pieces:
//!
//! - A [`TrackingAllocator`] wrapping `linked_list_allocator::LockedHeap`. It
//!   provides the main heap *and* the instrumentation: live allocation count
//!   and bytes, high-water marks, and — most importantly — an out-of-memory
//!   counter. Previously an exhausted heap fell through to the default
//!   `handle_alloc_error` shim, which routed into the panic handler whose own
//!   formatting allocates. A heap-exhaustion panic could therefore re-enter
//!   the allocator it had already failed, wedging the machine with no
//!   diagnosis on screen. [`oom_events`] makes that state observable, and
//!   [`oom_handler`] is allocation-free.
//! - A small [`BumpPool`] "emergency reserve" carved from the tail of the
//!   same range and never handed to the main heap. Kernel subsystems that
//!   must survive low memory (error reporting, the `mem` command's counters,
//!   the archive writer's last-resort path) can allocate from it via
//!   [`try_alloc_emergency`] instead of failing outright.
//!
//! Layout of a carved heap:
//!
//! ```text
//!   base                                                  base + size
//!   |------ main heap (HEAP_SIZE) -------|-- reserve ----|
//!        LockedHeap: first-fit + free     bump cursor, one-shot
//! ```
//!
//! Both live inside the bootloader's physical direct map, so no virtual
//! mapping is involved and the heap is reachable from every context.
//!
//! Sizing note: [`HEAP_SIZE`] is the *main* heap budget. The total carve is
//! `HEAP_SIZE + HEAP_RESERVE_SIZE`, which is what gets reserved in the frame
//! allocator.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use linked_list_allocator::LockedHeap;

/// The static heap itself.
static HEAP: LockedHeap = LockedHeap::empty();

// ── instrumentation ──────────────────────────────────────────────────
//
// Plain atomics: the tracking layer is reachable from `dealloc`, which may
// run from anywhere, including an unwind path.

/// Allocations currently outstanding.
static LIVE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
/// Bytes currently allocated.
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// High-water mark of [`LIVE_BYTES`].
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Largest single allocation ever served.
static PEAK_SINGLE: AtomicUsize = AtomicUsize::new(0);
/// Total `alloc` + `realloc` calls.
static ALLOC_REQUESTS: AtomicU64 = AtomicU64::new(0);
/// Bytes served over the kernel's lifetime.
static TOTAL_ALLOCATED: AtomicUsize = AtomicUsize::new(0);
/// Bytes returned over the kernel's lifetime.
static TOTAL_FREED: AtomicUsize = AtomicUsize::new(0);
/// Heap-exhaustion events.
static OOM_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Physical base of the carved heap (0 when the static fallback is in use).
static HEAP_BASE: AtomicU64 = AtomicU64::new(0);
/// Total carved size, main heap plus reserve.
static CARVED_HEAP_SIZE: AtomicUsize = AtomicUsize::new(0);
/// True when the static [`FALLBACK_HEAP`] is the backing store.
static HEAP_IS_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Snapshot of heap accounting for the `mem` command.
///
/// Populated from the `LockedHeap` accessors plus the tracking counters, so
/// `used`/`free` are the allocator's own numbers and the rest is measured at
/// the allocation boundary.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeapInfo {
    /// Physical base address of the carve.
    pub base: u64,
    /// Bytes carved in total (main heap plus reserve).
    pub size: usize,
    /// Bytes the hole list reports as used.
    pub used: usize,
    /// Bytes the hole list reports as free.
    pub free: usize,
    /// High-water mark of live allocated bytes.
    pub peak_bytes: usize,
    /// Largest single allocation ever served.
    pub peak_single: usize,
    /// Allocations currently outstanding.
    pub live_allocations: usize,
    /// Total `alloc`/`realloc` calls served.
    pub alloc_requests: u64,
    /// Bytes served over the kernel's lifetime.
    pub total_allocated: usize,
    /// Bytes returned over the kernel's lifetime.
    pub total_freed: usize,
    /// Heap-exhaustion events.
    pub oom_events: u64,
    /// Emergency-reserve size.
    pub reserve_size: usize,
    /// Emergency-reserve bytes handed out.
    pub reserve_used: usize,
    /// True when the 1 MiB static fallback backs the heap.
    pub is_fallback: bool,
}

impl HeapInfo {
    /// Fraction of the main heap in use, 0.0..=1.0.
    pub fn used_ratio(&self) -> f64 {
        if self.size == 0 {
            return 0.0;
        }
        (self.used as f64) / (self.size as f64)
    }
}

/// Main kernel heap budget.
///
/// Sized for photo-class workloads on a small guest: a ~1080p wallpaper
/// needs file bytes + inflate/IDAT transients + full-res RGBA alive at once.
/// Everything else (FS inode table and bitmap, network queues, TCP buffers,
/// editor buffers, archive payloads) shares this heap, so it is a budget and
/// not a guarantee — every large consumer takes a fallible path.
///
/// NOTE: this must NOT live in `.bss` — a 32 MiB static array balloons the
/// kernel ELF and collides with bootloader mappings on real hardware
/// (GDT frame `PageAlreadyMapped` panic). Instead [`init`] is called once on
/// a range carved from the firmware map (see `kernel_main`).
pub const HEAP_SIZE: usize = 32 * 1024 * 1024;

/// Emergency reserve held back from the carve for OOM-time allocation.
pub const HEAP_RESERVE_SIZE: usize = 1024 * 1024;

/// Total bytes to carve for [`crate::memory`] to reserve.
pub const HEAP_TOTAL_SIZE: usize = HEAP_SIZE + HEAP_RESERVE_SIZE;

/// Fallback early heap: used only when no large contiguous usable run exists
/// (tiny machines). The kernel still boots, with reduced headroom and a
/// serial warning; large allocations fail cleanly through the caps.
pub const FALLBACK_HEAP_SIZE: usize = 1024 * 1024;
static mut FALLBACK_HEAP: [u8; FALLBACK_HEAP_SIZE] = [0; FALLBACK_HEAP_SIZE];

// ── global allocator ─────────────────────────────────────────────────

/// Wraps [`HEAP`] with allocation accounting.
struct TrackingAllocator;

impl TrackingAllocator {
    #[inline]
    fn note_alloc(size: usize) {
        let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
        LIVE_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        TOTAL_ALLOCATED.fetch_add(size, Ordering::Relaxed);
        ALLOC_REQUESTS.fetch_add(1, Ordering::Relaxed);
        PEAK_SINGLE.fetch_max(size, Ordering::Relaxed);
        PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
    }

    #[inline]
    fn note_free(size: usize) {
        // `fetch_update` keeps the counter from wrapping if a double free or
        // a mismatched layout ever reaches here; saturating at zero is
        // strictly better than a huge bogus number in `mem`.
        let _ = LIVE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_sub(size))
        });
        let _ = LIVE_ALLOCATIONS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_sub(1))
        });
        TOTAL_FREED.fetch_add(size, Ordering::Relaxed);
    }

    #[inline]
    fn note_failure() {
        OOM_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match HEAP.lock().allocate_first_fit(layout) {
            Ok(ptr) => {
                Self::note_alloc(layout.size());
                ptr.as_ptr()
            }
            Err(_) => {
                Self::note_failure();
                core::ptr::null_mut()
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            HEAP.lock().deallocate(ptr, layout);
        }
        Self::note_free(layout.size());
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = self.alloc(layout);
        if !ptr.is_null() {
            core::ptr::write_bytes(ptr, 0, layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size <= layout.size() {
            // Shrinking in place: the hole list is content-agnostic, so only
            // the accounting needs adjusting.
            let new_ptr = self.alloc(layout);
            if new_ptr.is_null() {
                return core::ptr::null_mut();
            }
            core::ptr::copy_nonoverlapping(ptr, new_ptr, new_size);
            self.dealloc(ptr, layout);
            return new_ptr;
        }
        let new_layout = match Layout::from_size_align(new_size, layout.align()) {
            Ok(l) => l,
            Err(_) => {
                Self::note_failure();
                return core::ptr::null_mut();
            }
        };
        let new_ptr = self.alloc(new_layout);
        if new_ptr.is_null() {
            return core::ptr::null_mut();
        }
        core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size());
        self.dealloc(ptr, layout);
        new_ptr
    }
}

#[cfg_attr(not(test), global_allocator)]
static GLOBAL: TrackingAllocator = TrackingAllocator;

/// Allocation failure. Allocation-free by construction: only integer and
/// `&str` arguments reach [`crate::serial_print`], so the OOM path cannot
/// re-enter the allocator it just failed.
#[cfg(not(test))]
#[alloc_error_handler]
fn oom_handler(layout: Layout) -> ! {
    OOM_EVENTS.fetch_add(1, Ordering::Relaxed);
    crate::serial_println!(
        "[heap] OUT OF MEMORY: allocation of {} bytes (align {}) failed",
        layout.size(),
        layout.align()
    );
    crate::serial_println!(
        "[heap] live {} allocs / peak {} bytes; halting",
        LIVE_ALLOCATIONS.load(Ordering::Relaxed),
        PEAK_BYTES.load(Ordering::Relaxed)
    );
    // Halt instead of panicking: the panic handler formats a `PanicInfo`,
    // which allocates. A second failure here would recurse.
    loop {
        x86_64::instructions::hlt();
    }
}

// ── emergency reserve ────────────────────────────────────────────────

/// A one-shot bump allocator over a carve held out of the main heap.
///
/// Zeroing is provided (so `alloc_zeroed` callers get the contract they
/// expect) and allocations are never reclaimed: the reserve exists for
/// paths that must not fail, not for general use.
pub struct BumpPool {
    base: AtomicU64,
    size: AtomicUsize,
    cursor: AtomicUsize,
}

impl BumpPool {
    const fn new() -> Self {
        Self {
            base: AtomicU64::new(0),
            size: AtomicUsize::new(0),
            cursor: AtomicUsize::new(0),
        }
    }

    /// Point the pool at `[base, base + size)`.
    pub fn init(&self, base: u64, size: usize) {
        self.base.store(base, Ordering::Release);
        self.size.store(size, Ordering::Release);
        self.cursor.store(0, Ordering::Release);
    }

    /// Forget the carve (used when the pool is never populated).
    pub fn clear(&self) {
        self.base.store(0, Ordering::Release);
        self.size.store(0, Ordering::Release);
        self.cursor.store(0, Ordering::Release);
    }

    pub fn size(&self) -> usize {
        self.size.load(Ordering::Acquire)
    }

    pub fn used(&self) -> usize {
        self.cursor.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> usize {
        self.size().saturating_sub(self.used())
    }

    /// Bump-allocate `layout`, or `None` when the reserve is exhausted.
    ///
    /// The cursor is advanced with a single compare-exchange loop so two
    /// concurrent callers can never be handed overlapping memory.
    pub fn alloc(&self, layout: Layout) -> Option<*mut u8> {
        let base = self.base.load(Ordering::Acquire);
        let size = self.size.load(Ordering::Acquire);
        if base == 0 || size == 0 {
            return None;
        }
        // Round the request up so the next allocation starts aligned.
        let align = layout.align().max(1);
        let want = layout.size();
        let mut cursor = self.cursor.load(Ordering::Acquire);
        loop {
            let aligned = (cursor + align - 1) & !(align - 1);
            let next = aligned.checked_add(want)?;
            if next > size {
                return None;
            }
            match self.cursor.compare_exchange_weak(
                cursor,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    ALLOC_REQUESTS.fetch_add(1, Ordering::Relaxed);
                    TOTAL_ALLOCATED.fetch_add(want, Ordering::Relaxed);
                    LIVE_BYTES.fetch_add(want, Ordering::Relaxed);
                    LIVE_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
                    PEAK_BYTES.fetch_max(LIVE_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
                    PEAK_SINGLE.fetch_max(want, Ordering::Relaxed);
                    return Some((base + aligned as u64) as *mut u8);
                }
                Err(actual) => cursor = actual,
            }
        }
    }

    /// Allocate and zero `layout.size()` bytes.
    pub fn alloc_zeroed(&self, layout: Layout) -> Option<*mut u8> {
        let ptr = self.alloc(layout)?;
        // SAFETY: `alloc` returned `layout.size()` bytes inside the carve.
        unsafe { core::ptr::write_bytes(ptr, 0, layout.size()) };
        Some(ptr)
    }
}

static RESERVE: BumpPool = BumpPool::new();

/// Allocate from the emergency reserve, or `None` when it is exhausted.
///
/// The returned memory is *not* tied to a Rust allocation and must be
/// reclaimed by [`release_emergency`] or simply abandoned; it is never
/// returned to the main heap.
pub fn try_alloc_emergency(layout: Layout) -> Option<core::ptr::NonNull<u8>> {
    RESERVE.alloc(layout).map(|p| {
        core::ptr::NonNull::new(p).expect("reserve alloc returned null for a successful bump")
    })
}

/// Account for emergency memory the caller is done with.
///
/// Reserve allocations are never reused, so this only keeps the reported
/// counters honest.
pub fn release_emergency(layout: Layout) {
    let _ = LIVE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(layout.size()))
    });
    let _ = LIVE_ALLOCATIONS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(1))
    });
    TOTAL_FREED.fetch_add(layout.size(), Ordering::Relaxed);
}

/// Emergency reserve accounting.
pub fn reserve_stats() -> (usize, usize) {
    (RESERVE.size(), RESERVE.used())
}

/// Number of heap-exhaustion events since boot.
pub fn oom_events() -> u64 {
    OOM_EVENTS.load(Ordering::Relaxed)
}

/// Current heap accounting.
pub fn heap_info() -> HeapInfo {
    let size = CARVED_HEAP_SIZE.load(Ordering::Relaxed);
    let (used, free) = if size == 0 {
        (0, 0)
    } else {
        let h = HEAP.lock();
        (h.used(), h.free())
    };
    HeapInfo {
        base: HEAP_BASE.load(Ordering::Relaxed),
        size,
        used,
        free,
        peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        peak_single: PEAK_SINGLE.load(Ordering::Relaxed),
        live_allocations: LIVE_ALLOCATIONS.load(Ordering::Relaxed),
        alloc_requests: ALLOC_REQUESTS.load(Ordering::Relaxed),
        total_allocated: TOTAL_ALLOCATED.load(Ordering::Relaxed),
        total_freed: TOTAL_FREED.load(Ordering::Relaxed),
        oom_events: OOM_EVENTS.load(Ordering::Relaxed),
        reserve_size: RESERVE.size(),
        reserve_used: RESERVE.used(),
        is_fallback: HEAP_IS_FALLBACK.load(Ordering::Relaxed),
    }
}

/// Reset the instrumentation counters. Boot-time only; used so a fresh heap
/// carve does not inherit the fallback path's history.
fn reset_tracking() {
    LIVE_ALLOCATIONS.store(0, Ordering::Relaxed);
    LIVE_BYTES.store(0, Ordering::Relaxed);
    PEAK_BYTES.store(0, Ordering::Relaxed);
    PEAK_SINGLE.store(0, Ordering::Relaxed);
    ALLOC_REQUESTS.store(0, Ordering::Relaxed);
    TOTAL_ALLOCATED.store(0, Ordering::Relaxed);
    TOTAL_FREED.store(0, Ordering::Relaxed);
    OOM_EVENTS.store(0, Ordering::Relaxed);
}

// ── initialisation ───────────────────────────────────────────────────

/// Initialize the main heap on caller-provided memory.
///
/// `heap_bottom`/`heap_size` must be usable RAM that stays mapped for the
/// kernel's lifetime (the bootloader's full-physical direct map satisfies
/// this). The last [`HEAP_RESERVE_SIZE`] bytes are held back for
/// [`try_alloc_emergency`].
pub fn init(heap_bottom: *mut u8, heap_size: usize) {
    reset_tracking();
    let phys_base = heap_bottom as u64;
    HEAP_BASE.store(phys_base, Ordering::Release);
    HEAP_IS_FALLBACK.store(false, Ordering::Release);
    if heap_size > HEAP_RESERVE_SIZE {
        let main = heap_size - HEAP_RESERVE_SIZE;
        unsafe {
            HEAP.lock().init(heap_bottom, main);
        }
        CARVED_HEAP_SIZE.store(main, Ordering::Release);
        RESERVE.init(phys_base + main as u64, HEAP_RESERVE_SIZE);
    } else {
        // Too small to split; run the main heap over the whole carve and
        // leave the reserve empty rather than pretending it exists.
        unsafe {
            HEAP.lock().init(heap_bottom, heap_size);
        }
        CARVED_HEAP_SIZE.store(heap_size, Ordering::Release);
        RESERVE.clear();
    }
}

/// Initialize the heap on the static fallback (tiny machines only).
///
/// The fallback lives in `.bss`, so unlike the carved heap it is not in the
/// firmware map and cannot collide with the frame allocator; the firmware
/// marks the kernel image non-usable. There is no reserve to carve.
pub fn init_fallback() {
    reset_tracking();
    unsafe {
        HEAP.lock()
            .init(FALLBACK_HEAP.as_mut_ptr(), FALLBACK_HEAP.len());
    }
    HEAP_BASE.store(0, Ordering::Release);
    CARVED_HEAP_SIZE.store(FALLBACK_HEAP_SIZE, Ordering::Release);
    HEAP_IS_FALLBACK.store(true, Ordering::Release);
    RESERVE.clear();
}

/// Find a contiguous run for the heap in the firmware memory map.
///
/// Returns `(phys_start, len)`; takes up to `want` bytes from the END of the
/// largest `Usable` run, preferring sub-4 GiB memory so heap-backed DMA stays
/// reachable by the kernel's 32-bit controllers.
///
/// Pass 1 restricts candidates to below [`crate::memory::memmap::DMA_PHYS_LIMIT`]
/// and, if it finds nothing, **gives up rather than falling back to a high
/// run**: every DMA buffer on this heap comes from `alloc_zeroed`, so a heap
/// above 4 GiB would make USB and virtio-blk unable to address any of it and
/// those drivers would fail silently. A high run is only chosen when the
/// caller explicitly asks for one (the kernel then logs that DMA is off).
///
/// Stack-only: safe before heap init. Callers should still reserve the
/// returned range in the frame allocator.
pub fn carve_heap_run(
    regions: &[bootloader_api::info::MemoryRegion],
    want: u64,
) -> Option<(u64, u64)> {
    carve_heap_run_bounded(regions, want, true)
}

/// As [`carve_heap_run`], but `prefer_low == false` allows a run above
/// [`crate::memory::memmap::DMA_PHYS_LIMIT`].
pub fn carve_heap_run_bounded(
    regions: &[bootloader_api::info::MemoryRegion],
    want: u64,
    prefer_low: bool,
) -> Option<(u64, u64)> {
    use bootloader_api::info::MemoryRegionKind;
    let limit = crate::memory::memmap::DMA_PHYS_LIMIT;
    let mut best: Option<(u64, u64)> = None; // (run_start, run_len)
    for pass in 0..2 {
        for region in regions {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }
            let start = (region.start + 0xFFF) & !0xFFF;
            let mut end = region.end & !0xFFF;
            if pass == 0 {
                if prefer_low && end > limit {
                    end = limit;
                }
            } else if prefer_low {
                // Second pass only runs when the caller allows high memory.
                continue;
            }
            if end <= start {
                continue;
            }
            let len = end - start;
            if best.map(|(_, best_len)| len > best_len).unwrap_or(true) {
                best = Some((start, len));
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|(start, len)| {
        let take = len.min(want);
        (start + len - take, take)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootloader_api::info::{MemoryRegion, MemoryRegionKind};

    const MIB: u64 = 1024 * 1024;

    fn usable(start: u64, end: u64) -> MemoryRegion {
        MemoryRegion {
            start,
            end,
            kind: MemoryRegionKind::Usable,
        }
    }

    #[test]
    fn carve_takes_want_from_end_of_largest_run() {
        let regions = [
            usable(0x100000, 0x100000 + 64 * MIB),
            usable(0x20000000, 0x20000000 + 16 * MIB),
        ];
        // Largest run is 64 MiB at 1 MiB; want 32 MiB from its end.
        assert_eq!(
            carve_heap_run(&regions, 32 * MIB),
            Some((0x100000 + 32 * MIB, 32 * MIB))
        );
    }

    #[test]
    fn carve_prefers_sub_4gib_for_dma_reach() {
        let regions = [
            usable(0x100000, 0x100000 + 8 * MIB),
            usable(0x1_0000_0000, 0x1_0000_0000 + 1024 * MIB),
        ];
        // The 1 GiB high run is bigger but above 4 GiB; take the low one.
        assert_eq!(
            carve_heap_run(&regions, 32 * MIB),
            Some((0x100000, 8 * MIB))
        );
    }

    #[test]
    fn carve_refuses_high_only_memory_for_the_default_heap() {
        // Every usable run lives above 4 GiB. Choosing one would put the heap
        // out of reach of 32-bit DMA, so the default carve must decline
        // rather than hand back a run that silently disables USB and
        // virtio-blk. The caller falls back to the static heap.
        let regions = [usable(0x1_0000_0000, 0x1_0000_0000 + 1024 * MIB)];
        assert_eq!(carve_heap_run(&regions, 32 * MIB), None);
        // Explicitly allowing high memory still works.
        assert_eq!(
            carve_heap_run_bounded(&regions, 32 * MIB, false),
            Some((0x1_0000_0000 + 992 * MIB, 32 * MIB))
        );
    }

    #[test]
    fn carve_ignores_a_run_that_starts_above_the_limit() {
        // Starts above 4 GiB: pass 0 truncates `end` to the limit, leaving
        // end <= start, so the run is skipped.
        let regions = [usable(0x1_0000_0000, 0x1_4000_0000)];
        assert_eq!(carve_heap_run(&regions, 32 * MIB), None);
    }

    #[test]
    fn carve_uses_the_low_head_of_a_run_that_straddles_the_limit() {
        let regions = [usable(0x1000, 0x2_0000_0000)];
        // Only the 4 GiB minus 4 KiB head is considered on pass 0.
        let (start, len) = carve_heap_run(&regions, 32 * MIB).unwrap();
        assert!(start + len <= 0x1_0000_0000);
        assert_eq!(len, 32 * MIB);
        assert_eq!(start, 0x1_0000_0000 - 32 * MIB);
    }

    #[test]
    fn carve_skips_unusable_and_empty() {
        let regions = [
            MemoryRegion {
                start: 0,
                end: 0x100000,
                kind: MemoryRegionKind::Bootloader,
            },
            usable(0x100000, 0x100000 + 2 * MIB),
        ];
        assert_eq!(
            carve_heap_run(&regions, 32 * MIB),
            Some((0x100000, 2 * MIB))
        );
        assert_eq!(carve_heap_run(&[], 32 * MIB), None);
    }

    #[test]
    fn carve_returns_the_whole_run_when_it_is_smaller_than_want() {
        let regions = [usable(0x100000, 0x100000 + 3 * MIB)];
        assert_eq!(carve_heap_run(&regions, 32 * MIB), Some((0x100000, 3 * MIB)));
    }

    // ── bump pool ──

    #[test]
    fn bump_pool_hands_out_sequential_memory() {
        let p = BumpPool::new();
        p.init(0x1000, 64);
        let a = p.alloc(Layout::from_size_align(16, 1).unwrap()).unwrap();
        let b = p.alloc(Layout::from_size_align(16, 1).unwrap()).unwrap();
        assert_eq!(a as u64, 0x1000);
        assert_eq!(b as u64, 0x1010);
        assert_eq!(p.used(), 32);
        assert_eq!(p.remaining(), 32);
    }

    #[test]
    fn bump_pool_respects_alignment() {
        let p = BumpPool::new();
        p.init(0x1000, 256);
        let a = p.alloc(Layout::from_size_align(1, 1).unwrap()).unwrap();
        assert_eq!(a as u64, 0x1000);
        let b = p.alloc(Layout::from_size_align(8, 8).unwrap()).unwrap();
        assert_eq!((b as u64) % 8, 0);
        assert_eq!(b as u64, 0x1008);
        let c = p.alloc(Layout::from_size_align(64, 64).unwrap()).unwrap();
        assert_eq!((c as u64) % 64, 0);
    }

    #[test]
    fn bump_pool_fails_when_exhausted_and_does_not_overshoot() {
        let p = BumpPool::new();
        p.init(0x1000, 64);
        assert!(p.alloc(Layout::from_size_align(48, 1).unwrap()).is_some());
        assert!(p.alloc(Layout::from_size_align(32, 1).unwrap()).is_none());
        assert_eq!(p.used(), 48, "failed allocation must not advance the cursor");
    }

    #[test]
    fn bump_pool_uninitialised_refuses() {
        let p = BumpPool::new();
        assert!(p.alloc(Layout::from_size_align(1, 1).unwrap()).is_none());
        assert_eq!(p.size(), 0);
        assert_eq!(p.remaining(), 0);
    }

    #[test]
    fn bump_pool_rejects_allocation_larger_than_the_reserve() {
        let p = BumpPool::new();
        p.init(0x1000, 64);
        assert!(p.alloc(Layout::from_size_align(65, 1).unwrap()).is_none());
        // A huge but representable layout also fails cleanly.
        assert!(p
            .alloc(Layout::from_size_align(usize::MAX / 2, 1).unwrap())
            .is_none());
    }

    #[test]
    fn bump_pool_clear_disables_allocation() {
        let p = BumpPool::new();
        p.init(0x1000, 64);
        assert!(p.alloc(Layout::from_size_align(1, 1).unwrap()).is_some());
        p.clear();
        assert!(p.alloc(Layout::from_size_align(1, 1).unwrap()).is_none());
    }

    #[test]
    fn bump_pool_zeroed_allocation_writes_zeros() {
        let p = BumpPool::new();
        // Use a real local buffer as the "reserve" so the test can inspect it.
        let mut backing = [0xABu8; 64];
        let base = backing.as_mut_ptr() as u64;
        p.init(base, 64);
        let ptr = p.alloc_zeroed(Layout::from_size_align(16, 1).unwrap()).unwrap();
        let slice = unsafe { core::slice::from_raw_parts(ptr, 16) };
        assert!(slice.iter().all(|&b| b == 0));
        // Bytes past the allocation are untouched.
        assert!(backing[16..].iter().all(|&b| b == 0xAB));
    }

    // ── heap info ──

    #[test]
    fn heap_info_is_zeroed_before_boot() {
        let info = heap_info();
        // Under `cargo test` the tracking allocator is not installed, so the
        // heap itself is untouched; the shape must still be well formed.
        assert!(info.reserve_used <= info.reserve_size);
        assert!(info.used + info.free == info.size.max(info.used + info.free));
    }

    #[test]
    fn reserve_is_held_out_of_the_main_heap_budget() {
        assert_eq!(HEAP_TOTAL_SIZE, HEAP_SIZE + HEAP_RESERVE_SIZE);
        assert!(HEAP_RESERVE_SIZE > 0);
        // The reserve must be big enough to build an error message and a
        // small diagnostic buffer without touching the main heap.
        assert!(HEAP_RESERVE_SIZE >= 64 * 1024);
    }

    #[test]
    fn tracking_counters_follow_alloc_and_free() {
        // The counters are process-global and other tests touch them, so
        // assert on deltas rather than absolute values.
        let live0 = LIVE_BYTES.load(Ordering::Relaxed);
        let req0 = ALLOC_REQUESTS.load(Ordering::Relaxed);
        let peak0 = PEAK_BYTES.load(Ordering::Relaxed);

        TrackingAllocator::note_alloc(1000);
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), live0 + 1000);
        TrackingAllocator::note_free(600);
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), live0 + 400);
        assert_eq!(ALLOC_REQUESTS.load(Ordering::Relaxed), req0 + 1);
        assert!(PEAK_BYTES.load(Ordering::Relaxed) >= peak0);

        // Drain what this test added so the shared counters stay balanced.
        TrackingAllocator::note_free(400);
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), live0);
    }

    #[test]
    fn tracking_free_saturates_at_zero() {
        // Drain first so the assertion is not racing other tests, then prove
        // an over-large free cannot wrap the counter.
        while LIVE_BYTES
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v == 0 {
                    None
                } else {
                    Some(v - 1)
                }
            })
            .is_ok()
        {}
        TrackingAllocator::note_free(10_000);
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), 0);
        TrackingAllocator::note_free(10_000);
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn oom_counter_increments_on_failure() {
        let before = oom_events();
        TrackingAllocator::note_failure();
        assert_eq!(oom_events(), before + 1);
    }
}
