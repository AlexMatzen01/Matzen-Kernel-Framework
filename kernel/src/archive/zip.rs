//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Minimal ZIP reader/writer (`no_std`, `alloc` only).
//!
//! Container format follows PKWARE APPNOTE (6.3.9): local file headers
//! (`PK\x03\x04`), central directory (`PK\x01\x02`), end of central
//! directory (`PK\x05\x06`). Compression methods supported:
//! - Stored (0): verbatim copy, both directions.
//! - Deflated (8): raw DEFLATE via `miniz_oxide` (same backend as gzip).
//!
//! Deliberately out of scope (clear errors, never silent corruption):
//! Zip64, multi-disk, encryption, patched data, and exotic methods
//! (BZip2/LZMA/PPMd/...). Data-descriptor archives (streaming writers)
//! *are* accepted by trusting the central-directory sizes.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;

use super::crc32;
use super::tar::sanitize_path;

pub const METHOD_STORED: u16 = 0;
pub const METHOD_DEFLATED: u16 = 8;

/// Hard cap on entries per archive (bounds heap + shell loop time).
pub const MAX_ENTRIES: usize = 4096;

const SIG_LOCAL: [u8; 4] = [b'P', b'K', 3, 4];
const SIG_CENTRAL: [u8; 4] = [b'P', b'K', 1, 2];
const SIG_EOCD: [u8; 4] = [b'P', b'K', 5, 6];

/// A fully extracted member. Stored members borrow the source archive;
/// deflated members are owned (freshly decompressed). This keeps peak
/// heap near one copy of the payload, not two.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// Sanitized relative path with `/` separators.
    pub name: String,
    /// Payload (empty for directories).
    pub data: Cow<'a, [u8]>,
    /// True for directory members.
    pub is_dir: bool,
}

/// Member metadata for `unzip -l` (no payload buffered).
#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub name: String,
    pub method: u16,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub is_dir: bool,
}

/// A member to write: `data = None` encodes a directory.
pub struct BuildEntry<'a> {
    pub name: &'a str,
    pub data: Option<&'a [u8]>,
}

/// Compression choice for [`build`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pack {
    /// Best for speed/size balance (raw DEFLATE level 6).
    Deflated,
    /// Verbatim (fastest, biggest).
    Stored,
}

// ── little-endian helpers ────────────────────────────────────────────

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn name_from(bytes: &[u8]) -> Result<String, &'static str> {
    // Bit 11 (UTF-8) is the modern norm; historical names are CP437. MFK
    // filenames must be valid UTF-8 for SimplFS, so require UTF-8 here and
    // reject the rest with a clear error instead of guessing a codepage.
    core::str::from_utf8(bytes)
        .map(String::from)
        .map_err(|_| "zip: non-UTF8 member name")
}

// ── central directory scan ───────────────────────────────────────────

struct CentralEntry {
    info: EntryInfo,
    crc: u32,
    local_offset: u64,
    flags: u16,
}

fn find_eocd(data: &[u8]) -> Result<usize, &'static str> {
    // EOCD is at least 22 bytes; a comment of up to 64 KiB may follow it.
    if data.len() < 22 {
        return Err("zip: file too small");
    }
    let start = data.len().saturating_sub(22 + 0xFFFF);
    let mut i = data.len() - 22;
    loop {
        if data[i..].starts_with(&SIG_EOCD) {
            return Ok(i);
        }
        if i == start {
            break;
        }
        i -= 1;
    }
    Err("zip: end-of-central-directory not found")
}

fn parse_central(data: &[u8]) -> Result<Vec<CentralEntry>, &'static str> {
    let eocd = find_eocd(data)?;
    let e = &data[eocd..];
    if e.len() < 22 {
        return Err("zip: truncated end record");
    }
    let disk_no = le16(&e[4..]);
    let cd_disk = le16(&e[6..]);
    let cd_count_disk = le16(&e[8..]);
    let cd_count = le16(&e[10..]);
    let cd_size = le32(&e[12..]) as usize;
    let cd_offset = le32(&e[16..]) as usize;
    if disk_no != 0 || cd_disk != 0 || cd_count_disk != cd_count {
        return Err("zip: multi-disk archives not supported");
    }
    if cd_count as usize > MAX_ENTRIES {
        return Err("zip: too many members");
    }
    if cd_offset >= data.len() || cd_offset + cd_size > data.len() {
        return Err("zip: bad central directory range");
    }
    if cd_size >= data.len() {
        return Err("zip: bad central directory range");
    }

    let mut out = Vec::with_capacity(cd_count as usize);
    let mut pos = cd_offset;
    for _ in 0..cd_count {
        if pos + 46 > data.len() || !data[pos..].starts_with(&SIG_CENTRAL) {
            return Err("zip: bad central directory entry");
        }
        let c = &data[pos..];
        let flags = le16(&c[8..]);
        let method = le16(&c[10..]);
        let crc = le32(&c[16..]);
        let comp = le32(&c[20..]);
        let uncomp = le32(&c[24..]);
        let name_len = le16(&c[28..]) as usize;
        let extra_len = le16(&c[30..]) as usize;
        let comment_len = le16(&c[32..]) as usize;
        let disk_start = le16(&c[34..]);
        let ext_attr = le32(&c[38..]);
        let local_offset = le32(&c[42..]) as u64;
        pos += 46;
        if disk_start != 0 {
            return Err("zip: multi-disk archives not supported");
        }
        if pos + name_len + extra_len + comment_len > data.len() {
            return Err("zip: truncated central directory");
        }
        let raw_name = &data[pos..pos + name_len];
        pos += name_len + extra_len + comment_len;
        // Zip64 sentinels: real values live in extra fields (unsupported).
        if comp == 0xFFFF_FFFF || uncomp == 0xFFFF_FFFF || local_offset == 0xFFFF_FFFF {
            return Err("zip: Zip64 not supported");
        }
        let name = name_from(raw_name)?;
        let (clean, slash_dir) = sanitize_path(&name)?;
        let dos_dir = ext_attr & 0x10 != 0;
        out.push(CentralEntry {
            info: EntryInfo {
                name: clean,
                method,
                comp_size: comp as u64,
                uncomp_size: uncomp as u64,
                is_dir: slash_dir || dos_dir,
            },
            crc,
            local_offset,
            flags,
        });
    }
    Ok(out)
}

/// True if `data` starts with a local file header signature.
pub fn is_zip(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == SIG_LOCAL
}

// ── extraction ───────────────────────────────────────────────────────

fn extract_one<'a>(
    data: &'a [u8],
    e: &CentralEntry,
    limit: usize,
) -> Result<Cow<'a, [u8]>, &'static str> {
    if e.flags & 0x01 != 0 {
        return Err("zip: encrypted members not supported");
    }
    if e.info.method != METHOD_STORED && e.info.method != METHOD_DEFLATED {
        return Err("zip: unsupported compression method");
    }
    let off = e.local_offset as usize;
    if off + 30 > data.len() || !data[off..].starts_with(&SIG_LOCAL) {
        return Err("zip: bad local header");
    }
    let l = &data[off..];
    let l_method = le16(&l[8..]);
    let l_name = le16(&l[26..]) as usize;
    let l_extra = le16(&l[28..]) as usize;
    // With bit 3 (data descriptor), local sizes/method may be zeroed; the
    // central directory holds the truth. Otherwise cross-check.
    if e.flags & 0x08 == 0 && l_method != e.info.method {
        return Err("zip: header/method mismatch");
    }
    let start = off
        .checked_add(30 + l_name + l_extra)
        .ok_or("zip: bad local header")?;
    let end = (start as u64)
        .checked_add(e.info.comp_size)
        .ok_or("zip: member too large")? as usize;
    if end > data.len() {
        return Err("zip: truncated member data");
    }
    let raw = &data[start..end];

    let payload: Cow<'a, [u8]> = if e.info.is_dir {
        if e.info.uncomp_size != 0 {
            return Err("zip: bad directory member");
        }
        Cow::Borrowed(&[])
    } else if e.info.method == METHOD_STORED {
        if raw.len() as u64 != e.info.uncomp_size {
            return Err("zip: size mismatch");
        }
        if raw.len() > limit {
            return Err("zip: member too large");
        }
        Cow::Borrowed(raw)
    } else {
        if e.info.uncomp_size > limit as u64 {
            return Err("zip: member too large");
        }
        Cow::Owned(
            decompress_to_vec_with_limit(raw, limit)
                .map_err(|_| "zip: deflate error")?
                .to_vec(),
        )
    };
    if !e.info.is_dir {
        if payload.len() as u64 != e.info.uncomp_size {
            return Err("zip: size mismatch");
        }
        if crc32::crc32(&payload) != e.crc {
            return Err("zip: CRC mismatch");
        }
    }
    Ok(payload)
}

/// List member metadata without buffering payloads.
pub fn list(data: &[u8]) -> Result<Vec<EntryInfo>, &'static str> {
    parse_central(data).map(|v| v.into_iter().map(|e| e.info).collect())
}

/// Extract all members. `limit` bounds the total payload held at once
/// (borrowed stored members don't count against the heap, but deflated
/// copies do). Keep `data` alive while using the entries.
pub fn extract(data: &[u8], limit: usize) -> Result<Vec<Entry<'_>>, &'static str> {
    let central = parse_central(data)?;
    let mut out = Vec::with_capacity(central.len());
    let mut total = 0usize;
    for e in &central {
        let payload = extract_one(data, e, limit.saturating_sub(total))?;
        total = total.checked_add(payload.len()).ok_or("zip: archive too large")?;
        if total > limit {
            return Err("zip: archive too large");
        }
        out.push(Entry {
            name: e.info.name.clone(),
            data: payload,
            is_dir: e.info.is_dir,
        });
    }
    Ok(out)
}

// ── writer ───────────────────────────────────────────────────────────

fn push_le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Fixed DOS timestamp: 2024-01-01 00:00:00 (deterministic archives).
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2024 - 1980) << 9) | (1 << 5) | 1;

/// Build a ZIP archive. `pack` selects the method for file payloads
/// (directories are always stored).
pub fn build(entries: &[BuildEntry<'_>], pack: Pack) -> Result<Vec<u8>, &'static str> {
    if entries.len() > MAX_ENTRIES {
        return Err("zip: too many members");
    }
    // (name, method, crc, comp, uncomp, offset)
    let mut central: Vec<(String, u16, u32, u64, u64, u64)> = Vec::new();
    let mut out = Vec::new();

    for e in entries {
        let (clean, slash_dir) = sanitize_path(e.name)?;
        let is_dir = e.data.is_none() || slash_dir;
        let payload: &[u8] = match e.data {
            Some(d) if !is_dir => d,
            _ => &[],
        };
        if is_dir && e.data.map(|d| !d.is_empty()).unwrap_or(false) {
            return Err("zip: directory with payload");
        }
        // Directory names must end with '/' in the archive.
        let arc_name = if is_dir && !clean.ends_with('/') {
            alloc::format!("{}/", clean)
        } else {
            clean.clone()
        };
        let name_bytes = arc_name.as_bytes();
        if name_bytes.len() > 0xFFFF {
            return Err("zip: member name too long");
        }
        if payload.len() >= 0xFFFF_FFFF {
            return Err("zip: member too large (no Zip64)");
        }
        let (method, stored): (u16, Vec<u8>) = if is_dir || pack == Pack::Stored {
            (METHOD_STORED, Vec::new())
        } else {
            (METHOD_DEFLATED, compress_to_vec(payload, 6))
        };
        // Stored-bigger-than-source guard: keep the smaller representation.
        let (method, body): (u16, &[u8]) = if method == METHOD_DEFLATED && stored.len() >= payload.len() {
            (METHOD_STORED, payload)
        } else if method == METHOD_DEFLATED {
            (METHOD_DEFLATED, &stored)
        } else {
            (METHOD_STORED, payload)
        };
        let crc = crc32::crc32(payload);
        let offset = out.len() as u64;

        // Local file header.
        out.extend_from_slice(&SIG_LOCAL);
        push_le16(&mut out, if method == METHOD_DEFLATED { 20 } else { 10 });
        push_le16(&mut out, 0x0800); // UTF-8 names
        push_le16(&mut out, method);
        push_le16(&mut out, DOS_TIME);
        push_le16(&mut out, DOS_DATE);
        push_le32(&mut out, crc);
        push_le32(&mut out, body.len() as u32);
        push_le32(&mut out, payload.len() as u32);
        push_le16(&mut out, name_bytes.len() as u16);
        push_le16(&mut out, 0);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(body);

        central.push((arc_name, method, crc, body.len() as u64, payload.len() as u64, offset));
    }

    // Central directory.
    let cd_start = out.len() as u64;
    for (name, method, crc, comp, uncomp, offset) in &central {
        let name_bytes = name.as_bytes();
        let is_dir = name.ends_with('/');
        out.extend_from_slice(&SIG_CENTRAL);
        push_le16(&mut out, 63); // made by: Unix 6.3
        push_le16(&mut out, if *method == METHOD_DEFLATED { 20 } else { 10 });
        push_le16(&mut out, 0x0800);
        push_le16(&mut out, *method);
        push_le16(&mut out, DOS_TIME);
        push_le16(&mut out, DOS_DATE);
        push_le32(&mut out, *crc);
        push_le32(&mut out, *comp as u32);
        push_le32(&mut out, *uncomp as u32);
        push_le16(&mut out, name_bytes.len() as u16);
        push_le16(&mut out, 0);
        push_le16(&mut out, 0);
        push_le16(&mut out, 0);
        push_le16(&mut out, 0);
        push_le32(
            &mut out,
            if is_dir {
                (0o755 << 16) | 0x10
            } else {
                0o644 << 16
            },
        );
        push_le32(&mut out, *offset as u32);
        out.extend_from_slice(name_bytes);
    }
    let cd_size = out.len() as u64 - cd_start;

    // End of central directory.
    out.extend_from_slice(&SIG_EOCD);
    push_le16(&mut out, 0);
    push_le16(&mut out, 0);
    push_le16(&mut out, central.len() as u16);
    push_le16(&mut out, central.len() as u16);
    push_le32(&mut out, cd_size as u32);
    push_le32(&mut out, cd_start as u32);
    push_le16(&mut out, 0);
    Ok(out)
}
