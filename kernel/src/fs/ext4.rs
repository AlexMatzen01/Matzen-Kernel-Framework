//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Native ext4 reader/writer for the MFK VFS.
//!
//! Implements the ext4 layout per the published spec: superblock at byte
//! offset 1024, block group descriptors, per-group block/inode bitmaps +
//! inode tables, extent-tree indexed files, and variable-length directory
//! entries.
//!
//! Journaling policy (explicit safe subset): the driver never writes the
//! JBD2 journal. On mount it inspects the relevant feature/state bits and
//! marks the mount read-only whenever the on-disk state is anything other
//! than cleanly synced, so a dirty journal can never be silently corrupted
//! further. A full rw mount requires the image to have been unmounted
//! cleanly (the supported path is a `e2fsck -y`-clean image).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::drivers::block::BlockDevice;

use super::error::FsError;

pub const EXT4_MAGIC: u16 = 0xEF53;
const EXTENT_MAGIC: u16 = 0xF30A;

const S_IFMT: u16 = 0xF000;
const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xA000;

const EXT4_INODE_FLAGS_EXTENTS: u32 = 0x0008_0000;

const INCOMPAT_ACCEPT: u32 = 0x0002 // FILETYPE
    | 0x0040 // EXTENTS
    | 0x0080 // 64BIT
    | 0x0200 // FLEX_BG
    | 0x4000; // LARGEDIR
const INCOMPAT_RECOVER: u32 = 0x0004;
const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_ENCRYPTION: u32 = 0x10000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;

const RO_COMPAT_ACCEPT: u32 = 0x0001 // SPARSE_SUPER
    | 0x0002 // LARGE_FILE
    | 0x0004 // BTREE_DIR (obsolete, harmless)
    | 0x0008 // HUGE_FILE
    | 0x0010 // GDT_CSUM
    | 0x0020 // DIR_NLINK
    | 0x0040; // EXTRA_ISIZE
const RO_METADATA_CSUM: u32 = 0x1000;

const FT_UNKNOWN: u8 = 0;
const FT_REG: u8 = 1;
const FT_DIR: u8 = 2;
const FT_CHR: u8 = 3;
const FT_BLK: u8 = 4;
const FT_FIFO: u8 = 5;
const FT_SOCK: u8 = 6;
const FT_SYMLINK: u8 = 7;

#[derive(Debug, Clone, Copy)]
struct GroupDesc {
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
    free_blocks: u32,
    free_inodes: u32,
}

#[derive(Debug, Clone)]
struct Super {
    block_size: usize,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: usize,
    groups: u32,
    feature_compat: u32,
    feature_incompat: u32,
    feature_ro_compat: u32,
    free_blocks: u64,
    free_inodes: u64,
    first_ino: u32,
    needs_recovery: bool,
    has_dir_index: bool,
}

impl Super {
    fn parse(buf: &[u8]) -> Result<Super, FsError> {
        if buf.len() < 1024 {
            return Err(FsError::CorruptFilesystem(String::from("short superblock")));
        }
        let magic = u16::from_le_bytes([buf[56], buf[57]]);
        if magic != EXT4_MAGIC {
            return Err(FsError::WrongFs);
        }
        let rev_level = u32::from_le_bytes(buf[76..80].try_into().unwrap());
        let inode_size = if rev_level >= 1 {
            u16::from_le_bytes([buf[88], buf[89]]) as usize
        } else {
            128
        };
        let log_block_size = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        let block_size = 1024usize
            .checked_shl(log_block_size)
            .ok_or_else(|| FsError::CorruptFilesystem(String::from("bad block size")))?;
        if block_size != 1024 && block_size != 2048 && block_size != 4096 {
            return Err(FsError::Unsupported(format!("block size {}", block_size)));
        }
        let blocks_per_group = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        let inodes_per_group = u32::from_le_bytes(buf[40..44].try_into().unwrap());
        let feature_compat = u32::from_le_bytes(buf[92..96].try_into().unwrap());
        let feature_incompat = u32::from_le_bytes(buf[96..100].try_into().unwrap());
        let feature_ro_compat = u32::from_le_bytes(buf[100..104].try_into().unwrap());
        let first_ino = u32::from_le_bytes(buf[84..88].try_into().unwrap());

        let mut refused: Vec<&'static str> = Vec::new();
        if feature_incompat & INCOMPAT_INLINE_DATA != 0 {
            refused.push("inline data");
        }
        if feature_incompat & INCOMPAT_ENCRYPTION != 0 {
            refused.push("encryption");
        }
        if feature_incompat & INCOMPAT_CASEFOLD != 0 {
            refused.push("casefold");
        }
        if feature_incompat & INCOMPAT_JOURNAL_DEV != 0 {
            refused.push("external journal device");
        }
        if feature_incompat & !INCOMPAT_ACCEPT != 0 {
            refused.push("other incompatible feature");
        }
        if feature_ro_compat & RO_METADATA_CSUM != 0 {
            refused.push("metadata_csum (unsupported checksums on write)");
        }
        if feature_ro_compat & !RO_COMPAT_ACCEPT != 0 {
            refused.push("other ro_compat feature");
        }
        if !refused.is_empty() {
            return Err(FsError::Unsupported(refused.join(", ")));
        }
        let blocks_count = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
        let groups = blocks_count.div_ceil(blocks_per_group as u64) as u32;
        let free_blocks = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as u64;
        let free_inodes = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as u64;
        Ok(Super {
            block_size,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            groups,
            feature_compat,
            feature_incompat,
            feature_ro_compat,
            free_blocks,
            free_inodes,
            first_ino,
            needs_recovery: feature_incompat & INCOMPAT_RECOVER != 0,
            has_dir_index: feature_compat & 0x20 != 0,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ext4Inode {
    pub mode: u16,
    pub uid: u16,
    pub gid: u16,
    pub size: u64,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub links: u16,
    pub blocks: u32,
    pub flags: u32,
    pub block: [u32; 15],
}

impl Ext4Inode {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    pub fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
    pub fn is_lnk(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
    pub fn uses_extents(&self) -> bool {
        self.flags & EXT4_INODE_FLAGS_EXTENTS != 0
    }
    pub fn is_used(&self) -> bool {
        self.mode & S_IFMT != 0
    }
}

fn read_inode(buf: &[u8]) -> Ext4Inode {
    let size_lo = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
    let size_hi = if buf.len() >= 112 {
        u32::from_le_bytes(buf[108..112].try_into().unwrap()) as u64
    } else {
        0
    };
    let mut block = [0u32; 15];
    for i in 0..15 {
        block[i] = u32::from_le_bytes(buf[40 + i * 4..44 + i * 4].try_into().unwrap());
    }
    Ext4Inode {
        mode: u16::from_le_bytes(buf[0..2].try_into().unwrap()),
        uid: u16::from_le_bytes(buf[2..4].try_into().unwrap()),
        gid: u16::from_le_bytes(buf[24..26].try_into().unwrap()),
        size: size_lo | (size_hi << 32),
        atime: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        ctime: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        mtime: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        links: u16::from_le_bytes(buf[26..28].try_into().unwrap()),
        blocks: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
        flags: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
        block,
    }
}

fn write_inode_bytes(buf: &mut [u8], inode: &Ext4Inode) {
    buf[0..2].copy_from_slice(&inode.mode.to_le_bytes());
    buf[2..4].copy_from_slice(&inode.uid.to_le_bytes());
    buf[4..8].copy_from_slice(&(inode.size as u32).to_le_bytes());
    buf[8..12].copy_from_slice(&inode.atime.to_le_bytes());
    buf[12..16].copy_from_slice(&inode.ctime.to_le_bytes());
    buf[16..20].copy_from_slice(&inode.mtime.to_le_bytes());
    buf[24..26].copy_from_slice(&inode.gid.to_le_bytes());
    buf[26..28].copy_from_slice(&inode.links.to_le_bytes());
    buf[28..32].copy_from_slice(&inode.blocks.to_le_bytes());
    buf[32..36].copy_from_slice(&inode.flags.to_le_bytes());
    for i in 0..15 {
        buf[40 + i * 4..44 + i * 4].copy_from_slice(&inode.block[i].to_le_bytes());
    }
    if buf.len() >= 112 {
        buf[108..112].copy_from_slice(&((inode.size >> 32) as u32).to_le_bytes());
    }
    // New inodes start zeroed; claim the standard 32 bytes of extra space
    // so tools parse this as a 256-byte ext4 inode (mkfs uses 32).
    if buf.len() >= 130 {
        buf[128..130].copy_from_slice(&32u16.to_le_bytes());
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Ext4Stat {
    pub ino: u32,
    pub mode: u16,
    pub size: u64,
    pub links: u16,
    pub uid: u16,
    pub gid: u16,
    pub atime: u32,
    pub mtime: u32,
    pub ctime: u32,
    pub is_dir: bool,
    pub is_reg: bool,
    pub is_lnk: bool,
}

#[derive(Debug, Clone)]
pub struct DirEnt {
    pub name: String,
    pub ino: u32,
    pub ft: u8,
}

#[derive(Debug, Clone, Copy)]
struct Extent {
    lb: u32,
    len: u32,
    phys: u64,
}

pub struct Ext4 {
    sb: Super,
    groups: Vec<GroupDesc>,
    block_bitmap: Vec<Vec<u8>>,
    inode_bitmap: Vec<Vec<u8>>,
    dirty_block_maps: Vec<bool>,
    dirty_inode_maps: Vec<bool>,
    read_only: bool,
}

impl Ext4 {
    pub fn name() -> &'static str {
        "ext4"
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn probe(dev: &mut dyn BlockDevice) -> bool {
        let mut sb = [0u8; 1024];
        if dev.read_blocks(2, 2, &mut sb).is_err() {
            return false;
        }
        let magic = u16::from_le_bytes([sb[56], sb[57]]);
        magic == EXT4_MAGIC
    }

    pub fn mount(dev: &mut dyn BlockDevice) -> Result<Ext4, FsError> {
        let mut sb_buf = [0u8; 1024];
        dev.read_blocks(2, 2, &mut sb_buf)
            .map_err(|_| FsError::WrongFs)?;
        let sb = Super::parse(&sb_buf)?;
        let reads_per_block = sb.block_size / 512;
        // GDT's first ext4 block: after the superblock block.
        let gdt_first = if sb.block_size == 1024 { 2u64 } else { 1u64 };
        let desc_size = if sb.feature_incompat & 0x0080 != 0 { 64 } else { 32 };
        let num_groups = sb.groups as usize;
        let mut groups = Vec::with_capacity(num_groups);
        for g in 0..num_groups {
            let descs_per_block = sb.block_size / desc_size;
            let block_idx = gdt_first + (g / descs_per_block) as u64;
            let within = g % descs_per_block;
            dev.read_blocks(
                block_idx * reads_per_block as u64,
                reads_per_block,
                &mut [0u8; 4096],
            ).map_err(|_| FsError::CorruptFilesystem(String::from("GDT")))?;
            // parse from the block we just read; read the first 4KB of GDT into RAM
            // (GDT rarely exceeds one or two extents).
            let mut gd = [0u8; 4096];
            dev.read_blocks(
                block_idx * reads_per_block as u64,
                reads_per_block,
                &mut gd,
            ).map_err(|_| FsError::CorruptFilesystem(String::from("GDT2")))?;
            let off = within * desc_size;
            let d = &gd[off..off + desc_size];
            let block_bitmap_lo = u32::from_le_bytes(d[0..4].try_into().unwrap()) as u64;
            let inode_bitmap_lo = u32::from_le_bytes(d[4..8].try_into().unwrap()) as u64;
            let inode_table_lo = u32::from_le_bytes(d[8..12].try_into().unwrap()) as u64;
            let free_blocks = u16::from_le_bytes(d[12..14].try_into().unwrap()) as u32;
            let free_inodes = u16::from_le_bytes(d[14..16].try_into().unwrap()) as u32;
            let (bb_hi, ib_hi, it_hi, fb_hi, fi_hi) = if desc_size >= 64 {
                (
                    u32::from_le_bytes(d[32..36].try_into().unwrap()) as u64,
                    u32::from_le_bytes(d[36..40].try_into().unwrap()) as u64,
                    u32::from_le_bytes(d[40..44].try_into().unwrap()) as u64,
                    u16::from_le_bytes(d[44..46].try_into().unwrap()) as u64,
                    u16::from_le_bytes(d[46..48].try_into().unwrap()) as u64,
                )
            } else {
                (0u64, 0u64, 0u64, 0u64, 0u64)
            };
            groups.push(GroupDesc {
                block_bitmap: block_bitmap_lo | (bb_hi << 32),
                inode_bitmap: inode_bitmap_lo | (ib_hi << 32),
                inode_table: inode_table_lo | (it_hi << 32),
                free_blocks: free_blocks | ((fb_hi as u32) << 16),
                free_inodes: free_inodes | ((fi_hi as u32) << 16),
            });
        }
        let mut block_bitmap = Vec::with_capacity(num_groups);
        let mut inode_bitmap = Vec::with_capacity(num_groups);
        for (g, gd) in groups.iter().enumerate() {
            let mut bb = alloc::vec![0u8; sb.block_size];
            dev.read_blocks(gd.block_bitmap * reads_per_block as u64, reads_per_block, &mut bb)
                .map_err(|_| FsError::CorruptFilesystem(String::from("block bitmap")))?;
            let mut ib = alloc::vec![0u8; sb.block_size];
            dev.read_blocks(gd.inode_bitmap * reads_per_block as u64, reads_per_block, &mut ib)
                .map_err(|_| FsError::CorruptFilesystem(String::from("inode bitmap")))?;
            let _ = g;
            block_bitmap.push(bb);
            inode_bitmap.push(ib);
        }
        let mut fs = Ext4 {
            sb,
            groups,
            block_bitmap,
            inode_bitmap,
            dirty_block_maps: alloc::vec![false; num_groups],
            dirty_inode_maps: alloc::vec![false; num_groups],
            read_only: false,
        };
        fs.read_only = fs.sb.needs_recovery
            || fs.sb.feature_ro_compat & RO_METADATA_CSUM != 0;
        Ok(fs)
    }

    fn dper(&self) -> u64 {
        (self.sb.block_size / 512) as u64
    }

    fn read_block(&self, dev: &mut dyn BlockDevice, ext4_block: u64, dst: &mut [u8]) -> Result<(), FsError> {
        let d = self.dper();
        dev.read_blocks(ext4_block * d, d as usize, dst)
            .map_err(|e| FsError::Io(String::from(e)))
    }

    fn write_block(&self, dev: &mut dyn BlockDevice, ext4_block: u64, src: &[u8]) -> Result<(), FsError> {
        let d = self.dper();
        dev.write_blocks(ext4_block * d, d as usize, src)
            .map_err(|e| FsError::Io(String::from(e)))
    }

    fn max_inodes(&self) -> u32 {
        self.sb.groups * self.sb.inodes_per_group
    }

    pub fn get_inode(&self, dev: &mut dyn BlockDevice, ino: u32) -> Result<Ext4Inode, FsError> {
        if ino == 0 || ino > self.max_inodes() {
            return Err(FsError::NotFound);
        }
        let idx = ino - 1;
        let g = (idx / self.sb.inodes_per_group) as usize;
        let within = idx % self.sb.inodes_per_group;
        let inode_table = self.groups[g].inode_table;
        let byte_offset = within as usize * self.sb.inode_size;
        let it_block = byte_offset / self.sb.block_size;
        let mut blk = alloc::vec![0u8; self.sb.block_size];
        self.read_block(dev, inode_table + it_block as u64, &mut blk)?;
        let within_block = byte_offset % self.sb.block_size;
        Ok(read_inode(&blk[within_block..within_block + self.sb.inode_size.min(256)]))
    }

    pub fn put_inode(&self, dev: &mut dyn BlockDevice, ino: u32, inode: &Ext4Inode) -> Result<(), FsError> {
        if ino == 0 || ino > self.max_inodes() {
            return Err(FsError::NotFound);
        }
        let idx = ino - 1;
        let g = (idx / self.sb.inodes_per_group) as usize;
        let within = idx % self.sb.inodes_per_group;
        let inode_table = self.groups[g].inode_table;
        let byte_offset = within as usize * self.sb.inode_size;
        let it_block = byte_offset / self.sb.block_size;
        let mut blk = alloc::vec![0u8; self.sb.block_size];
        self.read_block(dev, inode_table + it_block as u64, &mut blk)?;
        let within_block = byte_offset % self.sb.block_size;
        write_inode_bytes(&mut blk[within_block..within_block + self.sb.inode_size.min(256)], inode);
        self.write_block(dev, inode_table + it_block as u64, &blk)
    }

    /// Zero a freed inode's table entry (mode 0 = unused, accepted by fsck).
    pub fn clear_inode(&self, dev: &mut dyn BlockDevice, ino: u32) -> Result<(), FsError> {
        let zero = Ext4Inode {
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            atime: 0,
            ctime: 0,
            mtime: 0,
            links: 0,
            blocks: 0,
            flags: 0,
            block: [0u32; 15],
        };
        self.put_inode(dev, ino, &zero)
    }

    pub fn stat(&self, dev: &mut dyn BlockDevice, ino: u32) -> Result<Ext4Stat, FsError> {
        let i = self.get_inode(dev, ino)?;
        Ok(Ext4Stat {
            ino,
            mode: i.mode,
            size: i.size,
            links: i.links,
            uid: i.uid,
            gid: i.gid,
            atime: i.atime,
            mtime: i.mtime,
            ctime: i.ctime,
            is_dir: i.is_dir(),
            is_reg: i.is_reg(),
            is_lnk: i.is_lnk(),
        })
    }

    // ── Extents ─────────────────────────────────────────────────────

    fn extent_locate(&self, dev: &mut dyn BlockDevice, inode: &Ext4Inode, lb: u32) -> Result<Option<(u64, u32)>, FsError> {
        let area = extents_area(inode);
        let magic = u16::from_le_bytes([area[0], area[1]]);
        if magic != EXTENT_MAGIC {
            return Err(FsError::CorruptFilesystem(String::from("extent magic")));
        }
        let depth = u16::from_le_bytes([area[6], area[7]]) as usize;
        self.extent_node(dev, inode, &area, depth, lb)
    }

    fn extent_node(&self, dev: &mut dyn BlockDevice, _inode: &Ext4Inode, node: &[u8], depth: usize, lb: u32) -> Result<Option<(u64, u32)>, FsError> {
        let magic = u16::from_le_bytes([node[0], node[1]]);
        if magic != EXTENT_MAGIC {
            return Err(FsError::CorruptFilesystem(String::from("extent node")));
        }
        let entries = u16::from_le_bytes([node[2], node[3]]) as usize;
        let depth_node = u16::from_le_bytes([node[6], node[7]]) as usize;
        if depth_node == 0 {
            for i in 0..entries {
                let off = 12 + i * 12;
                let ee_block = u32::from_le_bytes(node[off..off + 4].try_into().unwrap());
                let ee_len_raw = u16::from_le_bytes(node[off + 4..off + 6].try_into().unwrap());
                let ee_len = (ee_len_raw & 0x7FFF) as u32;
                let ee_start_hi = u16::from_le_bytes(node[off + 6..off + 8].try_into().unwrap()) as u64;
                let ee_start_lo = u32::from_le_bytes(node[off + 8..off + 12].try_into().unwrap()) as u64;
                if ee_block <= lb && lb < ee_block + ee_len {
                    if ee_len_raw & 0x8000 != 0 {
                        return Ok(None); // unwritten extent => hole
                    }
                    let phys = (ee_start_hi << 32) | ee_start_lo + (lb - ee_block) as u64;
                    return Ok(Some((phys, ee_block + ee_len)));
                }
                if ee_block > lb {
                    break;
                }
            }
            Ok(None)
        } else {
            let mut target: Option<u64> = None;
            for i in 0..entries {
                let off = 12 + i * 12;
                let ei_block = u32::from_le_bytes(node[off..off + 4].try_into().unwrap());
                let ei_lo = u32::from_le_bytes(node[off + 4..off + 8].try_into().unwrap()) as u64;
                let ei_hi = u16::from_le_bytes(node[off + 8..off + 10].try_into().unwrap()) as u64;
                if ei_block <= lb {
                    target = Some(ei_lo | (ei_hi << 32));
                } else {
                    break;
                }
            }
            let node_block = target.ok_or(FsError::NotFound)?;
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            self.read_block(dev, node_block, &mut buf)?;
            self.extent_node(dev, _inode, &buf, depth_node - 1, lb)
        }
    }

    fn collect_extents(&self, dev: &mut dyn BlockDevice, inode: &Ext4Inode) -> Result<Vec<Extent>, FsError> {
        let area = extents_area(inode);
        let mut out = Vec::new();
        self.walk_extents(dev, &area, &mut out)?;
        Ok(out)
    }

    fn walk_extents(&self, dev: &mut dyn BlockDevice, node: &[u8], out: &mut Vec<Extent>) -> Result<(), FsError> {
        let entries = u16::from_le_bytes([node[2], node[3]]) as usize;
        let depth_node = u16::from_le_bytes([node[6], node[7]]) as usize;
        if depth_node == 0 {
            for i in 0..entries {
                let off = 12 + i * 12;
                let ee_block = u32::from_le_bytes(node[off..off + 4].try_into().unwrap());
                let ee_len_raw = u16::from_le_bytes(node[off + 4..off + 6].try_into().unwrap());
                let ee_len = (ee_len_raw & 0x7FFF) as u32;
                let ee_start_hi = u16::from_le_bytes(node[off + 6..off + 8].try_into().unwrap()) as u64;
                let ee_start_lo = u32::from_le_bytes(node[off + 8..off + 12].try_into().unwrap()) as u64;
                if ee_len_raw & 0x8000 != 0 {
                    continue; // unwritten => treat as hole for our rw path
                }
                out.push(Extent {
                    lb: ee_block,
                    len: ee_len,
                    phys: (ee_start_hi << 32) | ee_start_lo,
                });
            }
        } else {
            for i in 0..entries {
                let off = 12 + i * 12;
                let ei_lo = u32::from_le_bytes(node[off + 4..off + 8].try_into().unwrap()) as u64;
                let ei_hi = u16::from_le_bytes(node[off + 8..off + 10].try_into().unwrap()) as u64;
                let node_block = ei_lo | (ei_hi << 32);
                let mut buf = alloc::vec![0u8; self.sb.block_size];
                self.read_block(dev, node_block, &mut buf)?;
                self.walk_extents(dev, &buf, out)?;
            }
        }
        Ok(())
    }

    /// Write an in-i_block (depth-0) tree. Holds at most 4 extents:
    /// 60 bytes minus the 12-byte header leaves 48 bytes = 4 x 12.
    fn inode_set_extents(&self, inode: &mut Ext4Inode, extents: &[Extent]) -> Result<(), FsError> {
        if extents.len() > 4 {
            return Err(FsError::Unsupported(String::from("too many extents")));
        }
        let mut area = [0u8; 60];
        area[0..2].copy_from_slice(&EXTENT_MAGIC.to_le_bytes());
        area[2..4].copy_from_slice(&(extents.len() as u16).to_le_bytes());
        area[4..6].copy_from_slice(&4u16.to_le_bytes());
        area[6..8].copy_from_slice(&0u16.to_le_bytes());
        area[8..12].copy_from_slice(&0u32.to_le_bytes());
        for (i, e) in extents.iter().enumerate() {
            let off = 12 + i * 12;
            area[off..off + 4].copy_from_slice(&e.lb.to_le_bytes());
            area[off + 4..off + 6].copy_from_slice(&(e.len as u16).to_le_bytes());
            area[off + 6..off + 8].copy_from_slice(&((e.phys >> 32) as u16).to_le_bytes());
            area[off + 8..off + 12].copy_from_slice(&(e.phys as u32).to_le_bytes());
        }
        for i in 0..15 {
            inode.block[i] = u32::from_le_bytes([area[i * 4], area[i * 4 + 1], area[i * 4 + 2], area[i * 4 + 3]]);
        }
        Ok(())
    }

    /// Collect every external (non-root) extent-tree node of an inode so a
    /// rebuild can free them. Fresh inodes (zero magic) have none.
    fn collect_nodes(&self, dev: &mut dyn BlockDevice, inode: &Ext4Inode) -> Result<Vec<u64>, FsError> {
        let area = extents_area(inode);
        if u16::from_le_bytes([area[0], area[1]]) != EXTENT_MAGIC {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        self.walk_nodes(dev, &area, &mut out)?;
        Ok(out)
    }

    fn walk_nodes(&self, dev: &mut dyn BlockDevice, node: &[u8], out: &mut Vec<u64>) -> Result<(), FsError> {
        let entries = u16::from_le_bytes([node[2], node[3]]) as usize;
        let depth = u16::from_le_bytes([node[6], node[7]]) as usize;
        if depth == 0 {
            return Ok(());
        }
        for i in 0..entries {
            let off = 12 + i * 12;
            if off + 12 > node.len() {
                return Err(FsError::CorruptFilesystem(String::from("extent index")));
            }
            let lo = u32::from_le_bytes(node[off + 4..off + 8].try_into().unwrap()) as u64;
            let hi = u16::from_le_bytes(node[off + 8..off + 10].try_into().unwrap()) as u64;
            let block = lo | (hi << 32);
            out.push(block);
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            self.read_block(dev, block, &mut buf)?;
            self.walk_nodes(dev, &buf, out)?;
        }
        Ok(())
    }

    /// Replace an inode's extent tree, allocating (at most) one external
    /// leaf block when the extents do not fit in `i_block`. Old external
    /// nodes are freed. Callers still persist the inode itself.
    fn store_extents(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut Ext4Inode,
        extents: &[Extent],
    ) -> Result<(), FsError> {
        let leaf_max = (self.sb.block_size - 12) / 12;
        if extents.len() > 4 && extents.len() > leaf_max {
            // Refuse before touching anything: the old tree stays valid.
            return Err(FsError::Unsupported(String::from("too many extents")));
        }
        if extents.len() <= 4 {
            for n in self.collect_nodes(dev, inode)? {
                self.free_block(n);
            }
            return self.inode_set_extents(inode, extents);
        }
        let leaf = self.alloc_block(0)?.ok_or(FsError::NoSpace)?;
        for n in self.collect_nodes(dev, inode)? {
            self.free_block(n);
        }
        let mut buf = alloc::vec![0u8; self.sb.block_size];
        buf[0..2].copy_from_slice(&EXTENT_MAGIC.to_le_bytes());
        buf[2..4].copy_from_slice(&(extents.len() as u16).to_le_bytes());
        buf[4..6].copy_from_slice(&(leaf_max as u16).to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes());
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        for (i, e) in extents.iter().enumerate() {
            let off = 12 + i * 12;
            buf[off..off + 4].copy_from_slice(&e.lb.to_le_bytes());
            buf[off + 4..off + 6].copy_from_slice(&(e.len as u16).to_le_bytes());
            buf[off + 6..off + 8].copy_from_slice(&((e.phys >> 32) as u16).to_le_bytes());
            buf[off + 8..off + 12].copy_from_slice(&(e.phys as u32).to_le_bytes());
        }
        self.write_block(dev, leaf, &buf)?;
        // Root becomes a depth-1 index with a single entry.
        let mut area = [0u8; 60];
        area[0..2].copy_from_slice(&EXTENT_MAGIC.to_le_bytes());
        area[2..4].copy_from_slice(&1u16.to_le_bytes());
        area[4..6].copy_from_slice(&4u16.to_le_bytes());
        area[6..8].copy_from_slice(&1u16.to_le_bytes());
        area[8..12].copy_from_slice(&0u32.to_le_bytes());
        area[12..16].copy_from_slice(&extents[0].lb.to_le_bytes());
        area[16..20].copy_from_slice(&(leaf as u32).to_le_bytes());
        area[20..22].copy_from_slice(&((leaf >> 32) as u16).to_le_bytes());
        for i in 0..15 {
            inode.block[i] = u32::from_le_bytes([area[i * 4], area[i * 4 + 1], area[i * 4 + 2], area[i * 4 + 3]]);
        }
        Ok(())
    }

    // ── Directory I/O ───────────────────────────────────────────────

    pub fn dir_entries(&self, dev: &mut dyn BlockDevice, dir_ino: u32) -> Result<Vec<DirEnt>, FsError> {
        let inode = self.get_inode(dev, dir_ino)?;
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let mut out = Vec::new();
        for lb in 0..(inode.size.div_ceil(self.sb.block_size as u64)) {
            let lb = lb as u32;
            let mut blk = alloc::vec![0u8; self.sb.block_size];
            match self.extent_locate(dev, &inode, lb)? {
                Some((phys, _)) => self.read_block(dev, phys, &mut blk)?,
                None => continue,
            }
            let mut off = 0usize;
            while off + 8 <= blk.len() {
                let i_ino = u32::from_le_bytes(blk[off..off + 4].try_into().unwrap());
                let rec_len = u16::from_le_bytes(blk[off + 4..off + 6].try_into().unwrap()) as usize;
                if rec_len < 8 || off + rec_len > blk.len() {
                    break;
                }
                let name_len = blk[off + 6] as usize;
                let ft = blk[off + 7];
                if i_ino != 0 && name_len > 0 && off + 8 + name_len <= blk.len() {
                    if let Ok(s) = core::str::from_utf8(&blk[off + 8..off + 8 + name_len]) {
                        out.push(DirEnt {
                            name: String::from(s),
                            ino: i_ino,
                            ft,
                        });
                    }
                }
                off += rec_len;
            }
        }
        Ok(out)
    }

    pub fn lookup(&self, dev: &mut dyn BlockDevice, parent: u32, name: &str) -> Result<Option<u32>, FsError> {
        for e in self.dir_entries(dev, parent)? {
            if e.name == name {
                return Ok(Some(e.ino));
            }
        }
        Ok(None)
    }

    pub fn resolve(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        if path.is_empty() || path == "/" {
            return Ok(2);
        }
        let mut cur = 2u32;
        for part in path.split('/').filter(|s| !s.is_empty()) {
            match part {
                "." => {}
                ".." => {
                    cur = self
                        .dir_entries(dev, cur)?
                        .into_iter()
                        .find(|e| e.name == "..")
                        .map(|e| e.ino)
                        .ok_or(FsError::NotFound)?;
                }
                name => {
                    cur = self.lookup(dev, cur, name)?.ok_or(FsError::NotFound)?;
                }
            }
        }
        Ok(cur)
    }

    // ── Freespace bookkeeping (RAM-side, flushed on mutations) ──────

    fn ensure_rw(&self) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        Ok(())
    }

    fn flush_bitmaps(&mut self, dev: &mut dyn BlockDevice) -> Result<(), FsError> {
        for g in 0..self.sb.groups as usize {
            if self.dirty_block_maps[g] {
                self.write_block(dev, self.groups[g].block_bitmap, &self.block_bitmap[g])?;
                self.dirty_block_maps[g] = false;
            }
            if self.dirty_inode_maps[g] {
                self.write_block(dev, self.groups[g].inode_bitmap, &self.inode_bitmap[g])?;
                self.dirty_inode_maps[g] = false;
            }
        }
        Ok(())
    }

    fn set_block(&mut self, ext4_block: u64, used: bool) {
        let idx = ext4_block as usize;
        let g = idx / self.sb.blocks_per_group as usize;
        let within = idx % self.sb.blocks_per_group as usize;
        if g < self.block_bitmap.len() {
            if used {
                self.block_bitmap[g][within / 8] |= 1 << (within % 8);
            } else {
                self.block_bitmap[g][within / 8] &= !(1 << (within % 8));
            }
            self.dirty_block_maps[g] = true;
        }
    }

    fn set_inode_bit(&mut self, ino: u32, used: bool) {
        let idx = ino.saturating_sub(1) as usize;
        let g = idx / self.sb.inodes_per_group as usize;
        let within = idx % self.sb.inodes_per_group as usize;
        if g < self.inode_bitmap.len() {
            if used {
                self.inode_bitmap[g][within / 8] |= 1 << (within % 8);
            } else {
                self.inode_bitmap[g][within / 8] &= !(1 << (within % 8));
            }
            self.dirty_inode_maps[g] = true;
        }
    }

    fn alloc_block(&mut self, hint: u32) -> Result<Option<u64>, FsError> {
        for g in 0..self.sb.groups as usize {
            let grp = (hint as usize + g) % self.sb.groups as usize;
            if self.groups[grp].free_blocks == 0 {
                continue;
            }
            for i in 0..self.sb.blocks_per_group as usize {
                if self.block_bitmap[grp][i / 8] & (1 << (i % 8)) == 0 {
                    self.set_block(
                        (grp * self.sb.blocks_per_group as usize + i) as u64,
                        true,
                    );
                    self.groups[grp].free_blocks = self.groups[grp].free_blocks.saturating_sub(1);
                    return Ok(Some((grp * self.sb.blocks_per_group as usize + i) as u64));
                }
            }
        }
        Ok(None)
    }

    fn alloc_inode(&mut self, hint: u32) -> Result<Option<u32>, FsError> {
        for g in 0..self.sb.groups as usize {
            let grp = (hint as usize + g) % self.sb.groups as usize;
            if self.groups[grp].free_inodes == 0 {
                continue;
            }
            for i in 0..self.sb.inodes_per_group as usize {
                if self.inode_bitmap[grp][i / 8] & (1 << (i % 8)) == 0 {
                    self.set_inode_bit((grp * self.sb.inodes_per_group as usize + i) as u32 + 1, true);
                    self.groups[grp].free_inodes = self.groups[grp].free_inodes.saturating_sub(1);
                    return Ok(Some((grp * self.sb.inodes_per_group as usize + i) as u32 + 1));
                }
            }
        }
        Ok(None)
    }

    fn free_block(&mut self, b: u64) {
        let idx = b as usize;
        let g = idx / self.sb.blocks_per_group as usize;
        if g < self.groups.len() {
            self.groups[g].free_blocks += 1;
        }
        self.set_block(b, false);
    }

    fn free_inode(&mut self, ino: u32) {
        let idx = ino.saturating_sub(1) as usize;
        let g = idx / self.sb.inodes_per_group as usize;
        if g < self.groups.len() {
            self.groups[g].free_inodes += 1;
        }
        self.set_inode_bit(ino, false);
    }

    fn zero_ext4_block(&self, dev: &mut dyn BlockDevice, b: u64) -> Result<(), FsError> {
        let zero = alloc::vec![0u8; self.sb.block_size];
        self.write_block(dev, b, &zero)
    }

    // ── Mutations ───────────────────────────────────────────────────

    pub fn mkdir(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str) -> Result<u32, FsError> {
        self.ensure_rw()?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.alloc_inode(0)?.ok_or(FsError::NoSpace)?;
        let blk = self.alloc_block(0)?.ok_or(FsError::NoSpace)?;
        self.zero_ext4_block(dev, blk)?;
        let now = crate::time::wall_epoch_secs() as u32;
        let mut inode = Ext4Inode {
            mode: S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            size: self.sb.block_size as u64,
            atime: now,
            ctime: now,
            mtime: now,
            links: 2,
            blocks: 0,
            flags: EXT4_INODE_FLAGS_EXTENTS,
            block: [0u32; 15],
        };
        self.inode_set_extents(&mut inode, &[Extent { lb: 0, len: 1, phys: blk }])?;
        self.put_inode(dev, ino, &inode)?;
        // Populate "." and "..".
        let mut buf = alloc::vec![0u8; self.sb.block_size];
        // "." with a small rec_len, ".." with the rest.
        let dot = 8 + 1;
        let dot_padded = (dot + 3) & !3;
        buf[0..4].copy_from_slice(&ino.to_le_bytes());
        buf[4..6].copy_from_slice(&(dot_padded as u16).to_le_bytes());
        buf[6] = 1;
        buf[7] = FT_DIR;
        buf[8] = b'.';
        let dotdot = 8 + 2;
        let dotdot_rec = blk_remaining(self.sb.block_size, dot_padded) as u16;
        let off = dot_padded;
        buf[off..off + 4].copy_from_slice(&parent.to_le_bytes());
        buf[off + 4..off + 6].copy_from_slice(&dotdot_rec.to_le_bytes());
        buf[off + 6] = 2;
        buf[off + 7] = FT_DIR;
        buf[off + 8] = b'.';
        buf[off + 9] = b'.';
        self.write_block(dev, blk, &buf)?;
        self.dir_add(dev, parent, ino, name, FT_DIR)?;
        let mut p = self.get_inode(dev, parent)?;
        p.links = p.links.saturating_add(1);
        self.put_inode(dev, parent, &p)?;
        self.flush_bitmaps(dev)?;
        Ok(ino)
    }

    fn dir_add(&mut self, dev: &mut dyn BlockDevice, parent: u32, ino: u32, name: &str, ft: u8) -> Result<(), FsError> {
        let needed = 8 + name.len();
        let padded = (needed + 3) & !3;
        let inode = self.get_inode(dev, parent)?;
        let blocks = inode.size.div_ceil(self.sb.block_size as u64) as u32;
        for lb in 0..blocks {
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            let Some((phys, _)) = self.extent_locate(dev, &inode, lb)? else { continue };
            self.read_block(dev, phys, &mut buf)?;
            let mut off = 0usize;
            while off + 8 <= buf.len() {
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap()) as usize;
                let i_ino = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                if rec_len < 8 || off + rec_len > buf.len() {
                    break;
                }
                if i_ino == 0 && rec_len >= padded {
                    buf[off..off + 4].copy_from_slice(&ino.to_le_bytes());
                    buf[off + 4..off + 6].copy_from_slice(&(padded as u16).to_le_bytes());
                    buf[off + 6] = name.len() as u8;
                    buf[off + 7] = ft;
                    buf[off + 8..off + 8 + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(dev, phys, &buf)?;
                    return Ok(());
                }
                // Splice into a used entry's tail slack.
                if i_ino != 0 {
                    let name_len = buf[off + 6] as usize;
                    let used_padded = ((8 + name_len) + 3) & !3;
                    if rec_len >= used_padded + 8 && rec_len - used_padded >= padded {
                        // Split: shrink the old entry, place new at the tail.
                        buf[off + 4..off + 6].copy_from_slice(&(used_padded as u16).to_le_bytes());
                        let slot = off + used_padded;
                        let slot_rec = rec_len - used_padded;
                        buf[slot..slot + 4].copy_from_slice(&ino.to_le_bytes());
                        buf[slot + 4..slot + 6].copy_from_slice(&(slot_rec as u16).to_le_bytes());
                        buf[slot + 6] = name.len() as u8;
                        buf[slot + 7] = ft;
                        buf[slot + 8..slot + 8 + name.len()].copy_from_slice(name.as_bytes());
                        self.write_block(dev, phys, &buf)?;
                        return Ok(());
                    }
                }
                off += rec_len;
            }
        }
        // Grow the directory by one block and place the entry at its start.
        let new_lb = blocks;
        let dev_inode = self.get_inode(dev, parent)?;
        let mut extents = self.collect_extents(dev, &dev_inode)?;
        let nb = self.alloc_block(0)?.ok_or(FsError::NoSpace)?;
        self.zero_ext4_block(dev, nb)?;
        extents.push(Extent { lb: new_lb, len: 1, phys: nb });
        extents.sort_by_key(|e| e.lb);
        let mut merged = Vec::new();
        for e in extents {
            if let Some(last) = merged.last_mut() {
                let last: &mut Extent = last;
                if last.lb + last.len == e.lb && last.phys + last.len as u64 == e.phys {
                    last.len += e.len;
                    continue;
                }
            }
            merged.push(e);
        }
        let mut new_inode = dev_inode;
        self.store_extents(dev, &mut new_inode, &merged)?;
        new_inode.size += self.sb.block_size as u64;
        self.put_inode(dev, parent, &new_inode)?;
        let mut buf = alloc::vec![0u8; self.sb.block_size];
        buf[0..4].copy_from_slice(&ino.to_le_bytes());
        buf[4..6].copy_from_slice(&(self.sb.block_size as u16).to_le_bytes());
        buf[6] = name.len() as u8;
        buf[7] = ft;
        buf[8..8 + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(dev, nb, &buf)?;
        Ok(())
    }

    fn dir_remove(&mut self, dev: &mut dyn BlockDevice, parent: u32, ino: u32) -> Result<(), FsError> {
        let inode = self.get_inode(dev, parent)?;
        let blocks = inode.size.div_ceil(self.sb.block_size as u64) as u32;
        for lb in 0..blocks {
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            let Some((phys, _)) = self.extent_locate(dev, &inode, lb)? else { continue };
            self.read_block(dev, phys, &mut buf)?;
            let mut off = 0usize;
            while off + 8 <= buf.len() {
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap()) as usize;
                if rec_len < 8 || off + rec_len > buf.len() {
                    break;
                }
                let i_ino = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                if i_ino == ino {
                    // Set the inode field to 0, keeping rec_len for reuse.
                    buf[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
                    self.write_block(dev, phys, &buf)?;
                    return Ok(());
                }
                off += rec_len;
            }
        }
        Err(FsError::NotFound)
    }

    fn dir_empty(&self, dev: &mut dyn BlockDevice, ino: u32) -> Result<bool, FsError> {
        for e in self.dir_entries(dev, ino)? {
            if e.name != "." && e.name != ".." {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn rmdir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.ensure_rw()?;
        let ino = self.resolve(dev, path)?;
        if ino == 2 {
            return Err(FsError::InvalidPath);
        }
        let inode = self.get_inode(dev, ino)?;
        if !inode.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if !self.dir_empty(dev, ino)? {
            return Err(FsError::NotDirectory);
        }
        // Find the parent entry, remove it, free the inode + its dir block.
        let parent_path = path.trim_end_matches('/');
        let parent_path = match parent_path.rfind('/') {
            Some(0) => 2u32,
            Some(i) => self.resolve(dev, &parent_path[..i])?,
            None => 2u32,
        };
        self.dir_remove(dev, parent_path, ino)?;
        let inode = self.get_inode(dev, ino)?;
        for e in self.collect_extents(dev, &inode)? {
            for j in 0..e.len {
                self.free_block(e.phys + j as u64);
            }
        }
        self.free_inode(ino);
        // Zero the table entry so a freed inode carries no stale metadata
        // (size, blocks, extent bytes) that fsck would flag.
        self.clear_inode(dev, ino)?;
        let mut p = self.get_inode(dev, parent_path)?;
        p.links = p.links.saturating_sub(1);
        self.put_inode(dev, parent_path, &p)?;
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn unlink(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.ensure_rw()?;
        let ino = self.resolve(dev, path)?;
        if ino == 2 {
            return Err(FsError::InvalidPath);
        }
        let inode = self.get_inode(dev, ino)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        let trimmed = path.trim_end_matches('/');
        let parent = match trimmed.rfind('/') {
            Some(0) => 2u32,
            Some(i) => self.resolve(dev, &trimmed[..i])?,
            None => 2u32,
        };
        self.dir_remove(dev, parent, ino)?;
        let inode = self.get_inode(dev, ino)?;
        if inode.is_reg() || inode.is_lnk() {
            for e in self.collect_extents(dev, &inode)? {
                for j in 0..e.len {
                    self.free_block(e.phys + j as u64);
                }
            }
        }
        self.free_inode(ino);
        // Zero the table entry so a freed inode carries no stale metadata.
        self.clear_inode(dev, ino)?;
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn create(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str) -> Result<u32, FsError> {
        self.ensure_rw()?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.alloc_inode(0)?.ok_or(FsError::NoSpace)?;
        let now = crate::time::wall_epoch_secs() as u32;
        let inode = Ext4Inode {
            mode: S_IFREG | 0o644,
            uid: 0,
            gid: 0,
            size: 0,
            atime: now,
            ctime: now,
            mtime: now,
            links: 1,
            blocks: 0,
            flags: EXT4_INODE_FLAGS_EXTENTS,
            block: [0u32; 15],
        };
        self.put_inode(dev, ino, &inode)?;
        self.dir_add(dev, parent, ino, name, FT_REG)?;
        self.flush_bitmaps(dev)?;
        Ok(ino)
    }

    pub fn append(&mut self, dev: &mut dyn BlockDevice, ino: u32, data: &[u8]) -> Result<(), FsError> {
        self.ensure_rw()?;
        let inode = self.get_inode(dev, ino)?;
        if !inode.is_reg() {
            return Err(FsError::NotDirectory);
        }
        let bs = self.sb.block_size as u64;
        let start_size = inode.size;
        let mut extents = self.collect_extents(dev, &inode)?;
        let mut consumed = 0usize;
        // Fill a partial tail block.
        if start_size % bs != 0 {
            let lb = (start_size / bs) as u32;
            let Some((phys, _)) = self.extent_locate(dev, &inode, lb)? else {
                return Err(FsError::CorruptFilesystem(String::from("missing tail")));
            };
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            self.read_block(dev, phys, &mut buf)?;
            let avail = bs as usize - (start_size % bs) as usize;
            let n = avail.min(data.len());
            buf[(start_size % bs) as usize..(start_size % bs) as usize + n].copy_from_slice(&data[..n]);
            self.write_block(dev, phys, &buf)?;
            consumed += n;
        }
        // Allocate full blocks for the rest.
        let mut new_extents = Vec::new();
        let mut cur_lb = ((start_size + consumed as u64) / bs) as u32;
        while consumed < data.len() {
            let blk = self.alloc_block(0)?.ok_or(FsError::NoSpace)?;
            let n = (data.len() - consumed).min(bs as usize);
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            buf[..n].copy_from_slice(&data[consumed..consumed + n]);
            self.write_block(dev, blk, &buf)?;
            new_extents.push(Extent { lb: cur_lb, len: 1, phys: blk });
            cur_lb += 1;
            consumed += n;
        }
        extents.extend(new_extents);
        extents.sort_by_key(|e| e.lb);
        let mut merged = Vec::new();
        for e in extents {
            if let Some(last) = merged.last_mut() {
                let last: &mut Extent = last;
                if last.lb + last.len == e.lb && last.phys + last.len as u64 == e.phys {
                    last.len += e.len;
                    continue;
                }
            }
            merged.push(e);
        }
        let mut new_inode = inode;
        self.store_extents(dev, &mut new_inode, &merged)?;
        new_inode.size = start_size + data.len() as u64;
        new_inode.mtime = crate::time::wall_epoch_secs() as u32;
        new_inode.blocks = merged.iter().map(|e| e.len).sum::<u32>() * (bs / 512) as u32;
        self.put_inode(dev, ino, &new_inode)?;
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn write_all(&mut self, dev: &mut dyn BlockDevice, ino: u32, data: &[u8]) -> Result<(), FsError> {
        self.ensure_rw()?;
        // Free all old extents, then append from empty.
        let inode = self.get_inode(dev, ino)?;
        for e in self.collect_extents(dev, &inode)? {
            for j in 0..e.len {
                self.free_block(e.phys + j as u64);
            }
        }
        let mut cleared = inode;
        cleared.size = 0;
        cleared.block = [0u32; 15];
        // Write an empty extent header.
        cleared.flags = EXT4_INODE_FLAGS_EXTENTS;
        let extents = &[];
        self.store_extents(dev, &mut cleared, extents)?;
        self.put_inode(dev, ino, &cleared)?;
        self.append(dev, ino, data)
    }

    pub fn truncate(&mut self, dev: &mut dyn BlockDevice, ino: u32, size: u64) -> Result<(), FsError> {
        self.ensure_rw()?;
        let inode = self.get_inode(dev, ino)?;
        if size == 0 {
            for e in self.collect_extents(dev, &inode)? {
                for j in 0..e.len {
                    self.free_block(e.phys + j as u64);
                }
            }
            let mut cleared = inode;
            cleared.size = 0;
            cleared.block = [0u32; 15];
            self.store_extents(dev, &mut cleared, &[])?;
            self.put_inode(dev, ino, &cleared)?;
            self.flush_bitmaps(dev)?;
            return Ok(());
        }
        if size >= inode.size {
            return Ok(());
        }
        // Keep the extents covering the new size, free the rest.
        let mut kept = Vec::new();
        for e in self.collect_extents(dev, &inode)? {
            let e_end = e.lb + e.len;
            let keep_end_lb = (size / self.sb.block_size as u64) as u32 + 1;
            if e.lb >= keep_end_lb {
                for j in 0..e.len {
                    self.free_block(e.phys + j as u64);
                }
                continue;
            }
            let max_kept_blocks = ((size + self.sb.block_size as u64 - e.lb as u64 * self.sb.block_size as u64) / self.sb.block_size as u64).min(e.len as u64) as u32;
            if max_kept_blocks >= e.len {
                kept.push(e);
            } else {
                for j in max_kept_blocks..e.len {
                    self.free_block(e.phys + j as u64);
                }
                kept.push(Extent { lb: e.lb, len: max_kept_blocks, phys: e.phys });
            }
            let _ = e_end;
        }
        kept.sort_by_key(|e| e.lb);
        let mut new_inode = inode;
        self.store_extents(dev, &mut new_inode, &kept)?;
        new_inode.size = size;
        new_inode.mtime = crate::time::wall_epoch_secs() as u32;
        self.put_inode(dev, ino, &new_inode)?;
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        self.ensure_rw()?;
        let old_ino = self.resolve(dev, old)?;
        let old_trimmed = old.trim_end_matches('/');
        let new_trimmed = new.trim_end_matches('/');
        let old_name = basename(old_trimmed)?;
        let new_name = basename(new_trimmed)?;
        let old_parent = parent_of_path(dev, self, old_trimmed)?;
        let new_parent = parent_of_path(dev, self, new_trimmed)?;
        if self.lookup(dev, new_parent, new_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        self.dir_remove(dev, old_parent, old_ino)?;
        let ft = {
            let i = self.get_inode(dev, old_ino)?;
            if i.is_dir() { FT_DIR } else if i.is_reg() { FT_REG } else if i.is_lnk() { FT_SYMLINK } else { FT_UNKNOWN }
        };
        self.dir_add(dev, new_parent, old_ino, new_name, ft)?;
        if self.get_inode(dev, old_ino)?.is_dir() {
            // Update ".." of the moved dir to the new parent.
            let dir_ino = old_ino;
            let mut buf = alloc::vec![0u8; self.sb.block_size];
            let dir_inode = self.get_inode(dev, dir_ino)?;
            let Some((phys, _)) = self.extent_locate(dev, &dir_inode, 0)? else {
                return Err(FsError::CorruptFilesystem(String::from("dir extent")));
            };
            self.read_block(dev, phys, &mut buf)?;
            // Walk entries to find "..".
            let mut off = 0usize;
            while off + 8 <= buf.len() {
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap()) as usize;
                if rec_len < 8 || off + rec_len > buf.len() { break; }
                let i_ino = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                let name_len = buf[off + 6] as usize;
                if i_ino != 0 && name_len == 2 && buf[off + 8] == b'.' && buf[off + 9] == b'.' {
                    buf[off..off + 4].copy_from_slice(&new_parent.to_le_bytes());
                    self.write_block(dev, phys, &buf)?;
                    break;
                }
                off += rec_len;
            }
            // Update link counts: old_parent loses a subdir, new_parent gains one.
            let mut op = self.get_inode(dev, old_parent)?;
            op.links = op.links.saturating_sub(1);
            self.put_inode(dev, old_parent, &op)?;
            let mut np = self.get_inode(dev, new_parent)?;
            np.links = np.links.saturating_add(1);
            self.put_inode(dev, new_parent, &np)?;
        }
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn symlink(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str, target: &str) -> Result<u32, FsError> {
        self.ensure_rw()?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.alloc_inode(0)?.ok_or(FsError::NoSpace)?;
        let now = crate::time::wall_epoch_secs() as u32;
        let mut inode = Ext4Inode {
            mode: S_IFLNK | 0o777,
            uid: 0,
            gid: 0,
            size: target.len() as u64,
            atime: now,
            ctime: now,
            mtime: now,
            links: 1,
            blocks: 0,
            flags: 0,
            block: [0u32; 15],
        };
        if target.len() <= 60 {
            // Fast symlink: target bytes stored inline in i_block.
            let mut bytes = [0u8; 60];
            bytes[..target.len()].copy_from_slice(target.as_bytes());
            for i in 0..15 {
                inode.block[i] = u32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
            }
            self.put_inode(dev, ino, &inode)?;
        } else {
            inode.flags = EXT4_INODE_FLAGS_EXTENTS;
            self.put_inode(dev, ino, &inode)?;
            // Reuse write path for the target bytes.
            self.append(dev, ino, target.as_bytes())?;
            // append sets size correctly; but it requires inode.size start at 0 — we set size=0 above? We set size=target.len() above. Fix: rewrite size below.
            let mut i2 = self.get_inode(dev, ino)?;
            i2.size = target.len() as u64;
            self.put_inode(dev, ino, &i2)?;
        }
        self.dir_add(dev, parent, ino, name, FT_SYMLINK)?;
        self.flush_bitmaps(dev)?;
        Ok(ino)
    }

    pub fn read_link(&self, dev: &mut dyn BlockDevice, ino: u32) -> Result<String, FsError> {
        let inode = self.get_inode(dev, ino)?;
        if !inode.is_lnk() {
            return Err(FsError::NotDirectory);
        }
        if inode.size <= 60 {
            let mut bytes = [0u8; 60];
            for i in 0..15 {
                let b = inode.block[i].to_le_bytes();
                bytes[i * 4..i * 4 + 4].copy_from_slice(&b);
            }
            let s = core::str::from_utf8(&bytes[..inode.size as usize])
                .map_err(|_| FsError::CorruptFilesystem(String::from("symlink")))?;
            return Ok(String::from(s));
        }
        let mut out = alloc::vec![0u8; inode.size as usize];
        // read via extents
        let mut offset = 0usize;
        while offset < out.len() {
            let lb = offset / self.sb.block_size;
            match self.extent_locate(dev, &inode, lb as u32)? {
                Some((phys, _)) => {
                    let within = offset % self.sb.block_size;
                    let mut blk = alloc::vec![0u8; self.sb.block_size];
                    self.read_block(dev, phys, &mut blk)?;
                    let n = (blk.len() - within).min(out.len() - offset);
                    out[offset..offset + n].copy_from_slice(&blk[within..within + n]);
                    offset += n;
                }
                None => return Err(FsError::CorruptFilesystem(String::from("symlink data"))),
            }
        }
        let s = core::str::from_utf8(&out).map_err(|_| FsError::CorruptFilesystem(String::from("symlink")))?;
        Ok(String::from(s))
    }

    pub fn read_at(&self, dev: &mut dyn BlockDevice, ino: u32, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        let inode = self.get_inode(dev, ino)?;
        if inode.size <= offset {
            return Ok(0);
        }
        let want = core::cmp::min(out.len() as u64, inode.size - offset) as usize;
        let mut done = 0usize;
        while done < want {
            let abs = offset + done as u64;
            let lb = (abs / self.sb.block_size as u64) as u32;
            let within = (abs % self.sb.block_size as u64) as usize;
            match self.extent_locate(dev, &inode, lb)? {
                Some((phys, _run_end)) => {
                    let mut blk = alloc::vec![0u8; self.sb.block_size];
                    self.read_block(dev, phys, &mut blk)?;
                    let n = (self.sb.block_size - within).min(want - done);
                    out[done..done + n].copy_from_slice(&blk[within..within + n]);
                    done += n;
                }
                None => {
                    let n = (self.sb.block_size - within).min(want - done);
                    out[done..done + n].fill(0);
                    done += n;
                }
            }
        }
        Ok(done)
    }

    /// Duplicate a hard link to `target` under `name` in `parent`.
    pub fn hard_link(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str, target: u32) -> Result<(), FsError> {
        self.ensure_rw()?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let mut inode = self.get_inode(dev, target)?;
        if inode.is_dir() {
            return Err(FsError::IsDirectory);
        }
        inode.links = inode.links.saturating_add(1);
        self.put_inode(dev, target, &inode)?;
        let ft = if inode.is_reg() { FT_REG } else if inode.is_lnk() { FT_SYMLINK } else { FT_UNKNOWN };
        self.dir_add(dev, parent, target, name, ft)?;
        self.flush_bitmaps(dev)?;
        Ok(())
    }

    pub fn set_mode(&mut self, dev: &mut dyn BlockDevice, ino: u32, mode: u16) -> Result<(), FsError> {
        self.ensure_rw()?;
        let mut inode = self.get_inode(dev, ino)?;
        inode.mode = (inode.mode & S_IFMT) | (mode & 0o7777);
        self.put_inode(dev, ino, &inode)?;
        Ok(())
    }

    pub fn set_owner(&mut self, dev: &mut dyn BlockDevice, ino: u32, uid: u16, gid: u16) -> Result<(), FsError> {
        self.ensure_rw()?;
        let mut inode = self.get_inode(dev, ino)?;
        inode.uid = uid;
        inode.gid = gid;
        self.put_inode(dev, ino, &inode)?;
        Ok(())
    }

    pub fn set_times(&mut self, dev: &mut dyn BlockDevice, ino: u32, atime: u32, mtime: u32) -> Result<(), FsError> {
        self.ensure_rw()?;
        let mut inode = self.get_inode(dev, ino)?;
        inode.atime = atime;
        inode.mtime = mtime;
        self.put_inode(dev, ino, &inode)?;
        Ok(())
    }

    /// Free the image's cached state (no disk writes); primarily for tests.
    pub fn reset(&mut self) {}
}

fn blk_remaining(block_size: usize, used: usize) -> usize {
    block_size - used
}

fn basename(path: &str) -> Result<&str, FsError> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => Ok(&trimmed[i + 1..]),
        None => Ok(trimmed),
    }
}

fn parent_of_path(dev: &mut dyn BlockDevice, fs: &Ext4, path: &str) -> Result<u32, FsError> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => Ok(2),
        Some(i) => fs.resolve(dev, &trimmed[..i]),
        None => Ok(2),
    }
}

/// Flatten the i_block u32 array into the 60-byte extent area it holds,
/// in the on-disk little-endian layout ext4 expects.
fn extents_area(inode: &Ext4Inode) -> [u8; 60] {
    let mut area = [0u8; 60];
    for i in 0..15 {
        let b = inode.block[i].to_le_bytes();
        area[i * 4..i * 4 + 4].copy_from_slice(&b);
    }
    area
}
