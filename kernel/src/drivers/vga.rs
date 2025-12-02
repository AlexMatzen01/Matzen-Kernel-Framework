use core::fmt;
use core::ptr::NonNull;
use spin::{Mutex, MutexGuard};
use volatile::Volatile;

const VGA_BUFFER: usize = 0xb8000;
const HEIGHT: usize = 25;
const WIDTH: usize = 80;

#[allow(dead_code)]
#[derive(Copy, Clone)]
#[repr(u8)]
pub enum Color {
    Black = 0x0,
    Blue = 0x1,
    Green = 0x2,
    Cyan = 0x3,
    Red = 0x4,
    Magenta = 0x5,
    Brown = 0x6,
    LightGray = 0x7,
    DarkGray = 0x8,
    LightBlue = 0x9,
    LightGreen = 0xa,
    LightCyan = 0xb,
    LightRed = 0xc,
    Pink = 0xd,
    Yellow = 0xe,
    White = 0xf,
}

#[repr(transparent)]
#[derive(Copy, Clone)]
struct ColorCode(u8);

impl ColorCode {
    const fn new(fg: Color, bg: Color) -> Self {
        Self((bg as u8) << 4 | (fg as u8))
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

impl core::ops::Deref for ScreenChar {
    type Target = Self;
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl core::ops::DerefMut for ScreenChar {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; WIDTH]; HEIGHT],
}

struct VgaInner {
    column_position: usize,
    color_code: ColorCode,
    buffer: NonNull<Buffer>,
}

unsafe impl Send for VgaInner {}

pub struct VgaTextWriter {
    inner: Mutex<VgaInner>,
}

impl VgaTextWriter {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(VgaInner {
                column_position: 0,
                color_code: ColorCode::new(Color::LightGray, Color::Black),
                buffer: unsafe { NonNull::new_unchecked(VGA_BUFFER as *mut Buffer) },
            }),
        }
    }

    /// Acquire the writer lock and return a guard that implements `fmt::Write`.
    pub fn lock(&self) -> VgaWriteGuard<'_> {
        VgaWriteGuard {
            guard: self.inner.lock(),
        }
    }
}

pub struct VgaWriteGuard<'a> {
    guard: MutexGuard<'a, VgaInner>,
}

impl<'a> VgaWriteGuard<'a> {
    fn buffer(&mut self) -> &mut Buffer {
        unsafe { self.guard.buffer.as_mut() }
    }

    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.guard.column_position >= WIDTH {
                    self.new_line();
                }

                let row = HEIGHT - 1;
                let col = self.guard.column_position;
                let color = self.guard.color_code;
                self.buffer().chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code: color,
                });
                self.guard.column_position += 1;
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..HEIGHT {
            for col in 0..WIDTH {
                let character = self.buffer().chars[row][col].read();
                self.buffer().chars[row - 1][col].write(character);
            }
        }
        self.clear_row(HEIGHT - 1);
        self.guard.column_position = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let color = self.guard.color_code;
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: color,
        };
        for col in 0..WIDTH {
            self.buffer().chars[row][col].write(blank);
        }
    }
}

impl fmt::Write for VgaWriteGuard<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

impl VgaWriteGuard<'_> {
    pub fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        fmt::Write::write_fmt(self, args)
    }
}
