//! PS/2 Keyboard Driver
//!
//! This module provides basic PS/2 keyboard input functionality.
//! It handles scan codes and converts them to ASCII characters.

use spin::Mutex;
use x86_64::instructions::port::Port;

/// PS/2 keyboard data port
const KEYBOARD_DATA_PORT: u16 = 0x60;
/// PS/2 keyboard status port
const KEYBOARD_STATUS_PORT: u16 = 0x64;

/// Global keyboard state
static KEYBOARD: Mutex<Keyboard> = Mutex::new(Keyboard::new());

/// Keyboard driver state
pub struct Keyboard {
    data_port: Port<u8>,
    status_port: Port<u8>,
    shift_pressed: bool,
    ctrl_pressed: bool,
    caps_lock: bool,
}

impl Keyboard {
    /// Create a new keyboard instance
    const fn new() -> Self {
        Self {
            data_port: Port::new(KEYBOARD_DATA_PORT),
            status_port: Port::new(KEYBOARD_STATUS_PORT),
            shift_pressed: false,
            ctrl_pressed: false,
            caps_lock: false,
        }
    }

    /// Read a scan code from the keyboard
    fn read_scancode(&mut self) -> Option<u8> {
        let status = unsafe { self.status_port.read() };
        if status & 0x01 != 0 {
            Some(unsafe { self.data_port.read() })
        } else {
            None
        }
    }

    /// Convert a scan code to an ASCII character
    fn scancode_to_char(&mut self, scancode: u8) -> Option<char> {
        // Handle key release (scan codes with bit 7 set)
        if scancode & 0x80 != 0 {
            let released = scancode & 0x7F;
            match released {
                0x2A | 0x36 => self.shift_pressed = false, // Left/Right Shift released
                0x1D => self.ctrl_pressed = false,          // Ctrl released
                _ => {}
            }
            return None;
        }

        // Handle key press
        match scancode {
            0x2A | 0x36 => {
                self.shift_pressed = true;
                None
            }
            0x1D => {
                self.ctrl_pressed = true;
                None
            }
            0x3A => {
                self.caps_lock = !self.caps_lock;
                None
            }
            _ => self.map_scancode(scancode),
        }
    }

    /// Map a scan code to a character
    fn map_scancode(&self, scancode: u8) -> Option<char> {
        let use_shift = self.shift_pressed ^ self.caps_lock;
        
        // Standard US QWERTY keyboard layout (Set 1 scan codes)
        let ch = match scancode {
            // Number row
            0x02 => if self.shift_pressed { '!' } else { '1' },
            0x03 => if self.shift_pressed { '@' } else { '2' },
            0x04 => if self.shift_pressed { '#' } else { '3' },
            0x05 => if self.shift_pressed { '$' } else { '4' },
            0x06 => if self.shift_pressed { '%' } else { '5' },
            0x07 => if self.shift_pressed { '^' } else { '6' },
            0x08 => if self.shift_pressed { '&' } else { '7' },
            0x09 => if self.shift_pressed { '*' } else { '8' },
            0x0A => if self.shift_pressed { '(' } else { '9' },
            0x0B => if self.shift_pressed { ')' } else { '0' },
            0x0C => if self.shift_pressed { '_' } else { '-' },
            0x0D => if self.shift_pressed { '+' } else { '=' },
            
            // Top row (QWERTY)
            0x10 => if use_shift { 'Q' } else { 'q' },
            0x11 => if use_shift { 'W' } else { 'w' },
            0x12 => if use_shift { 'E' } else { 'e' },
            0x13 => if use_shift { 'R' } else { 'r' },
            0x14 => if use_shift { 'T' } else { 't' },
            0x15 => if use_shift { 'Y' } else { 'y' },
            0x16 => if use_shift { 'U' } else { 'u' },
            0x17 => if use_shift { 'I' } else { 'i' },
            0x18 => if use_shift { 'O' } else { 'o' },
            0x19 => if use_shift { 'P' } else { 'p' },
            0x1A => if self.shift_pressed { '{' } else { '[' },
            0x1B => if self.shift_pressed { '}' } else { ']' },
            
            // Home row (ASDF)
            0x1E => if use_shift { 'A' } else { 'a' },
            0x1F => if use_shift { 'S' } else { 's' },
            0x20 => if use_shift { 'D' } else { 'd' },
            0x21 => if use_shift { 'F' } else { 'f' },
            0x22 => if use_shift { 'G' } else { 'g' },
            0x23 => if use_shift { 'H' } else { 'h' },
            0x24 => if use_shift { 'J' } else { 'j' },
            0x25 => if use_shift { 'K' } else { 'k' },
            0x26 => if use_shift { 'L' } else { 'l' },
            0x27 => if self.shift_pressed { ':' } else { ';' },
            0x28 => if self.shift_pressed { '"' } else { '\'' },
            0x29 => if self.shift_pressed { '~' } else { '`' },
            0x2B => if self.shift_pressed { '|' } else { '\\' },
            
            // Bottom row (ZXCV)
            0x2C => if use_shift { 'Z' } else { 'z' },
            0x2D => if use_shift { 'X' } else { 'x' },
            0x2E => if use_shift { 'C' } else { 'c' },
            0x2F => if use_shift { 'V' } else { 'v' },
            0x30 => if use_shift { 'B' } else { 'b' },
            0x31 => if use_shift { 'N' } else { 'n' },
            0x32 => if use_shift { 'M' } else { 'm' },
            0x33 => if self.shift_pressed { '<' } else { ',' },
            0x34 => if self.shift_pressed { '>' } else { '.' },
            0x35 => if self.shift_pressed { '?' } else { '/' },
            
            // Special keys
            0x0E => '\x08', // Backspace
            0x0F => '\t',   // Tab
            0x1C => '\n',   // Enter
            0x39 => ' ',    // Space
            
            _ => return None,
        };
        
        Some(ch)
    }
}

/// Read a character from the keyboard (non-blocking)
pub fn read_char() -> Option<char> {
    let mut keyboard = KEYBOARD.lock();
    if let Some(scancode) = keyboard.read_scancode() {
        keyboard.scancode_to_char(scancode)
    } else {
        None
    }
}

/// Check if Ctrl key is currently pressed
pub fn is_ctrl_pressed() -> bool {
    KEYBOARD.lock().ctrl_pressed
}
