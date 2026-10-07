//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Partition table parsing (MBR + GPT) and the partition block device.

use alloc::vec::Vec;
use core::convert::TryInto;

use crate::drivers::block::BlockDevice;

/// How the partition was declared on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartTable {
    /// No partition table: the filesystem starts at LBA 0.
    WholeDisk,
    /// Classic master boot record.
    Mbr,
    /// GUID partition table.
    Gpt,
}

/// One entry in a partition table.
#[derive(Debug, Clone, Copy)]
pub struct Partition {
    /// Zero-based index within the table.
    pub index: usize,
    pub table: PartTable,
    pub start_lba: u64,
    pub size_blocks: u64,
    /// MBR partition type / GPT type-GUID first byte, for display only.
    pub type_byte: u8,
}

/// Parsed partition table for a device.
#[derive(Debug, Clone)]
pub struct PartitionTable {
    pub table: PartTable,
    pub partitions: Vec<Partition>,
}

impl PartitionTable {
    /// Parse whichever table is present. A device with no recognizable
    /// table is returned as a single `WholeDisk` partition covering it.
    pub fn read(device: &mut dyn BlockDevice) -> PartitionTable {
        let mut mbr = [0u8; 512];
        if device.read_blocks(0, 1, &mut mbr).is_err() || mbr[510] != 0x55 || mbr[511] != 0xAA {
            return Self::whole_disk(device);
        }
        // Protective GPT: the first MBR entry has type 0xEE.
        let first_type = mbr[446 + 4];
        if first_type == 0xEE {
            let mut hdr = [0u8; 512];
            if device.read_blocks(1, 1, &mut hdr).is_ok() && &hdr[0..8] == b"EFI PART" {
                return Self::read_gpt(device, &hdr);
            }
        }
        Self::read_mbr(device, &mbr)
    }

    fn whole_disk(device: &dyn BlockDevice) -> PartitionTable {
        PartitionTable {
            table: PartTable::WholeDisk,
            partitions: alloc::vec![Partition {
                index: 0,
                table: PartTable::WholeDisk,
                start_lba: 0,
                size_blocks: device.block_count(),
                type_byte: 0,
            }],
        }
    }

    fn read_mbr(device: &mut dyn BlockDevice, mbr: &[u8; 512]) -> PartitionTable {
        let mut partitions = Vec::new();
        for i in 0..4 {
            let off = 446 + i * 16;
            let type_byte = mbr[off + 4];
            if type_byte == 0 || type_byte == 0xEE {
                continue;
            }
            let start = u32::from_le_bytes(mbr[off + 8..off + 12].try_into().unwrap()) as u64;
            let size = u32::from_le_bytes(mbr[off + 12..off + 16].try_into().unwrap()) as u64;
            if size == 0 || start + size > device.block_count() {
                continue;
            }
            partitions.push(Partition {
                index: i,
                table: PartTable::Mbr,
                start_lba: start,
                size_blocks: size,
                type_byte,
            });
        }
        if partitions.is_empty() {
            return Self::whole_disk(device);
        }
        PartitionTable {
            table: PartTable::Mbr,
            partitions,
        }
    }

    fn read_gpt(device: &mut dyn BlockDevice, hdr: &[u8; 512]) -> PartitionTable {
        let entry_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap());
        let num_entries = u32::from_le_bytes(hdr[80..84].try_into().unwrap()) as usize;
        let entry_size = u32::from_le_bytes(hdr[84..88].try_into().unwrap()) as usize;
        if entry_lba == 0 || num_entries == 0 || entry_size < 128 || entry_size > 4096 {
            return Self::whole_disk(device);
        }
        let per_block = 512 / entry_size;
        let mut partitions = Vec::new();
        let blocks = (num_entries * entry_size).div_ceil(512);
        let mut index = 0usize;
        for b in 0..blocks {
            let mut buf = [0u8; 512];
            if device
                .read_blocks(entry_lba + b as u64, 1, &mut buf)
                .is_err()
            {
                break;
            }
            for e in 0..per_block {
                let off = e * entry_size;
                let mut first16 = [0u8; 16];
                first16.copy_from_slice(&buf[off..off + 16]);
                if first16.iter().all(|&c| c == 0) {
                    index += 1;
                    continue;
                }
                let first = u64::from_le_bytes(buf[off + 32..off + 40].try_into().unwrap());
                let last = u64::from_le_bytes(buf[off + 40..off + 48].try_into().unwrap());
                if last < first {
                    index += 1;
                    continue;
                }
                let size = last - first + 1;
                if size == 0 || first + size > device.block_count() {
                    index += 1;
                    continue;
                }
                partitions.push(Partition {
                    index,
                    table: PartTable::Gpt,
                    start_lba: first,
                    size_blocks: size,
                    type_byte: buf[off],
                });
                index += 1;
            }
        }
        if partitions.is_empty() {
            return Self::whole_disk(device);
        }
        PartitionTable {
            table: PartTable::Gpt,
            partitions,
        }
    }
}

/// A `BlockDevice` view of a single partition, offset into its parent.
pub struct PartitionDevice {
    parent: alloc::boxed::Box<dyn BlockDevice>,
    offset: u64,
    size: u64,
}

impl PartitionDevice {
    pub fn new(parent: alloc::boxed::Box<dyn BlockDevice>, start_lba: u64, size: u64) -> Self {
        Self {
            parent,
            offset: start_lba,
            size,
        }
    }
}

impl BlockDevice for PartitionDevice {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if start_block + count as u64 > self.size {
            return Err("Read beyond partition bounds");
        }
        self.parent.read_blocks(start_block + self.offset, count, buffer)
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if start_block + count as u64 > self.size {
            return Err("Write beyond partition bounds");
        }
        self.parent.write_blocks(start_block + self.offset, count, buffer)
    }

    fn block_count(&self) -> u64 {
        self.size
    }

    fn block_size(&self) -> usize {
        self.parent.block_size()
    }
}

/// A stable cache tag for a partition: `(drive << 8) | partition_index`,
/// well above real drive indices (< 16) and below the whole-disk tag space.
pub fn partition_tag(drive_index: usize, partition_index: usize) -> usize {
    0x1000 + drive_index * 256 + partition_index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::block::RamDisk;

    #[test]
    fn an_empty_device_is_reported_as_whole_disk_filesystem() {
        let mut disk = RamDisk::new(64);
        let table = PartitionTable::read(&mut disk);
        assert_eq!(table.table, PartTable::WholeDisk);
        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].start_lba, 0);
    }

    #[test]
    fn gpt_with_one_partition_is_parsed() {
        let mut disk = RamDisk::new(2048);
        // Protective MBR.
        let mut mbr = [0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        mbr[446 + 4] = 0xEE;
        disk.write_blocks(0, 1, &mbr).unwrap();
        // GPT header at LBA 1: entries at LBA 2, two 128-byte entries.
        let mut hdr = [0u8; 512];
        hdr[0..8].copy_from_slice(b"EFI PART");
        hdr[72..80].copy_from_slice(&2u64.to_le_bytes());
        hdr[80..84].copy_from_slice(&2u32.to_le_bytes());
        hdr[84..88].copy_from_slice(&128u32.to_le_bytes());
        disk.write_blocks(1, 1, &hdr).unwrap();
        // Entry 0: Linux filesystem GUID-ish type, LBA 34..133.
        let mut entries = [0u8; 512];
        entries[0..16].copy_from_slice(&[0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);
        entries[32..40].copy_from_slice(&34u64.to_le_bytes());
        entries[40..48].copy_from_slice(&133u64.to_le_bytes());
        disk.write_blocks(2, 1, &entries).unwrap();
        let table = PartitionTable::read(&mut disk);
        assert_eq!(table.table, PartTable::Gpt);
        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].start_lba, 34);
        assert_eq!(table.partitions[0].size_blocks, 100);
    }

    #[test]
    fn partition_device_offsets_and_bounds() {
        use alloc::boxed::Box;
        let mut raw = RamDisk::new(64);
        let mut pattern = [0u8; 512];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        raw.write_blocks(10, 1, &pattern).unwrap();
        let mut part = PartitionDevice::new(Box::new(raw) as Box<dyn BlockDevice>, 10, 4);
        assert_eq!(part.block_count(), 4);
        let mut back = [0u8; 512];
        part.read_blocks(0, 1, &mut back).unwrap();
        assert_eq!(back, pattern);
        // Out-of-partition access is refused, never passed to the parent.
        assert!(part.read_blocks(4, 1, &mut back).is_err());
        assert!(part.write_blocks(3, 2, &back).is_err());
    }

    #[test]
    fn mbr_with_two_partitions_is_parsed() {
        let mut disk = RamDisk::new(8192);
        let mut mbr = [0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        // Entry 0: type 0x83, start 10, 100 blocks
        mbr[446 + 4] = 0x83;
        mbr[446 + 8..446 + 12].copy_from_slice(&10u32.to_le_bytes());
        mbr[446 + 12..446 + 16].copy_from_slice(&100u32.to_le_bytes());
        // Entry 1: type 0x0C, start 200, 100 blocks
        mbr[446 + 16 + 4] = 0x0C;
        mbr[446 + 16 + 8..446 + 16 + 12].copy_from_slice(&200u32.to_le_bytes());
        mbr[446 + 16 + 12..446 + 16 + 16].copy_from_slice(&100u32.to_le_bytes());
        disk.write_blocks(0, 1, &mbr).unwrap();
        let table = PartitionTable::read(&mut disk);
        assert_eq!(table.table, PartTable::Mbr);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].start_lba, 10);
        assert_eq!(table.partitions[1].type_byte, 0x0C);
    }
}
