//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Unified drive layer: single global drive index space across transports.
//!
//! ```text
//! 0-3 = ATA IDE (0 Primary Master boot, 1 Primary Slave data,
//!                2 Secondary Master, 3 Secondary Slave)
//! 4+N = virtio-blk PCI devices in enumeration order (requires the
//!       `usb` feature for DMA helpers; ATA-only without it)
//! ```
//!
//! All new code (shell `mkfs`/`mount`/`diskinfo`, installer, desktop
//! Drive app) should use this module instead of talking to `ata`
//! directly so IDE and virtio disks behave identically.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// First virtio-blk unified index (ATA occupies 0-3).
pub const VIRTIO_BASE: usize = 4;
/// Data disk default (Primary Slave, SimplFS). Preserved for compat.
pub const DATA_DRIVE: usize = 1;
/// Boot source (Primary Master in all runner configs).
pub const BOOT_DRIVE: usize = 0;
/// Upper bound for virtio devices (QEMU/runner cap is far lower).
#[cfg(feature = "usb")]
pub const MAX_VIRTIO: usize = 16;

/// Transport of a unified drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Ata,
    VirtioBlk,
}

/// Snapshot for one unified drive index.
pub struct UnifiedDriveInfo {
    pub exists: bool,
    pub total_sectors: u64,
    pub kind: DriveKind,
    pub is_ata: bool,
    pub last_error: Option<&'static str>,
    /// PCI address for virtio (`None` for ATA).
    pub pci: Option<(u8, u8, u8)>,
}

/// Total unified slots currently present (4 ATA + probed virtio).
pub fn drive_count() -> usize {
    #[cfg(feature = "usb")]
    {
        VIRTIO_BASE + crate::drivers::virtio_blk::count()
    }
    #[cfg(not(feature = "usb"))]
    {
        VIRTIO_BASE
    }
}

/// Info for unified drive `index`. Returns `None` only for out-of-range
/// indices; absent-but-valid slots yield `exists == false`.
pub fn drive_info(index: usize) -> Option<UnifiedDriveInfo> {
    if index < VIRTIO_BASE {
        return crate::drivers::ata::drive_info(index).map(|info| UnifiedDriveInfo {
            exists: info.exists,
            total_sectors: info.total_sectors,
            kind: DriveKind::Ata,
            is_ata: info.is_ata,
            last_error: info.last_error,
            pci: None,
        });
    }
    #[cfg(feature = "usb")]
    {
        return crate::drivers::virtio_blk::drive_info(index - VIRTIO_BASE).map(|info| {
            UnifiedDriveInfo {
                exists: info.exists,
                total_sectors: info.total_sectors,
                kind: DriveKind::VirtioBlk,
                is_ata: false,
                last_error: info.last_error,
                pci: Some((info.bus, info.device, info.function)),
            }
        });
    }
    #[cfg(not(feature = "usb"))]
    {
        return None;
    }
}

/// Read sectors from any unified drive.
pub fn read_sectors_from(
    drive_index: usize,
    lba: u64,
    count: u8,
    buffer: &mut [u8],
) -> Result<(), &'static str> {
    if drive_index < VIRTIO_BASE {
        return crate::drivers::ata::read_sectors_from(drive_index, lba, count, buffer);
    }
    #[cfg(feature = "usb")]
    {
        return crate::drivers::virtio_blk::read_sectors(drive_index - VIRTIO_BASE, lba, count, buffer);
    }
    #[cfg(not(feature = "usb"))]
    {
        return Err("Invalid drive index");
    }
}

/// Write sectors to any unified drive.
pub fn write_sectors_to(
    drive_index: usize,
    lba: u64,
    count: u8,
    buffer: &[u8],
) -> Result<(), &'static str> {
    if drive_index < VIRTIO_BASE {
        return crate::drivers::ata::write_sectors_to(drive_index, lba, count, buffer);
    }
    #[cfg(feature = "usb")]
    {
        return crate::drivers::virtio_blk::write_sectors(
            drive_index - VIRTIO_BASE,
            lba,
            count,
            buffer,
        );
    }
    #[cfg(not(feature = "usb"))]
    {
        return Err("Invalid drive index");
    }
}

/// Short human name for a slot: "Primary Master", ..., "virtio-blk #0".
pub fn slot_name(index: usize) -> String {
    match index {
        0 => String::from("Primary Master"),
        1 => String::from("Primary Slave"),
        2 => String::from("Secondary Master"),
        3 => String::from("Secondary Slave"),
        n => format!("virtio-blk #{} (drive {})", n - VIRTIO_BASE, n),
    }
}

/// One-line description, e.g. "4 virtio-blk #0: 131072 sectors (64 MB) [VIRTIO]".
pub fn drive_label(index: usize) -> String {
    let base = slot_name(index);
    let Some(info) = drive_info(index) else {
        return format!("{}: invalid index", index);
    };
    if !info.exists {
        return format!(
            "{} {}: absent ({})",
            index,
            base,
            info.last_error.unwrap_or("not detected")
        );
    }
    let mut label = format!(
        "{} {}: {} sectors ({} MB)",
        index,
        base,
        info.total_sectors,
        info.total_sectors / 2048
    );
    label.push_str(match info.kind {
        DriveKind::Ata => " [ATA]",
        DriveKind::VirtioBlk => " [VIRTIO]",
    });
    if let Some((b, d, f)) = info.pci {
        label.push_str(&format!(" {:02x}:{:02x}.{}", b, d, f));
    }
    if index == BOOT_DRIVE {
        label.push_str(" [BOOT SOURCE]");
    } else if index == DATA_DRIVE {
        label.push_str(" [DATA - SimplFS]");
    }
    label
}

/// Existing (probed) drive indices, boot source included.
pub fn existing_drives() -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..drive_count() {
        if let Some(info) = drive_info(i) {
            if info.exists {
                out.push(i);
            }
        }
    }
    out
}

/// Target candidates: existing drives other than the boot source.
pub fn target_candidates() -> Vec<usize> {
    existing_drives()
        .into_iter()
        .filter(|&i| i != BOOT_DRIVE)
        .collect()
}

/// Re-probe ATA + virtio without serial spam.
pub fn rescan_all_silent() {
    crate::drivers::ata::rescan_silent();
}
