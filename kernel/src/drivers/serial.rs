//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.


//! Serial Port Driver (COM1)
//!
//! Provides input/output to the first serial port for debugging and console access.

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// COM1 serial port base address
const COM1_PORT: u16 = 0x3F8;

/// Size of the serial input buffer
const BUFFER_SIZE: usize = 256;

/// Circular buffer for storing characters from serial port
struct InputBuffer {
    buffer: [u8; BUFFER_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl InputBuffer {
    const fn new() -> Self {
        InputBuffer {
            buffer: [0; BUFFER_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, c: u8) -> bool {
        if self.count >= BUFFER_SIZE {
            return false;
        }
        
        self.buffer[self.write_pos] = c;
        self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.count == 0 {
            return None;
        }
        
        let c = self.buffer[self.read_pos];
        self.read_pos = (self.read_pos + 1) % BUFFER_SIZE;
        self.count -= 1;
        Some(c)
    }
}

/// Serial port writer
pub struct SerialPort {
    data: Port<u8>,
    int_enable: Port<u8>,
    fifo_ctrl: Port<u8>,
    line_ctrl: Port<u8>,
    modem_ctrl: Port<u8>,
    line_status: Port<u8>,
}

impl SerialPort {
    /// Creates a new serial port instance for COM1
    const fn new() -> SerialPort {
        SerialPort {
            data: Port::new(COM1_PORT),
            int_enable: Port::new(COM1_PORT + 1),
            fifo_ctrl: Port::new(COM1_PORT + 2),
            line_ctrl: Port::new(COM1_PORT + 3),
            modem_ctrl: Port::new(COM1_PORT + 4),
            line_status: Port::new(COM1_PORT + 5),
        }
    }

    /// Initializes the serial port with interrupt support for receiving data
    fn init(&mut self) {
        unsafe {
            // Disable all interrupts during setup
            self.int_enable.write(0x00u8);
            
            // Enable DLAB (set baud rate divisor)
            self.line_ctrl.write(0x80u8);
            
            // Set divisor to 3 (lo byte) = 38400 baud
            self.data.write(0x03u8);
            // (hi byte)
            self.int_enable.write(0x00u8);
            
            // 8 bits, no parity, one stop bit, disable DLAB
            self.line_ctrl.write(0x03u8);
            
            // Enable FIFO, clear them, with 14-byte threshold
            self.fifo_ctrl.write(0xC7u8);
            
            // IRQs enabled, RTS/DSR set, enable aux output 2 (required for interrupts)
            self.modem_ctrl.write(0x0Bu8);
            
            // Enable received data available interrupt
            self.int_enable.write(0x01u8);
        }
    }

    /// Checks if data is available to read
    fn data_available(&mut self) -> bool {
        unsafe { self.line_status.read() & 0x01 != 0 }
    }

    /// Reads a byte from the serial port (non-blocking)
    fn read_byte(&mut self) -> Option<u8> {
        if self.data_available() {
            Some(unsafe { self.data.read() })
        } else {
            None
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
    
    /// Input buffer for serial port
    static ref INPUT_BUFFER: Mutex<InputBuffer> = Mutex::new(InputBuffer::new());
}

/// Initializes the serial port
pub fn init() {
    // Force initialization of the lazy_static
    let _ = &*SERIAL;
}

/// Called by interrupt handler when serial data arrives
pub fn handle_interrupt() {
    let mut serial = SERIAL.lock();
    while let Some(byte) = serial.read_byte() {
        let mut buffer = INPUT_BUFFER.lock();
        buffer.push(byte);
    }
}

/// Reads a character from serial input buffer (non-blocking)
pub fn read_char() -> Option<char> {
    // First try to get from buffer (interrupt-driven)
    {
        let mut buffer = INPUT_BUFFER.lock();
        if let Some(byte) = buffer.pop() {
            return Some(byte as char);
        }
    }
    
    // Also poll directly in case interrupts aren't working
    {
        let mut serial = SERIAL.lock();
        if let Some(byte) = serial.read_byte() {
            return Some(byte as char);
        }
    }
    
    None
}

/// Checks if there's input available
pub fn has_input() -> bool {
    let buffer = INPUT_BUFFER.lock();
    if buffer.count > 0 {
        return true;
    }
    
    let mut serial = SERIAL.lock();
    serial.data_available()
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
