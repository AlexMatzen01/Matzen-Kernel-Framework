//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Zstandard via `ruzstd` (pure Rust, `no_std` with `std` feature off).

use alloc::vec::Vec;
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::CompressionLevel;
use ruzstd::io::Read as RzRead;

/// Zstd frame magic 0xFD2FB528, little-endian.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// True if `data` looks like a zstd frame or a skippable frame.
pub fn is_zst(data: &[u8]) -> bool {
    // Frame magic 0xFD2FB528 (LE bytes 28 B5 2F FD) and the skippable-frame
    // range 0x184D2A50..=0x184D2A5F (LE bytes 50 2A 4D 18..5F).
    data.len() >= 4
        && (data[..4] == ZSTD_MAGIC
            || (data[0] == 0x50 && data[1] == 0x2A && data[2] == 0x4D && (0x18..=0x5F).contains(&data[3])))
}

/// Decompress a `.zst` payload. `limit` bounds the output size.
/// Consecutive frames decode as one stream (per the zstd frame spec).
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_zst(data) {
        return Err("zst: bad magic");
    }
    let mut out: Vec<u8> = Vec::new();
    // Scratch on the stack: 4 KiB keeps the frame decode off the heap.
    let mut chunk = [0u8; 4096];
    let mut pos = 0usize;
    while pos < data.len() {
        let mut input: &[u8] = &data[pos..];
        let mut decoder =
            StreamingDecoder::new(&mut input).map_err(|_| "zst: decode error")?;
        let mut produced = 0usize;
        loop {
            match RzRead::read(&mut decoder, &mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if out.len() + n > limit {
                        return Err("zst: output too large");
                    }
                    out.extend_from_slice(&chunk[..n]);
                    produced += n;
                }
                Err(_) => return Err("zst: decode error"),
            }
        }
        // `decoder` borrowed `input`, so its cursor advanced on `input`.
        let used = (data.len() - pos) - input.len();
        if used == 0 && produced == 0 {
            return Err("zst: truncated frame");
        }
        pos += used.max(1);
    }
    Ok(out)
}

/// Compress `data` into a `.zst` payload.
pub fn compress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    Ok(ruzstd::encoding::compress_to_vec(data, CompressionLevel::Fastest))
}
