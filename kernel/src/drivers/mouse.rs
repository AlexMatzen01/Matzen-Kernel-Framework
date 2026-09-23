//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Mouse Driver (PS/2 primary, USB HID merged)
//!
//! PS/2 path: polling-free, interrupt-driven — the IRQ12 handler in
//! `interrupts.rs` reads the data port (0x60) and forwards each byte here.
//! Three bytes form one packet (status, dx, dy).
//!
//! USB path: the xHCI/EHCI drivers claim HID mice and tablets and push
//! decoded reports via [`push_usb_mouse`]/[`push_usb_tablet`]. All sources
//! share one position/buttons/event queue: relative reports accumulate,
//! absolute (tablet) reports set the position, and button bytes are full
//! bitmasks (last-writer-wins).
//!
//! Position is tracked in framebuffer pixels; the desktop clamps it via
//! [`set_bounds`]. Init is fail-open with bounded waits: an absent mouse
//! only logs and leaves keyboard/serial input untouched.

use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const CMD_PORT: u16 = 0x64;

/// Status register: output buffer full (data ready to read).
const ST_OUT_FULL: u8 = 0x01;
/// Status register: input buffer full (controller busy, do not write).
const ST_IN_FULL: u8 = 0x02;

/// i8042 command: enable second PS/2 port (aux / mouse).
const CMD_ENABLE_AUX: u8 = 0xA8;
/// i8042 command: read controller config ("compaq status") byte.
const CMD_READ_CONFIG: u8 = 0x20;
/// i8042 command: write controller config byte.
const CMD_WRITE_CONFIG: u8 = 0x60;
/// i8042 prefix: route next data-port byte to the aux device.
const CMD_WRITE_TO_MOUSE: u8 = 0xD4;

/// Config byte bit: second-port (mouse) IRQ12 enable.
const CFG_MOUSE_IRQ: u8 = 0x02;
/// Config byte bit: second-port clock disable (0 = enabled).
const CFG_MOUSE_CLK_DISABLE: u8 = 0x20;

/// Mouse device command: reset (replies ACK then BAT-OK).
const MOUSE_RESET: u8 = 0xFF;
/// Mouse device command: set defaults.
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
/// Mouse device command: enable data reporting.
const MOUSE_ENABLE_REPORTING: u8 = 0xF4;

const MOUSE_ACK: u8 = 0xFA;
const MOUSE_BAT_OK: u8 = 0xAA;

/// Packet bit: always 1 in byte 0 (used to resync).
const PKT_ALWAYS_ONE: u8 = 0x08;
/// Packet bit: X overflow (movement data lost).
const PKT_X_OVERFLOW: u8 = 0x40;
/// Packet bit: Y overflow (movement data lost).
const PKT_Y_OVERFLOW: u8 = 0x80;

/// Queued movement events (ring buffer, power of two).
const QUEUE_SIZE: usize = 64;

/// One decoded mouse movement/button event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseDelta {
    /// Signed X movement (positive = right).
    pub dx: i16,
    /// Signed Y movement in screen pixels (positive = down).
    pub dy: i16,
    /// Button bitmask: bit0 left, bit1 right, bit2 middle.
    pub buttons: u8,
}

struct MouseState {
    present: bool,
    x: usize,
    y: usize,
    max_x: usize,
    max_y: usize,
    buttons: u8,
    packet: [u8; 3],
    packet_idx: u8,
    queue: [Option<MouseDelta>; QUEUE_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl MouseState {
    const fn new() -> Self {
        Self {
            present: false,
            x: 0,
            y: 0,
            max_x: 799,
            max_y: 599,
            buttons: 0,
            packet: [0; 3],
            packet_idx: 0,
            queue: [None; QUEUE_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, ev: MouseDelta) {
        // Drop oldest when full (cursor keeps tracking latest motion).
        if self.count >= QUEUE_SIZE {
            self.read_pos = (self.read_pos + 1) % QUEUE_SIZE;
            self.count -= 1;
        }
        self.queue[self.write_pos] = Some(ev);
        self.write_pos = (self.write_pos + 1) % QUEUE_SIZE;
        self.count += 1;
    }

    fn pop(&mut self) -> Option<MouseDelta> {
        if self.count == 0 {
            return None;
        }
        let ev = self.queue[self.read_pos];
        self.read_pos = (self.read_pos + 1) % QUEUE_SIZE;
        self.count -= 1;
        ev
    }
}

lazy_static! {
    static ref STATE: Mutex<MouseState> = Mutex::new(MouseState::new());
}

/// IRQ12 bytes received since boot.
static MOUSE_BYTES: AtomicU64 = AtomicU64::new(0);
/// Complete packets assembled since boot.
static MOUSE_PACKETS: AtomicU64 = AtomicU64::new(0);
/// Bytes/packets discarded (resync or overflow).
static MOUSE_DROPPED: AtomicU64 = AtomicU64::new(0);
/// USB HID boot-mouse reports consumed since boot.
static MOUSE_USB_PACKETS: AtomicU64 = AtomicU64::new(0);
/// USB HID tablet (absolute) reports consumed since boot.
static MOUSE_TABLET_PACKETS: AtomicU64 = AtomicU64::new(0);

/// (bytes, packets, dropped) mouse input stats. Exposed via `irqstat`.
pub fn mouse_stats() -> (u64, u64, u64) {
    (
        MOUSE_BYTES.load(Ordering::Relaxed),
        MOUSE_PACKETS.load(Ordering::Relaxed),
        MOUSE_DROPPED.load(Ordering::Relaxed),
    )
}

/// (usb mouse reports, usb tablet reports) since boot. Proves which USB
/// transport delivered movement when PS/2 and USB coexist.
pub fn mouse_usb_stats() -> (u64, u64) {
    (
        MOUSE_USB_PACKETS.load(Ordering::Relaxed),
        MOUSE_TABLET_PACKETS.load(Ordering::Relaxed),
    )
}

/// Fresh read of the i8042 config byte (side-effect free). Used by
/// `irqstat` to show IRQ1/IRQ12 enable state. `None` on timeout.
pub fn config_snapshot() -> Option<u8> {
    if !write_cmd(CMD_READ_CONFIG) {
        return None;
    }
    read_data()
}

/// Decode one 3-byte PS/2 packet.
///
/// Returns `(dx, dy_screen, buttons)` or `None` when the packet must be
/// dropped (desync marker missing or overflow). `dy_screen` is positive
/// downwards (device Y, positive upwards, is negated).
pub fn decode_packet(b0: u8, b1: u8, b2: u8) -> Option<(i16, i16, u8)> {
    if b0 & PKT_ALWAYS_ONE == 0 {
        return None;
    }
    if b0 & (PKT_X_OVERFLOW | PKT_Y_OVERFLOW) != 0 {
        return None;
    }
    let dx = b1 as i8 as i16;
    let dy = -(b2 as i8 as i16);
    let buttons = b0 & 0x07;
    Some((dx, dy, buttons))
}

fn wait_input_empty() -> bool {
    use x86_64::instructions::port::Port;
    let mut status = Port::<u8>::new(STATUS_PORT);
    for _ in 0..100_000 {
        if unsafe { status.read() } & ST_IN_FULL == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn wait_output_full() -> bool {
    use x86_64::instructions::port::Port;
    let mut status = Port::<u8>::new(STATUS_PORT);
    for _ in 0..100_000 {
        if unsafe { status.read() } & ST_OUT_FULL != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn read_data() -> Option<u8> {
    use x86_64::instructions::port::Port;
    if !wait_output_full() {
        return None;
    }
    let mut data = Port::<u8>::new(DATA_PORT);
    Some(unsafe { data.read() })
}

fn write_cmd(cmd: u8) -> bool {
    use x86_64::instructions::port::Port;
    if !wait_input_empty() {
        return false;
    }
    let mut port = Port::<u8>::new(CMD_PORT);
    unsafe { port.write(cmd) };
    true
}

fn write_data(b: u8) -> bool {
    use x86_64::instructions::port::Port;
    if !wait_input_empty() {
        return false;
    }
    let mut port = Port::<u8>::new(DATA_PORT);
    unsafe { port.write(b) };
    true
}

/// Send one byte to the mouse (via the 0xD4 aux prefix) and wait for ACK.
fn mouse_write(cmd: u8) -> bool {
    if !write_cmd(CMD_WRITE_TO_MOUSE) {
        return false;
    }
    if !write_data(cmd) {
        return false;
    }
    match read_data() {
        Some(MOUSE_ACK) => true,
        _ => false,
    }
}

/// Initializes the PS/2 mouse. Fail-open: any timeout only logs and
/// returns with the mouse marked absent.
pub fn init() {
    use crate::serial_println;
    use x86_64::instructions::port::Port;

    // Drain stale controller output so later reads are ours.
    {
        let mut status = Port::<u8>::new(STATUS_PORT);
        let mut data = Port::<u8>::new(DATA_PORT);
        for _ in 0..32 {
            if unsafe { status.read() } & ST_OUT_FULL == 0 {
                break;
            }
            let _ = unsafe { data.read() };
        }
    }

    // Enable the aux port.
    if !write_cmd(CMD_ENABLE_AUX) {
        serial_println!("[mouse] i8042 aux enable timeout, mouse absent");
        return;
    }

    // Read config, set mouse-IRQ enable + aux-clock enable.
    if !write_cmd(CMD_READ_CONFIG) {
        serial_println!("[mouse] config read timeout, mouse absent");
        return;
    }
    let Some(mut cfg) = read_data() else {
        serial_println!("[mouse] config read empty, mouse absent");
        return;
    };
    let orig_cfg = cfg;
    cfg |= CFG_MOUSE_IRQ;
    cfg &= !CFG_MOUSE_CLK_DISABLE;
    serial_println!(
        "[mouse] i8042 config {:#04x} -> {:#04x} (IRQ12 on, aux clock on)",
        orig_cfg,
        cfg
    );
    if !write_cmd(CMD_WRITE_CONFIG) || !write_data(cfg) {
        serial_println!("[mouse] config write timeout, mouse absent");
        return;
    }

    // Reset the device; expect ACK then BAT-OK (+optional device id).
    if !write_cmd(CMD_WRITE_TO_MOUSE) || !write_data(MOUSE_RESET) {
        serial_println!("[mouse] reset send timeout, mouse absent");
        return;
    }
    match read_data() {
        Some(MOUSE_ACK) => {}
        _ => {
            serial_println!("[mouse] reset without ACK, mouse absent");
            return;
        }
    }
    match read_data() {
        Some(MOUSE_BAT_OK) => {}
        _ => {
            serial_println!("[mouse] reset self-test failed, mouse absent");
            return;
        }
    }
    // Drain the optional device-id byte without blocking.
    {
        let mut status = Port::<u8>::new(STATUS_PORT);
        let mut data = Port::<u8>::new(DATA_PORT);
        for _ in 0..10_000 {
            if unsafe { status.read() } & ST_OUT_FULL == 0 {
                break;
            }
            let _ = unsafe { data.read() };
            break;
        }
    }

    if !mouse_write(MOUSE_SET_DEFAULTS) {
        serial_println!("[mouse] set-defaults failed, continuing anyway");
    }
    if !mouse_write(MOUSE_ENABLE_REPORTING) {
        serial_println!("[mouse] enable-reporting failed, mouse absent");
        return;
    }

    STATE.lock().present = true;
    serial_println!("[mouse] PS/2 mouse ready on IRQ12");
    crate::println!("[mouse] PS/2 mouse ready (run `mouse` to see it)");
}

/// Called by the IRQ12 handler with one byte read from port 0x60.
pub fn handle_byte(b: u8) {
    MOUSE_BYTES.fetch_add(1, Ordering::Relaxed);
    let mut st = STATE.lock();
    if !st.present {
        // IRQ fired before/without successful init: still try to track so
        // a late device works; mark present on first synced packet.
        st.present = true;
    }
    if st.packet_idx == 0 && b & PKT_ALWAYS_ONE == 0 {
        // Resync: drop bytes until a plausible first byte arrives.
        MOUSE_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let idx = st.packet_idx as usize;
    st.packet[idx] = b;
    st.packet_idx += 1;
    if st.packet_idx < 3 {
        return;
    }
    st.packet_idx = 0;
    let [b0, b1, b2] = st.packet;
    let Some((dx, dy, buttons)) = decode_packet(b0, b1, b2) else {
        MOUSE_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    MOUSE_PACKETS.fetch_add(1, Ordering::Relaxed);
    st.x = (st.x as isize + dx as isize).clamp(0, st.max_x as isize) as usize;
    st.y = (st.y as isize + dy as isize).clamp(0, st.max_y as isize) as usize;
    st.buttons = buttons;
    st.push(MouseDelta { dx, dy, buttons });
}

/// Push a USB HID boot-mouse report: full button bitmask plus signed
/// relative deltas (screen pixels, positive Y down). Merges into the same
/// position/buttons/queue as PS/2; callable from USB poll paths (EHCI/xHCI)
/// which already run with bounded, non-blocking discipline.
pub fn push_usb_mouse(buttons: u8, dx: i16, dy: i16) {
    use x86_64::instructions::interrupts;
    let signal = interrupts::without_interrupts(|| {
        let mut st = STATE.lock();
        st.present = true;
        let new_buttons = buttons & 0x07;
        // Re-polled identical reports carry no signal: update idempotently
        // but skip the queue/counter so idle endpoints stay quiet.
        if new_buttons == st.buttons && dx == 0 && dy == 0 {
            return false;
        }
        st.x = (st.x as isize + dx as isize).clamp(0, st.max_x as isize) as usize;
        st.y = (st.y as isize + dy as isize).clamp(0, st.max_y as isize) as usize;
        st.buttons = new_buttons;
        let btn = st.buttons;
        st.push(MouseDelta {
            dx,
            dy,
            buttons: btn,
        });
        true
    });
    if signal {
        MOUSE_USB_PACKETS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Push a USB HID tablet (absolute) report: position in device units over
/// `x_max`/`y_max`, plus full button bitmask. Scales to the current clamp
/// bounds (set by the desktop from the framebuffer size) and sets the
/// position absolutely so host and guest cursors stay glued. A movement
/// delta against the previous position is queued for event consumers.
pub fn push_usb_tablet(x_abs: u32, y_abs: u32, x_max: u32, y_max: u32, buttons: u8) {
    use x86_64::instructions::interrupts;
    let signal = interrupts::without_interrupts(|| {
        let mut st = STATE.lock();
        st.present = true;
        let new_buttons = buttons & 0x07;
        let nx = if x_max == 0 {
            st.x
        } else {
            ((x_abs as u64 * st.max_x as u64) / x_max.max(1) as u64).min(st.max_x as u64) as usize
        };
        let ny = if y_max == 0 {
            st.y
        } else {
            ((y_abs as u64 * st.max_y as u64) / y_max.max(1) as u64).min(st.max_y as u64) as usize
        };
        let dx = nx as isize - st.x as isize;
        let dy = ny as isize - st.y as isize;
        // Same no-signal filter as relative reports: a re-polled identical
        // absolute position with unchanged buttons queues nothing.
        if new_buttons == st.buttons && dx == 0 && dy == 0 {
            return false;
        }
        st.x = nx;
        st.y = ny;
        st.buttons = new_buttons;
        let btn = st.buttons;
        st.push(MouseDelta {
            dx: dx.clamp(-32768, 32767) as i16,
            dy: dy.clamp(-32768, 32767) as i16,
            buttons: btn,
        });
        true
    });
    if signal {
        MOUSE_TABLET_PACKETS.fetch_add(1, Ordering::Relaxed);
    }
}

/// True once init succeeded (or any mouse input arrived, PS/2 or USB).
pub fn is_present() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| STATE.lock().present)
}

/// Current cursor position in pixels (clamped to bounds).
pub fn position() -> (usize, usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let st = STATE.lock();
        (st.x, st.y)
    })
}

/// Current button bitmask: bit0 left, bit1 right, bit2 middle.
pub fn buttons() -> u8 {
    x86_64::instructions::interrupts::without_interrupts(|| STATE.lock().buttons)
}

/// Clamp region for the cursor (framebuffer size). Also pulls the current
/// position inside the new bounds.
pub fn set_bounds(max_x: usize, max_y: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut st = STATE.lock();
        st.max_x = max_x;
        st.max_y = max_y;
        st.x = st.x.min(max_x);
        st.y = st.y.min(max_y);
    })
}

/// Place the cursor (clamped to bounds). Used to center it on desktop entry.
pub fn set_position(x: usize, y: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut st = STATE.lock();
        st.x = x.min(st.max_x);
        st.y = y.min(st.max_y);
    })
}

/// Non-blocking: pop the oldest queued movement event, if any.
pub fn read_event() -> Option<MouseDelta> {
    x86_64::instructions::interrupts::without_interrupts(|| STATE.lock().pop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_basic_motion() {
        // Left button + dx=+5, dy device=+3 (up -> screen -3).
        let (dx, dy, btn) = decode_packet(0x09, 5, 3).unwrap();
        assert_eq!((dx, dy, btn), (5, -3, 1));
    }

    #[test]
    fn decode_negative_motion() {
        // dx=-1 (0xFF), dy device=-2 (0xFE -> screen +2), no buttons.
        let (dx, dy, btn) = decode_packet(0x08, 0xFF, 0xFE).unwrap();
        assert_eq!((dx, dy, btn), (-1, 2, 0));
    }

    #[test]
    fn decode_rejects_desync_and_overflow() {
        assert_eq!(decode_packet(0x00, 0, 0), None);
        assert_eq!(decode_packet(0x08 | 0x40, 1, 1), None);
        assert_eq!(decode_packet(0x08 | 0x80, 1, 1), None);
    }

    #[test]
    fn button_bits_pass_through() {
        let (_, _, btn) = decode_packet(0x08 | 0x07, 0, 0).unwrap();
        assert_eq!(btn, 0x07);
    }
}
