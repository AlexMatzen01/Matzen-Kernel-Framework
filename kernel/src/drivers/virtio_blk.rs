//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! virtio-blk block driver (legacy/transitional PCI interface).
//!
//! Targets QEMU's `virtio-blk-pci` transitional device (vendor 0x1AF4,
//! device 0x1001), which exposes the legacy I/O-port register layout in
//! BAR0. One request queue, single outstanding request, polling (no
//! interrupts). Queue + bounce buffers come from DMA-capable pages.
//!
//! Extra QEMU drives beyond the 4 IDE slots are attached as virtio-blk
//! devices by the runner and appear here as drive indices 4+ (IDE 0-3
//! stay stable). Hot-attached devices (QEMU `device_add` after boot) are
//! discovered by re-enumerating PCI; see `ata::rescan_silent`.

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::drivers::pci::PciDevice;

pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
pub const VIRTIO_BLK_LEGACY_DEVICE_ID: u16 = 0x1001;

// Legacy I/O-port register offsets from BAR0 (I/O base).
const R_DEVICE_FEATURES: u16 = 0;
const R_GUEST_FEATURES: u16 = 4;
const R_QUEUE_PFN: u16 = 8;
const R_QUEUE_SIZE: u16 = 12;
const R_QUEUE_SELECT: u16 = 14;
const R_QUEUE_NOTIFY: u16 = 16;
const R_DEVICE_STATUS: u16 = 18;
#[allow(dead_code)]
const R_ISR: u16 = 19;
// Device config starts here; blk capacity (u64, 512B sectors) is at +0.
const R_CONFIG_CAPACITY_LO: u16 = 20;
const R_CONFIG_CAPACITY_HI: u16 = 24;

// Device status bits.
const S_ACKNOWLEDGE: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;
const S_FAILED: u8 = 0x80;

// Block request types / status.
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const S_OK: u8 = 0;

// Descriptor flags.
const D_NEXT: u16 = 1;
const D_WRITE: u16 = 2; // device writes (read-only for driver on IN path)

/// Queue memory: descriptor table + avail ring in low pages, used ring
/// 4K-aligned after them (legacy layout; offsets derived from the
/// *device* queue size, which the device also uses for addressing).
const MAX_QUEUE_SIZE: u16 = 256;

/// One virtio-blk device (one PCI function).
pub struct VirtioBlkDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    io_base: u16,
    queue_size: u16,
    queue_virt: u64,
    _queue_phys: u64,
    used_offset: u64,
    data_virt: u64,
    _data_phys: u64,
    capacity: u64,
    avail_idx: u16,
    last_used_idx: u16,
    pub exists: bool,
    pub last_error: Option<&'static str>,
}
/// Next candidate I/O port base for hot-added virtio-blk BAR assignment.
/// SeaBIOS only programs BARs present at boot; QEMU `device_add` devices
/// arrive with BAR0 unassigned (base zero) and need guest-side assignment.
/// Pool starts at 0xC000 (above legacy IDE/PIC/PIT/config ranges) and
/// bump-allocates; each candidate is additionally checked against live
/// BARs (see `io_port_in_use`) so boot-assigned ports are never clobbered.
static NEXT_VIRTIO_IO_BASE: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(0xC000);

/// True if `[base, base+size)` overlaps any non-zero I/O BAR0 on PCI bus 0
/// or a reserved legacy port range.
fn io_port_in_use(base: u16, size: u16) -> bool {
    let end = base as u32 + size as u32;
    // Reserved legacy ranges (IDE compat, PIC, PIT, CMOS, PCI config).
    const RESERVED: [(u32, u32); 7] = [
        (0x1F0, 0x1F8),
        (0x3F4, 0x3F8),
        (0x170, 0x178),
        (0x374, 0x378),
        (0x020, 0x0A2),
        (0x040, 0x044),
        (0xCF8, 0xD00),
    ];
    for (s, e) in RESERVED {
        if (base as u32) < e && end > s {
            return true;
        }
    }
    for d in crate::drivers::pci::enumerate_bus(0) {
        if d.bar0 & 0x1 == 0 {
            continue;
        }
        let b = (d.bar0 & 0xFFFF_FFFC) as u32;
        if b == 0 {
            continue; // unassigned (possibly ourselves)
        }
        // Peer BAR size unknown without sizing each device; assume a 64B
        // window, which covers legacy virtio-blk (needs <64B).
        if (base as u32) < b + 64 && end > b {
            return true;
        }
    }
    false
}

/// Allocate a free I/O port window of `size` bytes (power-of-two aligned).
fn alloc_virtio_io_port(size: u16) -> Option<u16> {
    let size = size.max(8);
    let align = size.next_power_of_two();
    let mut cursor = NEXT_VIRTIO_IO_BASE.load(core::sync::atomic::Ordering::Relaxed);
    for _ in 0..64 {
        let aligned = cursor.next_multiple_of(align);
        if aligned as u32 + size as u32 > 0xF800 {
            return None;
        }
        if !io_port_in_use(aligned, size) {
            NEXT_VIRTIO_IO_BASE.store(
                aligned.wrapping_add(size),
                core::sync::atomic::Ordering::Relaxed,
            );
            return Some(aligned);
        }
        cursor = aligned.wrapping_add(align);
    }
    None
}

/// Data bounce buffer: one page -> at most 8 sectors per request chunk.
const BOUNCE_SECTORS: usize = 8;
const SECTOR_SIZE: usize = 512;

impl VirtioBlkDevice {
    /// Placeholder for a PCI virtio device that failed init (keeps the
    /// registry index stable; diskinfo shows the error).
    pub fn failed(bus: u8, device: u8, function: u8, err: &'static str) -> Self {
        VirtioBlkDevice {
            bus,
            device,
            function,
            io_base: 0,
            queue_size: 0,
            queue_virt: 0,
            _queue_phys: 0,
            used_offset: 0,
            data_virt: 0,
            _data_phys: 0,
            capacity: 0,
            avail_idx: 0,
            last_used_idx: 0,
            exists: false,
            last_error: Some(err),
        }
    }

    pub fn matches(&self, bus: u8, device: u8, function: u8) -> bool {
        self.bus == bus && self.device == device && self.function == function
    }

    /// Initialize a virtio-blk PCI device via the legacy interface.
    pub fn new(pci: &PciDevice) -> Result<Self, &'static str> {
        // Bus mastering (DMA) + memory space for the PCI command register.
        // I/O space is enabled below, after BAR0 is known assigned.
        pci.enable_bus_mastering();

        // Fresh BAR0 read: the cached `pci.bar0` snapshot predates any
        // assignment we perform here.
        let raw = pci.read_config(0x10);
        if raw & 0x1 == 0 {
            return Err("virtio BAR0 not I/O space");
        }
        let mut io_base = (raw & 0xFFFF_FFFC) as u16;
        if io_base == 0 {
            // Hot-added device (no firmware ran to assign its BAR).
            // Quiesce decoding while programming the BAR.
            let cmd = pci.read_config(0x04);
            pci.write_config(0x04, cmd & !0x3);
            // Size the BAR: write all ones, mask decodes the window size.
            pci.write_config(0x10, 0xFFFF_FFFD);
            let mask = pci.read_config(0x10);
            pci.write_config(0x10, raw); // restore before sizing math
            let mut size: u32 = (!(mask & 0xFFFF_FFFC)).wrapping_add(1);
            if size < 8 || size > 256 {
                size = 64; // sizing failed or implausible: legacy needs <64B
            }
            let base = alloc_virtio_io_port(size as u16)
                .ok_or("virtio I/O port space exhausted")?;
            pci.write_config(0x10, (base as u32) | 0x1);
            let back = pci.read_config(0x10);
            if (back & 0xFFFF_FFFC) as u16 != base {
                return Err("virtio BAR assign failed");
            }
            io_base = base;
        }
        // Enable I/O-space decoding (bit 0), preserving bus-master/mem bits.
        let cmd = pci.read_config(0x04);
        let lo = (cmd & 0xFFFF) as u16 | 0x1;
        pci.write_config(0x04, (cmd & 0xFFFF_0000) | lo as u32);

        unsafe {
            let mut status_p = Port::<u8>::new(io_base + R_DEVICE_STATUS);
            let mut feat_p = Port::<u32>::new(io_base + R_DEVICE_FEATURES);
            let mut guest_p = Port::<u32>::new(io_base + R_GUEST_FEATURES);
            let mut qsel_p = Port::<u16>::new(io_base + R_QUEUE_SELECT);
            let mut qsize_p = Port::<u16>::new(io_base + R_QUEUE_SIZE);
            let mut qpfn_p = Port::<u32>::new(io_base + R_QUEUE_PFN);

            // Reset + acknowledge + "we know how to drive it".
            status_p.write(0);
            status_p.write(S_ACKNOWLEDGE | S_DRIVER);

            // Negotiate no features (plain 512B R/W needs none).
            let _dev_feat = feat_p.read();
            guest_p.write(0);

            // Queue 0 setup. The legacy layout (desc + avail, then the
            // 4K-aligned used ring) is sized from the *device* queue size,
            // which the device also uses to compute ring addresses.
            qsel_p.write(0);
            let dev_qsize = qsize_p.read();
            if dev_qsize == 0 || dev_qsize > MAX_QUEUE_SIZE {
                status_p.write(S_FAILED);
                return Err("virtio queue 0 bad size");
            }
            let qsize = dev_qsize;
            let desc_bytes = 16u64 * qsize as u64;
            let avail_bytes = 4u64 + 2 * qsize as u64 + 2;
            let used_offset = (desc_bytes + avail_bytes + 4095) & !4095;
            let total = (used_offset + 4 + 8 * qsize as u64 + 4095) & !4095;
            let pages = (total / 4096) as usize;

            let layout = Layout::from_size_align(pages * 4096, 4096)
                .map_err(|_| "virtio queue layout")?;
            let ptr = alloc_zeroed(layout);
            if ptr.is_null() {
                status_p.write(S_FAILED);
                return Err("virtio queue alloc failed");
            }
            let qvirt = ptr as u64;
            let qphys = dma_phys(qvirt, 4096).ok_or("virtio queue DMA unreachable")?;
            if qphys & 0xFFF != 0 || qphys >> 32 != 0 {
                status_p.write(S_FAILED);
                return Err("virtio queue phys unsuitable");
            }
            // Verify all pages contiguous (used ring + header scratch live
            // in later pages for large queues).
            for p in 1..pages {
                let pp = dma_phys(qvirt + (p as u64) * 4096, 4096)
                    .ok_or("virtio queue page unreachable")?;
                if pp != qphys + (p as u64) * 4096 {
                    status_p.write(S_FAILED);
                    return Err("virtio queue not contiguous");
                }
            }
            qpfn_p.write((qphys >> 12) as u32);

            // Data bounce page.
            let (dvirt, dphys) = dma_single_page().ok_or("virtio bounce alloc failed")?;

            // Capacity (sectors) from device config.
            let mut cap_lo_p = Port::<u32>::new(io_base + R_CONFIG_CAPACITY_LO);
            let mut cap_hi_p = Port::<u32>::new(io_base + R_CONFIG_CAPACITY_HI);
            let cap = cap_lo_p.read() as u64 | ((cap_hi_p.read() as u64) << 32);

            status_p.write(S_ACKNOWLEDGE | S_DRIVER | S_DRIVER_OK);
            // Queue pages stay allocated for driver lifetime (never freed).
            let _ = layout;

            Ok(VirtioBlkDevice {
                bus: pci.bus,
                device: pci.device,
                function: pci.function,
                io_base,
                queue_size: qsize,
                queue_virt: qvirt,
                _queue_phys: qphys,
                used_offset,
                data_virt: dvirt,
                _data_phys: dphys,
                capacity: cap,
                avail_idx: 0,
                last_used_idx: 0,
                exists: true,
                last_error: None,
            })
        }
    }

    pub fn total_sectors(&self) -> u64 {
        self.capacity
    }

    pub fn read_sectors(&mut self, lba: u64, count: u8, buffer: &mut [u8]) -> Result<(), &'static str> {
        self.transfer(true, lba, count, buffer.as_mut_ptr(), buffer.len())
    }

    pub fn write_sectors(&mut self, lba: u64, count: u8, buffer: &[u8]) -> Result<(), &'static str> {
        self.transfer(false, lba, count, buffer.as_ptr() as *mut u8, buffer.len())
    }

    /// Chunked transfer through the bounce page. `buf` is only written on
    /// reads and only read on writes (raw pointer keeps the borrow checker
    /// out of DMA staging).
    fn transfer(&mut self, is_read: bool, lba: u64, count: u8, buf: *mut u8, len: usize) -> Result<(), &'static str> {
        if !self.exists {
            return Err("Drive not initialized");
        }
        if count == 0 {
            return Err("Invalid sector count");
        }
        let total = count as usize * SECTOR_SIZE;
        if len < total {
            return Err("Buffer too small");
        }
        if lba.checked_add(count as u64).is_none() {
            return Err("LBA overflow");
        }
        let mut done = 0usize;
        let mut sector = lba;
        while done < count as usize {
            let n = (count as usize - done).min(BOUNCE_SECTORS);
            // For writes, stage the chunk into the bounce page first.
            if !is_read {
                let dst = unsafe { core::slice::from_raw_parts_mut(self.data_virt as *mut u8, n * SECTOR_SIZE) };
                let src = unsafe { core::slice::from_raw_parts(buf, (done + n) * SECTOR_SIZE) };
                dst.copy_from_slice(&src[done * SECTOR_SIZE..]);
            }
            self.one_request(is_read, sector, n)?;
            if is_read {
                let src = unsafe { core::slice::from_raw_parts(self.data_virt as *const u8, n * SECTOR_SIZE) };
                let dst = unsafe { core::slice::from_raw_parts_mut(buf.add(done * SECTOR_SIZE), n * SECTOR_SIZE) };
                dst.copy_from_slice(src);
            }
            done += n;
            sector += n as u64;
        }
        Ok(())
    }

    /// Single request of up to BOUNCE_SECTORS sectors through the bounce page.
    fn one_request(&mut self, is_read: bool, sector: u64, nsectors: usize) -> Result<(), &'static str> {
        let q = self.queue_size as usize;
        let desc = self.queue_virt as *mut u8;
        let avail = unsafe { desc.add(16 * q) };
        let avail_end = 16 * q + 4 + 2 * q + 2;
        let used = (self.queue_virt + self.used_offset) as *mut u8;
        let data = self.data_virt as *mut u8;
        let status_slot = unsafe { data.add(4095) };

        unsafe {
            // Request header (16B) lives in queue scratch right after the
            // avail ring (never touched by the device without EVENT_IDX,
            // which we did not negotiate).
            let hdr = avail.add(4 + 2 * q + 2) as *mut u8;
            // Bounds: header + status must fit before the used ring.
            if avail_end + 17 > self.used_offset as usize {
                return Err("virtio queue scratch overflow");
            }
            // --- descriptors: header -> data -> status ---
            let d0 = desc as *mut u64;
            let req_type = if is_read { T_IN } else { T_OUT };
            core::ptr::write_unaligned(hdr as *mut u32, req_type);
            core::ptr::write_unaligned(hdr.add(4) as *mut u32, 0);
            core::ptr::write_unaligned(hdr.add(8) as *mut u64, sector);
            let hdr_phys = dma_phys(hdr as u64, 16).ok_or("virtio hdr DMA unreachable")?;

            let data_phys = dma_phys(data as u64, nsectors * SECTOR_SIZE)
                .ok_or("virtio data DMA unreachable")?;
            let status_phys = dma_phys(status_slot as u64, 1)
                .ok_or("virtio status DMA unreachable")?;
            status_slot.write_volatile(0xFF);

            // d0: header, device reads.
            write_desc(d0, 0, hdr_phys, 16, D_NEXT);
            // d1: data.
            let d1_flags = if is_read { D_NEXT | D_WRITE } else { D_NEXT };
            write_desc(d0, 1, data_phys, (nsectors * SECTOR_SIZE) as u32, d1_flags);
            // d2: status byte, device writes.
            write_desc(d0, 2, status_phys, 1, D_WRITE);

            // --- avail ring: publish head 0 ---
            let avail_flags = avail as *mut u16;
            let avail_idx_p = avail.add(2) as *mut u16;
            let avail_ring = avail.add(4) as *mut u16;
            let idx = self.avail_idx;
            avail_ring.add((idx as usize) % q).write_volatile(0);
            fence(Ordering::SeqCst);
            avail_flags.write_volatile(0);
            avail_idx_p.write_volatile(idx.wrapping_add(1));
            fence(Ordering::SeqCst);
            self.avail_idx = idx.wrapping_add(1);

            // --- notify queue 0 ---
            let mut notify_p = Port::<u16>::new(self.io_base + R_QUEUE_NOTIFY);
            notify_p.write(0);

            // --- poll used ring ---
            let used_idx_p = used.add(2) as *const u16;
            let mut timeout = crate::drivers::pit::timeout_ms(2000);
            let reliable = timeout.reliable();
            let mut spins = 0u32;
            loop {
                fence(Ordering::SeqCst);
                let used_idx = used_idx_p.read_volatile();
                if used_idx != self.last_used_idx {
                    break;
                }
                if reliable {
                    if timeout.poll() {
                        return Err("virtio-blk timeout");
                    }
                } else {
                    spins += 1;
                    if spins > 50_000_000 {
                        return Err("virtio-blk timeout");
                    }
                }
                core::hint::spin_loop();
            }
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            fence(Ordering::SeqCst);

            // Ack ISR (clears interrupt state even though we poll).
            let mut isr_p = Port::<u8>::new(self.io_base + R_ISR);
            let _ = isr_p.read();

            if status_slot.read_volatile() != S_OK {
                return Err("virtio-blk I/O error");
            }
            Ok(())
        }
    }
}

/// Write one 16B split-virtqueue descriptor (volatile: device reads RAM).
unsafe fn write_desc(table: *mut u64, i: usize, addr: u64, len: u32, flags: u16) {
    let d = (table as *mut u8).add(i * 16);
    core::ptr::write_unaligned(d as *mut u64, addr);
    core::ptr::write_unaligned(d.add(8) as *mut u32, len);
    core::ptr::write_unaligned(d.add(12) as *mut u16, flags);
    core::ptr::write_unaligned(d.add(14) as *mut u16, if flags & D_NEXT != 0 { (i + 1) as u16 } else { 0 });
}

/// Translate virt -> phys, requiring <4GB and a page-contained range
/// (single-page bounce + in-page scratch always satisfy this).
fn dma_phys(virt: u64, len: usize) -> Option<u64> {
    if len == 0 || len > 4096 {
        return None;
    }
    if (virt & !0xFFF) != ((virt + len as u64 - 1) & !0xFFF) {
        return None;
    }
    let phys = crate::drivers::usb::virt_to_phys_for_dma(virt)?;
    if phys >= 0x1_0000_0000 {
        return None;
    }
    Some(phys)
}

/// One zeroed DMA-capable page; leaked for driver lifetime.
fn dma_single_page() -> Option<(u64, u64)> {
    let (virt, phys, _) = crate::drivers::usb::dma_page()?;
    Some((virt, phys))
}

// ── Device registry (drive indices 4+ in the unified drive layer) ──

/// Probed virtio-blk devices in PCI enumeration order (bus, device,
/// function). Index `i` here is unified drive index `4 + i`.
static DEVICES: Mutex<Option<Vec<VirtioBlkDevice>>> = Mutex::new(None);

/// Probe all PCI buses for legacy virtio-blk devices and (re)initialize
/// them. Keeps a placeholder entry for failed devices so indices stay
/// stable and `diskinfo` can show the error.
pub fn rescan() {
    let mut found: Vec<VirtioBlkDevice> = Vec::new();
    for bus in 0..8u8 {
        for d in crate::drivers::pci::enumerate_bus(bus) {
            if d.vendor_id != VIRTIO_VENDOR_ID || d.device_id != VIRTIO_BLK_LEGACY_DEVICE_ID
            {
                continue;
            }
            match VirtioBlkDevice::new(&d) {
                Ok(dev) => found.push(dev),
                Err(e) => found.push(VirtioBlkDevice::failed(d.bus, d.device, d.function, e)),
            }
        }
    }
    // Stable order: sort by PCI address.
    found.sort_by_key(|d| (d.bus, d.device, d.function));
    *DEVICES.lock() = Some(found);
}

/// Initialize the virtio-blk driver (called once at boot).
pub fn init() {
    rescan();
    let n = DEVICES.lock().as_ref().map(|v| v.len()).unwrap_or(0);
    crate::serial_println!("virtio-blk initialized ({} device(s) found)", n);
}

/// Number of probed virtio-blk devices (both healthy and failed).
pub fn count() -> usize {
    DEVICES.lock().as_ref().map(|v| v.len()).unwrap_or(0)
}

/// Read sectors from virtio device `index` (0-based within virtio).
pub fn read_sectors(index: usize, lba: u64, count: u8, buffer: &mut [u8]) -> Result<(), &'static str> {
    let mut guard = DEVICES.lock();
    match guard.as_mut().and_then(|v| v.get_mut(index)) {
        Some(dev) => dev.read_sectors(lba, count, buffer),
        None => Err("Invalid virtio drive index"),
    }
}

/// Write sectors to virtio device `index` (0-based within virtio).
pub fn write_sectors(index: usize, lba: u64, count: u8, buffer: &[u8]) -> Result<(), &'static str> {
    let mut guard = DEVICES.lock();
    match guard.as_mut().and_then(|v| v.get_mut(index)) {
        Some(dev) => dev.write_sectors(lba, count, buffer),
        None => Err("Invalid virtio drive index"),
    }
}

/// Info snapshot for virtio device `index`.
pub struct VirtioDriveInfo {
    pub exists: bool,
    pub total_sectors: u64,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub last_error: Option<&'static str>,
}

/// Wrapper for the unified drive layer.
pub fn drive_info(index: usize) -> Option<VirtioDriveInfo> {
    let guard = DEVICES.lock();
    match guard.as_ref().and_then(|v| v.get(index)) {
        Some(dev) => Some(VirtioDriveInfo {
            exists: dev.exists,
            total_sectors: if dev.exists { dev.total_sectors() } else { 0 },
            bus: dev.bus,
            device: dev.device,
            function: dev.function,
            last_error: dev.last_error,
        }),
        None => None,
    }
}
