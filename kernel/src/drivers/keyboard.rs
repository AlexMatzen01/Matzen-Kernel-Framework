//! PS/2 Keyboard Driver
//!
//! Provides keyboard input support via PS/2 port polling.

use lazy_static::lazy_static;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// PS/2 data port - reads scancode data
const KEYBOARD_DATA_PORT: u16 = 0x60;
/// PS/2 status/command port - reads status, writes commands
const KEYBOARD_STATUS_PORT: u16 = 0x64;
/// Status register bit indicating data is available to read
const STATUS_OUTPUT_BUFFER_FULL: u8 = 0x01;

lazy_static! {
    /// Keyboard decoder
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = 
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore
        ));
}

/// Initializes the keyboard driver
pub fn init() {
    // Keyboard is already initialized via lazy_static
}

/// Reads a key from the keyboard (blocking)
pub fn read_key() -> Option<DecodedKey> {
    let mut data_port: Port<u8> = Port::new(KEYBOARD_DATA_PORT);
    let mut status_port: Port<u8> = Port::new(KEYBOARD_STATUS_PORT);
    
    // Check if data is available (output buffer full bit)
    let status = unsafe { status_port.read() };
    if status & STATUS_OUTPUT_BUFFER_FULL == 0 {
        return None;
    }
    
    // Read the scancode
    let scancode = unsafe { data_port.read() };
    
    // Process the scancode
    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            return Some(key);
        }
    }
    
    None
}

/// Reads a character from keyboard (blocking)
pub fn read_char() -> Option<char> {
    match read_key() {
        Some(DecodedKey::Unicode(character)) => Some(character),
        Some(DecodedKey::RawKey(_)) => None,
        None => None,
    }
}

/// Waits for and returns a character (blocking)
#[allow(dead_code)]
pub fn wait_for_char() -> char {
    loop {
        if let Some(c) = read_char() {
            return c;
        }
        x86_64::instructions::hlt();
    }
}
