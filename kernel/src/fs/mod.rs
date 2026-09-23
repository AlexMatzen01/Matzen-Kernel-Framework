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

/// Superblock structure (512 bytes)
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
    pub reserved: [u8; 456],   // Reserved for future use
}

impl Superblock {
    /// Magic number for SimplFS
    pub const MAGIC: u32 = 0x53464D4B; // "SFMK" in ASCII

    /// Current version
    pub const VERSION: u32 = 1;

    /// Create a new superblock
    pub fn new(total_blocks: u64) -> Self {
        let inode_blocks =
            (MAX_INODES * core::mem::size_of::<Inode>() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE;
        let data_block_start = 1 + inode_blocks as u64;
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
            reserved: [0; 456],
        }
    }
}

/// Inode structure (128 bytes)
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

/// Filesystem implementation
pub struct SimpleFilesystem {
    superblock: Superblock,
    inodes: Vec<Inode>,
    current_dir_inode: u32,
    parent: Vec<Option<u32>>, // parent[inode] = Some(parent_inode)
    allocated_blocks: Vec<u8>,
    allocation_cursor: usize,
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
            allocation_cursor: 0,
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

        // Initialize inode table
        let mut inodes = alloc::vec![Inode::new(); MAX_INODES];

        // Create root directory inode
        inodes[0] = Inode::new_directory();
        inodes[0].size = 0;
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
        if stored_version != Superblock::VERSION
            || stored_total > device.block_count()
            || (stored_inode_blocks as usize) < minimum_inode_blocks
            || stored_data_start != 1 + stored_inode_blocks as u64
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
            allocation_cursor: 0,
        };
        fs.rebuild_allocation_bitmap(device)?;
        // Rebuild parent map from directory entries
        // Ignore errors during rebuild (e.g., corrupted entries) — keep None
        let _ = fs.rebuild_parents(device);
        Ok(fs)
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

    /// Change current directory (by inode - legacy)
    pub fn change_directory(&mut self, inode: u32) -> Result<(), &'static str> {
        if inode >= self.superblock.inode_count {
            return Err("Invalid inode number");
        }

        if self.inodes[inode as usize].get_type() != FileType::Directory {
            return Err("Not a directory");
        }

        self.current_dir_inode = inode;
        Ok(())
    }

    // ── Path helpers ──────────────────────────────────────────

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
        if dir_inode >= self.superblock.inode_count {
            return Err("Invalid inode number");
        }
        self.reload_inodes(device)?;
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
        let dir_inode_copy = self.inodes[dir_inode as usize];
        let existing_block =
            unsafe { core::ptr::addr_of!(dir_inode_copy.direct_blocks[0]).read_unaligned() };
        if existing_block == 0 {
            return Err("Directory has no data blocks");
        }
        // For now only single block (legacy). TODO multi-block.
        let mut dir_buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(existing_block, 1, &mut dir_buffer)?;
        let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
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
                device.write_blocks(existing_block, 1, &dir_buffer)?;
                return Ok(());
            }
        }
        Err("Directory is full")
    }

    /// Remove entry from directory by child inode
    fn remove_entry_from_dir(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        dir_inode: u32,
        child_inode: u32,
    ) -> Result<(), &'static str> {
        let dir_inode_copy = self.inodes[dir_inode as usize];
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
                    if let Some(p) = self.parent[current as usize] {
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
                        let child_type = self.inodes[child_ino as usize].get_type();
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
                    if let Some(p) = self.parent[current as usize] {
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
                            let t = self.inodes[child_ino as usize].get_type();
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
            let parent = self.parent[cur as usize].unwrap_or(0);
            // Find name of cur in parent
            let mut found_name: Option<String> = None;
            // Scan parent dir entries
            let parent_inode_copy = self.inodes[parent as usize];
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
        new_dir_inode.size = 0;
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
            if let Some(p) = self.parent[cur as usize] {
                cur = p;
            } else {
                break;
            }
            depth += 1;
        }
        // Find parent of target
        let parent_inode = self.parent[target_inode as usize].ok_or("Invalid argument")?;
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
            return Ok(());
        }
        let target = self.resolve_path(device, trimmed)?;
        self.current_dir_inode = target;
        Ok(())
    }

    /// List files in a directory
    pub fn list_directory(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_number: u32,
    ) -> Result<Vec<FileInfo>, &'static str> {
        // Reload inodes from disk to get latest state
        self.reload_inodes(device)?;

        if inode_number >= self.superblock.inode_count {
            return Err("Invalid inode number");
        }

        let inode = &self.inodes[inode_number as usize];
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
                    let entry_inode = &self.inodes[entry.inode_number as usize];

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
        was_allocated
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

    fn rebuild_allocation_bitmap(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let (start, count) = self.data_region();
        let bitmap_len = count.saturating_add(7) / 8;
        self.allocated_blocks = Vec::new();
        self.allocated_blocks
            .try_reserve_exact(bitmap_len)
            .map_err(|_| "Unable to allocate filesystem bitmap")?;
        self.allocated_blocks.resize(bitmap_len, 0);
        self.allocation_cursor = 0;

        for inode_index in 0..self.inodes.len() {
            let inode = self.inodes[inode_index];
            for direct in 0..INODE_DIRECT_BLOCKS {
                let block =
                    unsafe { core::ptr::addr_of!(inode.direct_blocks[direct]).read_unaligned() };
                if block != 0 {
                    self.mark_block_allocated(block);
                }
            }
            let mut indirect = inode.indirect_head();
            for _ in 0..count {
                if indirect == 0 || indirect < start || indirect - start >= count as u64 {
                    break;
                }
                self.mark_block_allocated(indirect);
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                device.read_blocks(indirect, 1, &mut buffer)?;
                let next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
                for slot in 0..INDIRECT_DATA_BLOCKS {
                    let offset = 8 + slot * core::mem::size_of::<u64>();
                    let block = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
                    if block != 0 {
                        self.mark_block_allocated(block);
                    }
                }
                indirect = next;
            }
        }

        let used = (0..count).filter(|&index| self.block_bit(index)).count() as u64;
        unsafe {
            let free = (count as u64).saturating_sub(used);
            core::ptr::addr_of_mut!(self.superblock.free_blocks).write_unaligned(free);
        }
        self.write_superblock(device)
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

    /// Reload inode table from disk
    fn reload_inodes(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let inode_blocks =
            unsafe { core::ptr::addr_of!(self.superblock.inode_blocks).read_unaligned() };
        let inode_table_size = (inode_blocks as usize) * FS_BLOCK_SIZE;
        let mut inode_buffer = alloc::vec![0u8; inode_table_size];

        for i in 0..inode_blocks {
            let block_buffer =
                &mut inode_buffer[(i as usize * FS_BLOCK_SIZE)..((i as usize + 1) * FS_BLOCK_SIZE)];
            device.read_blocks(1 + i as u64, 1, block_buffer)?;
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
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        // Only write the portion of the inode table that actually exists on disk.
        // Writing past superblock.inode_blocks would overwrite data blocks (bug).
        let inode_table_bytes = (self.superblock.inode_blocks as usize) * FS_BLOCK_SIZE;
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(self.inodes.as_ptr() as *const u8, inode_table_bytes)
        };

        let mut block_num = 1u64;
        for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
            let mut block_buffer = [0u8; FS_BLOCK_SIZE];
            block_buffer[..chunk.len()].copy_from_slice(chunk);
            device.write_blocks(block_num, 1, &block_buffer)?;
            block_num += 1;
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
        if table_block >= self.superblock.inode_blocks as usize {
            return Err("Invalid inode table offset");
        }
        let slot_offset = byte_offset % FS_BLOCK_SIZE;
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(1 + table_block as u64, 1, &mut buffer)?;
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(self.inodes[inode_index]) as *const u8,
                inode_size,
            )
        };
        buffer[slot_offset..slot_offset + inode_size].copy_from_slice(inode_bytes);
        device.write_blocks(1 + table_block as u64, 1, &buffer)
    }

    /// Write superblock back to disk
    fn write_superblock(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
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
                let block = self.ensure_file_block(device, index, logical_block)?;
                let mut buffer = [0u8; FS_BLOCK_SIZE];
                if block_offset != 0 {
                    device.read_blocks(block, 1, &mut buffer)?;
                }
                let count = (FS_BLOCK_SIZE - block_offset).min(data.len() - consumed);
                buffer[block_offset..block_offset + count]
                    .copy_from_slice(&data[consumed..consumed + count]);
                device.write_blocks(block, 1, &buffer)?;
                self.inodes[index].size += count as u64;
                consumed += count;
            }
            Ok::<(), &'static str>(())
        })();
        if let Err(error) = append_result {
            let _ = self.write_inode(device, index);
            let _ = self.write_superblock(device);
            return Err(error);
        }

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

    fn ensure_file_block(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        logical_block: usize,
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
            let zero = [0u8; FS_BLOCK_SIZE];
            if let Err(error) = device.write_blocks(block, 1, &zero) {
                self.free_data_block(block);
                return Err(error);
            }
            self.inodes[inode_index].direct_blocks[logical_block] = block;
            return Ok(block);
        }

        let indirect_index = logical_block - INODE_DIRECT_BLOCKS;
        let node_index = indirect_index / INDIRECT_DATA_BLOCKS;
        let slot = indirect_index % INDIRECT_DATA_BLOCKS;
        if self.inodes[inode_index].indirect_head() == 0 {
            let root = self.allocate_data_block().ok_or("No free blocks")?;
            let zero = [0u8; FS_BLOCK_SIZE];
            if let Err(error) = device.write_blocks(root, 1, &zero) {
                self.free_data_block(root);
                return Err(error);
            }
            self.inodes[inode_index].set_indirect_head(root);
        }

        let mut node = self.inodes[inode_index].indirect_head();
        for _ in 0..node_index {
            let mut buffer = self.read_indirect_block(device, node)?;
            let next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
            if next != 0 {
                node = next;
                continue;
            }
            let next = self.allocate_data_block().ok_or("No free blocks")?;
            let zero = [0u8; FS_BLOCK_SIZE];
            if let Err(error) = device.write_blocks(next, 1, &zero) {
                self.free_data_block(next);
                return Err(error);
            }
            buffer[..8].copy_from_slice(&next.to_le_bytes());
            if let Err(error) = self.write_indirect_block(device, node, &buffer) {
                self.free_data_block(next);
                return Err(error);
            }
            node = next;
        }

        let mut buffer = self.read_indirect_block(device, node)?;
        let offset = 8 + slot * core::mem::size_of::<u64>();
        let existing = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
        if existing != 0 {
            return Ok(existing);
        }
        let block = self.allocate_data_block().ok_or("No free blocks")?;
        let zero = [0u8; FS_BLOCK_SIZE];
        if let Err(error) = device.write_blocks(block, 1, &zero) {
            self.free_data_block(block);
            return Err(error);
        }
        buffer[offset..offset + 8].copy_from_slice(&block.to_le_bytes());
        if let Err(error) = self.write_indirect_block(device, node, &buffer) {
            self.free_data_block(block);
            return Err(error);
        }
        Ok(block)
    }

    fn file_block_at(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
        logical_block: usize,
    ) -> Result<Option<u64>, &'static str> {
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
        let mut node = self.inodes[inode_index].indirect_head();
        for current_index in 0..=node_index {
            if node == 0 {
                return Ok(None);
            }
            let buffer = self.read_indirect_block(device, node)?;
            let offset = 8 + slot * core::mem::size_of::<u64>();
            if current_index == node_index {
                let block = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
                return Ok((block != 0).then_some(block));
            }
            node = u64::from_le_bytes(buffer[..8].try_into().unwrap());
        }
        Ok(None)
    }

    fn release_file_storage(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_index: usize,
    ) -> Result<(), &'static str> {
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
        // Reload inodes from disk to get latest state
        self.reload_inodes(device)?;

        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("Invalid argument");
        }
        let file_inode_num = self.resolve_file_or_dir(device, trimmed)?;
        if self.inodes[file_inode_num as usize].get_type() != FileType::File {
            return Err("Not a file");
        }

        let inode = &self.inodes[file_inode_num as usize];
        let file_size = inode.size as usize;

        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut data = Vec::new();
        data.try_reserve_exact(file_size)
            .map_err(|_| "Unable to allocate file buffer")?;
        let blocks_to_read = file_size.div_ceil(FS_BLOCK_SIZE);

        // Read data from blocks
        for i in 0..blocks_to_read {
            let Some(block_num) = self.file_block_at(device, file_inode_num as usize, i)? else {
                return Err("File block map is incomplete");
            };

            let mut block_buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut block_buffer)?;

            // Add data from this block
            let bytes_to_copy = core::cmp::min(FS_BLOCK_SIZE, file_size - data.len());
            data.extend_from_slice(&block_buffer[..bytes_to_copy]);
        }

        Ok(data)
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
    use super::{SimpleFilesystem, FS_BLOCK_SIZE};
    use crate::drivers::block::{BlockDevice, RamDisk};

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
}
