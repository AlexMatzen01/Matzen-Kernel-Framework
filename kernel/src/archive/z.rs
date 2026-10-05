//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Legacy `compress` (`.Z`) decoder: a small LZW (LSB-first) reader.
//!
//! This is intentionally a compact implementation covering the all-too-
//! common shape of real `.Z` files: `1F 9D` magic, flags byte with max
//! code size and the block-mode bit, codes LSB-first across byte
//! boundaries. Creation is not supported.

use alloc::vec::Vec;

const MAGIC0: u8 = 0x1F;
const MAGIC1: u8 = 0x9D;
const FLAG_BLOCK: u8 = 0x20;

/// True if `data` looks like a `compress` stream (`1F 9D` magic).
pub fn is_zcompress(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == MAGIC0 && data[1] == MAGIC1
}

/// An LZW table entry: referenced substrings are prefix-shared.
#[derive(Clone)]
struct Node {
    prefix: i32,
    first_char: u8,
}

fn read_code(
    data: &[u8],
    pos: &mut usize,
    bit_pos: &mut u8,
    code_size: usize,
) -> Result<i32, &'static str> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    while bits < code_size as u32 {
        if *pos >= data.len() {
            return Err("z: truncated stream");
        }
        let b = data[*pos];
        let avail = 8 - *bit_pos as u32;
        let take = core::cmp::min(avail, code_size as u32 - bits);
        let piece = ((b as u32) >> *bit_pos) & ((1u32 << take) - 1);
        acc |= piece << bits;
        bits += take;
        *bit_pos += take as u8;
        if *bit_pos >= 8 {
            *bit_pos = 0;
            *pos += 1;
        }
    }
    Ok(acc as i32)
}

fn first_char_of(tokens: &[Node], idx: usize) -> u8 {
    let mut cur = idx;
    while cur >= 258 {
        cur = tokens[cur].prefix as usize;
    }
    tokens[cur].first_char
}

fn emit(tokens: &[Node], idx: i32, out: &mut Vec<u8>, limit: usize) -> Result<(), &'static str> {
    let mut chars: Vec<u8> = Vec::new();
    let mut cur = idx;
    while cur >= 256 {
        chars.push(tokens[cur as usize].first_char);
        cur = tokens[cur as usize].prefix;
    }
    chars.push(cur as u8);
    if out.len() + chars.len() > limit {
        return Err("z: output too large");
    }
    out.extend(chars.iter().rev());
    Ok(())
}

/// Decompress a `.Z` payload. `limit` bounds the output size.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    if !is_zcompress(data) {
        return Err("z: bad magic");
    }
    let flags = data[2];
    let max_bits = (flags & 0x1F) as usize;
    if max_bits < 9 || max_bits > 16 {
        return Err("z: bad max code size");
    }
    let block_mode = flags & FLAG_BLOCK != 0;
    let clear_code: i32 = if block_mode { 256 } else { -1 };
    let eoi_code: i32 = if block_mode { 257 } else { 256 };
    let first_free: usize = if block_mode { 258 } else { 257 };

    let mut tokens: Vec<Node> = Vec::new();
    for i in 0..258 {
        tokens.push(Node { prefix: -1, first_char: i as u8 });
    }
    let mut next_free: usize = first_free;
    let mut code_size: usize = 9;
    let mut prev: i32 = -1;

    // LSB-first bit reader over data[3..]
    let mut pos = 3usize;
    let mut bit_pos: u8 = 0;
    let mut out: Vec<u8> = Vec::new();

    loop {
        let code = match read_code(data, &mut pos, &mut bit_pos, code_size) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        if code == eoi_code {
            return Ok(out);
        }
        if block_mode && code == clear_code {
            tokens.truncate(258);
            next_free = 258;
            code_size = 9;
            prev = -1;
            continue;
        }
        let word: i32;
        if code < next_free as i32 {
            word = code;
        } else if code == next_free as i32 && prev >= 0 {
            let first = first_char_of(&tokens, prev as usize);
            let idx = tokens.len();
            tokens.push(Node { prefix: prev, first_char: first });
            word = idx as i32;
            emit(&tokens, word, &mut out, limit)?;
            next_free += 1;
            if next_free + 1 > (1usize << code_size) - 1 && code_size < max_bits {
                code_size += 1;
            }
            prev = word;
            continue;
        } else {
            return Err("z: bad LZW code");
        };

        emit(&tokens, word, &mut out, limit)?;
        if prev >= 0 {
            let first = first_char_of(&tokens, word as usize);
            tokens.push(Node { prefix: prev, first_char: first });
            next_free += 1;
            if next_free + 1 > (1usize << code_size) - 1 && code_size < max_bits {
                code_size += 1;
            }
        }
        prev = word;
    }
}
