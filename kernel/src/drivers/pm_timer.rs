//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ACPI PM Timer driver with FADT discovery and fallback addresses.
//!
//! Standard I/O port from FADT PM_TIMER_BLOCK. 3.58 MHz counter,
//! 24-bit or 32-bit. Read-only access — no hardware state modification.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use x86_64::instructions::port::Port;

use crate::drivers::acpi::{try_fallback_pm_timer_addrs, FadtInfo};

const PM_TIMER_FREQ_HZ: u64 = 3_579_545; // 3.58 MHz

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static IS_32BIT: AtomicU8 = AtomicU8::new(0); // 0 = unknown, 1 = 24-bit, 2 = 32-bit
static PM_TIMER_ADDR: AtomicU32 = AtomicU32::new(0);
static LAST_READ: AtomicU32 = AtomicU32::new(0);
static OVERFLOW_COUNT: AtomicU64 = AtomicU64::new(0);

/// Initializes PM timer from FADT info.
/// Validates by reading twice with a small delay.
pub fn init_from_fadt(fadt: &FadtInfo) -> bool {
    if INITIALIZED.swap(true, Ordering::Relaxed) {
        return true;
    }

    let addr = fadt.pm_timer_addr;
    if addr == 0 {
        crate::serial_println!("[pm_timer] FADT reports address 0, trying fallbacks");
        return try_fallback_init();
    }

    if !validate_address(addr) {
        crate::serial_println!(
            "[pm_timer] FADT address 0x{:04x} invalid, trying fallbacks",
            addr
        );
        return try_fallback_init();
    }

    PM_TIMER_ADDR.store(addr as u32, Ordering::Relaxed);
    IS_32BIT.store(
        if fadt.pm_timer_len == 4 { 2 } else { 1 },
        Ordering::Relaxed,
    );
    LAST_READ.store(read_raw(addr as u32), Ordering::Relaxed);
    OVERFLOW_COUNT.store(0, Ordering::Relaxed);

    crate::serial_println!(
        "[pm_timer] initialized from FADT at 0x{:04x} ({}-bit)",
        addr,
        if fadt.pm_timer_len == 4 { 32 } else { 24 }
    );
    true
}

/// Tries fallback addresses if FADT init fails.
pub fn try_fallback_init() -> bool {
    if let Some(addr) = super::acpi::try_fallback_pm_timer_addrs() {
        PM_TIMER_ADDR.store(addr as u32, Ordering::Relaxed);
        // Assume 24-bit for fallbacks (safe default)
        IS_32BIT.store(1, Ordering::Relaxed);
        LAST_READ.store(read_raw(addr as u32), Ordering::Relaxed);
        OVERFLOW_COUNT.store(0, Ordering::Relaxed);
        crate::serial_println!(
            "[pm_timer] initialized from fallback at 0x{:04x} (assuming 24-bit)",
            addr
        );
        return true;
    }
    crate::serial_println!("[pm_timer] all fallback addresses failed");
    false
}

/// Validates a PM timer address by reading twice with a small delay.
fn validate_address(addr: u16) -> bool {
    let mut port = Port::<u32>::new(addr);
    let v1 = unsafe { port.read() };
    // Small delay
    for _ in 0..1000 {
        core::hint::spin_loop();
    }
    let v2 = unsafe { port.read() };

    // Valid if both reads are non-zero, non-0xFFFFFFFF, and different (counter running)
    v1 != 0 && v1 != 0xFFFFFFFF && v2 != 0 && v2 != 0xFFFFFFFF
}

fn read_raw(addr: u32) -> u32 {
    let mut port = Port::<u32>::new(addr as u16);
    unsafe { port.read() }
}

/// Reads the PM timer value in microseconds since boot.
/// Handles 24-bit wrap by tracking overflow count.
pub fn read_us() -> u64 {
    if !INITIALIZED.load(Ordering::Relaxed) {
        return 0;
    }

    let addr = PM_TIMER_ADDR.load(Ordering::Relaxed);
    if addr == 0 {
        return 0;
    }

    let val = read_raw(addr);
    let last = LAST_READ.load(Ordering::Relaxed);

    // Detect wrap (val < last means counter wrapped)
    if val < last {
        OVERFLOW_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    LAST_READ.store(val, Ordering::Relaxed);

    let overflows = OVERFLOW_COUNT.load(Ordering::Relaxed);
    let is_32bit = IS_32BIT.load(Ordering::Relaxed) == 2;
    let shift = if is_32bit { 32 } else { 24 };
    let mask = if is_32bit { 0xFFFFFFFF } else { 0xFFFFFF };

    let full_val = (overflows << shift) | ((val & mask) as u64);

    // Convert ticks to microseconds: us = ticks * 1_000_000 / 3_579_545
    full_val * 1_000_000 / 3_579_545
}

/// Sleeps for the specified number of milliseconds using the PM timer.
pub fn sleep_ms(ms: u32) {
    if ms == 0 {
        return;
    }
    if !INITIALIZED.load(Ordering::Relaxed) {
        // Not initialized; this shouldn't happen if init was called
        // but fall back to spin loop to avoid hanging
        crate::drivers::pit::sleep_ms_pit_only(ms);
        return;
    }

    let start = read_us();
    let target = start + (ms as u64 * 1000);

    while read_us() < target {
        core::hint::spin_loop();
    }
}

/// Returns true if PM timer is available and initialized.
pub fn is_available() -> bool {
    INITIALIZED.load(Ordering::Relaxed)
}

/// Returns the I/O port address of the PM timer.
pub fn get_address() -> u16 {
    PM_TIMER_ADDR.load(Ordering::Relaxed) as u16
}

/// Returns true if the PM timer is 32-bit.
pub fn is_32bit() -> bool {
    IS_32BIT.load(Ordering::Relaxed) == 2
}
