//! Hardware entropy source used to seed TLS handshakes.
//!
//! TLS must fail closed if the CPU cannot provide RDRAND. No timestamp or
//! predictable software fallback is acceptable for ephemeral key generation.

use core::num::NonZeroU32;

use rand_core::{CryptoRng, Error, RngCore};
use x86_64::instructions::random::RdRand;

pub struct Rdrand;

impl Rdrand {
    #[inline]
    fn word() -> Option<u32> {
        let rdrand = RdRand::new()?;
        for _ in 0..10 {
            if let Some(value) = rdrand.get_u32() {
                return Some(value);
            }
        }
        None
    }

    #[inline]
    fn error() -> Error {
        Error::from(NonZeroU32::new(Error::CUSTOM_START + 1).unwrap())
    }
}

impl RngCore for Rdrand {
    fn next_u32(&mut self) -> u32 {
        self.try_next_u32().expect("RDRAND unavailable")
    }

    fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | self.next_u32() as u64
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.try_fill_bytes(dest).expect("RDRAND unavailable")
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        for chunk in dest.chunks_mut(4) {
            let value = Self::word().ok_or_else(Self::error)?;
            let bytes = value.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(())
    }
}

impl Rdrand {
    fn try_next_u32(&mut self) -> Result<u32, Error> {
        Self::word().ok_or_else(Self::error)
    }
}

impl CryptoRng for Rdrand {}
