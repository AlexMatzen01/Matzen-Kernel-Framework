//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! VGA Text Buffer Driver
//!
//! Provides text output to the VGA text buffer at 0xb8000.

use core::fmt;
use core::ptr;
use spin::Mutex;

/// VGA text buffer width
const BUFFER_WIDTH: usize = 80;
/// VGA text buffer height
const BUFFER_HEIGHT: usize = 25;

/// VGA text buffer physical address
const VGA_BUFFER_PHYS: u64 = 0xb9000;

/// VGA color codes
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// A combination of foreground and background colors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    const fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

/// A VGA screen character with its color
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

/// The VGA text buffer
#[repr(transparent)]
struct Buffer {
    chars: [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

impl Buffer {
    /// Read a character at the given position
    fn read(&self, row: usize, col: usize) -> ScreenChar {
        unsafe { ptr::read_volatile(&self.chars[row][col]) }
    }

    /// Write a character at the given position
    fn write(&mut self, row: usize, col: usize, ch: ScreenChar) {
        unsafe { ptr::write_volatile(&mut self.chars[row][col], ch) }
    }
}

/// VGA hardware cursor ports
const VGA_CRTC_ADDR: u16 = 0x3D4;
const VGA_CRTC_DATA: u16 = 0x3D5;

/// Public constants for external use (editor viewport)
pub const VGA_WIDTH: usize = BUFFER_WIDTH;
pub const VGA_HEIGHT: usize = BUFFER_HEIGHT;

/// Writer for the VGA text buffer
pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: Option<&'static mut Buffer>,
}

impl Writer {
    /// Creates a new uninitialized writer
    const fn new_uninit() -> Writer {
        Writer {
            column_position: 0,
            color_code: ColorCode::new(Color::LightGreen, Color::Black),
            buffer: None,
        }
    }

    /// Initializes the writer with the buffer address
    fn initialize(&mut self, buffer_addr: u64) {
        self.buffer = Some(unsafe { &mut *(buffer_addr as *mut Buffer) });
    }

    /// Writes a byte to the VGA buffer
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            b'\x08' => {
                // Backspace: move cursor back one position
                if self.column_position > 0 {
                    self.column_position -= 1;
                }
            }
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                if let Some(buffer) = &mut self.buffer {
                    let row = BUFFER_HEIGHT - 1;
                    let col = self.column_position;

                    let color_code = self.color_code;
                    buffer.write(
                        row,
                        col,
                        ScreenChar {
                            ascii_character: byte,
                            color_code,
                        },
                    );
                    self.column_position += 1;
                }
                // Note: If buffer is None, we silently skip writing.
                // This is intentional to avoid panics before VGA is initialized.
            }
        }
    }

    /// Writes a string to the VGA buffer
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Printable ASCII, newline, or backspace
                0x20..=0x7e | b'\n' | b'\x08' => self.write_byte(byte),
                // Not part of printable ASCII range, print placeholder
                _ => self.write_byte(0xfe),
            }
        }
    }

    /// Moves all lines up by one and clears the last row
    fn new_line(&mut self) {
        if let Some(buffer) = &mut self.buffer {
            for row in 1..BUFFER_HEIGHT {
                for col in 0..BUFFER_WIDTH {
                    let character = buffer.read(row, col);
                    buffer.write(row - 1, col, character);
                }
            }
            self.clear_row(BUFFER_HEIGHT - 1);
            self.column_position = 0;
        }
    }

    /// Clears a row by filling it with blank characters
    fn clear_row(&mut self, row: usize) {
        if let Some(buffer) = &mut self.buffer {
            let blank = ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            };
            for col in 0..BUFFER_WIDTH {
                buffer.write(row, col, blank);
            }
        }
    }

    /// Clears the entire screen
    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        // After clearing, we're ready to write at the bottom row
        // The screen is now blank and ready for new content
    }

    /// Sets the text color
    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    /// Deletes the last character (backspace)
    pub fn backspace(&mut self) {
        if let Some(buffer) = &mut self.buffer {
            if self.column_position > 0 {
                self.column_position -= 1;
                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;
                let blank = ScreenChar {
                    ascii_character: b' ',
                    color_code: self.color_code,
                };
                buffer.write(row, col, blank);
                // Ensure the write is visible by reading it back
                let _ = buffer.read(row, col);
            }
        }
    }

    // ──────────────────────────────────────────────
    // Full-screen / editor extensions (nano-like)
    // ──────────────────────────────────────────────

    /// Write a single char at absolute (row,col) with explicit color
    pub fn write_at(&mut self, row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return;
        }
        if let Some(buffer) = &mut self.buffer {
            let color = ColorCode::new(fg, bg);
            buffer.write(
                row,
                col,
                ScreenChar {
                    ascii_character: byte,
                    color_code: color,
                },
            );
        }
    }

    /// Write a string at absolute (row,col) with explicit color, clipping at width
    pub fn write_str_at(&mut self, row: usize, col: usize, s: &str, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT {
            return;
        }
        let mut c = col;
        for b in s.bytes() {
            if c >= BUFFER_WIDTH {
                break;
            }
            let ch = match b {
                0x20..=0x7e => b,
                b'\t' => b' ',
                _ => 0xfe,
            };
            self.write_at(row, c, ch, fg, bg);
            c += 1;
        }
    }

    /// Fill a rectangular region with a char+color
    pub fn fill_rect(
        &mut self,
        row: usize,
        col: usize,
        width: usize,
        height: usize,
        ch: u8,
        fg: Color,
        bg: Color,
    ) {
        let color = ColorCode::new(fg, bg);
        if let Some(buffer) = &mut self.buffer {
            for r in row..(row + height).min(BUFFER_HEIGHT) {
                for c in col..(col + width).min(BUFFER_WIDTH) {
                    buffer.write(r, c, ScreenChar { ascii_character: ch, color_code: color });
                }
            }
        }
    }

    /// Clear a row with specific colors
    pub fn clear_row_with(&mut self, row: usize, fg: Color, bg: Color) {
        self.fill_rect(row, 0, BUFFER_WIDTH, 1, b' ', fg, bg);
    }

    /// Update hardware cursor position (visible cursor)
    pub fn set_cursor_pos(&mut self, row: usize, col: usize) {
        let pos = (row * BUFFER_WIDTH + col) as u16;
        unsafe {
            use x86_64::instructions::port::Port;
            let mut addr = Port::<u8>::new(VGA_CRTC_ADDR);
            let mut data = Port::<u8>::new(VGA_CRTC_DATA);
            addr.write(0x0F_u8);
            data.write((pos & 0xFF) as u8);
            addr.write(0x0E_u8);
            data.write(((pos >> 8) & 0xFF) as u8);
        }
    }

    /// Show hardware cursor (default shape: lines 0..15)
    pub fn show_cursor(&mut self) {
        unsafe {
            use x86_64::instructions::port::Port;
            let mut addr = Port::<u8>::new(VGA_CRTC_ADDR);
            let mut data = Port::<u8>::new(VGA_CRTC_DATA);
            addr.write(0x0A_u8);
            data.write(0x00_u8); // start line
            addr.write(0x0B_u8);
            data.write(0x0F_u8); // end line
        }
    }

    /// Hide hardware cursor
    pub fn hide_cursor(&mut self) {
        unsafe {
            use x86_64::instructions::port::Port;
            let mut addr = Port::<u8>::new(VGA_CRTC_ADDR);
            let mut data = Port::<u8>::new(VGA_CRTC_DATA);
            addr.write(0x0A_u8);
            data.write(0x20_u8); // bit 5 disables cursor
        }
    }

    /// Get current color code
    pub fn current_color(&self) -> (Color, Color) {
        // Decode; we only store ColorCode as u8
        let code = self.color_code.0;
        let fg_v = code & 0x0F;
        let bg_v = (code >> 4) & 0x0F;
        let to_color = |v: u8| match v {
            0 => Color::Black,
            1 => Color::Blue,
            2 => Color::Green,
            3 => Color::Cyan,
            4 => Color::Red,
            5 => Color::Magenta,
            6 => Color::Brown,
            7 => Color::LightGray,
            8 => Color::DarkGray,
            9 => Color::LightBlue,
            10 => Color::LightGreen,
            11 => Color::LightCyan,
            12 => Color::LightRed,
            13 => Color::Pink,
            14 => Color::Yellow,
            15 => Color::White,
            _ => Color::White,
        };
        (to_color(fg_v), to_color(bg_v))
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

/// Global writer instance
pub static WRITER: Mutex<Writer> = Mutex::new(Writer::new_uninit());

/// Initializes the VGA text buffer with the physical memory offset
pub fn init_with_offset(physical_memory_offset: u64) {
    let vga_buffer_virt = physical_memory_offset + VGA_BUFFER_PHYS;
    let mut writer = WRITER.lock();
    writer.initialize(vga_buffer_virt);
    writer.clear_screen();
}

/// Prints a formatted string to the VGA buffer and serial port.
///
/// Disables interrupts while writing to prevent race conditions with concurrent
/// access from interrupt handlers. This ensures the VGA buffer stays consistent.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    // Disable interrupts to prevent deadlock if an interrupt handler tries to print
    // while we're holding the WRITER lock
    interrupts::without_interrupts(|| {
        WRITER.lock().write_fmt(args).unwrap();
        // Also write to serial for console access
        crate::drivers::serial::_print(args);
    });
}

/// Print macro similar to std::print!
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::drivers::vga::_print(format_args!($($arg)*)));
}

/// Println macro similar to std::println!
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Clears the screen.
///
/// Disables interrupts to prevent race conditions with concurrent access.
pub fn clear_screen() {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        WRITER.lock().clear_screen();
    });
}

/// Handles backspace.
///
/// Disables interrupts to prevent race conditions with concurrent access.
pub fn backspace() {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        WRITER.lock().backspace();
    });
}

// ── Editor / full-screen helpers ─────────────────────────

/// Write a char at absolute position with color
pub fn write_at(row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().write_at(row, col, byte, fg, bg);
    });
}

/// Write string at absolute position with color
pub fn write_str_at(row: usize, col: usize, s: &str, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().write_str_at(row, col, s, fg, bg);
    });
}

/// Fill rect with char+color
pub fn fill_rect(row: usize, col: usize, w: usize, h: usize, ch: u8, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().fill_rect(row, col, w, h, ch, fg, bg);
    });
}

/// Set hardware cursor position
pub fn set_cursor_pos(row: usize, col: usize) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().set_cursor_pos(row, col);
    });
}

/// Show hardware cursor
pub fn show_cursor() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().show_cursor();
    });
}

/// Hide hardware cursor
pub fn hide_cursor() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().hide_cursor();
    });
}

/// Clear a single row with specific colors
pub fn clear_row_with(row: usize, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().clear_row_with(row, fg, bg);
    });
}
