//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Memory Allocator
//!
//! Provides a global allocator for dynamic memory allocation using a static buffer.

use linked_list_allocator::LockedHeap;

#[cfg_attr(not(test), global_allocator)]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Total kernel heap budget: 32 MiB.
///
/// Sized for photo-class workloads on a 128 MiB guest: a ~1080p wallpaper
/// needs file bytes + inflate/IDAT transients + full-res RGBA + a
/// screen-size cache alive at once (~20 MiB peak). Everything else
/// (FS bitmap/inodes, RX/TX queues, TCP buffers, editor, archives)
/// shares this heap.
///
/// NOTE: this must NOT live in `.bss` — a 32 MiB static array balloons the
/// kernel ELF and collides with bootloader mappings on real hardware
/// (GDT frame `PageAlreadyMapped` panic). Instead `init` is called once on
/// a range carved from the bootloader memory map (see `kernel_main`).
pub const HEAP_SIZE: usize = 32 * 1024 * 1024;

/// Fallback early heap: used only when no large contiguous usable run
/// exists (tiny machines). The kernel still boots, with reduced headroom
/// and a serial warning; large wallpapers will fail cleanly via caps.
const FALLBACK_HEAP_SIZE: usize = 1024 * 1024;
static mut FALLBACK_HEAP: [u8; FALLBACK_HEAP_SIZE] = [0; FALLBACK_HEAP_SIZE];

/// Initialize the heap allocator on caller-provided memory.
///
/// `heap_bottom`/`heap_size` must be usable RAM that stays mapped for the
/// kernel's lifetime (physical frames exposed via the bootloader's
/// full-physical mapping satisfy this). `heap_bottom` is auto-aligned by
/// the underlying allocator.
pub fn init(heap_bottom: *mut u8, heap_size: usize) {
    unsafe {
        ALLOCATOR.lock().init(heap_bottom, heap_size);
    }
}

/// Initialize the heap on the static 1 MiB fallback (tiny machines only).
pub fn init_fallback() {
    unsafe {
        ALLOCATOR.lock()
            .init(FALLBACK_HEAP.as_mut_ptr(), FALLBACK_HEAP.len());
    }
}

/// Find a contiguous run for the heap in the bootloader memory map.
///
/// Returns `(phys_start, len)`; takes up to `want` bytes from the END of
/// the largest `Usable` run. First pass prefers sub-4 GiB memory so
/// heap-backed DMA stays reachable; second pass takes anything. Stack
/// only (safe before heap init). Callers should still `reserve_range`
/// the result in the frame allocator.
pub fn carve_heap_run(
    regions: &[bootloader_api::info::MemoryRegion],
    want: u64,
) -> Option<(u64, u64)> {
    use bootloader_api::info::MemoryRegionKind;
    let mut best: Option<(u64, u64)> = None; // (run_start, run_len)
    for pass in 0..2 {
        for region in regions {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }
            let start = (region.start + 0xFFF) & !0xFFF;
            let mut end = region.end & !0xFFF;
            if pass == 0 && end > 0x1_0000_0000 {
                end = 0x1_0000_0000;
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
    use super::carve_heap_run;
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
}
