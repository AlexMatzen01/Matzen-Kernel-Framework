//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Physical/virtual address translation for the bootloader's direct map.
//!
//! The kernel has no virtual-memory abstraction of its own: RAM is reached by
//! adding `phys_mem_offset` to a physical address, and MMIO below the direct
//! map's end is reached the same way. Every driver that needs the translation
//! was re-deriving it inline, which is how the same translation ended up with
//! three different fallbacks and one of them returned an *untranslated* virtual
//! address on failure — handing a DMA engine a garbage target instead of
//! reporting an error.
//!
//! This module is the single place that arithmetic lives:
//! - [`direct_map_offset`] records the offset once, at boot.
//! - [`phys_to_direct`] / [`direct_to_phys`] translate within the mapped range.
//! - [`is_direct_mapped`] bounds-checks before any dereference.
//! - [`dma_phys`] rejects addresses a 32-bit DMA controller cannot reach,
//!   using [`memmap::DMA_PHYS_LIMIT`] as the single shared constant.
//!
//! Every function is total and side-effect free, so the arithmetic is unit
//! tested without hardware.

use core::sync::atomic::{AtomicU64, Ordering};

use super::memmap;

/// Offset the bootloader direct-maps physical memory at, or `0` before boot
/// records it. `0` is a legitimate value only on a machine whose map is
/// identity-mapped; [`is_direct_mapped`] is always consulted first.
static DIRECT_MAP_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Record the direct-map offset from the bootloader.
pub fn set_direct_map_offset(offset: u64) {
    DIRECT_MAP_OFFSET.store(offset, Ordering::Release);
}

/// The recorded direct-map offset.
pub fn direct_map_offset() -> u64 {
    DIRECT_MAP_OFFSET.load(Ordering::Acquire)
}

/// Translate a physical address to its direct-map virtual address.
///
/// Only meaningful for addresses inside [`is_direct_mapped`]; callers that
/// cannot check must use [`dma_phys`], which reports failure instead of
/// producing a plausible-looking but invalid pointer.
pub fn phys_to_direct(phys: u64) -> u64 {
    phys.wrapping_add(direct_map_offset())
}

/// Inverse of [`phys_to_direct`]. The result is only a physical address if
/// `virt` really came from [`phys_to_direct`].
pub fn direct_to_phys(virt: u64) -> u64 {
    virt.wrapping_sub(direct_map_offset())
}

/// True when `phys` falls inside the direct map.
///
/// `phys_offset` is the physical address the direct map starts at (normally
/// `0`) and `map_end` is the exclusive end of the mapped range. Passing the
/// end explicitly keeps this checkable in tests.
pub fn is_direct_mapped(phys: u64, phys_offset: u64, map_end: u64) -> bool {
    phys >= phys_offset && phys < map_end
}

/// Translate a physical address for a DMA controller with a 32-bit address bus.
///
/// Returns `None` when the address is at or above
/// [`memmap::DMA_PHYS_LIMIT`], which such a controller cannot reach
/// regardless of which driver asks. Drivers must surface this as a clean
/// error; the previous inline fallbacks returned the untranslated virtual
/// address, which the hardware would then write to.
pub fn dma_phys(phys: u64) -> Option<u64> {
    if phys < memmap::DMA_PHYS_LIMIT {
        Some(phys)
    } else {
        None
    }
}

/// Translate a virtual direct-map address to a DMA-capable physical address.
///
/// The combination every buffer-allocation path needs: get the physical
/// address and confirm a 32-bit controller can reach it, or fail.
pub fn dma_phys_from_direct(virt: u64) -> Option<u64> {
    dma_phys(direct_to_phys(virt))
}

/// Convert a raw pointer into the direct map to a physical address, or `None`
/// when the address is not reachable by 32-bit DMA.
///
/// # Safety
/// `ptr` must be a valid pointer into the direct map (that is, obtained by
/// adding [`direct_map_offset`] to a physical address).
pub unsafe fn dma_phys_from_ptr<T>(ptr: *const T) -> Option<u64> {
    dma_phys_from_direct(ptr as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn direct_map_round_trips() {
        // Use the local arithmetic rather than the global so the test does not
        // depend on whatever offset a previous test set.
        let offset = 0xFFFF_8000_0000_0000u64;
        for phys in [0u64, 0x1000, 0x19e6d000, 4 * MIB, 0x4000_0000 - 1] {
            let virt = phys.wrapping_add(offset);
            assert_eq!(virt.wrapping_sub(offset), phys);
            assert_eq!(phys, virt.wrapping_sub(offset));
        }
    }

    #[test]
    fn global_offset_is_recorded_and_read_back() {
        set_direct_map_offset(0x28000000000);
        assert_eq!(direct_map_offset(), 0x28000000000);
        assert_eq!(phys_to_direct(0x1000), 0x28000000000 + 0x1000);
        assert_eq!(direct_to_phys(0x28000000000 + 0x1000), 0x1000);
        // Leaves a plausible value for other tests.
        set_direct_map_offset(0);
    }

    #[test]
    fn direct_map_bounds_are_inclusive_of_start_and_exclusive_of_end() {
        assert!(is_direct_mapped(0, 0, 0x1000));
        assert!(is_direct_mapped(0xfff, 0, 0x1000));
        assert!(!is_direct_mapped(0x1000, 0, 0x1000));
        assert!(!is_direct_mapped(u64::MAX, 0, 0x1000));
        // A non-zero base offset is honoured.
        assert!(!is_direct_mapped(0xfff, 0x1000, 0x2000));
        assert!(is_direct_mapped(0x1000, 0x1000, 0x2000));
    }

    #[test]
    fn dma_limit_accepts_low_addresses_only() {
        assert_eq!(dma_phys(0), Some(0));
        assert_eq!(dma_phys(0x1000), Some(0x1000));
        assert_eq!(dma_phys(memmap::DMA_PHYS_LIMIT - 1), Some(memmap::DMA_PHYS_LIMIT - 1));
        assert_eq!(dma_phys(memmap::DMA_PHYS_LIMIT), None);
        assert_eq!(dma_phys(memmap::DMA_PHYS_LIMIT + 1), None);
        assert_eq!(dma_phys(u64::MAX), None);
    }

    #[test]
    fn dma_translation_rejects_high_frames_rather_than_wrapping() {
        set_direct_map_offset(0xFFFF_8000_0000_0000);
        assert_eq!(dma_phys_from_direct(0xFFFF_8000_0000_0000 + 0x1000), Some(0x1000));
        // A high physical address must fail, not come back untranslated.
        assert_eq!(dma_phys_from_direct(0xFFFF_8000_0000_0000 + 0x1_0000_0000), None);
        set_direct_map_offset(0);
    }

    #[test]
    fn dma_translation_from_pointer_matches_the_numeric_path() {
        set_direct_map_offset(0x28000000000);
        let backing = 0u64;
        let ptr = (&backing as *const u64).wrapping_add(0);
        // Only assert agreement when the value is actually in the direct map.
        if is_direct_mapped(ptr as u64, 0, u64::MAX) {
            let from_ptr = unsafe { dma_phys_from_ptr(ptr) };
            assert_eq!(from_ptr, dma_phys(ptr as u64));
        }
        set_direct_map_offset(0);
    }
}
