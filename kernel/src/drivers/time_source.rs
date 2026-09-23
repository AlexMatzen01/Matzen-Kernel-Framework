//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Time source selection and unified delay API.
//!
//! At boot, we test available time sources (PIT channel 2, PM Timer, TSC)
//! and select the best one. All `delay_ms()` calls route through the
//! selected source. Provides a `time` shell command for verification.

use alloc::string::String;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::drivers::{pit, pm_timer, tsc};
use crate::{print, println, serial_println};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TimeSource {
    Unselected = 0,
    Pit = 1,
    PmTimer = 2,
    Tsc = 3,
}

static SELECTED: AtomicU8 = AtomicU8::new(TimeSource::Unselected as u8);
static SELECTION_SUMMARY: spin::Mutex<Option<String>> = spin::Mutex::new(None);

/// Returns the currently selected time source.
pub fn current() -> TimeSource {
    let v = SELECTED.load(Ordering::Relaxed);
    match v {
        1 => TimeSource::Pit,
        2 => TimeSource::PmTimer,
        3 => TimeSource::Tsc,
        _ => TimeSource::Unselected,
    }
}

/// Sets the time source (used during boot selection).
fn set_source(src: TimeSource) {
    SELECTED.store(src as u8, Ordering::Relaxed);
}

/// Returns a human-readable summary of the selected time source for VGA display.
pub fn summary() -> Option<String> {
    SELECTION_SUMMARY.lock().clone()
}

/// Runs boot-time calibration and selects the best available time source.
/// Called once after interrupts are enabled, before USB init.
/// `phys_offset` is the bootloader physical memory mapping offset for FADT parsing.
pub fn select_at_boot(phys_offset: u64) {
    if SELECTED.load(Ordering::Relaxed) != TimeSource::Unselected as u8 {
        return; // already selected
    }

    crate::serial_println!("[time] Starting time source calibration...");

    // Test 1: PIT channel 2
    let pit_ok = test_pit();
    if pit_ok {
        crate::serial_println!("[time] PIT calibration: OK — selected");
        set_source(TimeSource::Pit);
        let msg = alloc::format!("[time] src=Pit");
        *SELECTION_SUMMARY.lock() = Some(msg.clone());
        crate::println!("{}", msg);
        return;
    }

    // Test 2: PM Timer (via FADT or fallback)
    let pm_ok = test_pm_timer(phys_offset);
    if pm_ok {
        crate::serial_println!("[time] PM Timer calibration: OK — selected");
        set_source(TimeSource::PmTimer);
        let addr = pm_timer::get_address();
        let bits = if pm_timer::is_32bit() { 32 } else { 24 };
        let msg = alloc::format!("[time] src=PmTimer pm=0x{:04x}/{}", addr, bits);
        *SELECTION_SUMMARY.lock() = Some(msg.clone());
        crate::println!("{}", msg);
        return;
    }

    // Test 3: TSC. PIT and PM are both dead — try CPUID leaves, then the
    // CMOS RTC seconds edge as an absolute 1-second reference.
    let tsc_ok = test_tsc();
    if tsc_ok {
        // Prefer an exact frequency: CPUID first, RTC second.
        tsc::init(TimeSource::Unselected);
        if !tsc::is_calibrated() {
            crate::println!("[time] TSC uncalibrated, trying RTC...");
            tsc::calibrate_via_rtc();
        }
        set_source(TimeSource::Tsc);
        let freq = tsc::get_freq_hz();
        let msg = if freq != 0 {
            alloc::format!("[time] src=Tsc freq={} MHz", freq / 1_000_000)
        } else {
            alloc::format!("[time] src=Tsc freq=UNKNOWN (2GHz guess)")
        };
        crate::serial_println!("[time] TSC selected — {}", msg);
        *SELECTION_SUMMARY.lock() = Some(msg.clone());
        crate::println!("{}", msg);
        return;
    }

    // Nothing worked — this should not happen
    crate::serial_println!("[time] ERROR: No working time source found!");
    set_source(TimeSource::Tsc); // last resort
    let msg = alloc::format!("[time] src=Tsc (fallback)");
    *SELECTION_SUMMARY.lock() = Some(msg.clone());
    crate::println!("{}", msg);
}

/// Tests PIT channel 2 with a 50ms delay.
fn test_pit() -> bool {
    if !pit::ch2_reliable() {
        crate::serial_println!("[time] PIT test: SKIPPED (marked unreliable)");
        return false;
    }
    crate::serial_println!("[time] PIT test: running 50ms calibration...");
    let t0 = crate::time::ticks();
    pit::sleep_ms_pit_only(50);
    let t1 = crate::time::ticks();
    let elapsed = t1.saturating_sub(t0);
    let expected = (50 * crate::time::tick_hz()) / 1000;
    let ok = elapsed >= expected / 2 && elapsed <= expected * 2; // within 50-200%
    crate::serial_println!(
        "[time] PIT test: 50ms expected, {} ticks actual (expected ~{}) {}",
        elapsed,
        expected,
        if ok { "OK" } else { "FAIL" }
    );
    if ok {
        crate::println!("[time] PIT test: OK");
    }
    ok
}

/// Tests PM Timer with a 50ms delay using FADT for address discovery.
fn test_pm_timer(phys_offset: u64) -> bool {
    crate::serial_println!("[time] PM Timer test: initializing...");

    // Try FADT first (preferred - gets exact address and 24/32-bit info)
    if let Some(fadt) = crate::drivers::acpi::find_pm_timer_info(phys_offset) {
        crate::serial_println!(
            "[time] PM Timer test: FADT found at 0x{:04x} ({}-bit)",
            fadt.pm_timer_addr,
            if fadt.pm_timer_len == 4 { 32 } else { 24 }
        );
        if pm_timer::init_from_fadt(&fadt) {
            crate::serial_println!("[time] PM Timer test: running 50ms calibration...");
            let start = pm_timer::read_us();
            pm_timer::sleep_ms(50);
            let elapsed = pm_timer::read_us().saturating_sub(start);
            let ok = elapsed >= 25_000 && elapsed <= 100_000; // 25-100ms
            crate::serial_println!(
                "[time] PM Timer test: 50ms expected, {}us actual {} (addr 0x{:04x}, {}-bit)",
                elapsed,
                if ok { "OK" } else { "FAIL" },
                pm_timer::get_address(),
                if pm_timer::is_32bit() { 32 } else { 24 }
            );
            if ok {
                let addr = pm_timer::get_address();
                let bits = if pm_timer::is_32bit() { 32 } else { 24 };
                crate::println!(
                    "[time] PM Timer test: 50ms expected, {}us actual OK (addr 0x{:04x}, {}-bit)",
                    elapsed,
                    addr,
                    bits
                );
            }
            return ok;
        }
    }

    // Fallback to hardcoded addresses
    crate::serial_println!("[time] PM Timer test: FADT failed, trying fallback addresses...");
    if pm_timer::try_fallback_init() {
        crate::serial_println!("[time] PM Timer test: running 50ms calibration...");
        let start = pm_timer::read_us();
        pm_timer::sleep_ms(50);
        let elapsed = pm_timer::read_us().saturating_sub(start);
        let ok = elapsed >= 25_000 && elapsed <= 100_000; // 25-100ms
        crate::serial_println!(
            "[time] PM Timer test: 50ms expected, {}us actual {} (fallback addr 0x{:04x})",
            elapsed,
            if ok { "OK" } else { "FAIL" },
            pm_timer::get_address()
        );
        if ok {
            crate::println!(
                "[time] PM Timer test: 50ms expected, {}us actual OK (fallback addr 0x{:04x})",
                elapsed,
                pm_timer::get_address()
            );
        }
        return ok;
    }

    crate::serial_println!("[time] PM Timer test: FAIL (no working address)");
    crate::println!("[time] PM Timer test: FAIL (no working address — FADT missing/invalid, fallbacks exhausted)");
    false
}

/// Tests TSC by calibrating against... we don't have a reference.
/// Just checks that TSC advances.
fn test_tsc() -> bool {
    crate::serial_println!("[time] TSC test: checking if TSC advances...");
    let t0 = tsc::read_tsc();
    // Spin for a bit
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
    let t1 = tsc::read_tsc();
    let ok = t1 > t0;
    crate::serial_println!(
        "[time] TSC test: {} (advances: {})",
        if ok { "OK" } else { "FAIL" },
        ok
    );
    if ok {
        let freq = tsc::get_freq_hz();
        crate::println!("[time] TSC test: OK (advances, freq={} Hz)", freq);
    }
    ok
}

/// Returns true if a time source has been selected.
pub fn is_selected() -> bool {
    SELECTED.load(Ordering::Relaxed) != TimeSource::Unselected as u8
}

/// Unified delay API — routes to the selected time source.
pub fn delay_ms(ms: u32) {
    match current() {
        TimeSource::Pit => pit::sleep_ms(ms),
        TimeSource::PmTimer => pm_timer::sleep_ms(ms),
        TimeSource::Tsc => tsc::sleep_ms(ms),
        TimeSource::Unselected => {
            select_at_boot(0);
            delay_ms(ms);
        }
    }
}

/// Gets calibration results for the `time` shell command.
pub fn get_calibration_results() -> CalibrationResults {
    CalibrationResults {
        selected: current(),
        pit_ok: test_pit(),
        pm_timer_ok: test_pm_timer(0),
        tsc_ok: test_tsc(),
        pit_addr: if pit::ch2_reliable() {
            Some("0x40/0x42 (ch0/ch2)".into())
        } else {
            None
        },
        pm_timer_addr: if pm_timer::is_available() {
            Some(alloc::format!(
                "0x{:04x} ({}-bit)",
                pm_timer::get_address(),
                if pm_timer::is_32bit() { 32 } else { 24 }
            ))
        } else {
            None
        },
        tsc_freq: if tsc::is_calibrated() {
            Some(tsc::get_freq_hz())
        } else {
            None
        },
    }
}

#[derive(Debug, Clone)]
pub struct CalibrationResults {
    pub selected: TimeSource,
    pub pit_ok: bool,
    pub pm_timer_ok: bool,
    pub tsc_ok: bool,
    pub pit_addr: Option<alloc::string::String>,
    pub pm_timer_addr: Option<alloc::string::String>,
    pub tsc_freq: Option<u64>,
}
