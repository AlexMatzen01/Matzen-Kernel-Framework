//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Block Device Layer
//!
//! Provides an abstraction layer for block devices (disks, partitions, etc.)

use alloc::vec::Vec;

/// Size of a block (same as sector size)
pub const BLOCK_SIZE: usize = 512;

/// Block device trait
pub trait BlockDevice: Send {
    /// Read blocks from the device
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str>;

    /// Write blocks to the device
    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str>;

    /// Get the total number of blocks on the device
    fn block_count(&self) -> u64;

    /// Get the block size (usually 512 bytes)
    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }
}

/// ATA disk wrapper that implements BlockDevice
pub struct AtaBlockDevice {
    // Using phantom data to represent the ATA drive
    // In practice, this would coordinate with the ATA driver
}

impl AtaBlockDevice {
    pub fn new() -> Self {
        AtaBlockDevice {}
    }
}

impl BlockDevice for AtaBlockDevice {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if count > 255 {
            return Err("Too many blocks requested");
        }
        crate::drivers::ata::read_sectors(start_block, count as u8, buffer)
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if count > 255 {
            return Err("Too many blocks requested");
        }
        crate::drivers::ata::write_sectors(start_block, count as u8, buffer)
    }

    fn block_count(&self) -> u64 {
        // Default to a reasonable size - this should be detected from the drive
        // For now, assume 100MB (204800 sectors of 512 bytes)
        204800
    }
}

/// RAM disk for testing (stores data in memory)
pub struct RamDisk {
    data: Vec<u8>,
    block_count: u64,
}

impl RamDisk {
    /// Create a new RAM disk with the specified number of blocks
    pub fn new(block_count: u64) -> Self {
        let size = (block_count as usize) * BLOCK_SIZE;
        RamDisk {
            data: alloc::vec![0u8; size],
            block_count,
        }
    }
}

impl BlockDevice for RamDisk {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if start_block + count as u64 > self.block_count {
            return Err("Read beyond disk bounds");
        }

        let start_byte = (start_block as usize) * BLOCK_SIZE;
        let end_byte = start_byte + (count * BLOCK_SIZE);

        buffer[..count * BLOCK_SIZE].copy_from_slice(&self.data[start_byte..end_byte]);
        Ok(())
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if start_block + count as u64 > self.block_count {
            return Err("Write beyond disk bounds");
        }

        let start_byte = (start_block as usize) * BLOCK_SIZE;
        let end_byte = start_byte + (count * BLOCK_SIZE);

        self.data[start_byte..end_byte].copy_from_slice(&buffer[..count * BLOCK_SIZE]);
        Ok(())
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }
}
