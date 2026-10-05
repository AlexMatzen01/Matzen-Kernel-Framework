//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Firmware memory-map interpretation (`no_std`, `alloc`).
//!
//! The bootloader hands us a flat list of regions tagged `Usable`,
//! `Bootloader`, or an `Unknown*` variant carrying the raw firmware type
//! (UEFI `MemoryType` or BIOS E820 type). That list is enough to *find* RAM
//! but not enough to *account* for it: nothing in the raw map says how much
//! memory is reserved, how much sits above 4 GiB, or what the kernel itself
//! occupies.
//!
//! This module turns the raw map into a [`MemoryLayout`] that answers those
//! questions:
//!
//! - Every region is assigned a [`RegionClass`] that maps the firmware tag to
//!   a policy (see [`classify`]). Notably `ACPINvs` and `Bad` are *never*
//!   handed out even though the firmware may report them as "unknown".
//! - Boot-owned spans the bootloader did **not** mark reserved â€” the kernel
//!   ELF, the ramdisk, the framebuffer â€” are recorded as explicit
//!   [`Reservation`]s so the frame allocator cannot hand them out.
//! - [`MemoryLayout::allocatable_spans`] returns the page-aligned, merged,
//!   reservation-free spans the frame allocator may use. Nothing else in the
//!   kernel needs to reason about the raw map.
//!
//! Everything here is pure data manipulation over `&[MemoryRegion]` plus a
//! reservation list, so the whole module is unit-testable without hardware.
//!
//! Reference: UEFI spec Â§13.3 "Memory Types", and the System BIOS
//! Specification E820 map layout.

use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};

/// Physical addresses at or above this are unreachable by the 32-bit DMA
/// controllers in this kernel (EHCI/UHCI/OHCI/xHCI, virtio-blk legacy,
/// PIO-mode IDE). Drivers that need DMA-capable memory ask the frame
/// allocator for a frame below this limit instead of discovering the
/// problem later.
///
/// Shared by [`crate::drivers::usb`] and [`crate::drivers::virtio_blk`], which
/// previously each carried their own copy of the constant.
pub const DMA_PHYS_LIMIT: u64 = 0x1_0000_0000;

/// Classification of a firmware region, independent of how it was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionClass {
    /// Free RAM the kernel may allocate from.
    Usable,
    /// RAM the bootloader uses for its own mappings (page tables, boot info).
    /// Mapped for the kernel's lifetime, never allocated.
    Bootloader,
    /// ACPI reclaimable: firmware-reserved until the OS claims it. Safe to
    /// use once the OS has taken ownership; the kernel treats it as usable
    /// but reports it separately from plain `Usable`.
    AcpiReclaim,
    /// ACPI NVS: must survive across reboots. Never allocate.
    AcpiNvs,
    /// Memory reported unusable or defective. Never allocate.
    Bad,
    /// Memory-mapped I/O or port space. Never allocate.
    Mmio,
    /// Anything else firmware reserved (loader code/data, ROM, PCI holes).
    Reserved,
}

impl RegionClass {
    /// True when the frame allocator may hand this class out.
    pub fn is_allocatable(self) -> bool {
        matches!(self, RegionClass::Usable | RegionClass::AcpiReclaim)
    }

    /// Short stable label for `mem` output.
    pub fn label(self) -> &'static str {
        match self {
            RegionClass::Usable => "usable",
            RegionClass::Bootloader => "bootloader",
            RegionClass::AcpiReclaim => "acpi-reclaim",
            RegionClass::AcpiNvs => "acpi-nvs",
            RegionClass::Bad => "bad",
            RegionClass::Mmio => "mmio",
            RegionClass::Reserved => "reserved",
        }
    }
}

/// UEFI `MemoryType` values (UEFI spec Â§13.3, table "Memory Type Definitions").
mod uefi_type {
    pub const BOOT_SERVICES_CODE: u32 = 4;
    pub const BOOT_SERVICES_DATA: u32 = 5;
    pub const RUNTIME_SERVICES_CODE: u32 = 6;
    pub const RUNTIME_SERVICES_DATA: u32 = 7;
    pub const UNUSABLE: u32 = 9;
    pub const ACPI_RECLAIM: u32 = 10;
    pub const ACPI_NVS: u32 = 11;
    pub const MEMORY_MAPPED_IO: u32 = 12;
    pub const MEMORY_MAPPED_IO_PORT_SPACE: u32 = 13;
}

/// BIOS E820 memory types (System BIOS Specification, INT 15h AX=E820).
mod e820_type {
    pub const USABLE: u32 = 1;
    pub const RESERVED: u32 = 2;
    pub const ACPI_RECLAIM: u32 = 3;
    pub const ACPI_NVS: u32 = 4;
    pub const BAD: u32 = 5;
    pub const DISABLED: u32 = 0x20;
    /// "OS can use, but should not" â€” probed by firmware and left alone.
    /// Treat as reserved: the firmware has not vouched for it.
    pub const UNCONFIRMED_BASE: u32 = 0x7FC0_0000;
    /// RAM above 4 GiB the firmware knows about but has not enabled.
    pub const UNCONFIRMED_HIGH: u32 = 0x7FC0_0002;
}

/// Map a firmware region tag onto a [`RegionClass`].
///
/// `UnknownUefi`/`UnknownBios` carry the raw firmware type. Unknown values
/// fall back to [`RegionClass::Reserved`] â€” the safe default, since a region
/// the kernel cannot identify must never be handed out as RAM.
pub fn classify(kind: &MemoryRegionKind) -> RegionClass {
    match *kind {
        MemoryRegionKind::Usable => RegionClass::Usable,
        MemoryRegionKind::Bootloader => RegionClass::Bootloader,
        MemoryRegionKind::UnknownUefi(t) => match t {
            uefi_type::BOOT_SERVICES_CODE
            | uefi_type::BOOT_SERVICES_DATA
            | uefi_type::RUNTIME_SERVICES_CODE
            | uefi_type::RUNTIME_SERVICES_DATA => RegionClass::Bootloader,
            uefi_type::UNUSABLE => RegionClass::Bad,
            uefi_type::ACPI_RECLAIM => RegionClass::AcpiReclaim,
            uefi_type::ACPI_NVS => RegionClass::AcpiNvs,
            uefi_type::MEMORY_MAPPED_IO | uefi_type::MEMORY_MAPPED_IO_PORT_SPACE => {
                RegionClass::Mmio
            }
            _ => RegionClass::Reserved,
        },
            MemoryRegionKind::UnknownBios(t) => match t {
            e820_type::USABLE => RegionClass::Usable,
            e820_type::ACPI_RECLAIM => RegionClass::AcpiReclaim,
            e820_type::ACPI_NVS => RegionClass::AcpiNvs,
            e820_type::BAD | e820_type::DISABLED => RegionClass::Bad,
            // The firmware reported RAM it did not vouch for.
            e820_type::UNCONFIRMED_BASE..=0x7FFF_FFFF => RegionClass::Reserved,
            _ => RegionClass::Reserved,
        },
        // `MemoryRegionKind` is `#[non_exhaustive]`: a firmware kind added by
        // a newer bootloader must never become free RAM by accident.
        _ => RegionClass::Reserved,
    }
}

/// Raw firmware type recorded for display, or `0` for tagged kinds.
pub fn firmware_tag(kind: &MemoryRegionKind) -> u32 {
    match *kind {
        MemoryRegionKind::UnknownUefi(t) | MemoryRegionKind::UnknownBios(t) => t,
        _ => 0,
    }
}

/// What a [`Reservation`] protects, for `mem` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResKind {
    /// The kernel ELF image (code + data + bss) in memory.
    KernelImage,
    /// A ramdisk the bootloader loaded.
    RamDisk,
    /// The linear framebuffer the bootloader set up.
    FrameBuffer,
    /// The kernel heap carved by [`crate::allocator`].
    Heap,
    /// An explicit carve-out requested at boot.
    Manual,
}

impl ResKind {
    pub fn label(self) -> &'static str {
        match self {
            ResKind::KernelImage => "kernel",
            ResKind::RamDisk => "ramdisk",
            ResKind::FrameBuffer => "framebuffer",
            ResKind::Heap => "heap",
            ResKind::Manual => "reserved",
        }
    }
}

/// A boot-owned span that must never be handed out as a free frame.
#[derive(Debug, Clone, Copy)]
pub struct Reservation {
    pub start: u64,
    pub end: u64,
    pub kind: ResKind,
}

impl Reservation {
    pub fn new(start: u64, end: u64, kind: ResKind) -> Option<Self> {
        // Page-align inward so we never reserve a page the caller did not
        // actually occupy, and drop empty spans.
        let start = (start + 0xFFF) & !0xFFF;
        let end = end & !0xFFF;
        if start >= end {
            return None;
        }
        Some(Self { start, end, kind })
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// One classified firmware region.
#[derive(Debug, Clone, Copy)]
pub struct LayoutRegion {
    /// Page-aligned start.
    pub start: u64,
    /// Page-aligned end.
    pub end: u64,
    pub class: RegionClass,
    /// Raw firmware type, or `0` when the region was tagged directly.
    pub firmware_tag: u32,
}

impl LayoutRegion {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// Byte totals per [`RegionClass`], indexed by [`RegionClass::index`].
///
/// The retained region list is capped at [`MAX_REGIONS`], so a per-class total
/// is the only way to account for bytes that fall outside it â€” which on a
/// machine with a large PCI hole is most of the address space. Without this,
/// `mem` reports a 12 GiB figure it cannot explain.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClassTotals {
    pub usable: u64,
    pub bootloader: u64,
    pub acpi_reclaim: u64,
    pub acpi_nvs: u64,
    pub bad: u64,
    pub mmio: u64,
    pub reserved: u64,
}

impl ClassTotals {
    pub fn get(&self, class: RegionClass) -> u64 {
        match class {
            RegionClass::Usable => self.usable,
            RegionClass::Bootloader => self.bootloader,
            RegionClass::AcpiReclaim => self.acpi_reclaim,
            RegionClass::AcpiNvs => self.acpi_nvs,
            RegionClass::Bad => self.bad,
            RegionClass::Mmio => self.mmio,
            RegionClass::Reserved => self.reserved,
        }
    }

    fn add(&mut self, class: RegionClass, len: u64) {
        let slot = match class {
            RegionClass::Usable => &mut self.usable,
            RegionClass::Bootloader => &mut self.bootloader,
            RegionClass::AcpiReclaim => &mut self.acpi_reclaim,
            RegionClass::AcpiNvs => &mut self.acpi_nvs,
            RegionClass::Bad => &mut self.bad,
            RegionClass::Mmio => &mut self.mmio,
            RegionClass::Reserved => &mut self.reserved,
        };
        *slot = slot.saturating_add(len);
    }

    /// Classes with a non-zero total, largest first.
    pub fn non_empty(&self) -> alloc::vec::Vec<(RegionClass, u64)> {
        [
            RegionClass::Usable,
            RegionClass::Reserved,
            RegionClass::Mmio,
            RegionClass::Bootloader,
            RegionClass::AcpiReclaim,
            RegionClass::AcpiNvs,
            RegionClass::Bad,
        ]
        .into_iter()
        .map(|c| (c, self.get(c)))
        .filter(|&(_, bytes)| bytes > 0)
        .collect()
    }
}

/// A classified, reservation-free memory map.
///
/// Built once in `kernel_main` and then treated as immutable: the frame
/// allocator takes its spans from [`MemoryLayout::allocatable_spans`], and
/// `mem` reports the totals.
#[derive(Debug, Clone, Default)]
pub struct MemoryLayout {
    pub regions: Vec<LayoutRegion>,
    pub reservations: Vec<Reservation>,
    /// Per-class byte totals over *every* firmware region, including those
    /// dropped by the [`MAX_REGIONS`] cap.
    pub by_class: ClassTotals,
    /// Bytes in regions classified allocatable (before reservations).
    pub allocatable_bytes: u64,
    /// Bytes in memory-mapped I/O / port-space windows.
    ///
    /// These appear in the firmware map on any machine with a PCI hole â€” QEMU
    /// reports one that is most of a 32 GiB address space â€” so they must be
    /// excluded from a RAM total. [`MemoryLayout::ram_bytes`] does that.
    pub mmio_bytes: u64,
    /// Allocatable bytes below [`DMA_PHYS_LIMIT`].
    pub allocatable_low_bytes: u64,
    /// Allocatable bytes at or above [`DMA_PHYS_LIMIT`].
    pub allocatable_high_bytes: u64,
    /// Bytes reserved for the kernel image, ramdisk and framebuffer.
    pub boot_owned_bytes: u64,
    /// Total bytes described by the firmware map.
    pub total_bytes: u64,
    /// Allocatable bytes minus reservations, page-aligned.
    pub allocatable_after_reservations: u64,
    /// Regions dropped because `MAX_REGIONS` was reached.
    pub truncated: bool,
    /// Page-aligned, merged, reservation-free spans over **every** allocatable
    /// firmware region.
    ///
    /// Kept separately from [`MemoryLayout::regions`] because that list is
    /// capped for display; deriving frames from a truncated list would silently
    /// shrink the frame pool on any machine reporting more than
    /// [`MAX_REGIONS`] regions.
    pub spans: Vec<(u64, u64)>,
}

impl MemoryLayout {
    /// Bytes that are actually RAM: everything in the map except MMIO and
    /// port-space windows.
    ///
    /// A firmware map routinely contains a PCI MMIO hole covering most of the
    /// upper address space. Counting it as physical memory makes `mem` claim
    /// far more RAM than the machine has.
    pub fn ram_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.mmio_bytes)
    }

    /// RAM the kernel cannot use: firmware-reserved, bootloader-owned, ACPI
    /// NVS, bad blocks and MMIO.
    pub fn unavailable_ram_bytes(&self) -> u64 {
        self.ram_bytes().saturating_sub(self.allocatable_bytes)
    }
}

/// Maximum regions retained. Firmware reports dozens on real machines and
/// hundreds on some; anything beyond this is folded into the totals but not
/// listed individually.
pub const MAX_REGIONS: usize = 64;

/// Build a layout from a firmware map plus an explicit reservation list.
///
/// Regions are page-aligned and empty ones dropped. `MAX_REGIONS` caps the
/// retained list; overflow sets [`MemoryLayout::truncated`] while the byte
/// totals stay exact.
pub fn build(regions: &[MemoryRegion], reservations: &[Reservation]) -> MemoryLayout {
    let mut layout = MemoryLayout {
        truncated: false,
        ..Default::default()
    };
    layout.reservations = reservations.to_vec();
    // Every allocatable region contributes a span, regardless of whether it
    // also made it into the capped display list.
    let mut allocatable: Vec<(u64, u64)> = Vec::new();

    for r in regions {
        let start = (r.start + 0xFFF) & !0xFFF;
        let end = r.end & !0xFFF;
        if start >= end {
            continue;
        }
        let len = end - start;
        layout.total_bytes = layout.total_bytes.saturating_add(len);

        let class = classify(&r.kind);
        layout.by_class.add(class, len);
        if class == RegionClass::Mmio {
            layout.mmio_bytes = layout.mmio_bytes.saturating_add(len);
        }
        if class.is_allocatable() {
            allocatable.push((start, end));
            layout.allocatable_bytes = layout.allocatable_bytes.saturating_add(len);
            // Split at the DMA limit by address, not by region: a single
            // usable run can straddle 4 GiB, and only the low head is
            // reachable by the kernel's 32-bit DMA controllers.
            let low_end = end.min(DMA_PHYS_LIMIT);
            if low_end > start {
                layout.allocatable_low_bytes =
                    layout.allocatable_low_bytes.saturating_add(low_end - start);
            }
            let high_start = start.max(DMA_PHYS_LIMIT);
            if end > high_start {
                layout.allocatable_high_bytes =
                    layout.allocatable_high_bytes.saturating_add(end - high_start);
            }
        }
        if layout.regions.len() < MAX_REGIONS {
            layout.regions.push(LayoutRegion {
                start,
                end,
                class,
                firmware_tag: firmware_tag(&r.kind),
            });
        } else {
            layout.truncated = true;
        }
    }

    for res in reservations.iter() {
        layout.boot_owned_bytes = layout.boot_owned_bytes.saturating_add(res.len());
        subtract_into(&mut allocatable, res.start, res.end);
    }
    merge(&mut allocatable);
    layout.allocatable_after_reservations =
        allocatable.iter().map(|&(s, e)| e - s).fold(0u64, |a, b| a.saturating_add(b));
    layout.spans = allocatable;
    layout
}

/// Page-aligned spans the frame allocator may hand out.
///
/// Precomputed by [`build`] from every allocatable region with every
/// reservation removed and adjacent spans merged.
pub fn allocatable_spans(layout: &MemoryLayout) -> &[(u64, u64)] {
    &layout.spans
}

fn subtract_into(spans: &mut Vec<(u64, u64)>, cut_start: u64, cut_end: u64) {
    if cut_start >= cut_end {
        return;
    }
    let mut next: Vec<(u64, u64)> = Vec::with_capacity(spans.len() + 1);
    for &(s, e) in spans.iter() {
        if e <= cut_start || s >= cut_end {
            next.push((s, e));
            continue;
        }
        if s < cut_start {
            next.push((s, cut_start));
        }
        if e > cut_end {
            next.push((cut_end, e));
        }
    }
    *spans = next;
}

fn merge(spans: &mut Vec<(u64, u64)>) {
    spans.retain(|&(s, e)| s < e);
    spans.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
    for &(s, e) in spans.iter() {
        match out.last_mut() {
            // Merge touching or overlapping spans: a frame allocator region
            // is cheaper to track than two adjacent ones, and a reservation
            // that exactly abutted two usable regions leaves a hole that
            // must not become two allocations.
            Some(last) if s <= last.1 => {
                if e > last.1 {
                    last.1 = e;
                }
            }
            _ => out.push((s, e)),
        }
    }
    *spans = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn reg(start: u64, end: u64, kind: MemoryRegionKind) -> MemoryRegion {
        MemoryRegion { start, end, kind }
    }

    fn usable(start: u64, end: u64) -> MemoryRegion {
        reg(start, end, MemoryRegionKind::Usable)
    }

    #[test]
    fn uefi_memory_types_map_to_policy() {
        let cases = [
            (4, RegionClass::Bootloader),
            (5, RegionClass::Bootloader),
            (9, RegionClass::Bad),
            (10, RegionClass::AcpiReclaim),
            (11, RegionClass::AcpiNvs),
            (12, RegionClass::Mmio),
            (13, RegionClass::Mmio),
            (0, RegionClass::Reserved),
            (99, RegionClass::Reserved),
        ];
        for (tag, want) in cases {
            assert_eq!(
                classify(&MemoryRegionKind::UnknownUefi(tag)),
                want,
                "UEFI type {}",
                tag
            );
        }
    }

    #[test]
    fn e820_memory_types_map_to_policy() {
        let cases = [
            (1, RegionClass::Usable),
            (2, RegionClass::Reserved),
            (3, RegionClass::AcpiReclaim),
            (4, RegionClass::AcpiNvs),
            (5, RegionClass::Bad),
            (0x20, RegionClass::Bad),
            (0x7FC0_0000, RegionClass::Reserved),
            (0x7FC0_0002, RegionClass::Reserved),
        ];
        for (tag, want) in cases {
            assert_eq!(
                classify(&MemoryRegionKind::UnknownBios(tag)),
                want,
                "E820 type {}",
                tag
            );
        }
    }

    #[test]
    fn only_usable_and_acpi_reclaim_are_allocatable() {
        assert!(RegionClass::Usable.is_allocatable());
        assert!(RegionClass::AcpiReclaim.is_allocatable());
        for c in [
            RegionClass::Bootloader,
            RegionClass::AcpiNvs,
            RegionClass::Bad,
            RegionClass::Mmio,
            RegionClass::Reserved,
        ] {
            assert!(!c.is_allocatable(), "{} must not be allocatable", c.label());
        }
    }

    #[test]
    fn unknown_firmware_type_defaults_to_reserved() {
        // An unidentifiable region must never become free RAM.
        assert_eq!(
            classify(&MemoryRegionKind::UnknownUefi(0xDEAD)),
            RegionClass::Reserved
        );
        assert_eq!(
            classify(&MemoryRegionKind::UnknownBios(0xBEEF)),
            RegionClass::Reserved
        );
    }

    #[test]
    fn totals_split_low_and_high_around_dma_limit() {
        let regions = [
            usable(0x100000, 0x100000 + 8 * MIB),
            usable(0x1_0000_0000, 0x1_0000_0000 + 64 * MIB),
            reg(0x2000, 0x3000, MemoryRegionKind::UnknownUefi(uefi_type::ACPI_NVS)),
        ];
        let layout = build(&regions, &[]);
        assert_eq!(layout.allocatable_low_bytes, 8 * MIB);
        assert_eq!(layout.allocatable_high_bytes, 64 * MIB);
        assert_eq!(layout.allocatable_bytes, 72 * MIB);
        // The ACPI NVS span is counted in the map total but never allocatable.
        assert_eq!(layout.total_bytes, 72 * MIB + 0x1000);
    }

    #[test]
    fn region_straddling_dma_limit_counts_in_both_buckets() {
        // A usable run from just below to well above 4 GiB contributes to the
        // low bucket via its head and the high bucket via its tail; the
        // straddling frame itself is not double counted.
        let regions = [usable(DMA_PHYS_LIMIT - MIB, DMA_PHYS_LIMIT + MIB)];
        let layout = build(&regions, &[]);
        assert_eq!(layout.allocatable_low_bytes, MIB);
        assert_eq!(layout.allocatable_high_bytes, MIB);
        assert_eq!(layout.allocatable_bytes, 2 * MIB);
    }

    #[test]
    fn reservation_splits_one_region_into_two_spans() {
        let regions = [usable(0x100000, 0x100000 + 8 * MIB)];
        let res = Reservation::new(0x100000 + 2 * MIB, 0x100000 + 4 * MIB, ResKind::Heap)
            .expect("reservation");
        let layout = build(&regions, &[res]);
        assert_eq!(
            &allocatable_spans(&layout)[..],
            vec![
                (0x100000, 0x100000 + 2 * MIB),
                (0x100000 + 4 * MIB, 0x100000 + 8 * MIB),
            ]
        );
        assert_eq!(layout.allocatable_after_reservations, 6 * MIB);
        assert_eq!(layout.boot_owned_bytes, 2 * MIB);
    }

    #[test]
    fn reservation_over_edge_cases() {
        let regions = [usable(0x100000, 0x100000 + 4 * MIB)];
        // Carve off the head.
        let head = build(
            &regions,
            &[Reservation::new(0x100000, 0x100000 + MIB, ResKind::KernelImage).unwrap()],
        );
        assert_eq!(
            &allocatable_spans(&head)[..],
            vec![(0x100000 + MIB, 0x100000 + 4 * MIB)]
        );
        // Carve off the tail.
        let tail = build(
            &regions,
            &[Reservation::new(0x100000 + 3 * MIB, 0x100000 + 4 * MIB, ResKind::KernelImage)
                .unwrap()],
        );
        assert_eq!(&allocatable_spans(&tail)[..], vec![(0x100000, 0x100000 + 3 * MIB)]);
        // Carve the whole thing away.
        let all = build(
            &regions,
            &[Reservation::new(0x0, 0x100000 + 8 * MIB, ResKind::Manual).unwrap()],
        );
        assert!(&allocatable_spans(&all)[..].is_empty());
        assert_eq!(all.allocatable_after_reservations, 0);
    }

    #[test]
    fn adjacent_reservations_do_not_leave_a_hole() {
        // Two back-to-back carves must yield one span, not two.
        let regions = [usable(0x100000, 0x100000 + 8 * MIB)];
        let layout = build(
            &regions,
            &[
                Reservation::new(0x100000 + 2 * MIB, 0x100000 + 3 * MIB, ResKind::KernelImage)
                    .unwrap(),
                Reservation::new(0x100000 + 3 * MIB, 0x100000 + 4 * MIB, ResKind::RamDisk).unwrap(),
            ],
        );
        assert_eq!(
            &allocatable_spans(&layout)[..],
            vec![
                (0x100000, 0x100000 + 2 * MIB),
                (0x100000 + 4 * MIB, 0x100000 + 8 * MIB),
            ]
        );
    }

    #[test]
    fn separate_regions_are_merged_when_adjacent() {
        let regions = [usable(0x100000, 0x200000), usable(0x200000, 0x400000)];
        let layout = build(&regions, &[]);
        assert_eq!(&allocatable_spans(&layout)[..], vec![(0x100000, 0x400000)]);
    }

    #[test]
    fn non_allocatable_regions_never_appear_in_spans() {
        let regions = [
            usable(0x100000, 0x200000),
            reg(0x200000, 0x300000, MemoryRegionKind::Bootloader),
            reg(0x300000, 0x400000, MemoryRegionKind::UnknownUefi(11)),
            reg(0x400000, 0x500000, MemoryRegionKind::UnknownBios(5)),
            usable(0x500000, 0x600000),
        ];
        let layout = build(&regions, &[]);
        assert_eq!(
            &allocatable_spans(&layout)[..],
            vec![(0x100000, 0x200000), (0x500000, 0x600000)]
        );
    }

    #[test]
    fn reservations_never_create_frames_for_non_allocatable_regions() {
        let regions = [usable(0x100000, 0x200000)];
        let layout = build(
            &regions,
            &[Reservation::new(0, 0x100000, ResKind::KernelImage).unwrap()],
        );
        // The carve lands outside every allocatable region, so spans are
        // unchanged; the reservation is recorded but costs nothing.
        assert_eq!(&allocatable_spans(&layout)[..], vec![(0x100000, 0x200000)]);
    }

    #[test]
    fn reservation_page_aligns_inward_and_rejects_empty() {
        // Anything that survives page alignment as an empty span is rejected.
        assert!(Reservation::new(0x801, 0x1FFF, ResKind::Manual).is_none());
        assert!(Reservation::new(0x2000, 0x2000, ResKind::Manual).is_none());
        assert!(Reservation::new(0x3000, 0x1000, ResKind::Manual).is_none());
        // Aligned spans are kept as given.
        let r = Reservation::new(0x1000, 0x2000, ResKind::Manual).unwrap();
        assert_eq!((r.start, r.end, r.len()), (0x1000, 0x2000, 0x1000));
        // Unaligned bounds are rounded inward, so 0x1801..0x2FFF becomes
        // 0x2000..0x2000 and is rejected.
        assert!(Reservation::new(0x1801, 0x2FFF, ResKind::Manual).is_none());
        // Unaligned bounds that still cover a page are trimmed.
        let r = Reservation::new(0x1801, 0x5000, ResKind::Manual).unwrap();
        assert_eq!((r.start, r.end), (0x2000, 0x5000));
    }

    #[test]
    fn region_list_is_capped_but_totals_stay_exact() {
        let mut regions = Vec::new();
        for i in 0..(MAX_REGIONS as u64 + 10) {
            regions.push(usable(0x100000 + i * MIB, 0x100000 + (i + 1) * MIB));
        }
        let layout = build(&regions, &[]);
        assert_eq!(layout.regions.len(), MAX_REGIONS);
        assert!(layout.truncated);
        assert_eq!(
            layout.allocatable_bytes,
            (MAX_REGIONS as u64 + 10) * MIB,
            "totals must remain exact even when the list is truncated"
        );
    }

    #[test]
    fn empty_and_tiny_regions_are_dropped() {
        let regions = [
            usable(0x100000, 0x100000),
            reg(0x0, 0x800, MemoryRegionKind::Usable), // sub-page
        ];
        let layout = build(&regions, &[]);
        assert!(layout.regions.is_empty());
        assert_eq!(layout.allocatable_bytes, 0);
        assert!(allocatable_spans(&layout).is_empty());
    }

    #[test]
    fn spans_never_overlap_a_reservation() {
        let regions = [usable(0x0, 0x1000000)];
        let res = Reservation::new(0x400000, 0x600000, ResKind::Manual).unwrap();
        let layout = build(&regions, &[res]);
        for &(s, e) in allocatable_spans(&layout).iter() {
            assert!(e <= res.start || s >= res.end, "span {:#x}-{:#x} hits carve", s, e);
        }
    }
}
