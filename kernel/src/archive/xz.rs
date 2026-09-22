//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! XZ container compression via `lzma-rust2` (pure Rust, `no_std`).
//!
//! `lzma-rust2` with `default-features = false` provides its own `Read` /
//! `Write` / `Error` types over `core` + `alloc` (its `no_std.rs`), so the
//! kernel never touches `std::io`. The `xz` cargo feature adds the `.xz`
//! container layer (stream/block headers, index, CRC/SHA checks via `sha2`,
//! whose default features are `alloc`-only).
//!
//! Memory note: XZ dictionary size is chosen by whoever *created* the file
//! (default `xz -6` = 8 MiB dict). Decoding needs roughly the dictionary
//! size plus output space, so very-high-preset files can exhaust the kernel
//! heap — that surfaces as a clean `OutOfMemory` error, never a panic.
//! Encoding uses preset 1 (1 MiB dict) to stay fast and small; any host
//! decoder still reads it.

use alloc::vec::Vec;
use lzma_rust2::{Read, Write, XzOptions, XzReader, XzWriter};

/// XZ stream magic: `FD 37 7A 58 5A 00`.
const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];

/// True if `data` starts with the XZ stream magic.
pub fn is_xz(data: &[u8]) -> bool {
    data.len() >= XZ_MAGIC.len() && data[..XZ_MAGIC.len()] == XZ_MAGIC
}

/// Encode preset used for `tar -cJf` (1 MiB dict; decodable everywhere).
const ENCODE_PRESET: u32 = 1;

/// Scratch buffer size for streaming decode (heap, reused).
const SCRATCH: usize = 32 * 1024;

/// Decompress an `.xz` file. `limit` bounds the total output (protects the
/// kernel heap); exceeding it is an error, not truncation.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_xz(data) {
        return Err("xz: bad magic");
    }
    let mut input: &[u8] = data;
    // `allow_multiple_streams = true`: concatenated `.xz` streams decode
    // transparently, mirroring the multi-member gzip handling.
    let mut reader = XzReader::new(&mut input, true);
    let mut out: Vec<u8> = Vec::new();
    let mut scratch: Vec<u8> = alloc::vec![0u8; SCRATCH];
    loop {
        if out.len() >= limit {
            return Err("xz: output too large");
        }
        let n = reader.read(&mut scratch).map_err(|e| match e {
            lzma_rust2::Error::OutOfMemory(_) => "xz: out of memory (file needs a bigger heap)",
            _ => "xz: decode error",
        })?;
        if n == 0 {
            break;
        }
        if out.len() + n > limit {
            return Err("xz: output too large");
        }
        out.extend_from_slice(&scratch[..n]);
    }
    Ok(out)
}

/// Compress `data` into an `.xz` file at preset 1.
pub fn compress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let options = XzOptions::with_preset(ENCODE_PRESET);
    let mut writer =
        XzWriter::new(Vec::<u8>::new(), options).map_err(|_| "xz: encoder init failed")?;
    writer.write_all(data).map_err(|e| match e {
        lzma_rust2::Error::OutOfMemory(_) => "xz: out of memory",
        _ => "xz: encode error",
    })?;
    writer.finish().map_err(|_| "xz: finish failed")
}
