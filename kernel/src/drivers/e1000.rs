//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Intel E1000 Network Driver
//!
//! Basic driver for Intel E1000 network card (commonly used in QEMU)

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};
use x86_64::PhysAddr;

const E1000_VENDOR_ID: u16 = 0x8086;
const E1000_DEVICE_ID: u16 = 0x100E;

// E1000 Registers
const REG_CTRL: u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EEPROM: u32 = 0x0014;
const REG_CTRL_EXT: u32 = 0x0018;
const REG_ICR: u32 = 0x00C0;
const REG_IMASK: u32 = 0x00D0;
const REG_IMC: u32 = 0x00E0;
const REG_RCTRL: u32 = 0x0100;
const REG_RXDESCLO: u32 = 0x2800;
const REG_RXDESCHI: u32 = 0x2804;
const REG_RXDESCLEN: u32 = 0x2808;
const REG_RXDESCHEAD: u32 = 0x2810;
const REG_RXDESCTAIL: u32 = 0x2818;
const REG_TCTRL: u32 = 0x0400;
const REG_TXDESCLO: u32 = 0x3800;
const REG_TXDESCHI: u32 = 0x3804;
const REG_TXDESCLEN: u32 = 0x3808;
const REG_TXDESCHEAD: u32 = 0x3810;
const REG_TXDESCTAIL: u32 = 0x3818;
const REG_RDTR: u32 = 0x2820;
const REG_RXDCTL: u32 = 0x3828;
const REG_RADV: u32 = 0x282C;
const REG_RSRPD: u32 = 0x2C00;

// Control bits
const CTRL_SLU: u32 = 0x40;
/// CTRL.Phy Reset (bit 31). Set and cleared around link setup.
const CTRL_PHY_RST: u32 = 1 << 31;

// Interrupt Mask Set / Interrupt Cause Register bits. The two registers share a
// bit layout, so one set of names covers reading a cause and setting a mask.
const IMS_TXDW: u32 = 1 << 0;
/// Transmit FIFO Underflow.
///
/// The card asserts this when a packet ends with data still queued in the TX
/// FIFO, which happens whenever a packet is larger than the FIFO and the card
/// has to be refilled mid-descriptor. It is expected here: the descriptor
/// buffer is filled in full before the descriptor is handed over, so there is no
/// race for the card to lose. It is masked and counted rather than ignored,
/// because a count that grows with bytes sent means the FIFO is draining per
/// packet, while a count that grows faster would mean a transmit-path bug.
const IMS_TDFU: u32 = 1 << 1;
/// Receive Descriptor minimum threshold exceeded: packets are waiting.
const IMS_RXD: u32 = 1 << 3;
/// Link status change. Raised on every carrier transition, so a link that is
/// still settling produces several before data flows.
const IMS_LINK: u32 = 1 << 4;
/// Receive Error: a descriptor arrived with its error bits set.
const IMS_RXC: u32 = 1 << 7;
/// Receive Status: a descriptor arrived with status bits set.
const IMS_RXS: u32 = 1 << 12;

const IMS_RECEIVE: u32 = IMS_RXD | IMS_RXC | IMS_RXS;
/// Everything acknowledged as expected. Any cause outside this set is counted
/// rather than ignored, so a bit this driver does not model is visible instead of
/// silently discarded.
const IMS_ALL: u32 = IMS_RECEIVE | IMS_TXDW | IMS_TDFU | IMS_LINK;
const RCTL_EN: u32 = 1 << 1;
const RCTL_SBP: u32 = 1 << 2;
const RCTL_UPE: u32 = 1 << 3;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_LPE: u32 = 1 << 5;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_BSIZE_2048: u32 = 0 << 16;
const RCTL_BSIZE_1024: u32 = 1 << 16;
const RCTL_BSIZE_512: u32 = 2 << 16;
const RCTL_BSIZE_256: u32 = 3 << 16;
const RCTL_SECRC: u32 = 1 << 26;

const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

// Descriptor counts
const RX_DESC_COUNT: usize = 32;
const TX_DESC_COUNT: usize = 8;

// Buffer size
const BUFFER_SIZE: usize = 2048;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDescriptor {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDescriptor {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

pub struct E1000 {
    mem_base: usize,
    mac_address: [u8; 6],
    rx_descriptors: Vec<RxDescriptor>,
    tx_descriptors: Vec<TxDescriptor>,
    rx_buffers: Vec<Vec<u8>>,
    tx_buffers: Vec<Vec<u8>>,
    rx_current: usize,
    tx_current: usize,
    phys_mem_offset: u64,
}

impl E1000 {
    pub fn new(mem_base: usize, phys_mem_offset: u64) -> Self {
        let mut driver = E1000 {
            mem_base,
            mac_address: [0; 6],
            rx_descriptors: alloc::vec![RxDescriptor {
                addr: 0,
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            }; RX_DESC_COUNT],
            tx_descriptors: alloc::vec![TxDescriptor {
                addr: 0,
                length: 0,
                cso: 0,
                cmd: 0,
                status: 0,
                css: 0,
                special: 0,
            }; TX_DESC_COUNT],
            rx_buffers: alloc::vec![alloc::vec![0u8; BUFFER_SIZE]; RX_DESC_COUNT],
            tx_buffers: alloc::vec![alloc::vec![0u8; BUFFER_SIZE]; TX_DESC_COUNT],
            rx_current: 0,
            tx_current: 0,
            phys_mem_offset,
        };

        driver.init();
        driver
    }

    fn read_reg(&self, reg: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.mem_base + reg as usize) as *const u32) }
    }

    fn write_reg(&self, reg: u32, value: u32) {
        unsafe {
            core::ptr::write_volatile((self.mem_base + reg as usize) as *mut u32, value);
        }
    }

    /// Physical address for the NIC to DMA to/from.
    ///
    /// Returns `0` on failure rather than a plausible-looking address: the
    /// previous version logged and then returned the *untranslated* virtual
    /// address, handing the DMA engine a target in the middle of the kernel
    /// image. A descriptor pointing at the wrong page corrupts memory silently;
    /// a zero address is at least a dead ring the caller can detect.
    ///
    /// Translation walks the page tables (correct for any mapping), then falls
    /// back to the direct map for heap buffers, and finally confirms the
    /// result is reachable by the E1000's 32-bit DMA engine.
    fn dma_addr(&self, ptr: *const u8) -> u64 {
        let virt = ptr as u64;
        let phys = self.translate(virt).unwrap_or_else(|| {
            crate::serial_println!("[e1000] cannot translate DMA address {:#x}", virt);
            0
        });
        // The E1000's descriptor engine uses 32-bit physical addresses.
        match crate::memory::addr::dma_phys(phys) {
            Some(p) => p,
            None => {
                crate::serial_println!(
                    "[e1000] DMA phys {:#x} >= 4 GiB, descriptor engine cannot reach it",
                    phys
                );
                0
            }
        }
    }

    /// Best-effort physical address for `virt`, with no reachability check.
    fn translate(&self, virt: u64) -> Option<u64> {
        if self.phys_mem_offset != 0 {
            use x86_64::registers::control::Cr3;
            use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};
            use x86_64::VirtAddr;

            let (frame, _) = Cr3::read();
            let p4_virt = VirtAddr::new(
                self.phys_mem_offset
                    .wrapping_add(frame.start_address().as_u64()),
            );
            let page_table = unsafe { &mut *(p4_virt.as_mut_ptr() as *mut PageTable) };
            let mapper =
                unsafe { OffsetPageTable::new(page_table, VirtAddr::new(self.phys_mem_offset)) };
            if let Some(phys) = mapper.translate_addr(VirtAddr::new(virt)) {
                return Some(phys.as_u64());
            }
        }
        // Identity mapping (BIOS, or a low heap).
        if virt < 0x1_0000_0000 {
            return Some(virt);
        }
        // Direct map (heap carved from the firmware map).
        if self.phys_mem_offset != 0 && virt >= self.phys_mem_offset {
            return Some(virt - self.phys_mem_offset);
        }
        None
    }

    fn read_eeprom(&self, addr: u8) -> u16 {
        self.write_reg(REG_EEPROM, 1 | ((addr as u32) << 8));

        for _ in 0..1_000_000 {
            let tmp = self.read_reg(REG_EEPROM);
            if (tmp & (1 << 4)) != 0 {
                return ((tmp >> 16) & 0xFFFF) as u16;
            }
        }
        0
    }

    fn read_mac_address(&mut self) {
        let mac_low = self.read_eeprom(0);
        let mac_mid = self.read_eeprom(1);
        let mac_high = self.read_eeprom(2);

        self.mac_address[0] = (mac_low & 0xFF) as u8;
        self.mac_address[1] = (mac_low >> 8) as u8;
        self.mac_address[2] = (mac_mid & 0xFF) as u8;
        self.mac_address[3] = (mac_mid >> 8) as u8;
        self.mac_address[4] = (mac_high & 0xFF) as u8;
        self.mac_address[5] = (mac_high >> 8) as u8;
    }

    fn init(&mut self) {
        // Read MAC addressx
        self.read_mac_address();

        // Enable bus mastering and memory access
        self.write_reg(REG_CTRL, self.read_reg(REG_CTRL) | CTRL_SLU);

        // Setup receive descriptors
        for i in 0..RX_DESC_COUNT {
            let virt_addr = &self.rx_buffers[i][0] as *const u8 as u64;
            let phys_addr = self.dma_addr(virt_addr as *const u8);
            self.rx_descriptors[i].addr = phys_addr;
            self.rx_descriptors[i].status = 0;
        }

        let rx_desc_phys = self.dma_addr(self.rx_descriptors.as_ptr() as *const u8);

        self.write_reg(REG_RXDESCLO, (rx_desc_phys & 0xFFFFFFFF) as u32);
        self.write_reg(REG_RXDESCHI, (rx_desc_phys >> 32) as u32);
        self.write_reg(REG_RXDESCLEN, (RX_DESC_COUNT * 16) as u32);
        self.write_reg(REG_RXDESCHEAD, 0);
        self.write_reg(REG_RXDESCTAIL, (RX_DESC_COUNT - 1) as u32);

        // Setup transmit descriptors
        for i in 0..TX_DESC_COUNT {
            let virt_addr = &self.tx_buffers[i][0] as *const u8 as u64;
            let phys_addr = self.dma_addr(virt_addr as *const u8);
            self.tx_descriptors[i].addr = phys_addr;
            self.tx_descriptors[i].status = 1; // DD bit
            self.tx_descriptors[i].cmd = 0;
        }

        let tx_desc_phys = self.dma_addr(self.tx_descriptors.as_ptr() as *const u8);
        self.write_reg(REG_TXDESCLO, (tx_desc_phys & 0xFFFFFFFF) as u32);
        self.write_reg(REG_TXDESCHI, (tx_desc_phys >> 32) as u32);
        self.write_reg(REG_TXDESCLEN, (TX_DESC_COUNT * 16) as u32);
        self.write_reg(REG_TXDESCHEAD, 0);
        self.write_reg(REG_TXDESCTAIL, 0);

        // Enable receive
        self.write_reg(
            REG_RCTRL,
            RCTL_EN | RCTL_SBP | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_BSIZE_2048 | RCTL_SECRC,
        );

        // Enable transmit
        self.write_reg(REG_TCTRL, TCTL_EN | TCTL_PSP | (15 << 4) | (64 << 12));

        // Start with interrupts masked: the cause register latches whatever the
        // card saw during this sequence, and an unmasked line with a stale cause
        // bit fires once immediately for work already handled here.
        self.write_reg(REG_IMASK, 0);
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    /// Hand one frame to the card.
    ///
    /// Blocks until the target descriptor's DD bit is set, so the TX ring cannot
    /// be overrun by a caller that sends faster than the link drains. With
    /// interrupts enabled the wait could instead park until the TX interrupt,
    /// but the pump is the only context that runs, so spinning here is what
    /// actually makes progress.
    pub fn send_packet(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() > BUFFER_SIZE {
            return Err("Packet too large");
        }

        let desc_index = self.tx_current;

        let mut status =
            unsafe { core::ptr::read_volatile(&self.tx_descriptors[desc_index].status) };
        if status & 1 == 0 {
            for _ in 0..100_000 {
                core::hint::spin_loop();
                status =
                    unsafe { core::ptr::read_volatile(&self.tx_descriptors[desc_index].status) };
                if status & 1 != 0 {
                    break;
                }
            }
        }
        if status & 1 == 0 {
            return Err("TX queue full");
        }

        // Copy data to buffer
        self.tx_buffers[desc_index][..data.len()].copy_from_slice(data);

        // Setup descriptor
        self.tx_descriptors[desc_index].length = data.len() as u16;
        self.tx_descriptors[desc_index].cmd = (1 << 0) | (1 << 1) | (1 << 3); // EOP, IFCS, RS
        self.tx_descriptors[desc_index].status = 0;
        core::sync::atomic::fence(Ordering::SeqCst);

        // Update tail
        self.tx_current = (self.tx_current + 1) % TX_DESC_COUNT;
        self.write_reg(REG_TXDESCTAIL, self.tx_current as u32);

        crate::net_log!(
            "E1000: TX packet {} bytes, desc={}, tail={}",
            data.len(),
            desc_index,
            self.tx_current
        );

        Ok(())
    }

    pub fn receive_packet(&mut self) -> Option<Vec<u8>> {
        for _ in 0..RX_DESC_COUNT {
            let desc_index = self.rx_current;
            let hardware_head = (self.read_reg(REG_RXDESCHEAD) as usize) % RX_DESC_COUNT;
            if desc_index == hardware_head {
                return None;
            }

            let status =
                unsafe { core::ptr::read_volatile(&self.rx_descriptors[desc_index].status) };
            if status & 1 == 0 {
                return None;
            }
            core::sync::atomic::fence(Ordering::Acquire);

            let length =
                unsafe { core::ptr::read_volatile(&self.rx_descriptors[desc_index].length) }
                    as usize;
            let errors =
                unsafe { core::ptr::read_volatile(&self.rx_descriptors[desc_index].errors) };
            if length == 0 || length > BUFFER_SIZE || errors != 0 {
                crate::net_log!(
                    "E1000: dropping invalid RX descriptor {} (length={}, status={:#x}, errors={:#x})",
                    desc_index,
                    length,
                    status,
                    errors
                );
                self.reclaim_rx_descriptor(desc_index);
                continue;
            }

            crate::net_log!(
                "E1000: RX descriptor {} has packet, length={}, status={:#x}",
                desc_index,
                length,
                status
            );
            let packet = self.rx_buffers[desc_index][..length].to_vec();
            self.reclaim_rx_descriptor(desc_index);
            return Some(packet);
        }
        None
    }

    fn reclaim_rx_descriptor(&mut self, desc_index: usize) {
        core::sync::atomic::fence(Ordering::Release);
        unsafe {
            core::ptr::write_volatile(&mut self.rx_descriptors[desc_index].status, 0);
        }
        self.rx_current = (desc_index + 1) % RX_DESC_COUNT;
        self.write_reg(REG_RXDESCTAIL, desc_index as u32);
    }
}

lazy_static! {
    pub static ref E1000_DRIVER: Mutex<Option<E1000>> = Mutex::new(None);
}

/// Whether an RX interrupt has arrived and not yet been serviced.
///
/// The handler itself only sets this flag. Draining the ring touches the
/// descriptor array and allocates for the packet, neither of which belongs in
/// interrupt context: the handler can interrupt `receive_packet` mid-update and
/// deadlock on the driver lock. The pump is the only place that takes it.
static RX_PENDING: AtomicBool = AtomicBool::new(false);

/// Counters for `netstat`, so an interrupt-driven path that silently stops
/// delivering is distinguishable from one that was never enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct E1000Stats {
    pub interrupts: u64,
    pub tx_interrupts: u64,
    /// Cause bits seen that the handler did not expect.
    pub unexpected_causes: u64,
    /// Transmit FIFO underflows: the card ran out of data mid-packet.
    pub tx_underflows: u64,
    /// Ring drains that produced a packet.
    pub notified_packets: u64,
}

// Interrupt counters are atomics rather than a locked struct: the handler runs
// in interrupt context and must not block on a lock the pump may already hold.
static STATS_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static STATS_TX_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static STATS_UNEXPECTED: AtomicU64 = AtomicU64::new(0);
static STATS_UNDERFLOWS: AtomicU64 = AtomicU64::new(0);
static STATS_PACKETS: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the interrupt counters, for the shell.
pub fn stats() -> E1000Stats {
    E1000Stats {
        interrupts: STATS_INTERRUPTS.load(Ordering::Relaxed),
        tx_interrupts: STATS_TX_INTERRUPTS.load(Ordering::Relaxed),
        unexpected_causes: STATS_UNEXPECTED.load(Ordering::Relaxed),
        tx_underflows: STATS_UNDERFLOWS.load(Ordering::Relaxed),
        notified_packets: STATS_PACKETS.load(Ordering::Relaxed),
    }
}

/// Take and clear the pending-RX flag.
pub fn take_rx_pending() -> bool {
    RX_PENDING.swap(false, Ordering::AcqRel)
}

/// True when an RX interrupt is waiting to be serviced.
pub fn rx_pending() -> bool {
    RX_PENDING.load(Ordering::Acquire)
}

/// Acknowledge and mask all interrupts, leaving the driver polling-only.
///
/// Used by the test path and as the recovery when a handler is not wired up.
pub fn disable_interrupts() {
    if let Some(d) = E1000_DRIVER.lock().as_ref() {
        d.write_reg(REG_IMC, IMS_ALL);
        d.write_reg(REG_IMASK, 0);
    }
    RX_PENDING.store(false, Ordering::Release);
}

/// Masked cause bits from the last interrupt, for diagnostics.
static LAST_CAUSE: AtomicUsize = AtomicUsize::new(0);

/// Cause bits from the most recent interrupt.
pub fn last_cause() -> u32 {
    LAST_CAUSE.load(Ordering::Relaxed) as u32
}

/// Service one interrupt.
///
/// Uses no lock of any kind. Taking `E1000_DRIVER` here would deadlock: on a
/// uniprocessor the interrupt can arrive while the pump already holds that spin
/// lock inside `receive_packet`, and the handler would spin forever waiting for
/// a lock its own interrupt preempts. The MMIO window and the counters are
/// therefore plain atomics, published during `init`.
///
/// Reads ICR, which clears the latched cause and lets the line deassert, then
/// records the cause and flags receive work for the pump. Doing the ring walk
/// here instead would allocate in interrupt context and hit the same lock.
pub fn handle_interrupt() {
    let base = MMIO_BASE.load(Ordering::Relaxed);
    let cause = if base == 0 {
        0
    } else {
        // SAFETY: the address was published by `init` from a BAR0 the driver
        // already holds, and only this handler and `init` ever read it here.
        unsafe { core::ptr::read_volatile((base + REG_ICR as usize) as *const u32) }
    };
    LAST_CAUSE.store(cause as usize, Ordering::Relaxed);

    if cause != 0 {
        STATS_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
        if cause & IMS_TXDW != 0 {
            STATS_TX_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
        }
        if cause & IMS_TDFU != 0 {
            STATS_UNDERFLOWS.fetch_add(1, Ordering::Relaxed);
        }
        if cause & !(IMS_ALL) != 0 {
            STATS_UNEXPECTED.fetch_add(1, Ordering::Relaxed);
        }
        if cause & IMS_RECEIVE != 0 {
            RX_PENDING.store(true, Ordering::Release);
        }
    }

    // A spurious interrupt can arrive with no cause set; it still has to be
    // acknowledged or the line stays asserted.
    unsafe {
        crate::pic::PICS
            .lock()
            .notify_end_of_interrupt(PIC_VECTOR.load(Ordering::Relaxed) as u8);
    }
}

/// MMIO window base, published by `init` so the interrupt handler can reach the
/// registers without taking the driver lock.
static MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

/// PIC interrupt vector this device was assigned, or `0` when interrupts are
/// not in use.
static PIC_VECTOR: AtomicUsize = AtomicUsize::new(0);

/// The PIC vector this device interrupts on, if interrupts are enabled.
pub fn interrupt_vector() -> Option<usize> {
    match PIC_VECTOR.load(Ordering::Relaxed) {
        0 => None,
        v => Some(v),
    }
}

pub fn init(phys_mem_offset: u64) -> Result<(), &'static str> {
    crate::serial_println!("E1000: Starting initialization...");

    // Try to find E1000 device via PCI
    crate::serial_println!("E1000: Scanning PCI bus...");
    let pci_device = crate::drivers::pci::find_device(E1000_VENDOR_ID, E1000_DEVICE_ID);

    if pci_device.is_none() {
        crate::serial_println!("E1000: Device not found on PCI bus");
        return Err("E1000 device not found");
    }

    crate::serial_println!("E1000: Device found!");
    let pci_dev = pci_device.unwrap();
    pci_dev.enable_bus_mastering();
    let mem_base = pci_dev.mmio_base().ok_or("E1000 BAR0 is not memory")?;

    if mem_base == 0 {
        crate::serial_println!("E1000: Invalid BAR0 address");
        return Err("Invalid E1000 memory address");
    }

    crate::serial_println!(
        "E1000 found at PCI bus {}, device {}, function {}",
        pci_dev.bus,
        pci_dev.device,
        pci_dev.function
    );
    crate::serial_println!("  Physical memory base: {:#x}", mem_base);

    // The bootloader maps all physical memory at the supplied offset.
    let virt_base = (phys_mem_offset + mem_base) as usize;
    crate::serial_println!("  Virtual memory base: {:#x}", virt_base);

    // Published before the card is enabled, so the interrupt handler can reach
    // the registers from the moment the first interrupt can arrive.
    MMIO_BASE.store(virt_base, Ordering::Release);

    let driver = E1000::new(virt_base, phys_mem_offset);
    let mac = driver.mac_address();
    if mac == [0; 6] {
        return Err("E1000 EEPROM did not return a MAC address");
    }

    crate::serial_println!("E1000 initialized");
    crate::serial_println!(
        "  MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    // Only a handful of lines have a handler installed, so a card placed anywhere
    // else keeps polling: unmasking its cause register with no vector behind it
    // would leave the PIC's interrupt stuck asserted.
    let irq = pci_dev.interrupt_pin_line().map(|(_, line)| line);
    let driver = E1000::new(virt_base, phys_mem_offset);
    let mac = driver.mac_address();
    if mac == [0; 6] {
        return Err("E1000 EEPROM did not return a MAC address");
    }

    crate::serial_println!("E1000 initialized");
    crate::serial_println!(
        "  MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    *E1000_DRIVER.lock() = Some(driver);

    crate::serial_println!(
        "  PCI interrupt pin {} line {}",
        pci_dev.interrupt_pin_line().map(|p| p.0).unwrap_or(0),
        irq.unwrap_or(0)
    );

    match irq {
        Some(irq) if crate::interrupts::is_nic_irq(irq) => {
            let vector = crate::interrupts::PIC_BASE as usize + irq as usize;
            {
                let driver = E1000_DRIVER.lock();
                if let Some(d) = driver.as_ref() {
                    d.write_reg(REG_IMASK, IMS_ALL);
                }
            }
            PIC_VECTOR.store(vector, Ordering::Release);
            unsafe {
                crate::pic::PICS.lock().set_mask(irq, false);
            }
            crate::serial_println!("  IRQ {} (vector {}), interrupts enabled", irq, vector);
        }
        Some(irq) => {
            crate::serial_println!(
                "  IRQ {} has no installed handler, staying in polling mode",
                irq
            );
        }
        None => crate::serial_println!("  No INTx line assigned, polling"),
    }

    Ok(())
}

pub fn send_packet(data: &[u8]) -> Result<(), &'static str> {
    let mut driver = E1000_DRIVER.lock();
    if let Some(ref mut d) = *driver {
        d.send_packet(data)
    } else {
        Err("E1000 not initialized")
    }
}

pub fn receive_packet() -> Option<Vec<u8>> {
    let mut driver = E1000_DRIVER.lock();
    if let Some(ref mut d) = *driver {
        let packet = d.receive_packet();
        if packet.is_some() {
            STATS_PACKETS.fetch_add(1, Ordering::Relaxed);
        }
        packet
    } else {
        None
    }
}

pub fn mac_address() -> Option<[u8; 6]> {
    let driver = E1000_DRIVER.lock();
    driver.as_ref().map(|d| d.mac_address())
}
