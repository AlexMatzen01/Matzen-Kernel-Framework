//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! `.mfk` — the Matzen Kernel Framework archive container.
//!
//! A small deterministic format with per-entry CRC-32 integrity and
//! per-entry compression (raw DEFLATE or stored), so a single archive
//! can mix both. Layout (LSB throughout):
//!
//! ```text
//!   4 bytes  magic "MFK1"
//!   per entry:
//!      u8     method   (0 = stored, 1 = deflate)
//!      u8     is_dir   (0 = file, 1 = directory)
//!      u32    uncompressed payload length
//!      u32    compressed payload length
//!      u32    CRC-32 of the uncompressed payload
//!      u16    name length
//!      name   UTF-8, `/` separators, no leading `/`, no `..`
//!      payload
//!   2 bytes  name length = 0 terminator (plus the 4 metadata bytes of a
//!            zero-size entry, i.e. method/is_dir/sizes/crc all zero)
//! ```
//!
//! Entries decode with the same raw-DEFLATE pairing as gzip/zip
//! (`deflate::compress_to_vec` + `inflate::decompress_to_vec_with_limit`),
//! and payloads are capped by the archive `limit` argument like `zip`/`tar`.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use super::crc32;
use super::tar::sanitize_path;

/// Magic at the start of every `.mfk` file.
const MAGIC: [u8; 4] = [b'M', b'F', b'K', b'1'];

const METHOD_STORED: u8 = 0;
const METHOD_DEFLATE: u8 = 1;

/// Hard cap on entries per archive.
pub const MAX_ENTRIES: usize = 4096;

/// A fully extracted member.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    pub name: String,
    pub data: Cow<'a, [u8]>,
    pub is_dir: bool,
}

/// Member metadata for listing.
#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub name: String,
    /// True when the payload is verbatim, false for DEFLATE.
    pub stored: bool,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub is_dir: bool,
}

/// A member to write: `data = None` encodes a directory.
pub struct BuildEntry<'a> {
    pub name: &'a str,
    pub data: Option<&'a [u8]>,
}

/// Compression choice for archive building.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pack {
    /// Raw DEFLATE level 6 (deterministic).
    Deflate,
    /// Verbatim.
    Stored,
}

/// True if `data` starts with the MFK magic.
pub fn is_mfk(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == MAGIC
}

fn bad(msg: &'static str) -> &'static str {
    msg
}

fn parse_name(raw: &[u8]) -> Result<(String, bool), &'static str> {
    let name = core::str::from_utf8(raw).map_err(|_| bad("mfk: non-UTF8 member name"))?;
    sanitize_path(name)
}

/// List member metadata without buffering payloads.
pub fn list(data: &[u8]) -> Result<Vec<EntryInfo>, &'static str> {
    if !is_mfk(data) {
        return Err("mfk: bad magic");
    }
    let mut pos = 4usize;
    let mut out = Vec::new();
    while pos < data.len() {
        // fixed header: method u8, is_dir u8, usize3, etc.
        if pos + 16 > data.len() {
            return Err("mfk: truncated entry header");
        }
        let method = data[pos];
        let is_dir = data[pos + 1] != 0;
        let usize_ = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]]) as u64;
        let csize = u32::from_le_bytes([data[pos + 6], data[pos + 7], data[pos + 8], data[pos + 9]]) as u64;
        let crc = u32::from_le_bytes([data[pos + 10], data[pos + 11], data[pos + 12], data[pos + 13]]);
        let nlen = u16::from_le_bytes([data[pos + 14], data[pos + 15]]) as usize;
        // terminator
        if nlen == 0 {
            return Ok(out);
        }
        pos += 16;
        let _ = crc;
        if pos + nlen > data.len() {
            return Err("mfk: truncated name");
        }
        let (clean, _) = parse_name(&data[pos..pos + nlen])?;
        pos += nlen;
        match method {
            METHOD_STORED | METHOD_DEFLATE => {}
            _ => return Err("mfk: bad method"),
        }
        out.push(EntryInfo {
            name: clean,
            stored: method == METHOD_STORED,
            comp_size: csize,
            uncomp_size: usize_,
            is_dir,
        });
        let csize_usize = csize as usize;
        if pos + csize_usize > data.len() {
            return Err("mfk: truncated payload");
        }
        pos += csize_usize;
    }
    Err("mfk: no terminator")
}

/// Extract all members. `limit` bounds the total decompressed payload held.
pub fn extract(data: &[u8], limit: usize) -> Result<Vec<Entry<'_>>, &'static str> {
    if !is_mfk(data) {
        return Err("mfk: bad magic");
    }
    let mut pos = 4usize;
    let mut out = Vec::new();
    let mut total = 0usize;
    while pos < data.len() {
        if pos + 16 > data.len() {
            return Err("mfk: truncated entry header");
        }
        let method = data[pos];
        let is_dir = data[pos + 1] != 0;
        let usize_ = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]]) as u64;
        let csize = u32::from_le_bytes([data[pos + 6], data[pos + 7], data[pos + 8], data[pos + 9]]) as u64;
        let crc = u32::from_le_bytes([data[pos + 10], data[pos + 11], data[pos + 12], data[pos + 13]]);
        let nlen = u16::from_le_bytes([data[pos + 14], data[pos + 15]]) as usize;
        pos += 16;
        if nlen == 0 {
            return Ok(out);
        }
        if pos + nlen > data.len() {
            return Err("mfk: truncated name");
        }
        let raw = &data[pos..pos + nlen];
        let (clean, _) = parse_name(raw)?;
        pos += nlen;
        let csize_usize = usize::try_from(csize).map_err(|_| "mfk: member too large")?;
        if pos + csize_usize > data.len() {
            return Err("mfk: truncated payload");
        }
        let payload = &data[pos..pos + csize_usize];
        pos += csize_usize;

        if is_dir {
            if usize_ != 0 || csize != 0 {
                return Err("mfk: bad directory entry");
            }
            if total > limit {
                return Err("mfk: archive too large");
            }
            out.push(Entry { name: clean, data: Cow::Borrowed(&[]), is_dir: true });
            continue;
        }

        let owning: Cow<'_, [u8]> = match method {
            METHOD_STORED => {
                if payload.len() as u64 != usize_ {
                    return Err("mfk: size mismatch");
                }
                Cow::Borrowed(payload)
            }
            METHOD_DEFLATE => {
                if usize_ > limit as u64 {
                    return Err("mfk: member too large");
                }
                let buf = miniz_oxide::inflate::decompress_to_vec_with_limit(payload, limit)
                    .map_err(|_| "mfk: deflate error")?
                    .to_vec();
                if buf.len() as u64 != usize_ {
                    return Err("mfk: size mismatch");
                }
                Cow::Owned(buf)
            }
            _ => return Err("mfk: bad method"),
        };
        if crc32::crc32(&owning) != crc {
            return Err("mfk: CRC mismatch");
        }
        total = total.checked_add(owning.len()).ok_or("mfk: archive too large")?;
        if total > limit {
            return Err("mfk: archive too large");
        }
        out.push(Entry { name: clean, data: owning, is_dir: false });
    }
    Err("mfk: no terminator")
}

/// Build an `.mfk` archive from entries.
pub fn build(entries: &[BuildEntry<'_>], pack: Pack) -> Result<Vec<u8>, &'static str> {
    if entries.len() > MAX_ENTRIES {
        return Err("mfk: too many members");
    }
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    for e in entries {
        let (clean, slash_dir) = sanitize_path(e.name)?;
        let is_dir = e.data.is_none() || slash_dir;
        if is_dir && e.data.map(|d| !d.is_empty()).unwrap_or(false) {
            return Err("mfk: directory with payload");
        }
        let payload: &[u8] = match e.data {
            Some(d) if !is_dir => d,
            _ => &[],
        };
        // A stored member carries the payload verbatim; a deflated member
        // carries the raw DEFLATE stream and falls back to stored whenever
        // deflate would not shrink the payload, so a stored member is never
        // larger than the source. Directories have a zero-length payload
        // either way and are distinguished by the directory flag.
        let compressed: Option<Vec<u8>> = if is_dir || pack == Pack::Stored {
            None
        } else {
            let comp = miniz_oxide::deflate::compress_to_vec(payload, 6);
            if comp.len() < payload.len() {
                Some(comp)
            } else {
                None
            }
        };
        let (method, body): (u8, &[u8]) = match &compressed {
            Some(comp) => (METHOD_DEFLATE, comp.as_slice()),
            None => (METHOD_STORED, payload),
        };
        let crc = crc32::crc32(payload);
        out.push(method);
        out.push(if is_dir { 1 } else { 0 });
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(clean.len() as u16).to_le_bytes());
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(&body);
    }
    // Terminator entry: all-zero header incl. nlen 0.
    out.extend_from_slice(&[0u8; 16]);
    Ok(out)
}
