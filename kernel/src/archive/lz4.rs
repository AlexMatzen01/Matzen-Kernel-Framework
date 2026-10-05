//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! LZ4 *frame* container (`.lz4`) via the `lz4_flex` block codec.
//!
//! GNU tar pipes `.tar.lz4` through the standard `lz4` tool, which writes
//! the LZ4 *frame* format; `lz4_flex` only exposes the no_std *block*
//! codec, so the (small) frame container is parsed here on top of
//! `block::{compress, decompress_into}`. Block max sizes come from the
//! frame header's block-description byte, bounding each decode.

use alloc::vec::Vec;

/// LZ4 frame magic (little-endian 0x184D2204).
const MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];

/// Size of the sliding window a linked-block frame may reference.
const WINDOW: usize = 64 * 1024;

/// True if `data` starts with an LZ4 frame magic.
pub fn is_lz4(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == MAGIC
}

// ── Minimal XXH32 (seed 0) for the LZ4 frame header checksum ──

const XX_P1: u32 = 0x9E37_79B1;
const XX_P2: u32 = 0x85EB_CA77;
const XX_P3: u32 = 0xC2B2_AE3D;
const XX_P4: u32 = 0x27D4_EB2F;
const XX_P5: u32 = 0x1656_67B1;

fn xxh32(data: &[u8]) -> u32 {
    let mut i = 0usize;
    let mut h32: u32;
    if data.len() >= 16 {
        let mut v1 = XX_P1.wrapping_add(XX_P2); // seed 0
        let mut v2 = XX_P2;
        let mut v3 = 0u32;
        let mut v4 = 0u32.wrapping_sub(XX_P1);
        while i + 16 <= data.len() {
            v1 = v1.wrapping_add(le32(&data[i..]).wrapping_mul(XX_P2)).rotate_left(13).wrapping_mul(XX_P1);
            v2 = v2.wrapping_add(le32(&data[i + 4..]).wrapping_mul(XX_P2)).rotate_left(13).wrapping_mul(XX_P1);
            v3 = v3.wrapping_add(le32(&data[i + 8..]).wrapping_mul(XX_P2)).rotate_left(13).wrapping_mul(XX_P1);
            v4 = v4.wrapping_add(le32(&data[i + 12..]).wrapping_mul(XX_P2)).rotate_left(13).wrapping_mul(XX_P1);
            i += 16;
        }
        h32 = v1.rotate_left(1).wrapping_add(v2.rotate_left(7)).wrapping_add(v3.rotate_left(12)).wrapping_add(v4.rotate_left(18));
    } else {
        h32 = XX_P5;
    }
    h32 = h32.wrapping_add(data.len() as u32);
    while i + 4 <= data.len() {
        h32 = h32.wrapping_add(le32(&data[i..]).wrapping_mul(XX_P3)).rotate_left(17).wrapping_mul(XX_P4);
        i += 4;
    }
    while i < data.len() {
        h32 = h32.wrapping_add((data[i] as u32).wrapping_mul(XX_P5)).rotate_left(11).wrapping_mul(XX_P1);
        i += 1;
    }
    h32 ^= h32 >> 15;
    h32 = h32.wrapping_mul(XX_P2);
    h32 ^= h32 >> 13;
    h32 = h32.wrapping_mul(XX_P3);
    h32 ^= h32 >> 16;
    h32
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// LZ4 frame header checksum: bits 8..15 of XXH32 over the header fields
/// (FLG, BD and any optional content-size / dictionary-id bytes).
pub fn header_checksum(header: &[u8]) -> u8 {
    (xxh32(header) >> 8) as u8
}

/// Maximum uncompressed size of a data block, from the BD byte's bits 6-4.
/// Values 0-3 are not assigned by the format, so they are rejected.
fn block_max(bd: u8) -> Result<usize, &'static str> {
    match (bd >> 4) & 0x7 {
        4 => Ok(64 * 1024),
        5 => Ok(256 * 1024),
        6 => Ok(1024 * 1024),
        7 => Ok(4 * 1024 * 1024),
        _ => Err("lz4: bad block description"),
    }
}

/// Decompress a `.lz4` (frame format) payload. `limit` bounds the output.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_lz4(data) {
        return Err("lz4: bad magic");
    }
    let mut pos = 4usize;
    let mut out: Vec<u8> = Vec::new();
    // Sliding window for linked-block frames.
    let mut window: Vec<u8> = Vec::new();
    while pos < data.len() {
        // Frame header: FLG, BD[, content size][, dict id], HC.
        if pos + 3 > data.len() {
            return Err("lz4: truncated frame header");
        }
        // FLG: bits 7-6 = version (must be 01), bit 5 = block independence,
        // bit 4 = block checksum, bit 3 = content size, bit 2 = content
        // checksum, bit 1 = reserved (must be 0), bit 0 = dictionary id.
        let flg = data[pos];
        let bd = data[pos + 1];
        if flg & 0xC0 != 0x40 || flg & 0x02 != 0 {
            return Err("lz4: reserved flags set");
        }
        let mut i = pos + 2;
        let mut content_size: Option<u64> = if flg & 0x08 != 0 {
            if i + 8 > data.len() {
                return Err("lz4: truncated content size");
            }
            let v = u64::from_le_bytes([
                data[i], data[i + 1], data[i + 2], data[i + 3],
                data[i + 4], data[i + 5], data[i + 6], data[i + 7],
            ]);
            i += 8;
            Some(v)
        } else {
            None
        };
        if flg & 0x01 != 0 {
            if i + 4 > data.len() {
                return Err("lz4: truncated frame header");
            }
            i += 4;
        }
        if i + 1 > data.len() {
            return Err("lz4: truncated header checksum");
        }
        // HC covers FLG..end-of-optional-fields. Historic encoders wrote a
        // zero here, so only a non-zero value is enforced.
        let hc = data[i];
        if hc != 0 && hc != header_checksum(&data[pos..i]) {
            return Err("lz4: header checksum mismatch");
        }
        i += 1;
        // BD: bits 6-4 select the max block size, bit 7 and bits 3-0 are
        // reserved.
        if (bd & 0x8F) != 0 {
            return Err("lz4: reserved BD bits set");
        }
        let block_checksum = flg & 0x10 != 0;
        let max = block_max(bd)?;
        // Linked blocks may reference the previous 64 KiB of output, which
        // we keep in `window`; independent blocks never consult it.
        let linked = flg & 0x20 == 0;

        let mut bp = i;
        loop {
            if bp + 4 > data.len() {
                return Err("lz4: truncated block");
            }
            let desc = u32::from_le_bytes([data[bp], data[bp + 1], data[bp + 2], data[bp + 3]]);
            bp += 4;
            if desc == 0 {
                break;
            }
            let raw_block = desc & 0x8000_0000 != 0;
            let bsize = (desc & 0x7FFF_FFFF) as usize;
            if bsize > max {
                return Err("lz4: oversized block");
            }
            if bp + bsize > data.len() {
                return Err("lz4: truncated block data");
            }
            let bdata = &data[bp..bp + bsize];
            bp += bsize;
            if block_checksum {
                if bp + 4 > data.len() {
                    return Err("lz4: truncated block checksum");
                }
                bp += 4;
            }
            // A compressed block cannot expand beyond the frame's max block
            // size (BD), and a stored block is bounded by its own length, so
            // this single check bounds the scratch buffer and the output.
            let produced_max = if raw_block { bsize } else { max };
            if out.len() + produced_max > limit {
                return Err("lz4: output too large");
            }
            let chunk: Vec<u8> = if raw_block {
                bdata.to_vec()
            } else {
                let mut buf = alloc::vec![0u8; max];
                let n = if linked {
                    lz4_flex::block::decompress_into_with_dict(bdata, &mut buf, &window)
                        .map_err(|_| "lz4: decode error")?
                } else {
                    lz4_flex::block::decompress_into(bdata, &mut buf)
                        .map_err(|_| "lz4: decode error")?
                };
                buf.truncate(n);
                buf
            };
            if let Some(left) = content_size.as_mut() {
                let t = chunk.len() as u64;
                if t > *left {
                    return Err("lz4: bad content size");
                }
                *left -= t;
            }
            out.extend_from_slice(&chunk);
            if linked {
                let start = out.len().saturating_sub(WINDOW);
                window.clear();
                window.extend_from_slice(&out[start..]);
            }
            if content_size == Some(0) {
                break;
            }
        }
        pos = bp;
        if flg & 0x04 != 0 {
            if pos + 4 > data.len() {
                return Err("lz4: truncated checksum");
            }
            pos += 4;
        }
        // Another frame may follow
        if pos + 4 <= data.len() && data[pos..].starts_with(&MAGIC) {
            pos += 4;
            continue;
        }
        break;
    }
    Ok(out)
}

/// Compress `data` into a single LZ4 frame (4 MiB blocks).
pub fn compress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.push(0x40); // FLG: version 01, block-independent
    out.push(0x70); // BD: block max 4 MiB
    let hc = header_checksum(&out[4..6]);
    out.push(hc);
    const BLOCK: usize = 4 * 1024 * 1024;
    if data.is_empty() {
        out.extend_from_slice(&0u32.to_le_bytes());
        return Ok(out);
    }
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + BLOCK).min(data.len());
        let chunk = &data[pos..end];
        let comp = lz4_flex::block::compress(chunk);
        if comp.len() >= chunk.len() {
            let desc = chunk.len() as u32 | 0x8000_0000;
            out.extend_from_slice(&desc.to_le_bytes());
            out.extend_from_slice(chunk);
        } else {
            let desc = comp.len() as u32;
            out.extend_from_slice(&desc.to_le_bytes());
            out.extend_from_slice(&comp);
        }
        pos = end;
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    Ok(out)
}
