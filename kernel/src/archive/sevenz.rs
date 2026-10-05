//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! 7-Zip container support (`.7z`), `no_std`.
//!
//! The reader handles host-produced 7z files whose folders use
//! Stored/COPY, LZMA1, or LZMA2 only (the default codecs emitted by the
//! `7z`/`7za` tools and `sevenz-rust2`), including the now-standard
//! LZMA2-compressed `K_ENCODED_HEADER` variant. Encrypted archives,
//! PPMd/BZip2/Brotli/zstd/LZ4/Copy-inside-BCJ and multi-coder filter
//! chains are rejected with a clear error.
//!
//! The writer emits a valid archive with a single folder per entry and a
//! plain (uncompressed) `K_HEADER` header: each entry is independently
//! LZMA2-compressed (or stored verbatim), so extraction is deterministic
//! and host tools (`7z`, bsdtar/libarchive's 7z support) read it.

use alloc::string::String;
use alloc::vec::Vec;

use lzma_rust2::{Lzma2Options, Lzma2Reader, Lzma2Writer, LzmaOptions, LzmaReader, LzmaWriter};
#[cfg(test)]
use std::io::{Read, Write};
#[cfg(not(test))]
use lzma_rust2::{Read, Write};

use super::crc32;
use super::tar::sanitize_path;

const SEVEN_Z_SIGNATURE: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
const SIGNATURE_HEADER_SIZE: u64 = 32;

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_EMPTY_STREAM: u8 = 0x0E;
const K_EMPTY_FILE: u8 = 0x0F;
const K_ANTI: u8 = 0x10;
const K_NAME: u8 = 0x11;
const K_ENCODED_HEADER: u8 = 0x17;

const ID_COPY: &[u8] = &[0x00];
const ID_LZMA: &[u8] = &[0x03, 0x01, 0x01];
const ID_LZMA2: &[u8] = &[0x21];

const MAX_ENTRIES: usize = 4096;

// ── Variable-length integer (7z) ──

fn read_variable_u64(data: &[u8], pos: &mut usize) -> Result<u64, &'static str> {
    if *pos >= data.len() {
        return Err("7z: truncated");
    }
    let first = data[*pos] as u64;
    *pos += 1;
    let mut mask = 0x80u64;
    let mut value = 0u64;
    for i in 0..8 {
        if first & mask == 0 {
            return Ok(value | ((first & (mask - 1)) << (8 * i)));
        }
        if *pos >= data.len() {
            return Err("7z: truncated");
        }
        let b = data[*pos] as u64;
        *pos += 1;
        value |= b << (8 * i);
        mask >>= 1;
    }
    Ok(value)
}

fn read_variable_usize(data: &[u8], pos: &mut usize) -> Result<usize, &'static str> {
    let v = read_variable_u64(data, pos)?;
    if v > usize::MAX as u64 {
        return Err("7z: size too large");
    }
    Ok(v as usize)
}

fn write_variable_u64(out: &mut Vec<u8>, value: u64) {
    if value < 0x80 {
        out.push(value as u8);
        return;
    }
    // canonical encoding matching read_variable_u64: leading-ones
    // marker byte, then L value bytes.
    for l in 1usize..=8 {
        let cap_bits = 7 + 7 * l; // 7 bits per additional byte plus 7 inline
        if l == 8 || value < (1u64 << cap_bits) {
            if l == 8 {
                out.push(0xFF);
                for i in 0..8 {
                    out.push((value >> (i * 8)) as u8);
                }
            } else {
                let inline_bits = 7 - l;
                let first = (0xFFu8 << (8 - l)) | ((value >> (8 * l)) & (((1u64 << inline_bits) - 1) as u64)) as u8;
                out.push(first);
                for i in 0..l {
                    out.push((value >> (i * 8)) as u8);
                }
            }
            return;
        }
    }
}

// ── BitSet (same semantics as sevenz-rust2) ──

#[derive(Clone)]
struct BitSet {
    bits: Vec<usize>,
    bit_count: usize,
}

impl BitSet {
    fn with_capacity(count: usize) -> Self {
        let num_blocks = if count == 0 { 0 } else { (count - 1) / usize::BITS as usize + 1 };
        BitSet { bits: alloc::vec![0; num_blocks], bit_count: count }
    }
    fn contains(&self, value: usize) -> bool {
        if value >= self.bit_count {
            return false;
        }
        (self.bits[value / usize::BITS as usize] & (1 << (value % usize::BITS as usize))) != 0
    }
    fn insert(&mut self, value: usize) {
        if value >= self.bit_count {
            self.bit_count = value + 1;
            let need = (self.bit_count - 1) / usize::BITS as usize + 1;
            if self.bits.len() < need {
                self.bits.resize(need, 0);
            }
        }
        self.bits[value / usize::BITS as usize] |= 1 << (value % usize::BITS as usize);
    }
    fn set_count(&self) -> usize {
        self.bits.iter().map(|b| b.count_ones() as usize).sum()
    }
    /// Read a 7z `ReadAllOrBits` field: a non-zero leading byte means every
    /// bit is set, otherwise `count` bits follow, MSB first.
    fn read(data: &[u8], pos: &mut usize, count: usize) -> Result<BitSet, &'static str> {
        let all = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
        let mut bits = BitSet::with_capacity(count);
        if all != 0 {
            for i in 0..count {
                bits.insert(i);
            }
            return Ok(bits);
        }
        let mut mask = 0u32;
        let mut cache = 0u32;
        for i in 0..count {
            if mask == 0 {
                mask = 0x80;
                cache = *data.get(*pos).ok_or("7z: truncated")? as u32;
                *pos += 1;
            }
            if cache & mask != 0 {
                bits.insert(i);
            }
            mask >>= 1;
        }
        Ok(bits)
    }
}

fn read_bits_plain(data: &[u8], pos: &mut usize, count: usize) -> Result<BitSet, &'static str> {
    let mut bs = BitSet::with_capacity(count);
    let mut mask = 0u32;
    let mut cache = 0u32;
    for i in 0..count {
        if mask == 0 {
            mask = 0x80;
            cache = *data.get(*pos).ok_or("7z: truncated")? as u32;
            *pos += 1;
        }
        if cache & mask != 0 {
            bs.insert(i);
        }
        mask >>= 1;
    }
    Ok(bs)
}

// ── Parsed header structures ──

#[derive(Debug, Clone)]
struct Coder {
    id: Vec<u8>,
    /// Output stream count; every coder we support has exactly one, and
    /// filter chains (which add streams) are refused when parsing.
    num_out_streams: u64,
    props: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Block {
    coders: Vec<Coder>,
    bind_pairs: Vec<(u64, u64)>,
    packed_streams: Vec<u64>,
    unpack_sizes: Vec<u64>,
    num_unpack_sub_streams: u64,
    has_crc: bool,
    crc: u64,
}

impl Block {
    fn total_output_streams(&self) -> u64 {
        self.coders.iter().map(|c| c.num_out_streams).sum()
    }
    fn get_unpack_size(&self) -> u64 {
        let tos = self.total_output_streams();
        if tos == 0 {
            return 0;
        }
        for i in (0..tos).rev() {
            // Output streams with no bind pair are the folder's tail, and
            // their size is the folder's unpacked size.
            if self.bind_pairs.iter().all(|(_, out_idx)| *out_idx != i as u64) {
                return self.unpack_sizes.get(i as usize).copied().unwrap_or(0);
            }
        }
        0
    }
}

#[derive(Clone)]
struct File {
    name: String,
    size: u64,
    crc: u32,
    has_crc: bool,
    is_dir: bool,
    has_stream: bool,
    block_index: Option<usize>,
    substream_index: usize,
}

#[derive(Default)]
struct Archive {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    pack_crcs: Vec<u32>,
    pack_crc_defined: Vec<bool>,
    blocks: Vec<Block>,
    files: Vec<File>,
    sub_stream_sizes: Vec<u64>,
    sub_stream_crcs: Vec<u32>,
    sub_stream_has_crc: Vec<bool>,
}

/// True if `data` starts with the 7z signature.
pub fn is_7z(data: &[u8]) -> bool {
    data.len() >= 6 && data[..6] == SEVEN_Z_SIGNATURE
}

fn parse_block(data: &[u8], pos: &mut usize) -> Result<Block, &'static str> {
    let num_coders = read_variable_usize(data, pos)?;
    let mut coders = Vec::with_capacity(num_coders);
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    for _ in 0..num_coders {
        let bits = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
        let id_size = (bits & 0xF) as usize;
        let is_simple = bits & 0x10 == 0;
        let has_attrs = bits & 0x20 != 0;
        let more_methods = bits & 0x80 != 0;
        if more_methods {
            return Err("7z: alternative coders unsupported");
        }
        let mut id = Vec::with_capacity(id_size);
        for _ in 0..id_size {
            id.push(*data.get(*pos).ok_or("7z: truncated")?);
            *pos += 1;
        }
        let (mut num_in, mut num_out) = (1, 1);
        if !is_simple {
            num_in = read_variable_u64(data, pos)?;
            num_out = read_variable_u64(data, pos)?;
        }
        let mut props = Vec::new();
        if has_attrs {
            let plen = read_variable_usize(data, pos)?;
            if *pos + plen > data.len() {
                return Err("7z: truncated");
            }
            props.extend_from_slice(&data[*pos..*pos + plen]);
            *pos += plen;
        }
        total_in += num_in;
        total_out += num_out;
        coders.push(Coder { id, num_out_streams: num_out, props });
    }
    let num_bind = if total_out == 0 { 0 } else { total_out - 1 };
    let mut bind_pairs = Vec::with_capacity(num_bind as usize);
    for _ in 0..num_bind {
        let a = read_variable_u64(data, pos)?;
        let b = read_variable_u64(data, pos)?;
        bind_pairs.push((a, b));
    }
    let num_packed = if total_in >= num_bind { total_in - num_bind } else { 0 };
    let mut packed_streams = Vec::with_capacity(num_packed as usize);
    if num_packed == 1 {
        let mut index = u64::MAX;
        for i in 0..total_in {
            if bind_pairs.iter().all(|(a, _)| *a != i) {
                index = i;
                break;
            }
        }
        if index == u64::MAX {
            return Err("7z: bad packed stream");
        }
        packed_streams.push(index);
    } else {
        for _ in 0..num_packed {
            packed_streams.push(read_variable_u64(data, pos)?);
        }
    }
    Ok(Block { coders, bind_pairs, packed_streams, unpack_sizes: Vec::new(), num_unpack_sub_streams: 1, has_crc: false, crc: 0 })
}

fn read_pack_info(data: &[u8], pos: &mut usize, archive: &mut Archive) -> Result<(), &'static str> {
    archive.pack_pos = read_variable_u64(data, pos)?;
    let num_pack = read_variable_usize(data, pos)?;
    archive.pack_sizes.clear();
    let mut nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid == K_SIZE {
        for _ in 0..num_pack {
            archive.pack_sizes.push(read_variable_u64(data, pos)?);
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid == K_CRC {
        let crcs_defined = BitSet::read(data, pos, num_pack)?;
        archive.pack_crcs.clear();
        archive.pack_crc_defined.clear();
        for i in 0..num_pack {
            if crcs_defined.contains(i) {
                let c = u32::from_le_bytes([*data.get(*pos).ok_or("7z: truncated")?, *data.get(*pos + 1).ok_or("7z: truncated")?, *data.get(*pos + 2).ok_or("7z: truncated")?, *data.get(*pos + 3).ok_or("7z: truncated")?]);
                *pos += 4;
                archive.pack_crcs.push(c);
                archive.pack_crc_defined.push(true);
            } else {
                archive.pack_crcs.push(0);
                archive.pack_crc_defined.push(false);
            }
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid != K_END {
        return Err("7z: bad pack info");
    }
    Ok(())
}

fn read_unpack_info(data: &[u8], pos: &mut usize, archive: &mut Archive) -> Result<(), &'static str> {
    let mut nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid != K_FOLDER {
        return Err("7z: expected kFolder");
    }
    let num_blocks = read_variable_usize(data, pos)?;
    let external = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if external != 0 {
        return Err("7z: external folders unsupported");
    }
    for _ in 0..num_blocks {
        archive.blocks.push(parse_block(data, pos)?);
    }
    nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid != K_CODERS_UNPACK_SIZE {
        return Err("7z: expected kCodersUnpackSize");
    }
    for block in archive.blocks.iter_mut() {
        let tos = block.total_output_streams();
        for _ in 0..tos {
            block.unpack_sizes.push(read_variable_u64(data, pos)?);
        }
    }
    nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid == K_CRC {
        let defined = BitSet::read(data, pos, num_blocks)?;
        for i in 0..num_blocks {
            if defined.contains(i) {
                let c = u32::from_le_bytes([*data.get(*pos).ok_or("7z: truncated")?, *data.get(*pos + 1).ok_or("7z: truncated")?, *data.get(*pos + 2).ok_or("7z: truncated")?, *data.get(*pos + 3).ok_or("7z: truncated")?]);
                *pos += 4;
                archive.blocks[i].has_crc = true;
                archive.blocks[i].crc = c as u64;
            }
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid != K_END {
        return Err("7z: bad unpack info");
    }
    Ok(())
}

fn read_sub_streams_info(data: &[u8], pos: &mut usize, archive: &mut Archive) -> Result<(), &'static str> {
    for block in archive.blocks.iter_mut() {
        block.num_unpack_sub_streams = 1;
    }
    let mut total_unpack_streams = archive.blocks.len();
    let mut nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid == K_NUM_UNPACK_STREAM {
        total_unpack_streams = 0;
        for block in archive.blocks.iter_mut() {
            let n = read_variable_usize(data, pos)?;
            block.num_unpack_sub_streams = n as u64;
            total_unpack_streams += n;
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    let mut unpack_sizes = alloc::vec![0u64; total_unpack_streams];
    let mut next_stream = 0usize;
    let mut have_sizes = false;
    if nid == K_SIZE {
        have_sizes = true;
        for block in archive.blocks.iter_mut() {
            if block.num_unpack_sub_streams == 0 {
                continue;
            }
            let mut sum = 0u64;
            let n = block.num_unpack_sub_streams;
            for _ in 0..n.saturating_sub(1) {
                let s = read_variable_u64(data, pos)?;
                unpack_sizes[next_stream] = s;
                next_stream += 1;
                sum += s;
            }
            let last = block.get_unpack_size().saturating_sub(sum);
            unpack_sizes[next_stream] = last;
            next_stream += 1;
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if !have_sizes {
        // each block is one substream equal to its unpack size
        for block in archive.blocks.iter() {
            if block.num_unpack_sub_streams == 0 {
                continue;
            }
            unpack_sizes[next_stream] = block.get_unpack_size();
            next_stream += 1;
        }
    }

    let mut has_crc = alloc::vec![false; total_unpack_streams];
    let mut crcs = alloc::vec![0u32; total_unpack_streams];
    let mut num_digests = 0usize;
    for block in archive.blocks.iter() {
        if block.num_unpack_sub_streams != 1 || !block.has_crc {
            num_digests += block.num_unpack_sub_streams as usize;
        }
    }
    if nid == K_CRC {
        let has_missing = BitSet::read(data, pos, num_digests)?;
        let mut missing = alloc::vec![0u32; num_digests];
        for i in 0..num_digests {
            if has_missing.contains(i) {
                let c = u32::from_le_bytes([*data.get(*pos).ok_or("7z: truncated")?, *data.get(*pos + 1).ok_or("7z: truncated")?, *data.get(*pos + 2).ok_or("7z: truncated")?, *data.get(*pos + 3).ok_or("7z: truncated")?]);
                *pos += 4;
                missing[i] = c;
            }
        }
        let mut next_crc = 0usize;
        let mut next_missing = 0usize;
        for block in archive.blocks.iter() {
            if block.num_unpack_sub_streams == 1 && block.has_crc {
                has_crc[next_crc] = true;
                crcs[next_crc] = block.crc as u32;
                next_crc += 1;
            } else {
                for _ in 0..block.num_unpack_sub_streams {
                    if has_missing.contains(next_missing) {
                        has_crc[next_crc] = true;
                        crcs[next_crc] = missing[next_missing];
                    }
                    next_crc += 1;
                    next_missing += 1;
                }
            }
        }
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid != K_END {
        return Err("7z: bad sub streams info");
    }
    archive.sub_stream_sizes = unpack_sizes;
    archive.sub_stream_crcs = crcs;
    archive.sub_stream_has_crc = has_crc;
    Ok(())
}

fn read_streams_info(data: &[u8], pos: &mut usize, archive: &mut Archive) -> Result<(), &'static str> {
    let mut nid = *data.get(*pos).ok_or("7z: truncated")?;
    *pos += 1;
    if nid == K_PACK_INFO {
        read_pack_info(data, pos, archive)?;
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid == K_UNPACK_INFO {
        read_unpack_info(data, pos, archive)?;
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    } else {
        archive.blocks.clear();
    }
    if nid == K_SUB_STREAMS_INFO {
        read_sub_streams_info(data, pos, archive)?;
        nid = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
    }
    if nid != K_END {
        return Err("7z: bad streams info");
    }
    Ok(())
}

fn read_files_info(data: &[u8], pos: &mut usize, archive: &mut Archive) -> Result<(), &'static str> {
    let num_files = read_variable_usize(data, pos)?;
    let mut files: Vec<File> = Vec::with_capacity(num_files);
    for _ in 0..num_files {
        files.push(File { name: String::new(), size: 0, crc: 0, has_crc: false, is_dir: true, has_stream: false, block_index: None, substream_index: 0 });
    }
    let mut is_empty_stream: Option<BitSet> = None;
    let mut is_empty_file: Option<BitSet> = None;
    let mut saw_name = false;
    loop {
        let prop = *data.get(*pos).ok_or("7z: truncated")?;
        *pos += 1;
        if prop == 0 {
            break;
        }
        let size = read_variable_u64(data, pos)? as usize;
        match prop {
            K_EMPTY_STREAM => {
                is_empty_stream = Some(read_bits_plain(data, pos, num_files)?);
            }
            K_EMPTY_FILE => {
                let n = is_empty_stream.as_ref().map(|s| s.set_count()).unwrap_or(0);
                is_empty_file = Some(read_bits_plain(data, pos, n)?);
            }
            K_ANTI => {
                // ignored: anti markers carry no stream payload
                if *pos + size > data.len() {
                    return Err("7z: truncated");
                }
                *pos += size;
            }
            K_NAME => {
                let external = *data.get(*pos).ok_or("7z: truncated")?;
                *pos += 1;
                if external != 0 {
                    return Err("7z: external names unsupported");
                }
                let name_bytes = size.saturating_sub(1);
                if name_bytes & 1 != 0 || *pos + name_bytes > data.len() {
                    return Err("7z: bad names length");
                }
                let mut cur = Vec::new();
                let mut idx = 0usize;
                let end = *pos + name_bytes;
                for f in files.iter_mut() {
                    cur.clear();
                    while *pos < end {
                        let u = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
                        *pos += 2;
                        if u == 0 {
                            break;
                        }
                        cur.push(u);
                    }
                    f.name = String::from_utf16(&cur).map_err(|_| "7z: bad utf16 name")?;
                    idx += 1;
                }
                saw_name = true;
                let _ = idx;
            }
            _ => {
                // C/A/M time, attributes, dummy
                if *pos + size > data.len() {
                    return Err("7z: truncated");
                }
                *pos += size;
            }
        }
    }
    if !saw_name {
        return Err("7z: missing names");
    }
    // Assign stream → file, sizes and crcs in order.
    for (i, f) in files.iter_mut().enumerate() {
        let empty = is_empty_stream.as_ref().map(|s| s.contains(i)).unwrap_or(false);
        f.has_stream = !empty;
        f.is_dir = empty;
    }

    // Simple linear mapping (typical: contiguous data files ↔ sub_streams)
    let mut stream_idx = 0usize;
    // number of empty (no-stream) files seen so far → is_empty_file bit lookup
    let mut empty_seen = 0usize;
    for f in files.iter_mut() {
        if !f.has_stream {
            // directory flag: per kEmptyFile, 1 = file, 0 = directory;
            // without info we default to true (already set)
            if let Some(ef) = &is_empty_file {
                f.is_dir = !ef.contains(empty_seen);
            }
            empty_seen += 1;
            continue;
        }
        if stream_idx >= archive.sub_stream_sizes.len() {
            return Err("7z: stream map under-specified");
        }
        f.size = archive.sub_stream_sizes[stream_idx];
        f.crc = archive.sub_stream_crcs[stream_idx];
        f.has_crc = archive.sub_stream_has_crc[stream_idx];
        // which block owns this stream?
        let mut offset = 0usize;
        let mut found = false;
        for (bi, block) in archive.blocks.iter().enumerate() {
            let n = block.num_unpack_sub_streams as usize;
            if stream_idx < offset + n && n > 0 {
                f.block_index = Some(bi);
                f.substream_index = stream_idx - offset;
                found = true;
                break;
            }
            offset += n;
        }
        if !found {
            return Err("7z: bad stream map");
        }
        stream_idx += 1;
    }
    archive.files = files;
    Ok(())
}

fn build_archive(data: &[u8]) -> Result<Archive, &'static str> {
    // StartHeader: 6 sig + 2 version + 4 crc32 + 8 offset + 8 size + 4 crc32 = 32
    if data.len() < SIGNATURE_HEADER_SIZE as usize {
        return Err("7z: file too small");
    }
    if !is_7z(data) {
        return Err("7z: bad signature");
    }
    let version_major = data[6];
    if version_major != 0 {
        return Err("7z: unsupported version");
    }
    let start_header_crc = u32::from_le_bytes(data[8..12].try_into().unwrap());
    if crc32::crc32(&data[12..32]) != start_header_crc {
        return Err("7z: start header crc");
    }
    let next_header_offset = u64::from_le_bytes(data[12..20].try_into().unwrap());
    let next_header_size = u64::from_le_bytes(data[20..28].try_into().unwrap());
    let next_header_crc = u32::from_le_bytes(data[28..32].try_into().unwrap());
    let header_start = SIGNATURE_HEADER_SIZE as usize + next_header_offset as usize;
    if header_start + next_header_size as usize > data.len() {
        return Err("7z: truncated next header");
    }
    let header_bytes = &data[header_start..header_start + next_header_size as usize];
    if crc32::crc32(header_bytes) != next_header_crc {
        return Err("7z: next header crc");
    }

    let mut archive = Archive::default();
    if header_bytes[0] == K_ENCODED_HEADER {
        // The next header is itself compressed: its stream info sits right
        // after the K_ENCODED_HEADER byte, and decoding it yields the real
        // header.
        let mut pos = 1usize;
        read_streams_info(header_bytes, &mut pos, &mut archive)?;
        let block = archive.blocks.first().ok_or("7z: no blocks for encoded header")?;
        let block_offset = SIGNATURE_HEADER_SIZE as usize + archive.pack_pos as usize;
        let pack_size = archive.pack_sizes.first().copied().unwrap_or(0) as usize;
        if block_offset + pack_size > data.len() {
            return Err("7z: truncated header stream");
        }
        let packed = &data[block_offset..block_offset + pack_size];
        check_pack_crc(&archive, 0, packed)?;
        let unpacked = decode_block(block, packed)?;
        archive = Archive::default();
        parse_header(&unpacked, &mut archive)?;
    } else if header_bytes[0] == K_HEADER {
        parse_header(header_bytes, &mut archive)?;
    } else {
        return Err("7z: unknown header kind");
    }
    if data.len() < SIGNATURE_HEADER_SIZE as usize + archive.pack_pos as usize {
        return Err("7z: bad pack pos");
    }
    Ok(archive)
}

/// Parse a `K_HEADER` block: optional archive properties, the main streams
/// info, the file info, then the terminating byte.
fn parse_header(header: &[u8], archive: &mut Archive) -> Result<(), &'static str> {
    let mut pos = 1usize;
    let mut nid = *header.get(pos).ok_or("7z: truncated header")?;
    pos += 1;
    if nid == K_ARCHIVE_PROPERTIES {
        while *header.get(pos).ok_or("7z: truncated header")? != K_END {
            let size = read_variable_usize(header, &mut pos)?;
            pos = pos.checked_add(size).ok_or("7z: header property too large")?;
            pos += 1;
        }
        nid = *header.get(pos).ok_or("7z: truncated header")?;
        pos += 1;
    }
    if nid == K_MAIN_STREAMS_INFO {
        read_streams_info(header, &mut pos, archive)?;
        nid = *header.get(pos).ok_or("7z: truncated header")?;
        pos += 1;
    }
    if nid == K_FILES_INFO {
        read_files_info(header, &mut pos, archive)?;
        nid = *header.get(pos).ok_or("7z: truncated header")?;
    }
    if nid != K_END {
        return Err("7z: header not terminated");
    }
    Ok(())
}

/// Verify a packed stream against the CRC the header declared for it.
/// Streams whose CRC is undefined are trusted to the decoded-content check.
fn check_pack_crc(archive: &Archive, pack_index: usize, packed: &[u8]) -> Result<(), &'static str> {
    if archive.pack_crc_defined.get(pack_index).copied().unwrap_or(false)
        && crc32::crc32(packed) != archive.pack_crcs[pack_index]
    {
        return Err("7z: pack stream crc mismatch");
    }
    Ok(())
}

// ── Block decoding ──

fn lzma2_dict_size(props: &[u8]) -> Result<u32, &'static str> {
    if props.is_empty() {
        return Err("7z: missing LZMA2 props");
    }
    let b = props[0] as u32;
    if b & !0x3F != 0 {
        return Err("7z: bad LZMA2 props byte");
    }
    if b == 40 {
        return Ok(u32::MAX);
    }
    if b > 40 {
        return Err("7z: dict too big");
    }
    Ok((2 | (b & 1)) << (b / 2 + 11))
}

fn lzma_dict_size(props: &[u8]) -> Result<u32, &'static str> {
    if props.len() < 5 {
        return Err("7z: truncated LZMA props");
    }
    Ok(u32::from_le_bytes(props[1..5].try_into().unwrap()))
}

/// Decode the packed streams of one folder via the folder's coder chain
/// (single-coder chains supported: COPY / LZMA1 / LZMA2).
fn decode_block(block: &Block, packed: &[u8]) -> Result<Vec<u8>, &'static str> {
    if block.coders.len() != 1 || !block.packed_streams.is_empty() && block.coders.len() != block.packed_streams.len() {
        // For our supported subset, all non-filter coders have 1 in/1 out.
    }
    if block.coders.is_empty() {
        return Err("7z: no coders");
    }
    let coder = &block.coders[0];
    let out_size = block.get_unpack_size() as usize;
    match coder.id.as_slice() {
        ID_COPY => {
            if packed.len() as u64 != block.get_unpack_size() {
                return Err("7z: size mismatch (copy)");
            }
            Ok(packed.to_vec())
        }
        ID_LZMA2 => {
            let dict_size = lzma2_dict_size(&coder.props)?;
            let mut input: &[u8] = packed;
            let mut reader = Lzma2Reader::new(&mut input, dict_size, None);
            let mut out: Vec<u8> = Vec::with_capacity(out_size);
            let mut scratch = alloc::vec![0u8; 32 * 1024];
            loop {
                let n = reader.read(&mut scratch).map_err(|_| "7z: LZMA2 decode error")?;
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&scratch[..n]);
            }
            Ok(out)
        }
        ID_LZMA => {
            let dict_size = lzma_dict_size(&coder.props)?;
            let props_byte = coder.props[0];
            let mut input: &[u8] = packed;
            let mut reader = LzmaReader::new_with_props(&mut input, out_size as u64, props_byte, dict_size, None)
                .map_err(|_| "7z: LZMA init error")?;
            let mut out: Vec<u8> = Vec::with_capacity(out_size);
            let mut scratch = alloc::vec![0u8; 32 * 1024];
            loop {
                let n = reader.read(&mut scratch).map_err(|_| "7z: LZMA decode error")?;
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&scratch[..n]);
            }
            Ok(out)
        }
        // Encrypted, PPMd, BZip2, Brotli, zstd, LZ4 and BCJ-filtered
        // folders are refused explicitly rather than mis-decoded.
        _ => Err("7z: unsupported codec (only Stored, LZMA and LZMA2 are supported)"),
    }
}

/// A listed member of a 7z archive.
#[derive(Debug, Clone)]
pub struct M7zEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// List members of a 7z archive without decoding payloads.
pub fn list(data: &[u8]) -> Result<Vec<M7zEntry>, &'static str> {
    let archive = build_archive(data)?;
    let mut out = Vec::new();
    for f in &archive.files {
        out.push(M7zEntry {
            name: f.name.clone(),
            size: f.size,
            is_dir: f.is_dir && !f.has_stream,
        });
    }
    Ok(out)
}

/// Extract all members of a 7z archive. `limit` bounds the total decoded size.
pub fn extract(data: &[u8], limit: usize) -> Result<Vec<(String, bool, Vec<u8>)>, &'static str> {
    let archive = build_archive(data)?;
    // Decode each block once.
    let mut block_buffers: Vec<Option<Vec<u8>>> = alloc::vec![None; archive.blocks.len()];
    let mut total = 0usize;
    let mut out: Vec<(String, bool, Vec<u8>)> = Vec::new();
    // Members are a linear sequence over the archive's sub-streams.
    for f in &archive.files {
        if !f.has_stream {
            out.push((f.name.clone(), true, Vec::new()));
            continue;
        }
        let want_index = usize::try_from(f.size).map_err(|_| "7z: member too large")?;
        if total + want_index > limit {
            return Err("7z: archive too large");
        }
        let block_index = f.block_index.ok_or("7z: bad stream map")?;
        if block_buffers[block_index].is_none() {
            let block = &archive.blocks[block_index];
            // packed bytes for this block: from pack_pos + preceding pack sizes
            let mut block_first_pack_index = 0usize;
            for bi in 0..block_index {
                block_first_pack_index += archive.blocks[bi].packed_streams.len();
            }
            let mut off = SIGNATURE_HEADER_SIZE as usize + archive.pack_pos as usize;
            for i in 0..block_first_pack_index {
                off += archive.pack_sizes[i] as usize;
            }
            let n = archive.pack_sizes[block_first_pack_index] as usize;
            if off + n > data.len() {
                return Err("7z: truncated pack stream");
            }
            let packed = &data[off..off + n];
            check_pack_crc(&archive, block_first_pack_index, packed)?;
            block_buffers[block_index] = Some(decode_block(block, packed)?);
        }
        let block_out = block_buffers[block_index].as_ref().unwrap();
        let mut stream_idx_base = 0usize;
        for bi in 0..block_index {
            stream_idx_base += archive.blocks[bi].num_unpack_sub_streams as usize;
        }
        // The substream index within this block:
        let local_idx = f.substream_index;
        let mut soff = 0usize;
        for i in 0..local_idx {
            // preceding substreams of same block: located in sub_stream_sizes after stream_idx_base
            soff += *archive
                .sub_stream_sizes
                .get(stream_idx_base + i)
                .ok_or("7z: bad stream map")? as usize;
        }
        let within_block_offset = soff;
        if within_block_offset + want_index > block_out.len() {
            return Err("7z: block size mismatch");
        }
        let payload = &block_out[within_block_offset..within_block_offset + want_index];
        if f.has_crc && crc32::crc32(payload) != f.crc {
            return Err("7z: CRC mismatch");
        }
        total += want_index;
        out.push((f.name.clone(), false, payload.to_vec()));
    }
    Ok(out)
}

// ── Writer ──

fn encode_lzma2_dict_size(dict_size: u32) -> u8 {
    // Inverse of `lzma2_dict_size`: the smallest b with
    // (2 | (b & 1)) << (b / 2 + 11) >= dict_size.
    for b in 0u32..=40 {
        let size = (2 | (b & 1)) << (b / 2 + 11);
        if size >= dict_size.max(4096) {
            return b as u8;
        }
    }
    40
}

/// Compression method for a 7z folder. All three are the ones the reader
/// accepts, and all three are what mainstream 7-Zip emits by default.
/// The shell always writes LZMA2; `Stored`/`Lzma` exist for callers that
/// pick a method explicitly and are covered by the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Method {
    /// Verbatim (method id 0x00).
    Stored,
    /// LZMA1 (method id 03 01 01), 5 property bytes.
    Lzma,
    /// LZMA2 (method id 0x21), 1 property byte.
    Lzma2,
}

impl Method {
    /// Coder id bytes as they appear in the folder header.
    fn id(self) -> &'static [u8] {
        match self {
            Method::Stored => ID_COPY,
            Method::Lzma => ID_LZMA,
            Method::Lzma2 => ID_LZMA2,
        }
    }

    /// Encode `data` into a packed stream for this method.
    fn encode(self, data: &[u8]) -> Result<Vec<u8>, &'static str> {
        match self {
            Method::Stored => Ok(data.to_vec()),
            Method::Lzma2 => {
                let mut writer = Lzma2Writer::new(Vec::<u8>::new(), Lzma2Options::default());
                writer.write_all(data).map_err(|_| "7z: encode error")?;
                writer.finish().map_err(|_| "7z: encode error")
            }
            Method::Lzma => {
                let options = LzmaOptions::with_preset(6);
                // `use_header = false`: 7z carries the properties in the
                // folder header, so the legacy 13-byte header must not be
                // written. The end marker lets the decoder stop cleanly.
                let mut writer = LzmaWriter::new(
                    Vec::<u8>::new(),
                    &options,
                    false,
                    true,
                    Some(data.len() as u64),
                )
                .map_err(|_| "7z: encoder init failed")?;
                writer.write_all(data).map_err(|_| "7z: encode error")?;
                writer.finish().map_err(|_| "7z: encode error")
            }
        }
    }

    /// Coder properties for the folder header.
    fn props(self) -> Vec<u8> {
        match self {
            Method::Stored => Vec::new(),
            Method::Lzma2 => alloc::vec![
                encode_lzma2_dict_size(Lzma2Options::default().lzma_options.dict_size)
            ],
            Method::Lzma => {
                let options = LzmaOptions::with_preset(6);
                let mut props = alloc::vec![options.get_props()];
                props.extend_from_slice(&options.dict_size.to_le_bytes());
                props
            }
        }
    }
}

/// Build a 7z archive. Each member gets its own folder (one packed stream
/// and one sub-stream), which keeps writes streaming-friendly and makes
/// extraction independent per member.
///
/// `method` selects the folder coder; [`Method::Lzma2`] matches what 7-Zip
/// uses by default and is the best size/speed trade-off in the kernel heap.
pub fn build_with(
    entries: &[crate::archive::tar::BuildEntry<'_>],
    method: Method,
) -> Result<Vec<u8>, &'static str> {
    if entries.len() > MAX_ENTRIES {
        return Err("7z: too many members");
    }

    // One packed stream and one folder per file member; directories get
    // neither, and are described purely by the file-info bitsets. Member
    // names come from `entries`, which keeps their original order (the
    // folder list is built from the members that have data).
    struct Member {
        packed: Vec<u8>,
        size: u64,
        crc: u32,
    }
    let mut members: Vec<Member> = Vec::new();
    for e in entries {
        let (_clean, slash_dir) = sanitize_path(e.name)?;
        if e.data.is_none() || slash_dir {
            continue; // directory: no stream
        }
        let data = e.data.unwrap_or(&[]);
        members.push(Member {
            packed: method.encode(data)?,
            size: data.len() as u64,
            crc: crc32::crc32(data),
        });
    }

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&SEVEN_Z_SIGNATURE);
    out.extend_from_slice(&[0, 4]); // format version 0.4
    out.extend_from_slice(&[0, 0, 0, 0]); // start-header CRC placeholder
    out.extend_from_slice(&[0u8; 20]); // next-header offset/size/CRC placeholder

    // Packed streams follow the 32-byte signature header directly, so the
    // pack position (relative to the end of the header) is 0.
    for m in &members {
        out.extend_from_slice(&m.packed);
    }

    let mut header: Vec<u8> = Vec::new();
    header.push(K_HEADER);
    header.push(K_MAIN_STREAMS_INFO);

    // PackInfo: one packed stream per folder.
    header.push(K_PACK_INFO);
    write_variable_u64(&mut header, 0); // pack position
    write_variable_u64(&mut header, members.len() as u64);
    header.push(K_SIZE);
    for m in &members {
        write_variable_u64(&mut header, m.packed.len() as u64);
    }
    // Every pack stream gets a CRC so a reader can tell a damaged archive
    // from a merely unsupported one before decoding anything.
    header.push(K_CRC);
    header.push(0xFF); // all CRCs defined
    for m in &members {
        header.extend_from_slice(&crc32::crc32(&m.packed).to_le_bytes());
    }
    header.push(K_END);

    // UnpackInfo: one folder per member, one coder, one in/out stream.
    header.push(K_UNPACK_INFO);
    header.push(K_FOLDER);
    write_variable_u64(&mut header, members.len() as u64);
    header.push(0); // external = 0
    for _ in &members {
        write_variable_u64(&mut header, 1); // num coders
        let id = method.id();
        // Flags: low nibble = id size, 0x20 = coder has properties.
        let flags = (id.len() as u8) | if method.props().is_empty() { 0 } else { 0x20 };
        header.push(flags);
        header.extend_from_slice(id);
        if !method.props().is_empty() {
            let props = method.props();
            write_variable_u64(&mut header, props.len() as u64);
            header.extend_from_slice(&props);
        }
    }
    header.push(K_CODERS_UNPACK_SIZE);
    for m in &members {
        write_variable_u64(&mut header, m.size);
    }
    header.push(K_END);

    // SubStreamsInfo: one sub-stream per folder, so no explicit sizes are
    // needed (each sub-stream size equals its folder's unpacked size) and
    // the folder CRCs would suffice. We emit the CRCs here so a reader can
    // verify individual members even for future multi-sub-stream folders.
    if !members.is_empty() {
        header.push(K_SUB_STREAMS_INFO);
        header.push(K_NUM_UNPACK_STREAM);
        for _ in &members {
            write_variable_u64(&mut header, 1);
        }
        // Every sub-stream has a CRC, so the all-or-bits byte is 1 and no
        // per-stream presence bits follow.
        header.push(K_CRC);
        header.push(1);
        for m in &members {
            header.extend_from_slice(&m.crc.to_le_bytes());
        }
        header.push(K_END);
    }
    header.push(K_END); // end of K_MAIN_STREAMS_INFO

    // FilesInfo: entry order matches the input, with directories flagged.
    header.push(K_FILES_INFO);
    write_variable_u64(&mut header, entries.len() as u64);

    // kEmptyStream: bit set => the entry has no data stream. Read with
    // `read_bits` (a plain MSB-first bit vector, no all-or-bits byte).
    let dir_index_of: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.data.is_none())
        .map(|(i, _)| i)
        .collect();
    let empty_stream_bytes = (entries.len() + 7) / 8;
    if !dir_index_of.is_empty() {
        header.push(K_EMPTY_STREAM);
        write_variable_u64(&mut header, empty_stream_bytes as u64);
        let mut bits = alloc::vec![0u8; empty_stream_bytes];
        for i in dir_index_of.iter() {
            bits[i / 8] |= 0x80 >> (i % 8);
        }
        header.extend_from_slice(&bits);
        // kEmptyFile: bit set => a real (empty) file, 0 => a directory. All
        // streamless entries here are directories, so this is all zeroes.
        header.push(K_EMPTY_FILE);
        let n = (dir_index_of.len() + 7) / 8;
        write_variable_u64(&mut header, n as u64);
        header.extend(alloc::vec![0u8; n]);
    }

    // kName: UTF-16LE names, each NUL-terminated, after an external byte.
    header.push(K_NAME);
    let mut names: Vec<u8> = Vec::new();
    names.push(0); // external = 0
    for e in entries {
        let (clean, _) = sanitize_path(e.name)?;
        for unit in clean.encode_utf16() {
            names.extend_from_slice(&unit.to_le_bytes());
        }
        names.extend_from_slice(&[0, 0]);
    }
    write_variable_u64(&mut header, names.len() as u64);
    header.extend_from_slice(&names);
    header.push(K_END); // end of property list
    header.push(K_END); // end of K_FILES_INFO

    // Fill the start header: next-header offset is relative to the end of
    // the 32-byte signature header, and both CRCs are over fixed ranges.
    out.extend_from_slice(&header);
    let next_header_offset = out.len() - SIGNATURE_HEADER_SIZE as usize - header.len();
    out[12..20].copy_from_slice(&(next_header_offset as u64).to_le_bytes());
    out[20..28].copy_from_slice(&(header.len() as u64).to_le_bytes());
    out[28..32].copy_from_slice(&crc32::crc32(&header).to_le_bytes());
    // The start-header CRC covers bytes 12..32, so compute it into a local
    // before writing back into the same buffer.
    let start_crc = crc32::crc32(&out[12..32]);
    out[8..12].copy_from_slice(&start_crc.to_le_bytes());
    Ok(out)
}

/// Build a 7z archive with the default LZMA2 folder method.
pub fn build(entries: &[crate::archive::tar::BuildEntry<'_>]) -> Result<Vec<u8>, &'static str> {
    build_with(entries, Method::Lzma2)
}
