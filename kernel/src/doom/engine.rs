//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Rust bridge to the vendored doomgeneric engine (see vendor/doomgeneric,
//! built by kernel/build.rs from kernel/doomc platform sources).
//!
//! - `start(wad)` validates the WAD, enables SSE (the C engine uses FPU),
//!   and runs `doomgeneric_Create` guarded so `I_Error`/`exit()` abort
//!   back instead of trapping the desktop.
//! - `tick()` runs one `doomgeneric_Tick` (35Hz pacing done by caller).
//! - `push_doomkey()` feeds the C key queue (`DG_GetKey` drains it).
//! - `copy_frame_rgba()` copies the 320x200x32 `DG_ScreenBuffer`.
//! - Without the `doomc_built` cfg (host tests, missing clang) every
//!   call fails cleanly with "engine not compiled".

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uchar};
use spin::Mutex;

/// Engine frame size (matches -DDOOMGENERIC_RESX/RESY=320/200).
pub const FRAME_W: usize = 320;
/// Engine frame height.
pub const FRAME_H: usize = 200;
/// Engine pixels per frame.
pub const FRAME_PIXELS: usize = FRAME_W * FRAME_H;

// Doom key codes (doomkeys.h) emitted into the engine queue.
pub const KEY_UP: u8 = 0xAD;
/// Down arrow.
pub const KEY_DOWN: u8 = 0xAF;
/// Left arrow.
pub const KEY_LEFT: u8 = 0xAC;
/// Right arrow.
pub const KEY_RIGHT: u8 = 0xAE;
/// Fire (Ctrl).
pub const KEY_FIRE: u8 = 0xA3;
/// Use (Space in default bindings).
pub const KEY_USE: u8 = b' ';
/// Strafe modifier (Alt).
pub const KEY_STRAFE: u8 = 0xB8;
/// Run modifier (Shift).
pub const KEY_RUN: u8 = 0xB6;
/// Menu / escape.
pub const KEY_ESC: u8 = 27;
/// Enter.
pub const KEY_ENTER: u8 = 13;

/// Map a desktop-level [`crate::doom::DoomKey`] to engine key code(s).
/// WASD arrive as arrows (default bindings have no WASD).
pub fn doomkey_code(key: crate::doom::DoomKey) -> u8 {
    match key {
        crate::doom::DoomKey::Forward => KEY_UP,
        crate::doom::DoomKey::Back => KEY_DOWN,
        crate::doom::DoomKey::Left => KEY_LEFT,
        crate::doom::DoomKey::Right => KEY_RIGHT,
        crate::doom::DoomKey::Fire => KEY_FIRE,
        crate::doom::DoomKey::Use => KEY_USE,
        crate::doom::DoomKey::Strafe => KEY_STRAFE,
        crate::doom::DoomKey::StrafeLeft => b',',
        crate::doom::DoomKey::StrafeRight => b'.',
        crate::doom::DoomKey::Run => KEY_RUN,
        crate::doom::DoomKey::Menu => KEY_ESC,
        crate::doom::DoomKey::Weapon(n) => b'0' + n.min(9),
    }
}

// ---- Key queue (Rust side; C drains via mfk_pop_key) ----------------------
const KEYQ_LEN: usize = 64;

struct KeyQueue {
    keys: [u8; KEYQ_LEN],
    pressed: [bool; KEYQ_LEN],
    head: usize,
    tail: usize,
}

static KEYQ: Mutex<KeyQueue> = Mutex::new(KeyQueue {
    keys: [0; KEYQ_LEN],
    pressed: [false; KEYQ_LEN],
    head: 0,
    tail: 0,
});

/// Push an engine key event (called from desktop input routing).
pub fn push_key(code: u8, pressed: bool) {
    let mut q = KEYQ.lock();
    let head = q.head;
    let next = (head + 1) % KEYQ_LEN;
    if next == q.tail {
        return; // full: drop, never block
    }
    q.keys[head] = code;
    q.pressed[head] = pressed;
    q.head = next;
}

/// Push a desktop-level DoomKey (press+release pairs handled by caller).
pub fn push_doomkey(key: crate::doom::DoomKey, pressed: bool) {
    push_key(doomkey_code(key), pressed);
}

/// Drain the queue (used by host tests).
#[cfg(test)]
pub fn drain_queue() -> Vec<(u8, bool)> {
    let mut q = KEYQ.lock();
    let mut out = Vec::new();
    while q.tail != q.head {
        out.push((q.keys[q.tail], q.pressed[q.tail]));
        q.tail = (q.tail + 1) % KEYQ_LEN;
    }
    out
}

// ---- WAD path --------------------------------------------------------------
// Lock-free static buffer: set from shell/desktop context, read
// transiently by C (`D_FindIWAD` duplicates it immediately). No IRQ
// handler touches it, and the engine runs synchronously, so sharing
// the pointer across the FFI boundary is sound here.
const WAD_PATH_MAX: usize = 128;

static mut WAD_PATH_BUF: [u8; WAD_PATH_MAX] = [0; WAD_PATH_MAX];

fn set_wad_path(path: &str) {
    let b = path.as_bytes();
    let n = b.len().min(WAD_PATH_MAX - 1);
    unsafe {
        WAD_PATH_BUF[..n].copy_from_slice(&b[..n]);
        WAD_PATH_BUF[n] = 0;
    }
}

// ---- C exports (mfk_defs.h) ------------------------------------------------

/// Guest file size (<0 on error).
#[no_mangle]
pub extern "C" fn mfk_fs_size(path: *const c_char) -> i64 {
    let p = match c_path_str(path) {
        Some(s) => s,
        None => return -1,
    };
    if !crate::shell::is_mounted() {
        return -1;
    }
    match crate::shell::mounted_file_size(p) {
        Ok(n) => n.min(i64::MAX as u64) as i64,
        Err(_) => -1,
    }
}

/// Read `len` bytes at `offset` into `buf`. Returns bytes read (<0 error).
#[no_mangle]
pub extern "C" fn mfk_fs_read(
    path: *const c_char,
    offset: u64,
    buf: *mut u8,
    len: u64,
) -> i64 {
    let p = match c_path_str(path) {
        Some(s) => s,
        None => return -1,
    };
    if buf.is_null() || len == 0 {
        return 0;
    }
    if !crate::shell::is_mounted() {
        return -1;
    }
    let out = unsafe { core::slice::from_raw_parts_mut(buf, len as usize) };
    let mut device = crate::shell::mounted_device();
    match crate::shell::read_file_chunk(p, &mut device, offset, out) {
        Ok(n) => n as i64,
        Err(_) => -1,
    }
}

/// Write `len` bytes (truncate/create). Returns bytes written (<0 error).
#[no_mangle]
pub extern "C" fn mfk_fs_write(path: *const c_char, buf: *const u8, len: u64) -> i64 {
    let p = match c_path_str(path) {
        Some(s) => s,
        None => return -1,
    };
    if buf.is_null() {
        return -1;
    }
    if !crate::shell::is_mounted() {
        return -1;
    }
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    let mut device = crate::shell::mounted_device();
    match crate::shell::write_file_contents(p, data, &mut device) {
        Ok(()) => len as i64,
        Err(_) => -1,
    }
}

/// Serial debug sink for engine printf.
#[no_mangle]
pub extern "C" fn mfk_debug_write(buf: *const u8, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf, len) };
    for chunk in bytes.chunks(64) {
        match core::str::from_utf8(chunk) {
            Ok(s) => crate::drivers::serial::_print(format_args!("{}", s)),
            Err(_) => {
                for &b in chunk {
                    crate::drivers::serial::_print(format_args!("<{:02X}>", b));
                }
            }
        }
    }
}

/// Monotonic milliseconds.
#[no_mangle]
pub extern "C" fn mfk_ticks_ms() -> u64 {
    crate::time::uptime_millis()
}

/// Cooperative sleep.
#[no_mangle]
pub extern "C" fn mfk_sleep_ms(ms: u64) {
    crate::drivers::pit::sleep_ms(ms.min(u32::MAX as u64) as u32);
}

/// Kernel heap allocation for the engine (zone + malloc).
#[no_mangle]
pub extern "C" fn mfk_alloc(size: usize) -> *mut u8 {
    let size = size.max(1);
    let layout = match core::alloc::Layout::from_size_align(size, 8) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    unsafe { alloc::alloc::alloc(layout) }
}

/// Zeroed allocation.
#[no_mangle]
pub extern "C" fn mfk_calloc(n: usize, size: usize) -> *mut u8 {
    let total = n.saturating_mul(size).max(1);
    let layout = match core::alloc::Layout::from_size_align(total, 8) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    unsafe { alloc::alloc::alloc_zeroed(layout) }
}

/// Reallocation (old size unknown: copy min(old,new) is impossible, so
/// allocate+copy-up-to-new then free — callers must not rely on growth
/// preserving beyond the smaller size; engine realloc use is rare).
#[no_mangle]
pub extern "C" fn mfk_realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    if ptr.is_null() {
        return mfk_alloc(size);
    }
    if size == 0 {
        mfk_free(ptr);
        return core::ptr::null_mut();
    }
    // Best effort: allocate new block. Precise old-size tracking would
    // need a header; the engine only reallocs a few config buffers, so
    // copy a bounded prefix (64KB) to avoid over-read.
    let fresh = mfk_alloc(size);
    if !fresh.is_null() {
        let n = size.min(64 * 1024);
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, fresh, n);
        }
        mfk_free(ptr);
    }
    fresh
}

/// Free (no-op for null).
#[no_mangle]
pub extern "C" fn mfk_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // Size/alignment unknown: deallocate a maximal page-sized layout.
    // LockedHeap (linked_list_allocator) ignores excess on dealloc as
    // long as alignment matches; use align 8 to match alloc path.
    //
    // NOTE: linked_list_allocator requires the *exact* layout on free.
    // Since we cannot know it here, engine allocations are intentionally
    // never freed back except via this 1-byte-layout-compatible path is
    // WRONG — instead leak: the engine lives as long as the window and
    // its total footprint (~20MB) fits the heap budget. Leak-by-design,
    // documented: engines are single-shot per window.
    let _ = ptr;
}

/// Duplicate a C string via the kernel heap (never freed: see mfk_free).
#[no_mangle]
pub extern "C" fn mfk_strdup(s: *const c_char) -> *mut c_char {
    if s.is_null() {
        return core::ptr::null_mut();
    }
    let mut n = 0usize;
    while n < 4096 && unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    let out = mfk_alloc(n + 1);
    if out.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(s as *const u8, out, n);
        *out.add(n) = 0;
    }
    out as *mut c_char
}

/// C key-queue drain for `DG_GetKey`. Returns 1 when an event was popped.
#[no_mangle]
pub extern "C" fn mfk_pop_key(pressed: *mut c_int, key: *mut c_uchar) -> c_int {
    if pressed.is_null() || key.is_null() {
        return 0;
    }
    let mut q = KEYQ.lock();
    if q.tail == q.head {
        return 0;
    }
    unsafe {
        *key = q.keys[q.tail];
        *pressed = if q.pressed[q.tail] { 1 } else { 0 };
    }
    q.tail = (q.tail + 1) % KEYQ_LEN;
    1
}

/// Active WAD path for C (`mfk_wad_path`). NUL-terminated static buffer.
#[no_mangle]
pub extern "C" fn mfk_wad_path() -> *const c_char {
    unsafe { WAD_PATH_BUF.as_ptr() as *const c_char }
}

/// Read a NUL-terminated C path (bounded 128 bytes) as &str.
fn c_path_str<'a>(path: *const c_char) -> Option<&'a str> {
    if path.is_null() {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(path as *const u8, WAD_PATH_MAX) };
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(WAD_PATH_MAX);
    core::str::from_utf8(&bytes[..n]).ok()
}

// ---- Engine lifecycle ------------------------------------------------------

/// Engine state tracked on the Rust side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// Never started.
    Idle,
    /// Running (ticks accepted).
    Running,
    /// Quit requested by the game (I_Quit/exit(0)).
    Quit,
    /// Fatal error; message in `last_error()`.
    Fatal,
}

static STATE: Mutex<EngineState> = Mutex::new(EngineState::Idle);
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// Current engine state.
pub fn state() -> EngineState {
    *STATE.lock()
}

/// Last fatal message (valid after [`EngineState::Fatal`]).
pub fn last_error() -> String {
    LAST_ERROR.lock().clone()
}

fn set_fatal(msg: &str) {
    *LAST_ERROR.lock() = String::from(msg);
    *STATE.lock() = EngineState::Fatal;
}

#[cfg(doomc_built)]
mod ffi {
    use core::ffi::{c_char, c_int};
    extern "C" {
        pub fn mfk_engine_create(argc: c_int, argv: *const *const c_char) -> c_int;
        pub fn mfk_engine_tick() -> c_int;
        pub fn mfk_abort_code() -> c_int;
        pub fn mfk_abort_msg() -> *const c_char;
        pub static mut DG_ScreenBuffer: *mut u32;
    }
}

/// Enable SSE/FXSR for the C engine (idempotent). Rust code is
/// soft-float and never touches XMM, and IRQ handlers preserve it by
/// not using it, so enabling once is safe for synchronous engine calls.
#[cfg(doomc_built)]
fn enable_sse() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR);
            f.insert(Cr0Flags::MONITOR_COPROCESSOR);
        });
        Cr4::update(|f| {
            f.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        });
    }
}

/// Start the engine on `wad` (guest path, e.g. `/wad/doom1.wad`).
/// Single-shot per window: call [`stop`] before starting again.
#[cfg(doomc_built)]
pub fn start(wad: &str) -> Result<(), String> {
    // Validate the WAD through the streaming inspector first (cheap,
    // no whole-file load) so a missing file never enters C code.
    let info = crate::doom::wad_info(wad, 1).map_err(|e| String::from(e))?;
    if info.num_lumps == 0 {
        return Err(String::from("WAD has no lumps"));
    }
    enable_sse();
    set_wad_path(wad);
    // Clear stale input.
    {
        let mut q = KEYQ.lock();
        q.head = 0;
        q.tail = 0;
    }
    *STATE.lock() = EngineState::Running;
    let arg0 = c"doom";
    let argv: [*const c_char; 1] = [arg0.as_ptr()];
    let rc = unsafe { ffi::mfk_engine_create(1, argv.as_ptr()) };
    if rc != 0 {
        let msg = abort_message();
        set_fatal(&msg);
        return Err(msg);
    }
    Ok(())
}

/// Run one engine tick. Returns `false` when the engine quit or died
/// (check [`state`] / [`last_error`]).
#[cfg(doomc_built)]
pub fn tick() -> bool {
    if state() != EngineState::Running {
        return false;
    }
    let rc = unsafe { ffi::mfk_engine_tick() };
    if rc != 0 {
        let code = unsafe { ffi::mfk_abort_code() };
        if code == 0 {
            *STATE.lock() = EngineState::Quit;
        } else {
            set_fatal(&abort_message());
        }
        return false;
    }
    true
}

/// Copy the current 320x200x32 frame into `out` (must hold FRAME_PIXELS
/// u32 entries). No-op unless running.
#[cfg(doomc_built)]
pub fn copy_frame(out: &mut [u32]) -> bool {
    if state() != EngineState::Running || out.len() < FRAME_PIXELS {
        return false;
    }
    unsafe {
        let src = ffi::DG_ScreenBuffer as *const u32;
        if src.is_null() {
            return false;
        }
        core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), FRAME_PIXELS);
    }
    true
}

/// Stop the engine (quit flag; the C heap is intentionally not reclaimed
/// — see `mfk_free` — until reboot; each window start reuses one zone).
#[cfg(doomc_built)]
pub fn stop() {
    *STATE.lock() = EngineState::Idle;
}

#[cfg(doomc_built)]
fn abort_message() -> String {
    let code = unsafe { ffi::mfk_abort_code() };
    let ptr = unsafe { ffi::mfk_abort_msg() };
    let msg = if ptr.is_null() {
        String::new()
    } else {
        c_path_str(ptr).unwrap_or("").into()
    };
    if msg.is_empty() {
        if code == 0 {
            String::from("quit")
        } else {
            String::from("engine aborted")
        }
    } else {
        msg
    }
}

// ---- Fallbacks when the C engine was not compiled --------------------------

/// Start stub (host tests / missing clang).
#[cfg(not(doomc_built))]
pub fn start(_wad: &str) -> Result<(), String> {
    Err(String::from("Doom engine not compiled into this build"))
}

/// Tick stub.
#[cfg(not(doomc_built))]
pub fn tick() -> bool {
    false
}

/// Frame stub.
#[cfg(not(doomc_built))]
pub fn copy_frame(_out: &mut [u32]) -> bool {
    false
}

/// Stop stub.
#[cfg(not(doomc_built))]
pub fn stop() {}

/// Whether the native engine is linked in.
pub fn available() -> bool {
    cfg!(doomc_built)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_push_pop_roundtrip() {
        drain_queue();
        push_key(KEY_UP, true);
        push_key(b'a', false);
        assert_eq!(drain_queue(), alloc::vec![(KEY_UP, true), (b'a', false)]);
    }

    #[test]
    fn doomkey_codes_match_bindings() {
        assert_eq!(doomkey_code(crate::doom::DoomKey::Forward), KEY_UP);
        assert_eq!(doomkey_code(crate::doom::DoomKey::Fire), KEY_FIRE);
        assert_eq!(doomkey_code(crate::doom::DoomKey::Menu), KEY_ESC);
        assert_eq!(
            doomkey_code(crate::doom::DoomKey::Weapon(3)),
            b'0' + 3
        );
    }

    #[cfg(not(doomc_built))]
    #[test]
    fn engine_stub_reports_unavailable_on_host() {
        assert!(!available());
        assert!(start("/wad/doom1.wad").is_err());
    }
}
