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

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::fb::{FrameBufferInfo, PixelFormat};
use crate::drivers::vga::Color;
use crate::memory::frame_allocator;
use core::fmt;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

/// Maximum frames per buffer.
/// Supports up to 3840×2160×4bpp (4K UHD) = 33 MB = 8192 frames.
const MAX_FRAMES_PER_BUFFER: usize = 8192;

/// Static arrays for frame lists (no heap allocation, no leaking).
static mut FRONT_FRAMES: [PhysFrame<Size4KiB>; MAX_FRAMES_PER_BUFFER] =
    [PhysFrame::containing_address(PhysAddr::new(0)); MAX_FRAMES_PER_BUFFER];
static mut BACK_FRAMES: [PhysFrame<Size4KiB>; MAX_FRAMES_PER_BUFFER] =
    [PhysFrame::containing_address(PhysAddr::new(0)); MAX_FRAMES_PER_BUFFER];

/// Double-buffered framebuffer state using static frame arrays.
///
/// WARNING: the frame lists are scattered 4KiB pages, but base+len treats
/// them as contiguous — writing past the first page aliases random RAM.
/// Do NOT re-enable `init_double_buffer` until a real virtual-range mapper
/// backs these lists with consecutive pages. The desktop draws direct and
/// stays fast via small dirty rects instead.
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

/// Temporary drawing clip used by the desktop compositor while repainting a
/// damage rectangle. `None` preserves the existing unrestricted drawing API.
static DRAW_CLIP: Mutex<Option<(usize, usize, usize, usize)>> = Mutex::new(None);

pub fn set_draw_clip(clip: Option<(usize, usize, usize, usize)>) {
    *DRAW_CLIP.lock() = clip;
}

fn clipped_rect(x: usize, y: usize, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    if w == 0 || h == 0 {
        return None;
    }
    let Some((cx, cy, cw, ch)) = *DRAW_CLIP.lock() else {
        return Some((x, y, w, h));
    };
    let left = x.max(cx);
    let top = y.max(cy);
    let right = x.saturating_add(w).min(cx.saturating_add(cw));
    let bottom = y.saturating_add(h).min(cy.saturating_add(ch));
    if right > left && bottom > top {
        Some((left, top, right - left, bottom - top))
    } else {
        None
    }
}

fn clipped_text_rect(
    clip: Option<(usize, usize, usize, usize)>,
) -> Option<(usize, usize, usize, usize)> {
    let damage = *DRAW_CLIP.lock();
    match (clip, damage) {
        (Some((x, y, w, h)), Some((cx, cy, cw, ch))) => {
            let left = x.max(cx);
            let top = y.max(cy);
            let right = x.saturating_add(w).min(cx.saturating_add(cw));
            let bottom = y.saturating_add(h).min(cy.saturating_add(ch));
            if right > left && bottom > top {
                Some((left, top, right - left, bottom - top))
            } else {
                // `Some(empty)` means draw no glyphs; `None` means unbounded.
                Some((0, 0, 0, 0))
            }
        }
        (Some(clip), None) | (None, Some(clip)) => Some(clip),
        (None, None) => None,
    }
}

static mut CAPTURE_ACTIVE: bool = false;
static mut CAPTURE_BUFFER: *mut alloc::string::String = core::ptr::null_mut();

/// Initialize double buffering with the given framebuffer info and physical memory offset.
/// Allocates front and back buffers from physical frames via the frame allocator.
/// Frame lists are stored in static arrays (no heap allocation, no leaking).
/// Fail-open: returns false (drawing stays direct-to-hardware) instead of
/// panicking when frames run out. Safe to call repeatedly; second call is a
/// no-op returning true.
pub fn init_double_buffer(info: FrameBufferInfo, phys_offset: u64) -> bool {
    if is_double_buffered() {
        return true;
    }

    let byte_len = info.byte_len;
    let frames_per_buffer = (byte_len + 4095) / 4096;

    if frames_per_buffer == 0 || frames_per_buffer > MAX_FRAMES_PER_BUFFER {
        crate::serial_println!(
            "[fb_gfx] ERROR: Framebuffer too large ({} frames, max {})",
            frames_per_buffer,
            MAX_FRAMES_PER_BUFFER
        );
        return false;
    }

    crate::serial_println!(
        "[fb_gfx] Allocating {} frames per buffer ({} bytes each)",
        frames_per_buffer,
        byte_len
    );

    let mut frame_alloc = crate::memory::frame_allocator::frame_allocator();

    // Allocate front buffer frames
    for i in 0..frames_per_buffer {
        match frame_alloc.allocate_frame() {
            Some(f) => unsafe {
                FRONT_FRAMES[i] = f;
            },
            None => {
                crate::serial_println!(
                    "[fb_gfx] Out of frames for front buffer, double buffering off"
                );
                return false;
            }
        }
    }

    // Allocate back buffer frames
    for i in 0..frames_per_buffer {
        match frame_alloc.allocate_frame() {
            Some(f) => unsafe {
                BACK_FRAMES[i] = f;
            },
            None => {
                crate::serial_println!(
                    "[fb_gfx] Out of frames for back buffer, double buffering off"
                );
                return false;
            }
        }
    }

    // Create slices from static arrays
    let front_slice =
        unsafe { core::slice::from_raw_parts_mut(FRONT_FRAMES.as_mut_ptr(), frames_per_buffer) };
    let back_slice =
        unsafe { core::slice::from_raw_parts_mut(BACK_FRAMES.as_mut_ptr(), frames_per_buffer) };

    let mut db = DOUBLE_BUFFER.lock();
    *db = Some(DoubleBuffer {
        front_frames: front_slice,
        back_frames: back_slice,
        info,
        phys_offset,
        enabled: true,
    });

    crate::serial_println!(
        "[fb_gfx] Double buffering initialized: {} bytes per buffer ({} frames each)",
        byte_len,
        frames_per_buffer
    );
    true
}

/// Check if double buffering is enabled.
pub fn is_double_buffered() -> bool {
    DOUBLE_BUFFER
        .lock()
        .as_ref()
        .map(|db| db.enabled)
        .unwrap_or(false)
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
                unsafe {
                    core::ptr::write_volatile(dst.add(i), px);
                }
            }
        } else {
            for py in 0..info.height {
                for px in 0..info.width {
                    let offset = (py * info.stride + px) * info.bytes_per_pixel;
                    if offset + info.bytes_per_pixel.min(4) <= back.len() {
                        for i in 0..info.bytes_per_pixel.min(4) {
                            unsafe {
                                core::ptr::write_volatile(
                                    back.as_mut_ptr().add(offset + i),
                                    solid[i],
                                );
                            }
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

/// Snapshot the back-buffer store (base, byte_len) when double buffering
/// is on. The lock is released before returning, so callers can then take
/// the framebuffer lock without nesting. Returns None when drawing should
/// go straight to hardware.
fn back_store() -> Option<(usize, usize)> {
    let db = DOUBLE_BUFFER.lock();
    let store = match db.as_ref() {
        Some(db) if db.enabled => Some((back_buffer_base(db), buffer_byte_len(db.back_frames))),
        _ => None,
    };
    store
}

/// Snapshot the front-buffer store (base, byte_len) for save/restore reads.
fn front_store() -> Option<(usize, usize)> {
    let db = DOUBLE_BUFFER.lock();
    let store = match db.as_ref() {
        Some(db) if db.enabled => Some((front_buffer_base(db), buffer_byte_len(db.front_frames))),
        _ => None,
    };
    store
}

/// Fill a pixel rectangle with RGB color (draws to back buffer if double buffered).
pub fn fill_rect_px(x: usize, y: usize, w: usize, h: usize, r: u8, g: u8, b: u8) {
    let Some((x, y, w, h)) = clipped_rect(x, y, w, h) else {
        return;
    };
    // Lock order is strictly sequential (buffer lock, then fb lock via
    // with_mapped_base) — never nested, so this cannot deadlock.
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.fill_px_rect(x, y, w, h, r, g, b);
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.fill_px_rect(x, y, w, h, r, g, b);
        });
    }
}

/// Draw a hollow rectangle outline in RGB color.
pub fn rect_px(x: usize, y: usize, w: usize, h: usize, r: u8, g: u8, b: u8) {
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.rect_px(x, y, w, h, r, g, b);
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
    let Some((cx, cy, cw, ch)) = clipped_rect(x, y, w, h) else {
        return;
    };
    let src_x = cx - x;
    let src_y = cy - y;
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.blit_rgba(cx, cy, cw, ch, rgba, w, src_x, src_y);
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.blit_rgba(cx, cy, cw, ch, rgba, w, src_x, src_y);
        });
    }
}

/// Blit a bitmap whose origin may be outside the framebuffer. Source pixels
/// are cropped on the left/top before applying screen and compositor clips.
pub fn blit_rgba_signed(x: i32, y: i32, w: usize, h: usize, rgba: &[u8]) {
    let Some((screen_w, screen_h)) = crate::drivers::fb::pixel_size() else {
        return;
    };
    let source_x = if x < 0 {
        x.saturating_neg() as usize
    } else {
        0
    };
    let source_y = if y < 0 {
        y.saturating_neg() as usize
    } else {
        0
    };
    if source_x >= w || source_y >= h {
        return;
    }
    let dest_x = x.max(0) as usize;
    let dest_y = y.max(0) as usize;
    if dest_x >= screen_w || dest_y >= screen_h {
        return;
    }
    let draw_w = (w - source_x).min(screen_w - dest_x);
    let draw_h = (h - source_y).min(screen_h - dest_y);
    let Some((cx, cy, cw, ch)) = clipped_rect(dest_x, dest_y, draw_w, draw_h) else {
        return;
    };
    let source_x = source_x + cx - dest_x;
    let source_y = source_y + cy - dest_y;
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.blit_rgba(cx, cy, cw, ch, rgba, w, source_x, source_y);
        });
    } else {
        crate::drivers::fb::with_lock(|st| {
            st.blit_rgba(cx, cy, cw, ch, rgba, w, source_x, source_y);
        });
    }
}

/// Transparent pixel text (back buffer when double buffered, else hardware).
pub fn draw_text_bb(
    x: usize,
    y: usize,
    s: &str,
    rgb: (u8, u8, u8),
    clip: Option<(usize, usize, usize, usize)>,
) {
    let clip = clipped_text_rect(clip);
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            crate::drivers::fb::draw_text_px_in(st, x, y, s, rgb, clip);
        });
    } else {
        crate::drivers::fb::draw_text_px(x, y, s, rgb, clip);
    }
}

/// Wallpaper gradient (back buffer when double buffered, else hardware).
pub fn paint_wallpaper_bb(x: usize, y: usize, w: usize, h: usize, top: (u8, u8, u8)) {
    let Some((x, y, w, h)) = clipped_rect(x, y, w, h) else {
        return;
    };
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            crate::drivers::fb::paint_wallpaper_gradient_in(st, x, y, w, h, top);
        });
    } else {
        crate::drivers::fb::paint_wallpaper_gradient(x, y, w, h, top);
    }
}

/// Save a pixel rectangle to a heap-allocated Vec<u8> (row-major RGBA).
/// Returns the pixel data (w * h * 4 bytes). Reads the front buffer when
/// double buffered (the presented image), else hardware directly.
pub fn save_pixels(x: usize, y: usize, w: usize, h: usize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(w * h * 4);
    if let Some((base, len)) = front_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.save_px_rect(x, y, w, h, &mut out);
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
    if let Some((base, len)) = back_store() {
        crate::drivers::fb::with_mapped_base(base, len, |st| {
            st.restore_px_rect(x, y, w, h, data);
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
pub fn fill_rect_cells_fb(
    row: usize,
    col: usize,
    w: usize,
    h: usize,
    ch: u8,
    fg: Color,
    bg: Color,
) {
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

/// Copy dirty rectangles from the front buffer to the actual hardware
/// framebuffer. Row-wise copies only (same shape as `swap_buffers`), so a
/// cursor twitch presents kilobytes instead of the whole screen.
pub fn present_to_hardware(dirty_rects: &[(usize, usize, usize, usize)]) {
    let db = DOUBLE_BUFFER.lock();
    let buffered = db.as_ref().and_then(|db| {
        db.enabled.then(|| {
            (
                front_buffer_base(db),
                buffer_byte_len(db.front_frames),
                db.info,
            )
        })
    });
    drop(db);

    let (front_base, front_len, info) = match buffered {
        Some(buffered) => buffered,
        None => return,
    };

    crate::drivers::fb::with_lock(|st| {
        let hw_base = st.base;
        let hw_len = st.byte_len.min(front_len);
        let stride = info.stride;
        let bpp = info.bytes_per_pixel;
        for &(x, y, w, h) in dirty_rects {
            if w == 0 || h == 0 {
                continue;
            }
            let x_end = x.saturating_add(w).min(info.width);
            let y_end = y.saturating_add(h).min(info.height);
            for py in y..y_end {
                let off = (py * stride + x) * bpp;
                let row_bytes = (x_end - x) * bpp;
                if off + row_bytes > hw_len {
                    break;
                }
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        (front_base as *const u8).add(off),
                        (hw_base as *mut u8).add(off),
                        row_bytes,
                    );
                }
            }
        }
    });
}
