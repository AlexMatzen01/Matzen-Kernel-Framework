//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Memory-management subsystem.
//!
//! - [`addr`]: physical/virtual translation through the bootloader's direct
//!   map, plus the DMA-reachability check every buffer path needs.
//! - [`memmap`]: firmware memory-map interpretation — classifies regions and
//!   subtracts kernel-owned spans, producing the spans the frame allocator
//!   may hand out.
//! - [`frame_allocator`]: bitmap-backed 4 KiB / 2 MiB physical frame
//!   allocator with free, counters and a DMA-reachable pool.

pub mod addr;
pub mod frame_allocator;
pub mod memmap;
