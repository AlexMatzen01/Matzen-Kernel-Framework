//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Keyboard Input Driver
//!
//! Provides keyboard input from multiple sources:
//! - Serial port (for QEMU console mode with -serial stdio)
//! - PS/2 keyboard (for graphical mode)
//!
//! Extended for nano-like editor: exposes KeyEvent with arrows/Ctrl/etc.

use lazy_static::lazy_static;
use pc_keyboard::{layouts, DecodedKey, HandleControl, KeyCode, Keyboard, ScancodeSet1};
use spin::Mutex;

/// Size of the keyboard buffer (must be power of 2)
const BUFFER_SIZE: usize = 256;

// ── Public Key abstraction for editor ─────────────────────

/// High-level key understood by shell & editor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char), // Ctrl+<letter>  ('A'..'Z')
    Enter,
    Backspace,
    Tab,
    Esc,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F(u8),
    Unknown,
}

/// Full key event with modifiers (shift/alt/ctrl tracked if needed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        Self { key, shift: false, alt: false, ctrl: false }
    }
    pub fn ctrl(c: char) -> Self {
        Self { key: Key::Ctrl(c), shift: false, alt: false, ctrl: true }
    }
    pub fn char(c: char) -> Self {
        Self { key: Key::Char(c), shift: false, alt: false, ctrl: false }
    }
}

// ── Internal circular buffer for KeyEvent ─────────────────

struct KeyboardBuffer {
    buffer: [Option<KeyEvent>; BUFFER_SIZE],
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

    fn push(&mut self, ev: KeyEvent) -> bool {
        if self.count >= BUFFER_SIZE {
            return false;
        }
        self.buffer[self.write_pos] = Some(ev);
        self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<KeyEvent> {
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
    /// We keep HandleControl::Ignore and manually map Ctrl scancodes via RawKey modifiers
    /// to retain full control over Ctrl+Letter detection.
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore
        ));

    /// Keyboard input buffer (for PS/2 interrupt-driven input)
    static ref BUFFER: Mutex<KeyboardBuffer> = Mutex::new(KeyboardBuffer::new());

    /// Modifier state tracking (updated on RawKey press/release)
    static ref MOD_STATE: Mutex<ModState> = Mutex::new(ModState::new());
}

#[derive(Debug, Clone, Copy)]
struct ModState {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl ModState {
    const fn new() -> Self { Self { ctrl: false, shift: false, alt: false } }
}

/// Map pc_keyboard KeyCode -> our Key (for RawKey)
fn map_keycode(code: KeyCode) -> Option<Key> {
    match code {
        KeyCode::ArrowUp => Some(Key::ArrowUp),
        KeyCode::ArrowDown => Some(Key::ArrowDown),
        KeyCode::ArrowLeft => Some(Key::ArrowLeft),
        KeyCode::ArrowRight => Some(Key::ArrowRight),
        KeyCode::Home => Some(Key::Home),
        KeyCode::End => Some(Key::End),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        KeyCode::Delete => Some(Key::Delete),
        KeyCode::Insert => Some(Key::Insert),
        KeyCode::Escape => Some(Key::Esc),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Return => Some(Key::Enter),
        KeyCode::LControl | KeyCode::RControl => None, // modifier
        KeyCode::LShift | KeyCode::RShift => None,
        KeyCode::LAlt | KeyCode::RAltGr => None,
        KeyCode::F1 => Some(Key::F(1)), KeyCode::F2 => Some(Key::F(2)),
        KeyCode::F3 => Some(Key::F(3)), KeyCode::F4 => Some(Key::F(4)),
        KeyCode::F5 => Some(Key::F(5)), KeyCode::F6 => Some(Key::F(6)),
        KeyCode::F7 => Some(Key::F(7)), KeyCode::F8 => Some(Key::F(8)),
        KeyCode::F9 => Some(Key::F(9)), KeyCode::F10 => Some(Key::F(10)),
        KeyCode::F11 => Some(Key::F(11)), KeyCode::F12 => Some(Key::F(12)),
        _ => None,
    }
}

/// Initializes the keyboard driver
pub fn init() {
    use crate::serial_println;
    use x86_64::instructions::port::Port;

    let mut data_port: Port<u8> = Port::new(0x60);
    let mut status_port: Port<u8> = Port::new(0x64);

    while unsafe { status_port.read() } & 0x01 != 0 {
        unsafe { data_port.read() };
    }

    serial_println!("Keyboard driver initialized (serial + PS/2, KeyEvent mode)");
}

/// Called by PS/2 keyboard interrupt handler when a scancode arrives
pub fn handle_interrupt(scancode: u8) {
    let mut keyboard = KEYBOARD.lock();

    // We need to track modifier state ourselves by inspecting raw scancodes.
    // However pc_keyboard already tracks internal mods; we duplicate for our mapping.
    // Simpler: infer Ctrl via DecodedKey::Unicode control chars 0x01-0x1A when HandleControl::Ignore
    // actually delivers Unicode char with ctrl? With Ignore, ctrl+letter yields same letter (no). So we
    // must detect via RawKey modifiers: pc_keyboard's process_keyevent returns DecodedKey::Unicode
    // only when not a modifier, and modifier state is internal. Instead we check if scancode is
    // make/break for Ctrl/Shift/Alt and update MOD_STATE separately, then map subsequent keys.
    // For simplicity we update MOD_STATE on known make/break codes before decode.

    // Quick mod tracking for set1: 0x1D ctrl make, 0x9D break, 0x2A/0x36 shift, 0x38 alt
    {
        let mut mods = MOD_STATE.lock();
        match scancode {
            0x1D => mods.ctrl = true,
            0x9D => mods.ctrl = false,
            0x2A | 0x36 => mods.shift = true,
            0xAA | 0xB6 => mods.shift = false,
            0x38 => mods.alt = true,
            0xB8 => mods.alt = false,
            _ => {}
        }
    }

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        let mods = *MOD_STATE.lock();
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => {
                    // Handle Ctrl mapping: if ctrl held and char is letter, emit Ctrl
                    let mut buffer = BUFFER.lock();
                    if mods.ctrl && character.is_ascii_alphabetic() {
                        let ctrl_char = (character.to_ascii_uppercase() as u8) as char;
                        let _ = buffer.push(KeyEvent { key: Key::Ctrl(ctrl_char), shift: mods.shift, alt: mods.alt, ctrl: true });
                    } else if (character as u32) < 32 {
                        // Control char directly from serial / fallback (e.g., 0x03 for Ctrl+C, 0x1C for Ctrl+\)
                        // Map 0x01..0x1F to Ctrl+char (adds 64 => '@'..'_')
                        if (1..=31).contains(&(character as u8)) {
                            let ctrl_char = ((character as u8) + 64) as char;
                            let _ = buffer.push(KeyEvent { key: Key::Ctrl(ctrl_char), shift: mods.shift, alt: mods.alt, ctrl: true });
                        } else {
                            // e.g., Enter already handled via RawKey; but handle \r \n, Backspace, etc.
                            match character {
                                '\n' | '\r' => { let _ = buffer.push(KeyEvent{ key: Key::Enter, shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl }); }
                                '\x08' | '\x7f' => { let _ = buffer.push(KeyEvent{ key: Key::Backspace, shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl }); }
                                '\x09' => { let _ = buffer.push(KeyEvent{ key: Key::Tab, shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl }); }
                                '\x1b' => { let _ = buffer.push(KeyEvent{ key: Key::Esc, shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl }); }
                                _ => { let _ = buffer.push(KeyEvent{ key: Key::Char(character), shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl}); }
                            }
                        }
                    } else {
                        // Regular char – if alt held, mark alt
                        let _ = buffer.push(KeyEvent{ key: Key::Char(character), shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl});
                    }
                }
                DecodedKey::RawKey(keycode) => {
                    // Check modifier-only keys already handled above – ignore extra
                    if let Some(k) = map_keycode(keycode) {
                        let mut buffer = BUFFER.lock();
                        let _ = buffer.push(KeyEvent{ key: k, shift: mods.shift, alt: mods.alt, ctrl: mods.ctrl });
                    } else {
                        // For letter keys with Ctrl/Alt: if Ctrl held, map to Ctrl
                        // pc_keyboard RawKey for letters gives KeyCode like KeyCode::A etc – not yet mapped
                        // We handle via Unicode path above, so raw letters already delivered as Unicode.
                        // Only non-mapped raws are ignored.
                    }
                }
            }
        }
    }
}

// ── Serial escape sequence state for arrow keys via serial console ──
static SERIAL_ESC_STATE: Mutex<u8> = Mutex::new(0); // 0 idle, 1 got ESC, 2 got '['

fn try_parse_serial_byte(b: u8) -> Option<KeyEvent> {
    // Handles VT100 escape sequences: ESC [ A/B/C/D  (up/down/right/left), also H/F etc.
    // Also handles raw Ctrl bytes (0x01..0x1A)
    let mut state = SERIAL_ESC_STATE.lock();
    match *state {
        0 => {
            if b == 0x1B { *state = 1; return None; }
            if b == b'\r' || b == b'\n' { return Some(KeyEvent::new(Key::Enter)); }
            if b == 0x7F || b == 0x08 { return Some(KeyEvent::new(Key::Backspace)); }
            if b == 0x09 { return Some(KeyEvent::new(Key::Tab)); }
            if b >= 1 && b <= 26 { return Some(KeyEvent::ctrl((b + 64) as char)); }
            if b == 0x1B { return Some(KeyEvent::new(Key::Esc)); }
            if b.is_ascii() && !b.is_ascii_control() { return Some(KeyEvent::char(b as char)); }
            None
        }
        1 => {
            if b == b'[' { *state = 2; return None; }
            // ESC alone
            *state = 0;
            // Push ESC then re-evaluate b
            let pending = b;
            // Recurse: treat as idle again
            drop(state);
            let ev = Some(KeyEvent::new(Key::Esc));
            // Now re-feed pending byte if it may produce key
            if let Some(ev2) = try_parse_serial_byte(pending) {
                // Queue ESC first then pending -> need to push both; we return ESC now and buffer pending via BUFFER
                // Protect BUFFER with without_interrupts (IRQ1 may push concurrently)
                x86_64::instructions::interrupts::without_interrupts(|| {
                    let mut buf = BUFFER.lock();
                    let _ = buf.push(ev2);
                });
            }
            return ev;
        }
        2 => {
            *state = 0;
            match b {
                b'A' => return Some(KeyEvent::new(Key::ArrowUp)),
                b'B' => return Some(KeyEvent::new(Key::ArrowDown)),
                b'C' => return Some(KeyEvent::new(Key::ArrowRight)),
                b'D' => return Some(KeyEvent::new(Key::ArrowLeft)),
                b'H' => return Some(KeyEvent::new(Key::Home)),
                b'F' => return Some(KeyEvent::new(Key::End)),
                b'5' => { // PageUp is ESC[5~
                    // Need to consume trailing ~ ; peek not available, assume next byte is ~
                    // We can't consume future; treat as PageUp
                    return Some(KeyEvent::new(Key::PageUp));
                }
                b'6' => return Some(KeyEvent::new(Key::PageDown)),
                b'2' => return Some(KeyEvent::new(Key::Insert)),
                b'3' => return Some(KeyEvent::new(Key::Delete)),
                _ => return None,
            }
        }
        _ => { *state = 0; None }
    }
}

/// Reads a KeyEvent from keyboard/serial (non-blocking)
/// Uses `without_interrupts` for BUFFER to avoid deadlock with IRQ1 handler.
pub fn read_key() -> Option<KeyEvent> {
    use x86_64::instructions::interrupts;

    // 1) PS/2 buffered KeyEvent – protect against IRQ1
    let ps2_ev = interrupts::without_interrupts(|| {
        let mut buffer = BUFFER.lock();
        buffer.pop()
    });
    if let Some(ev) = ps2_ev {
        return Some(ev);
    }

    // 2) Serial buffered chars -> map to KeyEvent
    // `serial::read_char` already disables interrupts internally, so safe to call outside.
    if let Some(c) = crate::drivers::serial::read_char() {
        let b = c as u8;
        if let Some(ev) = try_parse_serial_byte(b) {
            return Some(ev);
        } else {
            // Escape sequences that need more bytes will return None; we will poll again next call.
            // To avoid dropping byte, we rely on state machine; if None after ESC we wait for next byte.
            return None;
        }
    }

    None
}

/// Reads a character from keyboard/serial (non-blocking) – compat shim for shell
pub fn read_char() -> Option<char> {
    // Use read_key to preserve ordering, but convert to char where sensible
    if let Some(ev) = read_key() {
        match ev.key {
            Key::Char(c) => Some(c),
            Key::Enter => Some('\n'),
            Key::Backspace => Some('\x08'),
            Key::Ctrl(c) => {
                // Map Ctrl+C -> 0x03 etc. for shell interrupt handling
                let byte = (c as u8) & 0x1F;
                Some(byte as char)
            }
            Key::Tab => Some('\t'),
            Key::Esc => Some('\x1b'),
            _ => None, // arrows etc. not chars – drop for shell line editing (or Could be ignored)
        }
    } else {
        None
    }
}

/// Waits for and returns a KeyEvent (blocking)
pub fn wait_for_key() -> KeyEvent {
    loop {
        if let Some(ev) = read_key() {
            return ev;
        }
        x86_64::instructions::hlt();
    }
}

/// Waits for and returns a character (blocking) – compat
pub fn wait_for_char() -> char {
    loop {
        if let Some(c) = read_char() {
            return c;
        }
        x86_64::instructions::hlt();
    }
}

/// Checks if there are characters/keys available
pub fn has_char() -> bool {
    if crate::drivers::serial::has_input() { return true; }
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let buffer = BUFFER.lock();
        !buffer.is_empty()
    })
}

/// Checks if key available (including serial)
pub fn has_key() -> bool {
    if crate::drivers::serial::has_input() { return true; }
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let buffer = BUFFER.lock();
        !buffer.is_empty()
    })
}

/// Peek / debug: returns true if serial escape pending
pub fn serial_escape_pending() -> bool { *SERIAL_ESC_STATE.lock() != 0 }
