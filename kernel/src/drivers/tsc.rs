//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! TSC (Time Stamp Counter) calibration and delay.
//!
//! Used as a last-resort time source when both PIT and PM Timer are unavailable.
//! Calibrates against a reference hardware timer (PM Timer preferred) or
//! derives frequency from CPUID leaf 0x15 / 0x16.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);
static CALIBRATED: AtomicBool = AtomicBool::new(false);

/// Reads the TSC (Time Stamp Counter).
#[inline]
pub fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Tries to determine TSC frequency from CPUID leaf 0x15 (Crystal Hz)
/// Returns (freq_hz, true) if successful, (0, false) if not available.
fn try_cpuid_leaf_15() -> (u64, bool) {
    // CPUID leaf 0x15:
    // EAX = Crystal Hz denominator (if 0, leaf not supported)
    // EBX = Crystal Hz numerator
    // ECX = Crystal Hz (if non-zero)
    // EDX = TSC Hz (if non-zero, newer CPUs)
    let (eax, ebx, ecx, edx) = unsafe {
        let r = core::arch::x86_64::__cpuid_count(0x15, 0);
        (r.eax, r.ebx, r.ecx, r.edx)
    };

    // If EAX == 0, leaf 0x15 is not supported
    if eax == 0 {
        return (0, false);
    }

    // Method 1: EDX = TSC frequency directly (newer CPUs)
    if edx != 0 {
        return (edx as u64, true);
    }

    // Method 2: ECX = crystal Hz, EBX/EAX = TSC/crystal ratio
    if ecx != 0 && ebx != 0 && eax != 0 {
        // TSC frequency = ECX * EBX / EAX
        let freq = (ecx as u64) * (ebx as u64) / (eax as u64);
        if freq > 0 && freq <= 10_000_000_000 {
            return (freq, true);
        }
    }

    // Method 3: EBX = TSC frequency directly (some implementations)
    if ebx != 0 && eax != 0 && ecx == 0 {
        // Some CPUs report TSC Hz in EBX when ECX=0
        return (ebx as u64, true);
    }

    (0, false)
}

/// Tries to determine TSC frequency from CPUID leaf 0x16 (Processor Frequency)
/// Returns (freq_hz, true) if successful, (0, false) if not available.
fn try_cpuid_leaf_16() -> (u64, bool) {
    // CPUID leaf 0x16:
    // EAX = Processor Base Frequency (MHz)
    // EBX = Maximum Frequency (MHz)
    // ECX = Bus (Reference) Frequency (MHz)
    // EDX = reserved
    let (eax, ebx, ecx, _edx) = unsafe {
        let r = core::arch::x86_64::__cpuid_count(0x16, 0);
        (r.eax, r.ebx, r.ecx, r.edx)
    };

    // EAX = Processor Base Frequency in MHz
    if eax != 0 {
        return ((eax as u64) * 1_000_000, true);
    }

    // EBX = Maximum Frequency in MHz
    if ebx != 0 {
        return ((ebx as u64) * 1_000_000, true);
    }

    // ECX = Bus/Reference Frequency in MHz
    if ecx != 0 {
        return ((ecx as u64) * 1_000_000, true);
    }

    (0, false)
}

/// Tries to determine TSC frequency from CPUID leaves 0x15 and 0x16.
/// Returns (freq_hz, true) if successful, (0, false) if not available.
pub fn try_cpuid_frequency() -> (u64, bool) {
    // Try leaf 0x16 first (more direct)
    if let (freq, true) = try_cpuid_leaf_16() {
        crate::serial_println!(
            "[tsc] CPUID leaf 0x16: {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
        return (freq, true);
    }

    // Try leaf 0x15 (crystal-based)
    if let (freq, true) = try_cpuid_leaf_15() {
        crate::serial_println!(
            "[tsc] CPUID leaf 0x15: {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
        return (freq, true);
    }

    (0, false)
}

/// Calibrates TSC frequency against a reference timer.
/// `reference_sleep_ms` is called to wait for `calibration_ms` milliseconds.
/// Returns the calibrated frequency in Hz, or 0 on failure.
pub fn calibrate<F>(calibration_ms: u32, reference_sleep_ms: F) -> u64
where
    F: FnOnce(u32),
{
    if calibration_ms == 0 {
        return 0;
    }

    let tsc_start = read_tsc();
    reference_sleep_ms(calibration_ms);
    let tsc_end = read_tsc();

    let tsc_delta = tsc_end.saturating_sub(tsc_start);
    if tsc_delta == 0 {
        return 0;
    }

    // freq = tsc_delta * 1000 / calibration_ms
    let freq = tsc_delta.saturating_mul(1000) / calibration_ms as u64;

    if freq == 0 || freq > 10_000_000_000 {
        // Sanity check: >10 GHz is impossible
        return 0;
    }

    TSC_FREQ_HZ.store(freq, Ordering::Relaxed);
    CALIBRATED.store(true, Ordering::Relaxed);

    crate::serial_println!(
        "[tsc] calibrated @ {} Hz ({:.2} GHz)",
        freq,
        freq as f64 / 1_000_000_000.0
    );
    freq
}

/// Initializes TSC calibration.
/// Tries CPUID leaves 0x16/0x15 first (reference-free), falls back to calibration against reference timer.
pub fn init(reference: crate::drivers::time_source::TimeSource) {
    if CALIBRATED.load(Ordering::Relaxed) {
        return;
    }

    // Try CPUID leaves first (reference-free, works even on dead-PIT platforms)
    if let (freq, true) = try_cpuid_frequency() {
        TSC_FREQ_HZ.store(freq, Ordering::Relaxed);
        CALIBRATED.store(true, Ordering::Relaxed);
        crate::serial_println!(
            "[tsc] CPUID frequency: {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
        crate::println!(
            "[tsc] CPUID frequency: {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
        return;
    }

    // Fallback to calibration against reference timer
    crate::serial_println!("[tsc] CPUID frequency unavailable, falling back to calibration...");

    let freq = match reference {
        crate::drivers::time_source::TimeSource::PmTimer => {
            crate::serial_println!("[tsc] calibrating against PM Timer (10ms)...");
            calibrate(10, |ms| crate::drivers::pm_timer::sleep_ms(ms))
        }
        crate::drivers::time_source::TimeSource::Pit => {
            crate::serial_println!("[tsc] calibrating against PIT (10ms)...");
            calibrate(10, |ms| crate::drivers::pit::sleep_ms_pit_only(ms))
        }
        _ => {
            crate::serial_println!(
                "[tsc] WARNING: no hardware reference for calibration, skipping"
            );
            0
        }
    };

    if freq == 0 {
        crate::serial_println!("[tsc] calibration failed");
        crate::println!("[tsc] calibration failed");
    } else {
        crate::serial_println!(
            "[tsc] calibrated @ {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
        crate::println!(
            "[tsc] calibrated @ {} Hz ({:.2} GHz)",
            freq,
            freq as f64 / 1e9
        );
    }
}

/// Returns the calibrated TSC frequency in Hz.
pub fn get_freq_hz() -> u64 {
    TSC_FREQ_HZ.load(Ordering::Relaxed)
}

/// Returns true if TSC has been calibrated.
pub fn is_calibrated() -> bool {
    CALIBRATED.load(Ordering::Relaxed)
}

/// Calibrates TSC frequency against the CMOS RTC seconds edge.
///
/// Waits for the RTC seconds value to change, then measures TSC ticks
/// across one full RTC second. Bounded (~4 s max) so a dead RTC can never
/// hang boot. Returns frequency in Hz, or 0 on failure.
pub fn calibrate_via_rtc() -> u64 {
    // Wait for the current RTC second to roll over (up to ~2 s).
    let start_sec = crate::drivers::rtc::read_rtc().second;
    let mut spins: u64 = 0;
    const SPIN_BOUND: u64 = 1 << 34;
    loop {
        if crate::drivers::rtc::read_rtc().second != start_sec {
            break;
        }
        spins += 1;
        if spins >= SPIN_BOUND {
            crate::serial_println!("[tsc] RTC calibration: seconds never advanced");
            return 0;
        }
        core::hint::spin_loop();
    }
    // One full RTC second elapses here; count TSC across it.
    let tsc_start = read_tsc();
    let edge_sec = crate::drivers::rtc::read_rtc().second;
    spins = 0;
    loop {
        if crate::drivers::rtc::read_rtc().second != edge_sec {
            break;
        }
        spins += 1;
        if spins >= SPIN_BOUND {
            crate::serial_println!("[tsc] RTC calibration: second never elapsed");
            return 0;
        }
        core::hint::spin_loop();
    }
    let freq = read_tsc().saturating_sub(tsc_start);
    if freq < 1_000_000 || freq > 20_000_000_000 {
        crate::serial_println!("[tsc] RTC calibration implausible: {} Hz", freq);
        return 0;
    }
    TSC_FREQ_HZ.store(freq, Ordering::Relaxed);
    CALIBRATED.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[tsc] RTC calibrated @ {} Hz ({:.2} GHz)",
        freq,
        freq as f64 / 1_000_000_000.0
    );
    crate::println!("[time] TSC freq={} MHz (RTC)", freq / 1_000_000);
    freq
}

/// Sleeps for the specified number of milliseconds using TSC.
/// Requires successful calibration first.
pub fn sleep_ms(ms: u32) {
    if ms == 0 {
        return;
    }
    let freq = get_freq_hz();
    if freq == 0 {
        crate::serial_println!("[tsc] sleep_ms called but not calibrated, falling back to spin");
        // Fallback: spin loop with rough estimate (assume ~2 GHz)
        let cycles = (ms as u64 * 2_000_000) as u64;
        let start = read_tsc();
        while read_tsc().saturating_sub(start) < cycles {
            core::hint::spin_loop();
        }
        return;
    }

    let target = read_tsc() + (ms as u64 * freq / 1000);
    while read_tsc() < target {
        core::hint::spin_loop();
    }
}

/// Returns the TSC frequency in MHz (for display).
pub fn freq_mhz() -> u64 {
    get_freq_hz() / 1_000_000
}
