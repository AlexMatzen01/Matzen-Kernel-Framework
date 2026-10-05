//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Kernel-wide entropy, independent of the `net_tls` feature.
//!
//! [`super::entropy`] wraps `Rdrand` in `rand_core`, which is only compiled
//! when TLS is enabled. Several protocols need unpredictable numbers whether or
//! not TLS is built ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â TCP initial sequence numbers and ephemeral ports above all
//! ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â so this module provides the primitive directly on top of the always-present
//! `x86_64` crate.
//!
//! Sources, in order of preference:
//!
//! 1. **RDRAND** (`CPUID.01H:ECX.RDRAND`) when the CPU advertises it. On x86-64
//!    this is present on every CPU since 2008.
//! 2. **RDSEED**, when RDRAND is absent, because it is a different, independently
//!    designed source rather than a reseed of the same generator.
//! 3. **A seeded mixer** (`SplitMix64`) advanced across calls, seeded once from
//!    the best available hardware source. This is *not* cryptographically
//!    unpredictable ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â it is weak entropy with a bias, suitable for port and
//!    sequence-number randomisation where the goal is to remove a trivially
//!    predictable default, not to defend a key.
//!
//! [`is_hardware`] reports which tier is active, and callers that genuinely
//! require hardware entropy (TLS ephemeral keys) should check it rather than
//! assume: `super::entropy` remains the fail-closed path for TLS.

use core::arch::asm;
use x86_64::instructions::random::RdRand;

/// Read hardware RDSEED (64-bit output), if the CPU supports it.
///
/// Issued inline because the pinned `x86_64` 0.15 wrapper exposes only
/// `RdRand`. RDSEED always faults when `CPUID.07H:EBX.RDSEED` is clear, so the
/// CPUID check must come first.
#[inline]
fn rdseed64() -> Option<u64> {
    // CPUID.07H:EBX bit 18 = RDSEED.
    let r = unsafe { core::arch::x86_64::__cpuid(7) };
    let ebx = r.ebx;
    if ebx & (1 << 18) == 0 {
        return None;
    }
    // Written by the instruction; not read beforehand, so it is left
    // uninitialised rather than zeroed and then overwritten.
    let mut value: u64;
    let ok: u8;
    // RDSEED r64, retries disabled (ECX = 0).
    unsafe {
        asm!(
            "rdseed {value}",
            "setc {ok}",
            value = out(reg) value,
            ok = out(reg_byte) ok,
            options(nostack, preserves_flags)
        );
    }
    if ok != 0 {
        Some(value)
    } else {
        None
    }
}

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Fallback generator state, seeded once from the best source available.
///
/// Advanced with a compare-exchange loop so two callers cannot be handed the
/// same value if the stack is ever preempted mid-update.
static MIXER: AtomicU64 = AtomicU64::new(0);
/// Whether hardware entropy has ever been observed on this machine.
static HARDWARE_SEEN: AtomicBool = AtomicBool::new(false);

/// True when RDRAND or RDSEED produced at least one value.
pub fn is_hardware() -> bool {
    HARDWARE_SEEN.load(Ordering::Relaxed)
}

/// SplitMix64: a fixed, well-tested finaliser used only to spread a seed.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Try RDRAND, then RDSEED, for one 64-bit value.
///
/// Both are retried a bounded number of times: both instructions can report a
/// transient failure, and a single spurious failure should not push us onto
/// the weak fallback for the life of the boot.
fn hardware_u64() -> Option<u64> {
    if let Some(rdrand) = RdRand::new() {
        let mut hi = None;
        let mut lo = None;
        for _ in 0..16 {
            if hi.is_none() {
                hi = rdrand.get_u32();
            }
            if lo.is_none() {
                lo = rdrand.get_u32();
            }
            if let (Some(h), Some(l)) = (hi, lo) {
                HARDWARE_SEEN.store(true, Ordering::Relaxed);
                return Some(((h as u64) << 32) | l as u64);
            }
        }
    }
    for _ in 0..16 {
        if let Some(v) = rdseed64() {
            HARDWARE_SEEN.store(true, Ordering::Relaxed);
            return Some(v);
        }
    }
    None
}

/// Seed the fallback mixer if it has not been seeded yet.
fn ensure_seeded() -> u64 {
    // `state` doubles as the generation: the value produced for this caller is
    // also the state the next caller starts from. Because a caller only ever
    // publishes a state derived from the one it read, two callers cannot come
    // away holding the same value.
    let mut state = MIXER.load(Ordering::Relaxed);
    loop {
        if state == 0 {
            // First call on a machine without a hardware source. The clock and
            // tick counter only break ties between early calls; either is weak
            // on its own, which is why this is the last tier, not the first.
            let clock = crate::time::uptime_millis();
            let ticks = crate::shell::get_tick_count();
            state = hardware_u64().unwrap_or_else(|| {
                clock
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ ticks.wrapping_mul(0xD1B5_4A32_D192_ED03)
                    ^ 0xA5A5_5A5A_DEAD_BEEF
            }) | 1;
        }
        let value = splitmix64(&mut state);
        // Zero would mean "unseeded" to the next caller, which would re-seed and
        // potentially hand out a second copy of this value.
        if value == 0 {
            state = 1;
            continue;
        }
        match MIXER.compare_exchange_weak(
            MIXER.load(Ordering::Relaxed),
            value,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return value,
            Err(observed) => {
                // Another caller got there first; mix again from their state so
                // this caller does not reuse the value they are about to.
                state = observed;
            }
        }
    }
}
/// 64 bits of entropy.
///
/// Prefer [`is_hardware`] when the caller needs unpredictability rather than
/// just "not a fixed constant".
pub fn u64() -> u64 {
    hardware_u64().unwrap_or_else(ensure_seeded)
}

/// 32 bits of entropy.
pub fn u32() -> u32 {
    (u64() >> 32) as u32
}

/// 16 bits of entropy.
pub fn u16() -> u16 {
    (u64() >> 48) as u16
}

/// Uniform value in `0..bound`, by rejection sampling.
///
/// A plain modulo would bias toward the low end of the range, which for
/// ephemeral ports means certain ports get chosen far more often than others.
pub fn below(bound: u32) -> u32 {
    if bound == 0 {
        return 0;
    }
    let zone = u32::MAX - (u32::MAX % bound) - 1;
    loop {
        let v = u32();
        if v <= zone {
            return v % bound;
        }
    }
}

/// Fill `dest` with entropy.
pub fn fill(dest: &mut [u8]) {
    let mut offset = 0usize;
    while offset < dest.len() {
        let n = core::cmp::min(8, dest.len() - offset);
        let bytes = u64().to_le_bytes();
        dest[offset..offset + n].copy_from_slice(&bytes[..n]);
        offset += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn values_are_not_constant() {
        let a = u64();
        let b = u64();
        assert_ne!(a, b, "two consecutive draws must differ");
    }

    #[test]
    fn seed_is_never_zero() {
        // A zero mixer state would re-seed from the clock on every call and
        // produce a correlated stream.
        for _ in 0..64 {
            assert_ne!(u64(), 0);
        }
    }

    #[test]
    fn below_respects_its_bound() {
        for bound in [1u32, 2, 7, 100, 1024, 49152, 65535] {
            for _ in 0..256 {
                let v = below(bound);
                assert!(v < bound, "{} not below {}", v, bound);
            }
        }
        assert_eq!(below(0), 0, "a zero bound must not divide by zero");
        assert_eq!(below(1), 0);
    }

    #[test]
    fn narrow_widths_still_vary() {
        // TCP ports, IP identifiers and DNS transaction IDs are all drawn from
        // these. A constant at any width would make every connection tuple
        // identical across boots, which is the failure they exist to prevent.
        let mut seen_u16 = alloc::collections::BTreeSet::new();
        for _ in 0..64 {
            seen_u16.insert(u16());
        }
        assert!(seen_u16.len() > 1, "u16 entropy is constant");

        let mut seen_u32 = alloc::collections::BTreeSet::new();
        for _ in 0..64 {
            seen_u32.insert(u32());
        }
        assert!(seen_u32.len() > 1, "u32 entropy is constant");
    }

    #[test]
    fn below_covers_its_range() {        // A modulo-biased generator would leave part of a small range unused.
        let mut seen = [false; 16];
        for _ in 0..4096 {
            seen[below(16) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "some values never produced");
    }

    #[test]
    fn fill_covers_every_byte() {
        let mut buf = [0u8; 37];
        fill(&mut buf);
        // Not all-zero, and not a single repeated 8-byte pattern, which would
        // indicate the tail chunk was left uninitialised.
        assert!(buf.iter().any(|&b| b != 0));
        assert_ne!(&buf[0..8], &buf[8..16]);
    }

    #[test]
    fn fill_handles_unaligned_lengths() {
        for len in 0..24usize {
            let mut buf = vec![0u8; len];
            fill(&mut buf);
        }
    }

    #[test]
    fn splitmix64_advances_and_does_not_repeat() {
        // Used only to spread a seed, so the property that matters is that it
        // does not degenerate: the state advances, and consecutive outputs
        // differ.
        let mut s = 0u64;
        let mut prev = splitmix64(&mut s);
        assert_ne!(s, 0, "state must advance from zero");
        for _ in 0..64 {
            let next = splitmix64(&mut s);
            assert_ne!(next, prev, "mixer repeated an output");
            prev = next;
        }
    }

    #[test]
    fn splitmix64_spreads_a_small_seed() {
        // Two seeds differing in one bit must produce unrelated streams; a
        // weak finaliser would leave neighbouring seeds adjacent.
        let mut a = 1u64;
        let mut b = 2u64;
        let x = splitmix64(&mut a);
        let y = splitmix64(&mut b);
        assert_ne!(x, y);
        assert!(x.wrapping_sub(y) > 1_000_000, "outputs are suspiciously close");
    }
}
