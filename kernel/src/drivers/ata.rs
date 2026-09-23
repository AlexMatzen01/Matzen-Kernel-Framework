//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! ATA/IDE Disk Driver
//!
//! Provides basic ATA PIO (Programmed I/O) mode disk access.
//! Supports reading and writing sectors from ATA drives.

use spin::Mutex;
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};

/// Size of a disk sector in bytes
pub const SECTOR_SIZE: usize = 512;

/// ATA drive types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveType {
    Master,
    Slave,
}

/// ATA bus (Primary or Secondary)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtaBus {
    Primary,
    Secondary,
}

/// ATA status register flags
#[allow(dead_code)]
mod status {
    pub const ERR: u8 = 0x01; // Error
    pub const IDX: u8 = 0x02; // Index
    pub const CORR: u8 = 0x04; // Corrected data
    pub const DRQ: u8 = 0x08; // Data request ready
    pub const DSC: u8 = 0x10; // Drive seek complete
    pub const DF: u8 = 0x20; // Drive fault
    pub const DRDY: u8 = 0x40; // Drive ready
    pub const BSY: u8 = 0x80; // Busy
}

/// ATA command codes
#[allow(dead_code)]
mod commands {
    pub const READ_PIO: u8 = 0x20;
    pub const READ_PIO_EXT: u8 = 0x24;
    pub const WRITE_PIO: u8 = 0x30;
    pub const WRITE_PIO_EXT: u8 = 0x34;
    pub const CACHE_FLUSH: u8 = 0xE7;
    pub const CACHE_FLUSH_EXT: u8 = 0xEA;
    pub const IDENTIFY: u8 = 0xEC;
}

/// Represents an ATA drive
pub struct AtaDrive {
    bus: AtaBus,
    drive_type: DriveType,
    data_port: Port<u16>,
    error_port: PortReadOnly<u8>,
    sector_count_port: Port<u8>,
    lba_low_port: Port<u8>,
    lba_mid_port: Port<u8>,
    lba_high_port: Port<u8>,
    drive_port: Port<u8>,
    status_port: PortReadOnly<u8>,
    command_port: PortWriteOnly<u8>,
    alternate_status_port: PortReadOnly<u8>,
    control_port: PortWriteOnly<u8>,
    exists: bool,
    lba48_supported: bool,
    total_sectors_: u64,
}

impl AtaDrive {
    /// Create a new ATA drive interface
    pub fn new(bus: AtaBus, drive_type: DriveType) -> Self {
        let (base, ctrl) = match bus {
            AtaBus::Primary => (0x1F0, 0x3F6),
            AtaBus::Secondary => (0x170, 0x376),
        };

        AtaDrive {
            bus,
            drive_type,
            data_port: Port::new(base),
            error_port: PortReadOnly::new(base + 1),
            sector_count_port: Port::new(base + 2),
            lba_low_port: Port::new(base + 3),
            lba_mid_port: Port::new(base + 4),
            lba_high_port: Port::new(base + 5),
            drive_port: Port::new(base + 6),
            status_port: PortReadOnly::new(base + 7),
            command_port: PortWriteOnly::new(base + 7),
            alternate_status_port: PortReadOnly::new(ctrl),
            control_port: PortWriteOnly::new(ctrl),
            exists: false,
            lba48_supported: false,
            total_sectors_: 0,
        }
    }

    /// Initialize and identify the drive
    pub fn init(&mut self) -> Result<(), &'static str> {
        unsafe {
            self.exists = false;

            // Select drive
            let drive_select = match self.drive_type {
                DriveType::Master => 0xA0,
                DriveType::Slave => 0xB0,
            };
            self.drive_port.write(drive_select);
            self.wait_400ns();

            // An absent ATA device returns either zero or floating-bus 0xFF.
            // Do this check before issuing IDENTIFY so probing one empty slot
            // cannot leave the controller in a misleading state.
            let status = self.status_port.read();
            if status == 0 || status == 0xFF {
                return Err("Drive does not exist");
            }

            // A device can still be completing a previous command after the
            // select operation. Wait for it before sending IDENTIFY.
            if status & status::BSY != 0 {
                self.wait_not_busy()?;
            }

            // Some IDE devices can still be settling immediately after the
            // select operation. Retry IDENTIFY once if the first command does
            // not produce a usable response.
            for attempt in 0..2 {
                self.command_port.write(commands::IDENTIFY);
                self.wait_400ns();

                // Check that the device responded to IDENTIFY.
                let status = self.status_port.read();
                if status == 0 || status == 0xFF {
                    if attempt == 0 {
                        self.drive_port.write(drive_select);
                        self.wait_400ns();
                        continue;
                    }
                    return Err("Drive does not exist");
                }

                // Wait for drive to be ready or report an ATA error.
                self.wait_not_busy()?;

                // Check if this is an ATAPI device (we only want ATA hard disks)
                let lba_mid = self.lba_mid_port.read();
                let lba_high = self.lba_high_port.read();

                // ATAPI signature is 0x14, 0xEB or 0x69, 0x96
                if (lba_mid == 0x14 && lba_high == 0xEB) || (lba_mid == 0x69 && lba_high == 0x96) {
                    return Err("ATAPI device (not ATA)");
                }

                // For ATA devices, these should be 0, but be lenient for QEMU.
                let status = self.status_port.read();
                if status & status::ERR != 0 {
                    return Err("Device error during IDENTIFY");
                }

                // Wait for data to be ready.
                self.wait_drq()?;

                // Read identification data.
                let mut identify_data = [0u16; 256];
                for word in identify_data.iter_mut() {
                    *word = self.data_port.read();
                }

                // Check if LBA48 is supported (word 83, bit 10).
                self.lba48_supported = (identify_data[83] & (1 << 10)) != 0;
                self.exists = true;
                return Ok(());
            }

            Err("Drive does not exist")
        }
    }

    /// Check if the drive exists and is initialized
    pub fn exists(&self) -> bool {
        self.exists
    }

    /// Get total number of sectors on the drive
    pub fn total_sectors(&self) -> u64 {
        self.total_sectors_
    }

    /// Returns true for ATA drives (always true for this type)
    pub fn is_ata(&self) -> bool {
        true
    }

    /// Read sectors from the disk
    pub fn read_sectors(
        &mut self,
        lba: u64,
        count: u8,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        if !self.exists {
            return Err("Drive not initialized");
        }

        if buffer.len() < (count as usize * SECTOR_SIZE) {
            return Err("Buffer too small");
        }

        if count == 0 {
            return Err("Invalid sector count");
        }

        unsafe {
            // Select drive and set LBA mode
            let drive_select = match self.drive_type {
                DriveType::Master => 0xE0,
                DriveType::Slave => 0xF0,
            } | ((lba >> 24) & 0x0F) as u8;

            self.drive_port.write(drive_select);
            self.wait_400ns();

            // Send sector count and LBA
            self.sector_count_port.write(count);
            self.lba_low_port.write((lba & 0xFF) as u8);
            self.lba_mid_port.write(((lba >> 8) & 0xFF) as u8);
            self.lba_high_port.write(((lba >> 16) & 0xFF) as u8);

            // Send read command
            self.command_port.write(commands::READ_PIO);

            // Read data for each sector
            for sector in 0..count {
                self.wait_not_busy()?;
                self.wait_drq()?;

                let offset = sector as usize * SECTOR_SIZE;
                let sector_buffer = &mut buffer[offset..offset + SECTOR_SIZE];

                // Read 256 words (512 bytes)
                for i in (0..SECTOR_SIZE).step_by(2) {
                    let word = self.data_port.read();
                    sector_buffer[i] = (word & 0xFF) as u8;
                    sector_buffer[i + 1] = ((word >> 8) & 0xFF) as u8;
                }

                // Delay after reading
                self.wait_400ns();
            }

            Ok(())
        }
    }

    /// Write sectors to the disk
    pub fn write_sectors(
        &mut self,
        lba: u64,
        count: u8,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        if !self.exists {
            return Err("Drive not initialized");
        }

        if buffer.len() < (count as usize * SECTOR_SIZE) {
            return Err("Buffer too small");
        }

        if count == 0 {
            return Err("Invalid sector count");
        }

        unsafe {
            // Select drive and set LBA mode
            let drive_select = match self.drive_type {
                DriveType::Master => 0xE0,
                DriveType::Slave => 0xF0,
            } | ((lba >> 24) & 0x0F) as u8;

            self.drive_port.write(drive_select);
            self.wait_400ns();

            // Send sector count and LBA
            self.sector_count_port.write(count);
            self.lba_low_port.write((lba & 0xFF) as u8);
            self.lba_mid_port.write(((lba >> 8) & 0xFF) as u8);
            self.lba_high_port.write(((lba >> 16) & 0xFF) as u8);

            // Send write command
            self.command_port.write(commands::WRITE_PIO);

            // Write data for each sector
            for sector in 0..count {
                self.wait_not_busy()?;
                self.wait_drq()?;

                let offset = sector as usize * SECTOR_SIZE;
                let sector_buffer = &buffer[offset..offset + SECTOR_SIZE];

                // Write 256 words (512 bytes)
                for i in (0..SECTOR_SIZE).step_by(2) {
                    let word = sector_buffer[i] as u16 | ((sector_buffer[i + 1] as u16) << 8);
                    self.data_port.write(word);
                }

                // Flush cache after writing
                self.command_port.write(commands::CACHE_FLUSH);
                self.wait_not_busy()?;
            }

            Ok(())
        }
    }

    /// Wait for the busy flag to clear
    fn wait_not_busy(&mut self) -> Result<(), &'static str> {
        unsafe {
            // Timeout after ~1 second (arbitrary loop count)
            for _ in 0..1_000_000 {
                let status = self.status_port.read();
                if status & status::BSY == 0 {
                    // Check for errors
                    if status & status::ERR != 0 {
                        return Err("Drive error");
                    }
                    if status & status::DF != 0 {
                        return Err("Drive fault");
                    }
                    return Ok(());
                }
                core::hint::spin_loop();
            }
            Err("Drive timeout (busy)")
        }
    }

    /// Wait for data request to be ready
    fn wait_drq(&mut self) -> Result<(), &'static str> {
        unsafe {
            // Timeout after ~1 second
            for _ in 0..1_000_000 {
                let status = self.status_port.read();
                if status & status::DRQ != 0 {
                    return Ok(());
                }
                if status & status::ERR != 0 {
                    return Err("Drive error");
                }
                core::hint::spin_loop();
            }
            Err("Drive timeout (DRQ)")
        }
    }

    /// Wait 400ns by reading the alternate status register 4 times
    fn wait_400ns(&mut self) {
        unsafe {
            for _ in 0..4 {
                let _ = self.alternate_status_port.read();
            }
        }
    }
}

/// Global ATA drives
static DRIVES: Mutex<Option<[AtaDrive; 4]>> = Mutex::new(None);

/// Initialize ATA drives
pub fn init() {
    use crate::serial_println;

    let mut drives = [
        AtaDrive::new(AtaBus::Primary, DriveType::Master),
        AtaDrive::new(AtaBus::Primary, DriveType::Slave),
        AtaDrive::new(AtaBus::Secondary, DriveType::Master),
        AtaDrive::new(AtaBus::Secondary, DriveType::Slave),
    ];

    let mut found_count = 0;

    for (i, drive) in drives.iter_mut().enumerate() {
        let bus_name = match drive.bus {
            AtaBus::Primary => "Primary",
            AtaBus::Secondary => "Secondary",
        };
        let drive_name = match drive.drive_type {
            DriveType::Master => "Master",
            DriveType::Slave => "Slave",
        };

        serial_println!("  Probing ATA {}/{}...", bus_name, drive_name);
        match drive.init() {
            Ok(()) => {
                serial_println!("    -> Detected and initialized");
                found_count += 1;
            }
            Err(e) => {
                serial_println!("    -> {}", e);
            }
        }
    }

    *DRIVES.lock() = Some(drives);

    if found_count > 0 {
        serial_println!("ATA driver initialized ({} drive(s) found)", found_count);
    } else {
        serial_println!("ATA driver initialized (no drives found)");
    }
}

/// Read sectors from the primary slave drive
pub fn read_sectors(lba: u64, count: u8, buffer: &mut [u8]) -> Result<(), &'static str> {
    let mut drives = DRIVES.lock();
    if let Some(drives_array) = drives.as_mut() {
        // Use Primary Slave (index 1) as the data disk
        drives_array[1].read_sectors(lba, count, buffer)
    } else {
        Err("ATA not initialized")
    }
}

/// Write sectors to the primary slave drive
pub fn write_sectors(lba: u64, count: u8, buffer: &[u8]) -> Result<(), &'static str> {
    let mut drives = DRIVES.lock();
    if let Some(drives_array) = drives.as_mut() {
        // Use Primary Slave (index 1) as the data disk
        drives_array[1].write_sectors(lba, count, buffer)
    } else {
        Err("ATA not initialized")
    }
}

/// Wrapper for install.rs compatibility
pub fn read_sectors_from(
    drive_index: usize,
    lba: u64,
    count: u8,
    buffer: &mut [u8],
) -> Result<(), &'static str> {
    let mut drives = DRIVES.lock();
    if let Some(drives_array) = drives.as_mut() {
        if drive_index < drives_array.len() {
            drives_array[drive_index].read_sectors(lba, count, buffer)
        } else {
            Err("Invalid drive index")
        }
    } else {
        Err("ATA not initialized")
    }
}

/// Wrapper for install.rs compatibility
pub fn write_sectors_to(
    drive_index: usize,
    lba: u64,
    count: u8,
    buffer: &[u8],
) -> Result<(), &'static str> {
    let mut drives = DRIVES.lock();
    if let Some(drives_array) = drives.as_mut() {
        if drive_index < drives_array.len() {
            drives_array[drive_index].write_sectors(lba, count, buffer)
        } else {
            Err("Invalid drive index")
        }
    } else {
        Err("ATA not initialized")
    }
}

/// Drive information for the installer
pub struct DriveInfo {
    pub exists: bool,
    pub total_sectors: u64,
    pub is_ata: bool,
    pub last_error: Option<&'static str>,
}

/// Wrapper for install.rs compatibility
pub fn drive_info(drive_index: usize) -> Option<DriveInfo> {
    let drives = DRIVES.lock();
    if let Some(drives_array) = drives.as_ref() {
        if drive_index < drives_array.len() && drives_array[drive_index].exists() {
            Some(DriveInfo {
                exists: true,
                total_sectors: drives_array[drive_index].total_sectors(),
                is_ata: drives_array[drive_index].is_ata(),
                last_error: None,
            })
        } else {
            Some(DriveInfo {
                exists: false,
                total_sectors: 0,
                is_ata: false,
                last_error: Some("Drive not detected or not ATA"),
            })
        }
    } else {
        Some(DriveInfo {
            exists: false,
            total_sectors: 0,
            is_ata: false,
            last_error: Some("ATA not initialized"),
        })
    }
}
