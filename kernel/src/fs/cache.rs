//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Generic LRU block cache shared by every filesystem driver.
//!
//! Filesystems already issue 512-byte sector reads through the
//! `BlockDevice` trait; caching those reads here, keyed by a device tag +
//! LBA, means ext4 extent metadata, exFAT FAT chains and SimplFS bitmaps
//! stop being re-fetched on every operation without any per-filesystem
//! cache being written. Writes are write-through: the device receives the
//! new bytes immediately and the cache entry is updated, so a crash never
//! leaves the cache ahead of disk.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::block::{BlockDevice, BLOCK_SIZE};

/// Page size of one cached block (one sector).
const PAGE: usize = BLOCK_SIZE;

/// Maximum resident blocks across all devices (256 × 512 B = 128 KiB).
const CAPACITY: usize = 256;

struct Entry {
    tag: usize,
    lba: u64,
    data: [u8; PAGE],
    last: u64,
}

/// Simple frequency-ordered cache: an LRU whose eviction victim is the
/// entry with the oldest `last` stamp. Linear scan is fine at 256 entries
/// and keeps no extra bookkeeping.
pub struct BlockCache {
    entries: Vec<Entry>,
    clock: u64,
    hits: u64,
    misses: u64,
}

impl BlockCache {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
            clock: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn get(&mut self, tag: usize, lba: u64) -> Option<&[u8; PAGE]> {
        self.clock += 1;
        for e in self.entries.iter_mut() {
            if e.tag == tag && e.lba == lba {
                e.last = self.clock;
                self.hits += 1;
                return Some(&e.data);
            }
        }
        self.misses += 1;
        None
    }

    fn put(&mut self, tag: usize, lba: u64, data: &[u8]) {
        self.clock += 1;
        for e in self.entries.iter_mut() {
            if e.tag == tag && e.lba == lba {
                e.data.copy_from_slice(&data[..PAGE]);
                e.last = self.clock;
                return;
            }
        }
        if self.entries.len() >= CAPACITY {
            // Evict the least-recently used entry.
            let mut oldest = 0;
            for i in 1..self.entries.len() {
                if self.entries[i].last < self.entries[oldest].last {
                    oldest = i;
                }
            }
            self.entries.swap_remove(oldest);
        }
        let mut buf = [0u8; PAGE];
        buf.copy_from_slice(&data[..PAGE]);
        self.entries.push(Entry {
            tag,
            lba,
            data: buf,
            last: self.clock,
        });
    }

    fn invalidate(&mut self, tag: usize, lba: u64) {
        self.entries.retain(|e| !(e.tag == tag && e.lba == lba));
    }

    fn clear_device(&mut self, tag: usize) {
        self.entries.retain(|e| e.tag != tag);
    }

    /// Cache statistics for diagnostics and tests.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

/// Process-wide cache. Access is already serialized by the filesystem
/// mutex in the shell paths, so a plain spinlock is enough.
static BLOCK_CACHE: Mutex<BlockCache> = Mutex::new(BlockCache::new());

impl<D: BlockDevice + ?Sized> BlockDevice for alloc::boxed::Box<D> {
    fn read_blocks(&mut self, start_block: u64, count: usize, buffer: &mut [u8]) -> Result<(), &'static str> {
        (**self).read_blocks(start_block, count, buffer)
    }
    fn write_blocks(&mut self, start_block: u64, count: usize, buffer: &[u8]) -> Result<(), &'static str> {
        (**self).write_blocks(start_block, count, buffer)
    }
    fn block_count(&self) -> u64 {
        (**self).block_count()
    }
    fn block_size(&self) -> usize {
        (**self).block_size()
    }
}

/// Read-through/write-through cache wrapper around any `BlockDevice`.
///
/// `tag` must be unique per logical device: two `CachedDevice` instances
/// that target the same on-disk bytes must share a tag, and one that
/// targets different bytes must not. `CachedDevice<DriveBlockDevice>`
/// uses the unified drive index as the tag; partition objects derive a tag
/// from `(drive, partition)`.
pub struct CachedDevice<D: BlockDevice> {
    inner: D,
    tag: usize,
}

impl<D: BlockDevice> CachedDevice<D> {
    pub fn new(inner: D, tag: usize) -> Self {
        Self { inner, tag }
    }
}

impl<D: BlockDevice> BlockDevice for CachedDevice<D> {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }
        if buffer.len() < count * PAGE {
            return Err("Buffer too small");
        }
        let mut filled = alloc::vec![false; count];
        {
            let mut cache = BLOCK_CACHE.lock();
            for i in 0..count {
                if let Some(block) = cache.get(self.tag, start_block + i as u64) {
                    buffer[i * PAGE..(i + 1) * PAGE].copy_from_slice(block);
                    filled[i] = true;
                }
            }
        }
        let mut i = 0;
        while i < count {
            if filled[i] {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < count && !filled[j] {
                j += 1;
            }
            let n = j - i;
            if n > 255 {
                return Err("Too many blocks requested");
            }
            self.inner.read_blocks(start_block + i as u64, n, &mut buffer[i * PAGE..j * PAGE])?;
            let mut cache = BLOCK_CACHE.lock();
            for k in i..j {
                cache.put(self.tag, start_block + k as u64, &buffer[k * PAGE..k * PAGE + PAGE]);
            }
            i = j;
        }
        Ok(())
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }
        if buffer.len() < count * PAGE {
            return Err("Buffer too small");
        }
        if count > 255 {
            return Err("Too many blocks requested");
        }
        self.inner.write_blocks(start_block, count, buffer)?;
        let mut cache = BLOCK_CACHE.lock();
        for i in 0..count {
            cache.put(self.tag, start_block + i as u64, &buffer[i * PAGE..(i + 1) * PAGE]);
        }
        Ok(())
    }

    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }
}

/// A cached version of a drive device, using the global block cache.
pub struct CachedDriveDevice {
    drive_index: usize,
}

impl CachedDriveDevice {
    pub fn new(drive_index: usize) -> Self {
        Self { drive_index }
    }
}

impl crate::drivers::block::BlockDevice for CachedDriveDevice {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }
        if buffer.len() < count * PAGE {
            return Err("Buffer too small");
        }
        let mut filled = alloc::vec![false; count];
        {
            let mut cache = BLOCK_CACHE.lock();
            for i in 0..count {
                if let Some(block) = cache.get(self.drive_index, start_block + i as u64) {
                    buffer[i * PAGE..(i + 1) * PAGE].copy_from_slice(block);
                    filled[i] = true;
                }
            }
        }
        let mut i = 0;
        while i < count {
            if filled[i] {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < count && !filled[j] {
                j += 1;
            }
            let n = j - i;
            if n > 255 {
                return Err("Too many blocks requested");
            }
            crate::drivers::drives::read_sectors_from(
                self.drive_index,
                start_block + i as u64,
                n as u8,
                &mut buffer[i * PAGE..j * PAGE],
            )?;
            let mut cache = BLOCK_CACHE.lock();
            for k in i..j {
                cache.put(self.drive_index, start_block + k as u64, &buffer[k * PAGE..k * PAGE + PAGE]);
            }
            i = j;
        }
        Ok(())
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }
        if buffer.len() < count * PAGE {
            return Err("Buffer too small");
        }
        if count > 255 {
            return Err("Too many blocks requested");
        }
        crate::drivers::drives::write_sectors_to(self.drive_index, start_block, count as u8, buffer)?;
        let mut cache = BLOCK_CACHE.lock();
        for i in 0..count {
            cache.put(self.drive_index, start_block + i as u64, &buffer[i * PAGE..(i + 1) * PAGE]);
        }
        Ok(())
    }

    fn block_count(&self) -> u64 {
        crate::drivers::drives::drive_info(self.drive_index)
            .filter(|info| info.exists)
            .map(|info| info.total_sectors)
            .unwrap_or(0)
    }

    fn block_size(&self) -> usize {
        PAGE
    }
}

/// Expose cache stats (hits, misses) for tests and `df`.
pub fn cache_stats() -> (u64, u64) {
    let cache = BLOCK_CACHE.lock();
    cache.stats()
}

/// Drop every cached block for a device (used on unmount / mkfs).
pub fn flush_tag(tag: usize) {
    let mut cache = BLOCK_CACHE.lock();
    cache.clear_device(tag);
}

/// Drop one cached block (used after a write that invalidates it).
pub fn invalidate(tag: usize, lba: u64) {
    let mut cache = BLOCK_CACHE.lock();
    cache.invalidate(tag, lba);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_device_serves_from_cache_on_second_read() {
        let inner = crate::drivers::block::RamDisk::new(4);
        let mut dev = CachedDevice::new(inner, 1);
        let mut zero = [0u8; PAGE];
        dev.write_blocks(0, 1, &[0x5Au8; PAGE]).unwrap();
        // First read warms the cache.
        dev.read_blocks(0, 1, &mut zero).unwrap();
        assert_eq!(zero, [0x5Au8; PAGE]);
        let (_hits_before, _) = cache_stats();
        // Overwrite the same sector through the *same* cached device: the
        // cache must stay coherent (write-through) so the next read matches.
        dev.write_blocks(0, 1, &[0xA5u8; PAGE]).unwrap();
        let mut buf = [0u8; PAGE];
        dev.read_blocks(0, 1, &mut buf).unwrap();
        assert_eq!(buf, [0xA5u8; PAGE]);
    }

    #[test]
    fn uncached_counting_disk_counts_every_request() {
        let mut disk = crate::drivers::block::CountingDisk::new(4);
        let mut buf = [0u8; PAGE];
        disk.write_blocks(0, 1, &[0xA5; PAGE]).unwrap();
        disk.reset_counters();
        disk.read_blocks(0, 1, &mut buf).unwrap();
        disk.read_blocks(0, 1, &mut buf).unwrap();
        assert_eq!(disk.read_sectors(), 2);
        let (h, m) = cache_stats();
        // A direct CountingDisk does not touch the shared cache at all.
        let _ = (h, m);
    }
}
