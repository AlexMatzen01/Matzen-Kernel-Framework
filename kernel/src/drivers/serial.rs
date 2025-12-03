//! Serial Port Driver (COM1)
//!
//! Provides output to the first serial port for debugging.

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// COM1 serial port base address
const COM1_PORT: u16 = 0x3F8;

/// Serial port writer
pub struct SerialPort {
    data: Port<u8>,
    line_status: Port<u8>,
}

impl SerialPort {
    /// Creates a new serial port instance for COM1
    const fn new() -> SerialPort {
        SerialPort {
            data: Port::new(COM1_PORT),
            line_status: Port::new(COM1_PORT + 5),
        }
    }

    /// Initializes the serial port
    fn init(&mut self) {
        unsafe {
            // Disable all interrupts
            Port::new(COM1_PORT + 1).write(0x00u8);
            // Enable DLAB (set baud rate divisor)
            Port::new(COM1_PORT + 3).write(0x80u8);
            // Set divisor to 3 (lo byte) 38400 baud
            Port::new(COM1_PORT + 0).write(0x03u8);
            // (hi byte)
            Port::new(COM1_PORT + 1).write(0x00u8);
            // 8 bits, no parity, one stop bit
            Port::new(COM1_PORT + 3).write(0x03u8);
            // Enable FIFO, clear them, with 14-byte threshold
            Port::new(COM1_PORT + 2).write(0xC7u8);
            // IRQs enabled, RTS/DSR set
            Port::new(COM1_PORT + 4).write(0x0Bu8);
        }
    }

    /// Sends a byte to the serial port
    fn send(&mut self, byte: u8) {
        unsafe {
            // Wait for the transmit buffer to be empty
            while self.line_status.read() & 0x20 == 0 {}
            self.data.write(byte);
        }
    }

    /// Writes a string to the serial port
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            self.send(byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

lazy_static! {
    /// Global serial port instance
    pub static ref SERIAL: Mutex<SerialPort> = {
        let mut serial = SerialPort::new();
        serial.init();
        Mutex::new(serial)
    };
}

/// Initializes the serial port
pub fn init() {
    // Force initialization of the lazy_static
    let _ = &*SERIAL;
}

/// Prints to the serial port
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL.lock().write_fmt(args).unwrap();
    });
}

/// Serial print macro
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::drivers::serial::_print(format_args!($($arg)*)));
}

/// Serial println macro
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
