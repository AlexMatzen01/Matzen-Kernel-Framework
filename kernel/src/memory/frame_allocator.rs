//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Physical frame allocator.
//!
//! Bitmap-backed, with free, counters and a DMA-reachable pool. The previous
//! implementation was a bump cursor: it could never hand back a frame, kept
//! no bookkeeping at all, and (because it walked regions in ascending
//! address order with no upper bound) would eventually return frames above
//! 4 GiB that the kernel's 32-bit DMA controllers cannot address — turning
//! into silent driver failure rather than a clean allocation error.
//!
//! Layout: each usable span becomes one [`FrameRegion`] holding a 1-bit-per-
//! frame bitmap and a rotating search cursor. Allocation is first-fit from
//! the cursor and wraps, so blocks are reused in roughly allocation order
//! and long-running frame churn does not repeatedly walk the whole map.
//!
//! Three allocation policies:
//! - [`BootFrameAllocator::allocate_frame`] — any free 4 KiB frame.
//! - [`BootFrameAllocator::allocate_dma_frame`] — only frames at or below
//!   [`memmap::DMA_PHYS_LIMIT`], for controllers with 32-bit DMA.
//! - [`BootFrameAllocator::allocate_2mib_frame`] — a naturally aligned 2 MiB
//!   frame built from 512 consecutive free 4 KiB frames.
//!
//! All four public entry points are safe against double-free of the same
//! frame: [`reserve_range`] and the shared bit helpers are idempotent, so a
//! duplicate deallocation is a no-op rather than a corruption. Callers are
//! still required by [`FrameDeallocator`] to only pass genuinely unused
//! frames.

use alloc::vec;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, PhysFrame, Size2MiB, Size4KiB,
};
use x86_64::PhysAddr;

use super::memmap;

lazy_static! {
    static ref FRAME_ALLOCATOR: Mutex<BootFrameAllocator> = Mutex::new(BootFrameAllocator::new());
}

/// Bytes spanned by one 2 MiB frame, in 4 KiB units.
const FRAMES_PER_2MIB: usize = 512;

/// Upper bound on total bitmap bytes. Guards against a pathological firmware
/// map turning the allocator's own bookkeeping into the largest heap
/// allocation in the kernel. 4 MiB of bitmap tracks 32 GiB of RAM.
const MAX_BITMAP_BYTES: usize = 4 * 1024 * 1024;

/// Allocation accounting, surfaced by the `mem` command.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Frames present across all regions (after reservations).
    pub total_frames: u64,
    /// Frames currently free.
    pub free_frames: u64,
    /// Frames handed out and not yet returned.
    pub used_frames: u64,
    /// Frames allocated at or below [`memmap::DMA_PHYS_LIMIT`].
    pub free_low_frames: u64,
    /// Frames allocated above [`memmap::DMA_PHYS_LIMIT`].
    pub free_high_frames: u64,
    /// Successful 4 KiB allocations.
    pub alloc_count: u64,
    /// Successful deallocations.
    pub free_count: u64,
    /// Allocations that found no free frame.
    pub failures: u64,
    /// 2 MiB frame allocations.
    pub huge_alloc_count: u64,
}

/// One contiguous usable span plus its allocation bitmap.
struct FrameRegion {
    /// Page-aligned physical start.
    start: u64,
    /// Number of 4 KiB frames covered.
    frames: usize,
    /// 1 bit per frame: 1 = in use.
    bitmap: Vec<u8>,
    /// Rotating search cursor for first-fit allocation.
    cursor: usize,
}

impl FrameRegion {
    fn new(start: u64, frames: usize) -> Self {
        Self {
            start,
            frames,
            bitmap: vec![0u8; (frames + 7) / 8],
            cursor: 0,
        }
    }

    #[inline]
    fn is_used(&self, index: usize) -> bool {
        self.bitmap[index / 8] & (1u8 << (index % 8)) != 0
    }

    #[inline]
    fn set_used(&mut self, index: usize, used: bool) {
        let byte = &mut self.bitmap[index / 8];
        let mask = 1u8 << (index % 8);
        if used {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }

    #[inline]
    fn phys(&self, index: usize) -> u64 {
        self.start + (index as u64) * 4096
    }

    /// Find `count` consecutive free frames starting at or after `from`,
    /// wrapping once. Returns the index of the first frame.
    fn find_run(&self, from: usize, count: usize) -> Option<usize> {
        if count == 0 || count > self.frames {
            return None;
        }
        for step in 0..=self.frames - count {
            let start = (from + step) % (self.frames - count + 1);
            if (start..start + count).all(|i| !self.is_used(i)) {
                return Some(start);
            }
        }
        None
    }

    fn count_free(&self) -> u64 {
        let mut free = 0u64;
        for i in 0..self.frames {
            if !self.is_used(i) {
                free += 1;
            }
        }
        free
    }
}

/// Bitmap-backed physical frame allocator.
pub struct BootFrameAllocator {
    regions: Vec<FrameRegion>,
    stats: FrameStats,
    /// Set once [`BootFrameAllocator::init_from_spans`] has run.
    ready: bool,
}

impl BootFrameAllocator {
    const fn new() -> Self {
        Self {
            regions: Vec::new(),
            stats: FrameStats {
                total_frames: 0,
                free_frames: 0,
                used_frames: 0,
                free_low_frames: 0,
                free_high_frames: 0,
                alloc_count: 0,
                free_count: 0,
                failures: 0,
                huge_alloc_count: 0,
            },
            ready: false,
        }
    }

    /// (Re)initialise from page-aligned `(start, end)` spans.
    ///
    /// Callers derive spans from [`memmap::allocatable_spans`] so the
    /// kernel image, ramdisk, framebuffer and heap are already excluded; no
    /// further reservation is needed for a normal boot.
    pub fn init_from_spans(&mut self, spans: &[(u64, u64)]) {
        self.regions.clear();
        *&mut self.stats = FrameStats::default();
        self.ready = true;

        let mut budget = MAX_BITMAP_BYTES;
        let mut skipped = 0usize;
        for &(start, end) in spans {
            let start = (start + 0xFFF) & !0xFFF;
            let end = end & !0xFFF;
            if start >= end {
                continue;
            }
            let frames = ((end - start) / 4096) as usize;
            if frames == 0 {
                continue;
            }
            let bytes = (frames + 7) / 8;
            if bytes > budget {
                // Out of bookkeeping budget: skip the tail of the map rather
                // than exhausting the heap. Logged so `mem` stays honest.
                skipped += 1;
                continue;
            }
            budget -= bytes;
            let mut region = FrameRegion::new(start, frames);
            let free = region.count_free();
            if region.phys(0) < memmap::DMA_PHYS_LIMIT {
                if region.phys(frames - 1) < memmap::DMA_PHYS_LIMIT {
                    self.stats.free_low_frames += free;
                } else {
                    let low = ((memmap::DMA_PHYS_LIMIT - start) / 4096) as usize;
                    self.stats.free_low_frames += region.count_free_upto(low);
                    self.stats.free_high_frames += free - region.count_free_upto(low);
                }
            } else {
                self.stats.free_high_frames += free;
            }
            self.stats.total_frames += frames as u64;
            self.stats.free_frames += free;
            self.regions.push(region);
        }

        crate::serial_println!(
            "[mem] Frame allocator: {} region(s), {} frames ({} KiB), {} below DMA limit",
            self.regions.len(),
            self.stats.total_frames,
            self.stats.total_frames * 4,
            self.stats.free_low_frames
        );
        if skipped > 0 {
            crate::serial_println!(
                "[mem] WARNING: bitmap budget exhausted; {} span(s) left untracked",
                skipped
            );
        }
    }

    /// Mark `[start, end)` as in use. Idempotent, and safe to call after
    /// allocation has begun (it simply clears whatever is already set).
    pub fn reserve_range(&mut self, start: u64, end: u64) {
        let start = (start + 0xFFF) & !0xFFF;
        let end = end & !0xFFF;
        if start >= end {
            return;
        }
        for region in &mut self.regions {
            let region_end = region.start + (region.frames as u64) * 4096;
            if region_end <= start || region.start >= end {
                continue;
            }
            let first = ((start.max(region.start) - region.start) / 4096) as usize;
            let last = ((end.min(region_end) - region.start) / 4096) as usize;
            for i in first..last {
                if !region.is_used(i) {
                    region.set_used(i, true);
                    self.stats.free_frames = self.stats.free_frames.saturating_sub(1);
                    self.stats.used_frames = self.stats.used_frames.saturating_add(1);
                }
            }
        }
        crate::serial_println!(
            "[mem] Reserved {:#x}-{:#x} ({} KiB)",
            start,
            end,
            (end - start) / 1024
        );
    }

    /// Allocate one 4 KiB frame from anywhere in the map.
    ///
    /// `find_run` already searches from the cursor forward and then wraps, so
    /// one pass over the regions is exhaustive.
    fn alloc_any(&mut self) -> Option<PhysFrame<Size4KiB>> {
        for region in &mut self.regions {
            if region.cursor >= region.frames {
                region.cursor = 0;
            }
            if let Some(index) = region.find_run(region.cursor, 1) {
                region.set_used(index, true);
                region.cursor = (index + 1) % region.frames;
                let phys = region.phys(index);
                self.note_alloc(phys);
                return Some(PhysFrame::containing_address(PhysAddr::new(phys)));
            }
        }
        self.stats.failures += 1;
        None
    }

    fn note_alloc(&mut self, phys: u64) {
        self.stats.alloc_count += 1;
        self.stats.free_frames = self.stats.free_frames.saturating_sub(1);
        self.stats.used_frames = self.stats.used_frames.saturating_add(1);
        if phys < memmap::DMA_PHYS_LIMIT {
            self.stats.free_low_frames = self.stats.free_low_frames.saturating_sub(1);
        } else {
            self.stats.free_high_frames = self.stats.free_high_frames.saturating_sub(1);
        }
    }

    fn note_free(&mut self, phys: u64) {
        self.stats.free_count += 1;
        self.stats.free_frames = self.stats.free_frames.saturating_add(1);
        self.stats.used_frames = self.stats.used_frames.saturating_sub(1);
        if phys < memmap::DMA_PHYS_LIMIT {
            self.stats.free_low_frames = self.stats.free_low_frames.saturating_add(1);
        } else {
            self.stats.free_high_frames = self.stats.free_high_frames.saturating_add(1);
        }
    }

    /// Return a frame to the pool. Unknown addresses are ignored rather than
    /// corrupting a neighbouring region.
    fn release(&mut self, phys: u64) {
        for region in &mut self.regions {
            let region_end = region.start + (region.frames as u64) * 4096;
            if phys < region.start || phys >= region_end {
                continue;
            }
            let index = ((phys - region.start) / 4096) as usize;
            if index < region.frames && region.is_used(index) {
                region.set_used(index, false);
                self.note_free(phys);
            }
            return;
        }
    }

    /// Allocate a frame a 32-bit DMA controller can address.
    ///
    /// Used by USB (EHCI/UHCI/OHCI/xHCI), virtio-blk legacy and PIO-mode
    /// IDE paths. Returns `None` when the machine has no low memory left,
    /// which callers surface as a clean driver error instead of handing the
    /// hardware an address it cannot reach.
    pub fn allocate_dma_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        for region in &mut self.regions {
            if region.start >= memmap::DMA_PHYS_LIMIT {
                continue;
            }
            let usable = core::cmp::min(
                region.frames,
                ((memmap::DMA_PHYS_LIMIT - region.start) / 4096) as usize,
            );
            if usable == 0 {
                continue;
            }
            if region.cursor >= usable {
                region.cursor = 0;
            }
            for step in 0..usable {
                let index = (region.cursor + step) % usable;
                if !region.is_used(index) {
                    region.set_used(index, true);
                    region.cursor = (index + 1) % usable;
                    let phys = region.phys(index);
                    self.note_alloc(phys);
                    return Some(PhysFrame::containing_address(PhysAddr::new(phys)));
                }
            }
        }
        self.stats.failures += 1;
        None
    }

    /// Current accounting.
    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Total frames tracked.
    pub fn total_frames(&self) -> u64 {
        self.stats.total_frames
    }

    /// Frames currently free.
    pub fn free_frames(&self) -> u64 {
        self.stats.free_frames
    }
}

impl FrameRegion {
    fn count_free_upto(&self, limit: usize) -> u64 {
        let limit = core::cmp::min(limit, self.frames);
        (0..limit).filter(|&i| !self.is_used(i)).count() as u64
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if !self.ready {
            return None;
        }
        self.alloc_any()
    }
}

impl FrameDeallocator<Size4KiB> for BootFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.release(frame.start_address().as_u64());
    }
}

unsafe impl FrameAllocator<Size2MiB> for BootFrameAllocator {
    /// Allocate a naturally aligned 2 MiB frame, marking its 512 constituent
    /// 4 KiB frames used.
    ///
    /// A 2 MiB frame must start on a 2 MiB boundary, but a region starts
    /// wherever the firmware put it — commonly 1 MiB into low RAM. The head of
    /// such a region is therefore unusable for huge frames and is skipped
    /// rather than producing a misaligned "huge" frame.
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size2MiB>> {
        if !self.ready {
            return None;
        }
        const HUGE: u64 = 2 * 1024 * 1024;
        for region in &mut self.regions {
            let region_end = region.start + (region.frames as u64) * 4096;
            let aligned_start = (region.start + HUGE - 1) & !(HUGE - 1);
            if aligned_start >= region_end {
                continue;
            }
            let head_frames = ((aligned_start - region.start) / 4096) as usize;
            let usable_frames = region.frames - head_frames;
            let groups = usable_frames / FRAMES_PER_2MIB;
            if groups == 0 {
                continue;
            }
            // Search groups in allocation order, wrapping like `find_run`.
            let first_group = core::cmp::min(region.cursor / FRAMES_PER_2MIB, groups.saturating_sub(1));
            for step in 0..groups {
                let g = (first_group + step) % groups;
                let base = head_frames + g * FRAMES_PER_2MIB;
                if (base..base + FRAMES_PER_2MIB).all(|i| !region.is_used(i)) {
                    for i in base..base + FRAMES_PER_2MIB {
                        region.set_used(i, true);
                    }
                    region.cursor = (base + FRAMES_PER_2MIB) % region.frames;
                    let phys = region.phys(base);
                    for _ in 0..FRAMES_PER_2MIB as u64 {
                        self.note_alloc(phys);
                    }
                    self.stats.huge_alloc_count += 1;
                    return Some(PhysFrame::containing_address(PhysAddr::new(phys)));
                }
            }
        }
        self.stats.failures += 1;
        None
    }
}

/// Handle used by the MMIO mapper and any other frame consumer.
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        FRAME_ALLOCATOR.lock().allocate_frame()
    }
}

unsafe impl FrameAllocator<Size2MiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size2MiB>> {
        FRAME_ALLOCATOR.lock().allocate_frame()
    }
}

/// A frame allocator bound to 32-bit-DMA-reachable memory.
///
/// Drop-in for [`GlobalFrameAllocator`] in code paths that must hand
/// physically addressable buffers to hardware.
pub struct GlobalDmaFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalDmaFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        FRAME_ALLOCATOR.lock().allocate_dma_frame()
    }
}

pub fn frame_allocator() -> GlobalFrameAllocator {
    GlobalFrameAllocator
}

pub fn dma_frame_allocator() -> GlobalDmaFrameAllocator {
    GlobalDmaFrameAllocator
}

/// Initialise the global allocator from reservation-free usable spans.
///
/// See [`memmap::allocatable_spans`]. Passing the raw firmware map here
/// would hand out the kernel image and heap, so callers must not do that.
pub fn init_from_spans(spans: &[(u64, u64)]) {
    FRAME_ALLOCATOR.lock().init_from_spans(spans);
}

/// Explicit carve-out, for boot-time spans discovered after [`init_from_spans`].
pub fn reserve_range(start: u64, end: u64) {
    FRAME_ALLOCATOR.lock().reserve_range(start, end);
}

/// Current frame accounting for `mem`.
pub fn stats() -> FrameStats {
    FRAME_ALLOCATOR.lock().stats()
}

/// Frames currently free, across all regions.
pub fn free_frames() -> u64 {
    FRAME_ALLOCATOR.lock().free_frames()
}

/// Frames tracked in total, across all regions.
pub fn total_frames() -> u64 {
    FRAME_ALLOCATOR.lock().total_frames()
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::structures::paging::{PhysFrame as PF, Size2MiB, Size4KiB as S4K};

    const MIB: u64 = 1024 * 1024;

    /// Allocate one 4 KiB frame, disambiguating the `FrameAllocator` impls.
    fn alloc4(a: &mut BootFrameAllocator) -> Option<u64> {
        let f: Option<PF<S4K>> = a.allocate_frame();
        f.map(|f| f.start_address().as_u64())
    }

    /// Allocate one 2 MiB frame.
    fn alloc2(a: &mut BootFrameAllocator) -> Option<u64> {
        let f: Option<PF<Size2MiB>> = a.allocate_frame();
        f.map(|f| f.start_address().as_u64())
    }

    fn dealloc(a: &mut BootFrameAllocator, phys: u64) {
        unsafe {
            a.deallocate_frame(PhysFrame::containing_address(PhysAddr::new(phys)));
        }
    }

    #[test]
    fn allocation_walks_frames_in_ascending_order() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 4 * 4096)]);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(alloc4(&mut a).expect("frame"));
        }
        assert_eq!(
            seen,
            vec![0x100000, 0x101000, 0x102000, 0x103000]
        );
        assert_eq!(a.free_frames(), 0);
        assert_eq!(a.stats().used_frames, 4);
    }

    #[test]
    fn allocation_fails_cleanly_when_exhausted() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 2 * 4096)]);
        assert!(alloc4(&mut a).is_some());
        assert!(alloc4(&mut a).is_some());
        assert!(alloc4(&mut a).is_none());
        assert_eq!(a.stats().failures, 1);
        // Second attempt keeps counting rather than reusing the cursor blindly.
        assert!(alloc4(&mut a).is_none());
        assert_eq!(a.stats().failures, 2);
    }

    #[test]
    fn uninitialised_allocator_refuses_everything() {
        let mut a = BootFrameAllocator::new();
        assert!(alloc4(&mut a).is_none());
        assert!(alloc2(&mut a).is_none());
    }

    #[test]
    fn deallocated_frames_are_reused() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 4 * 4096)]);
        let f1 = alloc4(&mut a).unwrap();
        let f2 = alloc4(&mut a).unwrap();
        let f3 = alloc4(&mut a).unwrap();
        let f4 = alloc4(&mut a).unwrap();
        dealloc(&mut a, f2);
        assert_eq!(a.free_frames(), 1);
        // The freed frame is the one handed out next.
        assert_eq!(alloc4(&mut a).unwrap(), f2);
        assert_eq!(a.free_frames(), 0);
        // The other three are still allocated, so the pool is full.
        assert_eq!(a.stats().used_frames, 4);
        let _ = (f1, f3, f4);
    }

    #[test]
    fn double_free_is_a_no_op() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 2 * 4096)]);
        let f = alloc4(&mut a).unwrap();
        dealloc(&mut a, f);
        assert_eq!(a.free_frames(), 2);
        dealloc(&mut a, f);
        assert_eq!(a.free_frames(), 2, "second free must not double-count");
    }

    #[test]
    fn free_of_unknown_address_is_ignored() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 2 * 4096)]);
        dealloc(&mut a, 0xDEAD_0000);
        dealloc(&mut a, 0);
        assert_eq!(a.free_frames(), 2);
        assert_eq!(a.stats().free_count, 0);
    }

    #[test]
    fn reservation_marks_frames_used_and_shrinks_the_pool() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 8 * 4096)]);
        assert_eq!(a.total_frames(), 8);
        a.reserve_range(0x100000 + 2 * 4096, 0x100000 + 4 * 4096);
        assert_eq!(a.free_frames(), 6);
        // Frames 2 and 3 (0-indexed) must never be handed out.
        for _ in 0..6 {
            let f = alloc4(&mut a).unwrap();
            assert!(
                f != 0x100000 + 2 * 4096 && f != 0x100000 + 3 * 4096,
                "reserved frame {:#x} handed out",
                f
            );
        }
        assert!(alloc4(&mut a).is_none());
    }

    #[test]
    fn reservation_at_region_edge_is_handled() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 4 * 4096)]);
        a.reserve_range(0x0, 0x101000);
        assert_eq!(a.free_frames(), 3);
        a.reserve_range(0x103000, 0x200000);
        assert_eq!(a.free_frames(), 2);
    }

    #[test]
    fn reservation_is_idempotent() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 4 * 4096)]);
        a.reserve_range(0x100000, 0x102000);
        a.reserve_range(0x100000, 0x102000);
        assert_eq!(a.free_frames(), 2);
    }

    #[test]
    fn allocation_spans_multiple_regions() {
        let mut a = BootFrameAllocator::new();
        // 1 frame, then 2 frames.
        a.init_from_spans(&[(0x100000, 0x101000), (0x900000, 0x902000)]);
        assert_eq!(a.total_frames(), 3);
        let mut seen: Vec<u64> = (0..3).map(|_| alloc4(&mut a).unwrap()).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0x100000, 0x900000, 0x901000]);
        assert!(alloc4(&mut a).is_none());
    }

    #[test]
    fn region_larger_than_dma_limit_splits_the_low_pool() {
        // 8 MiB span starting 2 MiB below the 4 GiB line: 2 MiB of it is
        // reachable by 32-bit DMA, the rest is not.
        let start = memmap::DMA_PHYS_LIMIT - 2 * MIB;
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(start, memmap::DMA_PHYS_LIMIT + 6 * MIB)]);
        let total = 8 * MIB / 4096;
        assert_eq!(a.total_frames(), total);
        assert_eq!(a.stats().free_low_frames, 2 * MIB / 4096);
        assert_eq!(a.stats().free_high_frames, 6 * MIB / 4096);

        // Exhaust the DMA-reachable pool; it must fail cleanly rather than
        // silently return a high frame.
        for _ in 0..(2 * MIB / 4096) {
            let f = a.allocate_dma_frame().expect("low frame");
            assert!(f.start_address().as_u64() < memmap::DMA_PHYS_LIMIT);
        }
        assert!(a.allocate_dma_frame().is_none());
        // But a general allocation still works.
        assert!(alloc4(&mut a).is_some());
    }

    #[test]
    fn dma_allocation_skips_high_only_regions() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(memmap::DMA_PHYS_LIMIT, memmap::DMA_PHYS_LIMIT + MIB)]);
        assert_eq!(a.total_frames(), 256);
        assert_eq!(a.stats().free_low_frames, 0);
        assert!(a.allocate_dma_frame().is_none());
        assert!(alloc4(&mut a).is_some());
    }

    #[test]
    fn two_mib_frames_are_aligned_and_reserve_512_pages() {
        let mut a = BootFrameAllocator::new();
        // 8 MiB span starting at 1 MiB. Only the 2 MiB-aligned part of the
        // region can back a huge frame, so the first 1 MiB is skipped.
        a.init_from_spans(&[(0x100000, 0x100000 + 8 * MIB)]);
        let total_frames = 8 * MIB / 4096;
        assert_eq!(a.total_frames(), total_frames);
        let phys = alloc2(&mut a).expect("2 MiB frame");
        assert_eq!(phys % (2 * MIB), 0, "2 MiB frame must be naturally aligned");
        assert_eq!(phys, 0x200000, "must skip the unaligned 1 MiB region head");
        assert_eq!(a.free_frames(), total_frames - 512);
        assert_eq!(alloc2(&mut a).unwrap(), phys + 2 * MIB);
        assert_eq!(a.stats().huge_alloc_count, 2);
        // 8 MiB region: 1 MiB head skipped, so only 3 whole 2 MiB groups
        // (6 MiB) can back huge frames. After two, 2 MiB of 4 KiB frames are
        // still free: the third huge group and the 1 MiB tail.
        assert_eq!(a.free_frames(), 1024);
        assert_eq!(alloc2(&mut a).unwrap(), phys + 4 * MIB);
        assert_eq!(a.free_frames(), 512);
        // The 1 MiB tail cannot back a fourth huge frame, but is still
        // allocatable one 4 KiB frame at a time.
        assert!(alloc2(&mut a).is_none());
        assert!(alloc4(&mut a).is_some());
    }

    #[test]
    fn two_mib_frames_can_use_a_naturally_aligned_region() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(2 * MIB, 2 * MIB + 4 * MIB)]);
        assert_eq!(alloc2(&mut a).unwrap(), 2 * MIB);
        assert_eq!(alloc2(&mut a).unwrap(), 4 * MIB);
        assert_eq!(a.free_frames(), 0);
    }

    #[test]
    fn two_mib_allocation_fails_when_no_aligned_group_exists() {
        // 8 MiB starting 1 MiB in: after trimming the head, 6 whole 2 MiB
        // groups remain, so three succeed and the fourth fails.
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 8 * MIB)]);
        let mut ok = 0;
        while alloc2(&mut a).is_some() {
            ok += 1;
            assert!(ok < 10, "huge allocation did not terminate");
        }
        assert_eq!(ok, 3);
    }

    #[test]
    fn two_mib_allocation_fails_when_span_is_too_small() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + MIB)]);
        assert!(alloc2(&mut a).is_none());
    }

    #[test]
    fn two_mib_allocation_skips_partially_used_groups() {
        let mut a = BootFrameAllocator::new();
        // 2 MiB-aligned 16 MiB region so the group map is simple.
        a.init_from_spans(&[(2 * MIB, 2 * MIB + 16 * MIB)]);
        // Consume every frame of groups 0 and 1 except group 1's last frame,
        // which leaves group 1 unusable and group 2 free.
        for _ in 0..(2 * 512 - 1) {
            alloc4(&mut a).unwrap();
        }
        let before = a.free_frames();
        assert_eq!(alloc2(&mut a).unwrap(), 2 * MIB + 4 * MIB);
        assert_eq!(a.free_frames(), before - 512);
    }

    #[test]
    fn two_mib_and_small_allocations_share_one_bitmap() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(2 * MIB, 2 * MIB + 4 * MIB)]);
        let huge = alloc2(&mut a).unwrap();
        assert_eq!(huge, 2 * MIB);
        // A small allocation must not land inside the huge frame's pages:
        // the next free frame is the first one past it.
        let small = alloc4(&mut a).unwrap();
        assert_eq!(small, 4 * MIB);
        assert_eq!(a.free_frames(), 1024 - 512 - 1);
        assert!(alloc4(&mut a).is_some());
    }

    #[test]
    fn stats_track_alloc_and_free() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 4 * 4096)]);
        let f1 = alloc4(&mut a).unwrap();
        alloc4(&mut a).unwrap();
        dealloc(&mut a, f1);
        let s = a.stats();
        assert_eq!(s.total_frames, 4);
        assert_eq!(s.alloc_count, 2);
        assert_eq!(s.free_count, 1);
        assert_eq!(s.used_frames, 1);
        assert_eq!(s.free_frames, 3);
    }

    #[test]
    fn reinit_with_no_spans_clears_the_pool() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100000 + 64 * MIB)]);
        assert_eq!(a.total_frames(), 64 * MIB / 4096);
        a.init_from_spans(&[]);
        assert_eq!(a.total_frames(), 0);
        assert_eq!(a.free_frames(), 0);
        assert!(alloc4(&mut a).is_none());
    }

    #[test]
    fn sub_page_spans_are_ignored() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100000, 0x100800), (0x100801, 0x100801)]);
        assert_eq!(a.total_frames(), 0);
    }

    #[test]
    fn unaligned_spans_are_trimmed_inward() {
        let mut a = BootFrameAllocator::new();
        a.init_from_spans(&[(0x100001, 0x103001)]);
        assert_eq!(a.total_frames(), 2);
        assert_eq!(alloc4(&mut a).unwrap(), 0x101000);
        assert_eq!(alloc4(&mut a).unwrap(), 0x102000);
        assert!(alloc4(&mut a).is_none());
    }
}
