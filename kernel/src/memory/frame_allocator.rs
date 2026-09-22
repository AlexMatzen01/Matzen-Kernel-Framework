use x86_64::structures::paging::{FrameAllocator, Size4KiB, PhysFrame};
use x86_64::PhysAddr;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use spin::Mutex;
use lazy_static::lazy_static;
use alloc::vec::Vec;

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
        Self { regions: Vec::new(), current_region: 0, current_addr: 0 }
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
        crate::serial_println!("[mem] Frame allocator initialized with {} usable region(s)", self.regions.len());
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