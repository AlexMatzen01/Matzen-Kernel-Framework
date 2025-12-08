//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Memory Allocator
//!
//! Provides a global allocator for dynamic memory allocation using a static buffer.

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Static heap buffer (512 KiB)
static mut HEAP: [u8; 512 * 1024] = [0; 512 * 1024];

/// Initialize the heap allocator
pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(HEAP.as_mut_ptr(), HEAP.len());
    }
}
