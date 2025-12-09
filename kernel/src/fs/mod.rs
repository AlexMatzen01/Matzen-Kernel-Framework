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
        entry.name[..len].copy_from_slice(&bytes[..len]);

        entry
    }

    /// Check if entry is in use
    pub fn is_used(&self) -> bool {
        self.inode_number != 0
    }

    /// Get filename as string
    pub fn get_name(&self) -> Result<String, &'static str> {
        let mut len = 0;
        for (i, &byte) in self.name.iter().enumerate() {
            if byte == 0 {
                len = i;
                break;
            }
        }

        core::str::from_utf8(&self.name[..len])
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
}

impl SimpleFilesystem {
    /// Create a new filesystem instance (not formatted)
    pub fn new() -> Self {
        SimpleFilesystem {
            superblock: Superblock::new(0),
            inodes: Vec::new(),
            current_dir_inode: 0,
        }
    }

    /// Format a device with the filesystem
    pub fn format(device: &mut dyn crate::drivers::block::BlockDevice) -> Result<(), &'static str> {
        use crate::serial_println;

        let total_blocks = device.block_count();
        let superblock = Superblock::new(total_blocks);
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

        let superblock = unsafe { core::ptr::read(buffer.as_ptr() as *const Superblock) };

        // Verify magic number
        if superblock.magic != Superblock::MAGIC {
            return Err("Invalid filesystem magic number");
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
                vec.push(*ptr.add(i));
            }
            vec
        };

        Ok(SimpleFilesystem {
            superblock,
            inodes,
            current_dir_inode: 0, // Start at root
        })
    }

    /// Get current directory inode number
    pub fn current_directory(&self) -> u32 {
        self.current_dir_inode
    }

    /// Change current directory
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

    /// List files in a directory
    pub fn list_directory(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        inode_number: u32,
    ) -> Result<Vec<FileInfo>, &'static str> {
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
                    *ptr
                };

                if entry.is_used() {
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

    /// Find a free data block
    fn find_free_data_block(&self) -> Option<u64> {
        // Simple linear search from data block start
        // In a real implementation, this would use a bitmap
        let start =
            unsafe { core::ptr::addr_of!(self.superblock.data_block_start).read_unaligned() };
        let total = unsafe { core::ptr::addr_of!(self.superblock.total_blocks).read_unaligned() };
        let free = unsafe { core::ptr::addr_of!(self.superblock.free_blocks).read_unaligned() };

        if free > 0 {
            // For simplicity, just allocate sequentially
            Some(start + (total - start - free))
        } else {
            None
        }
    }

    /// Reload inode table from disk
    fn reload_inodes(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let inode_blocks = unsafe { core::ptr::addr_of!(self.superblock.inode_blocks).read_unaligned() };
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
                self.inodes[i] = *ptr.add(i);
            }
        }

        Ok(())
    }

    /// Write inode table back to disk
    fn write_inodes(
        &self,
        device: &mut dyn crate::drivers::block::BlockDevice,
    ) -> Result<(), &'static str> {
        let inode_bytes = unsafe {
            core::slice::from_raw_parts(
                self.inodes.as_ptr() as *const u8,
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

        Ok(())
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

    /// Create a new file in the current directory
    pub fn create_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        filename: &str,
    ) -> Result<u32, &'static str> {
        if filename.len() >= MAX_FILENAME_LEN {
            return Err("Filename too long");
        }

        // Check if file already exists
        let files = self.list_directory(device, self.current_dir_inode)?;
        for file in files {
            if file.name == filename {
                return Err("File already exists");
            }
        }

        // Find free inode
        let inode_num = self.find_free_inode().ok_or("No free inodes")?;

        // Create new file inode
        self.inodes[inode_num as usize] = Inode::new_file();
        self.inodes[inode_num as usize].size = 0;
        self.inodes[inode_num as usize].blocks_used = 0;

        // Add directory entry
        let dir_inode = &self.inodes[self.current_dir_inode as usize];

        // Read first directory block
        let dir_block_num =
            unsafe { core::ptr::addr_of!(dir_inode.direct_blocks[0]).read_unaligned() };

        if dir_block_num == 0 {
            return Err("Directory has no data blocks");
        }

        let mut dir_buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(dir_block_num, 1, &mut dir_buffer)?;

        // Find empty directory entry
        let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
        let mut entry_added = false;

        for i in 0..entries_per_block {
            let offset = i * core::mem::size_of::<DirectoryEntry>();
            let entry = unsafe {
                let ptr = dir_buffer.as_ptr().add(offset) as *const DirectoryEntry;
                *ptr
            };

            if !entry.is_used() {
                // Found empty slot, add entry
                let new_entry = DirectoryEntry::new_with_name(filename, inode_num);
                unsafe {
                    let ptr = dir_buffer.as_mut_ptr().add(offset) as *mut DirectoryEntry;
                    *ptr = new_entry;
                }
                entry_added = true;
                break;
            }
        }

        if !entry_added {
            return Err("Directory is full");
        }

        // Write directory block back
        device.write_blocks(dir_block_num, 1, &dir_buffer)?;

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

    /// Write data to a file
    pub fn write_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        filename: &str,
        data: &[u8],
    ) -> Result<(), &'static str> {
        // Find the file
        let files = self.list_directory(device, self.current_dir_inode)?;
        let file_inode_num = files
            .iter()
            .find(|f| f.name == filename && !f.is_directory)
            .map(|f| f.inode_number)
            .ok_or("File not found")?;

        if data.len() > INODE_DIRECT_BLOCKS * FS_BLOCK_SIZE {
            return Err("File too large");
        }

        // Calculate blocks needed
        let blocks_needed = (data.len() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE;

        // Allocate blocks if needed
        for i in 0..blocks_needed {
            let inode = &self.inodes[file_inode_num as usize];
            let block_num = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };

            if block_num == 0 {
                // Allocate new block
                let new_block = self.find_free_data_block().ok_or("No free blocks")?;

                // Now mutably borrow inode to update it
                let inode_mut = &mut self.inodes[file_inode_num as usize];
                inode_mut.direct_blocks[i] = new_block;

                // Update superblock
                unsafe {
                    let free_blocks_ptr =
                        core::ptr::addr_of!(self.superblock.free_blocks) as *mut u64;
                    let current_val = core::ptr::read_unaligned(free_blocks_ptr);
                    core::ptr::write_unaligned(free_blocks_ptr, current_val.saturating_sub(1));
                }
            }
        }

        // Write data to blocks
        let inode = &self.inodes[file_inode_num as usize];
        for (i, chunk) in data.chunks(FS_BLOCK_SIZE).enumerate() {
            let block_num = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };

            let mut block_buffer = [0u8; FS_BLOCK_SIZE];
            block_buffer[..chunk.len()].copy_from_slice(chunk);
            device.write_blocks(block_num, 1, &block_buffer)?;
        }

        // Update inode metadata
        let inode = &mut self.inodes[file_inode_num as usize];
        inode.size = data.len() as u64;
        inode.blocks_used = blocks_needed as u32;

        // Write updates to disk
        self.write_inodes(device)?;
        self.write_superblock(device)?;

        Ok(())
    }

    /// Read data from a file
    pub fn read_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        filename: &str,
    ) -> Result<Vec<u8>, &'static str> {
        // Reload inodes from disk to get latest state
        self.reload_inodes(device)?;
        
        // Find the file
        let files = self.list_directory(device, self.current_dir_inode)?;
        let file_inode_num = files
            .iter()
            .find(|f| f.name == filename && !f.is_directory)
            .map(|f| f.inode_number)
            .ok_or("File not found")?;

        let inode = &self.inodes[file_inode_num as usize];
        let file_size = inode.size as usize;

        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut data = Vec::with_capacity(file_size);
        let blocks_to_read = inode.blocks_used as usize;

        // Read data from blocks
        for i in 0..blocks_to_read {
            let block_num = unsafe { core::ptr::addr_of!(inode.direct_blocks[i]).read_unaligned() };

            if block_num == 0 {
                break;
            }

            let mut block_buffer = [0u8; FS_BLOCK_SIZE];
            device.read_blocks(block_num, 1, &mut block_buffer)?;

            // Add data from this block
            let bytes_to_copy = core::cmp::min(FS_BLOCK_SIZE, file_size - data.len());
            data.extend_from_slice(&block_buffer[..bytes_to_copy]);
        }

        Ok(data)
    }

    /// Delete a file
    pub fn delete_file(
        &mut self,
        device: &mut dyn crate::drivers::block::BlockDevice,
        filename: &str,
    ) -> Result<(), &'static str> {
        // Find the file
        let files = self.list_directory(device, self.current_dir_inode)?;
        let file_info = files
            .iter()
            .find(|f| f.name == filename && !f.is_directory)
            .ok_or("File not found")?;

        let file_inode_num = file_info.inode_number;

        // Clear the inode
        let inode = &mut self.inodes[file_inode_num as usize];
        let blocks_used = inode.blocks_used;
        *inode = Inode::new(); // Clear to empty

        // Remove directory entry
        let dir_inode = &self.inodes[self.current_dir_inode as usize];
        let dir_block_num =
            unsafe { core::ptr::addr_of!(dir_inode.direct_blocks[0]).read_unaligned() };

        let mut dir_buffer = [0u8; FS_BLOCK_SIZE];
        device.read_blocks(dir_block_num, 1, &mut dir_buffer)?;

        // Find and clear the directory entry
        let entries_per_block = FS_BLOCK_SIZE / core::mem::size_of::<DirectoryEntry>();
        for i in 0..entries_per_block {
            let offset = i * core::mem::size_of::<DirectoryEntry>();
            let entry = unsafe {
                let ptr = dir_buffer.as_ptr().add(offset) as *const DirectoryEntry;
                *ptr
            };

            if entry.is_used() && entry.inode_number == file_inode_num {
                // Clear this entry
                unsafe {
                    let ptr = dir_buffer.as_mut_ptr().add(offset) as *mut DirectoryEntry;
                    *ptr = DirectoryEntry::new();
                }
                break;
            }
        }

        // Write directory block back
        device.write_blocks(dir_block_num, 1, &dir_buffer)?;

        // Update superblock
        unsafe {
            let free_inodes_ptr = core::ptr::addr_of!(self.superblock.free_inodes) as *mut u32;
            let current_inodes = core::ptr::read_unaligned(free_inodes_ptr);
            core::ptr::write_unaligned(free_inodes_ptr, current_inodes.saturating_add(1));

            let free_blocks_ptr = core::ptr::addr_of!(self.superblock.free_blocks) as *mut u64;
            let current_blocks = core::ptr::read_unaligned(free_blocks_ptr);
            core::ptr::write_unaligned(
                free_blocks_ptr,
                current_blocks.saturating_add(blocks_used as u64),
            );
        }

        // Write updates to disk
        self.write_inodes(device)?;
        self.write_superblock(device)?;

        Ok(())
    }
}
