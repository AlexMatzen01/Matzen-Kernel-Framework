//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Framebuffer graphics primitives for the desktop.
//!
//! Provides per-pixel drawing, sprite blitting, and save/restore
//! for the graphical desktop. Separated from fb.rs (text console)
//! to keep the streaming console simple and the graphics API focused.
//!
//! Features double buffering to prevent screen tearing. Buffers are
//! allocated from physical frames via the frame allocator. Frame lists
//! are stored in static arrays (no heap allocation, no leaking).

use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use spin::Mutex;

use crate::drivers::fb::{FrameBufferInfo, PixelFormat};
use crate::drivers::vga::Color;
use crate::memory::frame_allocator;
use x86_64::structures::paging::{FrameAllocator, Size4KiB, PhysFrame};
use x86_64::PhysAddr;
use core::fmt;

/// Maximum frames per buffer.
/// Supports up to 3840×2160×4bpp (4K UHD) = 33 MB = 8192 frames.
const MAX_FRAMES_PER_BUFFER: usize = 8192;

/// Static arrays for frame lists (no heap allocation, no leaking).
static mut FRONT_FRAMES: [PhysFrame<Size4KiB>; MAX_FRAMES_PER_BUFFER] = 
    [PhysFrame::containing_address(PhysAddr::new(0)); MAX_FRAMES_PER_BUFFER];
static mut BACK_FRAMES: [PhysFrame<Size4KiB>; MAX_FRAMES_PER_BUFFER] = 
    [PhysFrame::containing_address(PhysAddr::new(0)); MAX_FRAMES_PER_BUFFER];

/// Double-buffered framebuffer state using static frame arrays.
struct DoubleBuffer {
    /// Slice to front buffer frames
    front_frames: &'static mut [PhysFrame<Size4KiB>],
    /// Slice to back buffer frames
    back_frames: &'static mut [PhysFrame<Size4KiB>],
    /// Framebuffer info for bounds checking
    info: FrameBufferInfo,
    /// Physical memory offset for virtual address translation
    phys_offset: u64,
    /// Whether double buffering is enabled
    enabled: bool,
}

static DOUBLE_BUFFER: Mutex<Option<DoubleBuffer>> = Mutex::new(None);

static mut CAPTURE_ACTIVE: bool = false;
static mut CAPTURE_BUFFER: *mut alloc::string::String = core::ptr::null_mut();

/// Initialize double buffering with the given framebuffer info and physical memory offset.
/// Allocates front and back buffers from physical frames via the frame allocator.
/// Frame lists are stored in static arrays (no heap allocation, no leaking).
pub fn init_double_buffer(info: FrameBufferInfo, phys_offset: u64) {
    let byte_len = info.byte_len;
    let frames_per_buffer = (byte_len + 4095) / 4096;
    
    if frames_per_buffer > MAX_FRAMES_PER_BUFFER {
        crate::serial_println!("[fb_gfx] ERROR: Framebuffer too large ({} frames, max {})", 
            frames_per_buffer, MAX_FRAMES_PER_BUFFER);
        return;
    }
    
    crate::serial_println!("[fb_gfx] Allocating {} frames per buffer ({} bytes each)", frames_per_buffer, byte_len);
    
    let mut frame_alloc = crate::memory::frame_allocator::frame_allocator();
    
    // Allocate front buffer frames
    for i in 0..frames_per_buffer {
        unsafe {
            FRONT_FRAMES[i] = frame_alloc.allocate_frame()
                .expect("[fb_gfx] Failed to allocate front buffer frame");
        }
    }
    
    // Allocate back buffer frames
    for i in 0..frames_per_buffer {
        unsafe {
            BACK_FRAMES[i] = frame_alloc.allocate_frame()
                .expect("[fb_gfx] Failed to allocate back buffer frame");
        }
    }
    
    // Create slices from static arrays
    let front_slice = unsafe {
        core::slice::from_raw_parts_mut(FRONT_FRAMES.as_mut_ptr(), frames_per_buffer)
    };
    let back_slice = unsafe {
        core::slice::from_raw_parts_mut(BACK_FRAMES.as_mut_ptr(), frames_per_buffer)
    };
    
    let mut db = DOUBLE_BUFFER.lock();
    *db = Some(DoubleBuffer {
        front_frames: front_slice,
        back_frames: back_slice,
        info,
        phys_offset,
        enabled: true,
    });
    
    crate::serial_println!("[fb_gfx] Double buffering initialized: {} bytes per buffer ({} frames each)", 
        byte_len, frames_per_buffer);
}

/// Check if double buffering is enabled.
pub fn is_double_buffered() -> bool {
    DOUBLE_BUFFER.lock().as_ref().map(|db| db.enabled).unwrap_or(false)
}

/// Get the virtual base address of the front buffer.
fn front_buffer_base(db: &DoubleBuffer) -> usize {
    (db.front_frames[0].start_address().as_u64() + db.phys_offset) as usize
}

/// Get the virtual base address of the back buffer.
fn back_buffer_base(db: &DoubleBuffer) -> usize {
    (db.back_frames[0].start_address().as_u64() + db.phys_offset) as usize
}

/// Get the total size of a buffer in bytes.
fn buffer_byte_len(frames: &[PhysFrame<Size4KiB>]) -> usize {
    frames.len() * 4096
}

/// Get the back buffer for drawing (mut).
pub fn with_back_buffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut [u8], &FrameBufferInfo) -> R,
{
    let mut db = DOUBLE_BUFFER.lock();
    if let Some(ref mut db) = db.as_mut() {
        if db.enabled {
            let base = back_buffer_base(db);
            let len = buffer_byte_len(db.back_frames);
            let slice = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, len) };
            return Some(f(slice, &db.info));
        }
    }
    None
}

/// Get the front buffer (read-only).
pub fn with_front_buffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&[u8], &FrameBufferInfo) -> R,
{
    let db = DOUBLE_BUFFER.lock();
    if let Some(ref db) = db.as_ref() {
        if db.enabled {
            let base = front_buffer_base(db);
            let len = buffer_byte_len(db.front_frames);
            let slice = unsafe { core::slice::from_raw_parts(base as *const u8, len) };
            return Some(f(slice, &db.info));
        }
    }
    None
}

/// Swap front and back buffers (present).
/// Copies only dirty rectangles from back to front buffer.
pub fn swap_buffers(dirty_rects: &[(usize, usize, usize, usize)]) {
    let mut db = DOUBLE_BUFFER.lock();
    if let Some(ref mut db) = db.as_mut() {
        if !db.enabled {
            return;
        }
        
        let info = &db.info;
        let stride = info.stride;
        let bpp = info.bytes_per_pixel;
        
        let front_base = front_buffer_base(db);
        let back_base = back_buffer_base(db);
        
        for &(x, y, w, h) in dirty_rects {
            if w == 0 || h == 0 {
                continue;
            }
            let x_end = (x + w).min(info.width);
            let y_end = (y + h).min(info.height);
            
            for py in y..y_end {
                let src_offset = (py * stride + x) * bpp;
                let dst_offset = src_offset;
                let row_bytes = (x_end - x) * bpp;
                
                unsafe {
                    let src = (back_base as *mut u8).add(src_offset);
                    let dst = (front_base as *mut u8).add(dst_offset);
                    core::ptr::copy_nonoverlapping(src, dst, row_bytes);
                }
            }
        }
    }
}

/// Clear the back buffer to a solid color.
pub fn clear_back_buffer(r: u8, g: u8, b: u8) {
    with_back_buffer(|back, info| {
        let solid = match info.pixel_format {
            PixelFormat::Rgb => [r, g, b, 0],
            _ => [b, g, r, 0], // Bgr / U8(approx) / Unknown
        };
        
        if info.bytes_per_pixel == 4 {
            let px = u32::from_le_bytes(solid);
            let count = back.len() / 4;
            let dst = back.as_mut_ptr() as *mut u32;
            for i in 0..count {
                unsafe { core::ptr::write_volatile(dst.add(i), px); }
            }
        } else {
            for py in 0..info.height {
                for px in 0..info.width {
                    let offset = (py * info.stride + px) * info.bytes_per_pixel;
                    if offset + info.bytes_per_pixel.min(4) <= back.len() {
                        for i in 0..info.bytes_per_pixel.min(4) {
                            unsafe { core::ptr::write_volatile(back.as_mut_ptr().add(offset + i), solid[i]); }
                        }
                    }
                }
            }
        }
    });
}

/// Initialize capture of streaming text output.
/// When active, all text written via fb::write_string is also appended here.
pub fn start_capture() {
    use alloc::string::String;
    unsafe {
        let s = alloc::boxed::Box::new(String::new());
        CAPTURE_BUFFER = alloc::boxed::Box::into_raw(s);
        CAPTURE_ACTIVE = true;
    }
}

/// Stop capture and return the captured text.
pub fn stop_capture() -> alloc::string::String {
    unsafe {
        CAPTURE_ACTIVE = false;
        if !CAPTURE_BUFFER.is_null() {
            let s = *alloc::boxed::Box::from_raw(CAPTURE_BUFFER);
            CAPTURE_BUFFER = core::ptr::null_mut();
            s
        } else {
            alloc::string::String::new()
        }
    }
}

/// Helper to append to capture buffer if active.
pub(crate) fn capture_write(s: &str) {
    unsafe {
        if CAPTURE_ACTIVE && !CAPTURE_BUFFER.is_null() {
            (*CAPTURE_BUFFER).push_str(s);
        }
    }
}

/// Exposed for fb.rs to call when writing streaming text.
pub fn capture_write_str(s: &str) {
    capture_write(s);
}

/// Fill a pixel rectangle with RGB color (draws to back buffer if double buffered).
pub fn fill_rect_px(x: usize, y: usize, w: usize, h: usize, r: u8, g: u8, b: u8) {
    if is_double_buffered() {
        with_back_buffer(|back, info| {
            crate::drivers::fb::with_lock(|st| {
                // Temporarily redirect to back buffer
                let old_base = st.base;
                let old_len = st.byte_len;
                let db = DOUBLE_BUFFER.lock();
                if let Some(ref db) = db.as_ref() {
                    st.base = back_buffer_base(db);
                    st.byte_len = buffer_byte_len(db.back_frames);
                }
                drop(db);
                st.fill_px_rect(x, y, w, h, r, g, b);
                st.base = old_base;
                st.byte_len = old_len;
            });
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.fill_px_rect(x, y, w, h, r, g, b);
        });
    }
}

/// Draw a hollow rectangle outline in RGB color.
pub fn rect_px(x: usize, y: usize, w: usize, h: usize, r: u8, g: u8, b: u8) {
    if is_double_buffered() {
        with_back_buffer(|back, info| {
            crate::drivers::fb::with_lock(|st| {
                let old_base = st.base;
                let old_len = st.byte_len;
                let db = DOUBLE_BUFFER.lock();
                if let Some(ref db) = db.as_ref() {
                    st.base = back_buffer_base(db);
                    st.byte_len = buffer_byte_len(db.back_frames);
                }
                drop(db);
                st.rect_px(x, y, w, h, r, g, b);
                st.base = old_base;
                st.byte_len = old_len;
            });
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.rect_px(x, y, w, h, r, g, b);
        });
    }
}

/// Blit a 32-bit RGBA bitmap (row-major, w*4 bytes per row).
/// `rgba` must be exactly `w * h * 4` bytes.
pub fn blit_rgba(x: usize, y: usize, w: usize, h: usize, rgba: &[u8]) {
    if is_double_buffered() {
        with_back_buffer(|back, info| {
            crate::drivers::fb::with_lock(|st| {
                let old_base = st.base;
                let old_len = st.byte_len;
                let db = DOUBLE_BUFFER.lock();
                if let Some(ref db) = db.as_ref() {
                    st.base = back_buffer_base(db);
                    st.byte_len = buffer_byte_len(db.back_frames);
                }
                drop(db);
                st.blit_rgba(x, y, w, h, rgba);
                st.base = old_base;
                st.byte_len = old_len;
            });
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.blit_rgba(x, y, w, h, rgba);
        });
    }
}

/// Save a pixel rectangle to a heap-allocated Vec<u8> (row-major RGBA).
/// Returns the pixel data (w * h * 4 bytes).
pub fn save_pixels(x: usize, y: usize, w: usize, h: usize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(w * h * 4);
    if is_double_buffered() {
        with_front_buffer(|front, info| {
            crate::drivers::fb::with_lock(|st| {
                let old_base = st.base;
                let old_len = st.byte_len;
                let db = DOUBLE_BUFFER.lock();
                if let Some(ref db) = db.as_ref() {
                    st.base = front_buffer_base(db);
                    st.byte_len = buffer_byte_len(db.front_frames);
                }
                drop(db);
                st.save_px_rect(x, y, w, h, &mut out);
                st.base = old_base;
                st.byte_len = old_len;
            });
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.save_px_rect(x, y, w, h, &mut out);
        });
    }
    out
}

/// Restore a pixel rectangle from RGBA data (row-major, w*4 bytes per row).
pub fn restore_pixels(x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
    if is_double_buffered() {
        with_back_buffer(|back, info| {
            crate::drivers::fb::with_lock(|st| {
                let old_base = st.base;
                let old_len = st.byte_len;
                let db = DOUBLE_BUFFER.lock();
                if let Some(ref db) = db.as_ref() {
                    st.base = back_buffer_base(db);
                    st.byte_len = buffer_byte_len(db.back_frames);
                }
                drop(db);
                st.restore_px_rect(x, y, w, h, data);
                st.base = old_base;
                st.byte_len = old_len;
            });
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.restore_px_rect(x, y, w, h, data);
        });
    }
}

/// Write a string at cell position using the text API (delegates to fb).
pub fn write_str_at_fb(row: usize, col: usize, s: &str, fg: Color, bg: Color) {
    crate::drivers::fb::write_str_at(row, col, s, fg, bg);
}

/// Fill a cell rectangle with a character and colors (delegates to fb).
pub fn fill_rect_cells_fb(row: usize, col: usize, w: usize, h: usize, ch: u8, fg: Color, bg: Color) {
    crate::drivers::fb::fill_rect(row, col, w, h, ch, fg, bg);
}

/// Write a single cell at absolute position (delegates to fb).
pub fn write_at_fb(row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
    crate::drivers::fb::write_at(row, col, byte, fg, bg);
}

/// Get framebuffer pixel dimensions.
pub fn framebuffer_size() -> Option<(usize, usize)> {
    crate::drivers::fb::pixel_size()
}

/// Get framebuffer text grid dimensions.
pub fn framebuffer_text_size() -> Option<(usize, usize)> {
    crate::drivers::fb::text_size()
}

/// Copy the front buffer to the actual hardware framebuffer.
/// This should be called after swap_buffers to present to screen.
pub fn present_to_hardware() {
    let db = DOUBLE_BUFFER.lock();
    if let Some(ref db) = db.as_ref() {
        if db.enabled {
            // Copy front buffer to the actual hardware framebuffer
            // The hardware FB is mapped at the original base address
            crate::drivers::fb::with_lock(|st| {
                let hw_base = st.base;
                let hw_len = st.byte_len;
                
                let front_base = front_buffer_base(db);
                let front_len = buffer_byte_len(db.front_frames);
                
                if hw_len == front_len {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            front_base as *const u8,
                            hw_base as *mut u8,
                            hw_len,
                        );
                    }
                }
            });
        }
    }
}