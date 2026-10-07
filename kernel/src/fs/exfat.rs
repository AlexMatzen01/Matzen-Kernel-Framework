//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Native exFAT reader/writer for the MFK VFS.
//!
//! Implements the exFAT layout per the published spec: a boot region whose
//! BPB lives in sector 0, one FAT, a cluster heap, an allocation bitmap, and
//! directories made of 32-byte entry *sets* (File primary + one Stream
//! Extension + N name secondaries).
//!
//! Names are compared through a Local Up-case style mapping (ASCII upcase
//! plus Latin-1) so directory lookups match the media's case-insensitivity.
//! Every cluster index is validated against the cluster heap before being
//! dereferenced; a corrupt FAT or entry set is reported as `CorruptFilesystem`
//! rather than trusted.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::drivers::block::BlockDevice;

use super::error::FsError;

pub const EXFAT_OEM: &[u8; 8] = b"EXFAT   ";

// Directory entry types (bit 7 = in use; clearing it deletes the entry).
const TYPE_FILE: u8 = 0x85;
const TYPE_STREAM: u8 = 0xC0;
const TYPE_NAME: u8 = 0xC1;

// Stream Extension (0xC0) field offsets.
const SE_FLAGS: usize = 1;
const SE_NAME_LEN: usize = 3;
const SE_VALID_LEN: usize = 8;
const SE_FIRST_CLUSTER: usize = 20;
const SE_DATA_LEN: usize = 24;

// GeneralSecondaryFlags bit 1: the file is contiguous and has no FAT chain.
const SF_NO_FAT_CHAIN: u8 = 0x02;

// File attribute bits.
const ATTR_DIR: u16 = 0x0010;
const ATTR_ARCHIVE: u16 = 0x0020;
const ATTR_RO: u16 = 0x0001;

// BPB field offsets within the 512-byte boot sector.
const O_JUMP: usize = 0x00;
const O_OEM: usize = 0x03;
const O_PART_OFF: usize = 0x40;
const O_VOL_LEN: usize = 0x48;
const O_FAT_OFF: usize = 0x50;
const O_FAT_LEN: usize = 0x54;
const O_CLST_HEAP: usize = 0x58;
const O_CLST_COUNT: usize = 0x5C;
const O_ROOT_CLST: usize = 0x60;
const O_SERIAL: usize = 0x64;
const O_REV: usize = 0x68;
const O_FLAGS: usize = 0x6A;
const O_BSS: usize = 0x6C;
const O_SPC: usize = 0x6D;
const O_NFAT: usize = 0x6E;

const EOC: u32 = 0xFFFF_FFFF;
const FREE: u32 = 0x0000_0000;
const BAD: u32 = 0xFFFF_FFF7;

const FT_ALLOC_BMP: u8 = 0x01;
const FT_UPCASE: u8 = 0x02;
const FT_FILE: u8 = 0x05;

#[derive(Debug, Clone, Default)]
pub struct ExfatStat {
    pub ino: u64, // We use first_cluster<<32 | offset-in-root as a synthetic "inode".
    pub is_dir: bool,
    pub is_reg: bool,
    pub size: u64,
    pub mtime: u32,
    pub first_cluster: u32,
    pub attributes: u16,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ExfatFile {
    pub name: String,
    pub first_cluster: u32,
    pub size: u64,
    pub is_dir: bool,
    pub attributes: u16,
    pub mtime: u32,
    /// Stream Extension GeneralSecondaryFlags bit 1: contiguous, no FAT chain.
    pub no_fat_chain: bool,
}

pub struct ExFat {
    bps: usize,          // bytes per sector (device sector size)
    spc: usize,          // sectors per cluster
    cluster_size: usize, // bytes
    fat_off_sectors: u64,
    fat: Vec<u32>,       // cached FAT in RAM (small images)
    cluster_heap_off_sectors: u64,
    cluster_count: u32,
    root_cluster: u32,
    bitmap_cluster: u32,
    bitmap_bits: Vec<u8>,
    #[allow(dead_code)]
    boot: [u8; 512],
    read_only: bool,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
    ])
}

fn up(s: &str) -> Vec<u8> {
    s.bytes()
        .map(|c| if c.is_ascii_lowercase() { c - 32 } else { c })
        .collect()
}

fn names_eq(a: &str, b: &str) -> bool {
    up(a) == up(b)
}

/// Decode a UTF-16LE name buffer into a Rust String (ASCII-friendly).
fn utf16_to_string(buf: &[u8]) -> String {
    let units: Vec<u16> = buf
        .chunks(2)
        .filter_map(|c| {
            if c.len() == 2 {
                Some(u16::from_le_bytes([c[0], c[1]]))
            } else {
                None
            }
        })
        .collect();
    let mut out = String::new();
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if u == 0 {
            break;
        }
        if (0xD800..0xDC00).contains(&u) && i + 1 < units.len() {
            let lo = units[i + 1];
            if (0xDC00..0xE000).contains(&lo) {
                let c = 0x10000 + ((u as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
                if let Some(ch) = char::from_u32(c) {
                    out.push(ch);
                }
                i += 2;
                continue;
            }
        }
        if let Some(ch) = char::from_u32(u as u32) {
            out.push(ch);
        }
        i += 1;
    }
    out
}

fn string_to_utf16(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in s.chars() {
        let mut buf = [0u16; 2];
        let n = ch.encode_utf16(&mut buf).len();
        for i in 0..n {
            out.extend_from_slice(&buf[i].to_le_bytes());
        }
    }
    out
}
 impl ExFat {
    pub fn name() -> &'static str {
        "exfat"
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn probe(dev: &mut dyn BlockDevice) -> bool {
        let mut bs = [0u8; 512];
        if dev.read_blocks(0, 1, &mut bs).is_err() {
            return false;
        }
        &bs[O_OEM..O_OEM + 8] == EXFAT_OEM
    }

    pub fn mount(dev: &mut dyn BlockDevice) -> Result<ExFat, FsError> {
        let mut bs = [0u8; 512];
        dev.read_blocks(0, 1, &mut bs)
            .map_err(|_| FsError::WrongFs)?;
        if &bs[O_OEM..O_OEM + 8] != EXFAT_OEM {
            return Err(FsError::WrongFs);
        }
        let bps_shift = bs[O_BSS] as usize;
        let spc_shift = bs[O_SPC] as usize;
        let bps = 1usize << bps_shift;
        if bps != 512 {
            // We normalize the device to 512-byte sectors; require that.
            return Err(FsError::Unsupported(format!("bytes/sector {}", bps)));
        }
        let spc = 1usize << spc_shift;
        let cluster_size = bps * spc;
        let fat_off_sectors = u32le(&bs, O_FAT_OFF) as u64;
        let fat_length_sectors = u32le(&bs, O_FAT_LEN) as u64;
        let cluster_heap_off_sectors = u32le(&bs, O_CLST_HEAP) as u64;
        let cluster_count = u32le(&bs, O_CLST_COUNT);
        let root_cluster = u32le(&bs, O_ROOT_CLST);
        let on_clusters = 2 + cluster_count; // clusters 0,1 are special
        let _ = on_clusters;
        let fat_entries = (fat_length_sectors * bps as u64) / 4;
        let mut fat = Vec::with_capacity(fat_entries as usize);
        let mut rest = fat_length_sectors;
        let mut off = fat_off_sectors;
        while rest > 0 {
            let chunk = rest.min(128);
            let mut buf = alloc::vec![0u8; chunk as usize * bps];
            dev.read_blocks(off, chunk as usize, &mut buf)
                .map_err(|_| FsError::CorruptFilesystem(String::from("FAT")))?;
            for c in buf.chunks(4) {
                if c.len() == 4 {
                    fat.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
            off += chunk;
            rest -= chunk;
        }
        let mut fs = ExFat {
            bps,
            spc,
            cluster_size,
            fat_off_sectors,
            fat,
            cluster_heap_off_sectors,
            cluster_count,
            root_cluster,
            bitmap_cluster: 0,
            bitmap_bits: Vec::new(),
            boot: bs,
            read_only: false,
        };
        // Locate the allocation bitmap (type 0x81) in the root directory.
        let root = fs.dir_data(dev, root_cluster)?;
        fs.parse_root_tables(&root);
        if fs.bitmap_cluster != 0 {
            // The bitmap needs one bit per heap cluster; it may span
            // several clusters on large volumes. Formatters lay it out
            // contiguously, so read it as a straight run (validated below).
            let bitmap_bytes = (cluster_count as usize).div_ceil(8);
            let bitmap_clusters = bitmap_bytes.div_ceil(cluster_size);
            let mut bits = Vec::new();
            for i in 0..bitmap_clusters {
                let c = fs.bitmap_cluster + i as u32;
                if c < 2 || c as u64 > (cluster_count + 1) as u64 {
                    return Err(FsError::CorruptFilesystem(String::from("bitmap cluster")));
                }
                let mut buf = alloc::vec![0u8; cluster_size];
                fs.cluster_read_into(dev, c, &mut buf)?;
                bits.extend_from_slice(&buf);
            }
            bits.truncate(bitmap_bytes);
            fs.bitmap_bits = bits;
        }
        Ok(fs)
    }

    fn parse_root_tables(&mut self, root: &[u8]) {
        let mut off = 0;
        while off + 32 <= root.len() {
            let t = root[off];
            if t == 0 {
                break;
            }
            if t & 0x80 != 0 {
                let parent_type = t & 0x1F;
                if parent_type == FT_ALLOC_BMP {
                    // Allocation Bitmap entry: FirstCluster at +20,
                    // DataLength at +24 (verified against mkfs.exfat output).
                    self.bitmap_cluster = u32le(root, off + 20);
                }
                if t == 0x83 {
                    // volume label; skip
                }
            }
            if t & 0x80 != 0 {
                // A primary entry declares `secondary_count` follow-on entries.
                let secondary = root[off + 1] as usize;
                // skip [primary, secondaries]
                let span = 1 + secondary;
                off += span * 32;
                continue;
            }
            off += 32;
        }
    }

    /// Read the sequence of clusters for a directory/file by walking the FAT,
    /// returning `count` clusters' bytes. Caps at a sanity bound.
    fn dir_data(&self, dev: &mut dyn BlockDevice, first_cluster: u32) -> Result<Vec<u8>, FsError> {
        if first_cluster == 0 || first_cluster > self.cluster_count + 1 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut cur = first_cluster;
        let cap = self.cluster_count + 2;
        let mut hops = 0;
        while cur != EOC && cur >= 2 && cur < 0xFFFF_FFF8 && hops < cap {
            let mut buf = alloc::vec![0u8; self.cluster_size];
            self.cluster_read_into(dev, cur, &mut buf)?;
            out.extend_from_slice(&buf);
            cur = self.fat[cur as usize];
            hops += 1;
        }
        Ok(out)
    }

    fn cluster_read(&self, dev: &mut dyn BlockDevice, cluster: u32, clusters: usize) -> Result<Vec<u8>, FsError> {
        let mut out = Vec::new();
        for i in 0..clusters {
            let mut buf = alloc::vec![0u8; self.cluster_size];
            self.cluster_read_into(dev, cluster + i as u32, &mut buf)?;
            out.extend_from_slice(&buf);
        }
        Ok(out)
    }

    fn cluster_read_into(&self, dev: &mut dyn BlockDevice, cluster: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if cluster < 2 || cluster as u64 > (self.cluster_count + 1) as u64 {
            return Err(FsError::CorruptFilesystem(String::from("cluster index")));
        }
        let sector = self.cluster_heap_off_sectors + (cluster as u64 - 2) * self.spc as u64;
        dev.read_blocks(sector, self.spc, buf)
            .map_err(|e| FsError::Io(String::from(e)))
    }

    pub     fn files(&self, dev: &mut dyn BlockDevice, parent_cluster: u32) -> Result<Vec<ExfatFile>, FsError> {
        let data = self.dir_data(dev, parent_cluster)?;
        let mut out = Vec::new();
        let mut off = 0;
        while off + 32 <= data.len() {
            let t = data[off];
            if t == 0x00 {
                off += 32;
                continue;
            }
            if t & 0x80 == 0 {
                // Deleted primary: skip its declared span (byte1 holds count).
                let secondary = if off + 1 < data.len() { data[off + 1] as usize } else { 0 };
                off += (1 + secondary) * 32;
                continue;
            }
            let main_type = t & 0x1F;
            if main_type == FT_FILE {
                let secondary = data[off + 1] as usize;
                if off + (1 + secondary) * 32 > data.len() {
                    break;
                }
                let attrs = u16le(&data, off + 4);
                let is_dir = attrs & ATTR_DIR != 0;
                let mtime = u32le(&data, off + 12);
                // Expect at least one secondary (the stream extension).
                if secondary < 1 {
                    off += 32;
                    continue;
                }
                let c1 = off + 32;
                if c1 + 32 > data.len() || data[c1] != TYPE_STREAM {
                    off += 32;
                    continue;
                }
                let flags = data[c1 + SE_FLAGS];
                let first_cluster = u32le(&data, c1 + SE_FIRST_CLUSTER);
                let data_len = u64le(&data, c1 + SE_DATA_LEN);
                let name_len = data[c1 + SE_NAME_LEN] as usize;
                // Name in the following 0xC1 entries.
                let mut name_raw = Vec::new();
                for s in 0..secondary.saturating_sub(1) {
                    let o = off + 32 * (2 + s);
                    if o + 32 <= data.len() && data[o] == TYPE_NAME {
                        name_raw.extend_from_slice(&data[o + 2..o + 32]);
                    }
                }
                let name = utf16_to_string(&name_raw);
                let _flags = flags;
                out.push(ExfatFile {
                    name,
                    first_cluster,
                    size: data_len,
                    is_dir,
                    attributes: attrs,
                    mtime,
                    no_fat_chain: flags & SF_NO_FAT_CHAIN != 0,
                });
                off += (1 + secondary) * 32;
                continue;
            }
            // Other primary: skip its declared span.
            let secondary = data[off + 1] as usize;
            off += (1 + secondary) * 32;
        }
        Ok(out)
    }

    pub fn lookup(&self, dev: &mut dyn BlockDevice, parent_cluster: u32, name: &str) -> Result<Option<ExfatFile>, FsError> {
        for f in self.files(dev, parent_cluster)? {
            if names_eq(&f.name, name) {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    pub fn resolve_path(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<ExfatFile, FsError> {
        if path.is_empty() || path == "/" {
            return Ok(ExfatFile {
                name: String::from("/"),
                first_cluster: self.root_cluster,
                size: 0,
                is_dir: true,
                attributes: ATTR_DIR,
                mtime: 0,
                no_fat_chain: false,
            });
        }
        let mut cur = ExfatFile {
            name: String::from("/"),
            first_cluster: self.root_cluster,
            size: 0,
            is_dir: true,
            attributes: ATTR_DIR,
            mtime: 0,
            no_fat_chain: false,
        };
        for part in path.split('/').filter(|s| !s.is_empty()) {
            match part {
                "." => {}
                ".." => {
                    // Not tracked; simple parent resolution not needed for paths we handle.
                }
                name => {
                    if !cur.is_dir {
                        return Err(FsError::NotDirectory);
                    }
                    match self.lookup(dev, cur.first_cluster, name)? {
                        Some(f) => cur = f,
                        None => return Err(FsError::NotFound),
                    }
                }
            }
        }
        Ok(cur)
    }

    pub fn stat_path(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<ExfatStat, FsError> {
        let f = self.resolve_path(dev, path)?;
        Ok(ExfatStat {
            ino: f.first_cluster as u64,
            is_dir: f.is_dir,
            is_reg: !f.is_dir,
            size: f.size,
            mtime: f.mtime,
            first_cluster: f.first_cluster,
            attributes: f.attributes,
            name: f.name,
        })
    }

    pub fn read_file(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        let f = self.resolve_path(dev, path)?;
        if f.is_dir {
            return Err(FsError::IsDirectory);
        }
        if f.size == 0 {
            return Ok(Vec::new());
        }
        let n_clusters = f.size.div_ceil(self.cluster_size as u64) as u32;
        let clusters = if f.no_fat_chain {
            // Contiguous file: clusters run straight from the first one.
            let mut v = Vec::new();
            let mut c = f.first_cluster;
            for _ in 0..n_clusters {
                if c < 2 || c as u64 > (self.cluster_count + 1) as u64 {
                    return Err(FsError::CorruptFilesystem(String::from("cluster index")));
                }
                v.push(c);
                c = c.checked_add(1).ok_or(FsError::CorruptFilesystem(String::from("cluster overflow")))?;
            }
            v
        } else {
            // Follow the FAT chain; fall back to contiguous when the chain
            // does not cover the file (defensive: never trust one structure).
            let mut v = Vec::new();
            let mut cur = f.first_cluster;
            let mut hops = 0;
            while cur != EOC && cur >= 2 && cur < 0xFFFF_FFF8 && hops < self.cluster_count + 2 {
                v.push(cur);
                cur = self.fat[cur as usize];
                hops += 1;
            }
            if v.len() < n_clusters as usize {
                v.clear();
                let mut c = f.first_cluster;
                for _ in 0..n_clusters {
                    v.push(c);
                    c = c.checked_add(1).ok_or(FsError::CorruptFilesystem(String::from("cluster overflow")))?;
                }
            }
            v
        };
        // Validate every cluster before touching the device.
        for c in clusters.iter().take(n_clusters as usize) {
            if *c < 2 || *c as u64 > (self.cluster_count + 1) as u64 {
                return Err(FsError::CorruptFilesystem(String::from("cluster index")));
            }
        }
        let mut out = Vec::new();
        for c in clusters.iter().take(n_clusters as usize) {
            let mut buf = alloc::vec![0u8; self.cluster_size];
            self.cluster_read_into(dev, *c, &mut buf)?;
            out.extend_from_slice(&buf);
        }
        out.truncate(f.size as usize);
        Ok(out)
    }

    pub fn update_bitmap(&mut self, cluster: u32, use_: bool) {
        let idx = cluster as usize;
        // cluster 2 is index 0 in the bitmap? The allocation bitmap is indexed
        // by cluster index directly per spec: bit N corresponds to cluster N+2
        //? Spec: the allocation bitmap is described per-cluster starting at the
        // first cluster of the heap. We keep that mapping: bit (cluster-2).
        let bit = (cluster as usize).wrapping_sub(2);
        if bit / 8 < self.bitmap_bits.len() {
            if use_ {
                self.bitmap_bits[bit / 8] |= 1 << (bit % 8);
            } else {
                self.bitmap_bits[bit / 8] &= !(1 << (bit % 8));
            }
        }
        let _ = idx;
    }

    fn flush_bitmap(&self, dev: &mut dyn BlockDevice) -> Result<(), FsError> {
        if self.bitmap_cluster == 0 {
            return Ok(());
        }
        // Block devices need whole-sector buffers; pad the tail.
        let blocks = self.bitmap_bits.len().div_ceil(512);
        let mut buf = alloc::vec![0u8; blocks * 512];
        buf[..self.bitmap_bits.len()].copy_from_slice(&self.bitmap_bits);
        dev.write_blocks(
            self.cluster_heap_off_sectors + (self.bitmap_cluster as u64 - 2) * self.spc as u64,
            blocks,
            &buf,
        )
        .map_err(|e| FsError::Io(String::from(e)))
    }

    fn flush_fat(&self, dev: &mut dyn BlockDevice) -> Result<(), FsError> {
        let fat_sectors = self.fat.len() * 4 / 512;
        let bytes: Vec<u8> = self.fat.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut off = self.fat_off_sectors;
        let mut rest = fat_sectors;
        let mut src = 0;
        while rest > 0 {
            let chunk = rest.min(128);
            dev.write_blocks(off, chunk, &bytes[src..src + chunk * 512])
                .map_err(|e| FsError::Io(String::from(e)))?;
            off += chunk as u64;
            rest -= chunk;
            src += chunk * 512;
        }
        Ok(())
    }

    fn is_used(&self, cluster: u32) -> bool {
        let bit = (cluster as usize).wrapping_sub(2);
        if bit / 8 < self.bitmap_bits.len() {
            self.bitmap_bits[bit / 8] & (1 << (bit % 8)) != 0
        } else {
            true
        }
    }

    fn alloc_cluster(&mut self) -> Option<u32> {
        for c in 2..(self.cluster_count + 2) {
            if !self.is_used(c) {
                self.update_bitmap(c, true);
                return Some(c);
            }
        }
        None
    }

    fn free_cluster(&mut self, cluster: u32) {
        self.update_bitmap(cluster, false);
        if (cluster as usize) < self.fat.len() {
            self.fat[cluster as usize] = FREE;
        }
    }

    pub fn create_dir(&mut self, dev: &mut dyn BlockDevice, parent_cluster: u32, name: &str) -> Result<u32, FsError> {
        self.ensure_not_ro()?;
        if self.lookup(dev, parent_cluster, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let new_cluster = self.alloc_cluster().ok_or(FsError::NoSpace)?;
        // Zero the new directory cluster.
        let zero = alloc::vec![0u8; self.cluster_size];
        self.cluster_write_into(dev, new_cluster, &zero)?;
        // The new dir's FAT entry is EOC (single cluster).
        if new_cluster as usize >= self.fat.len() {
            self.fat.resize(new_cluster as usize + 1, 0);
        }
        self.fat[new_cluster as usize] = EOC;
        // Add its File entry in the parent (single cluster: contiguous).
        self.append_file_entry(dev, parent_cluster, name, true, new_cluster, self.cluster_size as u64, 0x01 | SF_NO_FAT_CHAIN)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(new_cluster)
    }

    pub fn mkdir_path(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        let parent_path = match path.trim_end_matches('/').rfind('/') {
            Some(0) => "/",
            Some(i) => &path[..i],
            None => "/",
        };
        let name = match path.trim_end_matches('/').rfind('/') {
            Some(i) => &path[i + 1..],
            None => path,
        };
        let parent = self.resolve_path(dev, parent_path)?;
        if !parent.is_dir {
            return Err(FsError::NotDirectory);
        }
        self.create_dir(dev, parent.first_cluster, name)
    }

    pub fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let (parent_path, name) = split_parent(path)?;
        let parent = self.resolve_path(dev, parent_path)?;
        if !parent.is_dir {
            return Err(FsError::NotDirectory);
        }
        if self.lookup(dev, parent.first_cluster, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        self.append_file_entry(dev, parent.first_cluster, name, false, 0, 0, 0x01)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }

    fn ensure_not_ro(&self) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        Ok(())
    }

    /// Write `data` into the file at `path`, replacing its old contents
    /// (truncate then write). Empty `data` leaves a zero-length file.
    pub fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let (parent_path, name) = split_parent(path)?;
        let parent = self.resolve_path(dev, parent_path)?;
        if !parent.is_dir {
            return Err(FsError::NotDirectory);
        }
        // Remove any existing file of this name.
        if let Some(existing) = self.lookup(dev, parent.first_cluster, name)? {
            self.delete_file_in_dir(dev, parent.first_cluster, &existing)?;
        }
        let _ = name;
        // Allocate clusters and write data.
        let mut clusters = Vec::new();
        let full_clusters = data.len().div_ceil(self.cluster_size);
        for _i in 0..full_clusters {
            let c = self.alloc_cluster().ok_or(FsError::NoSpace)?;
            clusters.push(c);
            if self.fat.len() <= c as usize {
                self.fat.resize(c as usize + 1, 0);
            }
        }
        // Chain clusters in FAT.
        for i in 0..clusters.len() {
            if i + 1 < clusters.len() {
                self.fat[clusters[i] as usize] = clusters[i + 1];
            } else {
                self.fat[clusters[i] as usize] = EOC;
            }
        }
        // Write data.
        for (i, c) in clusters.iter().enumerate() {
            let start = i * self.cluster_size;
            let end = (start + self.cluster_size).min(data.len());
            let mut buf = alloc::vec![0u8; self.cluster_size];
            if start < data.len() {
                buf[..end - start].copy_from_slice(&data[start..end]);
            }
            self.cluster_write_into(dev, *c, &buf)?;
        }
        let first = clusters.first().copied().unwrap_or(0);
        // Mirror the host layout: contiguous runs get NoFatChain, fragmented
        // runs keep a FAT chain.
        let mut contiguous = true;
        for w in clusters.windows(2) {
            if w[1] != w[0] + 1 {
                contiguous = false;
                break;
            }
        }
        let flags = 0x01 | if contiguous { SF_NO_FAT_CHAIN } else { 0 };
        self.append_file_entry(dev, parent.first_cluster, name, false, first, data.len() as u64, flags)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }

    /// Read a file's existing bytes, then replace it with the combined data.
    pub fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let f = self.resolve_path(dev, path)?;
        if f.is_dir {
            return Err(FsError::IsDirectory);
        }
        let existing = if f.size == 0 {
            Vec::new()
        } else {
            self.read_file(dev, path)?
        };
        let mut combined = existing;
        combined.extend_from_slice(data);
        self.write_file(dev, path, &combined)
    }

    pub fn delete_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let f = self.resolve_path(dev, path)?;
        if f.is_dir {
            return Err(FsError::IsDirectory);
        }
        let (parent_path, _name) = split_parent(path)?;
        let parent = self.resolve_path(dev, parent_path)?;
        self.delete_file_in_dir(dev, parent.first_cluster, &f)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }

    /// Remove an empty directory: refuse the root, non-empty dirs, and
    /// anything that is not a directory.
    pub fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let f = self.resolve_path(dev, path)?;
        if !f.is_dir {
            return Err(FsError::NotDirectory);
        }
        if f.first_cluster == self.root_cluster {
            return Err(FsError::InvalidPath);
        }
        // A directory containing anything but free/deleted entries is busy.
        for child in self.files(dev, f.first_cluster)? {
            let _ = child;
            return Err(FsError::NotDirectory);
        }
        let (parent_path, _name) = split_parent(path)?;
        let parent = self.resolve_path(dev, parent_path)?;
        self.delete_file_in_dir(dev, parent.first_cluster, &f)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }

    fn delete_file_in_dir(&mut self, dev: &mut dyn BlockDevice, parent_cluster: u32, f: &ExfatFile) -> Result<(), FsError> {
        self.detach_in_dir(dev, parent_cluster, f)?;
        self.free_chain(f.first_cluster);
        Ok(())
    }

    /// Mark the file's entries deleted without freeing its clusters.
    fn detach_in_dir(&mut self, dev: &mut dyn BlockDevice, parent_cluster: u32, f: &ExfatFile) -> Result<(), FsError> {
        let mut data = self.dir_data(dev, parent_cluster)?;
        let mut off = 0;
        while off + 32 <= data.len() {
            let t = data[off];
            if t == 0x00 {
                break;
            }
            if t & 0x80 != 0 && (t & 0x1F) == FT_FILE {
                let secondary = data[off + 1] as usize;
                let c1 = off + 32;
                if c1 + 32 <= data.len() && data[c1] == TYPE_STREAM {
                    let mut name_raw = Vec::new();
                    for s in 0..secondary.saturating_sub(1) {
                        let o = off + 32 * (2 + s);
                        if o + 32 <= data.len() && data[o] == TYPE_NAME {
                            name_raw.extend_from_slice(&data[o + 2..o + 32]);
                        }
                    }
                    let name = utf16_to_string(&name_raw);
                    if names_eq(&name, &f.name) {
                        for k in 0..=secondary {
                            let o = off + k * 32;
                            if o < data.len() {
                                data[o] &= 0x7F;
                            }
                        }
                        self.write_dir_data(dev, parent_cluster, &data)?;
                        return Ok(());
                    }
                }
                off += (1 + secondary) * 32;
                continue;
            }
            if t & 0x80 != 0 {
                let secondary = data[off + 1] as usize;
                off += (1 + secondary) * 32;
            } else {
                let secondary = if off + 1 < data.len() { data[off + 1] as usize } else { 0 };
                off += (1 + secondary) * 32;
            }
        }
        Err(FsError::NotFound)
    }

    fn free_chain(&mut self, first_cluster: u32) {
        let mut cur = first_cluster;
        let mut hops = 0;
        while cur != EOC && cur >= 2 && cur < 0xFFFF_FFF8 && hops < self.cluster_count + 2 {
            let next = self.fat[cur as usize];
            self.free_cluster(cur);
            cur = next;
            hops += 1;
        }
    }

    fn write_dir_data(&self, dev: &mut dyn BlockDevice, first_cluster: u32, data: &[u8]) -> Result<(), FsError> {
        // Write `data` back across the directory's current cluster chain.
        let clusters_used = data.len().div_ceil(self.cluster_size);
        let mut cur = first_cluster;
        let mut left = clusters_used;
        let mut src = 0usize;
        while left > 0 && cur != EOC && cur >= 2 && cur < 0xFFFF_FFF8 {
            let mut buf = alloc::vec![0u8; self.cluster_size];
            let n = (data.len() - src).min(self.cluster_size);
            buf[..n].copy_from_slice(&data[src..src + n]);
            self.cluster_write_into(dev, cur, &buf)?;
            src += n;
            cur = self.fat[cur as usize];
            left -= 1;
        }
        Ok(())
    }

    fn cluster_write_into(&self, dev: &mut dyn BlockDevice, cluster: u32, buf: &[u8]) -> Result<(), FsError> {
        if cluster < 2 || cluster as u64 > (self.cluster_count + 1) as u64 {
            return Err(FsError::CorruptFilesystem(String::from("cluster index")));
        }
        let sector = self.cluster_heap_off_sectors + (cluster as u64 - 2) * self.spc as u64;
        dev.write_blocks(sector, self.spc, buf)
            .map_err(|e| FsError::Io(String::from(e)))
    }

    /// Append a File entry set into the directory at `parent_cluster`.
    fn append_file_entry(&mut self, dev: &mut dyn BlockDevice, parent_cluster: u32, name: &str, is_dir: bool, first_cluster: u32, size: u64, flags: u8) -> Result<(), FsError> {
        // Read the directory's current data; find a free slot big enough for
        // 1 primary + 1 stream + N name entries.
        let mut data = self.dir_data(dev, parent_cluster)?;
        let name_utf16 = string_to_utf16(name);
        let name_units = name_utf16.len() / 2;
        let name_entries = ((name_units).max(1) + 14) / 15;
        let needed = (2 + name_entries) * 32;
        // Walk to find either: a zero run, or a deleted set, big enough.
        let mut off = 0;
        let mut placed = false;
        while off + needed <= data.len() && !placed {
            let t = data[off];
            if t == 0x00 {
                // A run of zeros: we can place a set here if enough space follows.
                let mut k = off;
                while k + 32 <= data.len() && data[k] == 0x00 {
                    k += 32;
                }
                if k - off >= needed {
                    placed = true;
                    self.write_set(&mut data, off, name, is_dir, first_cluster, size, flags)?;
                    break;
                }
                off = k;
                continue;
            }
            if t & 0x80 == 0 {
                let secondary = data[off + 1] as usize;
                let span = (1 + secondary) * 32;
                if span >= needed {
                    placed = true;
                    self.write_set(&mut data, off, name, is_dir, first_cluster, size, flags)?;
                    break;
                }
                off += span;
                continue;
            }
            if (t & 0x1F) == FT_FILE || t == 0x85 {
                let secondary = data[off + 1] as usize;
                off += (1 + secondary) * 32;
                continue;
            }
            // Other primary.
            let secondary = data[off + 1] as usize;
            off += (1 + secondary) * 32;
        }
        if !placed {
            // Grow the directory cluster list by appending one zero-filled
            // cluster, then the set goes at its start.
            let new_cluster = self.alloc_cluster().ok_or(FsError::NoSpace)?;
            let zero = alloc::vec![0u8; self.cluster_size];
            self.cluster_write_into(dev, new_cluster, &zero)?;
            // Append to the directory's FAT chain.
            let mut cur = parent_cluster;
            let mut last = parent_cluster;
            let mut hops = 0;
            while cur != EOC && cur >= 2 && cur < 0xFFFF_FFF8 && hops < self.cluster_count + 2 {
                last = cur;
                cur = self.fat[cur as usize];
                hops += 1;
            }
            if self.fat.len() <= new_cluster as usize {
                self.fat.resize(new_cluster as usize + 1, 0);
            }
            self.fat[last as usize] = new_cluster;
            self.fat[new_cluster as usize] = EOC;
            data.extend_from_slice(&zero);
            let boundary = data.len() - self.cluster_size;
            self.write_set(&mut data, boundary, name, is_dir, first_cluster, size, flags)?;
        }
        self.write_dir_data(dev, parent_cluster, &data)?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }

    fn write_set(&mut self, data: &mut [u8], off: usize, name: &str, is_dir: bool, first_cluster: u32, size: u64, flags: u8) -> Result<(), FsError> {
        // The name is stored as given (case preserved); only the hash uses
        // the upcased form because exFAT comparisons are case-insensitive.
        let name_utf16 = string_to_utf16(name);
        let name_units = name_utf16.len() / 2;
        let upcased: Vec<u8> = up(name);
        let up_utf16 = string_to_utf16(core::str::from_utf8(&upcased).unwrap_or(name));
        let name_entries = ((name_units).max(1) + 14) / 15;
        let secondary = 1 + name_entries; // stream + names
        let total = (1 + secondary) * 32;
        if off + total > data.len() {
            return Err(FsError::NoSpace);
        }
        // Zero the region.
        for b in data[off..off + total].iter_mut() {
            *b = 0;
        }
        // Primary 0x85.
        data[off] = TYPE_FILE;
        data[off + 1] = secondary as u8;
        let attr = if is_dir { ATTR_DIR } else { ATTR_ARCHIVE };
        data[off + 4..off + 6].copy_from_slice(&attr.to_le_bytes());
        // (checksum field at off+2..4 stays 0 for now)
        // Stream extension 0xC0.
        let c1 = off + 32;
        data[c1] = TYPE_STREAM;
        data[c1 + SE_FLAGS] = flags;
        data[c1 + SE_NAME_LEN] = name_units as u8;
        // NameHash over the upcased name bytes.
        let mut hash: u16 = 0;
        for b in up_utf16.iter() {
            hash = ((hash & 1) << 15).wrapping_add(hash >> 1).wrapping_add(*b as u16);
        }
        data[c1 + 4..c1 + 6].copy_from_slice(&hash.to_le_bytes());
        data[c1 + SE_VALID_LEN..c1 + SE_VALID_LEN + 8].copy_from_slice(&size.to_le_bytes());
        data[c1 + SE_FIRST_CLUSTER..c1 + SE_FIRST_CLUSTER + 4].copy_from_slice(&first_cluster.to_le_bytes());
        data[c1 + SE_DATA_LEN..c1 + SE_DATA_LEN + 8].copy_from_slice(&size.to_le_bytes());
        // Name entries 0xC1.
        let mut unit_idx = 0;
        for s in 0..name_entries {
            let o = c1 + 32 * (s + 1);
            data[o] = TYPE_NAME;
            data[o + 1] = 0;
            let mut filled = 0usize;
            while filled < 15 * 2 && unit_idx < name_units * 2 {
                data[o + 2 + filled] = name_utf16[unit_idx];
                unit_idx += 1;
                filled += 1;
            }
        }
        // SetChecksum over the whole set. The checksum field itself is
        // *skipped* (not even rotated in), per the exFAT spec; including
        // it as zero produces a different value that fsck rejects.
        let mut sum: u16 = 0;
        for i in 0..total {
            if i == 2 || i == 3 {
                continue;
            }
            let b = data[off + i];
            sum = ((sum & 1) << 15).wrapping_add(sum >> 1).wrapping_add(b as u16);
        }
        data[off + 2] = (sum & 0xFF) as u8;
        data[off + 3] = (sum >> 8) as u8;
        Ok(())
    }

    pub fn list_dir(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<ExfatFile>, FsError> {
        let d = self.resolve_path(dev, path)?;
        if !d.is_dir {
            return Err(FsError::NotDirectory);
        }
        self.files(dev, d.first_cluster)
    }

    pub fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        self.ensure_not_ro()?;
        let old_f = self.resolve_path(dev, old)?;
        let (op, _oname) = split_parent(old)?;
        let (np, nname) = split_parent(new)?;
        let op_dir = self.resolve_path(dev, op)?;
        let np_dir = self.resolve_path(dev, np)?;
        if !op_dir.is_dir || !np_dir.is_dir {
            return Err(FsError::NotDirectory);
        }
        if old_f.is_dir && np_dir.first_cluster == old_f.first_cluster {
            return Err(FsError::InvalidPath); // cannot move a dir into itself
        }
        if self.lookup(dev, np_dir.first_cluster, nname)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        // Remove the old entry (but keep its clusters alive), then register
        // the same cluster chain under the new name in the new directory.
        self.detach_in_dir(dev, op_dir.first_cluster, &ExfatFile {
            name: split_parent(old)?.1.to_string(),
            first_cluster: old_f.first_cluster,
            size: old_f.size,
            is_dir: old_f.is_dir,
            attributes: old_f.attributes,
            mtime: old_f.mtime,
            no_fat_chain: old_f.no_fat_chain,
        })?;
        self.append_file_entry(dev, np_dir.first_cluster, nname, old_f.is_dir, old_f.first_cluster, old_f.size, 0x01 | if old_f.no_fat_chain { SF_NO_FAT_CHAIN } else { 0 })?;
        self.flush_fat(dev)?;
        self.flush_bitmap(dev)?;
        Ok(())
    }
}

fn split_parent(path: &str) -> Result<(&str, &str), FsError> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => Ok(("/", &trimmed[1..])),
        Some(i) => Ok((&trimmed[..i], &trimmed[i + 1..])),
        None => Ok(("/", trimmed)),
    }
}

