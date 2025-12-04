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
const VGA_BUFFER_PHYS: u64 = 0xb8000;

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
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                if let Some(buffer) = &mut self.buffer {
                    let row = BUFFER_HEIGHT - 1;
                    let col = self.column_position;

                    let color_code = self.color_code;
                    buffer.write(row, col, ScreenChar {
                        ascii_character: byte,
                        color_code,
                    });
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
                // Printable ASCII or newline
                0x20..=0x7e | b'\n' => self.write_byte(byte),
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
    }

    /// Sets the text color
    #[allow(dead_code)]
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
                buffer.write(row, col, ScreenChar {
                    ascii_character: b' ',
                    color_code: self.color_code,
                });
            }
        }
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

/// Prints a formatted string to the VGA buffer.
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
