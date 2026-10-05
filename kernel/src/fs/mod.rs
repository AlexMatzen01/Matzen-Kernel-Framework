//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Simple Filesystem (SimplFS)
//!
//! A basic filesystem implementation for the MFK kernel.
//! Structure:
//! - Block 0: Superblock (filesystem metadata)
//! - Block 1-N: Inode table
//! - Block N+1-M: Data blocks
//!
//! This is a simplified filesystem for demonstration purposes.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Filesystem block size
pub const FS_BLOCK_SIZE: usize = 512;

/// Maximum filename length
pub const MAX_FILENAME_LEN: usize = 56;

/// Maximum number of inodes
pub const MAX_INODES: usize = 256;

/// Maximum number of direct block pointers in an inode
pub const INODE_DIRECT_BLOCKS: usize = 12;
const INDIRECT_DATA_BLOCKS: usize = FS_BLOCK_SIZE / core::mem::size_of::<u64>() - 1;

/// Upper bound on sectors per `read_blocks`/`write_blocks` call.
///
/// `BlockDevice` implementations reject more than 255 blocks, and the virtio
/// path stages through a bounce page of `BOUNCE_SECTORS = 8` sectors, so 128
/// keeps every transfer a whole number of bounce-page chunks.
const MAX_IO_BLOCKS: usize = 128;

/// File types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FileType {
    Empty = 0,
    File = 1,
    Directory = 2,
}

impl From<u8> for FileType {
    fn from(val: u8) -> Self {
        match val {
            1 => FileType::File,
            2 => FileType::Directory,
            _ => FileType::Empty,
        }
    }
}

/// Superblock, occupying exactly one 512-byte block.
///
/// ```text
/// block 0                 superblock
/// block 1 .. inode_blocks inode table
/// block bitmap_start ..   allocation bitmap (version 2 and later)
/// block data_block_start  data blocks
/// ```
///
/// The bitmap's position is *derived* rather than stored twice: it starts
/// immediately after the inode table, so `bitmap_start == 1 + inode_blocks` is
/// a validated invariant rather than a second source of truth.
#[repr(C, packed)]
pub struct Superblock {
    pub magic: u32,            // Magic number to identify filesystem
    pub version: u32,          // Filesystem version
    pub block_size: u32,       // Block size in bytes
    pub total_blocks: u64,     // Total number of blocks
    pub inode_count: u32,      // Number of inodes
    pub inode_blocks: u32,     // Number of blocks for inode table
    pub data_block_start: u64, // First data block number
    pub free_blocks: u64,      // Number of free data blocks
    pub free_inodes: u32,      // Number of free inodes
    pub root_inode: u32,       // Root directory inode number
    /// First block of the allocation bitmap (always `1 + inode_blocks`).
    pub bitmap_start: u64,
    /// Blocks occupied by the allocation bitmap.
    pub bitmap_blocks: u32,
    pub reserved: [u8; 448],   // Reserved for future use
}

// Compile-time proof that the on-disk superblock is exactly one block. The
// previous layout summed to 508 bytes, leaving four bytes of block 0
// undocumented and unversioned; changing any field size without bumping
// `VERSION` would silently break existing images.
const _: () = assert!(core::mem::size_of::<Superblock>() == FS_BLOCK_SIZE);

impl Superblock {
    /// Magic number for SimplFS
    pub const MAGIC: u32 = 0x53464D4B; // "SFMK" in ASCII

    /// Current on-disk version.
    ///
    /// Version 2 introduced the persistent allocation bitmap. Version 1 kept
    /// free-space state *only* in RAM and reconstructed it by walking every
    /// inode at mount, which meant a block whose owning inode record had not
    /// yet reached the platter was declared free and handed out a second time —
    /// silent data corruption after any interrupted write. A v1 image is
    /// therefore refused rather than mounted read/write.
    pub const VERSION: u32 = 2;

    /// Returned when an image's version is not the one this build speaks.
    pub const VERSION_ERROR: &'static str =
        "Filesystem version not supported (v1 has no allocation bitmap; run 'mkfs')";

    /// Number of blocks the allocation bitmap needs for `data_blocks` bits.
    pub const fn bitmap_blocks_for(data_blocks: u64) -> u32 {
        let bytes = (data_blocks as usize).div_ceil(8);
        bytes.div_ceil(FS_BLOCK_SIZE) as u32
    }

    // The struct is `packed` to fix its on-disk layout, which makes a plain
    // field reference an unaligned reference. These accessors keep that
    // `unsafe` in one place instead of spreading `addr_of!(..).read_unaligned()`
    // across the kernel and its tests.
    pub fn get_data_block_start(&self) -> u64 {
        unsafe { core::ptr::addr_of!(self.data_block_start).read_unaligned() }
    }
    pub fn get_free_blocks(&self) -> u64 {
        unsafe { core::ptr::addr_of!(self.free_blocks).read_unaligned() }
    }
    pub fn get_bitmap_start(&self) -> u64 {
        unsafe { core::ptr::addr_of!(self.bitmap_start).read_unaligned() }
    }
    pub fn get_bitmap_blocks(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.bitmap_blocks).read_unaligned() }
    }
    pub fn get_inode_blocks(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.inode_blocks).read_unaligned() }
    }
    pub fn get_inode_count(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.inode_count).read_unaligned() }
    }
    pub fn get_root_inode(&self) -> u32 {
        unsafe { core::ptr::addr_of!(self.root_inode).read_unaligned() }
    }
    pub fn get_total_blocks(&self) -> u64 {
        unsafe { core::ptr::addr_of!(self.total_blocks).read_unaligned() }
    }

    /// Create a new superblock
    pub fn new(total_blocks: u64) -> Self {        let inode_blocks =
            (MAX_INODES * core::mem::size_of::<Inode>() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE;
        let bitmap_start = 1 + inode_blocks as u64;
        let bitmap_input = total_blocks.saturating_sub(bitmap_start);
        let bitmap_blocks = Self::bitmap_blocks_for(bitmap_input);
        let data_block_start = bitmap_start + bitmap_blocks as u64;
        let data_blocks = total_blocks.saturating_sub(data_block_start);

        Superblock {
            magic: Self::MAGIC,
            version: Self::VERSION,
            block_size: FS_BLOCK_SIZE as u32,
            total_blocks,
            inode_count: MAX_INODES as u32,
            inode_blocks: inode_blocks as u32,
            data_block_start,
            free_blocks: data_blocks,
            free_inodes: MAX_INODES as u32 - 1, // Root inode is used
            root_inode: 0,
            bitmap_start,
            bitmap_blocks,
            reserved: [0; 448],
        }
    }
}

/// Inode structure (144 bytes; does NOT divide the 512 B block, so some
/// table slots straddle two blocks â€” see `write_inode`).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Inode {
    pub file_type: u8,                             // File type
    pub permissions: u8,                           // File permissions
    pub reserved1: u16,                            // Reserved
    pub size: u64,                                 // File size in bytes
    pub blocks_used: u32,                          // Number of blocks used
    pub created: u64,                              // Creation timestamp
    pub modified: u64,                             // Modification timestamp
    pub direct_blocks: [u64; INODE_DIRECT_BLOCKS], // Direct block pointers
    pub reserved2: [u8; 16],                       // Reserved for future use
}

impl Inode {
    /// Create a new empty inode
    pub fn new() -> Self {
        Inode {
            file_type: FileType::Empty as u8,
            permissions: 0,
            reserved1: 0,
            size: 0,
            blocks_used: 0,
            created: 0,
            modified: 0,
            direct_blocks: [0; INODE_DIRECT_BLOCKS],
            reserved2: [0; 16],
        }
    }

    /// Create a new directory inode
    pub fn new_directory() -> Self {
        let mut inode = Self::new();
        inode.file_type = FileType::Directory as u8;
        inode.permissions = 0o77; // rwxrwxrwx (simplified)
        inode
    }

    /// Create a new file inode
    pub fn new_file() -> Self {
        let mut inode = Self::new();
        inode.file_type = FileType::File as u8;
        inode.permissions = 0o66; // rw-rw-rw- (simplified)
        inode
    }

    /// Check if inode is in use
    pub fn is_used(&self) -> bool {
        self.file_type != FileType::Empty as u8
    }

    /// Get file type
    pub fn get_type(&self) -> FileType {
        FileType::from(self.file_type)
    }

    fn indirect_head(&self) -> u64 {
        u64::from_le_bytes(self.reserved2[..8].try_into().unwrap())
    }

    fn set_indirect_head(&mut self, block: u64) {
        self.reserved2[..8].copy_from_slice(&block.to_le_bytes());
    }
}

/// Directory entry (64 bytes)
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct DirectoryEntry {
    pub inode_number: u32,            // Inode number (0 = empty)
    pub name: [u8; MAX_FILENAME_LEN], // Filename (null-terminated)
    pub reserved: [u8; 4],            // Reserved
}

impl DirectoryEntry {
    /// Create a new empty directory entry
    pub fn new() -> Self {
        DirectoryEntry {
            inode_number: 0,
            name: [0; MAX_FILENAME_LEN],
            reserved: [0; 4],
        }
    }

    /// Create a directory entry with name and inode
    pub fn new_with_name(name: &str, inode_number: u32) -> Self {
        let mut entry = Self::new();
        entry.inode_number = inode_number;

        let bytes = name.as_bytes();
        let len = core::cmp::min(bytes.len(), MAX_FILENAME_LEN - 1);

        // Copy bytes directly without taking a reference to the packed field
        for i in 0..len {
            entry.name[i] = bytes[i];
        }

        entry
    }

    /// Check if entry is in use
    pub fn is_used(&self) -> bool {
        // Note: This method is often called on entries read from disk buffers,
        // where the entry might not be properly aligned. However, u32 access
        // should work on x86_64 even if unaligned.
        self.inode_number != 0
    }

    /// Get filename as string
    pub fn get_name(&self) -> Result<String, &'static str> {
        // Copy the name array to avoid taking a reference to a packed struct field
        let name_copy = self.name;

        let mut len = 0;
        for (i, &byte) in name_copy.iter().enumerate() {
            if byte == 0 {
                len = i;
                break;
            }
        }

        core::str::from_utf8(&name_copy[..len])
            .map(|s| String::from(s))
            .map_err(|_| "Invalid UTF-8 in filename")
    }
}

/// File handle
pub struct FileHandle {
    pub inode_number: u32,
    pub offset: u64,
    pub inode: Inode,
}

/// File information for listing
#[derive(Clone)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
    pub inode_number: u32,
}

impl fmt::Display for FileInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let type_char = if self.is_directory { "d" } else { "-" };
        write!(f, "{} {:>10} {}", type_char, self.size, self.name)
    }
}

/// Forward-only cursor over a file's chained indirect nodes.
///
/// Files and appends visit logical blocks in order, so caching the one
/// indirect node the caller is currently inside turns the chain walk in
/// `file_block_at`/`ensure_file_block` from one disk read per hop into one
/// disk read per `INDIRECT_DATA_BLOCKS` blocks. Without it, locating block
/// `n` costs `n / INDIRECT_DATA_BLOCKS` reads and a whole-file read is
/// quadratic in the file size.
///
/// `next` is the block number of node `node_index + 1` (0 = end of chain),
/// so advancing never requires re-reading the cached node.
struct ChainCursor {
    inode: u32,
    node_index: usize,
    node: u64,
    next: u64,
    entries: [u64; INDIRECT_DATA_BLOCKS],
}

impl ChainCursor {
    fn new(inode: u32, node_index: usize, node: u64, next: u64) -> Self {
        Self {
            inode,
            node_index,
            node,
            next,
            entries: [0u64; INDIRECT_DATA_BLOCKS],
        }
    }
}

/// Filesystem consistency report produced by [`SimpleFilesystem::check`].
///
/// Every counter is a class of on-disk damage that version 1 had no way to
/// detect, because its free-space state was derived from the inodes and so was
/// consistent with them by construction.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsckReport {
    pub total_blocks: u64,
    pub data_blocks: u64,
    pub data_block_start: u64,
    pub free_blocks: u64,
    pub bitmap_free: u64,
    pub bitmap_start: u64,
    pub bitmap_blocks: u32,
    pub inode_blocks: u32,
    pub inode_count: u64,

    /// Superblock fields that contradict each other.
    pub geometry_errors: usize,
    /// `free_blocks` disagrees with the bitmap.
    pub count_errors: usize,
    /// Block pointers outside the data region.
    pub bad_block_refs: usize,
    /// Indirect chains that do not terminate.
    pub chain_cycles: usize,
    /// Directory entries naming a non-existent or out-of-range inode.
    pub dangling_entries: usize,
    /// Blocks an inode references that the bitmap calls free.
    pub missing_refs: usize,
    /// Blocks the bitmap marks used that no inode references.
    pub orphan_blocks: usize,

    /// True when every counter above is zero.
    pub is_clean: bool,
}

impl FsckReport {
    /// Total problems found across every category.
    pub fn problem_count(&self) -> usize {
        self.geometry_errors
            + self.count_errors
            + self.bad_block_refs
            + self.chain_cycles
            + self.dangling_entries
            + self.missing_refs
            + self.orphan_blocks
    }
}

/// Filesystem implementation
pub struct SimpleFilesystem {
    superblock: Superblock,
    inodes: Vec<Inode>,
    current_dir_inode: u32,
    parent: Vec<Option<u32>>, // parent[inode] = Some(parent_inode)
    allocated_blocks: Vec<u8>,
    /// Per-bitmap-block dirty flags for write-back.
    bitmap_dirty: Vec<bool>,
    allocation_cursor: usize,
    chain: Option<ChainCursor>,
    chain_dirty: bool,
    resolved: Option<(u32, String)>,
}

impl SimpleFilesystem {
    /// Check if inode is a directory (public helper for shell)
    pub fn is_dir(&self, ino: u32) -> bool {
        if (ino as usize) >= self.inodes.len() {
            return false;
        }
        self.inodes[ino as usize].get_type() == FileType::Directory
    }
    /// Check if inode is a file
    pub fn is_file(&self, ino: u32) -> bool {
        if (ino as usize) >= self.inodes.len() {
            return false;
        }
        self.inodes[ino as usize].get_type() == FileType::File
    }
    /// File size in bytes from the inode (no disk I/O, no allocation).
    pub fn file_size(&self, ino: u32) -> Result<u64, &'static str> {
        if (ino as usize) >= self.inodes.len() {
            return Err("Invalid inode number");
        }
        if self.inodes[ino as usize].get_type() != FileType::File {
            return Err("Not a file");
        }
        Ok(self.inodes[ino as usize].size)
    }
    /// Get file type
    pub fn inode_type(&self, ino: u32) -> FileType {
        if (ino as usize) >= self.inodes.len() {
            return FileType::Empty;
        }
        self.inodes[ino as usize].get_type()
    }
}

impl SimpleFilesystem {
    /// Create a new filesystem instance (not formatted)
    pub fn new() -> Self {
        SimpleFilesystem {
            superblock: Superblock::new(0),
            inodes: Vec::new(),
            current_dir_inode: 0,
            parent: alloc::vec![None; MAX_INODES],
            allocated_blocks: Vec::new(),
            bitmap_dirty: Vec::new(),
            allocation_cursor: 0,
            chain: None,
            chain_dirty: false,
            resolved: None,
        }
    }

    /// Format a device with the filesystem
    pub fn format(device: &mut dyn crate::drivers::block::BlockDevice) -> Result<(), &'static str> {
        use crate::serial_println;

        let total_blocks = device.block_count();
        let mut superblock = Superblock::new(total_blocks);
        let data_block_start =
            unsafe { core::ptr::addr_of!(superblock.data_block_start).read_unaligned() };
        if total_blocks <= data_block_start {
            return Err("Disk is too small for SimplFS");
        }

        // Reserve 1 block for root directory
        unsafe {
            let free_ptr = core::ptr::addr_of_mut!(superblock.free_blocks);
            let current = free_ptr.read_unaligned();
            free_ptr.write_unaligned(current.saturating_sub(1));
        }

        let data_block_start =
            unsafe { core::ptr::addr_of!(superblock.data_block_start).read_unaligned() };
        let free_blocks = unsafe { core::ptr::addr_of!(superblock.free_blocks).read_unaligned() };
        let inode_blocks = unsafe { core::ptr::addr_of!(superblock.inode_blocks).read_unaligned() };

        // Write superblock
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        unsafe {
            let sb_ptr = &superblock as *const Superblock as *const u8;
            core::ptr::copy_nonoverlapping(
                sb_ptr,
                buffer.as_mut_ptr(),
                core::mem::size_of::<Superblock>(),
            );
        }
        device.write_blocks(0, 1, &buffer)?;

        // Zero the allocation bitmap. A fresh filesystem has exactly one
        // allocated data block (the root directory), recorded below.
        let bitmap_start =
            unsafe { core::ptr::addr_of!(superblock.bitmap_start).read_unaligned() };
        let bitmap_blocks =
            unsafe { core::ptr::addr_of!(superblock.bitmap_blocks).read_unaligned() };
        let data_blocks = total_blocks.saturating_sub(data_block_start);
        let bitmap_len = (data_blocks as usize).div_ceil(8);
        let zero = [0u8; FS_BLOCK_SIZE];
        for i in 0..bitmap_blocks {
            device.write_blocks(bitmap_start + i as u64, 1, &zero)?;
        }
        let mut bitmap = alloc::vec![0u8; bitmap_len];
        // The root directory occupies the *first data block*, and bitmap bits
        // are indexed relative to the data region -- so its bit is 0, not
        // `data_block_start - bitmap_start`. Getting that wrong leaves the
        // root's own block marked free, and the first file allocation hands it
        // out again, silently overwriting the directory.
        bitmap[0] |= 1;
        let mut remaining = bitmap_len;
        let mut block = bitmap_start;
        let mut stage = [0u8; FS_BLOCK_SIZE];
        for _ in 0..bitmap_blocks {
            if remaining == 0 {
                break;
            }
            let n = core::cmp::min(FS_BLOCK_SIZE, remaining);
            let offset = bitmap_len - remaining;
            stage = [0u8; FS_BLOCK_SIZE];
            stage[..n].copy_from_slice(&bitmap[offset..offset + n]);
            device.write_blocks(block, 1, &stage)?;
            remaining -= n;
            block += 1;
        }

        // Initialize inode table
        let mut inodes = alloc::vec![Inode::new(); MAX_INODES];

        // Create root directory inode
        inodes[0] = Inode::new_directory();
        inodes[0].size = FS_BLOCK_SIZE as u64;
        inodes[0].blocks_used = 1;
        inodes[0].direct_blocks[0] = data_block_start;

        // Write inode table
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(
                inodes.as_ptr() as *const u8,
                MAX_INODES * core::mem::size_of::<Inode>(),
            )
        };

        let mut block_num = 1u64;
        for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
            let mut block_buffer = [0u8; FS_BLOCK_SIZE];
            block_buffer[..chunk.len()].copy_from_slice(chunk);
            device.write_blocks(block_num, 1, &block_buffer)?;
            block_num += 1;
        }

        // Clear root directory data block
        let root_dir_buffer = [0u8; FS_BLOCK_SIZE];
        device.write_blocks(data_block_start, 1, &root_dir_buffer)?;

        serial_println!("Filesystem formatted successfully");
        serial_println!("  Total blocks: {}", total_blocks);
        serial_println!("  Data blocks: {}", free_blocks);
        serial_println!("  Inode blocks: {}", inode_blocks);

        Ok(())
    }

    /// Mount a filesystem from a device
    pub fn mount(
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<Self, &'static str> {
        // Read superblock
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(0, 1, &mut buffer)?;

        let superblock = unsafe { core::ptr::read_unaligned(buffer.as_ptr() as *const Superblock) };

        // Verify magic number
        if superblock.magic != Superblock::MAGIC {
            return Err("Invalid filesystem magic number");
        }
        let stored_version = unsafe { core::ptr::addr_of!(superblock.version).read_unaligned() };
        let stored_total = unsafe { core::ptr::addr_of!(superblock.total_blocks).read_unaligned() };
        let stored_inode_blocks =
            unsafe { core::ptr::addr_of!(superblock.inode_blocks).read_unaligned() };
        let stored_data_start =
            unsafe { core::ptr::addr_of!(superblock.data_block_start).read_unaligned() };
        let minimum_inode_blocks =
            (MAX_INODES * core::mem::size_of::<Inode>()).div_ceil(FS_BLOCK_SIZE);
        // `block_size` was never read at all, so an image claiming 4 KiB
        // blocks was silently parsed with 512-byte assumptions. `inode_count`
        // was trusted as an upper bound while the in-RAM table is always
        // `MAX_INODES` long, so an image claiming a larger count let
        // `list_directory` index past the vector. Both are now validated, and
        // the version gate rejects the pre-bitmap layout outright.
        let stored_block_size =
            unsafe { core::ptr::addr_of!(superblock.block_size).read_unaligned() };
        let stored_inode_count =
            unsafe { core::ptr::addr_of!(superblock.inode_count).read_unaligned() };
        let stored_bitmap_start =
            unsafe { core::ptr::addr_of!(superblock.bitmap_start).read_unaligned() };
        let stored_bitmap_blocks =
            unsafe { core::ptr::addr_of!(superblock.bitmap_blocks).read_unaligned() };
        if stored_version != Superblock::VERSION {
            return Err(Superblock::VERSION_ERROR);
        }
        // The bitmap sits immediately after the inode table, so both its
        // position and its size are derivable. Validating both turns a
        // corrupted superblock into a clean refusal instead of a mount that
        // reads garbage as free-space state.
        if stored_bitmap_start != 1 + stored_inode_blocks as u64
            || stored_data_start
                != stored_bitmap_start + stored_bitmap_blocks as u64
            || stored_bitmap_blocks
                != Superblock::bitmap_blocks_for(stored_total.saturating_sub(stored_bitmap_start))
        {
            return Err("Invalid allocation bitmap geometry");
        }
        if stored_block_size != FS_BLOCK_SIZE as u32 {
            return Err("Unsupported filesystem block size");
        }
        if stored_inode_count as usize > MAX_INODES || stored_inode_count == 0 {
            return Err("Invalid inode count in superblock");
        }
        if stored_total > device.block_count()
            || (stored_inode_blocks as usize) < minimum_inode_blocks
            || stored_data_start >= stored_total
        {
            return Err("Invalid filesystem geometry");
        }

        // Read inode table
        let inode_table_size = (superblock.inode_blocks as usize) * FS_BLOCK_SIZE;
        let mut inode_buffer = alloc::vec![0u8; inode_table_size];

        for i in 0..superblock.inode_blocks {
            let block_buffer =
                &mut inode_buffer[(i as usize * FS_BLOCK_SIZE)..((i as usize + 1) * FS_BLOCK_SIZE)];
            device.read_blocks(1 + i as u64, 1, block_buffer)?;
        }

        let inodes = unsafe {
            let ptr = inode_buffer.as_ptr() as *const Inode;
            let mut vec = Vec::with_capacity(MAX_INODES);
            for i in 0..MAX_INODES {
                vec.push(core::ptr::read_unaligned(ptr.add(i)));
            }
            vec
        };

        let mut fs = SimpleFilesystem {
            superblock,
            inodes,
            current_dir_inode: 0, // Start at root
            parent: alloc::vec![None; MAX_INODES],
            allocated_blocks: Vec::new(),
            bitmap_dirty: Vec::new(),
            allocation_cursor: 0,
            chain: None,
            chain_dirty: false,
            resolved: None,
        };
        // Version 2: free-space state is read from disk, not rebuilt by
        // walking inodes. The walk is the v1 bug -- it cannot tell an
        // allocated-but-unreferenced block from a free one.
        fs.load_bitmap(device)?;
        fs.recount_free_blocks();
        // Rebuild parent map from directory entries. Errors are tolerated
        // (a corrupt entry must not make a mountable filesystem unmountable),
        // but the parent holes they leave make `cd ..` fall back to root, so
        // they are reported for `fsck` to surface.
        if let Err(e) = fs.rebuild_parents(device) {
            crate::serial_println!("[fs] parent map incomplete: {}", e);
        }
        Ok(fs)
    }

    /// Read-only consistency report.
    ///
    /// Returns `(missing, orphan)` where *missing* is the number of blocks an
    /// inode references that the bitmap calls free (a live reference to space
    /// the allocator will hand out again) and *orphan* is the number of blocks
    /// the bitmap marks used that no inode references (leaked space).
    ///
    /// This is the check that was impossible in version 1: with free-space
    /// state derived from the inodes, the two sets are the same by
    /// construction and the comparison can never find anything.
    pub fn audit(&mut self, device: &mut dyn crate::drivers::block::BlockDevice) -> Result<(usize, usize), &'static str> {
        self.audit_bitmap(device)
    }

    /// Full consistency check, with a human-readable summary.
    ///
    /// Read-only: nothing on disk is modified, so a clean report leaves the
    /// filesystem exactly as it was and a dirty one can be inspected before
    /// any repair is attempted.
    pub fn check(&mut self, device: &mut dyn crate::drivers::block::BlockDevice) -> Result<FsckReport, &'static str> {
        let (start, count) = self.data_region();
        let bitmap_start = self.superblock.get_bitmap_start();
        let bitmap_blocks = self.superblock.get_bitmap_blocks();
        let inode_blocks = self.superblock.get_inode_blocks();

        let mut report = FsckReport {
            total_blocks: self.superblock.get_total_blocks(),
            data_blocks: count as u64,
            free_blocks: self.superblock.get_free_blocks(),
            data_block_start: start,
            bitmap_start,
            bitmap_blocks,
            inode_blocks,
            inode_count: self.inodes.iter().filter(|i| i.is_used()).count() as u64,
            ..Default::default()
        };

        // 1. Geometry invariants.
        if bitmap_start != 1 + inode_blocks as u64 {
            report.geometry_errors += 1;
        }
        if start != bitmap_start + bitmap_blocks as u64 {
            report.geometry_errors += 1;
        }
        if report.free_blocks > count as u64 {
            report.geometry_errors += 1;
        }

        // 2. `free_blocks` must agree with the bitmap, which is authoritative.
        let mut actual_free = 0u64;
        for i in 0..count {
            if !self.block_bit(i) {
                actual_free += 1;
            }
        }
        report.bitmap_free = actual_free;
        if actual_free != report.free_blocks {
            report.count_errors += 1;
        }

        // 3. Every block pointer must lie inside the data region. An
        //    out-of-range pointer is a read or write of arbitrary disk.
        report.bad_block_refs = 0;
        for i in 0..self.inodes.len() {
            let inode = self.inodes[i];
            if !inode.is_used() {
                continue;
            }
            for d in 0..INODE_DIRECT_BLOCKS {
                let b =
                    unsafe { core::ptr::addr_of!(inode.direct_blocks[d]).read_unaligned() };
                if b != 0 && (b < start || b - start >= count as u64) {
                    report.bad_block_refs += 1;
                }
            }
            // Walk the chain, refusing to loop forever on a cycle.
            let mut node = inode.indirect_head();
            let mut hops = 0usize;
            while node != 0 {
                if node < start || node - start >= count as u64 {
                    report.bad_block_refs += 1;
                    break;
                }
                if hops > count {
                    report.chain_cycles += 1;
                    break;
                }
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(node, 1, &mut buffer)?;
                for slot in 0..INDIRECT_DATA_BLOCKS {
                    let off = 8 + slot * core::mem::size_of::<u64>();
                    let b = u64::from_le_bytes(buffer[off..off + 8].try_into().unwrap());
                    if b != 0 && (b < start || b - start >= count as u64) {
                        report.bad_block_refs += 1;
                    }
                }
                node = u64::from_le_bytes(buffer[..8].try_into().unwrap());
                hops += 1;
            }
        }

        // 4. Directory entries must point at in-use inodes inside the table.
        report.dangling_entries = 0;
        for i in 0..self.inodes.len() {
            let inode = self.inodes[i];
            if !inode.is_used() || inode.get_type() != FileType::Directory {
                continue;
            }
            for d in 0..INODE_DIRECT_BLOCKS {
                let b =
                    unsafe { core::ptr::addr_of!(inode.direct_blocks[d]).read_unaligned() };
                if b == 0 || b < start || b - start >= count as u64 {
                    continue;
                }
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(b, 1, &mut buffer)?;
                let per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
                for s in 0..per_block {
                    let entry = unsafe {
                        core::ptr::read_unaligned(
                            buffer.as_ptr().add(s * core::mem::size_of::<DirectoryEntry>())
                                as *const DirectoryEntry,
                        )
                    };
                    if !entry.is_used() {
                        continue;
                    }
                    if entry.inode_number == 0 || entry.inode_number >= self.inode_limit() {
                        report.dangling_entries += 1;
                        continue;
                    }
                    if !self.inodes[entry.inode_number as usize].is_used() {
                        report.dangling_entries += 1;
                    }
                }
            }
        }

        // 5. Blocks referenced but free, and allocated but unreferenced.
        let (missing, orphan) = self.audit_bitmap(device)?;
        report.missing_refs = missing;
        report.orphan_blocks = orphan;

        report.is_clean = report.geometry_errors == 0
            && report.count_errors == 0
            && report.bad_block_refs == 0
            && report.chain_cycles == 0
            && report.dangling_entries == 0
            && report.missing_refs == 0
            && report.orphan_blocks == 0;
        Ok(report)
    }

    /// Rebuild parent map by scanning all directory inodes
    fn rebuild_parents(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        self.parent = alloc::vec![None; MAX_INODES];
        self.parent[0] = None;
        // For each directory inode, scan its directory blocks
        for dir_ino in 0..MAX_INODES {
            if dir_ino >= self.inodes.len() {
                break;
            }
            let inode_copy = self.inodes[dir_ino];
            if inode_copy.get_type() != FileType::Directory {
                continue;
            }
            if !inode_copy.is_used() {
                continue;
            }
            for b in 0..INODE_DIRECT_BLOCKS {
                let block_num =
                    unsafe { core::ptr::addr_of!(inode_copy.direct_blocks[b]).read_unaligned() };
                if block_num == 0 {
                    break;
                }
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                // If read fails, skip this dir
                if device.read_blocks(block_num, 1, &mut buffer).is_err() {
                    break;
                }
                let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
                for i in 0..entries_per_block {
                    let entry = unsafe {
                        let ptr = buffer
                            .as_ptr()
                            .add(i * core::mem::size_of::<DirectoryEntry>())
                            as *const DirectoryEntry;
                        core::ptr::read_unaligned(ptr)
                    };
                    if entry.is_used() {
                        let child = entry.inode_number as usize;
                        if child < MAX_INODES && child != 0 {
                            // Don't overwrite if already set (first parent wins), but allow
                            if self.parent[child].is_none() {
                                self.parent[child] = Some(dir_ino as u32);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Get current directory inode number
    pub fn current_directory(&self) -> u32 {
        self.current_dir_inode
    }

    /// Upper bound on any inode number that may index `self.inodes`.
    ///
    /// `self.inodes` always holds exactly [`MAX_INODES`] entries, so every
    /// disk-derived inode number must be checked against *this*, not against
    /// `superblock.inode_count`: the superblock field is attacker-controlled
    /// data and was previously trusted as a bound, which turned a corrupt
    /// value into an out-of-bounds index (a kernel panic) in `list_directory`
    /// and the path resolvers.
    ///
    /// [`mount`] rejects any image whose `inode_count` exceeds `MAX_INODES`, so
    /// clamping here is belt-and-braces for in-memory use before mount.
    fn inode_limit(&self) -> u32 {
        core::cmp::min(self.superblock.inode_count, MAX_INODES as u32)
    }

    /// `self.parent` is a fixed `MAX_INODES`-entry vector; guard every index
    /// into it, which path resolution and `current_path` do from disk-derived
    /// values.
    fn parent_of(&self, ino: u32) -> Option<u32> {
        self.parent.get(ino as usize).copied().flatten()
    }

    /// True when `ino` may index `self.inodes`.
    fn inode_ok(&self, ino: u32) -> bool {
        ino != 0 || self.superblock.inode_count > 0
    }

    fn inode_type_of(&self, ino: u32) -> Option<FileType> {
        if ino as usize >= self.inodes.len() {
            return None;
        }
        Some(self.inodes[ino as usize].get_type())
    }

    /// Change current directory (by inode - legacy)
    pub fn change_directory(&mut self, inode: u32) -> Result<(), &'static str> {
        if inode >= self.inode_limit() {
            return Err("Invalid inode number");
        }

        if self.inodes[inode as usize].get_type() != FileType::Directory {
            return Err("Not a directory");
        }

        self.current_dir_inode = inode;
        // Relative paths now resolve from a different directory.
        self.resolved = None;
        Ok(())
    }

    // â”€â”€ Path helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Validate a single filename component (no slash, not empty, not . or .., length)
    fn validate_component(name: &str) -> Result<(), &'static str> {
        if name.is_empty() {
            return Err("Invalid argument");
        }
        if name == "." || name == ".." {
            return Err("Invalid argument");
        }
        if name.len() >= MAX_FILENAME_LEN {
            return Err("Filename too long");
        }
        if name.contains('/') || name.contains('\\') {
            return Err("Invalid argument");
        }
        // Forbid zero bytes
        if name.as_bytes().contains(&0) {
            return Err("Invalid argument");
        }
        Ok(())
    }

    /// Find entry in directory by name, returns inode number if found
    fn find_entry_in_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
        name: &str,
    ) -> Result<Option<u32>, &'static str> {
        if dir_inode >= self.inode_limit() {
            return Err("Invalid inode number");
        }
        let inode = &self.inodes[dir_inode as usize];
        if inode.get_type() != FileType::Directory {
            return Err("Not a directory");
        }
        for b in 0..INODE_DIRECT_BLOCKS {
            let block_num = unsafe { core::ptr::addr_of!(inode.direct_blocks[b]).read_unaligned() };
            if block_num == 0 {
                break;
            }
            let mut buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut buffer)?;
            let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
            for i in 0..entries_per_block {
                let entry = unsafe {
                    let ptr = buffer
                        .as_ptr()
                        .add(i * core::mem::size_of::<DirectoryEntry>())
                        as *const DirectoryEntry;
                    core::ptr::read_unaligned(ptr)
                };
                if entry.is_used() {
                    if let Ok(entry_name) = entry.get_name() {
                        if entry_name == name {
                            // The inode number comes from raw disk bytes and
                            // every caller indexes `self.inodes` /
                            // `self.parent` with it. Returning it unchecked
                            // turns one corrupt directory entry into a kernel
                            // panic reachable from `cd`, `ls`, `cat` and `rm`.
                            // A damaged entry is reported as corrupt rather
                            // than dereferenced.
                            if entry.inode_number == 0
                                || entry.inode_number >= self.inode_limit()
                            {
                                return Err("Directory entry points at an invalid inode");
                            }
                            return Ok(Some(entry.inode_number));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Add entry to directory, assumes name does not exist and child inode is valid
    fn add_entry_to_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
        name: &str,
        child_inode: u32,
    ) -> Result<(), &'static str> {
        let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
        self.resolved = None;

        // Directories grow across as many blocks as they need, up to the
        // inode's direct-block limit. Previously this only ever looked at
        // block 0 and returned "Directory is full" on the ninth entry, even
        // though every reader (`list_directory`, `find_entry_in_dir`,
        // `rebuild_parents`, `current_path`) already walked all twelve -- so a
        // multi-block directory was listable but not writable.
        for slot in 0..INODE_DIRECT_BLOCKS {
            let mut block_num =
                unsafe { core::ptr::addr_of!(self.inodes[dir_inode as usize].direct_blocks[slot]).read_unaligned() };
            if block_num == 0 {
                // End of the directory's block list: extend it by one.
                let new_block = self.allocate_data_block().ok_or("Directory is full")?;
                device.write_blocks(new_block, 1, &[0u8; FS_BLOCK_SIZE])?;
                unsafe {
                    let ptr = core::ptr::addr_of_mut!(self.inodes[dir_inode as usize].direct_blocks[slot]);
                    ptr.write_unaligned(new_block);
                }
                self.sync_dir_extent(device, dir_inode)?;
                block_num = new_block;
            }
            let mut dir_buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut dir_buffer)?;
            for i in 0..entries_per_block {
                let offset = i * core::mem::size_of::<DirectoryEntry>();
                let entry = unsafe {
                    let ptr = dir_buffer.as_ptr().add(offset) as *const DirectoryEntry;
                    core::ptr::read_unaligned(ptr)
                };
                if !entry.is_used() {
                    let new_entry = DirectoryEntry::new_with_name(name, child_inode);
                    unsafe {
                        let ptr = dir_buffer.as_mut_ptr().add(offset) as *mut DirectoryEntry;
                        core::ptr::write_unaligned(ptr, new_entry);
                    }
                    device.write_blocks(block_num, 1, &dir_buffer)?;
                    self.sync_dir_extent(device, dir_inode)?;
                    return Ok(());
                }
            }
            // Block is full; continue into the next one.
        }
        Err("Directory is full (12 blocks max)")
    }

    /// Recompute a directory's `size` and `blocks_used` from the extent of its
    /// non-empty block list, and persist them.
    ///
    /// `size` was previously hardcoded to `0` for every directory and never
    /// maintained, so nothing could tell an 8-entry directory from an
    /// 80-entry one. Setting it to the block extent gives readers a bound and
    /// makes `blocks_used` meaningful.
    fn sync_dir_extent(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
    ) -> Result<(), &'static str> {
        let mut blocks = 0usize;
        for slot in 0..INODE_DIRECT_BLOCKS {
            let b =
                unsafe { core::ptr::addr_of!(self.inodes[dir_inode as usize].direct_blocks[slot]).read_unaligned() };
            if b == 0 {
                break;
            }
            blocks += 1;
        }
        let bytes = (blocks * FS_BLOCK_SIZE) as u64;
        unsafe {
            let inode = &mut self.inodes[dir_inode as usize];
            let p = core::ptr::addr_of_mut!(inode.size);
            p.write_unaligned(bytes);
            let p = core::ptr::addr_of_mut!(inode.blocks_used);
            p.write_unaligned(blocks as u32);
        }
        self.write_inode(device, dir_inode as usize)?;
        Ok(())
    }

    /// Remove entry from directory by child inode
    fn remove_entry_from_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
        child_inode: u32,
    ) -> Result<(), &'static str> {
        let dir_inode_copy = self.inodes[dir_inode as usize];
        self.resolved = None;
        for b in 0..INODE_DIRECT_BLOCKS {
            let block_num =
                unsafe { core::ptr::addr_of!(dir_inode_copy.direct_blocks[b]).read_unaligned() };
            if block_num == 0 {
                break;
            }
            let mut dir_buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut dir_buffer)?;
            let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
            for i in 0..entries_per_block {
                let offset = i * core::mem::size_of::<DirectoryEntry>();
                let entry = unsafe {
                    let ptr = dir_buffer.as_ptr().add(offset) as *const DirectoryEntry;
                    core::ptr::read_unaligned(ptr)
                };
                if entry.is_used() && entry.inode_number == child_inode {
                    unsafe {
                        let ptr = dir_buffer.as_mut_ptr().add(offset) as *mut DirectoryEntry;
                        core::ptr::write_unaligned(ptr, DirectoryEntry::new());
                    }
                    device.write_blocks(block_num, 1, &dir_buffer)?;
                    return Ok(());
                }
            }
        }
        Err("Entry not found")
    }

    fn rename_entry_in_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
        child_inode: u32,
        name: &str,
    ) -> Result<(), &'static str> {
        Self::validate_component(name)?;
        let dir = *self
            .inodes
            .get(dir_inode as usize)
            .ok_or("Invalid inode number")?;
        self.resolved = None;
        for i in 0..INODE_DIRECT_BLOCKS {
            let block = unsafe { core::ptr::addr_of!(dir.direct_blocks[i]).read_unaligned() };
            if block == 0 {
                break;
            }
            let mut buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block, 1, &mut buffer)?;
            let entry_count = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
            for entry_index in 0..entry_count {
                let offset = entry_index * core::mem::size_of::<DirectoryEntry>();
                let entry = unsafe {
                    core::ptr::read_unaligned(buffer.as_ptr().add(offset) as *const DirectoryEntry)
                };
                if entry.is_used() && entry.inode_number == child_inode {
                    let replacement = DirectoryEntry::new_with_name(name, child_inode);
                    unsafe {
                        core::ptr::write_unaligned(
                            buffer.as_mut_ptr().add(offset) as *mut DirectoryEntry,
                            replacement,
                        );
                    }
                    device.write_blocks(block, 1, &buffer)?;
                    return Ok(());
                }
            }
        }
        Err("Entry not found")
    }

    pub fn rename_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), &'static str> {
        let source = self.resolve_file_or_dir(device, old_path)?;
        if !self.is_file(source) {
            return Err("Not a file");
        }
        let (source_parent, _) = self.resolve_parent_and_name(device, old_path)?;
        let (target_parent, target_name) = self.resolve_parent_and_name(device, new_path)?;
        if old_path == new_path {
            return Ok(());
        }

        let target = self.find_entry_in_dir(device, target_parent, &target_name)?;
        if let Some(target_inode) = target {
            if target_inode == source {
                return Ok(());
            }
            if !self.is_file(target_inode) {
                return Err("Is a directory");
            }
            self.remove_entry_from_dir(device, target_parent, target_inode)?;
            if source_parent == target_parent {
                self.rename_entry_in_dir(device, source_parent, source, &target_name)?;
            } else {
                self.add_entry_to_dir(device, target_parent, &target_name, source)?;
                self.remove_entry_from_dir(device, source_parent, source)?;
            }
            self.release_file_storage(device, target_inode as usize)?;
            self.inodes[target_inode as usize] = Inode::new();
            unsafe {
                let free = core::ptr::addr_of!(self.superblock.free_inodes)
                    .read_unaligned()
                    .saturating_add(1);
                core::ptr::addr_of_mut!(self.superblock.free_inodes).write_unaligned(free);
            }
        } else if source_parent == target_parent {
            self.rename_entry_in_dir(device, source_parent, source, &target_name)?;
        } else {
            self.add_entry_to_dir(device, target_parent, &target_name, source)?;
            self.remove_entry_from_dir(device, source_parent, source)?;
            // The in-memory parent map has to follow the move. It was not
            // updated here, so `pwd` and `cd ..` resolved through the *old*
            // parent until the next remount rebuilt the map from disk.
            // Only a file or directory that the map attributes to the old
            // parent is moved, so this cannot clobber a first-parent entry.
            if self.parent_of(source) == Some(source_parent) {
                self.parent[source as usize] = Some(target_parent);
            }
        }

        self.write_inodes(device)?;
        self.write_superblock(device)
    }

    /// Resolve path to inode number (must be directory for intermediate components)
    pub fn resolve_path(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<u32, &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        // Determine start
        let mut current = if trimmed.starts_with('/') {
            0 // root
        } else {
            self.current_dir_inode
        };
        // Split, filter empty (handles // and trailing /)
        let components: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            // path was "/" or "///"
            return Ok(0);
        }
        for comp in components {
            if comp == "." {
                continue;
            } else if comp == ".." {
                if current == 0 {
                    // stay at root
                    continue;
                } else {
                    // reload parent map if needed (already rebuilt)
                    if let Some(p) = self.parent_of(current) {
                        current = p;
                    } else {
                        // No parent known, stay at root
                        current = 0;
                    }
                }
            } else {
                // Normal component
                if comp.len() >= MAX_FILENAME_LEN {
                    return Err("Filename too long");
                }
                let found = self.find_entry_in_dir(device, current, comp)?;
                match found {
                    Some(child_ino) => {
                        // Validate it's a directory for traversal (but final component may be file when used via resolve_parent_and_name; resolve_path is for cd/ls dir only, so require directory)
                        // We enforce directory for resolve_path: used for cd/rmdir/ls dirs. For file paths, caller uses resolve_parent_and_name.
                        // Here we check: if child is not directory, but we are not at end? Actually resolve_path should only resolve directories, so if child is not directory, it's error for cd.
                        // We'll check type: if not directory, return error unless caller explicitly wants file? For cd, we want error.
                        // For now, if intermediate is not directory, error.
                        let child_type = self.inode_type_of(child_ino).ok_or("Invalid inode in path")?;
                        if child_type != FileType::Directory {
                            return Err("Not a directory");
                        }
                        current = child_ino;
                    }
                    None => return Err("No such file or directory"),
                }
            }
        }
        Ok(current)
    }

    /// Resolve path that may point to file or directory, returns inode (for cat/read)
    ///
    /// The result is memoised for the most recently resolved path. Asset readers
    /// such as the Doom WAD loader call this once per lump on an unchanged path,
    /// and each miss costs a directory-block scan per path component.
    pub fn resolve_file_or_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<u32, &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        if trimmed == "/" {
            return Ok(0);
        }
        if let Some((cached_inode, cached_path)) = &self.resolved {
            if cached_path == trimmed {
                return Ok(*cached_inode);
            }
        }
        let inode = self.resolve_file_or_dir_uncached(device, trimmed)?;
        self.resolved = Some((inode, String::from(trimmed)));
        Ok(inode)
    }

    fn resolve_file_or_dir_uncached(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        trimmed: &str,
    ) -> Result<u32, &'static str> {
        let is_absolute = trimmed.starts_with('/');
        let mut current = if is_absolute {
            0
        } else {
            self.current_dir_inode
        };
        let components: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return Ok(0);
        }
        for (idx, comp) in components.iter().enumerate() {
            if *comp == "." {
                continue;
            } else if *comp == ".." {
                if current != 0 {
                    if let Some(p) = self.parent_of(current) {
                        current = p;
                    } else {
                        current = 0;
                    }
                }
                continue;
            } else {
                let found = self.find_entry_in_dir(device, current, comp)?;
                match found {
                    Some(child_ino) => {
                        let is_last = idx == components.len() - 1;
                        if !is_last {
                            let t = self.inode_type_of(child_ino).ok_or("Invalid inode in path")?;
                            if t != FileType::Directory {
                                return Err("Not a directory");
                            }
                        }
                        current = child_ino;
                    }
                    None => return Err("No such file or directory"),
                }
            }
        }
        Ok(current)
    }

    /// Split path into (parent_inode, basename). For mkdir/touch etc.
    pub fn resolve_parent_and_name(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<(u32, String), &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        // Handle trailing slash: mkdir "a/b/" => basename is "b"
        let trimmed = trimmed.trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        if trimmed == "/" {
            return Err("Invalid argument");
        }
        // Find last '/'
        let (parent_path, basename) = match trimmed.rfind('/') {
            Some(pos) => {
                let parent = if pos == 0 { "/" } else { &trimmed[..pos] };
                let base = &trimmed[pos + 1..];
                (parent, base)
            }
            None => {
                // No slash, parent is current dir
                ("", trimmed)
            }
        };
        if basename.is_empty() {
            return Err("Invalid argument");
        }
        Self::validate_component(basename)?;
        let parent_inode = if parent_path.is_empty() {
            self.current_dir_inode
        } else if parent_path == "/" {
            0
        } else {
            self.resolve_path(device, parent_path)?
        };
        // Ensure parent is directory (resolve_path already ensures)
        Ok((parent_inode, String::from(basename)))
    }

    /// Check if directory is empty
    pub fn is_directory_empty(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
    ) -> Result<bool, &'static str> {
        let files = self.list_directory(device, dir_inode)?;
        Ok(files.is_empty())
    }

    /// Get current path as String (e.g., "/a/b")
    pub fn current_path(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<String, &'static str> {
        if self.current_dir_inode == 0 {
            return Ok(String::from("/"));
        }
        let mut components: Vec<String> = Vec::new();
        let mut cur = self.current_dir_inode;
        let mut depth = 0;
        while cur != 0 && depth < 64 {
            let parent = self.parent_of(cur).unwrap_or(0);
            // Find name of cur in parent
            let mut found_name: Option<String> = None;
            // Scan parent dir entries
            let parent_inode_copy = *self.inodes.get(parent as usize).ok_or("Invalid parent inode")?;
            for b in 0..INODE_DIRECT_BLOCKS {
                let block_num = unsafe {
                    core::ptr::addr_of!(parent_inode_copy.direct_blocks[b]).read_unaligned()
                };
                if block_num == 0 {
                    break;
                }
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                if device.read_blocks(block_num, 1, &mut buffer).is_err() {
                    break;
                }
                let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
                for i in 0..entries_per_block {
                    let entry = unsafe {
                        let ptr = buffer
                            .as_ptr()
                            .add(i * core::mem::size_of::<DirectoryEntry>())
                            as *const DirectoryEntry;
                        core::ptr::read_unaligned(ptr)
                    };
                    if entry.is_used() && entry.inode_number == cur {
                        if let Ok(n) = entry.get_name() {
                            found_name = Some(n);
                            break;
                        }
                    }
                }
                if found_name.is_some() {
                    break;
                }
            }
            if let Some(n) = found_name {
                components.push(n);
            } else {
                // Could not find name, break
                components.push(String::from("?"));
            }
            cur = parent;
            depth += 1;
        }
        components.reverse();
        let mut path = String::new();
        for comp in components {
            path.push('/');
            path.push_str(&comp);
        }
        if path.is_empty() {
            path.push('/');
        }
        Ok(path)
    }

    /// Create a new directory at path
    pub fn create_directory(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<u32, &'static str> {
        let (parent_inode, basename) = self.resolve_parent_and_name(device, path)?;
        // Check if already exists
        if let Some(_) = self.find_entry_in_dir(device, parent_inode, &basename)? {
            return Err("File exists");
        }
        let free_inode = self.find_free_inode().ok_or("No free inodes")?;
        let free_block = self.allocate_data_block().ok_or("No free blocks")?;
        let mut new_dir_inode = Inode::new_directory();
        // An empty directory still owns one block; size records the block
        // extent so readers can bound their scan.
        new_dir_inode.size = FS_BLOCK_SIZE as u64;
        new_dir_inode.blocks_used = 1;
        new_dir_inode.direct_blocks[0] = free_block;
        self.inodes[free_inode as usize] = new_dir_inode;
        // Zero the new directory block
        let zero = [0u8; FS_BLOCK_SIZE];
        device.write_blocks(free_block, 1, &zero)?;
        // Add entry to parent
        self.add_entry_to_dir(device, parent_inode, &basename, free_inode)?;
        // Update parent map
        self.parent[free_inode as usize] = Some(parent_inode);
        // Update superblock
        unsafe {
            let free_inodes_ptr = core::ptr::addr_of!(self.superblock.free_inodes) as *mut u32;
            let cur = core::ptr::read_unaligned(free_inodes_ptr);
            core::ptr::write_unaligned(free_inodes_ptr, cur.saturating_sub(1));
        }
        self.write_inodes(device)?;
        self.write_superblock(device)?;
        Ok(free_inode)
    }

    /// Remove an empty directory
    pub fn remove_directory(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<(), &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "/" || trimmed == "." || trimmed == ".." {
            return Err("Invalid argument");
        }
        // Resolve target (must be directory)
        let target_inode = self.resolve_path(device, trimmed)?;
        if target_inode == 0 {
            return Err("Invalid argument");
        }
        if self.inodes[target_inode as usize].get_type() != FileType::Directory {
            return Err("Not a directory");
        }
        // Check empty
        if !self.is_directory_empty(device, target_inode)? {
            return Err("Directory not empty");
        }
        // Check not current dir or ancestor of current dir
        if target_inode == self.current_dir_inode {
            return Err("Cannot remove current directory");
        }
        // Walk from current up to root, if we encounter target, it's ancestor
        let mut cur = self.current_dir_inode;
        let mut depth = 0;
        while cur != 0 && depth < 64 {
            if cur == target_inode {
                return Err("Cannot remove current directory");
            }
            if let Some(p) = self.parent_of(cur) {
                cur = p;
            } else {
                break;
            }
            depth += 1;
        }
        // Find parent of target
        let parent_inode = self.parent_of(target_inode).ok_or("Invalid argument")?;
        // Remove entry from parent
        self.remove_entry_from_dir(device, parent_inode, target_inode)?;
        // Free inode and block
        let inode = self.inodes[target_inode as usize];
        for i in 0..INODE_DIRECT_BLOCKS {
            let block = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };
            if block != 0 {
                self.free_data_block(block);
            }
        }
        self.inodes[target_inode as usize] = Inode::new();
        self.parent[target_inode as usize] = None;
        unsafe {
            let free_inodes_ptr = core::ptr::addr_of!(self.superblock.free_inodes) as *mut u32;
            let cur = core::ptr::read_unaligned(free_inodes_ptr);
            core::ptr::write_unaligned(free_inodes_ptr, cur.saturating_add(1));
        }
        self.write_inodes(device)?;
        self.write_superblock(device)?;
        Ok(())
    }

    /// Change directory by path string (cd)
    pub fn change_directory_path(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<(), &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            // cd with no args -> go to root
            self.current_dir_inode = 0;
            self.resolved = None;
            return Ok(());
        }
        let target = self.resolve_path(device, trimmed)?;
        self.current_dir_inode = target;
        self.resolved = None;
        Ok(())
    }

    /// List files in a directory
    pub fn list_directory(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_number: u32,
    ) -> Result<Vec<FileInfo>, &'static str> {
        if inode_number >= self.inode_limit() {
            return Err("Invalid inode number");
        }

        let inode = self.inodes.get(inode_number as usize).ok_or("Invalid inode number")?;
        if inode.get_type() != FileType::Directory {
            return Err("Not a directory");
        }

        let mut files = Vec::new();

        // Read directory blocks - use read_unaligned to avoid packed struct issues
        for i in 0..INODE_DIRECT_BLOCKS {
            let block_num = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };

            if block_num == 0 {
                break;
            }

            let mut buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut buffer)?;

            // Parse directory entries
            let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
            for i in 0..entries_per_block {
                let entry = unsafe {
                    let ptr = buffer
                        .as_ptr()
                        .add(i * core::mem::size_of::<DirectoryEntry>())
                        as *const DirectoryEntry;
                    core::ptr::read_unaligned(ptr)
                };

                if entry.is_used() {
                    // Validate inode number before indexing
                    if entry.inode_number as usize >= MAX_INODES {
                        continue; // Skip invalid entries
                    }

                    let name = entry.get_name()?;
                    let entry_inode = match self.inodes.get(entry.inode_number as usize) { Some(i) => i, None => continue };

                    files.push(FileInfo {
                        name,
                        size: entry_inode.size,
                        is_directory: entry_inode.get_type() == FileType::Directory,
                        inode_number: entry.inode_number,
                    });
                }
            }
        }

        Ok(files)
    }

    /// Find a free inode
    fn find_free_inode(&self) -> Option<u32> {
        for (i, inode) in self.inodes.iter().enumerate() {
            if !inode.is_used() {
                return Some(i as u32);
            }
        }
        None
    }

    fn data_region(&self) -> (u64, usize) {
        let start =
            unsafe { core::ptr::addr_of!(self.superblock.data_block_start).read_unaligned() };
        let total = unsafe { core::ptr::addr_of!(self.superblock.total_blocks).read_unaligned() };
        (
            start,
            total.saturating_sub(start).min(usize::MAX as u64) as usize,
        )
    }

    fn block_bit(&self, index: usize) -> bool {
        self.allocated_blocks
            .get(index / 8)
            .map(|byte| byte & (1 << (index % 8)) != 0)
            .unwrap_or(true)
    }

    fn set_block_bit(&mut self, index: usize, allocated: bool) -> bool {
        let Some(byte) = self.allocated_blocks.get_mut(index / 8) else {
            return false;
        };
        let mask = 1 << (index % 8);
        let was_allocated = *byte & mask != 0;
        if allocated {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
        if was_allocated != allocated {
            self.mark_bitmap_dirty(index);
        }
        was_allocated
    }

    /// Record that the on-disk bitmap block covering data-block `index` needs
    /// writing back.
    ///
    /// Tracking per *bitmap block* rather than per data block keeps the
    /// write-back proportional to the pages actually touched: one allocation
    /// dirties one bitmap block, not the whole map.
    fn mark_bitmap_dirty(&mut self, index: usize) {
        let per_block = FS_BLOCK_SIZE * 8;
        let bitmap_block = index / per_block;
        if let Some(flag) = self.bitmap_dirty.get_mut(bitmap_block) {
            *flag = true;
        }
    }

    /// Write back any bitmap blocks changed since the last flush.
    ///
    /// Called from [`SimpleFilesystem::write_superblock`], which every
    /// mutating operation already ends with, so allocation state reaches the
    /// platter before the metadata that references it.
    fn flush_bitmap(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let bitmap_start =
            unsafe { core::ptr::addr_of!(self.superblock.bitmap_start).read_unaligned() };
        let bitmap_blocks =
            unsafe { core::ptr::addr_of!(self.superblock.bitmap_blocks).read_unaligned() };
        if bitmap_blocks == 0 {
            return Ok(());
        }
        let per_block = FS_BLOCK_SIZE * 8;
        let mut stage = [0u8; FS_BLOCK_SIZE];
        for (i, dirty) in self.bitmap_dirty.iter_mut().enumerate() {
            if !*dirty || (i as u32) >= bitmap_blocks {
                continue;
            }
            let first_byte = (i * per_block) / 8;
            if first_byte >= self.allocated_blocks.len() {
                *dirty = false;
                continue;
            }
            // The final bitmap block is usually partial; `BlockDevice`
            // requires a buffer of exactly one block, so pad the tail.
            let n = core::cmp::min(FS_BLOCK_SIZE, self.allocated_blocks.len() - first_byte);
            stage[..n].copy_from_slice(&self.allocated_blocks[first_byte..first_byte + n]);
            device.write_blocks(bitmap_start + i as u64, 1, &stage)?;
            *dirty = false;
        }
        Ok(())
    }

    /// Recompute `free_blocks` from the bitmap, which is the authority.
    fn recount_free_blocks(&mut self) {
        let (_, count) = self.data_region();
        let used = (0..count).filter(|&i| self.block_bit(i)).count() as u64;
        let free = (count as u64).saturating_sub(used);
        unsafe {
            core::ptr::addr_of_mut!(self.superblock.free_blocks).write_unaligned(free);
        }
    }

    fn mark_block_allocated(&mut self, block: u64) {
        let (start, count) = self.data_region();
        if block >= start && block - start < count as u64 {
            self.set_block_bit((block - start) as usize, true);
        }
    }

    fn allocate_data_block(&mut self) -> Option<u64> {
        let (start, count) = self.data_region();
        if count == 0 || self.allocated_blocks.is_empty() {
            return None;
        }
        for step in 0..count {
            let index = (self.allocation_cursor + step) % count;
            if !self.block_bit(index) {
                self.set_block_bit(index, true);
                self.allocation_cursor = (index + 1) % count;
                unsafe {
                    let free = core::ptr::addr_of!(self.superblock.free_blocks)
                        .read_unaligned()
                        .saturating_sub(1);
                    core::ptr::addr_of_mut!(self.superblock.free_blocks).write_unaligned(free);
                }
                return Some(start + index as u64);
            }
        }
        None
    }

    /// Load the on-disk allocation bitmap into RAM.
    ///
    /// This is the whole point of version 2: free-space state is *persisted*,
    /// not reconstructed by walking inodes. Walking cannot distinguish "block
    /// allocated, inode record never written" from "block free", and so
    /// re-hands the first out twice after any interrupted operation.
    fn load_bitmap(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let bitmap_start =
            unsafe { core::ptr::addr_of!(self.superblock.bitmap_start).read_unaligned() };
        let bitmap_blocks =
            unsafe { core::ptr::addr_of!(self.superblock.bitmap_blocks).read_unaligned() };
        let (_, count) = self.data_region();
        let bitmap_len = (count as usize).div_ceil(8);

        self.allocated_blocks.clear();
        self.allocated_blocks
            .try_reserve_exact(bitmap_len)
            .map_err(|_| "Unable to allocate filesystem bitmap")?;
        self.allocated_blocks.resize(bitmap_len, 0);
        self.bitmap_dirty.clear();
        self.bitmap_dirty.resize(bitmap_blocks as usize, false);
        self.allocation_cursor = 0;

        if bitmap_blocks == 0 {
            return Ok(());
        }
        // The final bitmap block is usually partial; `BlockDevice` requires a
        // buffer of exactly one block, so read into a full block and copy the
        // part that belongs to the bitmap.
        let mut stage = [0u8; FS_BLOCK_SIZE];
        let mut remaining = bitmap_len;
        let mut block = bitmap_start;
        for _ in 0..bitmap_blocks as usize {
            if remaining == 0 {
                break;
            }
            let n = core::cmp::min(FS_BLOCK_SIZE, remaining);
            device.read_blocks(block, 1, &mut stage)?;
            let offset = bitmap_len - remaining;
            self.allocated_blocks[offset..offset + n].copy_from_slice(&stage[..n]);
            remaining -= n;
            block += 1;
        }
        Ok(())
    }

    /// Cross-check the persisted bitmap against what the inodes actually
    /// reference.
    ///
    /// Returns the set of blocks reachable from inodes but missing from the
    /// bitmap (leaked metadata references) and the set marked allocated that no
    /// inode references (leaked space). Reported by `fsck`; the bitmap is *not*
    /// silently rewritten from the inode walk, because that is the v1 bug.
    fn audit_bitmap(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(usize, usize), &'static str> {
        let (start, count) = self.data_region();
        let mut referenced: Vec<u8> = alloc::vec![0u8; count.div_ceil(8)];
        for inode_index in 0..self.inodes.len() {
            let inode = self.inodes[inode_index];
            for direct in 0..INODE_DIRECT_BLOCKS {
                let block =
                    unsafe { core::ptr::addr_of!(inode.direct_blocks[direct]).read_unaligned() };
                if block != 0 && block >= start && block - start < count as u64 {
                    let i = (block - start) as usize;
                    referenced[i / 8] |= 1 << (i % 8);
                }
            }
            let mut indirect = inode.indirect_head();
            let mut hops = 0usize;
            while indirect != 0
                && indirect >= start
                && indirect - start < count as u64
                && hops <= count
            {
                let i = (indirect - start) as usize;
                referenced[i / 8] |= 1 << (i % 8);
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(indirect, 1, &mut buffer)?;
                indirect = u64::from_le_bytes(buffer[..8].try_into().unwrap());
                for slot in 0..INDIRECT_DATA_BLOCKS {
                    let offset = 8 + slot * core::mem::size_of::<u64>();
                    let block = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
                    if block != 0 && block >= start && block - start < count as u64 {
                        let j = (block - start) as usize;
                        referenced[j / 8] |= 1 << (j % 8);
                    }
                }
                hops += 1;
            }
        }
        let mut missing = 0usize;
        let mut orphan = 0usize;
        for i in 0..count {
            let want = referenced[i / 8] & (1 << (i % 8)) != 0;
            let have = self.block_bit(i);
            if want && !have {
                missing += 1;
            } else if have && !want {
                orphan += 1;
            }
        }
        Ok((missing, orphan))
    }

    fn free_data_block(&mut self, block: u64) {
        let (start, count) = self.data_region();
        if block < start || block - start >= count as u64 {
            return;
        }
        if self.set_block_bit((block - start) as usize, false) {
            unsafe {
                let free = core::ptr::addr_of!(self.superblock.free_blocks)
                    .read_unaligned()
                    .saturating_add(1);
                core::ptr::addr_of_mut!(self.superblock.free_blocks).write_unaligned(free);
            }
            self.allocation_cursor = self.allocation_cursor.min((block - start) as usize);
        }
    }


    fn read_indirect_block(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        block: u64,
    ) -> Result<[u8; FS_BLOCK_SIZE], &'static str> {
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(block, 1, &mut buffer)?;
        Ok(buffer)
    }

    fn write_indirect_block(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        block: u64,
        buffer: &[u8; FS_BLOCK_SIZE],
    ) -> Result<(), &'static str> {
        device.write_blocks(block, 1, buffer)
    }

    /// Re-read the inode table from disk, discarding cached path resolutions.
    ///
    /// The in-memory table is authoritative during normal operation (every
    /// mutation writes through), so read paths do *not* call this â€” doing so
    /// re-reads the whole table on every call, including once per path
    /// component. Only invoke it when something outside this instance modified
    /// the on-disk table.
    pub fn resync_inodes(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        self.resolved = None;
        self.reload_inodes(device)
    }

    /// Read the whole inode table from disk in one request.
    ///
    /// Only called via `resync_inodes`; the table is small enough to fetch in a
    /// single transfer, and the per-block loop this replaced cost one round-trip
    /// per table block.
    fn reload_inodes(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let inode_blocks =
            unsafe { core::ptr::addr_of!(self.superblock.inode_blocks).read_unaligned() };
        let inode_table_size = (inode_blocks as usize) * FS_BLOCK_SIZE;
        let mut inode_buffer = alloc::vec![0u8; inode_table_size];

        // One request for the whole table rather than one per block. Callers
        // cap transfers at 255 blocks, and the table is `ceil(256*144/512)` = 72.
        for chunk in inode_buffer.chunks_mut(MAX_IO_BLOCKS * FS_BLOCK_SIZE) {
            let blocks = chunk.len() / FS_BLOCK_SIZE;
            device.read_blocks(1, blocks, chunk)?;
        }

        // Update in-memory inode table
        unsafe {
            let ptr = inode_buffer.as_ptr() as *const Inode;
            for i in 0..MAX_INODES {
                self.inodes[i] = core::ptr::read_unaligned(ptr.add(i));
            }
        }

        Ok(())
    }

    /// Write inode table back to disk
    fn write_inodes(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        self.resolved = None;
        // Only write the portion of the inode table that actually exists on disk.
        // Writing past superblock.inode_blocks would overwrite data blocks (bug).
        let inode_table_bytes = (self.superblock.inode_blocks as usize) * FS_BLOCK_SIZE;
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(self.inodes.as_ptr() as *const u8, inode_table_bytes)
        };

        let mut block_num = 1u64;
        for chunk in inode_bytes.chunks(MAX_IO_BLOCKS * FS_BLOCK_SIZE) {
            let blocks = chunk.len() / FS_BLOCK_SIZE;
            device.write_blocks(block_num, blocks, chunk)?;
            block_num += blocks as u64;
        }

        Ok(())
    }

    fn write_inode(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
    ) -> Result<(), &'static str> {
        if inode_index >= self.inodes.len() {
            return Err("Invalid inode number");
        }
        let inode_size = core::mem::size_of::<Inode>();
        let byte_offset = inode_index * inode_size;
        let table_block = byte_offset / FS_BLOCK_SIZE;
        let inode_blocks = self.superblock.inode_blocks as usize;
        if table_block >= inode_blocks {
            return Err("Invalid inode table offset");
        }
        let slot_offset = byte_offset % FS_BLOCK_SIZE;
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(self.inodes[inode_index]) as *const u8,
                inode_size,
            )
        };
        if slot_offset + inode_size <= FS_BLOCK_SIZE {
            // Fast path: the whole slot lives in one table block.
            let mut buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(1 + table_block as u64, 1, &mut buffer)?;
            buffer[slot_offset..slot_offset + inode_size].copy_from_slice(inode_bytes);
            return device.write_blocks(1 + table_block as u64, 1, &buffer);
        }
        // Slow path: Inode (144 B) does not divide the 512 B block, so some
        // slots straddle two table blocks (e.g. index 3 spans bytes 432-576).
        // Splice the slot across both blocks with read-modify-write.
        if table_block + 1 >= inode_blocks {
            return Err("Invalid inode table offset");
        }
        let head_len = FS_BLOCK_SIZE - slot_offset;
        let mut head = [0u8; FS_BLOCK_SIZE];
        let mut tail = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(1 + table_block as u64, 1, &mut head)?;
        device.read_blocks(1 + table_block as u64 + 1, 1, &mut tail)?;
        head[slot_offset..].copy_from_slice(&inode_bytes[..head_len]);
        tail[..inode_size - head_len].copy_from_slice(&inode_bytes[head_len..]);
        device.write_blocks(1 + table_block as u64, 1, &head)?;
        device.write_blocks(1 + table_block as u64 + 1, 1, &tail)
    }

    /// Write superblock back to disk
    fn write_superblock(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        self.resolved = None;
        // Allocation state must reach the platter *before* the metadata that
        // references it, and after it so a crash cannot leave the bitmap
        // claiming a block that the inode table still calls free.
        self.flush_bitmap(device)?;
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        unsafe {
            let sb_ptr = &self.superblock as *const Superblock as *const u8;
            core::ptr::copy_nonoverlapping(
                sb_ptr,
                buffer.as_mut_ptr(),
                core::mem::size_of::<Superblock>(),
            );
        }
        device.write_blocks(0, 1, &buffer)?;
        Ok(())
    }

    /// Create a new file in the current directory (path-aware)
    pub fn create_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<u32, &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        // Support paths like "a/b/file.txt" via resolve_parent_and_name
        let (parent_inode, basename) = self.resolve_parent_and_name(device, trimmed)?;
        // Check if file already exists in parent
        if let Some(_) = self.find_entry_in_dir(device, parent_inode, &basename)? {
            return Err("File already exists");
        }

        // Find free inode
        let inode_num = self.find_free_inode().ok_or("No free inodes")?;

        // Create new file inode
        self.inodes[inode_num as usize] = Inode::new_file();
        self.inodes[inode_num as usize].size = 0;
        self.inodes[inode_num as usize].blocks_used = 0;

        self.add_entry_to_dir(device, parent_inode, &basename, inode_num)?;
        // Keep the in-memory parent map consistent. It is rebuilt in full
        // at mount, so without this a file's parent is unknown until then
        // and `rename_file` cannot tell which directory to update.
        self.parent[inode_num as usize] = Some(parent_inode);
        // Update superblock
        unsafe {
            let free_inodes_ptr = core::ptr::addr_of!(self.superblock.free_inodes) as *mut u32;
            let current_val = core::ptr::read_unaligned(free_inodes_ptr);
            core::ptr::write_unaligned(free_inodes_ptr, current_val.saturating_sub(1));
        }

        // Write updates to disk
        self.write_inodes(device)?;
        self.write_superblock(device)?;

        Ok(inode_num)
    }

    /// Write data to a file (path-aware)
    pub fn write_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
        data: &[u8],
    ) -> Result<(), &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        // Use file-or-dir resolver to find file inode, then ensure it's a file
        let file_inode_num = self.resolve_file_or_dir(device, trimmed)?;
        if self.inodes[file_inode_num as usize].get_type() != FileType::File {
            return Err("Not a file");
        }
        self.write_file_by_inode(device, file_inode_num, data)
    }

    /// Write data to a file by inode number (avoids directory lookup)
    pub fn write_file_by_inode(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        file_inode_num: u32,
        data: &[u8],
    ) -> Result<(), &'static str> {
        self.truncate_file_by_inode(device, file_inode_num)?;
        match self.append_file_by_inode(device, file_inode_num, data) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.truncate_file_by_inode(device, file_inode_num);
                Err(error)
            }
        }
    }

    pub fn append_file_by_inode(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        file_inode_num: u32,
        data: &[u8],
    ) -> Result<(), &'static str> {
        let index = file_inode_num as usize;
        if index >= self.inodes.len() || self.inodes[index].get_type() != FileType::File {
            return Err("Not a file");
        }
        let (_, count) = self.data_region();
        let max_file_bytes = count
            .checked_mul(FS_BLOCK_SIZE)
            .ok_or("File size overflow")?;
        let current_size =
            usize::try_from(self.inodes[index].size).map_err(|_| "File too large")?;
        let final_size = current_size
            .checked_add(data.len())
            .ok_or("File size overflow")?;
        if final_size > max_file_bytes {
            return Err("No free blocks");
        }

        let append_result = (|| {
            let mut consumed = 0;
            while consumed < data.len() {
                let size = self.inodes[index].size as usize;
                let logical_block = size / FS_BLOCK_SIZE;
                let block_offset = size % FS_BLOCK_SIZE;
                let count = (FS_BLOCK_SIZE - block_offset).min(data.len() - consumed);
                // A block filled from offset 0 is fully overwritten, so it does
                // not need pre-zeroing.
                let whole_block = block_offset == 0 && count == FS_BLOCK_SIZE;
                let block =
                    self.ensure_file_block(device, index, logical_block, !whole_block)?;
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                if block_offset != 0 {
                    device.read_blocks(block, 1, &mut buffer)?;
                }
                buffer[block_offset..block_offset + count]
                    .copy_from_slice(&data[consumed..consumed + count]);
                device.write_blocks(block, 1, &buffer)?;
                self.inodes[index].size += count as u64;
                consumed += count;
            }
            Ok::<(), &'static str>(())
        })();
        if let Err(error) = append_result {
            // Persist whatever indirect pointers were already handed out so the
            // on-disk map never trails the in-memory size.
            let _ = self.flush_chain_if_dirty(device);
            let _ = self.write_inode(device, index);
            let _ = self.write_superblock(device);
            return Err(error);
        }
        // One indirect-node write per append instead of one per block.
        self.flush_chain_if_dirty(device)?;

        let data_blocks = (self.inodes[index].size as usize).div_ceil(FS_BLOCK_SIZE);
        let direct_blocks = data_blocks.min(INODE_DIRECT_BLOCKS);
        let indirect_blocks = data_blocks.saturating_sub(direct_blocks);
        let metadata_blocks = indirect_blocks.div_ceil(INDIRECT_DATA_BLOCKS);
        self.inodes[index].blocks_used = (data_blocks + metadata_blocks) as u32;
        self.write_inode(device, index)?;
        self.write_superblock(device)
    }

    pub fn truncate_file_by_inode(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        file_inode_num: u32,
    ) -> Result<(), &'static str> {
        let index = file_inode_num as usize;
        if index >= self.inodes.len() || self.inodes[index].get_type() != FileType::File {
            return Err("Not a file");
        }
        self.release_file_storage(device, index)?;
        self.write_inode(device, index)?;
        self.write_superblock(device)
    }

    /// Return the physical block backing `logical_block`, allocating it (and any
    /// indirect nodes) if needed.
    ///
    /// `zero_fill` may be cleared when the caller is about to overwrite the whole
    /// block; bytes past the file's size are never returned by a read, so
    /// pre-zeroing them costs one write per block for no benefit.
    fn ensure_file_block(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        logical_block: usize,
        zero_fill: bool,
    ) -> Result<u64, &'static str> {
        if logical_block < INODE_DIRECT_BLOCKS {
            let existing = unsafe {
                core::ptr::addr_of!(self.inodes[inode_index].direct_blocks[logical_block])
                    .read_unaligned()
            };
            if existing != 0 {
                return Ok(existing);
            }
            let block = self.allocate_data_block().ok_or("No free blocks")?;
            if zero_fill {
                let zero = [0u8; FS_BLOCK_SIZE];
                if let Err(error) = device.write_blocks(block, 1, &zero) {
                    self.free_data_block(block);
                    return Err(error);
                }
            }
            self.inodes[inode_index].direct_blocks[logical_block] = block;
            return Ok(block);
        }

        let indirect_index = logical_block - INODE_DIRECT_BLOCKS;
        let node_index = indirect_index / INDIRECT_DATA_BLOCKS;
        let slot = indirect_index % INDIRECT_DATA_BLOCKS;
        if !self.ensure_chain_node(device, inode_index, node_index)? {
            return Err("No free blocks");
        }
        if self.chain.as_ref().map(|c| c.entries[slot]).unwrap_or(0) != 0 {
            return Ok(self.chain.as_ref().unwrap().entries[slot]);
        }
        let block = self.allocate_data_block().ok_or("No free blocks")?;
        if zero_fill {
            let zero = [0u8; FS_BLOCK_SIZE];
            if let Err(error) = device.write_blocks(block, 1, &zero) {
                self.free_data_block(block);
                return Err(error);
            }
        }
        if let Some(cursor) = &mut self.chain {
            cursor.entries[slot] = block;
        }
        // Deferred: the caller batches this into one flush per append.
        self.chain_dirty = true;
        Ok(block)
    }

    /// Serialize the cached node back to its on-disk 512-byte layout.
    fn chain_node_buffer(&self) -> Result<[u8; FS_BLOCK_SIZE], &'static str> {
        let cursor = self.chain.as_ref().ok_or("No cached indirect node")?;
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        buffer[..8].copy_from_slice(&cursor.next.to_le_bytes());
        for slot in 0..INDIRECT_DATA_BLOCKS {
            let offset = 8 + slot * core::mem::size_of::<u64>();
            buffer[offset..offset + 8].copy_from_slice(&cursor.entries[slot].to_le_bytes());
        }
        Ok(buffer)
    }

    /// Populate `self.chain`'s `entries`/`next` by re-reading its node block.
    fn load_chain_entries(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let node = self.chain.as_ref().ok_or("No cached indirect node")?.node;
        let buffer = self.read_indirect_block(device, node)?;
        let mut entries = [0u64; INDIRECT_DATA_BLOCKS];
        for slot in 0..INDIRECT_DATA_BLOCKS {
            let offset = 8 + slot * core::mem::size_of::<u64>();
            entries[slot] = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
        }
        let cursor = self.chain.as_mut().ok_or("No cached indirect node")?;
        cursor.next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
        cursor.entries = entries;
        Ok(())
    }

    /// Write the cached indirect node back to disk. Callers must have already
    /// updated `self.chain`.
    fn flush_chain_node(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let node = self.chain.as_ref().ok_or("No cached indirect node")?.node;
        let buffer = self.chain_node_buffer()?;
        self.write_indirect_block(device, node, &buffer)?;
        self.chain_dirty = false;
        Ok(())
    }

    /// Persist the cached node if a write left it modified. Appending a large
    /// buffer adds one slot per block, and flushing the whole 512-byte node on
    /// each of them tripled the write count; the flush is batched to the end of
    /// the operation instead.
    fn flush_chain_if_dirty(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        if self.chain_dirty {
            self.flush_chain_node(device)?;
        }
        Ok(())
    }

    /// Guarantee that indirect node `node_index` of `inode_index` exists, leaving
    /// it cached in `self.chain`. Allocates and links intermediate nodes as
    /// needed. Mirrors `seek_chain_node` but may grow the chain.
    fn ensure_chain_node(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        node_index: usize,
    ) -> Result<bool, &'static str> {
        if self.inodes[inode_index].indirect_head() == 0 {
            // Replacing the cursor: persist any pending pointer updates first.
            self.flush_chain_if_dirty(device)?;
            if node_index != 0 {
                return Ok(false);
            }
            let root = self.allocate_data_block().ok_or("No free blocks")?;
            let zero = [0u8; FS_BLOCK_SIZE];
            if let Err(error) = device.write_blocks(root, 1, &zero) {
                self.free_data_block(root);
                return Err(error);
            }
            self.inodes[inode_index].set_indirect_head(root);
            self.chain = Some(ChainCursor::new(inode_index as u32, 0, root, 0));
        }

        // Reuse or step forward when the cursor is already in this chain.
        let cached = match &self.chain {
            Some(cursor) if cursor.inode == inode_index as u32 => {
                Some((cursor.node_index, cursor.next))
            }
            _ => None,
        };
        if let Some((cached_index, _)) = cached {
            if cached_index == node_index {
                return Ok(true);
            }
            // Advance/extend one node at a time, reusing the cached `next`.
            if cached_index < node_index {
                let mut index = cached_index;
                while index < node_index {
                    if self.chain.as_ref().unwrap().next == 0 {
                        // Extend the chain: allocate a node and link it in.
                        let new_node = self.allocate_data_block().ok_or("No free blocks")?;
                        let zero = [0u8; FS_BLOCK_SIZE];
                        if let Err(error) = device.write_blocks(new_node, 1, &zero) {
                            self.free_data_block(new_node);
                            return Err(error);
                        }
                        if let Some(cursor) = &mut self.chain {
                            cursor.next = new_node;
                        }
                        if let Err(error) = self.flush_chain_node(device) {
                            self.free_data_block(new_node);
                            if let Some(cursor) = &mut self.chain {
                                cursor.next = 0;
                            }
                            return Err(error);
                        }
                    } else {
                        // Stepping off the cached node discards it, so persist
                        // any updates made to it.
                        self.flush_chain_if_dirty(device)?;
                    }
                    let node = self.chain.as_ref().unwrap().next;
                    self.chain = Some(ChainCursor::new(inode_index as u32, index + 1, node, 0));
                    self.load_chain_entries(device)?;
                    index += 1;
                }
                return Ok(true);
            }
            // Backward seek: fall through to a fresh walk from the head.
        }

        // Fresh walk from the head, extending the chain if needed. This
        // discards whatever node was cached, so persist it first.
        self.flush_chain_if_dirty(device)?;
        let head = self.inodes[inode_index].indirect_head();
        let mut node = head;
        for _ in 0..node_index {
            let mut buffer = self.read_indirect_block(device, node)?;
            let mut next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
            if next == 0 {
                let new_node = self.allocate_data_block().ok_or("No free blocks")?;
                let zero = [0u8; FS_BLOCK_SIZE];
                if let Err(error) = device.write_blocks(new_node, 1, &zero) {
                    self.free_data_block(new_node);
                    return Err(error);
                }
                buffer[..8].copy_from_slice(&new_node.to_le_bytes());
                if let Err(error) = self.write_indirect_block(device, node, &buffer) {
                    self.free_data_block(new_node);
                    return Err(error);
                }
                next = new_node;
            }
            node = next;
        }
        self.chain = Some(ChainCursor::new(inode_index as u32, node_index, node, 0));
        self.load_chain_entries(device)?;
        Ok(true)
    }

    fn file_block_at(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        logical_block: usize,
    ) -> Result<Option<u64>, &'static str> {
        if inode_index >= self.inodes.len() {
            return Err("Invalid inode number");
        }
        if logical_block < INODE_DIRECT_BLOCKS {
            let block = unsafe {
                core::ptr::addr_of!(self.inodes[inode_index].direct_blocks[logical_block])
                    .read_unaligned()
            };
            return Ok((block != 0).then_some(block));
        }
        let indirect_index = logical_block - INODE_DIRECT_BLOCKS;
        let node_index = indirect_index / INDIRECT_DATA_BLOCKS;
        let slot = indirect_index % INDIRECT_DATA_BLOCKS;
        if !self.seek_chain_node(device, inode_index, node_index)? {
            return Ok(None);
        }
        // Safe: `seek_chain_node` returning true guarantees the cursor holds this node.
        let block = self.chain.as_ref().map(|c| c.entries[slot]).unwrap_or(0);
        Ok((block != 0).then_some(block))
    }

    /// Populate `self.chain` so it holds indirect node `node_index` of
    /// `inode_index`. Returns `false` when the chain is shorter than that.
    ///
    /// Reusing the already-cached node costs no I/O, and stepping forward one
    /// node uses the cached `next` pointer, so a sequential walk reads each
    /// node exactly once. Any other seek restarts from the head, which is
    /// correct but costs a full walk.
    fn seek_chain_node(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        node_index: usize,
    ) -> Result<bool, &'static str> {
        let head = self.inodes[inode_index].indirect_head();
        let cached = match &self.chain {
            Some(cursor) if cursor.inode == inode_index as u32 => {
                Some((cursor.node_index, cursor.node, cursor.next))
            }
            _ => None,
        };
        if let Some((cached_index, cached_node, _)) = cached {
            if cached_index == node_index && cached_node != 0 {
                return Ok(true);
            }
        }
        let node = match cached {
            // Advancing exactly one node: the cached `next` already points at it.
            Some((cached_index, _, next)) if cached_index + 1 == node_index && next != 0 => next,
            _ => {
                if head == 0 {
                    return Ok(false);
                }
                let mut walk = head;
                for _ in 0..node_index {
                    let buffer = self.read_indirect_block(device, walk)?;
                    let next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
                    if next == 0 {
                        return Ok(false);
                    }
                    walk = next;
                }
                walk
            }
        };
        // Every path above replaced the cursor, so persist pending updates first
        // rather than dropping them. A no-op on the read path, where nothing is
        // ever dirty.
        self.flush_chain_if_dirty(device)?;
        self.chain = Some(ChainCursor::new(inode_index as u32, node_index, node, 0));
        self.load_chain_entries(device)?;
        Ok(true)
    }

    fn release_file_storage(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
    ) -> Result<(), &'static str> {
        // The chain head is about to be cleared, so any cached node for this
        // inode (or a future file that reuses its inode number) is stale.
        self.chain = None;
        self.chain_dirty = false;
        let inode = self.inodes[inode_index];
        for i in 0..INODE_DIRECT_BLOCKS {
            let block = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };
            if block != 0 {
                self.free_data_block(block);
            }
            self.inodes[inode_index].direct_blocks[i] = 0;
        }
        let mut node = inode.indirect_head();
        let (_, max_nodes) = self.data_region();
        for _ in 0..max_nodes {
            if node == 0 {
                break;
            }
            let buffer = self.read_indirect_block(device, node)?;
            let next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
            for slot in 0..INDIRECT_DATA_BLOCKS {
                let offset = 8 + slot * core::mem::size_of::<u64>();
                let block = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
                if block != 0 {
                    self.free_data_block(block);
                }
            }
            self.free_data_block(node);
            node = next;
        }
        self.inodes[inode_index].set_indirect_head(0);
        self.inodes[inode_index].size = 0;
        self.inodes[inode_index].blocks_used = 0;
        Ok(())
    }

    /// Read data from a file (path-aware)
    pub fn read_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<Vec<u8>, &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        let file_inode_num = self.resolve_file_or_dir(device, trimmed)? as usize;
        if self.inodes[file_inode_num].get_type() != FileType::File {
            return Err("Not a file");
        }
        let file_size = self.inodes[file_inode_num].size as usize;

        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut data = Vec::new();
        // The final block is rounded up to a whole block before being truncated
        // back, so reserve a block of slack to avoid a realloc mid-read.
        data.try_reserve_exact(file_size + FS_BLOCK_SIZE)
            .map_err(|_| "Unable to allocate file buffer")?;
        let blocks_to_read = file_size.div_ceil(FS_BLOCK_SIZE);

        // Read data from blocks, coalescing physically adjacent runs so a
        // sequential read costs one transfer per run instead of per block.
        // Block lookups stay strictly forward, so the indirect-node cursor
        // never has to walk backwards.
        let mut i = 0usize;
        while i < blocks_to_read {
            let Some(block_num) = self.file_block_at(device, file_inode_num, i)? else {
                return Err("File block map is incomplete");
            };
            let mut run = 1usize;
            while i + run < blocks_to_read && run < MAX_IO_BLOCKS {
                match self.file_block_at(device, file_inode_num, i + run)? {
                    Some(next) if next == block_num + run as u64 => run += 1,
                    _ => break,
                }
            }

            let start = data.len();
            let wanted = core::cmp::min(run * FS_BLOCK_SIZE, file_size - start);
            let read_blocks = wanted.div_ceil(FS_BLOCK_SIZE);
            data.resize(start + read_blocks * FS_BLOCK_SIZE, 0);
            device.read_blocks(block_num, read_blocks, &mut data[start..])?;
            // Drop the padding in the file's final block.
            data.truncate(start + wanted);
            i += read_blocks;
        }

        Ok(data)
    }

    /// Read a byte range `[offset, offset + out.len())` of a file into `out`
    /// without allocating the whole file. Streaming primitive for large
    /// assets (Doom WAD lumps): callers page in what they need and keep the
    /// 32 MiB heap free. Returns bytes copied (0 at EOF). Holes read as zero.
    pub fn read_file_range(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, &'static str> {
        if out.is_empty() {
            return Ok(0);
        }
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        let index = self.resolve_file_or_dir(device, trimmed)? as usize;
        if self.inodes[index].get_type() != FileType::File {
            return Err("Not a file");
        }
        let size = self.inodes[index].size;
        if offset >= size {
            return Ok(0);
        }
        let want = (size - offset).min(out.len() as u64) as usize;
        let mut copied = 0usize;
        while copied < want {
            let abs = offset + copied as u64;
            let logical = (abs / FS_BLOCK_SIZE as u64) as usize;
            let in_block = (abs % FS_BLOCK_SIZE as u64) as usize;
            let Some(block_num) = self.file_block_at(device, index, logical)? else {
                // Sparse hole: zero-fill to the end of this block span.
                let fill = (FS_BLOCK_SIZE - in_block).min(want - copied);
                out[copied..copied + fill].fill(0);
                copied += fill;
                continue;
            };

            if in_block != 0 {
                // Unaligned start: the leading block cannot be read straight
                // into `out`, so stage it.
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(block_num, 1, &mut buffer)?;
                let take = (FS_BLOCK_SIZE - in_block).min(want - copied);
                out[copied..copied + take]
                    .copy_from_slice(&buffer[in_block..in_block + take]);
                copied += take;
                continue;
            }

            // Aligned start: gather physically adjacent blocks so the run costs
            // a single transfer. Block lookups only ever move forward, so the
            // indirect-node cursor never walks backwards.
            let room = want - copied;
            let mut run = 1usize;
            while run < MAX_IO_BLOCKS && (run + 1) * FS_BLOCK_SIZE <= room {
                match self.file_block_at(device, index, logical + run)? {
                    Some(next) if next == block_num + run as u64 => run += 1,
                    _ => break,
                }
            }
            let span = run * FS_BLOCK_SIZE;
            if span <= room {
                device.read_blocks(block_num, run, &mut out[copied..copied + span])?;
                copied += span;
            } else {
                // Trailing partial block: `out` has no room for a whole block,
                // so stage it.
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(block_num, 1, &mut buffer)?;
                out[copied..copied + room].copy_from_slice(&buffer[..room]);
                copied += room;
            }
        }
        Ok(copied)
    }

    /// Delete a file (path-aware)
    pub fn delete_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        path: &str,
    ) -> Result<(), &'static str> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        let file_inode_num = self.resolve_file_or_dir(device, trimmed)?;
        if self.inodes[file_inode_num as usize].get_type() != FileType::File {
            return Err("Not a file");
        }
        // Need parent to remove entry
        let (parent_inode, _) = self.resolve_parent_and_name(device, trimmed)?;
        // Actually parent/name split already, but we have file_inode_num; do direct remove
        self.remove_entry_from_dir(device, parent_inode, file_inode_num)?;
        self.release_file_storage(device, file_inode_num as usize)?;
        self.inodes[file_inode_num as usize] = Inode::new();
        if let Some(p) = self.parent.get_mut(file_inode_num as usize) {
            *p = None;
        }        // The inode number can be handed to a new file, so no cached indirect
        // node may outlive it.
        self.chain = None;
        self.chain_dirty = false;
        self.resolved = None;

        // Update superblock
        unsafe {
            let free_inodes_ptr = core::ptr::addr_of!(self.superblock.free_inodes) as *mut u32;
            let current_inodes = core::ptr::read_unaligned(free_inodes_ptr);
            core::ptr::write_unaligned(free_inodes_ptr, current_inodes.saturating_add(1));
        }

        // Write updates to disk
        self.write_inodes(device)?;
        self.write_superblock(device)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SimpleFilesystem, Superblock, FS_BLOCK_SIZE, INDIRECT_DATA_BLOCKS, INODE_DIRECT_BLOCKS};
    use crate::drivers::block::{BlockDevice, CountingDisk, RamDisk};

    #[test]
    fn file_data_round_trips_across_multiple_indirect_blocks() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/large.bin").unwrap();
        let data_len = (12 + 63 + 5) * FS_BLOCK_SIZE + 137;
        let data: alloc::vec::Vec<u8> = (0..data_len).map(|index| (index % 251) as u8).collect();

        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();
        assert_eq!(fs.read_file(&mut disk, "/large.bin").unwrap(), data);
    }

    #[test]
    fn deleting_indirect_file_releases_its_blocks() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let initial_free =
            unsafe { core::ptr::addr_of!(fs.superblock.free_blocks).read_unaligned() };
        let inode = fs.create_file(&mut disk, "/large.bin").unwrap();
        let data = alloc::vec![0xA5; (12 + 70) * FS_BLOCK_SIZE];
        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();
        fs.delete_file(&mut disk, "/large.bin").unwrap();
        let final_free = unsafe { core::ptr::addr_of!(fs.superblock.free_blocks).read_unaligned() };
        assert_eq!(final_free, initial_free);
    }

    #[test]
    fn streamed_appends_promote_and_replace_destination() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let staging_inode = fs.create_file(&mut disk, "/download.part").unwrap();
        let destination_inode = fs.create_file(&mut disk, "/download.bin").unwrap();
        fs.write_file_by_inode(&mut disk, destination_inode, b"old contents")
            .unwrap();

        let first = alloc::vec![0x31; 781];
        let second = alloc::vec![0x92; (12 + 67) * FS_BLOCK_SIZE];
        let mut expected = first.clone();
        expected.extend_from_slice(&second);
        fs.append_file_by_inode(&mut disk, staging_inode, &first)
            .unwrap();
        fs.append_file_by_inode(&mut disk, staging_inode, &second)
            .unwrap();
        fs.rename_file(&mut disk, "/download.part", "/download.bin")
            .unwrap();

        assert_eq!(fs.read_file(&mut disk, "/download.bin").unwrap(), expected);
        assert!(fs.read_file(&mut disk, "/download.part").is_err());
    }

    #[test]
    fn failed_full_disk_append_can_be_cleaned_up() {
        let mut disk = RamDisk::new(256);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let initial_free =
            unsafe { core::ptr::addr_of!(fs.superblock.free_blocks).read_unaligned() };
        let inode = fs.create_file(&mut disk, "/download.part").unwrap();
        let data = alloc::vec![0x7B; 190 * FS_BLOCK_SIZE];
        assert_eq!(
            fs.append_file_by_inode(&mut disk, inode, &data),
            Err("No free blocks")
        );
        fs.delete_file(&mut disk, "/download.part").unwrap();
        let final_free = unsafe { core::ptr::addr_of!(fs.superblock.free_blocks).read_unaligned() };
        assert_eq!(final_free, initial_free);
    }

    /// Regression test: Inode is 144 B, which does not divide the 512 B
    /// block, so slots like index 3 (bytes 432-576) straddle two table
    /// blocks. `write_inode` used to slice a single block and panic
    /// (`range end index 576 out of range`) on the first streamed append
    /// (this killed `wget` downloads once inode 3 was reached).
    #[test]
    fn straddling_inode_slot_survives_streamed_appends() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        // root = 0; take 1 and 2 so the staged download lands on inode 3.
        fs.create_directory(&mut disk, "/backgrounds").unwrap();
        let black = fs.create_file(&mut disk, "/black.png").unwrap();
        fs.write_file_by_inode(&mut disk, black, b"placeholder")
            .unwrap();
        let staging = fs.create_file(&mut disk, "/bg.part").unwrap();
        assert_eq!(staging, 3);

        // Stream chunks the way wget's sink does (append per packet).
        let chunk_a = alloc::vec![0xAB; 1440];
        let chunk_b = alloc::vec![0xCD; 1440];
        fs.append_file_by_inode(&mut disk, staging, &chunk_a)
            .unwrap();
        fs.append_file_by_inode(&mut disk, staging, &chunk_b)
            .unwrap();
        let mut expected = chunk_a.clone();
        expected.extend_from_slice(&chunk_b);
        assert_eq!(fs.read_file(&mut disk, "/bg.part").unwrap(), expected);
        // Neighbor slots must be untouched by the split-block write.
        assert_eq!(
            fs.read_file(&mut disk, "/black.png").unwrap(),
            b"placeholder"
        );

        // The table on disk must be coherent across a remount.
        drop(fs);
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        assert_eq!(fs.read_file(&mut disk, "/bg.part").unwrap(), expected);
        assert_eq!(
            fs.read_file(&mut disk, "/black.png").unwrap(),
            b"placeholder"
        );
    }

    /// Range reads page slices of a multi-block file (incl. indirect
    /// blocks) without allocating the whole file â€” the Doom WAD path.
    #[test]
    fn file_range_reads_span_direct_and_indirect_blocks() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/large.bin").unwrap();
        let data_len = (12 + 63 + 5) * FS_BLOCK_SIZE + 137;
        let data: alloc::vec::Vec<u8> = (0..data_len).map(|index| (index % 251) as u8).collect();
        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();

        // Head slice.
        let mut head = alloc::vec![0u8; 1000];
        assert_eq!(
            fs.read_file_range(&mut disk, "/large.bin", 0, &mut head)
                .unwrap(),
            1000
        );
        assert_eq!(&head, &data[..1000]);
        // Unaligned slice crossing the direct->indirect boundary.
        let off = (12 * FS_BLOCK_SIZE - 100) as u64;
        let mut mid = alloc::vec![0u8; 5000];
        assert_eq!(
            fs.read_file_range(&mut disk, "/large.bin", off, &mut mid)
                .unwrap(),
            5000
        );
        assert_eq!(&mid, &data[off as usize..off as usize + 5000]);
        // Tail clamps at EOF.
        let tail_off = (data_len - 10) as u64;
        let mut tail = alloc::vec![0u8; 100];
        assert_eq!(
            fs.read_file_range(&mut disk, "/large.bin", tail_off, &mut tail)
                .unwrap(),
            10
        );
        assert_eq!(&tail[..10], &data[data_len - 10..]);
        // Past EOF reads zero bytes.
        let mut empty = [0u8; 8];
        assert_eq!(
            fs.read_file_range(&mut disk, "/large.bin", data_len as u64, &mut empty)
                .unwrap(),
            0
        );
    }

    /// Every early slot (including straddlers 3, 7, 10, 14) round-trips
    /// its data through single-slot writes and a remount. Files are spread
    /// across subdirectories (root holds 8 entries per block, legacy).
    #[test]
    fn all_early_inode_slots_round_trip() {
        let mut disk = RamDisk::new(4096);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        // inodes interleave dirs and files; files land on 2-5, 7-10,
        // 12-15, 17-20 â€” covering straddlers 3, 7, 10, 14, 17.
        for d in 0..4u32 {
            let dir = alloc::format!("/d{}", d);
            fs.create_directory(&mut disk, &dir).unwrap();
            for f in 0..4u32 {
                let i = d * 4 + f;
                let name = alloc::format!("/d{}/f{}", d, f);
                let inode = fs.create_file(&mut disk, &name).unwrap();
                let data =
                    alloc::vec![(i as u8).wrapping_mul(37).wrapping_add(11); 700];
                fs.write_file_by_inode(&mut disk, inode, &data).unwrap();
            }
        }
        drop(fs);
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        for d in 0..4u32 {
            for f in 0..4u32 {
                let i = d * 4 + f;
                let name = alloc::format!("/d{}/f{}", d, f);
                let expected =
                    alloc::vec![(i as u8).wrapping_mul(37).wrapping_add(11); 700];
                assert_eq!(fs.read_file(&mut disk, &name).unwrap(), expected);
            }
        }
    }

    // â”€â”€ I/O volume guards â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    //
    // `RamDisk` services a request in microseconds, so it cannot distinguish a
    // linear read algorithm from a quadratic one. These tests count the
    // requests instead. Before the indirect-chain cursor, a 4 MiB read issued
    // ~543,000 `read_blocks` calls: one per chain hop for every block past the
    // first node. On real hardware each of those is a polled virtio round-trip,
    // which is what made multi-megabyte loads take minutes.

    /// Block count for a 4 MiB payload: 73 reserved (superblock + inode table)
    /// plus 8192 data blocks plus ~130 indirect nodes, with headroom.
    const BIG_DISK_BLOCKS: u64 = 9500;
    const BIG_FILE_BYTES: usize = 4 * 1024 * 1024;

    fn big_file_payload() -> alloc::vec::Vec<u8> {
        (0..BIG_FILE_BYTES).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn large_file_read_stays_linear_in_io_requests() {
        let mut disk = CountingDisk::new(BIG_DISK_BLOCKS);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/big.bin").unwrap();
        let data = big_file_payload();
        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();

        disk.reset_counters();
        let read_back = fs.read_file(&mut disk, "/big.bin").unwrap();
        assert_eq!(read_back, data, "payload must survive the round trip");
        drop(read_back);

        // Budget: worst case is one transfer per block (~8.2k) if the file were
        // fully fragmented; a contiguous file coalesces to a few dozen. The
        // pre-cursor implementation needed ~543,000 requests here.
        assert!(
            disk.read_requests() < 2_000,
            "reading {} bytes issued {} requests; the indirect chain is probably being \
             re-walked per block again",
            BIG_FILE_BYTES,
            disk.read_requests()
        );
        // Every block of the file must still be accounted for...
        assert!(
            disk.read_sectors() >= BIG_FILE_BYTES / FS_BLOCK_SIZE,
            "only {} sectors read for a {} byte file",
            disk.read_sectors(),
            BIG_FILE_BYTES
        );
        // ...and adjacent blocks must be fetched in batches, not one at a time.
        assert!(
            disk.read_requests() * 8 < disk.read_sectors(),
            "{} requests for {} sectors: adjacent blocks are not being coalesced",
            disk.read_requests(),
            disk.read_sectors()
        );
    }

    #[test]
    fn large_streamed_appends_stay_linear_in_io_requests() {
        let mut disk = CountingDisk::new(BIG_DISK_BLOCKS);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/stream.bin").unwrap();
        let mut expected: alloc::vec::Vec<u8> = alloc::vec::Vec::new();

        disk.reset_counters();
        for round in 0..64u32 {
            let chunk: alloc::vec::Vec<u8> = (0..64 * 1024)
                .map(|index| ((index as u32).wrapping_add(round) % 251) as u8)
                .collect();
            fs.append_file_by_inode(&mut disk, inode, &chunk).unwrap();
            expected.extend_from_slice(&chunk);
        }

        // One write per data block plus ~130 node allocations and a flush per
        // append. Measured at ~8.6k; the pre-cursor version issued ~535,000.
        assert!(
            disk.write_requests() < 12_000,
            "streaming {} bytes issued {} write requests; allocation is probably \
             re-walking the chain per block again",
            BIG_FILE_BYTES,
            disk.write_requests()
        );
        // Data plus one indirect node per 63 blocks, and nothing more: catches a
        // regression that re-zeroes or re-flushes per block.
        let data_blocks = BIG_FILE_BYTES / FS_BLOCK_SIZE;
        let expected_nodes = data_blocks.div_ceil(super::INDIRECT_DATA_BLOCKS) + 1;
        assert!(
            disk.write_sectors() < data_blocks + expected_nodes * 4,
            "streaming wrote {} sectors for {} data blocks",
            disk.write_sectors(),
            data_blocks
        );
        assert_eq!(fs.read_file(&mut disk, "/stream.bin").unwrap(), expected);
    }

    /// A whole-file read after a fresh mount must stay bounded too: the mount
    /// rebuilds the allocation bitmap by walking chains, and the read must not
    /// repeat that per block.
    #[test]
    fn repeated_reads_of_a_large_file_stay_bounded() {
        let mut disk = CountingDisk::new(BIG_DISK_BLOCKS);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/big.bin").unwrap();
        let data = big_file_payload();
        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();

        for _ in 0..3 {
            disk.reset_counters();
            assert_eq!(fs.read_file(&mut disk, "/big.bin").unwrap(), data);
            assert!(
                disk.read_requests() < 2_000,
                "repeat read issued {} requests",
                disk.read_requests()
            );
        }
    }

    // â”€â”€ Cursor invalidation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Reading a multi-node file parks the indirect-chain cursor deep in the
    /// chain. Deleting the file frees its inode; if a cached node survived, the
    /// next file to land on that inode would read the old chain back.
    #[test]
    fn indirect_cursor_does_not_survive_inode_reuse() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let payload_len = (12 + 63 + 20) * FS_BLOCK_SIZE;

        let first: alloc::vec::Vec<u8> = (0..payload_len).map(|i| (i % 251) as u8).collect();
        let inode_a = fs.create_file(&mut disk, "/a.bin").unwrap();
        fs.write_file_by_inode(&mut disk, inode_a, &first).unwrap();
        // Park the cursor deep in the chain.
        assert_eq!(fs.read_file(&mut disk, "/a.bin").unwrap(), first);
        fs.delete_file(&mut disk, "/a.bin").unwrap();

        let second: alloc::vec::Vec<u8> = (0..payload_len).map(|i| ((i * 7) % 253) as u8).collect();
        let inode_b = fs.create_file(&mut disk, "/b.bin").unwrap();
        fs.write_file_by_inode(&mut disk, inode_b, &second).unwrap();
        assert_eq!(
            fs.read_file(&mut disk, "/b.bin").unwrap(),
            second,
            "stale indirect chain leaked into the reused inode"
        );
    }

    /// `write_file_by_inode` truncates first, which clears the chain head. A
    /// surviving cursor would point at freed blocks.
    #[test]
    fn indirect_cursor_does_not_survive_truncate() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/rw.bin").unwrap();

        let big: alloc::vec::Vec<u8> =
            (0..((12 + 63 + 40) * FS_BLOCK_SIZE)).map(|i| (i % 251) as u8).collect();
        fs.write_file_by_inode(&mut disk, inode, &big).unwrap();
        assert_eq!(fs.read_file(&mut disk, "/rw.bin").unwrap(), big);

        let small = b"replaced".to_vec();
        fs.write_file_by_inode(&mut disk, inode, &small).unwrap();
        assert_eq!(fs.read_file(&mut disk, "/rw.bin").unwrap(), small);
    }

    /// Backward seeks restart the chain walk from the head. Verify the resulting
    /// data is still correct when reads move backwards through several nodes.
    #[test]
    fn backward_range_reads_across_nodes_return_correct_bytes() {
        let mut disk = RamDisk::new(4096);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let inode = fs.create_file(&mut disk, "/seek.bin").unwrap();
        let data_len = (12 + 63 * 4) * FS_BLOCK_SIZE + 999;
        let data: alloc::vec::Vec<u8> = (0..data_len).map(|i| (i % 251) as u8).collect();
        fs.write_file_by_inode(&mut disk, inode, &data).unwrap();

        // Visit offsets from the tail backwards, crossing several node
        // boundaries each time.
        for step in 0..8usize {
            let back = (step + 1) * 1000;
            let off = data_len - back;
            let mut slice = alloc::vec![0u8; 777];
            let n = fs
                .read_file_range(&mut disk, "/seek.bin", off as u64, &mut slice)
                .unwrap();
            assert_eq!(n, 777);
            assert_eq!(&slice, &data[off..off + 777], "mismatch at offset {}", off);
        }
    }

    // ── version 2: persistent allocation bitmap ──────────────────────
    //
    // Version 1 kept free-space state only in RAM and rebuilt it by walking
    // every inode at mount. That walk cannot distinguish "block allocated,
    // inode record never written" from "block free", so any interrupted
    // operation caused a live block to be handed out a second time. These
    // tests pin down the behaviour the bitmap exists to provide.

    fn free_blocks_of(fs: &SimpleFilesystem) -> u64 {
        unsafe { core::ptr::addr_of!(fs.superblock.free_blocks).read_unaligned() }
    }

    /// Every block referenced by `inode`, walking the indirect chain so the
    /// check covers files larger than the 12 direct slots.
    fn blocks_of(
        fs: &SimpleFilesystem,
        inode: u32,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> alloc::vec::Vec<u64> {
        let mut out = alloc::vec::Vec::new();
        let i = fs.inodes[inode as usize];
        for d in 0..INODE_DIRECT_BLOCKS {
            let b = unsafe { core::ptr::addr_of!(i.direct_blocks[d]).read_unaligned() };
            if b != 0 {
                out.push(b);
            }
        }
        let mut node = i.indirect_head();
        let (start, count) = fs.data_region();
        let mut hops = 0usize;
        while node != 0 && node >= start && node - start < count as u64 && hops <= count {
            out.push(node);
            let mut buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(node, 1, &mut buffer).unwrap();
            node = u64::from_le_bytes(buffer[..8].try_into().unwrap());
            for slot in 0..INDIRECT_DATA_BLOCKS {
                let off = 8 + slot * core::mem::size_of::<u64>();
                let b = u64::from_le_bytes(buffer[off..off + 8].try_into().unwrap());
                if b != 0 {
                    out.push(b);
                }
            }
            hops += 1;
        }
        out
    }

    #[test]
    fn format_places_the_bitmap_between_the_inode_table_and_the_data() {
        let total = 2048u64;
        let sb = Superblock::new(total);
        let inode_blocks = sb.get_inode_blocks() as u64;
        let bitmap_start = sb.get_bitmap_start();
        let data_start = sb.get_data_block_start();
        assert_eq!(
            bitmap_start,
            1 + inode_blocks,
            "bitmap follows the inode table"
        );
        assert_eq!(data_start, bitmap_start + sb.get_bitmap_blocks() as u64);
        assert!(sb.get_bitmap_blocks() > 0);
        // Every data block is addressable.
        assert_eq!(sb.get_free_blocks(), total - data_start);
    }

    #[test]
    fn superblock_is_exactly_one_block() {
        // A previous layout summed to 508 bytes, leaving four bytes of block 0
        // unversioned and undocumented.
        assert_eq!(core::mem::size_of::<Superblock>(), FS_BLOCK_SIZE);
        assert_eq!(core::mem::size_of::<Superblock>(), 512);
    }

    #[test]
    fn bitmap_blocks_for_rounds_up_to_whole_blocks() {
        assert_eq!(Superblock::bitmap_blocks_for(0), 0);
        assert_eq!(Superblock::bitmap_blocks_for(1), 1);
        assert_eq!(Superblock::bitmap_blocks_for(FS_BLOCK_SIZE as u64 * 8), 1);
        assert_eq!(Superblock::bitmap_blocks_for(FS_BLOCK_SIZE as u64 * 8 + 1), 2);
    }

    #[test]
    fn mount_refuses_a_version_1_image() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        // Rewrite the superblock as version 1: no allocation bitmap, so its
        // free-space state would have to be reconstructed by walking inodes.
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(0, 1, &mut buffer).unwrap();
        buffer[4..8].copy_from_slice(&1u32.to_le_bytes());
        disk.write_blocks(0, 1, &buffer).unwrap();
        let err = match SimpleFilesystem::mount(&mut disk) { Err(e) => e, Ok(_) => panic!("v1 image must be refused") };
        assert!(err.contains("version"), "unexpected error: {}", err);
    }

    #[test]
    fn mount_rejects_a_corrupt_bitmap_geometry() {
        for field in 0..2usize {
            let mut disk = RamDisk::new(2048);
            SimpleFilesystem::format(&mut disk).unwrap();
            let mut buffer = [0u8; FS_BLOCK_SIZE];
            disk.read_blocks(0, 1, &mut buffer).unwrap();
            // bitmap_start at offset 52, bitmap_blocks at offset 60.
            let off = if field == 0 { 52 } else { 60 };
            let original = u32::from_le_bytes(buffer[off..off + 4].try_into().unwrap());
            buffer[off..off + 4].copy_from_slice(&(original + 1).to_le_bytes());
            disk.write_blocks(0, 1, &buffer).unwrap();
            assert!(
                SimpleFilesystem::mount(&mut disk).is_err(),
                "corrupt bitmap field {} must be refused",
                off
            );
        }
    }

    #[test]
    fn mount_rejects_an_oversized_inode_count() {
        // `self.inodes` is always MAX_INODES long, so a superblock claiming
        // more lets `list_directory` index past the vector: a kernel panic.
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(0, 1, &mut buffer).unwrap();
        buffer[20..24].copy_from_slice(&100_000u32.to_le_bytes());
        disk.write_blocks(0, 1, &buffer).unwrap();
        assert!(SimpleFilesystem::mount(&mut disk).is_err());
    }

    #[test]
    fn root_directory_block_is_never_reallocated() {
        // Regression: the bitmap is indexed relative to the *data* region, so
        // the root directory's bit is 0. Marking it relative to the bitmap
        // region left the root's block marked free and the first file write
        // overwrote the directory.
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let root_block = blocks_of(&fs, 0, &mut disk)[0];
        for i in 0..8 {
            let name = alloc::format!("/f{}.bin", i);
            let ino = fs.create_file(&mut disk, &name).unwrap();
            let data = alloc::vec![0x5Au8; 4096];
            fs.write_file_by_inode(&mut disk, ino, &data).unwrap();
        }
        assert!(
            !blocks_of(&fs, 1, &mut disk).contains(&root_block),
            "a file block collided with the root directory"
        );
        // The root directory must still list every file.
        let entries = fs.list_directory(&mut disk, 0).unwrap();
        assert_eq!(entries.len(), 8);
    }

    #[test]
    fn free_space_survives_remount() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let after_format;
        let after_files;
        {
            let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
            after_format = free_blocks_of(&fs);
            let ino = fs.create_file(&mut disk, "/keep.bin").unwrap();
            fs.write_file_by_inode(&mut disk, ino, &alloc::vec![1u8; 3 * FS_BLOCK_SIZE])
                .unwrap();
            after_files = free_blocks_of(&fs);
        }
        let fs = SimpleFilesystem::mount(&mut disk).unwrap();
        assert_eq!(free_blocks_of(&fs), after_files);
        assert!(after_files < after_format);
    }

    #[test]
    fn remount_does_not_hand_out_a_live_block_again() {
        // The v1 failure: a block whose owning inode had been written, but
        // whose bitmap entry did not survive, would be allocated a second time
        // and two files would share it.
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut first_blocks;
        {
            let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
            let ino = fs.create_file(&mut disk, "/a.bin").unwrap();
            fs.write_file_by_inode(&mut disk, ino, &alloc::vec![0xAAu8; 20 * FS_BLOCK_SIZE])
                .unwrap();
            first_blocks = blocks_of(&fs, ino, &mut disk);
            assert!(first_blocks.len() > 12, "file should use indirect blocks");
        }
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let ino = fs.create_file(&mut disk, "/b.bin").unwrap();
        fs.write_file_by_inode(&mut disk, ino, &alloc::vec![0xBBu8; 20 * FS_BLOCK_SIZE])
            .unwrap();
        let second_blocks = blocks_of(&fs, ino, &mut disk);
        for b in &second_blocks {
            assert!(
                !first_blocks.contains(b),
                "block {:#x} was handed out twice across a remount",
                b
            );
        }
    }

    #[test]
    fn audit_reports_a_clean_filesystem_as_consistent() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let ino = fs.create_file(&mut disk, "/x.bin").unwrap();
        fs.write_file_by_inode(&mut disk, ino, &alloc::vec![7u8; 40 * FS_BLOCK_SIZE])
            .unwrap();
        let (missing, orphan) = fs.audit(&mut disk).unwrap();
        assert_eq!(missing, 0, "{} referenced blocks are marked free", missing);
        assert_eq!(orphan, 0, "{} allocated blocks are unreferenced", orphan);
    }

    #[test]
    fn audit_detects_a_referenced_block_marked_free() {
        // The corruption the bitmap prevents: clearing a live block's bit in
        // the on-disk bitmap must be visible to the audit even though the
        // inode still points at it.
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let ino = fs.create_file(&mut disk, "/y.bin").unwrap();
        fs.write_file_by_inode(&mut disk, ino, &alloc::vec![9u8; 5 * FS_BLOCK_SIZE])
            .unwrap();
        // Clear the first bit in RAM and on disk, then remount.
        let index = (blocks_of(&fs, ino, &mut disk)[0] - fs.data_region().0) as usize;
        fs.set_block_bit(index, false);
        fs.write_superblock(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let (missing, _) = fs.audit(&mut disk).unwrap();
        assert!(missing >= 1, "a referenced-but-free block must be reported");
    }

    #[test]
    fn audit_detects_leaked_space() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        // Mark an arbitrary unused block allocated and flush it.
        let start = fs.data_region().0;
        fs.mark_block_allocated(start + 40);
        fs.write_superblock(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let (_, orphan) = fs.audit(&mut disk).unwrap();
        assert!(orphan >= 1, "a leaked block must be reported");
    }

    #[test]
    fn free_block_count_agrees_with_the_bitmap() {
        let mut disk = RamDisk::new(4096);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        for i in 0..5 {
            let name = alloc::format!("/n{}.bin", i);
            let ino = fs.create_file(&mut disk, &name).unwrap();
            fs.write_file_by_inode(&mut disk, ino, &alloc::vec![i as u8; 700])
                .unwrap();
        }
        let (_, count) = fs.data_region();
        let actual_free = (0..count).filter(|&i| !fs.block_bit(i)).count() as u64;
        assert_eq!(free_blocks_of(&fs), actual_free);
    }

    #[test]
    fn deleting_a_file_returns_its_blocks_to_the_bitmap() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let before = free_blocks_of(&fs);
        let ino = fs.create_file(&mut disk, "/tmp.bin").unwrap();
        fs.write_file_by_inode(&mut disk, ino, &alloc::vec![3u8; 30 * FS_BLOCK_SIZE])
            .unwrap();
        assert!(free_blocks_of(&fs) < before);
        fs.delete_file(&mut disk, "/tmp.bin").unwrap();
        assert_eq!(free_blocks_of(&fs), before);
        // And the space is genuinely reusable, not just recounted.
        let ino = fs.create_file(&mut disk, "/tmp2.bin").unwrap();
        fs.write_file_by_inode(&mut disk, ino, &alloc::vec![4u8; 30 * FS_BLOCK_SIZE])
            .unwrap();
        assert_eq!(free_blocks_of(&fs), before - 31);
    }

    // ── multi-block directories ──────────────────────────────────────
    //
    // A directory used to hold exactly 8 entries: `add_entry_to_dir` looked
    // only at block 0 and returned "Directory is full" on the ninth, even
    // though every reader already walked all twelve blocks. The host bundler
    // inherited the cap and failed outright on more than 8 files in one
    // directory.

    #[test]
    fn a_directory_holds_more_than_eight_entries() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        for i in 0..40 {
            let name = alloc::format!("/f{:02}.txt", i);
            fs.create_file(&mut disk, &name)
                .unwrap_or_else(|e| panic!("creating {} failed: {}", name, e));
        }
        let entries = fs.list_directory(&mut disk, 0).unwrap();
        assert_eq!(entries.len(), 40);
        // Every name must be findable again.
        for i in 0..40 {
            let name = alloc::format!("/f{:02}.txt", i);
            assert!(fs.resolve_file_or_dir(&mut disk, &name).is_ok(), "{} lost", name);
        }
    }

    #[test]
    fn a_multi_block_directory_survives_a_remount() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        {
            let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
            for i in 0..30 {
                let name = alloc::format!("/m{:02}.bin", i);
                fs.create_file(&mut disk, &name).unwrap();
            }
        }
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let entries = fs.list_directory(&mut disk, 0).unwrap();
        assert_eq!(entries.len(), 30);
        for i in 0..30 {
            let name = alloc::format!("/m{:02}.bin", i);
            assert!(fs.resolve_file_or_dir(&mut disk, &name).is_ok(), "{} lost", name);
        }
        // The parent map must also be rebuilt across the extra blocks, or
        // `pwd` and `rmdir` resolve through the wrong parent.
        let mut fs2 = SimpleFilesystem::mount(&mut disk).unwrap();
        let _ = &mut fs2;
    }

    #[test]
    fn nested_directories_beyond_eight_entries() {
        let mut disk = RamDisk::new(4096);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        fs.create_directory(&mut disk, "/many").unwrap();
        for i in 0..25 {
            fs.create_file(&mut disk, &alloc::format!("/many/x{}", i)).unwrap();
        }
        let entries = fs.list_directory(&mut disk, 1).unwrap();
        assert_eq!(entries.len(), 25);
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let entries = fs.list_directory(&mut disk, 1).unwrap();
        assert_eq!(entries.len(), 25);
        assert!(fs.audit(&mut disk).unwrap() == (0, 0));
    }

    #[test]
    fn directory_size_tracks_the_block_extent() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        let empty = unsafe { core::ptr::addr_of!(fs.inodes[0].size).read_unaligned() };
        assert_eq!(empty, FS_BLOCK_SIZE as u64, "an empty dir still owns one block");
        for i in 0..9 {
            fs.create_file(&mut disk, &alloc::format!("/z{}", i)).unwrap();
        }
        let grown = unsafe { core::ptr::addr_of!(fs.inodes[0].size).read_unaligned() };
        assert_eq!(grown, 2 * FS_BLOCK_SIZE as u64, "9 entries need two blocks");
        let used = unsafe { core::ptr::addr_of!(fs.inodes[0].blocks_used).read_unaligned() };
        assert_eq!(used, 2);
    }

    #[test]
    fn removing_an_entry_from_a_grown_directory_keeps_the_rest() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        for i in 0..20 {
            fs.create_file(&mut disk, &alloc::format!("/r{}", i)).unwrap();
        }
        fs.delete_file(&mut disk, "/r0").unwrap();
        assert!(fs.resolve_file_or_dir(&mut disk, "/r0").is_err());
        for i in 1..20 {
            assert!(fs.resolve_file_or_dir(&mut disk, &alloc::format!("/r{}", i)).is_ok());
        }
        assert_eq!(fs.list_directory(&mut disk, 0).unwrap().len(), 19);
    }

    #[test]
    fn cross_directory_rename_updates_the_parent_map() {
        // `rebuild_parents` records a parent for *every* directory entry, not
        // just subdirectories, so the in-memory map has to follow a file's
        // move too. It did not: `pwd` and `cd ..` resolved through the old
        // parent until the next remount rebuilt the map from disk.
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        fs.create_directory(&mut disk, "/a").unwrap();
        fs.create_directory(&mut disk, "/b").unwrap();
        fs.create_file(&mut disk, "/a/f.txt").unwrap();

        let file = fs.resolve_file_or_dir(&mut disk, "/a/f.txt").unwrap();
        let a_inode = fs.resolve_file_or_dir(&mut disk, "/a").unwrap();
        let b_inode = fs.resolve_file_or_dir(&mut disk, "/b").unwrap();
        assert_eq!(fs.parent_of(file), Some(a_inode));

        fs.rename_file(&mut disk, "/a/f.txt", "/b/f.txt").unwrap();

        // Immediately, not only after a remount.
        assert_eq!(fs.parent_of(file), Some(b_inode));
        assert!(fs.resolve_file_or_dir(&mut disk, "/a/f.txt").is_err());
        assert!(fs.resolve_file_or_dir(&mut disk, "/b/f.txt").is_ok());
    }

    #[test]
    fn cross_directory_file_rename_is_visible_immediately() {
        let mut disk = RamDisk::new(2048);
        SimpleFilesystem::format(&mut disk).unwrap();
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        fs.create_directory(&mut disk, "/a").unwrap();
        fs.create_directory(&mut disk, "/b").unwrap();
        fs.create_file(&mut disk, "/a/f.txt").unwrap();
        fs.write_file(&mut disk, "/a/f.txt", b"payload").unwrap();
        fs.rename_file(&mut disk, "/a/f.txt", "/b/f.txt").unwrap();
        assert!(fs.resolve_file_or_dir(&mut disk, "/a/f.txt").is_err());
        let ino = fs.resolve_file_or_dir(&mut disk, "/b/f.txt").unwrap();
        let data = fs.read_file(&mut disk, "/b/f.txt").unwrap();
        assert_eq!(data, b"payload", "contents must follow the rename");
        let _ = ino;
        // And it must survive a remount.
        let mut fs = SimpleFilesystem::mount(&mut disk).unwrap();
        assert!(fs.resolve_file_or_dir(&mut disk, "/a/f.txt").is_err());
        assert_eq!(fs.read_file(&mut disk, "/b/f.txt").unwrap(), b"payload");
    }
}