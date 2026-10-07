//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Matzen Kernel Framework — Virtual Filesystem (VFS).
//!
//! Holds the mounted filesystem instance, path resolution, and the
//! boundaries every concrete driver implements. Concrete drivers live in
//! [`simple`], [`ext4`] and [`exfat`]; this module defines the shared
//! [`trait@Filesystem`] and the [`struct@Fs`] enum that unifies them, so no
//! caller knows which format a disk uses.

pub mod cache;
pub mod error;
pub mod exfat;
pub mod ext4;
pub mod partition;
pub mod simple;
pub mod vfs;

#[cfg(test)]
mod imagetests;

pub use error::FsError;
pub use partition::{Partition, PartitionDevice, PartitionTable, PartTable};
pub use simple::{
    DirectoryEntry, FileHandle, FileInfo, FileType, FsckReport, Inode, SimpleFilesystem, Superblock,
    FS_BLOCK_SIZE, INODE_DIRECT_BLOCKS, MAX_FILENAME_LEN, MAX_INODES,
};
pub use vfs::{Fs, Filesystem, Mount, Stat};
pub use vfs::{mount, mount_kind, mount_point, probe_device, set_cwd, unmount, is_mounted};
