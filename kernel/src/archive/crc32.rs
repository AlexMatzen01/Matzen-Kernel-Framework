//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Shared IEEE CRC-32 (reflected polynomial 0xEDB88320) used by gzip and zip.
//!
//! `no_std` + allocation-free on purpose: the whole implementation is a
//! table-driven software CRC over `core` only, so both the gzip trailer
//! (RFC 1952) and the zip entry headers (PKWARE APPNOTE) share one
//! implementation and one test vector.

/// Precomputed CRC-32 table for the reflected polynomial 0xEDB88320.
const TABLE: [u32; 256] = make_table();

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 == 1 {
                crc = 0xEDB8_8320 ^ (crc >> 1);
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Initial internal CRC state.
pub const INIT: u32 = 0xFFFF_FFFF;

/// Update a running CRC state with `data`. Start from [`INIT`] and finish
/// with [`finish`]; intermediate states compose: `update(update(s, a), b)`.
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// Finish a running CRC state into the final checksum value.
pub fn finish(crc: u32) -> u32 {
    crc ^ 0xFFFF_FFFF
}

/// One-shot CRC-32 over `data`.
pub fn crc32(data: &[u8]) -> u32 {
    finish(update(INIT, data))
}
