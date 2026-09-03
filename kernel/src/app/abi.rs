//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! App ABI - stable syscall numbers and helpers for app execution

/// Syscall numbers for MFKE bytecode VM and future native apps
/// Keep stable - docs and SDK must match
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNr {
    Exit = 0,
    PrintStr = 1,
    PrintInt = 2,
    Yield = 3,
    Sleep = 4,
    GetTick = 5,
    FsCreate = 10,
    FsWrite = 11,
    FsRead = 12,
    FsDelete = 13,
    FsList = 14,
    FsMkdir = 15,
}

impl SyscallNr {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Exit),
            1 => Some(Self::PrintStr),
            2 => Some(Self::PrintInt),
            3 => Some(Self::Yield),
            4 => Some(Self::Sleep),
            5 => Some(Self::GetTick),
            10 => Some(Self::FsCreate),
            11 => Some(Self::FsWrite),
            12 => Some(Self::FsRead),
            13 => Some(Self::FsDelete),
            14 => Some(Self::FsList),
            15 => Some(Self::FsMkdir),
            _ => None,
        }
    }
}

/// C-compatible syscall table for future native apps (ring 0 call style)
/// Version = 1
#[repr(C)]
pub struct SyscallTable {
    pub version: u32,
    pub print: extern "C" fn(*const u8, usize),
    pub print_int: extern "C" fn(i64),
    pub exit: extern "C" fn(i32) -> !,
    pub yield_now: extern "C" fn(),
    pub get_tick: extern "C" fn() -> u64,
}

extern "C" fn syscall_print(ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 { return; }
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        crate::print!("{}", s);
    }
}

extern "C" fn syscall_print_int(v: i64) {
    crate::println!("{}", v);
}

extern "C" fn syscall_exit(code: i32) -> ! {
    // In Phase 1 native mode, exit is cooperative -> just panic-like loop
    // But for VM bytecodes we return code; for native call we diverge
    crate::println!("\n[app exited with code {}]", code);
    // Return to caller by not diverging - we need a longjmp. For now halt.
    // Native apps will be rewritten to use this table inside VM only.
    // This entry should not be used for bytecode VM (handled inline).
    loop { x86_64::instructions::hlt(); }
}

extern "C" fn syscall_yield_now() {
    crate::shell::increment_tick();
    crate::net::process_packets();
    core::hint::spin_loop();
}

extern "C" fn syscall_get_tick() -> u64 {
    crate::shell::get_tick_count()
}

pub fn global_table() -> SyscallTable {
    SyscallTable {
        version: 1,
        print: syscall_print,
        print_int: syscall_print_int,
        exit: syscall_exit,
        yield_now: syscall_yield_now,
        get_tick: syscall_get_tick,
    }
}
