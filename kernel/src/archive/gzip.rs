//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! gzip framing (RFC 1952) around raw DEFLATE from `miniz_oxide`.
//!
//! `miniz_oxide` is pure Rust `no_std` (`with-alloc` only needs
//! `extern crate alloc`), so the only work here is the gzip container:
//! 10-byte header + optional fields (`FEXTRA`/`FNAME`/`FCOMMENT`/`FHCRC`),
//! raw deflate body, and the 8-byte trailer (`CRC32` + `ISIZE`).
//! Multi-member files (concatenated members) decode per RFC 1952 §2.2.

use alloc::vec::Vec;
use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZFlush, MZStatus};

use super::crc32;

/// Maximum concatenated members per file (bounds CPU on hostile input).
pub const MAX_MEMBERS: usize = 64;

/// Scratch buffer size for streaming inflation (heap, reused per member).
const SCRATCH: usize = 32 * 1024;

/// True if `data` starts with the gzip magic (`1f 8b`).
pub fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parse one member header; returns the header length in bytes.
fn member_header_len(data: &[u8]) -> Result<usize, &'static str> {
    if data.len() < 10 {
        return Err("gzip: truncated header");
    }
    if data[0] != 0x1F || data[1] != 0x8B {
        return Err("gzip: bad magic");
    }
    if data[2] != 8 {
        return Err("gzip: unsupported method (not deflate)");
    }
    let flg = data[3];
    if flg & 0xE0 != 0 {
        return Err("gzip: reserved header flags set");
    }
    let mut pos = 10usize;
    if flg & 0x04 != 0 {
        // FEXTRA: 2-byte length + payload.
        if pos + 2 > data.len() {
            return Err("gzip: truncated extra field");
        }
        let xlen = le16(&data[pos..]) as usize;
        pos += 2;
        pos = pos.checked_add(xlen).ok_or("gzip: bad extra length")?;
        if pos > data.len() {
            return Err("gzip: truncated extra field");
        }
    }
    if flg & 0x08 != 0 {
        // FNAME: zero-terminated original file name.
        match data[pos..].iter().position(|&b| b == 0) {
            Some(i) => pos += i + 1,
            None => return Err("gzip: truncated file name"),
        }
    }
    if flg & 0x10 != 0 {
        // FCOMMENT: zero-terminated comment.
        match data[pos..].iter().position(|&b| b == 0) {
            Some(i) => pos += i + 1,
            None => return Err("gzip: truncated comment"),
        }
    }
    if flg & 0x02 != 0 {
        // FHCRC: 2-byte header CRC (integrity hint; payload CRC is authoritative).
        if pos + 2 > data.len() {
            return Err("gzip: truncated header CRC");
        }
        pos += 2;
    }
    Ok(pos)
}

/// Decompress all members of a gzip file. `limit` bounds the total output
/// (protects the kernel heap); exceeding it is an error, not truncation.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_gzip(data) {
        return Err("gzip: bad magic");
    }
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut members = 0usize;

    while pos < data.len() {
        // Tolerate zero padding at the end (some archivers pad to blocks).
        if data.len() - pos < 10 {
            if data[pos..].iter().all(|&b| b == 0) {
                break;
            }
            return Err("gzip: truncated member");
        }
        let hlen = member_header_len(&data[pos..])?;
        let body = &data[pos + hlen..];

        // Box the 32 KiB inflate state (too big for the kernel stack).
        let mut state = InflateState::new_boxed(DataFormat::Raw);
        let mut scratch: Vec<u8> = alloc::vec![0u8; SCRATCH];
        let mut crc = crc32::INIT;
        let mut member_len: u32 = 0;
        let mut consumed = 0usize;

        loop {
            let res = inflate(&mut state, &body[consumed..], &mut scratch, MZFlush::None);
            consumed += res.bytes_consumed;
            let chunk = &scratch[..res.bytes_written];
            match res.status {
                Ok(MZStatus::StreamEnd) => {
                    if out.len() + chunk.len() > limit {
                        return Err("gzip: output too large");
                    }
                    out.extend_from_slice(chunk);
                    crc = crc32::update(crc, chunk);
                    member_len = member_len.wrapping_add(chunk.len() as u32);
                    break;
                }
                Ok(MZStatus::Ok) => {                    if res.bytes_consumed == 0 && res.bytes_written == 0 {
                        return Err("gzip: truncated deflate data");
                    }
                    if out.len() + chunk.len() > limit {
                        return Err("gzip: output too large");
                    }
                    out.extend_from_slice(chunk);
                    crc = crc32::update(crc, chunk);
                    member_len = member_len.wrapping_add(chunk.len() as u32);
                }
                Err(_) => return Err("gzip: deflate error"),
                Ok(_) => return Err("gzip: deflate error"),
            }
        }

        let tpos = pos + hlen + consumed;
        if tpos + 8 > data.len() {
            return Err("gzip: truncated trailer");
        }
        if crc32::finish(crc) != le32(&data[tpos..]) {
            return Err("gzip: CRC mismatch");
        }
        if member_len != le32(&data[tpos + 4..]) {
            return Err("gzip: size mismatch");
        }
        pos = tpos + 8;
        members += 1;
        if members > MAX_MEMBERS {
            return Err("gzip: too many members");
        }
    }
    if members == 0 {
        return Err("gzip: empty input");
    }
    Ok(out)
}

/// Compress `data` into a single-member gzip file (level 6, `MTIME=0`,
/// `OS=255` unknown for deterministic output).
pub fn compress(data: &[u8]) -> Vec<u8> {
    let raw = miniz_oxide::deflate::compress_to_vec(data, 6);
    let mut out = Vec::with_capacity(10 + raw.len() + 8);
    out.extend_from_slice(&[0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0xFF]);
    out.extend_from_slice(&raw);
    out.extend_from_slice(&crc32::crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}
