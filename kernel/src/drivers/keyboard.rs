//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.


//! PS/2 Keyboard Driver
//!
//! Provides keyboard input support via interrupt-driven PS/2 interface.
//! Uses a circular buffer to store scancodes from the keyboard interrupt handler.

use lazy_static::lazy_static;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// PS/2 data port - reads scancode data
const KEYBOARD_DATA_PORT: u16 = 0x60;
/// PS/2 command port
const KEYBOARD_COMMAND_PORT: u16 = 0x64;

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
            return false; // Buffer full
        }
        
        self.buffer[self.write_pos] = Some(c);
        self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<char> {
        if self.count == 0 {
            return None; // Buffer empty
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
    /// Keyboard decoder
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = 
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore
        ));
    
    /// Keyboard input buffer
    static ref BUFFER: Mutex<KeyboardBuffer> = Mutex::new(KeyboardBuffer::new());
}

/// Initializes the keyboard driver
pub fn init() {
    use crate::serial_println;
    
    // Clear the keyboard buffer
    let mut port: Port<u8> = Port::new(KEYBOARD_DATA_PORT);
    while unsafe { Port::<u8>::new(0x64).read() } & 0x01 != 0 {
        unsafe { port.read() };
    }
    
    serial_println!("Keyboard buffer cleared");
}

/// Called by interrupt handler when a scancode arrives
pub fn handle_interrupt(scancode: u8) {
    let mut keyboard = KEYBOARD.lock();
    
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => {
                    let mut buffer = BUFFER.lock();
                    if !buffer.push(character) {
                        // Buffer overflow - drop the character
                        use crate::serial_println;
                        serial_println!("Warning: Keyboard buffer overflow!");
                    }
                }
                DecodedKey::RawKey(_key) => {
                    // Handle special keys if needed
                }
            }
        }
    }
}

/// Reads a character from keyboard buffer (non-blocking)
pub fn read_char() -> Option<char> {
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

/// Checks if there are characters available in the buffer
pub fn has_char() -> bool {
    let buffer = BUFFER.lock();
    !buffer.is_empty()
}
