//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Minimal ustar tar reader/writer (`no_std`, `alloc` only).
//!
//! Covers the archives MFK is expected to meet in practice:
//! - POSIX ustar headers (512-byte blocks, octal fields, checksum), plus
//!   the pre-POSIX v7 layout (empty magic) which parses identically.
//! - GNU `L` long-name entries and PAX `x` extended headers (`path=` and
//!   `size=` overrides) on read; plain ustar (+ `L` fallback for very long
//!   names) on write.
//! - Regular files (`0`/`\0`) and directories (`5`). Symlinks, hardlinks,
//!   devices and other specials are reported as skipped, never followed.
//!
//! Reference: POSIX.1-1988 ustar + GNU tar extensions. The writer is kept
//! deliberately boring so host tools (`tar`, bsdtar, 7-Zip) accept it.

use alloc::string::String;
use alloc::vec::Vec;

/// Tar block size in bytes.
pub const BLOCK: usize = 512;

/// Maximum bytes of a single path component. Matches SimplFS
/// `MAX_FILENAME_LEN` (56 incl. the NUL terminator); the archive layer
/// must not produce names the filesystem cannot store.
pub const MAX_COMPONENT_LEN: usize = 55;

/// Hard cap on entries per archive (bounds heap + shell loop time).
pub const MAX_ENTRIES: usize = 4096;

/// A single parsed tar member. `data` borrows the source archive (no
/// copy), so large archives cost one buffer plus names, not two.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// Sanitized relative path with `/` separators (no leading `/`, no `..`).
    pub name: String,
    /// File payload (empty for directories), borrowed from the archive.
    pub data: &'a [u8],
    /// True for directory members.
    pub is_dir: bool,
}

/// A member to write: `data = None` encodes a directory.
pub struct BuildEntry<'a> {
    /// Relative path with `/` separators (`a/b/c.txt`, `a/b/` for dirs).
    pub name: &'a str,
    /// File payload, or `None` for a directory.
    pub data: Option<&'a [u8]>,
}

// ── low-level field helpers ──────────────────────────────────────────

/// Parse an octal number field (NUL/space terminated). Accepts GNU base-256
/// (high bit of first byte set) for archives made by host GNU tar.
fn parse_octal(field: &[u8]) -> Result<u64, &'static str> {
    if field.is_empty() {
        return Ok(0);
    }
    if field[0] & 0x80 != 0 {
        // Base-256: big-endian binary, top bit is the marker, not data.
        let mut v: u64 = 0;
        let mut started = false;
        for &b in field {
            let b = if !started { b & 0x7F } else { b };
            started = true;
            v = v
                .checked_shl(8)
                .and_then(|s| s.checked_add(b as u64))
                .ok_or("tar: numeric field overflow")?;
        }
        return Ok(v);
    }
    let mut v: u64 = 0;
    let mut any = false;
    for &b in field {
        if b == 0 || b == b' ' {
            break;
        }
        if !(b'0'..=b'7').contains(&b) {
            return Err("tar: bad octal field");
        }
        any = true;
        v = v
            .checked_shl(3)
            .and_then(|s| s.checked_add((b - b'0') as u64))
            .ok_or("tar: numeric field overflow")?;
    }
    if !any {
        return Ok(0);
    }
    Ok(v)
}

/// Read a NUL-terminated string field.
fn parse_str(field: &[u8]) -> Result<&str, &'static str> {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..len]).map_err(|_| "tar: non-UTF8 header field")
}

/// Write an octal value into `field`, NUL-terminated (`size` gets 11 digits).
fn write_octal(field: &mut [u8], value: u64) -> Result<(), &'static str> {
    let mut buf = [b'0'; 24];
    let mut v = value;
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while v > 0 {
            if i == 0 {
                return Err("tar: value too large for field");
            }
            i -= 1;
            buf[i] = b'0' + (v & 7) as u8;
            v >>= 3;
        }
    }
    let digits = &buf[i..];
    if digits.len() + 1 > field.len() {
        return Err("tar: value too large for field");
    }
    // Leading ASCII '0' padding (NUL padding would terminate the parse early).
    for b in field.iter_mut() {
        *b = b'0';
    }
    let start = field.len() - 1 - digits.len();
    field[start..start + digits.len()].copy_from_slice(digits);
    field[field.len() - 1] = 0;
    Ok(())
}

/// Verify the header checksum (checksum field counts as 8 spaces).
fn checksum_ok(block: &[u8; BLOCK]) -> bool {
    let stored = parse_octal(&block[148..156]).unwrap_or(u64::MAX);
    let mut sum: u64 = 0;
    for (i, &b) in block.iter().enumerate() {
        sum += if (148..156).contains(&i) {
            0x20
        } else {
            b as u64
        };
    }
    sum == stored
}

/// True if the whole block is zero (archive end marker).
fn is_zero_block(block: &[u8; BLOCK]) -> bool {
    block.iter().all(|&b| b == 0)
}

// ── path sanitation (shared by tar + zip extraction) ────────────────

/// Sanitize an archive member name into a safe relative path.
///
/// Rejects: empty names, absolute paths (`/...`), Windows drive prefixes
/// (`N:/...`, which would escape the destination drive on MFK), parent
/// references (`..`), backslashes and NUL bytes. Redundant `./` prefixes
/// and duplicate slashes are collapsed. A trailing `/` is stripped and
/// reported via the return flag (caller sets `is_dir`).
pub fn sanitize_path(raw: &str) -> Result<(String, bool), &'static str> {
    if raw.is_empty() {
        return Err("archive: empty member name");
    }
    if raw.as_bytes().contains(&0) {
        return Err("archive: NUL byte in member name");
    }
    if raw.contains('\\') {
        return Err("archive: backslash in member name");
    }
    let mut s = raw;
    let mut is_dir = false;
    if s.ends_with('/') {
        is_dir = true;
        s = s.trim_end_matches('/');
    }
    if s.is_empty() {
        return Err("archive: empty member name");
    }
    if s.starts_with('/') {
        return Err("archive: absolute member path");
    }
    // Drive prefix like `2:/x` or `2:x` would escape the destination drive.
    if let Some(colon) = s.find(':') {
        let head = &s[..colon];
        if !head.is_empty()
            && head.bytes().all(|b| b.is_ascii_digit())
            && !head.contains('/')
        {
            return Err("archive: drive prefix in member name");
        }
    }
    let mut out = String::new();
    for comp in s.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            return Err("archive: '..' in member name");
        }
        if comp.len() > MAX_COMPONENT_LEN {
            return Err("archive: member name component too long");
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(comp);
    }
    if out.is_empty() {
        return Err("archive: empty member name");
    }
    Ok((out, is_dir))
}

// ── reader ───────────────────────────────────────────────────────────

struct RawHeader<'a> {
    name: &'a str,
    prefix: &'a str,
    size: u64,
    typeflag: u8,
    magic: &'a [u8],
}

fn parse_header(block: &[u8; BLOCK]) -> Result<RawHeader<'_>, &'static str> {
    if !checksum_ok(block) {
        return Err("tar: bad header checksum");
    }
    Ok(RawHeader {
        name: parse_str(&block[0..100])?,
        prefix: parse_str(&block[345..500])?,
        size: parse_octal(&block[124..136])?,
        typeflag: block[156],
        magic: &block[257..262],
    })
}

/// Parse PAX extended-header records, returning (`path`, `size`) overrides.
/// Records look like `<len> <key>=<value>\n`; unknown keys are ignored.
fn parse_pax(data: &[u8]) -> (Option<String>, Option<u64>) {
    let mut path: Option<String> = None;
    let mut size: Option<u64> = None;
    let text = match core::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return (None, None),
    };
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        // Strip the leading "<len> " prefix.
        let kv = match line.find(' ') {
            Some(i) => &line[i + 1..],
            None => continue,
        };
        let (k, v) = match kv.find('=') {
            Some(i) => (&kv[..i], &kv[i + 1..]),
            None => continue,
        };
        match k {
            "path" => {
                if v.len() < 512 {
                    path = Some(String::from(v));
                }
            }
            "size" => {
                if let Ok(n) = v.trim().parse::<u64>() {
                    size = Some(n);
                }
            }
            _ => {}
        }
    }
    (path, size)
}

/// Read all members of a tar archive. `max_total` bounds the total payload
/// bytes *referenced* (protects the kernel heap from hostile archives).
/// Payloads borrow `data`; keep it alive while using the entries.
pub fn read_entries(data: &[u8], max_total: usize) -> Result<Vec<Entry<'_>>, &'static str> {
    if data.len() % BLOCK != 0 {
        return Err("tar: truncated archive (not a multiple of 512)");
    }
    let nblocks = data.len() / BLOCK;
    let mut entries: Vec<Entry> = Vec::new();
    let mut total: usize = 0;
    let mut pending_longname: Option<String> = None;
    let mut pending_pax_path: Option<String> = None; // from `x`: one-shot
    let mut pending_pax_size: Option<u64> = None; // from `x`: one-shot
    let mut global_pax_path: Option<String> = None; // from `g`: sticky
    let mut i = 0usize;

    // Closure-free block fetch (borrow-friendly).
    let block_at = |idx: usize| -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b.copy_from_slice(&data[idx * BLOCK..(idx + 1) * BLOCK]);
        b
    };

    while i < nblocks {
        let block = block_at(i);
        i += 1;
        if is_zero_block(&block) {
            // Two zero blocks end the archive; a single one is tolerated.
            if i < nblocks && is_zero_block(&block_at(i)) {
                break;
            }
            continue;
        }
        let h = parse_header(&block)?;
        // ustar magic is "ustar\0" + "00"; v7 has spaces/zeros. Anything
        // else with a valid checksum is still accepted (old GNU magic
        // "ustar  \0" included) since fields are positional.
        let _ = h.magic;

        let data_blocks = h
            .size
            .checked_add(BLOCK as u64 - 1)
            .ok_or("tar: member too large")?
            / BLOCK as u64;
        let data_blocks_usize: usize = data_blocks.try_into().map_err(|_| "tar: member too large")?;
        if i + data_blocks_usize > nblocks {
            return Err("tar: truncated member data");
        }
        let payload = &data[i * BLOCK..(i + data_blocks_usize) * BLOCK];
        let content = &payload[..h.size as usize];
        i += data_blocks_usize;

        match h.typeflag {
            b'L' => {
                // GNU long name: content is the next member's full path.
                let end = content.iter().position(|&b| b == 0).unwrap_or(content.len());
                let name = core::str::from_utf8(&content[..end])
                    .map_err(|_| "tar: non-UTF8 long name")?;
                if name.len() >= 512 {
                    return Err("tar: long name too large");
                }
                pending_longname = Some(String::from(name));
                continue;
            }
            b'x' | b'g' => {
                // PAX extended header: `x` applies to the next member,
                // `g` is global (sticky for all following members).
                let (p, s) = parse_pax(content);
                if h.typeflag == b'x' {
                    if p.is_some() {
                        pending_pax_path = p;
                    }
                    if s.is_some() {
                        pending_pax_size = s;
                    }
                } else {
                    if p.is_some() {
                        global_pax_path = p;
                    }
                    // Global `size` is meaningless; ignore `s`.
                }
                continue;
            }
            _ => {}
        }

        // Resolve the member name.
        let mut full = String::new();
        if !h.prefix.is_empty() {
            full.push_str(h.prefix);
            full.push('/');
        }
        full.push_str(h.name);
        if let Some(long) = pending_longname.take() {
            full = long;
        }
        if let Some(pax) = pending_pax_path.take() {
            // A per-member `x` header overrides the ustar name/prefix.
            full = pax;
        } else if full.is_empty() {
            // A sticky `g` header only fills in an otherwise empty name.
            if let Some(pax) = global_pax_path.clone() {
                full = pax;
            }
        }
        let size = pending_pax_size.take().unwrap_or(h.size);
        if size != h.size && (size as usize) > content.len() {
            return Err("tar: PAX size exceeds member data");
        }
        let content = &content[..core::cmp::min(size as usize, content.len())];

        match h.typeflag {
            b'0' | 0 => {
                let (name, slash_dir) = sanitize_path(&full)?;
                total = total
                    .checked_add(content.len())
                    .ok_or("tar: archive too large")?;
                if total > max_total {
                    return Err("tar: archive too large");
                }
                if entries.len() >= MAX_ENTRIES {
                    return Err("tar: too many members");
                }
                entries.push(Entry {
                    name,
                    data: content,
                    is_dir: slash_dir,
                });
            }
            b'5' => {
                let (name, _) = sanitize_path(&full)?;
                if entries.len() >= MAX_ENTRIES {
                    return Err("tar: too many members");
                }
                entries.push(Entry { name, data: &[], is_dir: true });
            }
            _ => {
                // Symlinks, hardlinks, devices, ... : never followed.
                // Silently skipped (counted by the caller via names listing).
                continue;
            }
        }
    }
    Ok(entries)
}

// ── writer ───────────────────────────────────────────────────────────

/// Split a path into ustar (prefix, name) parts. Returns `None` when the
/// name needs a GNU `L` long-name entry instead.
fn split_ustar(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    // Find a '/' such that name <= 100 and prefix <= 155.
    let bytes = path.as_bytes();
    let mut best: Option<usize> = None;
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'/' && idx > 0 {
            let (pre, rest) = (&path[..idx], &path[idx + 1..]);
            if rest.len() <= 100 && pre.len() <= 155 {
                best = Some(idx);
            }
        }
    }
    best.map(|idx| (&path[..idx], &path[idx + 1..]))
}

fn write_field(dst: &mut [u8], src: &str) {
    let b = src.as_bytes();
    let n = core::cmp::min(b.len(), dst.len());
    dst[..n].copy_from_slice(&b[..n]);
}

fn emit_header(
    out: &mut Vec<u8>,
    name: &str,
    prefix: &str,
    size: u64,
    typeflag: u8,
    mode: u64,
) -> Result<(), &'static str> {
    let mut h = [0u8; BLOCK];
    write_field(&mut h[0..100], name);
    write_octal(&mut h[100..108], mode)?;
    write_octal(&mut h[108..116], 0)?;
    write_octal(&mut h[116..124], 0)?;
    write_octal(&mut h[124..136], size)?;
    write_octal(&mut h[136..148], 0)?; // mtime 0: deterministic archives
    // Checksum field: spaces for the computation.
    for b in &mut h[148..156] {
        *b = b' ';
    }
    h[156] = typeflag;
    write_field(&mut h[257..263], "ustar");
    h[263] = b'0';
    h[264] = b'0';
    write_field(&mut h[265..297], "root");
    write_field(&mut h[297..329], "root");
    write_field(&mut h[345..500], prefix);
    let mut sum: u64 = 0;
    for &b in &h {
        sum += b as u64;
    }
    // 6 octal digits + NUL + space.
    let mut cks = [b' '; 8];
    let mut v = sum;
    for k in (0..6).rev() {
        cks[k] = b'0' + (v & 7) as u8;
        v >>= 3;
    }
    if v != 0 {
        return Err("tar: checksum overflow");
    }
    cks[6] = 0;
    h[148..156].copy_from_slice(&cks);
    out.extend_from_slice(&h);
    Ok(())
}

fn emit_file(out: &mut Vec<u8>, name: &str, data: &[u8]) -> Result<(), &'static str> {
    match split_ustar(name) {
        Some((prefix, short)) => emit_header(out, short, prefix, data.len() as u64, b'0', 0o644)?,
        None => {
            // GNU long name entry.
            if name.len() >= 512 {
                return Err("tar: member name too long");
            }
            let mut payload = Vec::with_capacity(name.len() + 1);
            payload.extend_from_slice(name.as_bytes());
            payload.push(0);
            emit_header(out, "././@LongLink", "", payload.len() as u64, b'L', 0)?;
            out.extend_from_slice(&payload);
            let pad = (BLOCK - payload.len() % BLOCK) % BLOCK;
            out.extend_from_slice(&alloc::vec![0u8; 512][..pad]);
            emit_header(out, &name[name.len().saturating_sub(100)..], "", data.len() as u64, b'0', 0o644)?;
        }
    }
    out.extend_from_slice(data);
    let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
    if pad > 0 {
        out.extend_from_slice(&alloc::vec![0u8; 512][..pad]);
    }
    Ok(())
}

/// Build a tar archive from `entries`. Directory names should end with `/`
/// (a missing slash is tolerated when `data` is `None`).
pub fn build(entries: &[BuildEntry<'_>]) -> Result<Vec<u8>, &'static str> {
    if entries.len() > MAX_ENTRIES {
        return Err("tar: too many members");
    }
    let mut out = Vec::new();
    for e in entries {
        if e.name.is_empty() || e.name.len() >= 512 {
            return Err("tar: bad member name");
        }
        match e.data {
            Some(data) => {
                let (clean, slash_dir) = sanitize_path(e.name)?;
                if slash_dir {
                    return Err("tar: file name ends with '/'");
                }
                emit_file(&mut out, &clean, data)?;
            }
            None => {
                let base = e.name.trim_end_matches('/');
                let (clean, _) = sanitize_path(base)?;
                if clean.len() > 255 {
                    return Err("tar: directory name too long");
                }
                match split_ustar(&clean) {
                    Some((prefix, short)) => {
                        emit_header(&mut out, short, prefix, 0, b'5', 0o755)?
                    }
                    None => {
                        let mut payload = Vec::with_capacity(clean.len() + 1);
                        payload.extend_from_slice(clean.as_bytes());
                        payload.push(0);
                        emit_header(&mut out, "././@LongLink", "", payload.len() as u64, b'L', 0)?;
                        out.extend_from_slice(&payload);
                        let pad = (BLOCK - payload.len() % BLOCK) % BLOCK;
                        out.extend_from_slice(&alloc::vec![0u8; 512][..pad]);
                        emit_header(
                            &mut out,
                            &clean[clean.len().saturating_sub(100)..],
                            "",
                            0,
                            b'5',
                            0o755,
                        )?;
                    }
                }
            }
        }
    }
    // End-of-archive: two zero blocks.
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    Ok(out)
}
