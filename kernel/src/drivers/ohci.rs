//! OHCI 1.0 host-controller driver — bring-up, control transfers,
//! enumeration and HID boot device claim.
//!
//! Polling-only, like the UHCI/EHCI/xHCI drivers. OHCI differs from UHCI in
//! the schedule model: instead of a frame list of TDs, the controller walks
//! endpoint-descriptor (ED) lists hanging off HcControlHeadED, and periodic
//! EDs are reached only through the 32-entry schedule table inside the HCCA
//! (Host Controller Communications Area).
//!
//! Descriptor layout follows OHCI 1.0 §4.3 and Linux `ohci.h`:
//!
//! ```text
//! ED (32B):  hwINFO | hwTailP | hwHeadP | hwNextED
//! TD (16B):  hwINFO | hwCBP    | hwNextTD| hwBE
//! ```
//!
//! Two protocol points are easy to get wrong:
//!
//! * **The tail is the terminator, not the last real TD.** The controller
//!   runs `while (head != tail)`, so a TD chain is published by filling the
//!   TDs, then a single write of `hwTailP` pointing at an inert dummy TD.
//!   Descriptor contents must be written first, then `hwTailP`.
//! * **A TD's length comes from `hwCBP`/`hwBE`, not a length field**, and a
//!   zero-length transfer is encoded as `hwCBP == 0 && hwBE == 0`.
//!
//! Completion is detected by the controller rewriting a TD's condition-code
//! field: TDs are armed with `TD_NOTACCESSED` (0xF) and the HC overwrites it,
//! so `CC != 0xF` means the transfer retired.

use alloc::vec::Vec;

use crate::drivers::usb::{
    delay_ms, dispatch_report, dma_page, find_hid_keyboard, find_hid_pointer, hid_binterval,
    io_r32, io_w32, parse_tablet_layout, setup_packet, HidPointerKind, StreamKind, TabletLayout,
};

// ---------------------------------------------------------------------------
// Register offsets (OHCI 1.0 §7, matches Linux `struct ohci_regs`)
// ---------------------------------------------------------------------------

const R_HC_REVISION: u16 = 0x00;
const R_HC_CONTROL: u16 = 0x04;
const R_HC_COMMAND_STATUS: u16 = 0x08;
const R_HC_HCCA: u16 = 0x18;
const R_HC_CONTROL_HEAD_ED: u16 = 0x20;
const R_HC_DONE_HEAD: u16 = 0x30;
const R_HC_FM_INTERVAL: u16 = 0x34;
const R_HC_PERIODIC_START: u16 = 0x40;
const R_HC_LS_THRESHOLD: u16 = 0x44;
const R_HC_RH_STATUS: u16 = 0x50;
const R_HC_RH_PORT_STATUS: u16 = 0x54;

// HcControl
const HCCTRL_PLE: u32 = 1 << 2;
const HCCTRL_IE: u32 = 1 << 3;
const HCCTRL_CLE: u32 = 1 << 4;
const HCCTRL_BLE: u32 = 1 << 5;
const HCCTRL_HCFS_MASK: u32 = 3 << 6;
const HCCTRL_OPERATIONAL: u32 = 1 << 6;

// HcCommandStatus
const HCCS_HCR: u32 = 1 << 0;
const HCCS_CLF: u32 = 1 << 1;
const HCCS_BLF: u32 = 1 << 2;

// HcRevision
const OHCI_REV_1_0: u32 = 0x10;
const OHCI_REV_1_1: u32 = 0x11;

// HcRhPortStatus (§7.4.2)
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PESC: u32 = 1 << 1;
const PORTSC_PESS: u32 = 1 << 2;
const PORTSC_POCI: u32 = 1 << 3;
const PORTSC_PRSC: u32 = 1 << 4;
const PORTSC_PRSS: u32 = 1 << 5;
const PORTSC_PPS: u32 = 1 << 8;
const PORTSC_LSDA: u32 = 1 << 9;
const PORTSC_CSC: u32 = 1 << 16;
const PORTSC_PSSC: u32 = 1 << 18;
const PORTSC_OCIC: u32 = 1 << 19;
const PORTSC_PRSC_W1C: u32 = 1 << 20;
const PORTSC_W1C: u32 = PORTSC_CSC | PORTSC_PESC | PORTSC_PSSC | PORTSC_OCIC | PORTSC_PRSC_W1C;

/// Enable assertion. QEMU's `pci-ohci` models the enable bit at 1 and the
/// reset bit at 4, while OHCI 1.0 §7.4.2 places the "status set" bits at 2
/// and 5. Writing both is harmless on real hardware (bit 1/4 are write-1-to-
/// clear change bits) and works on either model.
const PORTSC_ENABLE: u32 = PORTSC_PESS | PORTSC_PESC;
const PORTSC_RESET: u32 = PORTSC_PRSS | PORTSC_PRSC;

// Frame interval: 12000 bit times per frame, minus one.
const FM_INTERVAL: u32 = 0x2EDF;
const FM_FIT: u32 = 1 << 31;

// ED hwINFO
const ED_FA_SHIFT: u32 = 0;
const ED_FA_MASK: u32 = 0x7F << ED_FA_SHIFT;
const ED_EN_SHIFT: u32 = 7;
const ED_EN_MASK: u32 = 0xF << ED_EN_SHIFT;
const ED_IN: u32 = 0x02 << 11;
const ED_OUT: u32 = 0x01 << 11;
const ED_LOWSPEED: u32 = 1 << 13;
const ED_ISO: u32 = 1 << 15;
const ED_MPS_SHIFT: u32 = 16;
const ED_MPS_MASK: u32 = 0x7FF << ED_MPS_SHIFT;

// ED hwHeadP status bits
const ED_C: u32 = 0x02;
const ED_H: u32 = 0x01;

// TD hwINFO
const TD_CC_SHIFT: u32 = 28;
const TD_CC_MASK: u32 = 0xF << TD_CC_SHIFT;
const TD_EC: u32 = 0x3 << 26;
const TD_T_DATA0: u32 = 0x2 << 24;
const TD_T_DATA1: u32 = 0x3 << 24;
const TD_T_TOGGLE: u32 = 0x0 << 24;
const TD_DI: u32 = 0x7 << 21;
const TD_DP_IN: u32 = 0x1 << 19;
const TD_DP_OUT: u32 = 0x2 << 19;
const TD_R: u32 = 1 << 18;
const TD_NOTACCESSED: u32 = 0xF << TD_CC_SHIFT;

const CC_NOERROR: u32 = 0;
const CC_STALL: u32 = 4;

// ---------------------------------------------------------------------------
// DMA structures
// ---------------------------------------------------------------------------

#[repr(C, align(16))]
struct Ed {
    hwinfo: u32,
    tail: u32,
    head: u32,
    next: u32,
}

const ED_BYTES: usize = 32;

#[repr(C, align(16))]
struct Td {
    hwinfo: u32,
    cbp: u32,
    next: u32,
    be: u32,
}

const TD_BYTES: usize = 16;

/// HCCA is 256 bytes: a 32-entry periodic schedule table, then the frame
/// counter and done-head pointer the controller writes back.
const HCCA_BYTES: usize = 256;
const HCCA_INTS: usize = 32;
const HCCA_DONE_OFF: usize = 128;

/// Control scratch: one control ED, four TDs (setup/data/status/dummy) and
/// the setup + data buffers. One page covers the 4096-byte control cap.
const CTRL_ED_OFF: usize = 0;
const CTRL_TD_SETUP: usize = 32;
const CTRL_TD_DATA: usize = 48;
const CTRL_TD_STATUS: usize = 64;
const CTRL_TD_DUMMY: usize = 80;
const CTRL_SETUP_BUF: usize = 96;
const CTRL_DATA_BUF: usize = 112;
const CTRL_DATA_MAX: usize = 256;

/// Per-interrupt-endpoint page: one ED, one real TD, one dummy terminator,
/// and the report buffer.
const STREAM_ED_OFF: usize = 0;
const STREAM_TD_OFF: usize = 32;
const STREAM_TD_DUMMY: usize = 48;
const STREAM_BUF_OFF: usize = 64;
const STREAM_BUF_MAX: usize = 64;

const MAX_HID_STREAMS: usize = 8;
const MAX_PORTS: u8 = 2;

// ---------------------------------------------------------------------------
// Claimed HID endpoint
// ---------------------------------------------------------------------------

/// A claimed interrupt endpoint. The single TD is re-armed after each
/// completed transfer; the dummy TD terminates the chain so the controller's
/// `head != tail` walk stops.
struct HidStream {
    page_virt: u64,
    ed_phys: u32,
    td_phys: u32,
    kind: StreamKind,
    layout: Option<TabletLayout>,
}

impl HidStream {
    fn ed(&self) -> *mut Ed {
        unsafe { self.page_virt as *mut Ed }
    }
    fn td(&self) -> *mut Td {
        unsafe { (self.page_virt + STREAM_TD_OFF as u64) as *mut Td }
    }
    fn buf(&self) -> *mut u8 {
        unsafe { (self.page_virt + STREAM_BUF_OFF as u64) as *mut u8 }
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

pub struct OhciController {
    pci_bus: u8,
    pci_dev: u8,
    pci_func: u8,
    io: u16,
    n_ports: u8,

    hcca_virt: u64,
    hcca_phys: u64,
    ctrl_virt: u64,
    ctrl_phys: u64,
    ctrl_ed_phys: u32,
    ctrl_dummy_phys: u32,

    next_addr: u8,
    seen: u8,
    keyboards: Vec<HidStream>,
    mice: Vec<HidStream>,
}

// SAFETY: owns leaked DMA pages and raw pointers into them; all access is
// serialized by the `UsbState` mutex.
unsafe impl Send for OhciController {}

impl OhciController {
    // ---- register access -------------------------------------------------

    #[inline]
    fn r32(&self, off: u16) -> u32 {
        unsafe { io_r32(self.io + off) }
    }

    #[inline]
    fn w32(&self, off: u16, val: u32) {
        unsafe { io_w32(self.io + off, val) }
    }

    #[inline]
    fn portsc(&self, port: u8) -> u32 {
        self.r32(R_HC_RH_PORT_STATUS + (port as u16) * 4)
    }

    #[inline]
    fn set_portsc(&self, port: u8, val: u32) {
        self.w32(R_HC_RH_PORT_STATUS + (port as u16) * 4, val);
    }

    #[inline]
    fn hcca_int(&self, slot: usize) -> *mut u32 {
        unsafe { (self.hcca_virt as *mut u32).add(slot) }
    }

    // ---- bring-up --------------------------------------------------------

    pub fn new(
        pci: crate::drivers::pci::PciDevice,
        _phys_mem_offset: u64,
    ) -> Result<Self, &'static str> {
        pci.enable_bus_mastering();
        let io = pci
            .bar_io_base(0)
            .ok_or("OHCI I/O BAR0 is not a firmware-assigned I/O window")?;

        let mut ctl = Self {
            pci_bus: pci.bus,
            pci_dev: pci.device,
            pci_func: pci.function,
            io,
            n_ports: MAX_PORTS,
            hcca_virt: 0,
            hcca_phys: 0,
            ctrl_virt: 0,
            ctrl_phys: 0,
            ctrl_ed_phys: 0,
            ctrl_dummy_phys: 0,
            next_addr: 1,
            seen: 0,
            keyboards: Vec::new(),
            mice: Vec::new(),
        };

        crate::println!(
            "[usb] OHCI {:02x}:{:02x}.{} {:04x}:{:04x} io={:#06x}",
            ctl.pci_bus,
            ctl.pci_dev,
            ctl.pci_func,
            pci.vendor_id,
            pci.device_id,
            ctl.io
        );

        ctl.reset()?;
        ctl.allocate()?;
        ctl.program()?;

        Ok(ctl)
    }

    fn reset(&self) -> Result<(), &'static str> {
        // Drain any outstanding status, then request a global reset.
        self.w32(R_HC_COMMAND_STATUS, HCCS_HCR);
        for _ in 0..2000 {
            if self.r32(R_HC_COMMAND_STATUS) & HCCS_HCR == 0 {
                let rev = self.r32(R_HC_REVISION) & 0xFF;
                if rev != OHCI_REV_1_0 && rev != OHCI_REV_1_1 {
                    crate::println!("[usb] OHCI unexpected revision {:#x}", rev);
                }
                crate::println!(
                    "[usb] OHCI {:02x}:{:02x}.{} reset ok (rev {:#x})",
                    self.pci_bus, self.pci_dev, self.pci_func, rev
                );
                return Ok(());
            }
            delay_ms(1);
        }
        Err("OHCI global reset timeout")
    }

    fn allocate(&mut self) -> Result<(), &'static str> {
        // HCCA must be 256-byte aligned; a 4 KiB page satisfies that.
        let (hv, hp, hptr) = dma_page().ok_or("OHCI HCCA alloc")?;
        unsafe { core::ptr::write_bytes(hptr, 0, HCCA_BYTES) };
        self.hcca_virt = hv;
        self.hcca_phys = hp;

        let (cv, cp, cptr) = dma_page().ok_or("OHCI ctrl scratch alloc")?;
        unsafe { core::ptr::write_bytes(cptr, 0, 4096) };
        self.ctrl_virt = cv;
        self.ctrl_phys = cp;
        self.ctrl_ed_phys = (cp as u32) + CTRL_ED_OFF as u32;
        self.ctrl_dummy_phys = (cp as u32) + CTRL_TD_DUMMY as u32;
        Ok(())
    }

    fn program(&mut self) -> Result<(), &'static str> {
        // A reset clears HcHCCA, so it must be programmed after the reset.
        self.w32(R_HC_HCCA, (self.hcca_phys & 0xFFFF_FF00) as u32);

        // The dummy TD self-points so the controller's `head != tail` walk
        // terminates without executing it.
        unsafe {
            let dummy = (self.ctrl_virt + CTRL_TD_DUMMY as u64) as *mut Td;
            (*dummy).hwinfo = TD_NOTACCESSED;
            (*dummy).cbp = 0;
            (*dummy).be = 0;
            (*dummy).next = self.ctrl_dummy_phys;

            let ed = self.ctrl_virt as *mut Ed;
            (*ed).hwinfo = 0;
            (*ed).tail = self.ctrl_dummy_phys;
            (*ed).head = self.ctrl_dummy_phys;
            (*ed).next = 0;
        }

        // Frame timing: FI = 12000 bit times, periodic schedule starts at 90%
        // of the frame so the front is left for the control slice.
        self.w32(R_HC_FM_INTERVAL, FM_FIT | FM_INTERVAL);
        self.w32(R_HC_PERIODIC_START, (9 * FM_INTERVAL) / 10);
        self.w32(R_HC_LS_THRESHOLD, 0x628);

        // Lists start empty.
        self.w32(R_HC_CONTROL_HEAD_ED, 0);
        let ctl = self.r32(R_HC_CONTROL);
        self.w32(
            R_HC_CONTROL,
            (ctl & !HCCTRL_HCFS_MASK) | HCCTRL_OPERATIONAL,
        );
        Ok(())
    }

    // ---- control transfers ----------------------------------------------

    fn control_xfer(
        &self,
        addr: u8,
        maxpacket: u8,
        setup: [u8; 8],
        data_out: Option<&[u8]>,
        data_in_len: usize,
    ) -> Option<Vec<u8>> {
        let is_in = setup[0] & 0x80 != 0;
        let data_len = if is_in {
            data_in_len
        } else {
            data_out.map(|d| d.len()).unwrap_or(0)
        };
        if data_len > CTRL_DATA_MAX {
            crate::serial_println!("[usb] OHCI control data {} exceeds buffer", data_len);
            return None;
        }
        let mxp = maxpacket.clamp(8, 64) as u32;

        let setup_phys = (self.ctrl_phys as u32) + CTRL_TD_SETUP as u32;
        let data_phys = (self.ctrl_phys as u32) + CTRL_TD_DATA as u32;
        let status_phys = (self.ctrl_phys as u32) + CTRL_TD_STATUS as u32;

        unsafe {
            let base = self.ctrl_virt as *mut u8;
            core::ptr::write_bytes(self.ctrl_virt as *mut u8, 0, 4096);
            core::ptr::copy_nonoverlapping(setup.as_ptr(), base.add(CTRL_SETUP_BUF), 8);
            if is_in {
                core::ptr::write_bytes(base.add(CTRL_DATA_BUF), 0, data_len);
            } else if let Some(d) = data_out {
                core::ptr::copy_nonoverlapping(d.as_ptr(), base.add(CTRL_DATA_BUF), data_len);
            }

            // Re-arm the dummy terminator.
            let dummy = base.add(CTRL_TD_DUMMY).cast::<Td>();
            (*dummy).hwinfo = TD_NOTACCESSED;
            (*dummy).cbp = 0;
            (*dummy).be = 0;
            (*dummy).next = self.ctrl_dummy_phys;

            // SETUP: DATA0, 8 bytes, chained to the data stage.
            let td = base.add(CTRL_TD_SETUP).cast::<Td>();
            (*td).hwinfo = TD_NOTACCESSED | TD_T_DATA0;
            (*td).cbp = (self.ctrl_phys as u32) + CTRL_SETUP_BUF as u32;
            (*td).be = ((self.ctrl_phys as u32) + CTRL_SETUP_BUF as u32) + 7;
            (*td).next = data_phys;

            // DATA: DATA1, TD_R so a short IN does not halt the ED.
            let td = base.add(CTRL_TD_DATA).cast::<Td>();
            (*td).hwinfo = TD_NOTACCESSED
                | TD_EC
                | TD_T_DATA1
                | TD_R
                | if is_in { TD_DP_IN } else { TD_DP_OUT };
            if data_len > 0 {
                (*td).cbp = (self.ctrl_phys as u32) + CTRL_DATA_BUF as u32;
                (*td).be = ((self.ctrl_phys as u32) + CTRL_DATA_BUF as u32) + data_len as u32 - 1;
            } else {
                // A zero-length stage is the all-zero buffer descriptor.
                (*td).cbp = 0;
                (*td).be = 0;
            }
            (*td).next = status_phys;

            // STATUS: zero length, opposite direction (IN when there was no
            // data stage or the data stage was OUT).
            let td = base.add(CTRL_TD_STATUS).cast::<Td>();
            (*td).hwinfo = TD_NOTACCESSED
                | TD_EC
                | TD_T_DATA1
                | if is_in { TD_DP_OUT } else { TD_DP_IN };
            (*td).cbp = 0;
            (*td).be = 0;
            (*td).next = self.ctrl_dummy_phys;

            // Control ED: function address and MPS; direction lives in the
            // TDs, not the ED.
            let ed = self.ctrl_virt as *mut Ed;
            (*ed).hwinfo = ((addr as u32) << ED_FA_SHIFT) & ED_FA_MASK
                | ((mxp) << ED_MPS_SHIFT) & ED_MPS_MASK;
            (*ed).head = setup_phys;
            (*ed).tail = self.ctrl_dummy_phys;
            (*ed).next = 0;
        }

        // The control ED is the whole list, so point the list head at it and
        // kick the controller.
        self.w32(R_HC_CONTROL_HEAD_ED, self.ctrl_ed_phys);
        let ctl = self.r32(R_HC_CONTROL);
        if ctl & HCCTRL_CLE == 0 {
            self.w32(R_HC_CONTROL, ctl | HCCTRL_CLE);
        }
        self.w32(R_HC_COMMAND_STATUS, HCCS_CLF);

        // Wait for the status TD's condition code to be rewritten.
        let status_td = unsafe { (self.ctrl_virt as *mut u8).add(CTRL_TD_STATUS).cast::<Td>() };
        let mut done = false;
        let mut cc = TD_NOTACCESSED;
        for _ in 0..40_000 {
            let info = unsafe { core::ptr::read_volatile(&(*status_td).hwinfo) };
            cc = info & TD_CC_MASK;
            if cc != TD_NOTACCESSED {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Consume the done-head writeback so the chain does not grow, and
        // reset the ED for the next transfer.
        self.w32(R_HC_DONE_HEAD, 0);
        unsafe {
            core::ptr::write_volatile(
                &mut (*(self.ctrl_virt as *mut Ed)).head,
                self.ctrl_dummy_phys,
            );
            core::ptr::write_volatile(
                &mut (*(self.ctrl_virt as *mut Ed)).tail,
                self.ctrl_dummy_phys,
            );
        }
        self.w32(R_HC_CONTROL_HEAD_ED, 0);

        if !done {
            crate::serial_println!("[usb] OHCI control timeout addr={}", addr);
            return None;
        }
        if cc >> TD_CC_SHIFT != CC_NOERROR {
            crate::serial_println!(
                "[usb] OHCI control error addr={} cc={:#x}",
                addr,
                cc >> TD_CC_SHIFT
            );
            return None;
        }

        if !is_in || data_len == 0 {
            return Some(Vec::new());
        }

        // The controller advances cbp as data lands. cbp == 0 means the
        // whole buffer was consumed.
        let data_td = unsafe { (self.ctrl_virt as *mut u8).add(CTRL_TD_DATA).cast::<Td>() };
        let cbp = unsafe { core::ptr::read_volatile(&(*data_td).cbp) } & !0xFFF;
        let n = if cbp == 0 {
            data_len
        } else {
            let off = unsafe { core::ptr::read_volatile(&(*data_td).cbp) } & 0xFFF;
            let n = if off == 0 { data_len } else { off as usize };
            n.min(data_len)
        };

        let mut out = Vec::with_capacity(n);
        unsafe {
            let src = (self.ctrl_virt + CTRL_DATA_BUF as u64) as *const u8;
            for i in 0..n {
                out.push(*src.add(i));
            }
        }
        Some(out)
    }

    // ---- port scan / enumeration -----------------------------------------

    pub fn enumerate(&mut self) {
        for port in 0..self.n_ports {
            self.scan_port(port);
        }
    }

    pub fn rescan_ports(&mut self) {
        self.enumerate();
    }

    fn scan_port(&mut self, port: u8) {
        if !self.reset_port(port) {
            return;
        }
        if self.seen & (1 << port) != 0 {
            return;
        }
        if self.enumerate_device(port) {
            self.seen |= 1 << port;
        }
    }

    fn port_low_speed(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_LSDA != 0
    }

    fn reset_port(&self, port: u8) -> bool {
        for _ in 0..2 {
            if self.portsc(port) & PORTSC_CCS == 0 {
                return false;
            }
            // Power, clear change bits, disable.
            let v = (self.portsc(port) | PORTSC_PPS | PORTSC_W1C) & !PORTSC_ENABLE;
            self.set_portsc(port, v);
            delay_ms(20);

            self.set_portsc(port, self.portsc(port) | PORTSC_RESET);
            delay_ms(50);
            let mut released = false;
            for _ in 0..200 {
                if self.portsc(port) & PORTSC_RESET == 0 {
                    released = true;
                    break;
                }
                delay_ms(1);
            }
            if !released {
                continue;
            }
            let v = (self.portsc(port) | PORTSC_ENABLE | PORTSC_W1C) & !PORTSC_PESC;
            self.set_portsc(port, v);
            if self.portsc(port) & PORTSC_CCS != 0 {
                return true;
            }
        }
        false
    }

    fn enumerate_device(&mut self, port: u8) -> bool {
        let low_speed = self.port_low_speed(port);

        let setup = setup_packet(0x80, 6, 0x0100, 0, 8);
        let Some(data) = self.control_xfer(0, 8, setup, None, 8) else {
            return false;
        };
        if data.len() < 8 {
            return false;
        }
        let maxpacket0 = data[7].clamp(8, 64);

        if self.next_addr >= 127 {
            return false;
        }
        let addr = self.next_addr;
        self.next_addr += 1;

        let setup = setup_packet(0x00, 5, addr as u16, 0, 0);
        if self.control_xfer(0, maxpacket0, setup, None, 0).is_none() {
            return false;
        }
        delay_ms(10);

        let setup = setup_packet(0x80, 6, 0x0100, 0, 18);
        let Some(full) = self.control_xfer(addr, maxpacket0, setup, None, 18) else {
            return false;
        };
        if full.len() < 18 {
            return false;
        }
        let dev_class = full[4];
        let vid = u16::from_le_bytes([full[8], full[9]]);
        let pid = u16::from_le_bytes([full[10], full[11]]);

        let setup = setup_packet(0x80, 6, 0x0200, 0, 9);
        let Some(cfg9) = self.control_xfer(addr, maxpacket0, setup, None, 9) else {
            return false;
        };
        if cfg9.len() < 9 {
            return false;
        }
        let total = (cfg9[2] as usize) | ((cfg9[3] as usize) << 8);
        if !(9..=CTRL_DATA_MAX).contains(&total) {
            return false;
        }
        let setup = setup_packet(0x80, 6, 0x0200, 0, total as u16);
        let Some(cfg) = self.control_xfer(addr, maxpacket0, setup, None, total) else {
            return false;
        };

        if dev_class == 9 {
            crate::println!(
                "[usb] OHCI port{} hub {:04x}:{:04x}: not traversed",
                port, vid, pid
            );
            return false;
        }

        if let Some((iface, ep, max)) = find_hid_keyboard(&cfg) {
            if !self.configure(addr, maxpacket0, &cfg, iface) {
                return false;
            }
            if self.arm_stream(addr, ep, max, &cfg, StreamKind::Keyboard, None, 8, low_speed) {
                crate::println!(
                    "[usb] OHCI {:04x}:{:04x} HID keyboard addr={} ep={:#x} max={}{} (port{})",
                    vid,
                    pid,
                    addr,
                    ep,
                    max,
                    if low_speed { " low-speed" } else { "" },
                    port
                );
                return true;
            }
            return false;
        }

        let Some((iface, ep, max, kind)) = find_hid_pointer(&cfg) else {
            crate::println!(
                "[usb] OHCI port{} {:04x}:{:04x} no HID keyboard/pointer interface",
                port, vid, pid
            );
            return false;
        };

        let mut layout: Option<TabletLayout> = None;
        let mut report_len: u16 = 4;
        let mut label = "mouse";
        if kind == HidPointerKind::Tablet {
            label = "tablet";
            report_len = 5;
            let setup = setup_packet(0x81, 6, 0x2200, iface as u16, 255);
            if let Some(desc) = self.control_xfer(addr, maxpacket0, setup, None, 255) {
                match parse_tablet_layout(&desc) {
                    Some(l) => {
                        report_len = l.report_len as u16;
                        layout = Some(l);
                    }
                    None => {
                        let fb = TabletLayout::qm_fallback();
                        report_len = fb.report_len as u16;
                        layout = Some(fb);
                    }
                }
            }
        }

        if !self.configure(addr, maxpacket0, &cfg, iface) {
            return false;
        }
        if self.arm_stream(
            addr,
            ep,
            max,
            &cfg,
            pointer_kind(kind),
            layout,
            report_len,
            low_speed,
        ) {
            crate::println!(
                "[usb] OHCI {:04x}:{:04x} HID {} addr={} ep={:#x} max={} len={}{} (port{})",
                vid,
                pid,
                label,
                addr,
                ep,
                max,
                report_len,
                if low_speed { " low-speed" } else { "" },
                port
            );
            return true;
        }
        false
    }

    fn configure(&self, addr: u8, maxpacket0: u8, cfg: &[u8], iface: u8) -> bool {
        let cfg_value = cfg.get(5).copied().unwrap_or(1);
        let setup = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
        if self.control_xfer(addr, maxpacket0, setup, None, 0).is_none() {
            crate::println!("[usb] OHCI addr={} SET_CONFIGURATION failed", addr);
            return false;
        }
        // Boot protocol and report-on-change are advisory.
        let proto = setup_packet(0x21, 0x0B, 0, iface as u16, 0);
        let _ = self.control_xfer(addr, maxpacket0, proto, None, 0);
        let idle = setup_packet(0x21, 0x0A, 0, iface as u16, 0);
        let _ = self.control_xfer(addr, maxpacket0, idle, None, 0);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn arm_stream(
        &mut self,
        addr: u8,
        ep: u8,
        max: u16,
        cfg: &[u8],
        kind: StreamKind,
        layout: Option<TabletLayout>,
        report_len: u16,
        low_speed: bool,
    ) -> bool {
        if self.keyboards.len() + self.mice.len() >= MAX_HID_STREAMS {
            return false;
        }
        let mxp = max.clamp(1, 64) as u32;
        let report_len = (report_len as usize).clamp(1, STREAM_BUF_MAX);

        // HCCA has 32 periodic slots. bInterval is 2^(n-1) ms; clamp to a
        // 2..16 ms window (bInterval=10 would otherwise be 512 ms).
        let bint = hid_binterval(cfg, ep);
        let frames = 1usize << (bint.saturating_sub(1)).min(10);
        let interval = frames.clamp(2, 16).min(HCCA_INTS);

        let (page_virt, page_phys, page_ptr) = match dma_page() {
            Some(p) => p,
            None => return false,
        };
        unsafe { core::ptr::write_bytes(page_ptr, 0, 256) };

        let ed_phys = (page_phys as u32) + STREAM_ED_OFF as u32;
        let td_phys = (page_phys as u32) + STREAM_TD_OFF as u32;
        let dummy_phys = (page_phys as u32) + STREAM_TD_DUMMY as u32;
        let buf_phys = (page_phys as u32) + STREAM_BUF_OFF as u32;

        unsafe {
            // Dummy terminator: self-pointing, never executed.
            let dummy = (page_virt + STREAM_TD_DUMMY as u64) as *mut Td;
            (*dummy).hwinfo = TD_NOTACCESSED;
            (*dummy).cbp = 0;
            (*dummy).be = 0;
            (*dummy).next = dummy_phys;

            // Interrupt ED: function address, endpoint number, IN direction,
            // max packet size.
            let ed = page_virt as *mut Ed;
            (*ed).hwinfo = ((addr as u32) << ED_FA_SHIFT) & ED_FA_MASK
                | (((ep & 0x0F) as u32) << ED_EN_SHIFT) & ED_EN_MASK
                | ED_IN
                | if low_speed { ED_LOWSPEED } else { 0 }
                | (mxp << ED_MPS_SHIFT) & ED_MPS_MASK;
            (*ed).tail = dummy_phys;
            (*ed).head = dummy_phys;
            (*ed).next = 0;
        }

        let stream = HidStream {
            page_virt,
            ed_phys,
            td_phys,
            kind,
            layout,
        };

        // Arm the first transfer.
        Self::arm_one(page_virt, td_phys, buf_phys, report_len);

        // Link into every periodic slot for this interval. The HCCA write is
        // last: the controller walks int_table[i] -> ED.hwNextED -> ...
        unsafe {
            for i in 0..interval {
                core::ptr::write_volatile(self.hcca_int(i), ed_phys);
            }
        }
        let ctl = self.r32(R_HC_CONTROL);
        if ctl & HCCTRL_PLE == 0 {
            self.w32(R_HC_CONTROL, ctl | HCCTRL_PLE);
        }
        if ctl & HCCTRL_IE == 0 {
            self.w32(R_HC_CONTROL, self.r32(R_HC_CONTROL) | HCCTRL_IE);
        }

        match kind {
            StreamKind::Keyboard => self.keyboards.push(stream),
            _ => self.mice.push(stream),
        }
        true
    }

    /// Fills the single TD and publishes it by advancing `hwHeadP` past the
    /// dummy tail. The controller retires it and moves head back to the
    /// dummy, which is why re-arming means writing head again.
    fn arm_one(page_virt: u64, td_phys: u32, buf_phys: u32, report_len: usize) {
        unsafe {
            let td = ((page_virt + STREAM_TD_OFF as u64) as *mut Td);
            (*td).hwinfo = TD_NOTACCESSED | TD_EC | TD_T_TOGGLE | TD_R | TD_DI | TD_DP_IN;
            (*td).cbp = buf_phys;
            (*td).be = buf_phys + report_len as u32 - 1;
            (*td).next = (page_virt as u32) + STREAM_TD_DUMMY as u32;

            // EDs are 32-byte aligned, so this is a clean publish.
            let ed = page_virt as *mut Ed;
            core::ptr::write_volatile(&mut (*ed).head, td_phys);
        }
    }

    // ---- polling ---------------------------------------------------------

    fn poll_streams(&mut self, is_kbd: bool) {
        let mut pending: Vec<(StreamKind, Option<TabletLayout>, [u8; STREAM_BUF_MAX], usize)> =
            Vec::new();

        let streams = if is_kbd {
            &mut self.keyboards
        } else {
            &mut self.mice
        };
        for s in streams.iter_mut() {
            let td = s.td();
            let info = unsafe { core::ptr::read_volatile(&(*td).hwinfo) };
            if info & TD_CC_MASK == TD_NOTACCESSED {
                continue; // not retired yet
            }
            let cc = info & TD_CC_MASK;
            if cc >> TD_CC_SHIFT == CC_NOERROR {
                let cbp = unsafe { core::ptr::read_volatile(&(*td).cbp) };
                let mut n = if cbp == 0 {
                    STREAM_BUF_MAX
                } else {
                    (cbp & 0xFFF) as usize
                };
                if n == 0 || n > STREAM_BUF_MAX {
                    n = STREAM_BUF_MAX;
                }
                let mut data = [0u8; STREAM_BUF_MAX];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        s.buf(),
                        data.as_mut_ptr(),
                        STREAM_BUF_MAX,
                    );
                }
                pending.push((s.kind, s.layout, data, n));
            } else {
                crate::serial_println!(
                    "[usb] OHCI HID transfer cc={:#x}, re-arming",
                    cc >> TD_CC_SHIFT
                );
            }
            // Re-arm for the next poll.
            let buf_phys = (s.page_virt as u32) + STREAM_BUF_OFF as u32;
            Self::arm_one(s.page_virt, s.td_phys, buf_phys, STREAM_BUF_MAX);
        }

        // Consume done-head writeback periodically so the retired-TD chain
        // the controller builds does not grow without bound.
        self.w32(R_HC_DONE_HEAD, 0);

        for (kind, layout, data, len) in pending {
            dispatch_report(kind, layout, &data[..len]);
        }
    }

    pub fn poll(&mut self) {
        self.poll_streams(true);
        self.poll_streams(false);
    }

    pub fn hid_keyboard_count(&self) -> usize {
        self.keyboards.len()
    }

    pub fn hid_pointer_count(&self) -> usize {
        self.mice.len()
    }
}

/// Maps a descriptor-detected pointer type onto the report format the poll
/// loop decodes.
fn pointer_kind(kind: HidPointerKind) -> StreamKind {
    match kind {
        HidPointerKind::BootMouse => StreamKind::BootMouse,
        HidPointerKind::Tablet => StreamKind::Tablet,
    }
}
