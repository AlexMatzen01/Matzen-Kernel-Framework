//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Framebuffer graphics primitives for the desktop.
//!
//! Provides per-pixel drawing, sprite blitting, and save/restore
//! for the graphical desktop. Separated from fb.rs (text console)
//! to keep the streaming console simple and the graphics API focused.
//!
//! The desktop draws into a heap-allocated back buffer and only touches the
//! hardware framebuffer in [`present`], once per frame, for the damaged
//! region. That is what keeps a window update from being visible while it is
//! being painted.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::fb::{FrameBufferInfo, PixelFormat};
use crate::drivers::vga::Color;
use core::fmt;

/// Off-screen copy of the screen that all drawing targets.
///
/// An earlier version built its frame lists from scattered 4 KiB physical
/// pages and then treated them as one contiguous range, so writing past the
/// first page aliased unrelated RAM. It was never safe to enable, and so it
/// was never enabled: the desktop drew straight into the live GOP
/// framebuffer, which meant every window repaint became visible immediately
/// and swept from the top of the damage rect downwards as the compositor
/// walked it.
///
/// A `Vec` from the kernel heap is contiguous by construction, so none of
/// that applies. Its length is `stride * height` to match the hardware row
/// layout, which lets the same drawing code run against either store
/// (`fb::with_mapped_base` just redirects `FbState` at another base).
struct DoubleBuffer {
    /// Everything is drawn here; `present` copies damage out of it.
    back: Vec<u8>,
    /// Framebuffer geometry, captured at init.
    info: FrameBufferInfo,
    /// Cleared if the heap could not supply the buffer.
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

/// Allocate the back buffer from the kernel heap.
///
/// Fail-open: returns `false` and leaves drawing pointed at the hardware
/// framebuffer if the heap cannot supply a full-screen buffer, so a
/// memory-constrained machine still gets a desktop (with the tearing this
/// module exists to remove). Safe to call repeatedly; the second call is a
/// no-op returning `true`.
pub fn init_back_buffer() -> bool {
    if is_double_buffered() {
        return true;
    }
    let Some(info) = crate::drivers::fb::get_framebuffer_info() else {
        crate::serial_println!("[fb_gfx] no framebuffer info; drawing direct to hardware");
        return false;
    };

    // Match the hardware buffer size exactly, including any stride padding, so
    // the drawing primitives can be pointed at either store unchanged.
    // `byte_len` is authoritative; derive it if the bootloader left it unset.
    // (`stride` is in *pixels*, hence the multiply by bytes-per-pixel.)
    let needed = if info.byte_len != 0 {
        info.byte_len
    } else {
        info.stride
            .saturating_mul(info.height)
            .saturating_mul(info.bytes_per_pixel)
    };
    if needed == 0 {
        crate::serial_println!("[fb_gfx] degenerate framebuffer geometry; drawing direct");
        return false;
    }

    let mut back: Vec<u8> = Vec::new();
    if back.try_reserve_exact(needed).is_err() {
        crate::serial_println!(
            "[fb_gfx] cannot reserve {} bytes for the back buffer; drawing direct",
            needed
        );
        return false;
    }
    back.resize(needed, 0);

    // Seed from the current screen so the first damage-only pass cannot expose
    // whatever the heap happened to hand us.
    crate::drivers::fb::with_lock(|st| {
        let n = needed.min(st.byte_len);
        unsafe {
            core::ptr::copy_nonoverlapping(st.base as *const u8, back.as_mut_ptr(), n);
        }
    });

    *DOUBLE_BUFFER.lock() = Some(DoubleBuffer {
        back,
        info,
        enabled: true,
    });

    crate::serial_println!(
        "[fb_gfx] back buffer: {} bytes ({}x{}, stride {})",
        needed,
        info.width,
        info.height,
        info.stride
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

/// Get the virtual base address of the back buffer.
fn back_buffer_base(db: &mut DoubleBuffer) -> usize {
    db.back.as_mut_ptr() as usize
}

/// Get the back buffer for drawing (mut).
pub fn with_back_buffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut [u8], &FrameBufferInfo) -> R,
{
    let mut db = DOUBLE_BUFFER.lock();
    if let Some(ref mut db) = db.as_mut() {
        if db.enabled {
            let info = db.info;
            return Some(f(db.back.as_mut_slice(), &info));
        }
    }
    None
}

/// One contiguous row range of a damage rect: `y` plus the byte offsets of its
/// first and last pixel. Kept separate from the copy so the clipping rules are
/// unit-testable without a framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    /// Row index within the framebuffer.
    pub y: usize,
    /// Byte offset of the first pixel in the row.
    pub start: usize,
    /// Byte offset one past the last pixel in the row.
    pub end: usize,
}

/// Turn damage rects into the row spans to copy, clipped to the screen.
///
/// Returns `None` if the screen bounds would leave nothing to copy, so the
/// caller can skip the frame entirely. A rect that only partly fits is
/// truncated rather than dropped, and zero-sized rects are ignored.
pub fn damage_copy_plan(
    damage: &[(usize, usize, usize, usize)],
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
) -> Option<Vec<RowSpan>> {
    let mut spans: Vec<RowSpan> = Vec::new();
    for &(x, y, w, h) in damage {
        if w == 0 || h == 0 || x >= width || y >= height {
            continue;
        }
        let x_end = x.saturating_add(w).min(width);
        let y_end = y.saturating_add(h).min(height);
        if x_end <= x {
            continue;
        }
        for py in y..y_end {
            let start = (py * stride + x) * bpp;
            spans.push(RowSpan {
                y: py,
                start,
                end: (py * stride + x_end) * bpp,
            });
        }
    }
    if spans.is_empty() {
        None
    } else {
        Some(spans)
    }
}

/// Gate presents to the estimated start of the display's frame.
///
/// The GOP aperture — which carries `CurrentDisplayLine`, the only way to
/// actually observe scanout — is not reachable: `bootloader_api` hands over
/// the linear framebuffer, not the protocol handle. So the period is estimated
/// from the interval between successive presents and the copy is aimed just
/// after the predicted frame boundary. That shrinks the window during which a
/// mixed old/new image is visible; it cannot guarantee a tear-free present.
///
/// Every wait is bounded by one estimated period, so the worst case is the same
/// unsynchronised copy that happens today.
struct FramePacer {
    /// Smoothed period in milliseconds; 0 until enough samples arrive.
    period_ms: u64,
    /// `uptime_millis()` of the previous present.
    last_ms: u64,
    /// Samples observed so far, used to gate engagement on `WARMUP`.
    samples: u32,
    /// Set once the estimate is trusted and waiting has begun.
    engaged: bool,
    /// Set when engagement first happens so the line is logged exactly once.
    announce: bool,
}

impl FramePacer {
    const MIN_PERIOD_MS: u64 = 8;
    const MAX_PERIOD_MS: u64 = 100;
    /// Samples needed before the estimate is trusted. A couple of early
    /// frames are unrepresentative (window open, first layout), so waiting on
    /// them would aim the copy at the wrong moment.
    const WARMUP: u32 = 8;

    const fn new() -> Self {
        Self {
            period_ms: 0,
            last_ms: 0,
            samples: 0,
            engaged: false,
            announce: false,
        }
    }

    /// Feed one inter-present interval and report whether to wait.
    fn observe(&mut self, now_ms: u64) -> bool {
        if self.last_ms != 0 {
            let delta = now_ms.saturating_sub(self.last_ms);
            if delta >= Self::MIN_PERIOD_MS && delta <= Self::MAX_PERIOD_MS {
                // Exponential average: tracks a refresh-rate change without
                // letting one long frame dominate.
                self.period_ms = if self.period_ms == 0 {
                    delta
                } else {
                    (self.period_ms * 3 + delta) / 4
                };
                self.samples += 1;
            }
        }
        self.last_ms = now_ms;

        if !self.engaged {
            if self.samples < Self::WARMUP {
                return false;
            }
            self.engaged = true;
            self.announce = true;
        }
        true
    }

    /// Spin until the copy is unlikely to straddle active scanout.
    ///
    /// Only fine-tunes when the next predicted boundary is already close: if it
    /// is far away the estimate has drifted and blocking for it would cost more
    /// than the tear it saves. So the wait is capped at a fraction of a frame,
    /// which bounds the added latency to a few milliseconds per frame.
    fn wait(&mut self, now_ms: u64) {
        if !self.observe(now_ms) || self.period_ms == 0 {
            return;
        }
        if self.announce {
            crate::serial_println!(
                "[fb_gfx] frame pacing engaged (~{} ms/frame)",
                self.period_ms
            );
            self.announce = false;
        }
        let next = self.last_ms.saturating_add(self.period_ms);
        let start = crate::time::uptime_millis();
        if start >= next {
            // Already at or past the predicted boundary: the ideal moment.
            return;
        }
        let budget_ms = core::cmp::max(1, self.period_ms / 4);
        if next - start > budget_ms {
            // Too far from the boundary to be worth stalling the desktop.
            return;
        }
        while crate::time::uptime_millis() < next {
            core::hint::spin_loop();
        }
    }
}

static PACER: Mutex<FramePacer> = Mutex::new(FramePacer::new());

/// Copy damage from the back buffer into the hardware framebuffer.
///
/// This is the only point at which the display changes. Everything else draws
/// into the back buffer, so a window update is published in one pass instead of
/// streaming row by row into live scanout.
pub fn present(damage: &[(usize, usize, usize, usize)]) {
    if !is_double_buffered() {
        return;
    }
    let (plan, back_base) = {
        let mut db = DOUBLE_BUFFER.lock();
        let Some(db) = db.as_mut() else { return };
        if !db.enabled {
            return;
        }
        let info = db.info;
        let Some(plan) = damage_copy_plan(damage, info.width, info.height, info.stride, info.bytes_per_pixel)
        else {
            return;
        };
        (plan, back_buffer_base(db))
    };

    // Pace before taking the framebuffer lock so the wait cannot serialise
    // against any other drawing.
    PACER.lock().wait(crate::time::uptime_millis());

    crate::drivers::fb::with_lock(|st| {
        let hw_len = st.byte_len;
        for span in &plan {
            let n = span.end.saturating_sub(span.start);
            if n == 0 || span.end > hw_len {
                // Skip this row rather than abandoning the rest of the rect.
                continue;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (back_base as *const u8).add(span.start),
                    (st.base as *mut u8).add(span.start),
                    n,
                );
            }
        }
    });
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
    let mut db = DOUBLE_BUFFER.lock();
    match db.as_mut() {
        Some(db) if db.enabled => Some((db.back.as_mut_ptr() as usize, db.back.len())),
        _ => None,
    }
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
/// Returns the pixel data (w * h * 4 bytes).
///
/// Reads the hardware framebuffer, which is the presented image: the back
/// buffer may hold a newer frame that has not been shown yet.
pub fn save_pixels(x: usize, y: usize, w: usize, h: usize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(w * h * 4);
    crate::drivers::fb::with_lock(|st| {
        st.save_px_rect(x, y, w, h, &mut out);
    });
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

/// Split a 0xRRGGBB color into components.
#[inline]
pub fn split_rgb(c: u32) -> (u8, u8, u8) {
    (((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8)
}

/// Blend `fg` over `bg` with 0..255 alpha (integer only, no float).
#[inline]
pub fn blend_u32(fg: u32, bg: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let ia = 255 - a;
    let r = (((fg >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
    let g = (((fg >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
    let b = ((fg & 0xFF) * a + (bg & 0xFF) * ia) / 255;
    (r << 16) | (g << 8) | b
}

/// Row-by-row filled rounded rect. `radius` is clamped to half size.
/// Cost is O(h) rect fills, safe for titlebars/buttons/windows.
pub fn fill_rounded_rect_px(x: usize, y: usize, w: usize, h: usize, radius: usize, color: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let (r, g, b) = split_rgb(color);
    let rad = radius.min(w / 2).min(h / 2);
    if rad == 0 {
        fill_rect_px(x, y, w, h, r, g, b);
        return;
    }
    // Middle band (full width).
    if h > rad * 2 {
        fill_rect_px(x, y + rad, w, h - rad * 2, r, g, b);
    }
    // Top and bottom bands with per-row corner insets (circle test).
    for row in 0..rad {
        // dy from circle center: rad-1-row .. rad-1 ; use integer circle.
        let dy = (rad as i32 - 1 - row as i32).abs() as usize;
        // dx = rad - floor(sqrt(rad^2 - dy^2)); isqrt via integer loop (rad<=16 typical).
        let mut inset = rad;
        let rad2 = rad * rad;
        let dy2 = dy * dy;
        if rad2 > dy2 {
            let mut dx: usize = 0;
            while dx * dx < rad2 - dy2 {
                dx += 1;
            }
            // dx = ceil(sqrt(...)); inset = rad - dx
            inset = rad.saturating_sub(dx);
        }
        let rw = w.saturating_sub(inset * 2);
        let rx = x.saturating_add(inset);
        if rw > 0 {
            fill_rect_px(rx, y + row, rw, 1, r, g, b);
            if h > row {
                fill_rect_px(rx, y + h - 1 - row, rw, 1, r, g, b);
            }
        }
    }
    // Fill the side strips between the corner rows and middle band edges.
    // Top strip rows [0..rad] already painted; rows [rad..h-rad] painted.
    // Left/right full-height edges inside corners:
    if w > 0 {
        // Left + right columns for the corner band height already covered by
        // the inset rows above; nothing extra needed. Middle band covers rest.
    }
}

/// Vertical gradient fill (top -> bottom), integer lerp per row.
pub fn fill_gradient_px(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    top: u32,
    bottom: u32,
    radius: usize,
) {
    if w == 0 || h == 0 {
        return;
    }
    let (tr, tg, tb) = split_rgb(top);
    let (br, bg_, bb) = split_rgb(bottom);
    let rad = radius.min(w / 2).min(h / 2);
    for row in 0..h {
        let t = if h <= 1 { 0 } else { (row * 255 / (h - 1)) as u32 };
        let it = 255 - t;
        let r = ((tr as u32 * it + br as u32 * t) / 255) as u8;
        let g = ((tg as u32 * it + bg_ as u32 * t) / 255) as u8;
        let b = ((tb as u32 * it + bb as u32 * t) / 255) as u8;
        if rad == 0 {
            fill_rect_px(x, y + row, w, 1, r, g, b);
        } else if row < rad || row >= h - rad {
            let band_row = if row < rad { row } else { h - 1 - row };
            let dy = (rad as i32 - 1 - band_row as i32).abs() as usize;
            let mut inset = rad;
            let rad2 = rad * rad;
            let dy2 = dy * dy;
            if rad2 > dy2 {
                let mut dx: usize = 0;
                while dx * dx < rad2 - dy2 {
                    dx += 1;
                }
                inset = rad.saturating_sub(dx);
            }
            let rw = w.saturating_sub(inset * 2);
            if rw > 0 {
                fill_rect_px(x + inset, y + row, rw, 1, r, g, b);
            }
        } else {
            fill_rect_px(x, y + row, w, 1, r, g, b);
        }
    }
}

/// Soft drop shadow: two stacked rounded layers under the window.
/// Outer = faint large halo, inner = tighter dark edge. Both clipped.
pub fn shadow_rounded_px(x: usize, y: usize, w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 {
        return;
    }
    // Offset down/right like a light source from top-left.
    let ox = 0usize;
    let oy = 4usize;
    let blur = 8usize;
    // Outer halo.
    let (hx, hy, hw, hh) = (
        x.saturating_add(ox).saturating_sub(blur / 2),
        y.saturating_add(oy).saturating_sub(blur / 2),
        w.saturating_add(blur),
        h.saturating_add(blur),
    );
    fill_rounded_rect_px(hx, hy, hw, hh, radius + blur / 2, 0x05070A);
    // Inner tight shadow hugging the window.
    fill_rounded_rect_px(
        x.saturating_add(ox),
        y.saturating_add(oy),
        w,
        h,
        radius,
        0x0A0E13,
    );
}

/// 1px glass highlight along the top-inner edge of a rounded rect.
pub fn glass_top_line_px(x: usize, y: usize, w: usize, radius: usize, color: u32) {
    if w == 0 {
        return;
    }
    let (r, g, b) = split_rgb(color);
    let inset = radius.min(w / 2);
    let rw = w.saturating_sub(inset * 2);
    if rw > 0 {
        fill_rect_px(x + inset, y, rw, 1, r, g, b);
    }
}


#[cfg(test)]
mod tests {
    use super::{damage_copy_plan, RowSpan};

    /// An 8x4 screen at 4 bytes per pixel. Note `stride` is in *pixels* (the
    /// bootloader reports it that way and the whole module scales by bpp), so
    /// `STRIDE == W` here and the total buffer is `STRIDE * H * BPP` bytes.
    const W: usize = 8;
    const H: usize = 4;
    const BPP: usize = 4;
    const STRIDE: usize = W;
    const TOTAL_BYTES: usize = STRIDE * H * BPP;

    fn plan(damage: &[(usize, usize, usize, usize)]) -> Vec<RowSpan> {
        damage_copy_plan(damage, W, H, STRIDE, BPP).expect("expected a copy plan")
    }

    #[test]
    fn single_rect_covers_exactly_its_rows() {
        let spans = plan(&[(1, 1, 2, 2)]);
        assert_eq!(
            spans,
            alloc::vec![
                RowSpan { y: 1, start: (1 * STRIDE + 1) * BPP, end: (1 * STRIDE + 3) * BPP },
                RowSpan { y: 2, start: (2 * STRIDE + 1) * BPP, end: (2 * STRIDE + 3) * BPP },
            ]
        );
    }

    #[test]
    fn every_row_copies_exactly_the_requested_pixels() {
        let (x, y, w, h) = (2, 1, 3, 2);
        let spans = plan(&[(x, y, w, h)]);
        assert_eq!(spans.len(), h);
        for span in spans {
            let row = span.y - y;
            assert_eq!(span.start, ((y + row) * STRIDE + x) * BPP);
            assert_eq!(span.end, ((y + row) * STRIDE + x + w) * BPP);
            // 3 pixels * 4 bytes.
            assert_eq!(span.end - span.start, w * BPP);
        }
    }

    #[test]
    fn rect_clipped_at_right_and_bottom_edges() {
        // Runs off the right and bottom edges; must be truncated, not dropped,
        // and the partial rows must still be produced.
        let spans = plan(&[(6, 2, 10, 10)]);
        assert_eq!(spans.len(), H - 2, "rows past the bottom must be dropped");
        for span in spans {
            assert_eq!(span.start, (span.y * STRIDE + 6) * BPP);
            assert_eq!(span.end, (span.y * STRIDE + W) * BPP);
            assert_eq!(span.end - span.start, 2 * BPP);
        }
    }

    #[test]
    fn stride_padding_is_honoured() {
        // Wider stride than width: each row must start at py * padded_stride.
        let padded = STRIDE + 4;
        let spans = damage_copy_plan(&[(0, 2, 1, 1)], W, H, padded, BPP).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 2 * padded * BPP);
        assert_eq!(spans[0].end, (2 * padded + 1) * BPP);
    }

    #[test]
    fn zero_sized_and_offscreen_rects_are_ignored() {
        assert!(damage_copy_plan(&[(0, 0, 0, 5)], W, H, STRIDE, BPP).is_none());
        assert!(damage_copy_plan(&[(0, 0, 5, 0)], W, H, STRIDE, BPP).is_none());
        assert!(damage_copy_plan(&[], W, H, STRIDE, BPP).is_none());
        // Fully below / fully right of the screen: nothing to copy.
        assert!(damage_copy_plan(&[(0, H, 2, 2)], W, H, STRIDE, BPP).is_none());
        assert!(damage_copy_plan(&[(W, 0, 2, 2)], W, H, STRIDE, BPP).is_none());
    }

    #[test]
    fn multiple_rects_produce_one_group_of_rows_each() {
        let spans = plan(&[(0, 0, 1, 1), (0, 3, 1, 1)]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].y, 0);
        assert_eq!(spans[1].y, 3);
    }

    #[test]
    fn spans_never_exceed_the_framebuffer() {
        // Full-screen damage must stay inside the buffer.
        for span in plan(&[(0, 0, W, H)]) {
            assert!(
                span.end <= TOTAL_BYTES,
                "span {:?} escapes {} bytes",
                span,
                TOTAL_BYTES
            );
        }
        assert_eq!(plan(&[(0, 0, W, H)]).len(), H);
    }
}
