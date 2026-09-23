//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.
//!
//! Wallpaper + personalization backend for the desktop.
//!
//! - Supported files: `.png`, `.jpg`, `.jpeg` (magic-byte sniffed, not
//!   just extension). `jpg` and `jpeg` are the same baseline-JPEG decoder.
//! - Modes: Solid, Fit (letterbox), Fill (cover+crop), Stretch,
//!   Center (1:1), Tile. Landscape and portrait images share the same
//!   aspect math (no separate code path; orientation() is informational).
//! - `no_std` compatible: only `alloc`, no external crates. PNG inflate
//!   (zlib/deflate) and baseline-JPEG (Huffman + DCT + YCbCr) are
//!   hand-rolled so the kernel stays self-contained.
//! - The compositor never scales per-frame: `build_display_cache` renders
//!   a screen-sized RGBA buffer once per image/mode/screen change, and
//!   painting is a single clipped `blit_rgba(0,0,...)` (dirty-rect clip
//!   handled by `fb_gfx`).

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

// ──────────────────────────────────────────────
// Limits
// ──────────────────────────────────────────────

/// Max accepted image file size (8 MiB).
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Max image dimension (either axis).
pub const MAX_IMAGE_DIM: u32 = 4096;
/// Max decoded pixels (16 MP; bounds transient + cache memory).
pub const MAX_IMAGE_PIXELS: u64 = 16_777_216;
/// Max zlib/deflate output while inflating IDAT.
const MAX_INFLATE_BYTES: usize = 64 * 1024 * 1024;
/// Largest screen cache we build (4K). Bigger screens use direct rendering.
const MAX_CACHE_PIXELS: u64 = 3840 * 2160;
/// Settings file location on SimplFS.
pub const SETTINGS_PATH: &str = "/config/settings.cfg";
/// Directory the Settings UI browses by default.
pub const WALLPAPER_DIR: &str = "/wallpapers";

// ──────────────────────────────────────────────
// Modes / config / orientation
// ──────────────────────────────────────────────

/// Wallpaper layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperMode {
    Solid,
    Fit,
    Fill,
    Stretch,
    Center,
    Tile,
}

impl WallpaperMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WallpaperMode::Solid => "solid",
            WallpaperMode::Fit => "fit",
            WallpaperMode::Fill => "fill",
            WallpaperMode::Stretch => "stretch",
            WallpaperMode::Center => "center",
            WallpaperMode::Tile => "tile",
        }
    }

    pub fn from_str(s: &str) -> Option<WallpaperMode> {
        let t = s.trim().to_ascii_lowercase();
        match t.as_str() {
            "solid" | "none" | "color" => Some(WallpaperMode::Solid),
            "fit" | "letterbox" => Some(WallpaperMode::Fit),
            "fill" | "cover" | "crop" => Some(WallpaperMode::Fill),
            "stretch" | "fill-stretch" | "distort" => Some(WallpaperMode::Stretch),
            "center" | "centre" | "middle" => Some(WallpaperMode::Center),
            "tile" | "tiled" | "repeat" => Some(WallpaperMode::Tile),
            _ => None,
        }
    }

    pub fn all() -> [WallpaperMode; 6] {
        [
            WallpaperMode::Solid,
            WallpaperMode::Fit,
            WallpaperMode::Fill,
            WallpaperMode::Stretch,
            WallpaperMode::Center,
            WallpaperMode::Tile,
        ]
    }
}

/// Persisted personalization knobs.
#[derive(Debug, Clone)]
pub struct WallpaperConfig {
    pub path: String,
    pub mode: WallpaperMode,
    pub bg: u32,
    pub accent: u32,
}

impl WallpaperConfig {
    pub fn defaults() -> Self {
        Self {
            path: String::new(),
            mode: WallpaperMode::Solid,
            bg: 0x102a4e,
            accent: 0x00be5a,
        }
    }
}

/// Landscape vs portrait helper (informational: scaling math is shared).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

pub fn orientation(w: u32, h: u32) -> Orientation {
    if w > h {
        Orientation::Landscape
    } else if h > w {
        Orientation::Portrait
    } else {
        Orientation::Square
    }
}

/// Decoded image in RGBA8 row-major order.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

// ──────────────────────────────────────────────
// Format sniffing + extensions
// ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Unknown,
}

pub fn sniff_kind(bytes: &[u8]) -> ImageKind {
    if is_png(bytes) {
        ImageKind::Png
    } else if is_jpeg(bytes) {
        ImageKind::Jpeg
    } else {
        ImageKind::Unknown
    }
}

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == b'P'
        && bytes[2] == b'N'
        && bytes[3] == b'G'
        && bytes[4] == 0x0D
        && bytes[5] == 0x0A
        && bytes[6] == 0x1A
        && bytes[7] == 0x0A
}

pub fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// Case-insensitive `.png` / `.jpg` / `.jpeg` check.
pub fn has_supported_extension(path: &str) -> bool {
    let p = path.trim();
    let lower = p.to_ascii_lowercase();
    lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

pub fn decode_auto(bytes: &[u8]) -> Result<DecodedImage, &'static str> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image too large (max 8 MiB)");
    }
    if bytes.is_empty() {
        return Err("empty file");
    }
    match sniff_kind(bytes) {
        ImageKind::Png => decode_png(bytes),
        ImageKind::Jpeg => decode_jpeg(bytes),
        ImageKind::Unknown => Err("unsupported format (need PNG/JPG/JPEG)"),
    }
}

// ──────────────────────────────────────────────
// Settings text format (key=value, no serde)
// ──────────────────────────────────────────────

pub fn parse_hex_color(s: &str) -> Option<u32> {
    let t = s.trim();
    let hex = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        h
    } else if let Some(h) = t.strip_prefix('#') {
        h
    } else {
        t
    };
    if hex.is_empty() || hex.len() > 6 {
        return None;
    }
    let mut v: u32 = 0;
    for ch in hex.bytes() {
        let d = match ch {
            b'0'..=b'9' => (ch - b'0') as u32,
            b'a'..=b'f' => (ch - b'a' + 10) as u32,
            b'A'..=b'F' => (ch - b'A' + 10) as u32,
            _ => return None,
        };
        v = (v << 4) | d;
    }
    Some(v & 0xFFFFFF)
}

pub fn format_hex_color(v: u32) -> String {
    alloc::format!("0x{:06x}", v & 0xFFFFFF)
}

pub fn parse_settings(text: &str) -> WallpaperConfig {
    let mut cfg = WallpaperConfig::defaults();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let (k, v) = (line[..eq].trim(), line[eq + 1..].trim());
        match k.to_ascii_lowercase().as_str() {
            "wallpaper" | "wallpaper_path" | "path" | "background" => {
                if v.len() <= 256 {
                    cfg.path = String::from(v);
                }
            }
            "mode" | "wallpaper_mode" | "fit" => {
                if let Some(m) = WallpaperMode::from_str(v) {
                    cfg.mode = m;
                }
            }
            "bg" | "background_color" | "color" => {
                if let Some(c) = parse_hex_color(v) {
                    cfg.bg = c;
                }
            }
            "accent" | "accent_color" => {
                if let Some(c) = parse_hex_color(v) {
                    cfg.accent = c;
                }
            }
            _ => {}
        }
    }
    // Solid with empty path is the "no image" state; keep mode as-is.
    cfg
}

pub fn format_settings(cfg: &WallpaperConfig) -> String {
    alloc::format!(
        "# MFK personalization (generated)\nversion=1\nwallpaper_path={}\nmode={}\nbg={}\naccent={}\n",
        cfg.path,
        cfg.mode.as_str(),
        format_hex_color(cfg.bg),
        format_hex_color(cfg.accent),
    )
}

// ──────────────────────────────────────────────
// Deflate (zlib) — for PNG IDAT
// ──────────────────────────────────────────────

struct LsBitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u8,
}

impl<'a> LsBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32, &'static str> {
        if self.byte >= self.data.len() {
            return Err("png: truncated deflate stream");
        }
        let b = ((self.data[self.byte] >> self.bit) & 1) as u32;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Ok(b)
    }

    fn read_bits(&mut self, n: u32) -> Result<u32, &'static str> {
        if n > 16 {
            return Err("png: bad bit length");
        }
        let mut v = 0u32;
        for i in 0..n {
            v |= self.read_bit()? << i;
        }
        Ok(v)
    }

    fn align_to_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }
}

/// Canonical Huffman table built from code lengths.
struct DeflateHuff {
    /// symbols ordered by (len, symbol)
    symbols: Vec<u16>,
    /// bl_count[len]
    counts: [u32; 17],
    /// start index in symbols for each len
    offsets: [usize; 17],
}

impl DeflateHuff {
    fn from_lengths(lengths: &[u8]) -> Result<Self, &'static str> {
        let mut counts = [0u32; 17];
        for &l in lengths {
            if (l as usize) >= counts.len() {
                return Err("png: bad huffman length");
            }
            if l != 0 {
                counts[l as usize] += 1;
            }
        }
        // Over-subscribed / incomplete check (allow incomplete: single-symbol ok).
        let mut symbols = Vec::new();
        let mut offsets = [0usize; 17];
        for len in 1..=16usize {
            offsets[len] = symbols.len();
            for (sym, &l) in lengths.iter().enumerate() {
                if l as usize == len {
                    if sym > 0xFFFF {
                        return Err("png: too many symbols");
                    }
                    symbols.push(sym as u16);
                }
            }
        }
        Ok(Self {
            symbols,
            counts,
            offsets,
        })
    }

    fn decode(&self, br: &mut LsBitReader) -> Result<u16, &'static str> {
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        for len in 1..=16usize {
            code |= br.read_bit()?;
            let count = self.counts[len];
            if code.wrapping_sub(first) < count {
                let idx = self.offsets[len] + (code - first) as usize;
                return self
                    .symbols
                    .get(idx)
                    .copied()
                    .ok_or("png: bad huffman code");
            }
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("png: invalid huffman code")
    }
}

const LENGTH_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_litlen_table() -> Result<DeflateHuff, &'static str> {
    let mut lens = alloc::vec![0u8; 288];
    for i in 0..144 {
        lens[i] = 8;
    }
    for i in 144..256 {
        lens[i] = 9;
    }
    for i in 256..280 {
        lens[i] = 7;
    }
    for i in 280..288 {
        lens[i] = 8;
    }
    DeflateHuff::from_lengths(&lens)
}

fn fixed_dist_table() -> Result<DeflateHuff, &'static str> {
    DeflateHuff::from_lengths(&alloc::vec![5u8; 32])
}

fn inflate_raw(deflate: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut br = LsBitReader::new(deflate);
    let mut out: Vec<u8> = Vec::new();
    let fixed_lit = fixed_litlen_table()?;
    let fixed_dist = fixed_dist_table()?;

    loop {
        let bfinal = br.read_bit()?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                br.align_to_byte();
                if br.byte + 4 > deflate.len() {
                    return Err("png: truncated stored block");
                }
                let len = (deflate[br.byte] as u32) | ((deflate[br.byte + 1] as u32) << 8);
                let nlen = (deflate[br.byte + 2] as u32) | ((deflate[br.byte + 3] as u32) << 8);
                br.byte += 4;
                if len ^ 0xFFFF != nlen {
                    return Err("png: bad stored block lengths");
                }
                if br.byte + len as usize > deflate.len() {
                    return Err("png: truncated stored data");
                }
                if out.len() + len as usize > MAX_INFLATE_BYTES {
                    return Err("png: decompressed too large");
                }
                out.extend_from_slice(&deflate[br.byte..br.byte + len as usize]);
                br.byte += len as usize;
            }
            1 | 2 => {
                let (litlen, dist) = if btype == 1 {
                    (&fixed_lit, &fixed_dist)
                } else {
                    let hlit = br.read_bits(5)? + 257;
                    let hdist = br.read_bits(5)? + 1;
                    let hclen = br.read_bits(4)? + 4;
                    if hlit > 286 || hdist > 32 {
                        return Err("png: bad dynamic header");
                    }
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl_lens = [0u8; 19];
                    for i in 0..hclen as usize {
                        cl_lens[ORDER[i]] = br.read_bits(3)? as u8;
                    }
                    let cl_huff = DeflateHuff::from_lengths(&cl_lens)?;
                    let total = hlit as usize + hdist as usize;
                    let mut lens: Vec<u8> = Vec::with_capacity(total);
                    while lens.len() < total {
                        let s = cl_huff.decode(&mut br)?;
                        match s {
                            0..=15 => lens.push(s as u8),
                            16 => {
                                let rep = br.read_bits(2)? + 3;
                                let prev = *lens.last().ok_or("png: bad repeat")?;
                                for _ in 0..rep {
                                    lens.push(prev);
                                }
                            }
                            17 => {
                                let rep = br.read_bits(3)? + 3;
                                for _ in 0..rep {
                                    lens.push(0);
                                }
                            }
                            18 => {
                                let rep = br.read_bits(7)? + 11;
                                for _ in 0..rep {
                                    lens.push(0);
                                }
                            }
                            _ => return Err("png: bad code-length symbol"),
                        }
                        if lens.len() > total + 10 {
                            return Err("png: length overflow");
                        }
                    }
                    if lens.len() != total {
                        return Err("png: bad lengths");
                    }
                    let litlen = DeflateHuff::from_lengths(&lens[..hlit as usize])?;
                    let dist = DeflateHuff::from_lengths(&lens[hlit as usize..])?;
                    // The tables must stay alive for the block loop, so hand
                    // the rest of the stream to a helper that owns them.
                    return inflate_with_tables(
                        deflate, br.byte, br.bit, bfinal, litlen, dist, out,
                    );
                };
                decode_huffman_block(&mut br, litlen, dist, &mut out, bfinal)?;
                if bfinal == 1 {
                    break;
                }
            }
            _ => return Err("png: reserved deflate block type"),
        }
        if bfinal == 1 {
            break;
        }
        if out.len() > MAX_INFLATE_BYTES {
            return Err("png: decompressed too large");
        }
    }
    Ok(out)
}

/// Continue inflation for a dynamic block whose tables were just parsed.
/// `br_byte/bit` is the stream position right after the table description.
fn inflate_with_tables(
    deflate: &[u8],
    start_byte: usize,
    start_bit: u8,
    first_bfinal: u32,
    litlen: DeflateHuff,
    dist: DeflateHuff,
    mut out: Vec<u8>,
) -> Result<Vec<u8>, &'static str> {
    let mut br = LsBitReader {
        data: deflate,
        byte: start_byte,
        bit: start_bit,
    };
    // Current block (dynamic) first; honor its BFINAL.
    decode_huffman_block(&mut br, &litlen, &dist, &mut out, 0)?;
    if first_bfinal == 1 {
        return Ok(out);
    }
    // Subsequent blocks.
    let fixed_lit = fixed_litlen_table()?;
    let fixed_dist = fixed_dist_table()?;
    loop {
        if out.len() > MAX_INFLATE_BYTES {
            return Err("png: decompressed too large");
        }
        let bfinal = br.read_bit()?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                br.align_to_byte();
                if br.byte + 4 > deflate.len() {
                    return Err("png: truncated stored block");
                }
                let len = (deflate[br.byte] as u32) | ((deflate[br.byte + 1] as u32) << 8);
                let nlen = (deflate[br.byte + 2] as u32) | ((deflate[br.byte + 3] as u32) << 8);
                br.byte += 4;
                if len ^ 0xFFFF != nlen {
                    return Err("png: bad stored block lengths");
                }
                if br.byte + len as usize > deflate.len() {
                    return Err("png: truncated stored data");
                }
                if out.len() + len as usize > MAX_INFLATE_BYTES {
                    return Err("png: decompressed too large");
                }
                out.extend_from_slice(&deflate[br.byte..br.byte + len as usize]);
                br.byte += len as usize;
                if bfinal == 1 {
                    break;
                }
            }
            1 => {
                decode_huffman_block(&mut br, &fixed_lit, &fixed_dist, &mut out, bfinal)?;
                if bfinal == 1 {
                    break;
                }
            }
            2 => {
                let hlit = br.read_bits(5)? + 257;
                let hdist = br.read_bits(5)? + 1;
                let hclen = br.read_bits(4)? + 4;
                if hlit > 286 || hdist > 32 {
                    return Err("png: bad dynamic header");
                }
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl_lens = [0u8; 19];
                for i in 0..hclen as usize {
                    cl_lens[ORDER[i]] = br.read_bits(3)? as u8;
                }
                let cl_huff = DeflateHuff::from_lengths(&cl_lens)?;
                let total = hlit as usize + hdist as usize;
                let mut lens: Vec<u8> = Vec::with_capacity(total);
                while lens.len() < total {
                    let s = cl_huff.decode(&mut br)?;
                    match s {
                        0..=15 => lens.push(s as u8),
                        16 => {
                            let rep = br.read_bits(2)? + 3;
                            let prev = *lens.last().ok_or("png: bad repeat")?;
                            for _ in 0..rep {
                                lens.push(prev);
                            }
                        }
                        17 => {
                            let rep = br.read_bits(3)? + 3;
                            for _ in 0..rep {
                                lens.push(0);
                            }
                        }
                        18 => {
                            let rep = br.read_bits(7)? + 11;
                            for _ in 0..rep {
                                lens.push(0);
                            }
                        }
                        _ => return Err("png: bad code-length symbol"),
                    }
                }
                let lit = DeflateHuff::from_lengths(&lens[..hlit as usize])?;
                let d = DeflateHuff::from_lengths(&lens[hlit as usize..])?;
                decode_huffman_block(&mut br, &lit, &d, &mut out, bfinal)?;
                if bfinal == 1 {
                    break;
                }
            }
            _ => return Err("png: reserved deflate block type"),
        }
    }
    Ok(out)
}

fn decode_huffman_block(
    br: &mut LsBitReader,
    litlen: &DeflateHuff,
    dist: &DeflateHuff,
    out: &mut Vec<u8>,
    _bfinal: u32,
) -> Result<(), &'static str> {
    loop {
        let sym = litlen.decode(br)?;
        match sym {
            0..=255 => {
                if out.len() >= MAX_INFLATE_BYTES {
                    return Err("png: decompressed too large");
                }
                out.push(sym as u8);
            }
            256 => break, // end of block
            257..=285 => {
                let li = (sym - 257) as usize;
                let length = LENGTH_BASE[li] + br.read_bits(LENGTH_EXTRA[li])?;
                let dsym = dist.decode(br)? as usize;
                if dsym >= 30 {
                    return Err("png: bad distance symbol");
                }
                let d = DIST_BASE[dsym] + br.read_bits(DIST_EXTRA[dsym])? as u32;
                if d == 0 || d as usize > out.len() {
                    return Err("png: bad match distance");
                }
                if out.len() + length as usize > MAX_INFLATE_BYTES {
                    return Err("png: decompressed too large");
                }
                // LZ77: each step copies from `d` bytes back in the
                // *current* output (overlapping matches repeat).
                for _ in 0..length as usize {
                    let v = *out.get(out.len() - d as usize).ok_or("png: match oob")?;
                    out.push(v);
                }
            }
            _ => return Err("png: bad literal/length symbol"),
        }
    }
    Ok(())
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for chunk in data.chunks(4096) {
        for &b in chunk {
            s1 = (s1 + b as u32) % MOD;
            s2 = (s2 + s1) % MOD;
        }
    }
    (s2 << 16) | s1
}

fn zlib_decompress(zlib: &[u8]) -> Result<Vec<u8>, &'static str> {
    if zlib.len() < 6 {
        return Err("png: truncated zlib stream");
    }
    let cmf = zlib[0];
    let flg = zlib[1];
    if cmf & 0x0F != 8 {
        return Err("png: unsupported zlib compression");
    }
    if ((cmf as u16) * 256 + flg as u16) % 31 != 0 {
        return Err("png: bad zlib header");
    }
    if flg & 0x20 != 0 {
        return Err("png: zlib preset dict not supported");
    }
    let deflate = &zlib[2..zlib.len() - 4];
    let out = inflate_raw(deflate)?;
    let want = ((zlib[zlib.len() - 4] as u32) << 24)
        | ((zlib[zlib.len() - 3] as u32) << 16)
        | ((zlib[zlib.len() - 2] as u32) << 8)
        | (zlib[zlib.len() - 1] as u32);
    if adler32(&out) != want {
        return Err("png: checksum mismatch (corrupt file?)");
    }
    Ok(out)
}

// ──────────────────────────────────────────────
// PNG decoder
// ──────────────────────────────────────────────

fn read_u32be(b: &[u8], off: usize) -> Result<u32, &'static str> {
    if off + 4 > b.len() {
        return Err("png: truncated file");
    }
    Ok(((b[off] as u32) << 24)
        | ((b[off + 1] as u32) << 16)
        | ((b[off + 2] as u32) << 8)
        | (b[off + 3] as u32))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<DecodedImage, &'static str> {
    if !is_png(bytes) {
        return Err("not a PNG file");
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image too large (max 8 MiB)");
    }
    let mut off = 8usize;
    let mut w = 0u32;
    let mut h = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut have_ihdr = false;
    let mut idat: Vec<u8> = Vec::new();
    let mut plte: Vec<u8> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();

    while off + 8 <= bytes.len() {
        let len = read_u32be(bytes, off)? as usize;
        if off + 12 + len > bytes.len() {
            return Err("png: truncated chunk");
        }
        let typ = &bytes[off + 4..off + 8];
        let data = &bytes[off + 8..off + 8 + len];
        match typ {
            b"IHDR" => {
                if have_ihdr || len != 13 {
                    return Err("png: bad IHDR");
                }
                have_ihdr = true;
                w = read_u32be(data, 0)?;
                h = read_u32be(data, 4)?;
                bit_depth = *data.get(8).ok_or("png: bad IHDR")?;
                color_type = *data.get(9).ok_or("png: bad IHDR")?;
                let comp = *data.get(10).ok_or("png: bad IHDR")?;
                let filt = *data.get(11).ok_or("png: bad IHDR")?;
                let interlace = *data.get(12).ok_or("png: bad IHDR")?;
                if comp != 0 || filt != 0 {
                    return Err("png: unsupported compression/filter method");
                }
                if interlace != 0 {
                    return Err("png: interlaced PNG not supported (re-save without interlace)");
                }
                if w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
                    return Err("png: bad dimensions");
                }
                if (w as u64) * (h as u64) > MAX_IMAGE_PIXELS {
                    return Err("png: image has too many pixels");
                }
                match (color_type, bit_depth) {
                    (0, 1) | (0, 2) | (0, 4) | (0, 8) | (0, 16) => {}
                    (2, 8) | (2, 16) => {}
                    (3, 1) | (3, 2) | (3, 4) | (3, 8) => {}
                    (4, 8) | (4, 16) => {}
                    (6, 8) | (6, 16) => {}
                    _ => return Err("png: unsupported color type/bit depth"),
                }
            }
            b"PLTE" => {
                if len % 3 != 0 || len > 768 {
                    return Err("png: bad PLTE");
                }
                plte = data.to_vec();
            }
            b"IDAT" => {
                if idat.len() + len > MAX_IMAGE_BYTES {
                    return Err("png: IDAT too large");
                }
                idat.extend_from_slice(data);
            }
            b"tRNS" => {
                trns = data.to_vec();
            }
            b"IEND" => break,
            _ => {}
        }
        off += 12 + len;
        if typ == b"IEND" {
            break;
        }
    }
    if !have_ihdr {
        return Err("png: missing IHDR");
    }
    if idat.is_empty() {
        return Err("png: missing image data");
    }
    if color_type == 3 && plte.is_empty() {
        return Err("png: palette missing PLTE");
    }

    let raw = zlib_decompress(&idat)?;

    // Bytes per complete pixel for filtering.
    let channels: usize = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err("png: unsupported color type"),
    };
    let bpp: usize = if color_type == 0 && bit_depth < 8 {
        1
    } else if color_type == 3 && bit_depth < 8 {
        1
    } else {
        channels * ((bit_depth as usize + 7) / 8)
    };
    // Row data bytes (without filter byte).
    let row_bytes: usize = if (color_type == 0 || color_type == 3) && bit_depth < 8 {
        ((w as usize) * (bit_depth as usize) + 7) / 8
    } else {
        (w as usize) * bpp
    };
    if raw.len() != (h as usize) * (row_bytes + 1) {
        return Err("png: size mismatch (corrupt file?)");
    }

    // Unfilter.
    let mut pixels = alloc::vec![0u8; (h as usize) * row_bytes];
    let mut prev = alloc::vec![0u8; row_bytes];
    for y in 0..h as usize {
        let base = y * (row_bytes + 1);
        let f = raw[base];
        if f > 4 {
            return Err("png: bad filter type");
        }
        let cur_in = &raw[base + 1..base + 1 + row_bytes];
        let cur_out = &mut pixels[y * row_bytes..(y + 1) * row_bytes];
        match f {
            0 => cur_out.copy_from_slice(cur_in),
            1 => {
                for i in 0..row_bytes {
                    let a = if i >= bpp { cur_out[i - bpp] } else { 0 };
                    cur_out[i] = cur_in[i].wrapping_add(a);
                }
            }
            2 => {
                for i in 0..row_bytes {
                    cur_out[i] = cur_in[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                for i in 0..row_bytes {
                    let a = if i >= bpp { cur_out[i - bpp] } else { 0 };
                    let b = prev[i];
                    cur_out[i] = cur_in[i].wrapping_add(((a as u16 + b as u16) >> 1) as u8);
                }
            }
            4 => {
                for i in 0..row_bytes {
                    let a = if i >= bpp { cur_out[i - bpp] } else { 0 };
                    let b = prev[i];
                    let c = if i >= bpp { prev[i - bpp] } else { 0 };
                    cur_out[i] = cur_in[i].wrapping_add(paeth(a, b, c));
                }
            }
            _ => return Err("png: bad filter"),
        }
        prev.copy_from_slice(cur_out);
    }

    // Convert to RGBA8.
    let npix = (w as usize) * (h as usize);
    let mut rgba = alloc::vec![0u8; npix * 4];
    match color_type {
        0 => {
            // Grayscale.
            match bit_depth {
                1 | 2 | 4 => {
                    let mask = (1u8 << bit_depth) - 1;
                    let scale = 255 / mask as u16;
                    // Transparent gray via tRNS (2 bytes BE gray value).
                    let trans: Option<u8> = if trns.len() >= 2 {
                        let g16 = ((trns[0] as u16) << 8) | trns[1] as u16;
                        // Only exact low-bit values can match; scale down.
                        if g16 <= 255 && g16 % (scale as u16) == 0 {
                            Some((g16 / scale as u16) as u8)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    for y in 0..h as usize {
                        for x in 0..w as usize {
                            let bitpos = x * bit_depth as usize;
                            let byte = pixels[y * row_bytes + bitpos / 8];
                            let shift = 8 - (bitpos % 8) - bit_depth as usize;
                            let v = ((byte >> shift) & mask) as u16 * scale;
                            let g = v as u8;
                            let o = (y * w as usize + x) * 4;
                            rgba[o] = g;
                            rgba[o + 1] = g;
                            rgba[o + 2] = g;
                            rgba[o + 3] = if Some((byte >> shift) & mask) == trans {
                                0
                            } else {
                                255
                            };
                        }
                    }
                }
                8 => {
                    let trans: Option<u8> = if trns.len() >= 2 && trns[0] == 0 {
                        Some(trns[1])
                    } else {
                        None
                    };
                    for i in 0..npix {
                        let g = pixels[i];
                        let o = i * 4;
                        rgba[o] = g;
                        rgba[o + 1] = g;
                        rgba[o + 2] = g;
                        rgba[o + 3] = if Some(g) == trans { 0 } else { 255 };
                    }
                }
                16 => {
                    for i in 0..npix {
                        let g = pixels[i * 2]; // high byte
                        let o = i * 4;
                        rgba[o] = g;
                        rgba[o + 1] = g;
                        rgba[o + 2] = g;
                        rgba[o + 3] = 255;
                    }
                }
                _ => return Err("png: bad gray depth"),
            }
        }
        2 => {
            // Truecolor RGB.
            let trans: Option<(u8, u8, u8)> =
                if trns.len() >= 6 && trns[0] == 0 && trns[2] == 0 && trns[4] == 0 {
                    Some((trns[1], trns[3], trns[5]))
                } else {
                    None
                };
            match bit_depth {
                8 => {
                    for i in 0..npix {
                        let (r, g, b) = (pixels[i * 3], pixels[i * 3 + 1], pixels[i * 3 + 2]);
                        let o = i * 4;
                        rgba[o] = r;
                        rgba[o + 1] = g;
                        rgba[o + 2] = b;
                        rgba[o + 3] = if Some((r, g, b)) == trans { 0 } else { 255 };
                    }
                }
                16 => {
                    for i in 0..npix {
                        let o = i * 4;
                        rgba[o] = pixels[i * 6];
                        rgba[o + 1] = pixels[i * 6 + 2];
                        rgba[o + 2] = pixels[i * 6 + 4];
                        rgba[o + 3] = 255;
                    }
                }
                _ => return Err("png: bad RGB depth"),
            }
        }
        3 => {
            // Palette.
            let ncolors = plte.len() / 3;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let idx: usize = match bit_depth {
                        8 => pixels[y * row_bytes + x] as usize,
                        4 => {
                            let b = pixels[y * row_bytes + x / 2];
                            if x % 2 == 0 {
                                (b >> 4) as usize
                            } else {
                                (b & 0x0F) as usize
                            }
                        }
                        2 => {
                            let b = pixels[y * row_bytes + x / 4];
                            ((b >> (6 - (x % 4) * 2)) & 0x03) as usize
                        }
                        1 => {
                            let b = pixels[y * row_bytes + x / 8];
                            ((b >> (7 - (x % 8))) & 0x01) as usize
                        }
                        _ => return Err("png: bad palette depth"),
                    };
                    if idx >= ncolors {
                        return Err("png: palette index out of range");
                    }
                    let o = (y * w as usize + x) * 4;
                    rgba[o] = plte[idx * 3];
                    rgba[o + 1] = plte[idx * 3 + 1];
                    rgba[o + 2] = plte[idx * 3 + 2];
                    rgba[o + 3] = *trns.get(idx).unwrap_or(&255);
                }
            }
        }
        4 => {
            // Gray + alpha.
            match bit_depth {
                8 => {
                    for i in 0..npix {
                        let (g, a) = (pixels[i * 2], pixels[i * 2 + 1]);
                        let o = i * 4;
                        rgba[o] = g;
                        rgba[o + 1] = g;
                        rgba[o + 2] = g;
                        rgba[o + 3] = a;
                    }
                }
                16 => {
                    for i in 0..npix {
                        let o = i * 4;
                        rgba[o] = pixels[i * 4];
                        rgba[o + 1] = pixels[i * 4];
                        rgba[o + 2] = pixels[i * 4];
                        rgba[o + 3] = pixels[i * 4 + 2];
                    }
                }
                _ => return Err("png: bad gray-alpha depth"),
            }
        }
        6 => {
            // RGBA.
            match bit_depth {
                8 => {
                    for i in 0..npix {
                        let o = i * 4;
                        rgba[o] = pixels[i * 4];
                        rgba[o + 1] = pixels[i * 4 + 1];
                        rgba[o + 2] = pixels[i * 4 + 2];
                        rgba[o + 3] = pixels[i * 4 + 3];
                    }
                }
                16 => {
                    for i in 0..npix {
                        let o = i * 4;
                        rgba[o] = pixels[i * 8];
                        rgba[o + 1] = pixels[i * 8 + 2];
                        rgba[o + 2] = pixels[i * 8 + 4];
                        rgba[o + 3] = pixels[i * 8 + 6];
                    }
                }
                _ => return Err("png: bad RGBA depth"),
            }
        }
        _ => return Err("png: unsupported color type"),
    }

    // Composite semi-transparent pixels against black so the desktop
    // (which blits opaque) shows sane colors. Keep simple alpha-over.
    for i in 0..npix {
        let a = rgba[i * 4 + 3] as u16;
        if a != 255 {
            for c in 0..3 {
                let v = rgba[i * 4 + c] as u16;
                rgba[i * 4 + c] = ((v * a) / 255) as u8;
            }
            rgba[i * 4 + 3] = 255;
        }
    }

    Ok(DecodedImage { rgba, w, h })
}

// ──────────────────────────────────────────────
// JPEG decoder (baseline sequential, 8-bit)
// ──────────────────────────────────────────────

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

struct JpegHuff {
    counts: [u32; 17],
    offsets: [usize; 17],
    symbols: Vec<u8>,
}

impl JpegHuff {
    fn empty() -> Self {
        Self {
            counts: [0; 17],
            offsets: [0; 17],
            symbols: Vec::new(),
        }
    }

    fn build(&mut self, bits: &[u8; 16], vals: &[u8]) -> Result<(), &'static str> {
        self.counts = [0; 17];
        self.symbols.clear();
        let mut total = 0usize;
        for (i, &c) in bits.iter().enumerate() {
            self.counts[i + 1] = c as u32;
            total += c as usize;
        }
        if total != vals.len() {
            return Err("jpeg: bad DHT lengths");
        }
        // Order symbols by (len) preserving file order within len.
        let mut pos = 0usize;
        for len in 1..=16usize {
            self.offsets[len] = self.symbols.len();
            for _ in 0..self.counts[len] {
                self.symbols.push(*vals.get(pos).ok_or("jpeg: bad DHT")?);
                pos += 1;
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

/// Bit reader over JPEG entropy data (handles 0xFF00 stuffing).
struct JpegBits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    nbits: u8,
    /// Set when EOI seen; further reads fail.
    eoi: bool,
    /// Pending restart marker (0xD0..0xD7) consumed at byte boundary.
    pending_restart: Option<u8>,
}

impl<'a> JpegBits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            nbits: 0,
            eoi: false,
            pending_restart: None,
        }
    }

    fn fill(&mut self) -> Result<(), &'static str> {
        while self.nbits <= 24 {
            if self.pos >= self.data.len() {
                return Ok(());
            }
            let b = self.data[self.pos];
            self.pos += 1;
            if b != 0xFF {
                self.buf = (self.buf << 8) | b as u32;
                self.nbits += 8;
            } else {
                if self.pos >= self.data.len() {
                    self.eoi = true;
                    return Ok(());
                }
                let m = self.data[self.pos];
                self.pos += 1;
                if m == 0x00 {
                    self.buf = (self.buf << 8) | 0xFF;
                    self.nbits += 8;
                } else if (0xD0..=0xD7).contains(&m) {
                    // Restart: byte-aligned; bits before it were padded with 1s.
                    self.pending_restart = Some(m);
                    self.nbits = 0;
                    self.buf = 0;
                    return Ok(());
                } else if m == 0xD9 {
                    self.eoi = true;
                    return Ok(());
                } else {
                    return Err("jpeg: unexpected marker in scan");
                }
            }
        }
        Ok(())
    }

    fn get_bits(&mut self, n: u32) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        if n > 16 {
            return Err("jpeg: bad bit count");
        }
        self.fill()?;
        if (self.nbits as u32) < n {
            // Pad with 1s per spec if truncated.
            if self.eoi {
                return Err("jpeg: truncated scan");
            }
        }
        while (self.nbits as u32) < n {
            self.fill()?;
            if self.eoi && (self.nbits as u32) < n {
                return Err("jpeg: truncated scan");
            }
        }
        self.nbits -= n as u8;
        Ok((self.buf >> self.nbits) & ((1u32 << n) - 1))
    }

    fn decode_huff(&mut self, t: &JpegHuff) -> Result<u8, &'static str> {
        if t.is_empty() {
            return Err("jpeg: missing huffman table");
        }
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        for len in 1..=16usize {
            let bit = self.get_bits(1)?;
            code = (code << 1) | bit;
            let count = t.counts[len];
            if code.wrapping_sub(first) < count {
                let idx = t.offsets[len] + (code - first) as usize;
                return t.symbols.get(idx).copied().ok_or("jpeg: bad huffman code");
            }
            first = (first + count) << 1;
        }
        Err("jpeg: invalid huffman code")
    }

    fn take_restart(&mut self) -> Option<u8> {
        self.pending_restart.take()
    }
}

fn jpeg_extend(v: u32, s: u32) -> i32 {
    if s == 0 {
        return 0;
    }
    let vt = v as i32;
    let limit = 1i32 << (s - 1);
    if vt < limit {
        vt - ((1i32 << s) - 1)
    } else {
        vt
    }
}

#[derive(Clone, Copy)]
struct JpegComp {
    id: u8,
    h: u8,
    v: u8,
    q: u8,
    dc_tbl: u8,
    ac_tbl: u8,
}

/// cos(j*PI/16) for j in 0..16 (exact literals; no libm needed).
const COS_PI_16: [f32; 16] = [
    1.000000000,
    0.980785280,
    0.923879533,
    0.831469612,
    0.707106781,
    0.555570233,
    0.382683432,
    0.195090322,
    0.000000000,
    -0.195090322,
    -0.382683432,
    -0.555570233,
    -0.707106781,
    -0.831469612,
    -0.923879533,
    -0.980785280,
];

/// cos(m*PI/16) for any integer m (period 32, folded by symmetry).
fn cos_m_pi_16(m: u32) -> f32 {
    match m & 31 {
        j @ 0..=8 => COS_PI_16[j as usize],
        j @ 9..=16 => -COS_PI_16[(16 - j) as usize],
        j @ 17..=24 => -COS_PI_16[(j - 16) as usize],
        j => COS_PI_16[(32 - j) as usize],
    }
}

fn jpeg_idct_block(coeff: &[i32; 64], out: &mut [u8; 64]) {
    // Naive float 2D IDCT (AAN would be faster; this runs once per load).
    // C(u) = 1/sqrt(2) for u==0 else 1; orthonormal form carries 1/4.
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0f32;
            for v in 0..8 {
                let cv = if v == 0 { 0.70710678 } else { 1.0 };
                let cos_yv = cos_m_pi_16((2 * y as u32 + 1) * v as u32);
                for u in 0..8 {
                    let f = coeff[v * 8 + u] as f32;
                    if f != 0.0 {
                        let cu = if u == 0 { 0.70710678 } else { 1.0 };
                        sum += cu * cv * f * cos_m_pi_16((2 * x as u32 + 1) * u as u32) * cos_yv;
                    }
                }
            }
            // Manual round(): core float `round` is unavailable under
            // `-Zbuild-std-features=compiler-builtins-mem`.
            let v = sum / 4.0 + 128.0;
            let val = if v >= 0.0 {
                (v + 0.5) as i32
            } else {
                -((0.5 - v) as i32)
            };
            out[y * 8 + x] = val.clamp(0, 255) as u8;
        }
    }
}

fn read_u16be(b: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > b.len() {
        return Err("jpeg: truncated file");
    }
    Ok(((b[off] as u16) << 8) | b[off + 1] as u16)
}

pub fn decode_jpeg(bytes: &[u8]) -> Result<DecodedImage, &'static str> {
    if !is_jpeg(bytes) {
        return Err("not a JPEG file");
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image too large (max 8 MiB)");
    }

    let mut pos = 2usize; // skip SOI
    let mut w = 0u32;
    let mut h = 0u32;
    let mut comps = [JpegComp {
        id: 0,
        h: 1,
        v: 1,
        q: 0,
        dc_tbl: 0,
        ac_tbl: 0,
    }; 4];
    let mut ncomp = 0usize;
    let mut qtables = [[0u16; 64]; 4];
    let mut qset = [false; 4];
    let mut dc_huff = [
        JpegHuff::empty(),
        JpegHuff::empty(),
        JpegHuff::empty(),
        JpegHuff::empty(),
    ];
    let mut ac_huff = [
        JpegHuff::empty(),
        JpegHuff::empty(),
        JpegHuff::empty(),
        JpegHuff::empty(),
    ];
    let mut saw_sof = false;
    let mut restart_interval = 0u16;
    let mut scan_data: &[u8] = &[];
    let mut scan_comps: [JpegComp; 4] = comps;
    let mut scan_ncomp = 0usize;

    let next_marker = |pos: &mut usize| -> Result<u8, &'static str> {
        // Markers are FF [FF..] xx (xx != 00, FF).
        if *pos >= bytes.len() {
            return Err("jpeg: truncated file");
        }
        if bytes[*pos] != 0xFF {
            return Err("jpeg: expected marker");
        }
        let mut p = *pos + 1;
        while p < bytes.len() && bytes[p] == 0xFF {
            p += 1;
        }
        if p >= bytes.len() {
            return Err("jpeg: truncated marker");
        }
        let m = bytes[p];
        if m == 0x00 || m == 0x01 {
            return Err("jpeg: bad marker");
        }
        *pos = p + 1;
        Ok(m)
    };

    loop {
        let m = next_marker(&mut pos)?;
        if m == 0xD8 {
            continue; // SOI (extra)
        }
        if m == 0xD9 {
            break; // EOI
        }
        if (0xD0..=0xD7).contains(&m) {
            continue; // RST outside scan
        }
        if m == 0x01 {
            continue; // TEM
        }
        // Length-prefixed segment.
        if pos + 2 > bytes.len() {
            return Err("jpeg: truncated segment");
        }
        let seg_len = read_u16be(bytes, pos)? as usize;
        if seg_len < 2 || pos + seg_len > bytes.len() {
            return Err("jpeg: bad segment length");
        }
        let body = &bytes[pos + 2..pos + seg_len];
        match m {
            0xC0 | 0xC1 => {
                // SOF0 / SOF1 (baseline / extended sequential).
                if body.len() < 6 {
                    return Err("jpeg: bad SOF");
                }
                let prec = body[0];
                if prec != 8 {
                    return Err("jpeg: only 8-bit JPEG supported");
                }
                h = ((body[1] as u32) << 8) | body[2] as u32;
                w = ((body[3] as u32) << 8) | body[4] as u32;
                ncomp = body[5] as usize;
                if w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
                    return Err("jpeg: bad dimensions");
                }
                if (w as u64) * (h as u64) > MAX_IMAGE_PIXELS {
                    return Err("jpeg: image has too many pixels");
                }
                if ncomp != 1 && ncomp != 3 {
                    return Err("jpeg: only grayscale/RGB JPEG supported");
                }
                if body.len() < 6 + ncomp * 3 {
                    return Err("jpeg: bad SOF components");
                }
                for i in 0..ncomp {
                    let id = body[6 + i * 3];
                    let samp = body[6 + i * 3 + 1];
                    let q = body[6 + i * 3 + 2];
                    let (hh, vv) = ((samp >> 4) & 0x0F, samp & 0x0F);
                    if hh == 0 || vv == 0 || hh > 4 || vv > 4 {
                        return Err("jpeg: bad sampling factors");
                    }
                    if hh > 2 || vv > 2 {
                        return Err("jpeg: sampling beyond 4:2:0 not supported (re-save as 4:4:4/4:2:2/4:2:0)");
                    }
                    if q > 3 {
                        return Err("jpeg: bad quant table id");
                    }
                    comps[i] = JpegComp {
                        id,
                        h: hh,
                        v: vv,
                        q,
                        dc_tbl: 0,
                        ac_tbl: 0,
                    };
                }
                saw_sof = true;
            }
            0xC2 => return Err("jpeg: progressive JPEG not supported (re-save as baseline)"),
            0xC3 => return Err("jpeg: lossless JPEG not supported"),
            0xC4 => {
                // DHT.
                let mut p = 0usize;
                while p < body.len() {
                    if p + 17 > body.len() {
                        return Err("jpeg: bad DHT");
                    }
                    let tc_th = body[p];
                    p += 1;
                    let tc = (tc_th >> 4) & 0x0F;
                    let th = (tc_th & 0x0F) as usize;
                    if th > 3 || (tc != 0 && tc != 1) {
                        return Err("jpeg: bad DHT table id");
                    }
                    let mut bits = [0u8; 16];
                    bits.copy_from_slice(&body[p..p + 16]);
                    p += 16;
                    let total: usize = bits.iter().map(|&c| c as usize).sum();
                    if p + total > body.len() {
                        return Err("jpeg: bad DHT values");
                    }
                    let vals = &body[p..p + total];
                    p += total;
                    if tc == 0 {
                        dc_huff[th].build(&bits, vals)?;
                    } else {
                        ac_huff[th].build(&bits, vals)?;
                    }
                }
            }
            0xDB => {
                // DQT.
                let mut p = 0usize;
                while p < body.len() {
                    if p + 1 > body.len() {
                        return Err("jpeg: bad DQT");
                    }
                    let pq_tq = body[p];
                    p += 1;
                    let pq = (pq_tq >> 4) & 0x0F;
                    let tq = (pq_tq & 0x0F) as usize;
                    if tq > 3 {
                        return Err("jpeg: bad DQT id");
                    }
                    if pq == 0 {
                        if p + 64 > body.len() {
                            return Err("jpeg: bad DQT values");
                        }
                        for i in 0..64 {
                            qtables[tq][i] = body[p + i] as u16;
                        }
                        p += 64;
                    } else if pq == 1 {
                        if p + 128 > body.len() {
                            return Err("jpeg: bad DQT values");
                        }
                        for i in 0..64 {
                            qtables[tq][i] =
                                ((body[p + i * 2] as u16) << 8) | body[p + i * 2 + 1] as u16;
                        }
                        p += 128;
                    } else {
                        return Err("jpeg: bad DQT precision");
                    }
                    qset[tq] = true;
                }
            }
            0xDD => {
                if body.len() != 2 {
                    return Err("jpeg: bad DRI");
                }
                restart_interval = ((body[0] as u16) << 8) | body[1] as u16;
            }
            0xDA => {
                // SOS: scan header, then entropy data runs to EOI/RST-aware end.
                if !saw_sof {
                    return Err("jpeg: SOS before SOF");
                }
                if body.len() < 3 {
                    return Err("jpeg: bad SOS");
                }
                scan_ncomp = body[0] as usize;
                if scan_ncomp == 0 || scan_ncomp > ncomp || scan_ncomp > 4 {
                    return Err("jpeg: bad SOS component count");
                }
                if body.len() < 1 + scan_ncomp * 2 + 3 {
                    return Err("jpeg: bad SOS header");
                }
                for i in 0..scan_ncomp {
                    let id = body[1 + i * 2];
                    let tbl = body[1 + i * 2 + 1];
                    let slot = comps[..ncomp]
                        .iter()
                        .position(|c| c.id == id)
                        .ok_or("jpeg: bad SOS id")?;
                    let mut c = comps[slot];
                    c.dc_tbl = (tbl >> 4) & 0x0F;
                    c.ac_tbl = tbl & 0x0F;
                    if c.dc_tbl > 3 || c.ac_tbl > 3 {
                        return Err("jpeg: bad SOS table id");
                    }
                    scan_comps[i] = c;
                }
                // Ss, Se, Ah/Al ignored for baseline (must be 0,63,0).
                scan_data = &bytes[pos + seg_len..];
                break;
            }
            _ => {
                // APPn, COM, etc: skip.
            }
        }
        pos += seg_len;
    }

    if scan_data.is_empty() {
        return Err("jpeg: missing scan data");
    }
    // JpegBits stops at EOI on its own; trailing garbage after FFD9 is ignored.

    // Validate tables.
    for i in 0..scan_ncomp {
        let c = scan_comps[i];
        if !qset[c.q as usize] {
            return Err("jpeg: missing quant table");
        }
        if dc_huff[c.dc_tbl as usize].is_empty() || ac_huff[c.ac_tbl as usize].is_empty() {
            return Err("jpeg: missing huffman table");
        }
    }

    // MCU geometry.
    let (max_h, max_v) = if scan_ncomp == 1 {
        (1u32, 1u32)
    } else {
        let mut mh = 1u32;
        let mut mv = 1u32;
        for i in 0..scan_ncomp {
            mh = mh.max(scan_comps[i].h as u32);
            mv = mv.max(scan_comps[i].v as u32);
        }
        (mh, mv)
    };
    let mcu_w = 8 * max_h;
    let mcu_h = 8 * max_v;
    let mcu_cols = (w + mcu_w - 1) / mcu_w;
    let mcu_rows = (h + mcu_h - 1) / mcu_h;

    let mut rgba = alloc::vec![0u8; (w as usize) * (h as usize) * 4];
    let mut bits = JpegBits::new(scan_data);
    let mut prev_dc = [0i32; 4];
    let mut mcu_count = 0u32;

    // Scratch per data unit (8x8 pixels).
    let mut block_pixels: [[u8; 64]; 12] = [[0; 64]; 12];

    for my in 0..mcu_rows {
        for mx in 0..mcu_cols {
            // Decode all data units of this MCU in SOS order.
            let mut unit_base = [0usize; 4];
            let mut unit_idx = 0usize;
            for ci in 0..scan_ncomp {
                let c = scan_comps[ci];
                let n = (c.h as usize) * (c.v as usize);
                unit_base[ci] = unit_idx;
                for _ in 0..n {
                    // Decode one 8x8 block.
                    let mut coeff = [0i32; 64];
                    // DC.
                    let s = bits.decode_huff(&dc_huff[c.dc_tbl as usize])? as u32;
                    if s > 11 {
                        return Err("jpeg: bad DC size");
                    }
                    let diff = jpeg_extend(bits.get_bits(s)?, s);
                    prev_dc[ci] = prev_dc[ci].wrapping_add(diff);
                    coeff[0] = prev_dc[ci] * qtables[c.q as usize][0] as i32;
                    // AC coefficients.
                    let ac = &ac_huff[c.ac_tbl as usize];
                    let q = &qtables[c.q as usize];
                    let mut k = 1usize;
                    while k < 64 {
                        // Check restart pending between symbols? Restart only
                        // appears at byte boundary; our bit reader surfaces it
                        // via pending_restart after fill. If set, it means the
                        // scan ended unexpectedly mid-block.
                        if bits.pending_restart.is_some() {
                            return Err("jpeg: unexpected restart in block");
                        }
                        let rs = bits.decode_huff(ac)?;
                        let r = (rs >> 4) as usize;
                        let s = (rs & 0x0F) as u32;
                        if s == 0 {
                            if r == 15 {
                                k += 16;
                            } else {
                                break; // EOB
                            }
                        } else {
                            k += r;
                            if k >= 64 {
                                return Err("jpeg: bad AC run");
                            }
                            let v = jpeg_extend(bits.get_bits(s)?, s);
                            coeff[ZIGZAG[k]] = v * q[k] as i32;
                            k += 1;
                        }
                    }
                    let mut px = [0u8; 64];
                    jpeg_idct_block(&coeff, &mut px);
                    if unit_idx >= block_pixels.len() {
                        return Err("jpeg: too many units");
                    }
                    block_pixels[unit_idx] = px;
                    unit_idx += 1;
                }
            }
            // Restart handling at MCU boundary.
            if restart_interval != 0 {
                mcu_count += 1;
                if mcu_count % restart_interval as u32 == 0 {
                    // Consume to next byte boundary and expect RST.
                    // Our JpegBits surfaces pending_restart when the marker
                    // bytes are pulled by fill(). Force a fill to surface it.
                    let _ = bits.get_bits(0);
                    if let Some(_r) = bits.take_restart() {
                        prev_dc = [0; 4];
                    } else {
                        // Tolerate missing RST (some encoders omit padding):
                        // just reset predictors to avoid streaking.
                        prev_dc = [0; 4];
                    }
                }
            }

            // Upsample MCU area to RGB and write.
            if scan_ncomp == 1 {
                let base = unit_base[0];
                // Single comp: 1 unit per MCU (h=v=1 enforced by most encoders;
                // if larger, handle first unit only for gray).
                let px = &block_pixels[base];
                for dy in 0..8u32 {
                    for dx in 0..8u32 {
                        let x = mx * mcu_w + dx;
                        let y = my * mcu_h + dy;
                        if x < w && y < h {
                            let g = px[(dy * 8 + dx) as usize];
                            let o = ((y as usize) * (w as usize) + x as usize) * 4;
                            rgba[o] = g;
                            rgba[o + 1] = g;
                            rgba[o + 2] = g;
                            rgba[o + 3] = 255;
                        }
                    }
                }
            } else {
                // 3 comps: Y=0, Cb=1, Cr=2 (SOS order).
                let yb = unit_base[0];
                let cb = unit_base[1];
                let cr = unit_base[2];
                let yh = scan_comps[0].h as u32;
                let yv = scan_comps[0].v as u32;
                let cbh = scan_comps[1].h as u32;
                let cbv = scan_comps[1].v as u32;
                let crh = scan_comps[2].h as u32;
                let crv = scan_comps[2].v as u32;
                for dy in 0..mcu_h {
                    for dx in 0..mcu_w {
                        let x = mx * mcu_w + dx;
                        let y = my * mcu_h + dy;
                        if x >= w || y >= h {
                            continue;
                        }
                        // Nearest-neighbor chroma siting (matches most viewers
                        // closely enough for wallpaper).
                        let y_unit = ((dy * yv / max_v) / 8) * yh + ((dx * yh / max_h) / 8);
                        let y_px = &block_pixels[yb + y_unit as usize];
                        let yy = y_px[((dy * yv / max_v) % 8 * 8 + (dx * yh / max_h) % 8) as usize]
                            as i32;
                        let cb_unit = ((dy * cbv / max_v) / 8) * cbh + ((dx * cbh / max_h) / 8);
                        let cb_px = &block_pixels[cb + cb_unit as usize];
                        let ccb = cb_px
                            [((dy * cbv / max_v) % 8 * 8 + (dx * cbh / max_h) % 8) as usize]
                            as i32;
                        let cr_unit = ((dy * crv / max_v) / 8) * crh + ((dx * crh / max_h) / 8);
                        let cr_px = &block_pixels[cr + cr_unit as usize];
                        let ccr = cr_px
                            [((dy * crv / max_v) % 8 * 8 + (dx * crh / max_h) % 8) as usize]
                            as i32;
                        // YCbCr -> RGB (JFIF).
                        let cb_s = ccb - 128;
                        let cr_s = ccr - 128;
                        let r = (yy + ((359 * cr_s) >> 8)).clamp(0, 255) as u8;
                        let g = (yy - ((88 * cb_s + 183 * cr_s) >> 8)).clamp(0, 255) as u8;
                        let b = (yy + ((454 * cb_s) >> 8)).clamp(0, 255) as u8;
                        let o = ((y as usize) * (w as usize) + x as usize) * 4;
                        rgba[o] = r;
                        rgba[o + 1] = g;
                        rgba[o + 2] = b;
                        rgba[o + 3] = 255;
                    }
                }
            }
            if bits.eoi {
                // Ended mid-image: fill rest with black (truncated file).
                break;
            }
        }
        if bits.eoi {
            break;
        }
    }

    Ok(DecodedImage { rgba, w, h })
}

// ──────────────────────────────────────────────
// Scaling (nearest-neighbor; runs once per change)
// ──────────────────────────────────────────────

fn rgb_of(bg: u32) -> (u8, u8, u8) {
    (
        ((bg >> 16) & 0xFF) as u8,
        ((bg >> 8) & 0xFF) as u8,
        (bg & 0xFF) as u8,
    )
}

fn rgba_len(w: u32, h: u32) -> Option<usize> {
    (w as usize).checked_mul(h as usize)?.checked_mul(4)
}

fn zeroed_rgba(w: u32, h: u32) -> Option<Vec<u8>> {
    let len = rgba_len(w, h)?;
    let mut out = Vec::new();
    out.try_reserve_exact(len).ok()?;
    out.resize(len, 0);
    Some(out)
}

/// Scale `src` (RGBA) to exactly `dw×dh` with nearest neighbor.
pub fn scale_nearest_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Option<Vec<u8>> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Some(Vec::new());
    }
    if src.len() < rgba_len(sw, sh)? {
        return Some(Vec::new());
    }
    let mut out = zeroed_rgba(dw, dh)?;
    for y in 0..dh as usize {
        let sy = (y * sh as usize) / dh as usize;
        for x in 0..dw as usize {
            let sx = (x * sw as usize) / dw as usize;
            let s = (sy * sw as usize + sx) * 4;
            let d = (y * dw as usize + x) * 4;
            out[d] = src[s];
            out[d + 1] = src[s + 1];
            out[d + 2] = src[s + 2];
            out[d + 3] = 255;
        }
    }
    Some(out)
}

fn fill_solid(w: u32, h: u32, bg: u32) -> Option<Vec<u8>> {
    let (r, g, b) = rgb_of(bg);
    let mut out = zeroed_rgba(w, h)?;
    for pixel in out.chunks_exact_mut(4) {
        pixel[0] = r;
        pixel[1] = g;
        pixel[2] = b;
        pixel[3] = 255;
    }
    Some(out)
}

/// Render `src` into a screen-sized opaque buffer for `mode`.
pub fn build_display_cache(
    src: Option<&DecodedImage>,
    mode: WallpaperMode,
    scr_w: u32,
    scr_h: u32,
    bg: u32,
) -> Option<Vec<u8>> {
    if scr_w == 0 || scr_h == 0 {
        return Some(Vec::new());
    }
    let Some(img) = src else {
        return Some(Vec::new());
    };
    if img.w == 0 || img.h == 0 || img.rgba.is_empty() {
        return fill_solid(scr_w, scr_h, bg);
    }
    match mode {
        WallpaperMode::Solid => fill_solid(scr_w, scr_h, bg),
        WallpaperMode::Stretch => scale_nearest_rgba(&img.rgba, img.w, img.h, scr_w, scr_h),
        WallpaperMode::Fit => {
            // scale = min(sw/iw, sh/ih); center on bg.
            let scale_w = (scr_w as u64) * (img.h as u64);
            let scale_h = (scr_h as u64) * (img.w as u64);
            let (dw, dh) = if scale_w <= scale_h {
                // width-constrained? compare scr_w/img.w vs scr_h/img.h
                let dw = scr_w;
                let dh = ((img.h as u64) * (scr_w as u64) / (img.w as u64).max(1)) as u32;
                (dw, dh.min(scr_h))
            } else {
                let dh = scr_h;
                let dw = ((img.w as u64) * (scr_h as u64) / (img.h as u64).max(1)) as u32;
                (dw.min(scr_w), dh)
            };
            let dw = dw.max(1);
            let dh = dh.max(1);
            let mut out = fill_solid(scr_w, scr_h, bg)?;
            let scaled = scale_nearest_rgba(&img.rgba, img.w, img.h, dw, dh)?;
            let ox = (scr_w.saturating_sub(dw)) / 2;
            let oy = (scr_h.saturating_sub(dh)) / 2;
            for y in 0..dh as usize {
                for x in 0..dw as usize {
                    let s = (y * dw as usize + x) * 4;
                    let d = (((oy as usize) + y) * scr_w as usize + (ox as usize) + x) * 4;
                    if d + 3 < out.len() && s + 3 < scaled.len() {
                        out[d] = scaled[s];
                        out[d + 1] = scaled[s + 1];
                        out[d + 2] = scaled[s + 2];
                        out[d + 3] = 255;
                    }
                }
            }
            Some(out)
        }
        WallpaperMode::Fill => {
            // scale = max(sw/iw, sh/ih); center-crop.
            let (tw, th) = if (scr_w as u64) * (img.h as u64) >= (scr_h as u64) * (img.w as u64) {
                let tw = scr_w;
                let th = ((img.h as u64) * (scr_w as u64) / (img.w as u64).max(1)) as u32;
                (tw, th.max(1))
            } else {
                let th = scr_h;
                let tw = ((img.w as u64) * (scr_h as u64) / (img.h as u64).max(1)) as u32;
                (tw.max(1), th)
            };
            let mut out = zeroed_rgba(scr_w, scr_h)?;
            let scaled = scale_nearest_rgba(&img.rgba, img.w, img.h, tw, th)?;
            let ox = tw.saturating_sub(scr_w) / 2;
            let oy = th.saturating_sub(scr_h) / 2;
            for y in 0..scr_h as usize {
                for x in 0..scr_w as usize {
                    let s = (((oy as usize) + y) * tw as usize + (ox as usize) + x) * 4;
                    let d = (y * scr_w as usize + x) * 4;
                    if s + 3 < scaled.len() {
                        out[d] = scaled[s];
                        out[d + 1] = scaled[s + 1];
                        out[d + 2] = scaled[s + 2];
                        out[d + 3] = 255;
                    }
                }
            }
            Some(out)
        }
        WallpaperMode::Center => {
            let mut out = fill_solid(scr_w, scr_h, bg)?;
            // Center 1:1; crop if image bigger than screen.
            let sx0 = img.w.saturating_sub(scr_w) / 2;
            let sy0 = img.h.saturating_sub(scr_h) / 2;
            let dx0 = scr_w.saturating_sub(img.w) / 2;
            let dy0 = scr_h.saturating_sub(img.h) / 2;
            let cw = img.w.min(scr_w);
            let ch = img.h.min(scr_h);
            for y in 0..ch as usize {
                for x in 0..cw as usize {
                    let s = (((sy0 as usize) + y) * img.w as usize + (sx0 as usize) + x) * 4;
                    let d = (((dy0 as usize) + y) * scr_w as usize + (dx0 as usize) + x) * 4;
                    if s + 3 < img.rgba.len() && d + 3 < out.len() {
                        out[d] = img.rgba[s];
                        out[d + 1] = img.rgba[s + 1];
                        out[d + 2] = img.rgba[s + 2];
                        out[d + 3] = 255;
                    }
                }
            }
            Some(out)
        }
        WallpaperMode::Tile => {
            let mut out = zeroed_rgba(scr_w, scr_h)?;
            for y in 0..scr_h as usize {
                for x in 0..scr_w as usize {
                    let sx = x % img.w as usize;
                    let sy = y % img.h as usize;
                    let s = (sy * img.w as usize + sx) * 4;
                    let d = (y * scr_w as usize + x) * 4;
                    if s + 3 < img.rgba.len() {
                        out[d] = img.rgba[s];
                        out[d + 1] = img.rgba[s + 1];
                        out[d + 2] = img.rgba[s + 2];
                        out[d + 3] = 255;
                    }
                }
            }
            Some(out)
        }
    }
}

/// Small Fit preview for the Settings window (opaque RGBA).
pub fn make_preview(
    src: &DecodedImage,
    max_w: u32,
    max_h: u32,
    bg: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let (mw, mh) = (max_w.max(1), max_h.max(1));
    let (dw, dh) = if (src.w as u64) * (mh as u64) <= (mw as u64) * (src.h as u64) {
        // height-constrained? fit inside box
        let dh = mh.min(src.h.max(1));
        let dw = ((src.w as u64) * (dh as u64) / (src.h as u64).max(1)) as u32;
        (dw.max(1).min(mw), dh)
    } else {
        let dw = mw.min(src.w.max(1));
        let dh = ((src.h as u64) * (dw as u64) / (src.w as u64).max(1)) as u32;
        (dw, dh.max(1).min(mh))
    };
    let scaled = scale_nearest_rgba(&src.rgba, src.w, src.h, dw, dh)?;
    let mut out = fill_solid(mw, mh, 0x14181d)?;
    let ox = mw.saturating_sub(dw) / 2;
    let oy = mh.saturating_sub(dh) / 2;
    for y in 0..dh as usize {
        for x in 0..dw as usize {
            let s = (y * dw as usize + x) * 4;
            let d = (((oy as usize) + y) * mw as usize + (ox as usize) + x) * 4;
            if s + 3 < scaled.len() && d + 3 < out.len() {
                out[d] = scaled[s];
                out[d + 1] = scaled[s + 1];
                out[d + 2] = scaled[s + 2];
                out[d + 3] = 255;
            }
        }
    }
    let _ = bg;
    Some((out, mw, mh))
}

// ──────────────────────────────────────────────
// Global store
// ──────────────────────────────────────────────

struct WallpaperState {
    original: Option<DecodedImage>,
    path: String,
    mode: WallpaperMode,
    bg: u32,
    accent: u32,
    cache: Vec<u8>,
    cache_w: u32,
    cache_h: u32,
}

impl WallpaperState {
    const fn new() -> Self {
        Self {
            original: None,
            path: String::new(),
            mode: WallpaperMode::Solid,
            bg: 0x102a4e,
            accent: 0x00be5a,
            cache: Vec::new(),
            cache_w: 0,
            cache_h: 0,
        }
    }
}

static WALLPAPER: Mutex<WallpaperState> = Mutex::new(WallpaperState::new());

fn rebuild_locked(st: &mut WallpaperState, scr_w: u32, scr_h: u32) {
    st.cache = Vec::new();
    st.cache_w = 0;
    st.cache_h = 0;
    if scr_w == 0 || scr_h == 0 || st.original.is_none() {
        return;
    }
    if (scr_w as u64) * (scr_h as u64) > MAX_CACHE_PIXELS {
        return;
    }
    let Some(cache) = build_display_cache(st.original.as_ref(), st.mode, scr_w, scr_h, st.bg)
    else {
        crate::serial_println!(
            "[wallpaper] cache unavailable at {}x{}; using direct renderer",
            scr_w,
            scr_h
        );
        return;
    };
    st.cache = cache;
    st.cache_w = scr_w;
    st.cache_h = scr_h;
}

/// Install decoded bytes as the wallpaper (decodes + recaches).
pub fn set_wallpaper_bytes(
    bytes: &[u8],
    path: &str,
    scr_w: u32,
    scr_h: u32,
) -> Result<(u32, u32), &'static str> {
    let img = decode_auto(bytes)?;
    let (w, h) = (img.w, img.h);
    let mut st = WALLPAPER.lock();
    st.path = String::from(path.trim());
    st.original = Some(img);
    if st.mode == WallpaperMode::Solid {
        st.mode = WallpaperMode::Fill;
    }
    rebuild_locked(&mut st, scr_w, scr_h);
    Ok((w, h))
}

pub fn clear_wallpaper(scr_w: u32, scr_h: u32) {
    let mut st = WALLPAPER.lock();
    st.original = None;
    st.path = String::new();
    st.mode = WallpaperMode::Solid;
    rebuild_locked(&mut st, scr_w, scr_h);
}

pub fn set_mode(mode: WallpaperMode, scr_w: u32, scr_h: u32) {
    let mut st = WALLPAPER.lock();
    st.mode = mode;
    rebuild_locked(&mut st, scr_w, scr_h);
}

pub fn set_colors(bg: u32, accent: u32, scr_w: u32, scr_h: u32) {
    let mut st = WALLPAPER.lock();
    st.bg = bg & 0xFFFFFF;
    st.accent = accent & 0xFFFFFF;
    rebuild_locked(&mut st, scr_w, scr_h);
}

pub fn apply_config(cfg: &WallpaperConfig, scr_w: u32, scr_h: u32) {
    let mut st = WALLPAPER.lock();
    st.mode = cfg.mode;
    st.bg = cfg.bg & 0xFFFFFF;
    st.accent = cfg.accent & 0xFFFFFF;
    // Path applied separately once bytes are loaded; keep existing image
    // unless caller clears/loads.
    rebuild_locked(&mut st, scr_w, scr_h);
}

pub fn rebuild_for_screen(scr_w: u32, scr_h: u32) {
    rebuild_locked(&mut WALLPAPER.lock(), scr_w, scr_h);
}

pub fn current_path() -> String {
    WALLPAPER.lock().path.clone()
}

pub fn current_colors() -> (u32, u32) {
    let st = WALLPAPER.lock();
    (st.bg, st.accent)
}

pub fn original_dims() -> Option<(u32, u32)> {
    WALLPAPER.lock().original.as_ref().map(|i| (i.w, i.h))
}

pub fn current_config() -> WallpaperConfig {
    let st = WALLPAPER.lock();
    WallpaperConfig {
        path: st.path.clone(),
        mode: st.mode,
        bg: st.bg,
        accent: st.accent,
    }
}

/// Paint the cached wallpaper fullscreen (dirty-rect clipping is handled
/// by `fb_gfx` via the active draw clip). Returns true when an image cache
/// matching `scr_w × scr_h` was painted; false means the caller should draw
/// the gradient/solid fallback instead.
pub fn paint_cached_fullscreen(scr_w: u32, scr_h: u32) -> bool {
    // Hold the lock across the blit (blit copies synchronously; lock order
    // is wallpaper -> framebuffer, never reversed). Cloning megabytes per
    // dirty rect would be far worse.
    let guard = WALLPAPER.lock();
    if guard.cache.is_empty() || guard.cache_w != scr_w || guard.cache_h != scr_h {
        return false;
    }
    if guard.cache.len() < (scr_w as usize) * (scr_h as usize) * 4 {
        return false;
    }
    // Only paint when there is an actual image; solid colors stay on the
    // cheap gradient path (identical pixels, less memory traffic).
    if guard.original.is_none() {
        return false;
    }
    crate::drivers::fb_gfx::blit_rgba(0, 0, scr_w as usize, scr_h as usize, &guard.cache);
    true
}

/// Build a preview RGBA for the current original (if any).
pub fn preview_rgba(max_w: u32, max_h: u32) -> Option<(Vec<u8>, u32, u32)> {
    let st = WALLPAPER.lock();
    let img = st.original.as_ref()?;
    make_preview(img, max_w, max_h, st.bg)
}
