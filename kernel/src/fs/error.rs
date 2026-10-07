//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Generic filesystem error type shared by the VFS and every driver.

use alloc::string::String;

/// A device-independent filesystem error.
///
/// Drivers convert their internal failures into this type at the VFS
/// boundary, so `&'static str` messages never leak out of a driver and the
/// shell/desktop can match on stable kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    /// The requested file or directory does not exist.
    NotFound,
    /// The name already exists in its parent directory.
    AlreadyExists,
    /// A directory was required but a non-directory was given.
    NotDirectory,
    /// A file/directory was required but a directory was given.
    IsDirectory,
    /// The caller lacks the access the operation requires.
    PermissionDenied,
    /// The path is syntactically invalid.
    InvalidPath,
    /// The mounted filesystem is read-only.
    ReadOnly,
    /// No free blocks or inodes remain.
    NoSpace,
    /// The underlying block device failed.
    Io(String),
    /// The feature or operation is not supported by this driver.
    Unsupported(String),
    /// On-disk metadata failed validation; refusing to continue.
    CorruptFilesystem(String),
    /// The mount is busy (open files / active cwd); cannot unmount.
    Busy,
    /// No filesystem is mounted.
    NotMounted,
    /// The probe could not identify a filesystem on the device.
    WrongFs,
    /// The device is not ready or not present.
    Device(String),
}

impl FsError {
    pub fn as_str(&self) -> &'static str {
        match self {
            FsError::NotFound => "No such file or directory",
            FsError::AlreadyExists => "File already exists",
            FsError::NotDirectory => "Not a directory",
            FsError::IsDirectory => "Is a directory",
            FsError::PermissionDenied => "Permission denied",
            FsError::InvalidPath => "Invalid path",
            FsError::ReadOnly => "Read-only filesystem",
            FsError::NoSpace => "No space left on device",
            FsError::Io(_) => "I/O error",
            FsError::Unsupported(_) => "Unsupported filesystem feature",
            FsError::CorruptFilesystem(_) => "Corrupt filesystem",
            FsError::Busy => "Device or resource busy",
            FsError::NotMounted => "Filesystem not mounted",
            FsError::WrongFs => "Unknown or unsupported filesystem",
            FsError::Device(_) => "Device error",
        }
    }
}

impl<'a> From<&'a str> for FsError {
    fn from(s: &'a str) -> Self {
        match s {
            "No such file or directory" => FsError::NotFound,
            "File already exists" | "File exists" => FsError::AlreadyExists,
            "Not a directory" => FsError::NotDirectory,
            "Is a directory" => FsError::IsDirectory,
            "Permission denied" => FsError::PermissionDenied,
            "Invalid argument" | "Invalid path" => FsError::InvalidPath,
            "Read-only filesystem" => FsError::ReadOnly,
            "No free blocks" | "No space left on device" => FsError::NoSpace,
            "Directory not empty" => FsError::NotDirectory,
            "Filesystem not mounted" => FsError::NotMounted,
            "Directory is full" | "Directory is full (12 blocks max)" => {
                FsError::NoSpace
            }
            "Invalid filesystem magic number" | "Wrong filesystem" => FsError::WrongFs,
            "No free inodes" => FsError::NoSpace,
            other if other.contains("version") || other.contains("geometry") => {
                FsError::CorruptFilesystem(String::from(other))
            }
            other => FsError::Io(String::from(other)),
        }
    }
}

impl From<String> for FsError {
    fn from(s: String) -> Self {
        FsError::from(s.as_str())
    }
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::Io(m)
            | FsError::Unsupported(m)
            | FsError::CorruptFilesystem(m)
            | FsError::Device(m) => write!(f, "{}: {}", self.as_str(), m),
            _ => f.write_str(self.as_str()),
        }
    }
}
