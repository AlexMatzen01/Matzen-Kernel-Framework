//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! `.lz` (lzip) container via `lzma-rust2`'s `LzipReader`/`LzipWriter`.

use alloc::vec::Vec;
use lzma_rust2::{LzipOptions, LzipReader, LzipWriter};
#[cfg(test)]
use std::io::{Read, Write};
#[cfg(not(test))]
use lzma_rust2::{Read, Write};

/// Lzip magic: 'L', 'Z', 'I', 'P'.
const LZIP_MAGIC: [u8; 4] = [b'L', b'Z', b'I', b'P'];

/// True if `data` starts with the lzip magic.
pub fn is_lzip(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == LZIP_MAGIC
}

/// Decompress a `.lz` payload. `limit` bounds the output size.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_lzip(data) {
        return Err("lzip: bad magic");
    }
    let mut input: &[u8] = data;
    let mut reader = LzipReader::new(&mut input);
    let mut out: Vec<u8> = Vec::new();
    let mut scratch = alloc::vec![0u8; 32 * 1024];
    loop {
        let n = reader.read(&mut scratch).map_err(|_| "lzip: decode error")?;
        if n == 0 {
            break;
        }
        if out.len() + n > limit {
            return Err("lzip: output too large");
        }
        out.extend_from_slice(&scratch[..n]);
    }
    Ok(out)
}

/// Compress `data` into a `.lz` payload.
pub fn compress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut writer = LzipWriter::new(Vec::<u8>::new(), LzipOptions::default());
    writer.write_all(data).map_err(|_| "lzip: encode error")?;
    writer.finish().map_err(|_| "lzip: finish failed")
}

// ── legacy `.lzma` (headered LZMA1) ──

use lzma_rust2::{LzmaOptions, LzmaReader, LzmaWriter};

/// Decompress a legacy `.lzma` payload. The format starts with the LZMA
/// properties byte, so there is no magic to probe: callers must select it
/// by extension (see `Kind::TarLzma`), and a wrong choice fails inside the
/// decoder rather than returning garbage.
pub fn lzma1_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut input: &[u8] = data;
    let mut reader = LzmaReader::new_mem_limit(&mut input, u32::MAX, None)
        .map_err(|_| "lzma: bad header")?;
    let mut out: Vec<u8> = Vec::new();
    let mut scratch = alloc::vec![0u8; 32 * 1024];
    loop {
        let n = reader.read(&mut scratch).map_err(|_| "lzma: decode error")?;
        if n == 0 {
            break;
        }
        if out.len() + n > limit {
            return Err("lzma: output too large");
        }
        out.extend_from_slice(&scratch[..n]);
    }
    Ok(out)
}

/// Compress `data` into a legacy `.lzma` payload.
pub fn lzma1_compress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut writer = LzmaWriter::new_use_header(Vec::<u8>::new(), &LzmaOptions::default(), Some(data.len() as u64))
        .map_err(|_| "lzma: encoder init failed")?;
    writer.write_all(data).map_err(|_| "lzma: encode error")?;
    writer.finish().map_err(|_| "lzma: finish failed")
}
