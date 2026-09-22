//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Kernel clock: monotonic ticks (PIT IRQ0) + wall clock (RTC at boot + ticks).
//!
//! - `tick()` is called from the timer interrupt handler (IRQ0).
//! - `init()` latches the RTC wall time once and zeroes the tick counter.
//! - All getters are lock-free (`AtomicU64`) so they work in IRQ context.

use crate::drivers::rtc::{self, DateTime};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Timer frequency in Hz. Must match the PIT programming in `drivers::pit`.
pub const TICK_HZ: u64 = 100;
/// Milliseconds per tick at `TICK_HZ`.
pub const MS_PER_TICK: u64 = 10;

/// Monotonic ticks since `init()` (incremented by IRQ0).
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Wall-clock epoch seconds latched from RTC at boot.
static BOOT_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);
/// Whether `init()` has run.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the clock. Reads RTC once for the boot wall time.
pub fn init() {
    let dt = rtc::read_rtc();
    let epoch = datetime_to_epoch(&dt);
    BOOT_EPOCH_SECS.store(epoch, Ordering::SeqCst);
    TICKS.store(0, Ordering::SeqCst);
    INITIALIZED.store(true, Ordering::SeqCst);
    crate::serial_println!(
        "[clock] boot wall {}.{}.{} {:02}:{:02}:{:02} (epoch {})",
        dt.day,
        dt.month,
        dt.year,
        dt.hour,
        dt.minute,
        dt.second,
        epoch
    );
}

/// Called from the timer interrupt handler. Must stay lock-free.
#[inline]
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Whether the clock has been initialized.
#[inline]
pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Relaxed)
}

/// Raw monotonic ticks since boot.
#[inline]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Timer frequency in Hz.
#[inline]
pub const fn tick_hz() -> u64 {
    TICK_HZ
}

/// Milliseconds per tick.
#[inline]
pub const fn ms_per_tick() -> u64 {
    MS_PER_TICK
}

/// Monotonic milliseconds since boot.
#[inline]
pub fn uptime_millis() -> u64 {
    ticks().wrapping_mul(MS_PER_TICK)
}

/// Monotonic seconds since boot.
#[inline]
pub fn uptime_secs() -> u64 {
    ticks() / TICK_HZ
}

/// Wall-clock seconds since Unix epoch (1970-01-01 00:00:00 UTC).
#[inline]
pub fn wall_epoch_secs() -> u64 {
    BOOT_EPOCH_SECS
        .load(Ordering::Relaxed)
        .wrapping_add(uptime_secs())
}

/// Wall-clock milliseconds since Unix epoch.
#[inline]
pub fn wall_epoch_millis() -> u64 {
    BOOT_EPOCH_SECS
        .load(Ordering::Relaxed)
        .wrapping_mul(1000)
        .wrapping_add(uptime_millis())
}

/// Current wall-clock as `DateTime` (UTC).
pub fn wall_datetime() -> DateTime {
    epoch_to_datetime(wall_epoch_secs())
}

/// Uptime broken into (days, hours, minutes, seconds).
pub fn uptime_parts() -> (u64, u64, u64, u64) {
    let s = uptime_secs();
    (s / 86_400, (s / 3600) % 24, (s / 60) % 60, s % 60)
}

// ── Epoch conversion (Howard Hinnant days_from_civil / civil_from_days) ──

/// Convert an RTC `DateTime` (UTC) to Unix epoch seconds.
pub fn datetime_to_epoch(dt: &DateTime) -> u64 {
    let days = days_from_civil(dt.year as i64, dt.month as i64, dt.day as i64);
    let secs_of_day = dt.hour as i64 * 3600 + dt.minute as i64 * 60 + dt.second as i64;
    (days * 86_400 + secs_of_day).max(0) as u64
}

/// Convert Unix epoch seconds to a `DateTime` (UTC).
pub fn epoch_to_datetime(epoch: u64) -> DateTime {
    let days = (epoch / 86_400) as i64;
    let secs_of_day = (epoch % 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    DateTime {
        year: y as u16,
        month: m as u8,
        day: d as u8,
        hour: (secs_of_day / 3600) as u8,
        minute: ((secs_of_day % 3600) / 60) as u8,
        second: (secs_of_day % 60) as u8,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
