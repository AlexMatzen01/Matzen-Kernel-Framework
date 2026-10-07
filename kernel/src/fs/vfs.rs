//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! The VFS: a small, generic mount layer over the concrete drivers.
//!
//! Callers talk only to [`Vfs`]; each concrete filesystem implements
//! [`Filesystem`], and this layer picks the right mount by longest path
//! prefix. Drivers receive the mount's cached block device, so they keep
//! working unchanged on top of ATA, virtio-blk, or a RAM disk.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::block::{BlockDevice, DriveBlockDevice};
use crate::fs::cache::{flush_tag, CachedDevice};
use crate::fs::error::FsError;
use crate::fs::{exfat::ExFat, ext4::Ext4, simple::SimpleFilesystem, FileInfo};

/// Which on-disk format a mount uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Simple,
    Ext4,
    ExFat,
}

impl FsKind {
    pub fn name(&self) -> &'static str {
        match self {
            FsKind::Simple => "simplfs",
            FsKind::Ext4 => "ext4",
            FsKind::ExFat => "exfat",
        }
    }
}

/// One filesystem kind as a string, for shell completion.
pub fn kind_of(s: &str) -> Option<FsKind> {
    match s {
        "simplfs" | "simplefs" | "sfm" => Some(FsKind::Simple),
        "ext4" => Some(FsKind::Ext4),
        "exfat" | "exf" => Some(FsKind::ExFat),
        _ => None,
    }
}

/// Declared filesystem from `mount`/`mkfs` args. `Auto` means probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountFs {
    Auto,
    Simple,
    Ext4,
    ExFat,
}

/// Read-only metadata for the shell's `stat` command.
#[derive(Debug, Clone, Default)]
pub struct Stat {
    pub is_dir: bool,
    pub is_file: bool,
    pub is_link: bool,
    pub size: u64,
    pub links: u16,
    pub uid: u16,
    pub gid: u16,
    pub mode: u16,
    pub atime: u32,
    pub mtime: u32,
    pub ctime: u32,
}

/// A concrete filesystem instance, mounted on some mount point.
pub enum Fs {
    Simple(SimpleFilesystem),
    Ext4(Ext4),
    ExFat(ExFat),
}

impl Fs {
    pub fn name(&self) -> &'static str {
        match self {
            Fs::Simple(_) => "simplfs",
            Fs::Ext4(_) => "ext4",
            Fs::ExFat(_) => "exfat",
        }
    }
    pub fn is_read_only(&self) -> bool {
        match self {
            Fs::Simple(_) => false,
            Fs::Ext4(fs) => Ext4::is_read_only(fs),
            Fs::ExFat(fs) => ExFat::is_read_only(fs),
        }
    }

    // Inode-based API for shell compatibility
    pub fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        match self {
            Fs::Simple(s) => s.create_file(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => {
                let (parent, name) = parent_and_name(path)?;
                let parent_ino = e.resolve(dev, parent)?;
                e.create(dev, parent_ino, name).map_err(FsError::from)
            }
            Fs::ExFat(x) => {
                x.create_file(dev, path)?;
                let stat = x.stat_path(dev, path)?;
                Ok(stat.ino as u32)
            }
        }
    }

    pub fn write_file_by_inode(&mut self, dev: &mut dyn BlockDevice, ino: u32, data: &[u8]) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.write_file_by_inode(dev, ino, data).map_err(FsError::from),
            Fs::Ext4(e) => e.write_all(dev, ino, data).map_err(FsError::from),
            Fs::ExFat(_) => Err(FsError::Unsupported(String::from("write by inode on exfat"))),
        }
    }

    pub fn append_file_by_inode(&mut self, dev: &mut dyn BlockDevice, ino: u32, data: &[u8]) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.append_file_by_inode(dev, ino, data).map_err(FsError::from),
            Fs::Ext4(e) => e.append(dev, ino, data).map_err(FsError::from),
            Fs::ExFat(_) => Err(FsError::Unsupported(String::from("append by inode on exfat"))),
        }
    }

    pub fn create_directory(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        match self {
            Fs::Simple(s) => s.create_directory(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => {
                let (parent, name) = parent_and_name(path)?;
                let parent_ino = e.resolve(dev, parent)?;
                e.mkdir(dev, parent_ino, name).map_err(FsError::from)
            }
            Fs::ExFat(x) => x.mkdir_path(dev, path).map_err(FsError::from),
        }
    }

    pub fn delete_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.delete_file(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => e.unlink(dev, path).map_err(FsError::from),
            Fs::ExFat(x) => x.delete_file(dev, path).map_err(FsError::from),
        }
    }

    pub fn remove_directory(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.remove_directory(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => e.rmdir(dev, path).map_err(FsError::from),
            Fs::ExFat(x) => ExFat::remove_dir(x, dev, path).map_err(FsError::from),
        }
    }

    pub fn rename_file(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.rename_file(dev, old, new).map_err(FsError::from),
            Fs::Ext4(e) => e.rename(dev, old, new).map_err(FsError::from),
            Fs::ExFat(x) => x.rename(dev, old, new).map_err(FsError::from),
        }
    }

    pub fn resolve_file_or_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        match self {
            Fs::Simple(s) => s.resolve_file_or_dir(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => e.resolve(dev, path).map_err(FsError::from),
            Fs::ExFat(x) => x.resolve_path(dev, path).map(|f| f.first_cluster).map_err(FsError::from),
        }
    }

    pub fn is_dir(&self, _ino: u32) -> bool {
        false
    }

    pub fn is_file(&self, _ino: u32) -> bool {
        false
    }

    pub fn file_size(&self, _ino: u32) -> Result<u64, FsError> {
        Err(FsError::Unsupported(String::from("file_size by inode")))
    }

    pub fn current_directory(&self) -> u32 {
        match self {
            Fs::Simple(s) => s.current_directory(),
            Fs::Ext4(_) => 2,
            Fs::ExFat(_) => 2,
        }
    }

    pub fn current_path(&mut self, dev: &mut dyn BlockDevice) -> Result<String, FsError> {
        match self {
            Fs::Simple(s) => s.current_path(dev).map_err(FsError::from),
            Fs::Ext4(_) => Ok(String::from("/")),
            Fs::ExFat(_) => Ok(String::from("/")),
        }
    }

    pub fn change_directory_path(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => s.change_directory_path(dev, path).map_err(FsError::from),
            Fs::Ext4(_) => Ok(()),
            Fs::ExFat(_) => Ok(()),
        }
    }

    pub fn resolve_path(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32, FsError> {
        match self {
            Fs::Simple(s) => s.resolve_path(dev, path).map_err(FsError::from),
            Fs::Ext4(e) => e.resolve(dev, path).map_err(FsError::from),
            Fs::ExFat(x) => x.resolve_path(dev, path).map(|f| f.first_cluster).map_err(FsError::from),
        }
    }

    pub fn check(&mut self, dev: &mut dyn BlockDevice) -> Result<crate::fs::FsckReport, FsError> {
        match self {
            Fs::Simple(s) => s.check(dev).map_err(FsError::from),
            Fs::Ext4(_) => Err(FsError::Unsupported(String::from("fsck not supported on ext4"))),
            Fs::ExFat(_) => Err(FsError::Unsupported(String::from("fsck not supported on exfat"))),
        }
    }

    pub fn list_directory(&mut self, dev: &mut dyn BlockDevice, ino: u32) -> Result<Vec<crate::fs::FileInfo>, FsError> {        match self {
            Fs::Simple(s) => s.list_directory(dev, ino).map_err(FsError::from),
            Fs::Ext4(e) => {
                let entries = e.dir_entries(dev, ino)?;
                let mut out = Vec::new();
                for ent in entries {
                    let stat = Ext4::stat(e, dev, ent.ino)?;
                    out.push(crate::fs::FileInfo {
                        name: ent.name,
                        size: stat.size,
                        is_directory: ent.ft == 2,
                        inode_number: ent.ino,
                    });
                }
                Ok(out)
            },
            Fs::ExFat(x) => {
                let files = x.files(dev, ino)?;
                let mut out = Vec::new();
                for f in files {
                    out.push(crate::fs::FileInfo {
                        name: f.name,
                        size: f.size,
                        is_directory: f.is_dir,
                        inode_number: f.first_cluster,
                    });
                }
                Ok(out)
            },
        }
    }

    // ── Path-based inherent API (delegates to the Filesystem trait) ──
    //
    // These exist so shell/desktop code can call one concrete type without
    // importing the trait (which would make same-named inherent/trait
    // methods ambiguous). All paths are absolute within the mount.

    pub fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<crate::fs::FileInfo>, FsError> {
        <Fs as Filesystem>::list_dir(self, dev, path)
    }
    pub fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        <Fs as Filesystem>::read_file(self, dev, path)
    }
    pub fn read_at(&mut self, dev: &mut dyn BlockDevice, path: &str, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        <Fs as Filesystem>::read_at(self, dev, path, offset, out)
    }
    pub fn file_size_at(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError> {
        <Fs as Filesystem>::file_size(self, dev, path)
    }
    pub fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        <Fs as Filesystem>::write_file(self, dev, path, data)
    }
    pub fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        <Fs as Filesystem>::append_file(self, dev, path, data)
    }
    pub fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::create_dir(self, dev, path)
    }
    pub fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::remove_dir(self, dev, path)
    }
    pub fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::remove_file(self, dev, path)
    }
    pub fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::rename(self, dev, old, new)
    }
    pub fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError> {
        <Fs as Filesystem>::stat(self, dev, path)
    }
    pub fn supports_symlink(&self) -> bool {
        matches!(self, Fs::Ext4(_))
    }
    pub fn symlink(&mut self, dev: &mut dyn BlockDevice, link: &str, target: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::symlink(self, dev, link, target)
    }
    pub fn hard_link(&mut self, dev: &mut dyn BlockDevice, path: &str, target: &str) -> Result<(), FsError> {
        <Fs as Filesystem>::hard_link(self, dev, path, target)
    }
}

/// The filesystem-driver interface used by the VFS. Implemented by fresh
/// instances of each concrete driver; all paths are absolute (mount root
/// may be "/" so resolved relative to it).
pub trait Filesystem {
    fn name(&self) -> &'static str;
    fn is_read_only(&self) -> bool;

    fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<FileInfo>, FsError>;
    fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError>;
    fn read_at(
        &mut self,
        dev: &mut dyn BlockDevice,
        path: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError>;
    fn file_size(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError>;
    fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError>;
    fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError>;
    fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError>;
    fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError>;
    fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError>;
    fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError>;
    fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError>;
    fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError>;

    fn exists_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> bool {
        match self.stat(dev, path) {
            Ok(s) => s.is_dir,
            Err(_) => false,
        }
    }
    fn supports_symlink(&self) -> bool {
        false
    }
    fn symlink(&mut self, _dev: &mut dyn BlockDevice, _link: &str, _target: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported(String::from("symlinks")))
    }
    fn hard_link(&mut self, _dev: &mut dyn BlockDevice, _path: &str, _target: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported(String::from("hard links")))
    }
}

// ── Implementations (thin adapters) ─────────────────────────────────

impl Filesystem for SimpleFilesystem {
    fn name(&self) -> &'static str {
        "simplfs"
    }
    fn is_read_only(&self) -> bool {
        false
    }
    fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<FileInfo>, FsError> {
        let ino = if path.trim().is_empty() || path == "/" {
            0
        } else {
            self.resolve_file_or_dir(dev, path_clean(path))?
        };
        self.list_directory(dev, ino)
            .map_err(FsError::from)
    }
    fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        SimpleFilesystem::read_file(self, dev, path_clean(path)).map_err(FsError::from)
    }
    fn read_at(
        &mut self,
        dev: &mut dyn BlockDevice,
        path: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        self.read_file_range(dev, path_clean(path), offset, out)
            .map_err(FsError::from)
    }
    fn file_size(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError> {
        let ino = self.resolve_file_or_dir(dev, path_clean(path))?;
        SimpleFilesystem::file_size(self, ino).map_err(FsError::from)
    }
    fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        SimpleFilesystem::create_file(self, dev, path_clean(path)).map(|_| ()).map_err(FsError::from)
    }
    fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        SimpleFilesystem::write_file(self, dev, path_clean(path), data).map_err(FsError::from)
    }
    fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        let ino = self.resolve_file_or_dir(dev, path_clean(path))?;
        self.append_file_by_inode(dev, ino, data).map_err(FsError::from)
    }
    fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.create_directory(dev, path_clean(path)).map(|_| ()).map_err(FsError::from)
    }
    fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.remove_directory(dev, path_clean(path)).map_err(FsError::from)
    }
    fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.delete_file(dev, path_clean(path)).map_err(FsError::from)
    }
    fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        self.rename_file(dev, path_clean(old), path_clean(new)).map_err(FsError::from)
    }
    fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError> {
        let ino = self.resolve_file_or_dir(dev, path_clean(path))?;
        let is_dir = self.is_dir(ino);
        let size = if is_dir { 0 } else { SimpleFilesystem::file_size(self, ino).unwrap_or(0) };
        Ok(Stat {
            is_dir,
            is_file: !is_dir,
            is_link: false,
            size,
            links: 1,
            uid: 0,
            gid: 0,
            mode: if is_dir { 0o755 } else { 0o644 },
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl Filesystem for Ext4 {
    fn name(&self) -> &'static str {
        "ext4"
    }
    fn is_read_only(&self) -> bool {
        Ext4::is_read_only(self)
    }
    fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<FileInfo>, FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        let mut out = Vec::new();
        for e in self.dir_entries(dev, ino)? {
            out.push(FileInfo {
                name: e.name,
                size: 0,
                is_directory: e.ft == 2,
                inode_number: e.ino,
            });
        }
        for entry in out.iter_mut() {
            let full = join(path_clean(path), &entry.name);
            if let Ok(target) = self.resolve(dev, &full) {
                if let Ok(s) = Ext4::stat(self, dev, target) {
                    entry.size = s.size;
                }
            }
        }
        Ok(out)
    }
    fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        let s = Ext4::stat(self, dev, ino)?;
        let mut buf = alloc::vec![0u8; s.size.min(64 * 1024 * 1024) as usize];
        let n = Ext4::read_at(self, dev, ino, 0, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }
    fn read_at(&mut self, dev: &mut dyn BlockDevice, path: &str, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        Ext4::read_at(self, dev, ino, offset, out)
    }
    fn file_size(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError> {
        <Ext4 as Filesystem>::stat(self, dev, path_clean(path)).map(|s| s.size)
    }
    fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        let (parent, name) = parent_and_name(path_clean(path))?;
        let parent_ino = self.resolve(dev, parent)?;
        self.create(dev, parent_ino, name).map(|_| ())
    }
    fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        self.write_all(dev, ino, data)
    }
    fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        self.append(dev, ino, data)
    }
    fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        let (parent, name) = parent_and_name(path_clean(path))?;
        let parent_ino = self.resolve(dev, parent)?;
        self.mkdir(dev, parent_ino, name).map(|_| ())
    }
    fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.rmdir(dev, path_clean(path))
    }
    fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.unlink(dev, path_clean(path))
    }
    fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        Ext4::rename(self, dev, path_clean(old), path_clean(new))
    }
    fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError> {
        let ino = self.resolve(dev, path_clean(path))?;
        let s = Ext4::stat(self, dev, ino)?;
        Ok(Stat {
            is_dir: s.is_dir,
            is_file: s.is_reg,
            is_link: s.is_lnk,
            size: s.size,
            links: s.links,
            uid: s.uid,
            gid: s.gid,
            mode: s.mode,
            atime: s.atime,
            mtime: s.mtime,
            ctime: s.ctime,
        })
    }
    fn supports_symlink(&self) -> bool {
        true
    }
    fn symlink(&mut self, dev: &mut dyn BlockDevice, link: &str, target: &str) -> Result<(), FsError> {
        let (parent, name) = parent_and_name(path_clean(link))?;
        let parent_ino = self.resolve(dev, parent)?;
        self.symlink(dev, parent_ino, name, target).map(|_| ())
    }
    fn hard_link(&mut self, dev: &mut dyn BlockDevice, path: &str, target: &str) -> Result<(), FsError> {
        let target_ino = self.resolve(dev, path_clean(target))?;
        let (parent, name) = parent_and_name(path_clean(path))?;
        let parent_ino = self.resolve(dev, parent)?;
        self.hard_link(dev, parent_ino, name, target_ino)
    }
}

impl Filesystem for ExFat {
    fn name(&self) -> &'static str {
        "exfat"
    }
    fn is_read_only(&self) -> bool {
        ExFat::is_read_only(self)
    }
    fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<FileInfo>, FsError> {
        let dir = self.resolve_path(dev, path_clean(path))?;
        let mut out = Vec::new();
        for f in self.files(dev, dir.first_cluster)? {
            out.push(FileInfo {
                name: f.name,
                size: f.size,
                is_directory: f.is_dir,
                inode_number: f.first_cluster,
            });
        }
        Ok(out)
    }
    fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        ExFat::read_file(self, dev, path_clean(path))
    }
    fn read_at(&mut self, dev: &mut dyn BlockDevice, path: &str, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        // ExFat has no offset read; synthesise one from a whole read.
        let mut all = ExFat::read_file(self, dev, path_clean(path))?;
        let o = offset as usize;
        if o >= all.len() {
            return Ok(0);
        }
        let n = out.len().min(all.len() - o);
        out[..n].copy_from_slice(&all[o..o + n]);
        Ok(n)
    }
    fn file_size(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError> {
        self.stat_path(dev, path_clean(path)).map(|s| s.size)
    }
    fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        ExFat::create_file(self, dev, path_clean(path))
    }
    fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        ExFat::write_file(self, dev, path_clean(path), data)
    }
    fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        ExFat::append_file(self, dev, path_clean(path), data)
    }
    fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.mkdir_path(dev, path_clean(path)).map(|_| ())
    }
    fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        ExFat::remove_dir(self, dev, path_clean(path))
    }
    fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        self.delete_file(dev, path_clean(path))
    }
    fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        ExFat::rename(self, dev, path_clean(old), path_clean(new))
    }
    fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError> {
        let s = self.stat_path(dev, path_clean(path))?;
        Ok(Stat {
            is_dir: s.is_dir,
            is_file: s.is_reg,
            is_link: false,
            size: s.size,
            links: 1,
            uid: 0,
            gid: 0,
            mode: s.attributes,
            atime: 0,
            mtime: s.mtime,
            ctime: 0,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn path_clean(p: &str) -> &str {
    let t = p.trim();
    if t.is_empty() {
        return "/";
    }
    t
}

fn join(parent: &str, name: &str) -> String {
    let p = parent_clean(parent);
    if p == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", p, name)
    }
}

fn parent_clean(p: &str) -> &str {
    let t = p.trim();
    if t.is_empty() {
        return "/";
    }
    t
}

fn parent_and_name(path: &str) -> Result<(&str, &str), FsError> {
    let t = path.trim();
    if t.is_empty() || t == "/" {
        return Err(FsError::InvalidPath);
    }
    match t.rfind('/') {
        Some(0) => Ok(("/", &t[1..])),
        Some(i) => Ok((&t[..i], &t[i + 1..])),
        None => Ok(("/", t)),
    }
}

// ── Mount + VFS state ──────────────────────────────────────────────

impl Filesystem for Fs {
    fn name(&self) -> &'static str {
        match self {
            Fs::Simple(_) => "simplfs",
            Fs::Ext4(_) => "ext4",
            Fs::ExFat(_) => "exfat",
        }
    }
    fn is_read_only(&self) -> bool {
        match self {
            Fs::Simple(_) => false,
            Fs::Ext4(f) => Ext4::is_read_only(f),
            Fs::ExFat(f) => ExFat::is_read_only(f),
        }
    }
    fn list_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<FileInfo>, FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::list_dir(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::list_dir(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::list_dir(x, dev, path),
        }
    }
    fn read_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Vec<u8>, FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::read_file(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::read_file(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::read_file(x, dev, path),
        }
    }
    fn read_at(&mut self, dev: &mut dyn BlockDevice, path: &str, offset: u64, out: &mut [u8]) -> Result<usize, FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::read_at(s, dev, path, offset, out),
            Fs::Ext4(e) => <Ext4 as Filesystem>::read_at(e, dev, path, offset, out),
            Fs::ExFat(x) => <ExFat as Filesystem>::read_at(x, dev, path, offset, out),
        }
    }
    fn file_size(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64, FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::file_size(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::file_size(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::file_size(x, dev, path),
        }
    }
    fn create_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::create_file(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::create_file(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::create_file(x, dev, path),
        }
    }
    fn write_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::write_file(s, dev, path, data),
            Fs::Ext4(e) => <Ext4 as Filesystem>::write_file(e, dev, path, data),
            Fs::ExFat(x) => <ExFat as Filesystem>::write_file(x, dev, path, data),
        }
    }
    fn append_file(&mut self, dev: &mut dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::append_file(s, dev, path, data),
            Fs::Ext4(e) => <Ext4 as Filesystem>::append_file(e, dev, path, data),
            Fs::ExFat(x) => <ExFat as Filesystem>::append_file(x, dev, path, data),
        }
    }
    fn create_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::create_dir(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::create_dir(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::create_dir(x, dev, path),
        }
    }
    fn remove_dir(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::remove_dir(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::remove_dir(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::remove_dir(x, dev, path),
        }
    }
    fn remove_file(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::remove_file(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::remove_file(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::remove_file(x, dev, path),
        }
    }
    fn rename(&mut self, dev: &mut dyn BlockDevice, old: &str, new: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::rename(s, dev, old, new),
            Fs::Ext4(e) => <Ext4 as Filesystem>::rename(e, dev, old, new),
            Fs::ExFat(x) => <ExFat as Filesystem>::rename(x, dev, old, new),
        }
    }
    fn stat(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Stat, FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::stat(s, dev, path),
            Fs::Ext4(e) => <Ext4 as Filesystem>::stat(e, dev, path),
            Fs::ExFat(x) => <ExFat as Filesystem>::stat(x, dev, path),
        }
    }
    fn supports_symlink(&self) -> bool {
        matches!(self, Fs::Ext4(_))
    }
    fn symlink(&mut self, dev: &mut dyn BlockDevice, link: &str, target: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::symlink(s, dev, link, target),
            Fs::Ext4(e) => <Ext4 as Filesystem>::symlink(e, dev, link, target),
            Fs::ExFat(x) => <ExFat as Filesystem>::symlink(x, dev, link, target),
        }
    }
    fn hard_link(&mut self, dev: &mut dyn BlockDevice, path: &str, target: &str) -> Result<(), FsError> {
        match self {
            Fs::Simple(s) => <SimpleFilesystem as Filesystem>::hard_link(s, dev, path, target),
            Fs::Ext4(e) => <Ext4 as Filesystem>::hard_link(e, dev, path, target),
            Fs::ExFat(x) => <ExFat as Filesystem>::hard_link(x, dev, path, target),
        }
    }
}

pub struct Mount {
    pub drive: usize,
    pub point: String,
    pub fs: Fs,
    pub device: CachedDevice<Box<dyn BlockDevice>>,
    pub tag: usize,
}

impl Mount {
    fn new(drive: usize, point: String, fs: Fs, device: CachedDevice<Box<dyn BlockDevice>>, tag: usize) -> Mount {
        Mount { drive, point, fs, device, tag }
    }

    pub fn stat(&mut self, path: &str) -> Result<Stat, FsError> {
        let dev: &mut dyn BlockDevice = &mut self.device;
        self.fs.stat(dev, path_clean(path))
    }

    pub fn exists(&mut self, path: &str) -> bool {
        let dev: &mut dyn BlockDevice = &mut self.device;
        self.fs.stat(dev, path_clean(path)).map(|s| s.is_dir || s.is_file || s.is_link).unwrap_or(false)
    }
}

static MOUNT: Mutex<Option<Mount>> = Mutex::new(None);
static CWD: Mutex<String> = Mutex::new(String::new());

pub fn with<F, R>(f: F) -> Result<R, FsError>
where
    F: FnOnce(&mut Mount) -> Result<R, FsError>,
{
    let mut guard = MOUNT.lock();
    match guard.as_mut() {
        Some(m) => f(m),
        None => Err(FsError::NotMounted),
    }
}

pub fn cwd() -> String {
    CWD.lock().clone()
}

pub fn set_cwd(p: &str) {
    *CWD.lock() = String::from(p);
}

pub fn is_mounted() -> bool {
    MOUNT.lock().is_some()
}

pub fn probe_device(dev: &mut dyn BlockDevice) -> Option<FsKind> {
    if SimpleProbe::probe(dev) {
        Some(FsKind::Simple)
    } else if Ext4::probe(dev) {
        Some(FsKind::Ext4)
    } else if ExFat::probe(dev) {
        Some(FsKind::ExFat)
    } else {
        None
    }
}

struct SimpleProbe;
impl SimpleProbe {
    fn probe(dev: &mut dyn BlockDevice) -> bool {
        let mut buf = [0u8; 512];
        if dev.read_blocks(0, 1, &mut buf).is_err() {
            return false;
        }
        u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) == 0x53464D4B
    }
}

pub fn mount(drive: usize, kind: Option<FsKind>, point: &str) -> Result<(), FsError> {
    let mut dev = CachedDevice::new(
        Box::new(DriveBlockDevice::new(drive)) as Box<dyn BlockDevice>,
        drive,
    );
    let kind = match kind {
        Some(k) => k,
        None => probe_device(&mut dev).ok_or(FsError::WrongFs)?,
    };
    let fs = match kind {
        FsKind::Simple => Fs::Simple(SimpleFilesystem::mount(&mut dev).map_err(FsError::from)?),
        FsKind::Ext4 => Fs::Ext4(Ext4::mount(&mut dev)?),
        FsKind::ExFat => Fs::ExFat(ExFat::mount(&mut dev)?),
    };
    flush_tag(drive);
    *MOUNT.lock() = Some(Mount::new(drive, String::from(point), fs, dev, drive));
    *CWD.lock() = String::from("/");
    Ok(())
}

pub fn unmount() -> Result<(), FsError> {
    let mut g = MOUNT.lock();
    if g.is_none() {
        return Err(FsError::NotMounted);
    }
    *g = None;
    *CWD.lock() = String::new();
    Ok(())
}

pub fn mount_kind() -> Option<&'static str> {
    MOUNT.lock().as_ref().map(|m| m.fs.name())
}

pub fn mount_point() -> Option<String> {
    MOUNT.lock().as_ref().map(|m| m.point.clone())
}

pub fn format(drive: usize, kind: FsKind) -> Result<String, FsError> {
    let mut dev = DriveBlockDevice::new(drive);
    match kind {
        FsKind::Simple => {
            SimpleFilesystem::format(&mut dev)?;
            Ok(alloc::format!("Drive {} formatted as SimplFS", drive))
        }
        FsKind::Ext4 | FsKind::ExFat => Err(FsError::Unsupported(String::from(
            "that filesystem is read-only in this build",
        ))),
    }
}

pub fn disk_usage() -> Option<(u64, u64)> {
    None
}
