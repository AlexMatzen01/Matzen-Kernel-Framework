//! UHCI (USB 1.1) host-controller driver — bring-up, control transfers,
//! enumeration and HID boot device claim.
//!
//! Polling-only, matching the EHCI and xHCI drivers: no IRQ, no MSI, no
//! interrupt-driven completion. Transfers complete by polling TD status.
//!
//! UHCI decodes an I/O BAR (unlike EHCI/xHCI which are MMIO) and is the
//! full/low-speed companion of an EHCI controller on real chipsets. The
//! schedule is a 1024-entry frame list of TD pointers; the async schedule
//! lives in entry 1023 and is selected by SOFMOD bit 2.
//!
//! The frame-list stride for interrupt endpoints is clamped below the
//! bInterval-derived period (see `arm_stream_with`). bInterval is a maximum
//! latency, not a floor: a device must tolerate being polled more often, and
//! unhindered full-speed HID keyboards commonly advertise bInterval=10,
//! which unclamped means a 512 ms poll.

use alloc::vec::Vec;

use crate::drivers::usb::{
    delay_ms, dispatch_report, dma_page, find_hid_keyboard, find_hid_pointer, hid_binterval, io_r16,
    io_w16, io_w32, parse_tablet_layout, setup_packet, HidPointerKind, StreamKind, TabletLayout,
};

// ---------------------------------------------------------------------------
// I/O register offsets (UHCI 1.1 §3)
// ---------------------------------------------------------------------------

const R_USBCMD: u16 = 0x00;
const R_USBSTS: u16 = 0x02;
const R_USBINTR: u16 = 0x04;
const R_FRNUM: u16 = 0x06;
const R_FRBASEADD: u16 = 0x08;
const R_SOFMOD: u16 = 0x0C;
const R_PORTSC1: u16 = 0x10;
const R_PORTSC2: u16 = 0x12;
const R_FLBASEADD: u16 = 0x14;

// USBCMD
const CMD_HCRESET: u16 = 1 << 0;
const CMD_HCRS: u16 = 1 << 1;
const CMD_RUN_STOP: u16 = 1 << 4;
const CMD_ASYNCH: u16 = 1 << 6;
const CMD_PERIODIC: u16 = 1 << 7;
const CMD_HCHALT: u16 = 1 << 9;
const CMD_ASYNCH_INTMASK: u16 = 1 << 15;
const CMD_MAXPKT0_SHIFT: u16 = 8;
const CMD_MAXPKT0_MASK: u16 = 0xFF << 8;

// USBSTS
const STS_HCHALTED: u16 = 1 << 0;
#[allow(dead_code)]
const STS_HCINT: u16 = 1 << 1;
#[allow(dead_code)]
const STS_HCINTDMA: u16 = 1 << 2;

// PORTSC
const PORTSC_CCS: u16 = 1 << 0;
const PORTSC_CC: u16 = 1 << 1;
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_PEDC: u16 = 1 << 3;
const PORTSC_PR: u16 = 1 << 8;
const PORTSC_PRC: u16 = 1 << 9;
const PORTSC_PP: u16 = 1 << 12;
/// 1 = full speed (12 Mb/s), 0 = low speed (1.5 Mb/s).
const PORTSC_PS: u16 = 1 << 15;

// SOFMOD
const SOFMOD_1MS: u16 = 0b100;
const SOFMOD_ASYNC: u16 = 0b100_0000;

// Frame list
const FRAMES: usize = 1024;
const ASYNC_SLOT: usize = FRAMES - 1;

// TDCTRLSTS
const TD_ACTIVE: u32 = 1 << 0;
const TD_STP: u32 = 1 << 1;
const TD_SPD_FULL: u32 = 0 << 2;
const TD_SPD_LOW: u32 = 1 << 2;
const TD_IOC: u32 = 1 << 4;
const TD_TERR_MAX: u32 = 3 << 16;
const TD_TERR_SHIFT: u32 = 16;
const TD_LEN_SHIFT: u32 = 21;
const TD_LEN_MAX: u32 = 0x7FF << TD_LEN_SHIFT;
const TD_LEN_MASK: u32 = 0x7FF << TD_LEN_SHIFT;

// TD token
const TOK_PID_SHIFT: u32 = 0;
const TOK_ADDR_SHIFT: u32 = 8;
const TOK_EP_SHIFT: u32 = 15;
const TOK_TOGGLE: u32 = 1 << 19;
const TOK_MAXLEN_SHIFT: u32 = 21;
const TOK_MAXLEN_MASK: u32 = 0x7FF;

const PID_SETUP: u32 = 0x2D;
const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xE1;

/// A SETUP stage is 8 bytes; UHCI expects MaxLen 11 for the SETUP stage
/// specifically (same convention as the Linux uhci-hcd driver).
const SETUP_MAXLEN: u32 = 11;

// ---------------------------------------------------------------------------
// DMA structures
// ---------------------------------------------------------------------------

#[repr(C, align(16))]
struct Td {
    link: u32,
    ctrl: u32,
    token: u32,
    buf: [u32; 4],
}

const TD_BYTES: usize = 32;

/// Layout of the shared control-transfer scratch page.
const CTRL_TD_BYTES: usize = 256;
const CTRL_SETUP_OFF: usize = 256;
const CTRL_DATA_OFF: usize = 272;
const CTRL_DATA_MAX: usize = 256;
const CTRL_STATUS_OFF: usize = 528;
const CTRL_STATUS_MAX: usize = 16;

/// Layout of a per-interrupt-endpoint page: TD array, then status buffers.
const STREAM_TD_OFF: usize = 512;
const STREAM_BUF_STRIDE: usize = 64;
const STREAM_MAX_SLOTS: usize = 16;
const STREAM_BUF_OFF: usize = STREAM_TD_OFF;

/// Bound on claimed HID endpoints per controller so a pathological hub chain
/// cannot leak unbounded DMA pages.
const MAX_HID_STREAMS: usize = 8;

/// The standard 32-byte UHCI register file exposes exactly two root ports
/// (PORTSC1/PORTSC2 at +0x10/+0x12). PIIX3, PIIX4 and VT82C686 all match.
const MAX_PORTS: u8 = 2;

// ---------------------------------------------------------------------------
// Claimed HID endpoint
// ---------------------------------------------------------------------------

/// One claimed HID interrupt endpoint. Several identical TDs are spread
/// across frame-list slots so the effective poll period is the stride rather
/// than the 1024 ms frame-list wrap.
struct HidStream {
    addr: u8,
    page_virt: u64,
    page_phys: u64,
    slots: u8,
    /// ctrl word with the Active bit clear, used to re-arm.
    template_ctrl: u32,
    kind: StreamKind,
    layout: Option<TabletLayout>,
}

impl HidStream {
    fn td(&self, slot: usize) -> *mut Td {
        unsafe { (self.page_virt as *mut Td).add(slot) }
    }

    fn buf(&self, slot: usize) -> *mut u8 {
        unsafe {
            ((self.page_virt + STREAM_BUF_OFF as u64) as *mut u8).add(slot * STREAM_BUF_STRIDE)
        }
    }

    fn phys(&self, slot: usize) -> u32 {
        (self.page_phys as u32) + (slot * TD_BYTES) as u32
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

pub struct UhciController {
    pci_bus: u8,
    pci_dev: u8,
    pci_func: u8,
    io: u16,
    n_ports: u8,

    /// Frame list: exactly one 4 KiB page = 1024 dwords.
    fl_virt: u64,
    fl_phys: u64,

    /// Control-transfer scratch, reused by every control transfer.
    ctrl_virt: u64,
    ctrl_phys: u64,

    next_addr: u8,
    /// Root ports already enumerated, so a rescan is idempotent.
    seen: u8,
    keyboards: Vec<HidStream>,
    mice: Vec<HidStream>,
}

// SAFETY: owns leaked DMA pages and raw pointers into them. All access is
// serialized by the `UsbState` mutex in `usb::poll` / `usb::init`.
unsafe impl Send for UhciController {}

impl UhciController {
    // ---- register access -------------------------------------------------

    #[inline]
    fn r16(&self, off: u16) -> u16 {
        unsafe { io_r16(self.io + off) }
    }

    #[inline]
    fn w16(&self, off: u16, val: u16) {
        unsafe { io_w16(self.io + off, val) }
    }

    #[inline]
    fn w32(&self, off: u16, val: u32) {
        unsafe { io_w32(self.io + off, val) }
    }

    #[inline]
    fn portsc(&self, port: u8) -> u16 {
        self.r16(R_PORTSC1 + (port as u16) * 2)
    }

    #[inline]
    fn set_portsc(&self, port: u8, val: u16) {
        self.w16(R_PORTSC1 + (port as u16) * 2, val);
    }

    #[inline]
    fn set_fl(&self, index: usize, val: u32) {
        unsafe { core::ptr::write_volatile((self.fl_virt as *mut u32).add(index), val) }
    }

    // ---- bring-up --------------------------------------------------------

    pub fn new(
        pci: crate::drivers::pci::PciDevice,
        _phys_mem_offset: u64,
    ) -> Result<Self, &'static str> {
        pci.enable_bus_mastering();

        // Firmware assigns the I/O BAR. A hot-added or firmware-less
        // controller leaves it zero; refusing is the honest outcome, since
        // guessing a base would collide with other port ranges.
        let io = pci
            .bar_io_base(0)
            .ok_or("UHCI I/O BAR0 is not a firmware-assigned I/O window")?;

        let mut ctl = Self {
            pci_bus: pci.bus,
            pci_dev: pci.device,
            pci_func: pci.function,
            io,
            n_ports: MAX_PORTS,
            fl_virt: 0,
            fl_phys: 0,
            ctrl_virt: 0,
            ctrl_phys: 0,
            next_addr: 1,
            seen: 0,
            keyboards: Vec::new(),
            mice: Vec::new(),
        };

        crate::println!(
            "[usb] UHCI {:02x}:{:02x}.{} {:04x}:{:04x} io={:#06x}",
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
        // Stop the controller and acknowledge any pending status first, so
        // the global reset starts from a quiescent state.
        self.w16(R_USBCMD, CMD_HCHALT);
        self.w16(R_USBSTS, 0xFFFF);
        self.w16(R_USBCMD, 0);
        self.w16(R_USBSTS, 0xFFFF);

        self.w16(R_USBCMD, CMD_HCRESET);
        for _ in 0..2000 {
            // Reading USBSTS clears the interrupt status bits.
            let sts = self.r16(R_USBSTS);
            if sts & STS_HCHALTED != 0 && self.r16(R_USBCMD) & CMD_HCRS == 0 {
                self.w16(R_USBSTS, 0xFFFF);
                crate::println!(
                    "[usb] UHCI {:02x}:{:02x}.{} reset ok",
                    self.pci_bus, self.pci_dev, self.pci_func
                );
                return Ok(());
            }
            delay_ms(1);
        }
        Err("UHCI global reset timeout")
    }

    fn allocate(&mut self) -> Result<(), &'static str> {
        let (fl_virt, fl_phys, fl_ptr) = dma_page().ok_or("UHCI frame list alloc")?;
        unsafe { core::ptr::write_bytes(fl_ptr, 0, 4096) };
        self.fl_virt = fl_virt;
        self.fl_phys = fl_phys;

        let (ctrl_virt, ctrl_phys, ctrl_ptr) = dma_page().ok_or("UHCI ctrl scratch alloc")?;
        unsafe { core::ptr::write_bytes(ctrl_ptr, 0, CTRL_STATUS_OFF + CTRL_STATUS_MAX) };
        self.ctrl_virt = ctrl_virt;
        self.ctrl_phys = ctrl_phys;
        Ok(())
    }

    fn program(&mut self) -> Result<(), &'static str> {
        if self.fl_phys & 0xFFF != 0 {
            return Err("UHCI frame list not 4 KiB aligned");
        }
        // FLBASEADD is 32-byte aligned; FRBASEADD is 4 KiB aligned.
        self.w32(R_FLBASEADD, (self.fl_phys & !0x1F) as u32);
        self.w32(R_FRBASEADD, (self.fl_phys & !0xFFF) as u32);

        // MaxPkt0 = 64 bytes, the power-on default, stated explicitly.
        let cmd = self.r16(R_USBCMD);
        self.w16(R_USBCMD, (cmd & !CMD_MAXPKT0_MASK) | (64 << CMD_MAXPKT0_SHIFT));
        self.w16(R_USBINTR, 0);
        self.w16(R_USBCMD, self.r16(R_USBCMD) | CMD_ASYNCH_INTMASK);
        self.w16(R_SOFMOD, SOFMOD_1MS);
        Ok(())
    }

    // ---- control transfers ----------------------------------------------

    /// Runs one control transfer on endpoint 0. Direction comes from the
    /// setup packet's bmRequestType bit 7. Returns the data-in bytes, or
    /// `None` on timeout or error.
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
            crate::serial_println!("[usb] UHCI control data {} exceeds buffer", data_len);
            return None;
        }
        let mxp = maxpacket.clamp(8, 64) as u32;

        const TD_SETUP: usize = 0;
        const TD_DATA: usize = 1;
        const TD_STATUS: usize = 2;
        const TD_TERM: usize = 3;

        unsafe {
            let base = self.ctrl_virt as *mut u8;
            let td = |i: usize| -> *mut Td { (self.ctrl_virt as *mut Td).add(i) };

            core::ptr::write_bytes(
                self.ctrl_virt as *mut u8,
                0,
                CTRL_STATUS_OFF + CTRL_STATUS_MAX,
            );
            core::ptr::copy_nonoverlapping(setup.as_ptr(), base.add(CTRL_SETUP_OFF), 8);
            if is_in {
                core::ptr::write_bytes(base.add(CTRL_DATA_OFF), 0, data_len);
            } else if let Some(d) = data_out {
                core::ptr::copy_nonoverlapping(d.as_ptr(), base.add(CTRL_DATA_OFF), data_len);
            }

            let term_phys = (self.ctrl_phys as u32) + (TD_TERM * TD_BYTES) as u32;
            let ctrl = TD_TERR_MAX | TD_SPD_FULL | TD_STP;
            let data_maxlen = if data_len == 0 {
                0
            } else {
                (data_len as u32).div_ceil(mxp).clamp(1, TOK_MAXLEN_MASK)
            };
            let data_pid = if is_in { PID_IN } else { PID_OUT };

            let t = td(TD_SETUP);
            (*t).link = term_phys;
            (*t).ctrl = ctrl | TD_ACTIVE | TD_LEN_MAX;
            (*t).token = (SETUP_MAXLEN << TOK_MAXLEN_SHIFT)
                | ((addr as u32) << TOK_ADDR_SHIFT)
                | (PID_SETUP << TOK_PID_SHIFT);
            (*t).buf = [(self.ctrl_phys as u32) + CTRL_SETUP_OFF as u32, 0, 0, 0];

            let t = td(TD_DATA);
            (*t).link = term_phys;
            (*t).ctrl = ctrl | TD_ACTIVE | TD_LEN_MAX;
            (*t).token = (data_maxlen << TOK_MAXLEN_SHIFT)
                | TOK_TOGGLE
                | ((addr as u32) << TOK_ADDR_SHIFT)
                | (data_pid << TOK_PID_SHIFT);
            (*t).buf = [(self.ctrl_phys as u32) + CTRL_DATA_OFF as u32, 0, 0, 0];

            // Zero-length status stage, opposite direction to the data stage.
            let t = td(TD_STATUS);
            (*t).link = term_phys;
            (*t).ctrl = ctrl | TD_ACTIVE | TD_LEN_MAX;
            (*t).token = (1 << TOK_MAXLEN_SHIFT)
                | TOK_TOGGLE
                | ((addr as u32) << TOK_ADDR_SHIFT)
                | ((if is_in { PID_OUT } else { PID_IN }) << TOK_PID_SHIFT);
            (*t).buf = [
                (self.ctrl_phys as u32) + CTRL_STATUS_OFF as u32,
                0,
                0,
                0,
            ];

            // Terminate the list. Link bit 0 set marks "last entry"; the TD is
            // never executed because Active is clear.
            let t = td(TD_TERM);
            (*t).link = 1;
            (*t).ctrl = TD_LEN_MAX;
            (*t).token = 0;
            (*t).buf = [0; 4];
        }

        // Publish the chain in the async slot and switch SOFMOD to it.
        self.set_fl(ASYNC_SLOT, self.ctrl_phys as u32);
        self.w16(R_SOFMOD, SOFMOD_ASYNC);
        self.w16(
            R_USBCMD,
            self.r16(R_USBCMD) | CMD_RUN_STOP | CMD_ASYNCH,
        );

        let status_td = unsafe { (self.ctrl_virt as *mut Td).add(TD_STATUS) };
        let mut done = false;
        for _ in 0..20_000 {
            let ctrl = unsafe { core::ptr::read_volatile(&(*status_td).ctrl) };
            if ctrl & TD_ACTIVE == 0 {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Restore the periodic schedule regardless of outcome.
        self.set_fl(ASYNC_SLOT, 0);
        self.w16(R_SOFMOD, SOFMOD_1MS);

        if !done {
            crate::serial_println!("[usb] UHCI control timeout addr={}", addr);
            return None;
        }

        let status_ctrl = unsafe { core::ptr::read_volatile(&(*status_td).ctrl) };
        if status_ctrl & TD_ACTIVE == 0 && (status_ctrl >> TD_TERR_SHIFT) & 0x7 != 0 {
            crate::serial_println!(
                "[usb] UHCI control error addr={} terr={}",
                addr,
                (status_ctrl >> TD_TERR_SHIFT) & 0x7
            );
            return None;
        }

        if !is_in || data_len == 0 {
            return Some(Vec::new());
        }

        // The DATA TD's TdLEN is the transferred count. A short packet must
        // not cause an over-read of the destination.
        let data_td = unsafe { (self.ctrl_virt as *mut Td).add(TD_DATA) };
        let n = {
            let ctrl = unsafe { core::ptr::read_volatile(&(*data_td).ctrl) };
            let tdl = ((ctrl & TD_LEN_MASK) >> TD_LEN_SHIFT) as usize;
            if tdl == 0 || tdl > data_len {
                data_len
            } else {
                tdl
            }
        };
        let mut out = Vec::with_capacity(n);
        unsafe {
            let src = (self.ctrl_virt + CTRL_DATA_OFF as u64) as *const u8;
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

    /// True when a port's speed field reads low speed (1.5 Mb/s).
    fn port_low_speed(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_PS == 0
    }

    fn reset_port(&self, port: u8) -> bool {
        for _ in 0..2 {
            let v = self.portsc(port);
            if v & PORTSC_CCS == 0 {
                return false;
            }
            // Power the port and clear the W1C change bits, with enable off.
            let v = (v | PORTSC_PP | PORTSC_CC | PORTSC_PEDC | PORTSC_PRC) & !PORTSC_PED;
            self.set_portsc(port, v);
            delay_ms(20);

            // Assert reset; the controller clears it in hardware.
            self.set_portsc(port, self.portsc(port) | PORTSC_PR);
            delay_ms(50);
            let mut released = false;
            for _ in 0..200 {
                if self.portsc(port) & PORTSC_PR == 0 {
                    released = true;
                    break;
                }
                delay_ms(1);
            }
            if !released {
                continue;
            }
            // Acknowledge PRC and enable the port.
            let v = (self.portsc(port) | PORTSC_PRC | PORTSC_PED) & !PORTSC_PEDC;
            self.set_portsc(port, v);
            if self.portsc(port) & PORTSC_CCS != 0 {
                return true;
            }
        }
        false
    }

    fn enumerate_device(&mut self, port: u8) -> bool {
        let low_speed = self.port_low_speed(port);

        // 8-byte device-descriptor probe at address 0.
        let setup = setup_packet(0x80, 6, 0x0100, 0, 8);
        let Some(data) = self.control_xfer(0, 8, setup, None, 8) else {
            return false;
        };
        if data.len() < 8 {
            return false;
        }
        let maxpacket0 = data[7].clamp(8, 64);

        if self.next_addr >= 127 {
            crate::println!("[usb] UHCI address space exhausted");
            return false;
        }
        let addr = self.next_addr;
        self.next_addr += 1;

        // SET_ADDRESS has no data stage.
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

        // Config descriptor: 9-byte header to learn the total length.
        let setup = setup_packet(0x80, 6, 0x0200, 0, 9);
        let Some(cfg9) = self.control_xfer(addr, maxpacket0, setup, None, 9) else {
            return false;
        };
        if cfg9.len() < 9 {
            return false;
        }
        let total = (cfg9[2] as usize) | ((cfg9[3] as usize) << 8);
        if !(9..=CTRL_DATA_MAX).contains(&total) {
            crate::println!(
                "[usb] UHCI port{} {:04x}:{:04x} config total={} out of range",
                port, vid, pid, total
            );
            return false;
        }
        let setup = setup_packet(0x80, 6, 0x0200, 0, total as u16);
        let Some(cfg) = self.control_xfer(addr, maxpacket0, setup, None, total) else {
            return false;
        };

        if dev_class == 9 {
            // A hub needs its own ED list to forward traffic; this driver has
            // no hub support, so devices behind one are not reachable.
            crate::println!(
                "[usb] UHCI port{} hub {:04x}:{:04x}: not traversed",
                port, vid, pid
            );
            return false;
        }

        // Keyboard takes precedence, matching the EHCI claim order.
        if let Some((iface, ep, max)) = find_hid_keyboard(&cfg) {
            if !self.configure(addr, maxpacket0, &cfg, iface) {
                return false;
            }
            if self.arm_stream(port, addr, ep, max, &cfg, StreamKind::Keyboard, None, 8, low_speed)
            {
                crate::println!(
                    "[usb] UHCI {:04x}:{:04x} HID keyboard addr={} ep={:#x} max={}{} (port{})",
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
                "[usb] UHCI port{} {:04x}:{:04x} no HID keyboard/pointer interface",
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
                        crate::println!(
                            "[usb] UHCI {:04x}:{:04x} tablet desc unparsed, QEMU fallback (X/Y max {})",
                            vid, pid, fb.x_max
                        );
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
            port,
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
                "[usb] UHCI {:04x}:{:04x} HID {} addr={} ep={:#x} max={} len={}{} (port{})",
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

    /// SET_CONFIGURATION plus the best-effort HID boot-protocol requests.
    fn configure(&self, addr: u8, maxpacket0: u8, cfg: &[u8], iface: u8) -> bool {
        let cfg_value = cfg.get(5).copied().unwrap_or(1);
        let setup = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
        if self.control_xfer(addr, maxpacket0, setup, None, 0).is_none() {
            crate::println!("[usb] UHCI addr={} SET_CONFIGURATION failed", addr);
            return false;
        }
        // Boot protocol and report-only-on-change. Both are advisory: most
        // devices already default to a usable mode.
        let proto = setup_packet(0x21, 0x0B, 0, iface as u16, 0);
        if self.control_xfer(addr, maxpacket0, proto, None, 0).is_none() {
            crate::println!("[usb] UHCI addr={} HID SET_PROTOCOL failed, continuing", addr);
        }
        let idle = setup_packet(0x21, 0x0A, 0, iface as u16, 0);
        if self.control_xfer(addr, maxpacket0, idle, None, 0).is_none() {
            crate::println!("[usb] UHCI addr={} HID SET_IDLE failed, continuing", addr);
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn arm_stream(
        &mut self,
        port: u8,
        addr: u8,
        ep: u8,
        max: u16,
        cfg: &[u8],
        kind: StreamKind,
        layout: Option<TabletLayout>,
        report_len: u16,
        low_speed: bool,
    ) -> bool {
        let _ = port;
        if self.keyboards.len() + self.mice.len() >= MAX_HID_STREAMS {
            return false;
        }
        let mxp = max.clamp(1, 64) as u32;
        let report_len = (report_len as usize).clamp(1, STREAM_BUF_STRIDE);

        // bInterval is 2^(n-1) ms. Clamp to a 2..16 ms window: unhindered
        // bInterval=10 would otherwise mean a 512 ms poll.
        let bint = hid_binterval(cfg, ep);
        let frames = 1usize << (bint.saturating_sub(1)).min(10);
        let stride = frames.clamp(2, 16);
        let slots = (FRAMES / stride).clamp(1, STREAM_MAX_SLOTS);

        let need = STREAM_BUF_OFF + slots * STREAM_BUF_STRIDE;
        if need > 4096 {
            return false;
        }
        let (page_virt, page_phys, page_ptr) = match dma_page() {
            Some(p) => p,
            None => return false,
        };
        unsafe { core::ptr::write_bytes(page_ptr, 0, need) };

        let spd = if low_speed { TD_SPD_LOW } else { TD_SPD_FULL };
        let template_ctrl = TD_TERR_MAX | TD_IOC | spd | TD_LEN_MAX | TD_ACTIVE;
        let maxlen = (report_len as u32).div_ceil(mxp).clamp(1, TOK_MAXLEN_MASK);
        let token = (maxlen << TOK_MAXLEN_SHIFT)
            | (((ep & 0x0F) as u32) << TOK_EP_SHIFT)
            | TOK_TOGGLE
            | ((addr as u32) << TOK_ADDR_SHIFT)
            | (PID_IN << TOK_PID_SHIFT);

        let stream = HidStream {
            addr,
            page_virt,
            page_phys,
            slots: slots as u8,
            template_ctrl,
            kind,
            layout,
        };

        unsafe {
            for slot in 0..slots {
                let t = stream.td(slot);
                (*t).link = 1; // terminate: each TD stands alone in its slot
                (*t).ctrl = template_ctrl;
                (*t).token = token;
                (*t).buf = [
                    (page_phys as u32) + (STREAM_BUF_OFF + slot * STREAM_BUF_STRIDE) as u32,
                    0,
                    0,
                    0,
                ];
                self.set_fl(slot * stride, stream.phys(slot));
            }
        }
        self.w16(R_USBCMD, self.r16(R_USBCMD) | CMD_RUN_STOP | CMD_PERIODIC);

        match kind {
            StreamKind::Keyboard => self.keyboards.push(stream),
            _ => self.mice.push(stream),
        }
        true
    }

    // ---- polling ---------------------------------------------------------

    /// Drains completed interrupt TDs, then dispatches the reports. Reports
    /// are buffered so the per-stream borrow ends before dispatch.
    fn poll_streams(&mut self, is_kbd: bool) {
        let mut pending: Vec<(StreamKind, Option<TabletLayout>, [u8; STREAM_BUF_STRIDE], usize)> =
            Vec::new();

        let streams = if is_kbd {
            &mut self.keyboards
        } else {
            &mut self.mice
        };
        for s in streams.iter_mut() {
            for slot in 0..s.slots as usize {
                let t = s.td(slot);
                let ctrl = unsafe { core::ptr::read_volatile(&(*t).ctrl) };
                if ctrl & TD_ACTIVE != 0 {
                    continue; // still in flight
                }
                let got = ((ctrl & TD_LEN_MASK) >> TD_LEN_SHIFT) as usize;
                if got > 0 {
                    let mut data = [0u8; STREAM_BUF_STRIDE];
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            s.buf(slot),
                            data.as_mut_ptr(),
                            STREAM_BUF_STRIDE,
                        );
                    }
                    pending.push((s.kind, s.layout, data, got.min(STREAM_BUF_STRIDE)));
                }
                unsafe { core::ptr::write_volatile(&mut (*t).ctrl, s.template_ctrl) };
            }
        }

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