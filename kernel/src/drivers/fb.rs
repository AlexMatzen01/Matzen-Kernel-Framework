//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! UEFI GOP framebuffer text console.
//!
//! On UEFI the bootloader provides a pixel framebuffer (GOP) instead of
//! VGA text mode at 0xB8000. Writing to 0xB8000 is then invisible (and the
//! display keeps showing the bootloader log / goes black after the kernel
//! takes over). This driver renders the same 80x25-style text API the rest
//! of the kernel uses directly into the GOP framebuffer, so `println!`
//! works on UEFI the way it does on BIOS + serial.
//!
//! Design notes:
//! - `no_std`, no `alloc`: state is a static `Mutex<FbState>` holding a raw
//!   pointer + `FrameBufferInfo`. The bootloader mapping lives forever, so
//!   leaking the `&'static mut FrameBuffer` borrow is intentional.
//! - `buffer_start` from `bootloader_api` is already a *virtual* address
//!   with the framebuffer mapped; no physical-memory offset math needed.
//! - 8x8 font glyphs scaled vertically x2 into 8x16 cells.
//! - Streaming writes (`write_byte`) are top-anchored like a normal
//!   terminal: text starts at row 0 and advances downward; the view scrolls
//!   up by one row only when the cursor moves past the last row.
//!   (Deliberately different from `vga::Writer`, which is bottom-anchored.)
//! - Absolute helpers (`write_at`, `fill_rect`, ...) map the 80x25 callers
//!   onto the top-left of whatever mode GOP picked (e.g. 1280x800 ->
//!   160x50 cells); callers keep working unchanged.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo as _, PixelFormat as _};
use core::fmt;

pub type FrameBufferInfo = bootloader_api::info::FrameBufferInfo;
pub type PixelFormat = bootloader_api::info::PixelFormat;
use spin::Mutex;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::vga::Color;

/// Glyph cell size in pixels.
const CELL_W: usize = 8;
const CELL_H: usize = 16;

/// Fallback text grid when no framebuffer is present (callers clip anyway).
const FALLBACK_COLS: usize = 80;
const FALLBACK_ROWS: usize = 25;

/// 8x8 bitmap font (public domain, Daniel Hepper "font8x8_basic",
/// after the public domain VGA fonts).
/// Bit 0 of each byte is the LEFTMOST pixel; the renderer tests
/// `(bits >> gx) & 1`. Glyphs are doubled vertically into 8x16 cells.
#[rustfmt::skip]
const FONT: [[u8; 8]; 128] = [
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x00
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x01
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x02
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x03
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x04
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x05
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x06
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x07
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x08
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x09
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0a
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0b
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0c
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0d
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0e
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x0f
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x10
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x11
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x12
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x13
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x14
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x15
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x16
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x17
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x18
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x19
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1a
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1b
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1c
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1d
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1e
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x1f
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x20 space
    [0x18,0x3C,0x3C,0x18,0x18,0x00,0x18,0x00], // 0x21 !
    [0x36,0x36,0x00,0x00,0x00,0x00,0x00,0x00], // 0x22 "
    [0x36,0x36,0x7F,0x36,0x7F,0x36,0x36,0x00], // 0x23 #
    [0x0C,0x3E,0x03,0x1E,0x30,0x1F,0x0C,0x00], // 0x24 $
    [0x00,0x63,0x33,0x18,0x0C,0x66,0x63,0x00], // 0x25 %
    [0x1C,0x36,0x1C,0x6E,0x3B,0x33,0x6E,0x00], // 0x26 &
    [0x06,0x06,0x03,0x00,0x00,0x00,0x00,0x00], // 0x27 '
    [0x18,0x0C,0x06,0x06,0x06,0x0C,0x18,0x00], // 0x28 (
    [0x06,0x0C,0x18,0x18,0x18,0x0C,0x06,0x00], // 0x29 )
    [0x00,0x66,0x3C,0xFF,0x3C,0x66,0x00,0x00], // 0x2a *
    [0x00,0x0C,0x0C,0x3F,0x0C,0x0C,0x00,0x00], // 0x2b +
    [0x00,0x00,0x00,0x00,0x00,0x0C,0x0C,0x06], // 0x2c ,
    [0x00,0x00,0x00,0x3F,0x00,0x00,0x00,0x00], // 0x2d -
    [0x00,0x00,0x00,0x00,0x00,0x0C,0x0C,0x00], // 0x2e .
    [0x60,0x30,0x18,0x0C,0x06,0x03,0x01,0x00], // 0x2f /
    [0x3E,0x63,0x73,0x7B,0x6F,0x67,0x3E,0x00], // 0x30 0
    [0x0C,0x0E,0x0C,0x0C,0x0C,0x0C,0x3F,0x00], // 0x31 1
    [0x1E,0x33,0x30,0x1C,0x06,0x33,0x3F,0x00], // 0x32 2
    [0x1E,0x33,0x30,0x1C,0x30,0x33,0x1E,0x00], // 0x33 3
    [0x38,0x3C,0x36,0x33,0x7F,0x30,0x78,0x00], // 0x34 4
    [0x3F,0x03,0x1F,0x30,0x30,0x33,0x1E,0x00], // 0x35 5
    [0x1C,0x06,0x03,0x1F,0x33,0x33,0x1E,0x00], // 0x36 6
    [0x3F,0x33,0x30,0x18,0x0C,0x0C,0x0C,0x00], // 0x37 7
    [0x1E,0x33,0x33,0x1E,0x33,0x33,0x1E,0x00], // 0x38 8
    [0x1E,0x33,0x33,0x3E,0x30,0x18,0x0E,0x00], // 0x39 9
    [0x00,0x0C,0x0C,0x00,0x00,0x0C,0x0C,0x00], // 0x3a :
    [0x00,0x0C,0x0C,0x00,0x00,0x0C,0x0C,0x06], // 0x3b ;
    [0x18,0x0C,0x06,0x03,0x06,0x0C,0x18,0x00], // 0x3c <
    [0x00,0x00,0x3F,0x00,0x00,0x3F,0x00,0x00], // 0x3d =
    [0x06,0x0C,0x18,0x30,0x18,0x0C,0x06,0x00], // 0x3e >
    [0x1E,0x33,0x30,0x18,0x0C,0x00,0x0C,0x00], // 0x3f ?
    [0x3E,0x63,0x7B,0x7B,0x7B,0x03,0x1E,0x00], // 0x40 @
    [0x0C,0x1E,0x33,0x33,0x3F,0x33,0x33,0x00], // 0x41 A
    [0x3F,0x66,0x66,0x3E,0x66,0x66,0x3F,0x00], // 0x42 B
    [0x3C,0x66,0x03,0x03,0x03,0x66,0x3C,0x00], // 0x43 C
    [0x1F,0x36,0x66,0x66,0x66,0x36,0x1F,0x00], // 0x44 D
    [0x7F,0x46,0x16,0x1E,0x16,0x46,0x7F,0x00], // 0x45 E
    [0x7F,0x46,0x16,0x1E,0x16,0x06,0x0F,0x00], // 0x46 F
    [0x3C,0x66,0x03,0x03,0x73,0x66,0x7C,0x00], // 0x47 G
    [0x33,0x33,0x33,0x3F,0x33,0x33,0x33,0x00], // 0x48 H
    [0x1E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0x00], // 0x49 I
    [0x78,0x30,0x30,0x30,0x33,0x33,0x1E,0x00], // 0x4a J
    [0x67,0x66,0x36,0x1E,0x36,0x66,0x67,0x00], // 0x4b K
    [0x0F,0x06,0x06,0x06,0x46,0x66,0x7F,0x00], // 0x4c L
    [0x63,0x77,0x7F,0x7F,0x6B,0x63,0x63,0x00], // 0x4d M
    [0x63,0x67,0x6F,0x7B,0x73,0x63,0x63,0x00], // 0x4e N
    [0x1C,0x36,0x63,0x63,0x63,0x36,0x1C,0x00], // 0x4f O
    [0x3F,0x66,0x66,0x3E,0x06,0x06,0x0F,0x00], // 0x50 P
    [0x1E,0x33,0x33,0x33,0x3B,0x1E,0x38,0x00], // 0x51 Q
    [0x3F,0x66,0x66,0x3E,0x36,0x66,0x67,0x00], // 0x52 R
    [0x1E,0x33,0x07,0x0E,0x38,0x33,0x1E,0x00], // 0x53 S
    [0x3F,0x2D,0x0C,0x0C,0x0C,0x0C,0x1E,0x00], // 0x54 T
    [0x33,0x33,0x33,0x33,0x33,0x33,0x3F,0x00], // 0x55 U
    [0x33,0x33,0x33,0x33,0x33,0x1E,0x0C,0x00], // 0x56 V
    [0x63,0x63,0x63,0x6B,0x7F,0x77,0x63,0x00], // 0x57 W
    [0x63,0x63,0x36,0x1C,0x1C,0x36,0x63,0x00], // 0x58 X
    [0x33,0x33,0x33,0x1E,0x0C,0x0C,0x1E,0x00], // 0x59 Y
    [0x7F,0x63,0x31,0x18,0x4C,0x66,0x7F,0x00], // 0x5a Z
    [0x1E,0x06,0x06,0x06,0x06,0x06,0x1E,0x00], // 0x5b [
    [0x03,0x06,0x0C,0x18,0x30,0x60,0x40,0x00], // 0x5c backslash
    [0x1E,0x18,0x18,0x18,0x18,0x18,0x1E,0x00], // 0x5d ]
    [0x08,0x1C,0x36,0x63,0x00,0x00,0x00,0x00], // 0x5e ^
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0xFF], // 0x5f _
    [0x0C,0x0C,0x18,0x00,0x00,0x00,0x00,0x00], // 0x60 `
    [0x00,0x00,0x1E,0x30,0x3E,0x33,0x6E,0x00], // 0x61 a
    [0x07,0x06,0x06,0x3E,0x66,0x66,0x3B,0x00], // 0x62 b
    [0x00,0x00,0x1E,0x33,0x03,0x33,0x1E,0x00], // 0x63 c
    [0x38,0x30,0x30,0x3E,0x33,0x33,0x6E,0x00], // 0x64 d
    [0x00,0x00,0x1E,0x33,0x3F,0x03,0x1E,0x00], // 0x65 e
    [0x1C,0x36,0x06,0x0F,0x06,0x06,0x0F,0x00], // 0x66 f
    [0x00,0x00,0x6E,0x33,0x33,0x3E,0x30,0x1F], // 0x67 g
    [0x07,0x06,0x36,0x6E,0x66,0x66,0x67,0x00], // 0x68 h
    [0x0C,0x00,0x0E,0x0C,0x0C,0x0C,0x1E,0x00], // 0x69 i
    [0x30,0x00,0x30,0x30,0x30,0x33,0x33,0x1E], // 0x6a j
    [0x07,0x06,0x66,0x36,0x1E,0x36,0x67,0x00], // 0x6b k
    [0x0E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0x00], // 0x6c l
    [0x00,0x00,0x33,0x7F,0x7F,0x6B,0x63,0x00], // 0x6d m
    [0x00,0x00,0x1F,0x33,0x33,0x33,0x33,0x00], // 0x6e n
    [0x00,0x00,0x1E,0x33,0x33,0x33,0x1E,0x00], // 0x6f o
    [0x00,0x00,0x3B,0x66,0x66,0x3E,0x06,0x0F], // 0x70 p
    [0x00,0x00,0x6E,0x33,0x33,0x3E,0x30,0x78], // 0x71 q
    [0x00,0x00,0x3B,0x6E,0x66,0x06,0x0F,0x00], // 0x72 r
    [0x00,0x00,0x3E,0x03,0x1E,0x30,0x1F,0x00], // 0x73 s
    [0x08,0x0C,0x3E,0x0C,0x0C,0x2C,0x18,0x00], // 0x74 t
    [0x00,0x00,0x33,0x33,0x33,0x33,0x6E,0x00], // 0x75 u
    [0x00,0x00,0x33,0x33,0x33,0x1E,0x0C,0x00], // 0x76 v
    [0x00,0x00,0x63,0x6B,0x7F,0x7F,0x36,0x00], // 0x77 w
    [0x00,0x00,0x63,0x36,0x1C,0x36,0x63,0x00], // 0x78 x
    [0x00,0x00,0x33,0x33,0x33,0x3E,0x30,0x1F], // 0x79 y
    [0x00,0x00,0x3F,0x19,0x0C,0x26,0x3F,0x00], // 0x7a z
    [0x38,0x0C,0x0C,0x07,0x0C,0x0C,0x38,0x00], // 0x7b {
    [0x18,0x18,0x18,0x00,0x18,0x18,0x18,0x00], // 0x7c |
    [0x07,0x0C,0x0C,0x38,0x0C,0x0C,0x07,0x00], // 0x7d }
    [0x6E,0x3B,0x00,0x00,0x00,0x00,0x00,0x00], // 0x7e ~
    [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00], // 0x7f
];

/// Classic 16-color VGA palette as 24-bit RGB.
fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Black => (0, 0, 0),
        Color::Blue => (0, 0, 170),
        Color::Green => (0, 170, 0),
        Color::Cyan => (0, 170, 170),
        Color::Red => (170, 0, 0),
        Color::Magenta => (170, 0, 170),
        Color::Brown => (170, 85, 0),
        Color::LightGray => (170, 170, 170),
        Color::DarkGray => (85, 85, 85),
        Color::LightBlue => (85, 85, 255),
        Color::LightGreen => (85, 255, 85),
        Color::LightCyan => (85, 255, 255),
        Color::LightRed => (255, 85, 85),
        Color::Pink => (255, 85, 255),
        Color::Yellow => (255, 255, 85),
        Color::White => (255, 255, 255),
    }
}

pub(crate) struct FbState {
    pub(crate) base: usize,
    pub(crate) byte_len: usize,
    pub(crate) info: Option<FrameBufferInfo>,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) col: usize,
    pub(crate) row: usize,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
}

impl FbState {
    const fn new() -> Self {
        Self {
            base: 0,
            byte_len: 0,
            info: None,
            cols: FALLBACK_COLS,
            rows: FALLBACK_ROWS,
            col: 0,
            row: 0,
            fg: Color::LightGreen,
            bg: Color::Black,
        }
    }

    fn is_active(&self) -> bool {
        self.info.is_some() && self.base != 0
    }

    fn attach(&mut self, base: usize, byte_len: usize, info: FrameBufferInfo) {
        self.base = base;
        self.byte_len = byte_len;
        self.cols = (info.width / CELL_W).max(1);
        self.rows = (info.height / CELL_H).max(1);
        self.col = 0;
        self.row = 0;
        self.info = Some(info);
    }

    #[inline]
    fn pixel_offset(&self, x: usize, y: usize) -> Option<usize> {
        let info = self.info?;
        if x >= info.width || y >= info.height {
            return None;
        }
        let off = (y * info.stride + x) * info.bytes_per_pixel;
        if off + info.bytes_per_pixel.min(4) > self.byte_len {
            return None;
        }
        Some(off)
    }

    fn put_pixel(&mut self, x: usize, y: usize, rgb: (u8, u8, u8)) {
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let off = match self.pixel_offset(x, y) {
            Some(o) => o,
            None => return,
        };
        unsafe {
            let fb = self.base as *mut u8;
            let p = fb.add(off);
            match info.pixel_format {
                PixelFormat::Rgb => {
                    core::ptr::write_volatile(p, rgb.0);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), rgb.1);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), rgb.2);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
                PixelFormat::Bgr => {
                    core::ptr::write_volatile(p, rgb.2);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), rgb.1);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), rgb.0);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
                PixelFormat::U8 => {
                    // Luminance approximation.
                    let lum =
                        ((rgb.0 as u16 * 30 + rgb.1 as u16 * 59 + rgb.2 as u16 * 11) / 100) as u8;
                    core::ptr::write_volatile(p, lum);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), lum);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), lum);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
                _ => {
                    // Unknown layout: assume BGRx like most UEFI GOP modes.
                    core::ptr::write_volatile(p, rgb.2);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), rgb.1);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), rgb.0);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
            }
        }
    }

    fn fill_cell(&mut self, col: usize, row: usize, fg: Color, bg: Color, byte: u8) {
        if !self.is_active() {
            return;
        }
        let (fr, fgg, fb_) = color_to_rgb(fg);
        let (br, bg_, bb) = color_to_rgb(bg);
        let glyph = FONT[(byte as usize).min(127)];
        let x0 = col * CELL_W;
        let y0 = row * CELL_H;
        for gy in 0..8 {
            let bits = glyph[gy];
            for gx in 0..8 {
                // Bit 0 = leftmost pixel (font8x8_basic convention).
                let on = (bits >> gx) & 1 == 1;
                let c = if on { (fr, fgg, fb_) } else { (br, bg_, bb) };
                // Vertical 2x scale: each font row -> two pixel rows.
                self.put_pixel(x0 + gx, y0 + gy * 2, c);
                self.put_pixel(x0 + gx, y0 + gy * 2 + 1, c);
            }
        }
    }

    fn fill_cell_bg(&mut self, col: usize, row: usize, bg: Color) {
        if !self.is_active() {
            return;
        }
        let c = color_to_rgb(bg);
        let x0 = col * CELL_W;
        let y0 = row * CELL_H;
        for y in 0..CELL_H {
            for x in 0..CELL_W {
                self.put_pixel(x0 + x, y0 + y, c);
            }
        }
    }

    fn scroll_up_one_row(&mut self) {
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        if !self.is_active() || self.rows == 0 {
            return;
        }
        let bpp = info.bytes_per_pixel;
        let line_bytes = info.stride * bpp;
        let move_rows_px = info.height.saturating_sub(CELL_H);
        unsafe {
            let fb = self.base as *mut u8;
            // Move pixel rows [CELL_H..height) to [0..height-CELL_H).
            core::ptr::copy(fb.add(CELL_H * line_bytes), fb, move_rows_px * line_bytes);
        }
        // Clear the freed bottom text row.
        let last = self.rows.saturating_sub(1);
        for c in 0..self.cols {
            self.fill_cell_bg(c, last, self.bg);
        }
    }

    /// Top-anchored newline: move the cursor down, scrolling only when the
    /// cursor would move past the last row.
    fn new_line(&mut self) {
        if self.row + 1 >= self.rows.max(1) {
            self.scroll_up_one_row();
            self.row = self.rows.saturating_sub(1);
        } else {
            self.row += 1;
        }
        self.col = 0;
    }

    fn write_byte(&mut self, byte: u8) {
        if !self.is_active() {
            return;
        }
        match byte {
            b'\n' => self.new_line(),
            b'\x08' => {
                if self.col > 0 {
                    self.col -= 1;
                    let (row, col, bg) = (self.row, self.col, self.bg);
                    self.fill_cell_bg(col, row, bg);
                }
            }
            b'\r' => self.col = 0,
            byte => {
                if self.col >= self.cols {
                    self.new_line();
                }
                let (row, col, fg, bg) = (self.row, self.col, self.fg, self.bg);
                self.fill_cell(col, row, fg, bg, byte);
                self.col += 1;
            }
        }
    }

    fn write_string(&mut self, s: &str) {
        for b in s.bytes() {
            match b {
                0x20..=0x7e | b'\n' | b'\r' | b'\x08' => self.write_byte(b),
                b'\t' => {
                    // Tab -> advance to next 8-col stop.
                    let next = (self.col + 8) & !7;
                    while self.col < next.min(self.cols) {
                        let (row, col, fg, bg) = (self.row, self.col, self.fg, self.bg);
                        self.fill_cell(col, row, fg, bg, b' ');
                        self.col += 1;
                    }
                    if self.col >= self.cols {
                        self.new_line();
                    }
                }
                _ => self.write_byte(b' '),
            }
        }
        // Mirror to capture buffer if active (for desktop command output)
        crate::drivers::fb_gfx::capture_write_str(s);
    }

    fn clear_screen(&mut self) {
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let (br, bg, bb) = color_to_rgb(self.bg);
        // Fast path for the common packed formats: fill raw bytes.
        let solid = match info.pixel_format {
            PixelFormat::Rgb => [br, bg, bb, 0],
            _ => [bb, bg, br, 0], // Bgr / U8(approx) / Unknown
        };
        unsafe {
            let fb = self.base as *mut u8;
            let bpp = info.bytes_per_pixel;
            if bpp == 4 {
                let px = u32::from_le_bytes(solid);
                let count = self.byte_len / 4;
                let dst = fb as *mut u32;
                for i in 0..count {
                    core::ptr::write_volatile(dst.add(i), px);
                }
            } else {
                for y in 0..info.height {
                    for x in 0..info.width {
                        if let Some(off) = self.pixel_offset(x, y) {
                            let p = fb.add(off);
                            for i in 0..bpp.min(4) {
                                core::ptr::write_volatile(p.add(i), solid[i]);
                            }
                        }
                    }
                }
            }
        }
        self.col = 0;
        self.row = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let (row, col, bg) = (self.row, self.col, self.bg);
            self.fill_cell_bg(col, row, bg);
        }
    }

    // ──────────────────────────────────────────────
    // Pixel-level graphics primitives (for desktop/gfx)
    // ──────────────────────────────────────────────

    /// Fill a pixel rectangle with RGB color.
    pub(crate) fn fill_px_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        r: u8,
        g: u8,
        b: u8,
    ) {
        if !self.is_active() {
            return;
        }
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let xs = x.min(info.width);
        let x_end = (x + w).min(info.width);
        let y_end = (y + h).min(info.height);
        // Fast path: packed 4bpp rows as single u32 stores (no per-pixel
        // format match, no offset math). Covers all QEMU/GOP modes.
        if info.bytes_per_pixel == 4 {
            let px: u32 = match info.pixel_format {
                PixelFormat::Rgb => u32::from_le_bytes([r, g, b, 0]),
                _ => u32::from_le_bytes([b, g, r, 0]),
            };
            unsafe {
                let fb = self.base as *mut u32;
                for py in y.min(info.height)..y_end {
                    let row = fb.add(py * info.stride + xs);
                    for i in 0..(x_end - xs) {
                        core::ptr::write_volatile(row.add(i), px);
                    }
                }
            }
            return;
        }
        for py in y.min(info.height)..y_end {
            for px in xs..x_end {
                if let Some(off) = self.pixel_offset(px, py) {
                    self.put_pixel_rgb(off, r, g, b);
                }
            }
        }
    }

    /// Draw a hollow rectangle outline in RGB color.
    pub(crate) fn rect_px(&mut self, x: usize, y: usize, w: usize, h: usize, r: u8, g: u8, b: u8) {
        if !self.is_active() || w == 0 || h == 0 {
            return;
        }
        // Top and bottom edges
        self.fill_px_rect(x, y, w, 1, r, g, b);
        if h > 1 {
            self.fill_px_rect(x, y + h - 1, w, 1, r, g, b);
        }
        // Left and right edges
        if h > 2 {
            self.fill_px_rect(x, y + 1, 1, h - 2, r, g, b);
            if w > 1 {
                self.fill_px_rect(x + w - 1, y + 1, 1, h - 2, r, g, b);
            }
        }
    }

    /// Blit a 32-bit RGBA bitmap (row-major, w*4 bytes per row).
    pub(crate) fn blit_rgba(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        rgba: &[u8],
        source_stride: usize,
        source_x: usize,
        source_y: usize,
    ) {
        if !self.is_active() {
            return;
        }
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let stride_bytes = info.stride * info.bytes_per_pixel;
        let bpp = info.bytes_per_pixel;
        let x_end = (x + w).min(info.width);
        let y_end = (y + h).min(info.height);
        for py in 0..(y_end - y) {
            for px in 0..(x_end - x) {
                let src_idx = ((source_y + py) * source_stride + source_x + px) * 4;
                if src_idx + 3 >= rgba.len() {
                    continue;
                }
                let r = rgba[src_idx];
                let g = rgba[src_idx + 1];
                let b_ = rgba[src_idx + 2];
                let a = rgba[src_idx + 3];
                if a == 0 {
                    continue; // fully transparent
                }
                let dst_x = x + px;
                let dst_y = y + py;
                if let Some(off) = self.pixel_offset(dst_x, dst_y) {
                    unsafe {
                        let fb = self.base as *mut u8;
                        let p = fb.add(off);
                        if a == 255 {
                            // Opaque: direct write
                            match info.pixel_format {
                                PixelFormat::Rgb => {
                                    core::ptr::write_volatile(p, r);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), g);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), b_);
                                    }
                                }
                                _ => {
                                    core::ptr::write_volatile(p, b_);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), g);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), r);
                                    }
                                }
                            }
                        } else {
                            // Alpha blend
                            let dst_r = match info.pixel_format {
                                PixelFormat::Rgb => *p,
                                _ => *p.add(2),
                            };
                            let dst_g = if bpp > 1 {
                                match info.pixel_format {
                                    PixelFormat::Rgb => *p.add(1),
                                    _ => *p.add(1),
                                }
                            } else {
                                0
                            };
                            let dst_b = if bpp > 2 {
                                match info.pixel_format {
                                    PixelFormat::Rgb => *p.add(2),
                                    _ => *p,
                                }
                            } else {
                                0
                            };
                            let inv_a = 255 - a as u16;
                            let blend = |src: u8, dst: u8| -> u8 {
                                ((src as u16 * a as u16 + dst as u16 * inv_a) >> 8) as u8
                            };
                            let nr = blend(r, dst_r);
                            let ng = blend(g, dst_g);
                            let nb = blend(b_, dst_b);
                            match info.pixel_format {
                                PixelFormat::Rgb => {
                                    core::ptr::write_volatile(p, nr);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), ng);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), nb);
                                    }
                                }
                                _ => {
                                    core::ptr::write_volatile(p, nb);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), ng);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), nr);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Save a pixel rectangle to an RGBA buffer (row-major, 4 bytes/pixel).
    pub(crate) fn save_px_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut alloc::vec::Vec<u8>,
    ) {
        if !self.is_active() {
            return;
        }
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let x_end = (x + w).min(info.width);
        let y_end = (y + h).min(info.height);
        out.clear();
        out.reserve((x_end - x) * (y_end - y) * 4);
        for py in y..y_end {
            for px in x..x_end {
                if let Some(off) = self.pixel_offset(px, py) {
                    unsafe {
                        let fb = self.base as *const u8;
                        let p = fb.add(off);
                        let bpp = info.bytes_per_pixel;
                        let (r, g, b) = match info.pixel_format {
                            PixelFormat::Rgb => (*p, *p.add(1), *p.add(2)),
                            _ => (*p.add(2), *p.add(1), *p),
                        };
                        out.push(r);
                        out.push(g);
                        out.push(b);
                        out.push(255);
                    }
                }
            }
        }
    }

    /// Restore a pixel rectangle from RGBA data.
    pub(crate) fn restore_px_rect(&mut self, x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
        if !self.is_active() {
            return;
        }
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        let bpp = info.bytes_per_pixel;
        let x_end = (x + w).min(info.width);
        let y_end = (y + h).min(info.height);
        for py in 0..(y_end - y) {
            for px in 0..(x_end - x) {
                let src_idx = (py * w + px) * 4;
                if src_idx + 3 >= data.len() {
                    continue;
                }
                let r = data[src_idx];
                let g = data[src_idx + 1];
                let b_ = data[src_idx + 2];
                let a = data[src_idx + 3];
                if a == 0 {
                    continue;
                }
                let dst_x = x + px;
                let dst_y = y + py;
                if let Some(off) = self.pixel_offset(dst_x, dst_y) {
                    unsafe {
                        let fb = self.base as *mut u8;
                        let p = fb.add(off);
                        if a == 255 {
                            match info.pixel_format {
                                PixelFormat::Rgb => {
                                    core::ptr::write_volatile(p, r);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), g);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), b_);
                                    }
                                }
                                _ => {
                                    core::ptr::write_volatile(p, b_);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), g);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), r);
                                    }
                                }
                            }
                        } else {
                            let dst_r = match info.pixel_format {
                                PixelFormat::Rgb => *p,
                                _ => *p.add(2),
                            };
                            let dst_g = if bpp > 1 {
                                match info.pixel_format {
                                    PixelFormat::Rgb => *p.add(1),
                                    _ => *p.add(1),
                                }
                            } else {
                                0
                            };
                            let dst_b = if bpp > 2 {
                                match info.pixel_format {
                                    PixelFormat::Rgb => *p.add(2),
                                    _ => *p,
                                }
                            } else {
                                0
                            };
                            let inv_a = 255 - a as u16;
                            let blend = |src: u8, dst: u8| -> u8 {
                                ((src as u16 * a as u16 + dst as u16 * inv_a) >> 8) as u8
                            };
                            let nr = blend(r, dst_r);
                            let ng = blend(g, dst_g);
                            let nb = blend(b_, dst_b);
                            match info.pixel_format {
                                PixelFormat::Rgb => {
                                    core::ptr::write_volatile(p, nr);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), ng);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), nb);
                                    }
                                }
                                _ => {
                                    core::ptr::write_volatile(p, nb);
                                    if bpp > 1 {
                                        core::ptr::write_volatile(p.add(1), ng);
                                    }
                                    if bpp > 2 {
                                        core::ptr::write_volatile(p.add(2), nr);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Write a single pixel at precomputed offset (no bounds check).
    pub(crate) fn put_pixel_rgb(&mut self, off: usize, r: u8, g: u8, b: u8) {
        let info = match self.info {
            Some(i) => i,
            None => return,
        };
        unsafe {
            let fb = self.base as *mut u8;
            let p = fb.add(off);
            match info.pixel_format {
                PixelFormat::Rgb => {
                    core::ptr::write_volatile(p, r);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), g);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), b);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
                _ => {
                    core::ptr::write_volatile(p, b);
                    if info.bytes_per_pixel > 1 {
                        core::ptr::write_volatile(p.add(1), g);
                    }
                    if info.bytes_per_pixel > 2 {
                        core::ptr::write_volatile(p.add(2), r);
                    }
                    if info.bytes_per_pixel > 3 {
                        core::ptr::write_volatile(p.add(3), 0);
                    }
                }
            }
        }
    }
}

static FB: Mutex<FbState> = Mutex::new(FbState::new());

/// Initialize the framebuffer console. Takes ownership of the bootloader
/// framebuffer; the mapping lives for the rest of the kernel's life.
pub fn init(mut fb: FrameBuffer) {
    let info = fb.info();
    let buf = fb.buffer_mut();
    let base = buf.as_mut_ptr() as usize;
    let len = buf.len();
    // Keep the mapping alive forever; we use the raw pointer from here on.
    core::mem::forget(fb);
    let mut s = FB.lock();
    s.attach(base, len, info);
    s.clear_screen();
}

/// True once a GOP framebuffer has been attached.
pub fn is_active() -> bool {
    FB.lock().is_active()
}

/// Pixel dimensions, if active.
pub fn pixel_size() -> Option<(usize, usize)> {
    let s = FB.lock();
    s.info.map(|i| (i.width, i.height))
}

/// Text grid dimensions, if active.
pub fn text_size() -> Option<(usize, usize)> {
    let s = FB.lock();
    if s.is_active() {
        Some((s.cols, s.rows))
    } else {
        None
    }
}

/// Get the framebuffer info, if available.
pub fn get_framebuffer_info() -> Option<FrameBufferInfo> {
    let s = FB.lock();
    s.info
}

pub(crate) fn with_lock<F, R>(f: F) -> R
where
    F: FnOnce(&mut FbState) -> R,
{
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| f(&mut FB.lock()))
}

/// Run `f` with the framebuffer state temporarily remapped to another
/// backing store (the double-buffer back/front buffers). Restores the
/// hardware base/len afterwards.
///
/// Lock discipline: takes ONLY the framebuffer lock. Callers must snapshot
/// the target base/len beforehand and must NOT hold any other driver lock
/// across this call — the old fb_gfx redirect helpers deadlocked by taking
/// the double-buffer lock outside and again inside.
pub(crate) fn with_mapped_base<F, R>(base: usize, len: usize, f: F) -> R
where
    F: FnOnce(&mut FbState) -> R,
{
    with_lock(|st| {
        let old_base = st.base;
        let old_len = st.byte_len;
        st.base = base;
        st.byte_len = len;
        let r = f(st);
        st.base = old_base;
        st.byte_len = old_len;
        r
    })
}

/// Streaming write used by the console dispatcher.
pub fn write_string(s: &str) {
    with_lock(|st| st.write_string(s));
}

/// Formatted write used by the console dispatcher (`fmt::Arguments`
/// cannot be turned into `&str` without allocating).
#[doc(hidden)]
pub fn write_fmt(args: fmt::Arguments) {
    use core::fmt::Write;
    with_lock(|st| {
        let _ = st.write_fmt(args);
    });
}

pub fn clear_screen() {
    with_lock(|st| st.clear_screen());
}

pub fn backspace() {
    with_lock(|st| st.backspace());
}

pub fn set_color(fg: Color, bg: Color) {
    with_lock(|st| {
        st.fg = fg;
        st.bg = bg;
    });
}

/// Absolute cell write for full-screen callers (editor/installer/shell).
pub fn write_at(row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
    with_lock(|st| {
        if !st.is_active() || row >= st.rows || col >= st.cols {
            return;
        }
        st.fill_cell(col, row, fg, bg, byte);
    });
}

/// Draw one 8x16 glyph (8x8 font, doubled vertically like the console)
/// with a transparent background: only set pixels are written, clipped to
/// the framebuffer and to `clip` (x, y, w, h) when given.
fn draw_glyph_px(
    st: &mut FbState,
    x0: usize,
    y0: usize,
    byte: u8,
    rgb: (u8, u8, u8),
    clip: Option<(usize, usize, usize, usize)>,
) {
    if !st.is_active() {
        return;
    }
    let glyph = FONT[(byte as usize).min(127)];
    for gy in 0..8 {
        let bits = glyph[gy];
        for gx in 0..8 {
            if (bits >> gx) & 1 == 0 {
                continue; // transparent: leave background alone
            }
            for dy in 0..2 {
                let px = x0 + gx;
                let py = y0 + gy * 2 + dy;
                if let Some((cx, cy, cw, ch)) = clip {
                    if px < cx || py < cy || px >= cx + cw || py >= cy + ch {
                        continue;
                    }
                }
                st.put_pixel(px, py, rgb);
            }
        }
    }
}

/// Pixel-positioned transparent text for the desktop compositor.
/// Unlike the grid-snapped console helpers, this draws at exact pixel
/// coordinates in `rgb` with no background fill and no line wrapping;
/// callers clip and wrap. Non-printable bytes render as space.
pub fn draw_text_px(
    x: usize,
    y: usize,
    s: &str,
    rgb: (u8, u8, u8),
    clip: Option<(usize, usize, usize, usize)>,
) {
    with_lock(|st| {
        draw_text_px_in(st, x, y, s, rgb, clip);
    });
}

/// `draw_text_px` core operating on an already-locked (possibly remapped)
/// state. Used by the double-buffer path without taking a second lock.
pub(crate) fn draw_text_px_in(
    st: &mut FbState,
    x: usize,
    y: usize,
    s: &str,
    rgb: (u8, u8, u8),
    clip: Option<(usize, usize, usize, usize)>,
) {
    for (i, b) in s.bytes().enumerate() {
        let ch = match b {
            0x20..=0x7e => b,
            b'\t' => b' ',
            _ => b' ',
        };
        // Skip fully off-screen glyphs early (put_pixel clips anyway).
        draw_glyph_px(st, x + i * CELL_W, y, ch, rgb, clip);
    }
}

/// Width in pixels of `s` in the 8px desktop font.
pub fn text_px_width(s: &str) -> usize {
    s.bytes().count() * CELL_W
}

/// Wallpaper gradient from `top` (screen top) toward 45% brightness at the
/// bottom edge. One lock for the whole region (the compositor used to issue
/// ~800 one-row fills per frame). Pure function of y, so partial repaints
/// blend seamlessly with full repaints.
pub fn paint_wallpaper_gradient(x: usize, y: usize, w: usize, h: usize, top: (u8, u8, u8)) {
    with_lock(|st| {
        paint_wallpaper_gradient_in(st, x, y, w, h, top);
    });
}

/// `paint_wallpaper_gradient` core operating on an already-locked
/// (possibly remapped) state. Used by the double-buffer path without
/// taking a second lock.
pub(crate) fn paint_wallpaper_gradient_in(
    st: &mut FbState,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    top: (u8, u8, u8),
) {
    if !st.is_active() {
        return;
    }
    let info = match st.info {
        Some(i) => i,
        None => return,
    };
    let full_h = info.height.max(1);
    let xs = x.min(info.width);
    let x_end = (x + w).min(info.width);
    let y_end = (y + h).min(info.height);
    if info.bytes_per_pixel == 4 {
        unsafe {
            let fb = st.base as *mut u32;
            let rgb = info.pixel_format == PixelFormat::Rgb;
            for py in y.min(info.height)..y_end {
                let t = py * 100 / full_h; // 0..100
                let k = (100 - t * 55 / 100) as u16; // 100 -> 45
                let (r, g, b) = (
                    (top.0 as u16 * k / 100) as u8,
                    (top.1 as u16 * k / 100) as u8,
                    (top.2 as u16 * k / 100) as u8,
                );
                let px: u32 = if rgb {
                    u32::from_le_bytes([r, g, b, 0])
                } else {
                    u32::from_le_bytes([b, g, r, 0])
                };
                let row = fb.add(py * info.stride + xs);
                for i in 0..(x_end - xs) {
                    core::ptr::write_volatile(row.add(i), px);
                }
            }
        }
        return;
    }
    for py in y.min(info.height)..y_end {
        let t = py * 100 / full_h;
        let k = (100 - t * 55 / 100) as u16;
        st.fill_px_rect(
            xs,
            py,
            x_end - xs,
            1,
            (top.0 as u16 * k / 100) as u8,
            (top.1 as u16 * k / 100) as u8,
            (top.2 as u16 * k / 100) as u8,
        );
    }
}

pub fn write_str_at(row: usize, col: usize, s: &str, fg: Color, bg: Color) {
    with_lock(|st| {
        if !st.is_active() || row >= st.rows {
            return;
        }
        let mut c = col;
        for b in s.bytes() {
            if c >= st.cols {
                break;
            }
            let ch = match b {
                0x20..=0x7e => b,
                b'\t' => b' ',
                _ => b' ',
            };
            st.fill_cell(c, row, fg, bg, ch);
            c += 1;
        }
    });
}

/// Fill a cell-rect with a char+color (maps `vga::fill_rect` semantics).
pub fn fill_rect(row: usize, col: usize, w: usize, h: usize, ch: u8, fg: Color, bg: Color) {
    with_lock(|st| {
        if !st.is_active() {
            return;
        }
        for r in row..(row + h).min(st.rows) {
            for c in col..(col + w).min(st.cols) {
                st.fill_cell(c, r, fg, bg, ch);
            }
        }
    });
}

pub fn clear_row_with(row: usize, fg: Color, bg: Color) {
    with_lock(|st| {
        if !st.is_active() || row >= st.rows {
            return;
        }
        let cols = st.cols;
        for c in 0..cols {
            st.fill_cell(c, row, fg, bg, b' ');
        }
    });
}

impl fmt::Write for FbState {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}
