use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

lazy_static! {
    static ref FRAME_ALLOCATOR: Mutex<BootFrameAllocator> = Mutex::new(BootFrameAllocator::new());
}

pub struct BootFrameAllocator {
    regions: Vec<(u64, u64)>,
    current_region: usize,
    current_addr: u64,
}

impl BootFrameAllocator {
    fn new() -> Self {
        Self {
            regions: Vec::new(),
            current_region: 0,
            current_addr: 0,
        }
    }

    pub fn init_from_memory_map(&mut self, memory_regions: &[MemoryRegion]) {
        self.regions.clear();
        self.current_region = 0;
        self.current_addr = 0;

        for region in memory_regions {
            if region.kind == MemoryRegionKind::Usable {
                let start = (region.start + 0xFFF) & !0xFFF;
                let end = region.end & !0xFFF;
                if start < end {
                    self.regions.push((start, end));
                }
            }
        }
        if let Some((start, _)) = self.regions.first() {
            self.current_addr = *start;
        }
        crate::serial_println!(
            "[mem] Frame allocator initialized with {} usable region(s)",
            self.regions.len()
        );
    }

    /// Remove `[reserve_start, reserve_end)` from the usable ranges (e.g.
    /// the pages backing the kernel heap). Regions are clamped or split;
    /// the bump cursor is moved out if it sat inside the reserved span.
    /// Must be called before any frame is handed out past the reservation.
    pub fn reserve_range(&mut self, reserve_start: u64, reserve_end: u64) {
        if reserve_start >= reserve_end {
            return;
        }
        let mut kept: Vec<(u64, u64)> = Vec::new();
        for (start, end) in self.regions.iter().copied() {
            if end <= reserve_start || start >= reserve_end {
                kept.push((start, end));
                continue;
            }
            if start < reserve_start {
                kept.push((start, reserve_start));
            }
            if end > reserve_end {
                kept.push((reserve_end, end));
            }
        }
        self.regions = kept;
        // Re-seat the cursor: first range containing it, else first range.
        let mut seated = false;
        for (i, (start, end)) in self.regions.iter().enumerate() {
            if self.current_addr >= *start && self.current_addr < *end {
                self.current_region = i;
                seated = true;
                break;
            }
        }
        if !seated {
            self.current_region = 0;
            if let Some((start, _)) = self.regions.first() {
                self.current_addr = *start;
            }
        }
        crate::serial_println!(
            "[mem] Reserved {:#x}-{:#x}; {} usable region(s) remain",
            reserve_start,
            reserve_end,
            self.regions.len()
        );
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        loop {
            if self.current_region >= self.regions.len() {
                crate::serial_println!("[mem] Frame allocator exhausted");
                return None;
            }
            let (start, end) = self.regions[self.current_region];
            if self.current_addr + 4096 <= end {
                let frame = PhysFrame::containing_address(PhysAddr::new(self.current_addr));
                self.current_addr += 4096;
                return Some(frame);
            }
            self.current_region += 1;
            if self.current_region < self.regions.len() {
                self.current_addr = self.regions[self.current_region].0;
            }
        }
    }
}

pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        FRAME_ALLOCATOR.lock().allocate_frame()
    }
}

pub fn frame_allocator() -> GlobalFrameAllocator {
    GlobalFrameAllocator
}

pub fn init_from_memory_map(memory_regions: &[MemoryRegion]) {
    FRAME_ALLOCATOR.lock().init_from_memory_map(memory_regions);
}

/// Carve `[start, end)` out of future frame allocation (heap backing).
pub fn reserve_range(start: u64, end: u64) {
    FRAME_ALLOCATOR.lock().reserve_range(start, end);
}
