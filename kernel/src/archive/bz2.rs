//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! bzip2 framing via `libbz2-rs-sys` (pure Rust, `no_std` with the
//! `rust-allocator` backend, same shape as `xz.rs`).

use alloc::vec::Vec;
use libbz2_rs_sys as bz;

/// True if `data` starts with the bzip2 magic (`BZh` + level digit).
pub fn is_bz2(data: &[u8]) -> bool {
    data.len() >= 4 && data[0] == b'B' && data[1] == b'Z' && data[2] == b'h' && data[3].is_ascii_digit()
}

const SCRATCH: usize = 32 * 1024;

/// Decompress a `.bz2` payload. `limit` bounds the output size.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_bz2(data) {
        return Err("bz2: bad magic");
    }
    unsafe {
        let mut stream = bz::bz_stream::zeroed();
        if bz::BZ2_bzDecompressInit(&mut stream, 0, 0) != bz::BZ_OK {
            return Err("bz2: decoder init failed");
        }
        let mut out: Vec<u8> = Vec::new();
        let mut scratch: Vec<u8> = alloc::vec![0u8; SCRATCH];
        stream.next_in = data.as_ptr() as *const i8;
        stream.avail_in = data.len().min(u32::MAX as usize) as u32;
        loop {
            stream.next_out = scratch.as_mut_ptr() as *mut i8;
            stream.avail_out = SCRATCH as u32;
            let rc = bz::BZ2_bzDecompress(&mut stream);
            let got = SCRATCH - stream.avail_out as usize;
            if out.len() + got > limit {
                bz::BZ2_bzDecompressEnd(&mut stream);
                return Err("bz2: output too large");
            }
            out.extend_from_slice(&scratch[..got]);
            match rc {
                x if x == bz::BZ_STREAM_END => break,
                x if x == bz::BZ_OK => {
                    if got == 0 && stream.avail_in == 0 {
                        bz::BZ2_bzDecompressEnd(&mut stream);
                        return Err("bz2: truncated stream");
                    }
                }
                _ => {
                    bz::BZ2_bzDecompressEnd(&mut stream);
                    return Err("bz2: decode error");
                }
            }
        }
        bz::BZ2_bzDecompressEnd(&mut stream);
        Ok(out)
    }
}
