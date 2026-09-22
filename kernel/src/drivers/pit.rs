//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Intel 8253/8254 Programmable Interval Timer (PIT) Driver
//!
//! Channel 0 is wired to IRQ0 and drives the kernel clock.
//! We program it for square-wave mode (mode 3) at a fixed frequency
//! (default 100 Hz) so `crate::time` gets a stable monotonic tick.
//!
//! Channel 2 (speaker timer) provides real-time delays via one-shot
//! mode 0 polling. On UEFI platforms where the legacy PIT is not
//! available, we detect this and fall back to PM Timer or TSC.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

/// PIT channel 0 data port
const PIT_CHANNEL0: u16 = 0x40;
/// PIT mode/command register
const PIT_COMMAND: u16 = 0x43;

/// PIT base input clock: 1.193182 MHz
pub const PIT_BASE_FREQUENCY: u32 = 1_193_182;

/// Default timer frequency in Hz (10 ms per tick).
pub const PIT_DEFAULT_HZ: u32 = 100;

/// Program channel 0, lobyte/hibyte, mode 3 (square wave), binary.
const PIT_CMD_CH0_LOHI_MODE3: u8 = 0x36;

/// Global: PIT channel 2 reliability flag.
/// Once set to false, we never try PIT channel 2 again.
static PIT_CH2_RELIABLE: AtomicBool = AtomicBool::new(true);

/// Checks if PIT channel 2 is still considered reliable.
pub fn ch2_reliable() -> bool {
    PIT_CH2_RELIABLE.load(Ordering::Relaxed)
}

/// Marks PIT channel 2 as unreliable (e.g., OUT never transitions).
/// Once marked, never cleared.
pub fn mark_ch2_unreliable() {
    PIT_CH2_RELIABLE.store(false, Ordering::Relaxed);
}

/// Initialize the PIT to fire IRQ0 at `freq_hz`.
///
/// # Safety
/// Programs hardware ports; must be called once before unmasking IRQ0.
pub fn init(freq_hz: u32) {
    let freq = freq_hz.clamp(1, PIT_BASE_FREQUENCY);
    let divisor = (PIT_BASE_FREQUENCY / freq) as u16;
    // Divisor 0 means 65536 in hardware; avoid accidental 0 from rounding.
    let divisor = if divisor == 0 { 1 } else { divisor };

    unsafe {
        let mut cmd: Port<u8> = Port::new(PIT_COMMAND);
        let mut ch0: Port<u8> = Port::new(PIT_CHANNEL0);
        cmd.write(PIT_CMD_CH0_LOHI_MODE3);
        ch0.write((divisor & 0xFF) as u8);
        ch0.write((divisor >> 8) as u8);
    }

    crate::serial_println!(
        "[pit] channel0 @ {} Hz (divisor {})",
        PIT_BASE_FREQUENCY / divisor as u32,
        divisor
    );
}

/// Convenience: init at the kernel default rate.
pub fn init_default() {
    init(PIT_DEFAULT_HZ);
}

// ---------------------------------------------------------------------------
// Pre-timer delays: PIT channel 2 (speaker timer), polled one-shot mode
// ---------------------------------------------------------------------------
//
// Channel 0 drives IRQ0, which is unavailable before `sti`, and spin-loop
// calibration is useless under emulation (guest instructions run far slower
// than the host). Channel 2 in mode 0 (interrupt on terminal count) gives
// real-time delays anywhere: program a count, poll OUT via port 0x61 bit 5.
// Only the gate bit (0x61 bit 0) is ever set; all other bits are preserved,
// so the speaker stays silent and nothing else is disturbed.
//
// If no PIT decodes these ports (reads return 0xFF), every wait degrades to
// an immediate timeout instead of hanging: callers report missing hardware
// and boot continues.

/// PIT channel 2 data port.
const PIT_CHANNEL2: u16 = 0x42;
/// Speaker control port (channel-2 gate + OUT status).
const SPEAKER_CTRL: u16 = 0x61;

/// 0x61 bit 5: channel-2 OUT status (1 = terminal count reached).
const SPK_CH2_OUT: u8 = 0x20;
/// 0x61 bit 0: channel-2 gate enable. Only this bit is ever modified.
const SPK_CH2_GATE: u8 = 0x01;

/// Program channel 2, lobyte/hibyte, mode 0 (one-shot), binary.
const PIT_CMD_CH2_LOHI_MODE0: u8 = 0xB0;

/// PIT input clocks per millisecond.
const PIT_COUNTS_PER_MS: u32 = PIT_BASE_FREQUENCY / 1000;

/// Longest single channel-2 shot: 0xFFFF counts at ~1193/ms.
const CH2_MAX_MS: u32 = 54;

/// Arm a one-shot countdown of up to `CH2_MAX_MS` milliseconds.
/// Returns false when no PIT is present (OUT never drops): callers must
/// fail open instead of waiting.
/// Verifies OUT actually goes LOW after programming.
fn ch2_arm(chunk_ms: u32) -> bool {
    if !PIT_CH2_RELIABLE.load(Ordering::Relaxed) {
        return false;
    }
    let chunk = chunk_ms.clamp(1, CH2_MAX_MS);
    let count = ((chunk * PIT_COUNTS_PER_MS).max(1).min(0xFFFF)) as u16;
    unsafe {
        let mut cmd: Port<u8> = Port::new(PIT_COMMAND);
        let mut ch2: Port<u8> = Port::new(PIT_CHANNEL2);
        let mut spk: Port<u8> = Port::new(SPEAKER_CTRL);
        let s = spk.read();
        spk.write(s | SPK_CH2_GATE);
        cmd.write(PIT_CMD_CH2_LOHI_MODE0);
        ch2.write((count & 0xFF) as u8);
        ch2.write((count >> 8) as u8);
        // Verify OUT goes LOW (counting). A fresh one-shot drives OUT low
        // for the whole countdown. Still high after a short wait => no PIT.
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        spk.read() & SPK_CH2_OUT == 0
    }
}

#[inline]
fn ch2_out_set() -> bool {
    unsafe { Port::<u8>::new(SPEAKER_CTRL).read() & SPK_CH2_OUT != 0 }
}

/// Busy-wait `ms` real milliseconds. Works with interrupts disabled.
/// Returns immediately when no PIT is present (never hangs).
/// Falls back to PM timer if PIT is unavailable (common on UEFI).
/// Also validates that each chunk takes at least 50% of expected time;
/// if not, marks PIT unreliable and falls back permanently.
pub fn sleep_ms(ms: u32) {
    let mut left = ms;
    while left > 0 {
        let chunk = left.min(CH2_MAX_MS);
        if ch2_arm(chunk) {
            // PIT armed — wait for OUT to go high
            let start_ticks = crate::time::ticks();
            let expected_ticks = ((chunk as u64 * crate::time::tick_hz()) / 1000).max(1);
            let mut guard = 0u32;
            loop {
                if ch2_out_set() {
                    break;
                }
                guard += 1;
                if guard > 5_000_000 {
                    // Guard timeout — something wrong
                    crate::drivers::pit::mark_ch2_unreliable();
                    crate::drivers::pm_timer::sleep_ms(left);
                    return;
                }
                core::hint::spin_loop();
            }
            let elapsed_ticks = crate::time::ticks().saturating_sub(start_ticks);
            // If elapsed < 50% of expected, PIT is running too fast (broken)
            if elapsed_ticks * 2 < expected_ticks {
                crate::serial_println!(
                    "[pit] chunk {}ms: expected {} ticks, got {} — marking UNRELIABLE",
                    chunk, expected_ticks, elapsed_ticks
                );
                crate::drivers::pit::mark_ch2_unreliable();
                crate::drivers::pm_timer::sleep_ms(left);
                return;
            }
            left -= chunk;
        } else {
            // PIT not present — fall back to PM timer
            crate::drivers::pm_timer::sleep_ms(left);
            return;
        }
    }
}

/// PIT-only sleep (no fallback). Used internally by PM timer init.
pub(crate) fn sleep_ms_pit_only(ms: u32) {
    let mut left = ms;
    while left > 0 {
        let chunk = left.min(CH2_MAX_MS);
        if !ch2_arm(chunk) {
            return;
        }
        let mut guard = 0u32;
        loop {
            if ch2_out_set() {
                break;
            }
            guard += 1;
            if guard > 5_000_000 {
                return;
            }
            core::hint::spin_loop();
        }
        left -= chunk;
    }
}

/// Real-time deadline built from chained channel-2 one-shots.
/// Only one `Timeout` may be polled at a time (they share channel 2);
/// use sequentially, never nested.
pub struct Timeout {
    remaining_ms: u32,
    chunk_ms: u32,
    no_pit: bool,
    // PM timer fallback state
    pm_fallback: bool,
    pm_start_us: u64,
    pm_duration_us: u64,
    // TSC fallback state (used when both PIT and PM timer are dead)
    tsc_fallback: bool,
    tsc_start: u64,
    tsc_target: u64,
}

/// Arm the TSC fallback on an already-created timeout.
fn arm_tsc_fallback(t: &mut Timeout, duration_ms: u32) -> bool {
    let freq = crate::drivers::tsc::get_freq_hz();
    if freq == 0 {
        return false;
    }
    t.tsc_fallback = true;
    t.tsc_start = crate::drivers::tsc::read_tsc();
    t.tsc_target = t.tsc_start.saturating_add(duration_ms as u64 * freq / 1000);
    true
}

fn tsc_expired(t: &Timeout) -> bool {
    let now = crate::drivers::tsc::read_tsc();
    now.saturating_sub(t.tsc_start) >= t.tsc_target.saturating_sub(t.tsc_start)
}

/// Start a deadline `total_ms` in the future.
pub fn timeout_ms(total_ms: u32) -> Timeout {
    let mut t = Timeout {
        remaining_ms: total_ms,
        chunk_ms: 0,
        no_pit: false,
        pm_fallback: false,
        pm_start_us: 0,
        pm_duration_us: 0,
        tsc_fallback: false,
        tsc_start: 0,
        tsc_target: 0,
    };
    if total_ms == 0 {
        return t;
    }
    let chunk = total_ms.min(CH2_MAX_MS);
    if ch2_arm(chunk) {
        t.chunk_ms = chunk;
    } else {
        t.no_pit = true;
        // Try PM timer fallback, then calibrated TSC.
        if crate::drivers::pm_timer::is_available() {
            t.pm_fallback = true;
            t.pm_start_us = crate::drivers::pm_timer::read_us();
            t.pm_duration_us = (total_ms as u64) * 1000;
        } else if !arm_tsc_fallback(&mut t, total_ms) {
            // No timebase at all: fail open (instant expiry).
        }
    }
    t
}

impl Timeout {
    /// True once the full duration has elapsed. Without a PIT this is
    /// immediately true (fail open: callers time out instead of hanging).
    pub fn poll(&mut self) -> bool {
        if self.remaining_ms == 0 {
            return true;
        }
        if self.no_pit {
            if self.pm_fallback {
                // Use PM timer for deadline tracking
                let elapsed_us = crate::drivers::pm_timer::read_us().saturating_sub(self.pm_start_us);
                return elapsed_us >= self.pm_duration_us;
            }
            if self.tsc_fallback {
                return tsc_expired(self);
            }
            return true; // fail open: no timebase at all
        }
        if !ch2_out_set() {
            return false;
        }
        self.remaining_ms = self.remaining_ms.saturating_sub(self.chunk_ms);
        if self.remaining_ms == 0 {
            return true;
        }
        let chunk = self.remaining_ms.min(CH2_MAX_MS);
        if ch2_arm(chunk) {
            self.chunk_ms = chunk;
            false
        } else {
            self.no_pit = true;
            // Try PM timer fallback, then calibrated TSC.
            if crate::drivers::pm_timer::is_available() {
                self.pm_fallback = true;
                self.pm_start_us = crate::drivers::pm_timer::read_us();
                self.pm_duration_us = self.remaining_ms as u64 * 1000;
                // Now poll using PM timer
                let elapsed_us = crate::drivers::pm_timer::read_us().saturating_sub(self.pm_start_us);
                elapsed_us >= self.pm_duration_us
            } else if arm_tsc_fallback(self, self.remaining_ms) {
                tsc_expired(self)
            } else {
                true // fail open
            }
        }
    }

    /// Whether this deadline is backed by a real timebase (false =>
    /// every `poll()` expires immediately). Use to skip deadline enforcement
    /// where an instant expiry would do harm.
    pub fn reliable(&self) -> bool {
        !self.no_pit || self.pm_fallback || self.tsc_fallback
    }
}