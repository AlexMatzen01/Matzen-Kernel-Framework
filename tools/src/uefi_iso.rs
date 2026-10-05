//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! UEFI ISO creation for the MFK runner.
//!
//! Pure-std implementation (no external tools, no new crates) so `mfk-runner`
//! can emit a Hyper-V Gen2-bootable UEFI DVD image on any host:
//!
//! ```text
//! <kernel>-uefi.img  ->  extract ESP files  ->  efiboot.img (FAT16)
//!                                             ->  <kernel>-uefi.iso (ISO9660 + El Torito)
//! ```
//!
//! The ESP produced by `bootloader 0.11` (`UefiBoot`) is a GPT disk with one
//! FAT16 partition containing `EFI/BOOT/BOOTX64.EFI`, the kernel file and
//! `NvVars`. All three are copied byte-for-byte into the El Torito boot
//! image, so the EFI loader finds exactly what it expects. The ISO9660 tree
//! itself carries the same files under 8.3 names (informational only).

use std::path::Path;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One file extracted from the ESP, path uses `/` separators.
#[derive(Debug, Clone)]
pub struct EspFile {
    pub path: String,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Little-endian helpers
// ---------------------------------------------------------------------------

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

fn w16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn w32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn both16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    buf[off + 2..off + 4].copy_from_slice(&v.to_be_bytes());
}

fn both32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    buf[off + 4..off + 8].copy_from_slice(&v.to_be_bytes());
}

// ---------------------------------------------------------------------------
// GPT parsing: locate the ESP
// ---------------------------------------------------------------------------

/// ESP partition type GUID in `bytes_le` order:
/// C12A7328-F81F-11D2-BA4B-00A0C93EC93B
const ESP_GUID_LE: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
    0x3B,
];

fn find_esp(img: &[u8]) -> Result<(u64, u64), String> {
    if img.len() < 3 * 512 {
        return Err("UEFI image too small to contain GPT".to_string());
    }
    if &img[512..520] != b"EFI PART" {
        return Err("UEFI image has no GPT header (not a bootloader UEFI disk?)".to_string());
    }
    let entry_lba = u64le(img, 512 + 72);
    let entry_count = u32le(img, 512 + 80);
    let entry_size = u32le(img, 512 + 84).max(128);
    for i in 0..entry_count {
        let off = entry_lba as usize * 512 + i as usize * entry_size as usize;
        if off + 128 > img.len() {
            break;
        }
        if img[off..off + 16] == ESP_GUID_LE {
            let start = u64le(img, off + 32);
            let end = u64le(img, off + 40);
            if end > start {
                return Ok((start, end));
            }
        }
    }
    Err("No ESP partition found in UEFI image".to_string())
}

// ---------------------------------------------------------------------------
// FAT reading (FAT12/16/32)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

struct FatVol<'a> {
    img: &'a [u8],
    part_start: u64,
    bps: u16,
    spc: u8,
    reserved: u16,
    _fats: u8,
    _fatsz: u32,
    root_lba: u64,
    root_sectors: u32,
    data_lba: u64,
    kind: FatKind,
    root_cluster: u32,
}

impl<'a> FatVol<'a> {
    fn parse(img: &'a [u8], part_start: u64, part_end: u64) -> Result<FatVol<'a>, String> {
        let boot_off = part_start as usize * 512;
        if boot_off + 512 > img.len() {
            return Err("ESP boot sector out of range".to_string());
        }
        let boot = &img[boot_off..boot_off + 512];
        if boot[510] != 0x55 || boot[511] != 0xAA {
            return Err("ESP boot sector signature missing".to_string());
        }
        let bps = u16le(boot, 11);
        let spc = boot[13];
        let reserved = u16le(boot, 14);
        let fats = boot[16];
        let rootents = u16le(boot, 17);
        let tot16 = u16le(boot, 19);
        let fatsz16 = u16le(boot, 22);
        let tot32 = u32le(boot, 32);
        let fatsz32 = u32le(boot, 36);
        let rootclus = u32le(boot, 44);
        if bps != 512 || spc == 0 || fats == 0 {
            return Err("Unsupported ESP geometry (bps/spc)".to_string());
        }
        let total = if tot16 != 0 {
            tot16 as u64
        } else {
            tot32 as u64
        };
        if total == 0 {
            return Err("ESP has zero total sectors".to_string());
        }
        // Clamp to the GPT partition size so corrupt BPBs cannot OOB-read.
        let part_sectors = part_end - part_start + 1;
        let total = total.min(part_sectors);
        let fatsz = if fatsz16 != 0 { fatsz16 as u32 } else { fatsz32 };
        if fatsz == 0 {
            return Err("ESP FAT size is zero".to_string());
        }
        let root_sectors = (rootents as u32 * 32).div_ceil(bps as u32);
        let data_sectors = total.saturating_sub(reserved as u64 + fats as u64 * fatsz as u64 + root_sectors as u64);
        let clusters = data_sectors / spc as u64;
        let kind = if clusters < 4085 {
            FatKind::Fat12
        } else if clusters < 65525 {
            FatKind::Fat16
        } else {
            FatKind::Fat32
        };
        let fat_lba = part_start + reserved as u64;
        let root_lba = fat_lba + fats as u64 * fatsz as u64;
        let data_lba = root_lba + root_sectors as u64;
        Ok(FatVol {
            img,
            part_start,
            bps,
            spc,
            reserved,
            _fats: fats,
            _fatsz: fatsz,
            root_lba,
            root_sectors,
            data_lba,
            kind,
            root_cluster: if kind == FatKind::Fat32 {
                if rootclus < 2 { 2 } else { rootclus }
            } else {
                0
            },
        })
    }

    fn fat_entry(&self, cluster: u32) -> u32 {
        let fat_off = (self.part_start + self.reserved as u64) as usize * 512;
        match self.kind {
            FatKind::Fat16 => {
                let off = fat_off + cluster as usize * 2;
                if off + 2 > self.img.len() {
                    return 0xFFFF;
                }
                u16le(self.img, off) as u32
            }
            FatKind::Fat32 => {
                let off = fat_off + cluster as usize * 4;
                if off + 4 > self.img.len() {
                    return 0x0FFF_FFFF;
                }
                u32le(self.img, off) & 0x0FFF_FFFF
            }
            FatKind::Fat12 => {
                let off = fat_off + cluster as usize * 3 / 2;
                if off + 2 > self.img.len() {
                    return 0xFFF;
                }
                let v = u16le(self.img, off);
                if cluster & 1 == 0 {
                    (v & 0x0FFF) as u32
                } else {
                    (v >> 4) as u32
                }
            }
        }
    }

    fn end_of_chain(&self, v: u32) -> bool {
        match self.kind {
            FatKind::Fat12 => v >= 0xFF8,
            FatKind::Fat16 => v >= 0xFFF8,
            FatKind::Fat32 => v >= 0x0FFF_FFF8,
        }
    }

    fn cluster_lba(&self, cluster: u32) -> Option<u64> {
        if cluster < 2 {
            return None;
        }
        Some(self.data_lba + (cluster as u64 - 2) * self.spc as u64)
    }

    fn read_chain(&self, start: u32, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut c = start;
        let mut guard = 0usize;
        let cluster_bytes = self.spc as usize * self.bps as usize;
        loop {
            if c < 2 {
                break;
            }
            let Some(lba) = self.cluster_lba(c) else {
                break;
            };
            let off = lba as usize * self.bps as usize;
            if off + cluster_bytes > self.img.len() {
                break;
            }
            out.extend_from_slice(&self.img[off..off + cluster_bytes]);
            let next = self.fat_entry(c);
            if self.end_of_chain(next) {
                break;
            }
            c = next;
            guard += 1;
            if guard > 1_000_000 || out.len() > 256 * 1024 * 1024 {
                break;
            }
        }
        out.truncate(size);
        out
    }

    fn root_bytes(&self) -> Vec<u8> {
        if self.kind == FatKind::Fat32 {
            self.read_chain(self.root_cluster, usize::MAX)
        } else {
            let off = self.root_lba as usize * self.bps as usize;
            let len = self.root_sectors as usize * self.bps as usize;
            if off + len > self.img.len() {
                return Vec::new();
            }
            self.img[off..off + len].to_vec()
        }
    }
}

/// Short-name checksum used by LFN entries.
fn short_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &b in short.iter() {
        sum = sum.rotate_right(1).wrapping_add(b);
    }
    sum
}

/// Parse one directory buffer into `(name, attr, cluster, size)` entries,
/// resolving LFN entries. Returns names as stored (original case).
fn parse_dir(buf: &[u8], fat32: bool) -> Vec<(String, u8, u32, u32)> {
    let mut out = Vec::new();
    // (seq, u16-chars)
    let mut lfn_parts: Vec<(u8, Vec<u16>)> = Vec::new();
    // Checksum stored in the LFN entries (offset 13 there); the short entry
    // has unrelated data at that offset, so it must be saved here.
    let mut lfn_sum: u8 = 0;
    let mut i = 0;
    while i + 32 <= buf.len() {
        let e = &buf[i..i + 32];
        i += 32;
        if e[0] == 0x00 {
            break;
        }
        if e[0] == 0xE5 {
            lfn_parts.clear();
            continue;
        }
        let attr = e[11];
        if attr == 0x0F {
            let seq = e[0] & 0x1F;
            lfn_sum = e[13];
            let mut chars = Vec::with_capacity(13);
            for k in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                chars.push(u16::from_le_bytes([e[k], e[k + 1]]));
            }
            lfn_parts.push((seq, chars));
            continue;
        }
        let is_label = attr & 0x08 != 0 && attr & 0x10 == 0;
        let name_raw = &e[0..8];
        let ext_raw = &e[8..11];
        let cluster = if fat32 {
            ((u16le(e, 20) as u32) << 16) | u16le(e, 26) as u32
        } else {
            u16le(e, 26) as u32
        };
        let size = u32le(e, 28);
        if is_label {
            lfn_parts.clear();
            continue;
        }
        let name_str = String::from_utf8_lossy(name_raw).trim_end().to_string();
        let ext_str = String::from_utf8_lossy(ext_raw).trim_end().to_string();
        let short = if ext_str.is_empty() {
            name_str.clone()
        } else {
            format!("{}.{}", name_str, ext_str)
        };
        // Resolve LFN: sort parts by seq, concat, strip padding.
        let mut full = short.clone();
        if !lfn_parts.is_empty() {
            let mut shorts = [b' '; 11];
            shorts[..8].copy_from_slice(name_raw);
            shorts[8..11].copy_from_slice(ext_raw);
            if short_checksum(&shorts) == lfn_sum {
                let mut ordered = lfn_parts.clone();
                ordered.sort_by_key(|(s, _)| *s);
                let mut u: Vec<u16> = Vec::new();
                for (_, c) in ordered {
                    u.extend_from_slice(&c);
                }
                // Strip trailing 0x0000 / 0xFFFF padding.
                while matches!(u.last(), Some(0x0000) | Some(0xFFFF)) {
                    u.pop();
                }
                if !u.is_empty() {
                    full = String::from_utf16_lossy(&u);
                }
            }
            lfn_parts.clear();
        }
        out.push((full, attr, cluster, size));
    }
    out
}

fn walk_esp(vol: &FatVol) -> Result<Vec<EspFile>, String> {
    let fat32 = vol.kind == FatKind::Fat32;
    let mut files = Vec::new();
    let mut stack: Vec<(String, Vec<u8>)> = vec![(String::new(), vol.root_bytes())];
    while let Some((prefix, buf)) = stack.pop() {
        for (name, attr, cluster, size) in parse_dir(&buf, fat32) {
            if name == "." || name == ".." {
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };
            if attr & 0x10 != 0 {
                if cluster >= 2 {
                    let sub = vol.read_chain(cluster, usize::MAX);
                    stack.push((path, sub));
                }
            } else if attr & 0x08 == 0 {
                let data = if cluster >= 2 {
                    vol.read_chain(cluster, size as usize)
                } else if size == 0 {
                    Vec::new()
                } else {
                    return Err(format!("File has no cluster: {}", path));
                };
                files.push(EspFile { path, data });
            }
        }
    }
    if files.is_empty() {
        return Err("ESP contains no files".to_string());
    }
    Ok(files)
}

/// Extract all files from the ESP of a bootloader UEFI disk image.
pub fn extract_esp_files(uefi_img: &Path) -> Result<Vec<EspFile>, String> {
    let img =
        std::fs::read(uefi_img).map_err(|e| format!("Cannot read {}: {}", uefi_img.display(), e))?;
    let (start, end) = find_esp(&img)?;
    let vol = FatVol::parse(&img, start, end)?;
    walk_esp(&vol)
}

// ---------------------------------------------------------------------------
// FAT16 writer (for the El Torito boot image)
// ---------------------------------------------------------------------------

/// 8.3 short name for a file (`NAME.EXT` form). Dot/dotdot handled separately.
fn split_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    if name == "." {
        out[0] = b'.';
        return out;
    }
    if name == ".." {
        out[0] = b'.';
        out[1] = b'.';
        return out;
    }
    if let Some((stem, ext)) = name.rsplit_once('.') {
        let s = stem.to_ascii_uppercase();
        let e = ext.to_ascii_uppercase();
        let sb = s.as_bytes();
        let eb = e.as_bytes();
        let n = sb.len().min(8);
        out[..n].copy_from_slice(&sb[..n]);
        let m = eb.len().min(3);
        out[8..8 + m].copy_from_slice(&eb[..m]);
    } else {
        let s = name.to_ascii_uppercase();
        let sb = s.as_bytes();
        let n = sb.len().min(8);
        // No extension: if longer than 8, it must have been pre-mangled.
        out[..n].copy_from_slice(&sb[..n]);
    }
    out
}

/// Write LFN entries (in reverse order) for `long` with checksum `sum`.
/// Returns bytes to prepend before the short entry.
fn lfn_entries(long: &str, sum: u8) -> Vec<u8> {
    let mut chars: Vec<u16> = long.encode_utf16().collect();
    // LFN entries hold 13 chars each.
    let n = chars.len().div_ceil(13);
    // Pad with 0xFFFF, terminate with 0x0000 if room.
    chars.push(0x0000);
    while chars.len() < n * 13 {
        chars.push(0xFFFF);
    }
    let mut out = Vec::with_capacity(n * 32);
    for idx in 0..n {
        // Stored reverse: first entry holds the LAST chunk.
        let chunk_idx = n - 1 - idx;
        let chunk = &chars[chunk_idx * 13..chunk_idx * 13 + 13];
        let mut e = [0u8; 32];
        let mut seq = (chunk_idx + 1) as u8;
        if idx == 0 {
            seq |= 0x40;
        }
        e[0] = seq;
        let copy = |dst: &mut [u8], u: &[u16]| {
            for (k, c) in u.iter().enumerate() {
                dst[k * 2..k * 2 + 2].copy_from_slice(&c.to_le_bytes());
            }
        };
        copy(&mut e[1..11], &chunk[0..5]);
        e[11] = 0x0F;
        e[12] = 0x00;
        e[13] = sum;
        copy(&mut e[14..26], &chunk[5..11]);
        w16(&mut e, 26, 0);
        copy(&mut e[28..32], &chunk[11..13]);
        out.extend_from_slice(&e);
    }
    out
}

fn short_entry(short: &[u8; 11], attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(short);
    e[11] = attr;
    e[12] = 0; // NT reserved
    e[13] = 0; // creation Vittles (deterministic: zero)
    w16(&mut e, 14, 0x6000); // creation time 12:00
    w16(&mut e, 16, 0x5A21); // creation date 2025-01-01
    w16(&mut e, 18, 0x5A21); // access date
    w16(&mut e, 20, (cluster >> 16) as u16);
    w16(&mut e, 22, 0x6000); // write time
    w16(&mut e, 24, 0x5A21); // write date
    w16(&mut e, 26, (cluster & 0xFFFF) as u16);
    w32(&mut e, 28, size);
    e
}

/// Case-insensitive path lookup.
fn find_file<'f>(files: &'f [EspFile], want: &str) -> Option<&'f EspFile> {
    files.iter().find(|f| f.path.eq_ignore_ascii_case(want))
}

/// Build a FAT16 `efiboot.img` containing the ESP boot files.
///
/// Canonical layout: `/EFI/BOOT/BOOTX64.EFI` + the kernel + `NvVars` in the
/// root. The kernel is the largest root-level non-EFI file (robust against
/// bootloader renames); `NvVars` matches case-insensitively.
fn build_efiboot_img(files: &[EspFile]) -> Result<Vec<u8>, String> {
    let efi = find_file(files, "EFI/BOOT/BOOTX64.EFI")
        .ok_or_else(|| "ESP is missing EFI/BOOT/BOOTX64.EFI".to_string())?;
    let nvvars = find_file(files, "NvVars");
    // Kernel: largest root file that is not the EFI loader and not NvVars.
    let kernel = files
        .iter()
        .filter(|f| {
            !f.path.contains('/') && !f.path.eq_ignore_ascii_case("NvVars")
        })
        .max_by_key(|f| f.data.len())
        .ok_or_else(|| "ESP has no kernel file in the root".to_string())?;

    let mut bundle: Vec<(&str, &[u8])> = vec![
        ("EFI/BOOT/BOOTX64.EFI", &efi.data),
        (&kernel.path, &kernel.data),
    ];
    if let Some(nv) = nvvars {
        // Avoid double-adding if NvVars *is* the kernel pick (can't be: filtered).
        bundle.push((&nv.path, &nv.data));
    }

    // Short names: BOOTX64.EFI fits; kernel/nvvars get mangled 8.3 names with
    // LFN entries preserving the exact original names.
    const SPARE_CLUSTERS: u32 = 2; // EFI + BOOT dirs
    let spc: u32 = 4;
    let cluster_bytes = (spc as usize) * 512;
    let mut data_clusters: u32 = SPARE_CLUSTERS;
    for (_, d) in &bundle {
        // Root-level files only need clusters here (BOOTX64.EFI lives in BOOT).
        data_clusters += d.len().div_ceil(cluster_bytes) as u32;
    }
    let fatsz = ((data_clusters + 2) * 2).div_ceil(512).max(1);
    let root_sectors: u32 = 32; // 512 entries
    let reserved: u32 = 4;
    let total_sectors = reserved + 2 * fatsz + root_sectors + data_clusters * spc;
    if total_sectors > 0xFFFF {
        return Err("ESP contents too large for FAT16 boot image".to_string());
    }

    let img_len = total_sectors as usize * 512;
    let mut img = vec![0u8; img_len];

    // --- Boot sector ---
    img[0] = 0xEB;
    img[1] = 0x3C;
    img[2] = 0x90;
    img[3..11].copy_from_slice(b"MSWIN4.1");
    w16(&mut img, 11, 512);
    img[13] = spc as u8;
    w16(&mut img, 14, reserved as u16);
    img[16] = 2;
    w16(&mut img, 17, 512);
    w16(&mut img, 19, total_sectors as u16);
    img[21] = 0xF8;
    w16(&mut img, 22, fatsz as u16);
    w16(&mut img, 24, 32);
    w16(&mut img, 26, 64);
    w32(&mut img, 28, 0); // hidden
    w32(&mut img, 32, 0); // tot32
    img[36] = 0x80;
    img[38] = 0x29;
    w32(&mut img, 39, 0x4D464B31);
    img[43..54].copy_from_slice(b"MFK_UEFI   ");
    img[54..62].copy_from_slice(b"FAT16   ");
    img[510] = 0x55;
    img[511] = 0xAA;

    // --- FATs ---
    let fat_off = reserved as usize * 512;
    let fat_len = fatsz as usize * 512;
    let mut fat = vec![0u8; fat_len];
    w16(&mut fat, 0, 0xFFF8);
    w16(&mut fat, 2, 0xFFFF);
    // Allocate clusters sequentially starting at 2.
    // Order: EFI dir, BOOT dir, BOOTX64.EFI, kernel, nvvars.
    let mut next: u32 = 2;
    let mut alloc = |n: u32, fat: &mut [u8]| -> u32 {
        let start = next;
        for k in 0..n {
            let cur = start + k;
            let v: u16 = if k + 1 == n { 0xFFFF } else { (cur + 1) as u16 };
            w16(fat, cur as usize * 2, v);
        }
        next = start + n;
        start
    };
    let efi_clus = alloc(1, &mut fat);
    let boot_clus = alloc(1, &mut fat);
    let mut file_clus: Vec<u32> = Vec::new();
    for (_, d) in bundle.iter().filter(|(p, _)| *p != "EFI/BOOT/BOOTX64.EFI") {
        let n = d.len().div_ceil(cluster_bytes).max(1) as u32;
        file_clus.push(alloc(n, &mut fat));
    }
    // BOOTX64.EFI clusters (allocated last so root files keep low numbers;
    // order is irrelevant, this just keeps it simple).
    let loader_n = efi.data.len().div_ceil(cluster_bytes).max(1) as u32;
    let loader_clus = alloc(loader_n, &mut fat);
    img[fat_off..fat_off + fat_len].copy_from_slice(&fat);
    img[fat_off + fat_len..fat_off + 2 * fat_len].copy_from_slice(&fat);

    // --- Data area ---
    let data_off = (reserved + 2 * fatsz + root_sectors) as usize * 512;
    let write_chain = |start: u32, data: &[u8], img: &mut [u8]| {
        let mut c = start;
        for chunk in data.chunks(cluster_bytes) {
            let off = data_off + (c as usize - 2) * cluster_bytes;
            img[off..off + chunk.len()].copy_from_slice(chunk);
            let nxt = u16le(&fat, c as usize * 2) as u32;
            if nxt >= 0xFFF8 {
                break;
            }
            c = nxt;
        }
    };

    // Directory buffers (filled below, then written as chains).
    let mut efi_dir = vec![0u8; cluster_bytes];
    let mut boot_dir = vec![0u8; cluster_bytes];

    // BOOT dir: `.`, `..`, BOOTX64.EFI.
    boot_dir[0..32].copy_from_slice(&short_entry(&split_83("."), 0x10, boot_clus, 0));
    let mut dd = [0u8; 32];
    dd[0] = b'.';
    dd[1] = b'.';
    dd[2..11].fill(b' ');
    dd[11] = 0x10;
    w16(&mut dd, 26, efi_clus as u16);
    boot_dir[32..64].copy_from_slice(&dd);
    let loader_short = split_83("BOOTX64.EFI");
    // No LFN: short name already matches (case-insensitively).
    let bent = short_entry(
        &loader_short,
        0x20,
        loader_clus,
        efi.data.len() as u32,
    );
    boot_dir[64..64 + bent.len()].copy_from_slice(&bent);
    write_chain(boot_clus, &boot_dir, &mut img);

    // EFI dir: `.`, `..`, BOOT.
    efi_dir[0..32].copy_from_slice(&short_entry(&split_83("."), 0x10, efi_clus, 0));
    let mut dd2 = [0u8; 32];
    dd2[0] = b'.';
    dd2[1] = b'.';
    dd2[2..11].fill(b' ');
    dd2[11] = 0x10;
    w16(&mut dd2, 26, 0); // root parent
    efi_dir[32..64].copy_from_slice(&dd2);
    efi_dir[64..96].copy_from_slice(&short_entry(&split_83("BOOT"), 0x10, boot_clus, 0));
    write_chain(efi_clus, &efi_dir, &mut img);

    // Loader file data.
    write_chain(loader_clus, &efi.data, &mut img);

    // --- Root directory ---
    let root_off = (reserved + 2 * fatsz) as usize * 512;
    let mut root = vec![0u8; root_sectors as usize * 512];
    let mut roff = 0usize;
    // Volume label.
    {
        let mut lab = [0u8; 32];
        lab[0..11].copy_from_slice(b"MFK_UEFI   ");
        lab[11] = 0x08;
        root[roff..roff + 32].copy_from_slice(&lab);
        roff += 32;
    }
    // EFI dir entry.
    root[roff..roff + 32].copy_from_slice(&short_entry(&split_83("EFI"), 0x10, efi_clus, 0));
    roff += 32;
    // Root files: kernel + nvvars (in bundle order, skipping the loader).
    for (ci, (p, d)) in bundle
        .iter()
        .filter(|(p, _)| *p != "EFI/BOOT/BOOTX64.EFI")
        .enumerate()
    {
        let short: [u8; 11] = if p.eq_ignore_ascii_case("NvVars") {
            split_83("NVVARS")
        } else {
            // kernel: 8.3 mangled name; exact name preserved via LFN.
            let mut s = [b' '; 11];
            s[..8].copy_from_slice(b"KERNEL~1");
            s
        };
        let sum = short_checksum(&short);
        // LFN preserves the exact original name/case whenever it is not
        // byte-identical to the 8.3 short form.
        let short_str = String::from_utf8_lossy(&short).trim_end().to_string();
        if p != &short_str {
            let lfn = lfn_entries(p, sum);
            root[roff..roff + lfn.len()].copy_from_slice(&lfn);
            roff += lfn.len();
        }
        root[roff..roff + 32].copy_from_slice(&short_entry(
            &short,
            0x20,
            file_clus[ci],
            d.len() as u32,
        ));
        roff += 32;
        write_chain(file_clus[ci], d, &mut img);
    }
    img[root_off..root_off + root.len()].copy_from_slice(&root);

    Ok(img)
}

// ---------------------------------------------------------------------------
// ISO9660 + El Torito writer
// ---------------------------------------------------------------------------

const SECTOR: usize = 2048;

/// Build one directory record. `name` is the raw identifier bytes
/// (for `.` = [0], `..` = [1], files include `;1`).
fn dir_record(extent: u32, size: u32, flags: u8, name: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 33 + name.len()];
    r[0] = r.len() as u8;
    if r.len() % 2 == 1 {
        r.push(0);
        r[0] += 1;
    }
    r[1] = 0; // xa
    both32(&mut r, 2, extent);
    both32(&mut r, 10, size);
    // Fixed date 2025-01-01 12:00:00 +0000.
    r[18..25].copy_from_slice(&[125u8, 1, 1, 12, 0, 0, 0]);
    r[25] = flags;
    r[26] = 0;
    r[27] = 0;
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

struct IsoFile {
    /// 8.3 ISO name with `;1` version.
    iso_name: Vec<u8>,
    data: Vec<u8>,
}

fn build_iso(efiboot: &[u8], files: &[EspFile]) -> Result<Vec<u8>, String> {
    let efi = find_file(files, "EFI/BOOT/BOOTX64.EFI")
        .ok_or_else(|| "ESP is missing EFI/BOOT/BOOTX64.EFI".to_string())?;
    let nvvars = find_file(files, "NvVars");
    let kernel = files
        .iter()
        .filter(|f| !f.path.contains('/') && !f.path.eq_ignore_ascii_case("NvVars"))
        .max_by_key(|f| f.data.len())
        .ok_or_else(|| "ESP has no kernel file in the root".to_string())?;

    // ISO tree (8.3 informational copies; boot uses the El Torito image).
    let iso_files = vec![IsoFile {
        iso_name: b"BOOTX64.EFI;1".to_vec(),
        data: efi.data.clone(),
    }];
    // kernel + nvvars live in the root; BOOTX64.EFI in /EFI/BOOT.
    let root_files: Vec<IsoFile> = {
        let mut v = vec![IsoFile {
            iso_name: b"KERNEL.BIN;1".to_vec(),
            data: kernel.data.clone(),
        }];
        if let Some(nv) = nvvars {
            v.push(IsoFile {
                iso_name: b"NVVARS;1".to_vec(),
                data: nv.data.clone(),
            });
        }
        v
    };

    let boot_sectors = efiboot.len().div_ceil(SECTOR) as u32;
    // Sector layout.
    let lba_catalog: u32 = 19;
    let lba_boot: u32 = 20;
    let lba_pt_le = lba_boot + boot_sectors;
    let lba_pt_be = lba_pt_le + 1;
    let lba_root = lba_pt_be + 1;
    let lba_efi = lba_root + 1;
    let lba_bootdir = lba_efi + 1;
    let mut next_extent = lba_bootdir + 1;
    let mut data_extents: Vec<u32> = Vec::new();
    for f in root_files.iter().chain(iso_files.iter()) {
        data_extents.push(next_extent);
        let n = f.data.len().div_ceil(SECTOR) as u32;
        next_extent += n;
    }
    let total_sectors = next_extent;
    let pt_size: u32 = 40; // 3 records: root(10) + EFI(12) + BOOT(18)

    // --- Directory buffers ---
    // Root: `.`, `..`, EFI dir, KERNEL.BIN;1, [NVVARS;1].
    let mut root_dir = Vec::new();
    root_dir.extend_from_slice(&dir_record(lba_root, SECTOR as u32, 2, &[0]));
    root_dir.extend_from_slice(&dir_record(lba_root, SECTOR as u32, 2, &[1]));
    root_dir.extend_from_slice(&dir_record(lba_efi, SECTOR as u32, 2, b"EFI"));
    // root file extents in order.
    for (ei, f) in root_files.iter().enumerate() {
        root_dir.extend_from_slice(&dir_record(
            data_extents[ei],
            f.data.len() as u32,
            0,
            &f.iso_name,
        ));
    }
    // EFI dir: `.`, `..`, BOOT.
    let mut efi_dir = Vec::new();
    efi_dir.extend_from_slice(&dir_record(lba_efi, SECTOR as u32, 2, &[0]));
    efi_dir.extend_from_slice(&dir_record(lba_root, SECTOR as u32, 2, &[1]));
    efi_dir.extend_from_slice(&dir_record(lba_bootdir, SECTOR as u32, 2, b"BOOT"));
    // BOOT dir: `.`, `..`, BOOTX64.EFI;1.
    let mut boot_dir = Vec::new();
    boot_dir.extend_from_slice(&dir_record(lba_bootdir, SECTOR as u32, 2, &[0]));
    boot_dir.extend_from_slice(&dir_record(lba_efi, SECTOR as u32, 2, &[1]));
    boot_dir.extend_from_slice(&dir_record(
        data_extents[root_files.len()],
        iso_files[0].data.len() as u32,
        0,
        &iso_files[0].iso_name,
    ));

    // --- Path tables (LE + BE), one sector each ---
    fn pt_entry(name: &[u8], extent: u32, parent: u16, be: bool) -> Vec<u8> {
        let mut r = vec![0u8; 8 + name.len()];
        r[0] = name.len() as u8;
        r[1] = 0;
        if be {
            r[2..6].copy_from_slice(&extent.to_be_bytes());
            r[6..8].copy_from_slice(&parent.to_be_bytes());
        } else {
            r[2..6].copy_from_slice(&extent.to_le_bytes());
            r[6..8].copy_from_slice(&parent.to_le_bytes());
        }
        r[8..8 + name.len()].copy_from_slice(name);
        if name.len() % 2 == 1 {
            r.push(0);
        }
        r
    }
    let mut pt_le = Vec::new();
    pt_le.extend_from_slice(&pt_entry(&[0], lba_root, 1, false));
    pt_le.extend_from_slice(&pt_entry(b"EFI", lba_efi, 1, false));
    pt_le.extend_from_slice(&pt_entry(b"BOOT", lba_bootdir, 2, false));
    assert!(pt_le.len() as u32 <= pt_size);
    let mut pt_be = Vec::new();
    pt_be.extend_from_slice(&pt_entry(&[0], lba_root, 1, true));
    pt_be.extend_from_slice(&pt_entry(b"EFI", lba_efi, 1, true));
    pt_be.extend_from_slice(&pt_entry(b"BOOT", lba_bootdir, 2, true));

    // --- Boot catalog (1 sector) ---
    let mut catalog = vec![0u8; SECTOR];
    // Validation entry.
    catalog[0] = 0x01; // header id
    catalog[1] = 0xEF; // platform: EFI
    catalog[2..4].copy_from_slice(&[0, 0]);
    catalog[4..10].copy_from_slice(b"MFK   ");
    // Checksum over the 16 words of this 32-byte record.
    {
        let mut sum: u32 = 0;
        for k in 0..15 {
            sum += u16le(&catalog, k * 2) as u32;
        }
        sum += 0xAA55;
        w16(&mut catalog, 28, (0u32.wrapping_sub(sum)) as u16);
    }
    catalog[30] = 0x55;
    catalog[31] = 0xAA;
    // Initial/default entry: EFI, no emulation.
    catalog[32] = 0x88; // bootable
    catalog[33] = 0x00; // no emulation
    w16(&mut catalog, 34, 0); // load segment
    catalog[36] = 0x00; // system type
    catalog[37] = 0x00;
    let boot_512 = efiboot.len().div_ceil(512) as u32;
    if boot_512 > 0xFFFF {
        return Err("Boot image too large for El Torito".to_string());
    }
    w16(&mut catalog, 38, boot_512 as u16); // sector count (512B units)
    w32(&mut catalog, 40, lba_boot); // boot image LBA

    // --- Assemble ---
    let mut iso = vec![0u8; total_sectors as usize * SECTOR];

    // PVD (sector 16).
    {
        let p = &mut iso[16 * SECTOR..17 * SECTOR];
        p[0] = 1;
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        p[8..40].copy_from_slice(b"MFK                             ");
        p[40..72].copy_from_slice(b"MFK_UEFI                        ");
        both32(p, 80, total_sectors);
        p[88..120].copy_from_slice(&[0; 32]);
        both16(p, 120, 1); // vol set size
        both16(p, 124, 1); // seq number
        both16(p, 128, SECTOR as u16); // block size
        both32(p, 132, pt_size);
        w32(p, 140, lba_pt_le);
        w32(p, 144, 0);
        // BE path table location at 148.
        p[148..152].copy_from_slice(&lba_pt_be.to_be_bytes());
        p[152..156].copy_from_slice(&0u32.to_be_bytes());
        // Root dir record at 156 (34 bytes -> 156..190).
        let root_rec = dir_record(lba_root, SECTOR as u32, 2, &[0]);
        p[156..156 + root_rec.len()].copy_from_slice(&root_rec);
        // Volume set identifier (190..318, 128 bytes).
        p[190..198].copy_from_slice(b"MFK_UEFI");
        // Publisher identifier (318..446, 128 bytes).
        p[318..321].copy_from_slice(b"MFK");
        // Data preparer identifier (446..574, 128 bytes).
        p[446..449].copy_from_slice(b"MFK");
        p[813..830].copy_from_slice(b"2025010100000000\x00");
        p[830..847].copy_from_slice(b"2025010100000000\x00");
        p[847..864].copy_from_slice(b"0000000000000000\x00");
        p[864..881].copy_from_slice(b"0000000000000000\x00");
        p[881] = 1;
    }
    // Boot Record (sector 17).
    {
        let b = &mut iso[17 * SECTOR..18 * SECTOR];
        b[0] = 0;
        b[1..6].copy_from_slice(b"CD001");
        b[6] = 1;
        b[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
        w32(b, 71, lba_catalog);
    }
    // Terminator (sector 18).
    {
        let t = &mut iso[18 * SECTOR..19 * SECTOR];
        t[0] = 255;
        t[1..6].copy_from_slice(b"CD001");
        t[6] = 1;
    }
    iso[lba_catalog as usize * SECTOR..(lba_catalog as usize + 1) * SECTOR]
        .copy_from_slice(&catalog);
    // Boot image.
    {
        let off = lba_boot as usize * SECTOR;
        iso[off..off + efiboot.len()].copy_from_slice(efiboot);
    }
    // Path tables.
    {
        let off = lba_pt_le as usize * SECTOR;
        iso[off..off + pt_le.len()].copy_from_slice(&pt_le);
        let off = lba_pt_be as usize * SECTOR;
        iso[off..off + pt_be.len()].copy_from_slice(&pt_be);
    }
    // Directories.
    {
        let off = lba_root as usize * SECTOR;
        iso[off..off + root_dir.len()].copy_from_slice(&root_dir);
        let off = lba_efi as usize * SECTOR;
        iso[off..off + efi_dir.len()].copy_from_slice(&efi_dir);
        let off = lba_bootdir as usize * SECTOR;
        iso[off..off + boot_dir.len()].copy_from_slice(&boot_dir);
    }
    // File data.
    for (ei, f) in root_files.iter().chain(iso_files.iter()).enumerate() {
        let off = data_extents[ei] as usize * SECTOR;
        iso[off..off + f.data.len()].copy_from_slice(&f.data);
    }

    Ok(iso)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Create `<kernel>-uefi.iso` from `<kernel>-uefi.img`.
pub fn create_uefi_iso(uefi_img: &Path, iso_out: &Path) -> Result<(), String> {
    let files = extract_esp_files(uefi_img)?;
    let mut names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    names.sort();
    println!("  ESP files: {}", names.join(", "));
    let efiboot = build_efiboot_img(&files)?;
    println!(
        "  El Torito boot image: {} bytes FAT16",
        efiboot.len()
    );
    let iso = build_iso(&efiboot, &files)?;
    std::fs::write(iso_out, &iso)
        .map_err(|e| format!("Cannot write {}: {}", iso_out.display(), e))?;
    Ok(())
}
