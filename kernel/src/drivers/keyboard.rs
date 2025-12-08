//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Keyboard Input Driver
//!
//! Provides keyboard input from multiple sources:
//! - Serial port (for QEMU console mode with -serial stdio)
//! - PS/2 keyboard (for graphical mode)

use lazy_static::lazy_static;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;

/// Size of the keyboard buffer (must be power of 2)
const BUFFER_SIZE: usize = 128;

/// Circular buffer for storing characters from keyboard
struct KeyboardBuffer {
    buffer: [Option<char>; BUFFER_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl KeyboardBuffer {
    const fn new() -> Self {
        KeyboardBuffer {
            buffer: [None; BUFFER_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, c: char) -> bool {
        if self.count >= BUFFER_SIZE {
            return false;
        }

        self.buffer[self.write_pos] = Some(c);
        self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<char> {
        if self.count == 0 {
            return None;
        }

        let c = self.buffer[self.read_pos];
        self.read_pos = (self.read_pos + 1) % BUFFER_SIZE;
        self.count -= 1;
        c
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }
}

lazy_static! {
    /// Keyboard decoder for PS/2 scancodes
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore
        ));

    /// Keyboard input buffer (for PS/2 interrupt-driven input)
    static ref BUFFER: Mutex<KeyboardBuffer> = Mutex::new(KeyboardBuffer::new());
}

/// Initializes the keyboard driver
pub fn init() {
    use crate::serial_println;
    use x86_64::instructions::port::Port;

    // Clear any pending data in the PS/2 keyboard buffer
    let mut data_port: Port<u8> = Port::new(0x60);
    let mut status_port: Port<u8> = Port::new(0x64);

    // Flush keyboard buffer
    while unsafe { status_port.read() } & 0x01 != 0 {
        unsafe { data_port.read() };
    }

    serial_println!("Keyboard driver initialized (serial + PS/2)");
}

/// Called by PS/2 keyboard interrupt handler when a scancode arrives
pub fn handle_interrupt(scancode: u8) {
    let mut keyboard = KEYBOARD.lock();

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => {
                    let mut buffer = BUFFER.lock();
                    let _ = buffer.push(character);
                }
                DecodedKey::RawKey(_key) => {
                    // Handle special keys if needed
                }
            }
        }
    }
}

/// Reads a character from keyboard/serial (non-blocking)
///
/// This checks both serial input (for QEMU console mode) and
/// the PS/2 keyboard buffer (for graphical mode).
pub fn read_char() -> Option<char> {
    // First, check serial port (primary input for console mode)
    if let Some(c) = crate::drivers::serial::read_char() {
        return Some(c);
    }

    // Then check PS/2 keyboard buffer
    let mut buffer = BUFFER.lock();
    buffer.pop()
}

/// Waits for and returns a character (blocking)
pub fn wait_for_char() -> char {
    loop {
        if let Some(c) = read_char() {
            return c;
        }
        // Halt until next interrupt
        x86_64::instructions::hlt();
    }
}

/// Checks if there are characters available
pub fn has_char() -> bool {
    // Check serial first
    if crate::drivers::serial::has_input() {
        return true;
    }

    // Then check PS/2 buffer
    let buffer = BUFFER.lock();
    !buffer.is_empty()
}
