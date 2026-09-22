//! xHCI (USB 3.x) host-controller driver â€” Phase 1+2: bring-up + enumeration.
//!
//! Polling-only design matching the EHCI driver: no MSI-X/APIC work, the
//! shell loop calls `poll()` and we drain the event ring by hand. Phase 1
//! brings the controller from firmware state to running (BIOS handoff,
//! reset, scratchpad, DCBAA, command + event rings, Run) and proves the
//! rings end-to-end with a NO-OP command. Phase 2 enumerates root ports
//! (reset, Enable Slot, Address Device, descriptors, SET_CONFIGURATION)
//! including one hub level, stopping at configured devices. HID endpoint
//! claiming comes in Phase 3.
//!
//! Register/TRB layouts follow the xHCI spec and Linux `drivers/usb/host`
//! (`xhci.h`, `xhci-caps.h`, `xhci-ext-caps.h`); bit names mirror those
//! headers so the two can be diffed by eye.
//!
//! All waits are bounded so missing/broken hardware cannot hang boot.

use crate::drivers::usb::{
    delay_ms, dma_page, find_hid_keyboard, mmio_r16, mmio_r32, mmio_r64, mmio_r8, mmio_w32,
    mmio_w64, setup_packet,
};
use alloc::vec::Vec;
// ---------------------------------------------------------------------------
// Capability registers (offsets from MMIO base, xHCI 5.3)
// ---------------------------------------------------------------------------

const CAP_CAPLENGTH: usize = 0x00;
const CAP_HCIVERSION: usize = 0x02;
const CAP_HCSPARAMS1: usize = 0x04;
const CAP_HCSPARAMS2: usize = 0x08;
const CAP_HCCPARAMS1: usize = 0x10;
const CAP_DBOFF: usize = 0x14;
const CAP_RTSOFF: usize = 0x18;

// HCSPARAMS1 (5.3.3): slots [7:0], interrupters [18:8], ports [31:24].
// HCSPARAMS2 (5.3.4): scratchpad Hi [25:21], Lo [31:27].
// HCCPARAMS1 (5.3.6): AC64 bit0, CSZ bit2, xECP [31:16] (DWORDs from base).

// Operational registers (offsets from opbase = mmio + CAPLENGTH, xHCI 5.4).
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_PAGESIZE: usize = 0x08;
const OP_DNCTRL: usize = 0x14;
const OP_CRCR: usize = 0x18;
const OP_DCBAAP: usize = 0x30;
const OP_CONFIG: usize = 0x38;

// USBCMD bits.
const CMD_RS: u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
// USBSTS bits.
const STS_HCHALTED: u32 = 1 << 0;
const STS_CNR: u32 = 1 << 11;

// Runtime interrupter-0 registers (IR set 0 starts at runbase + 0x20, so
// `ir0` already includes that; xHCI 5.5.2). Offsets below are relative to
// the IR set start â€” NOT including the +0x20 (a double-add here once sent
// every interrupter access 0x20 too high and broke event delivery).
const IR_IMAN: usize = 0x00;
const IR_ERSTSZ: usize = 0x08;
const IR_ERSTBA: usize = 0x10;
const IR_ERDP: usize = 0x18;
// IMAN bits: IP (pending, W1C) bit0, IE (enable) bit1. We poll with IE=0.
const IMAN_IP: u32 = 1 << 0;
// ERDP: dequeue pointer [63:4], EHB bit3 (always written set, like Linux).
const ERDP_EHB: u64 = 1 << 3;

// Command ring: one 4K page = 128 TRBs; last slot is the Link TRB.
const RING_TRBS: usize = 128;
const LINK_IDX: usize = RING_TRBS - 1;
// Event ring: one 4K page = 128 TRBs, single segment (implicit wrap).
const TRB_SIZE: u64 = 16;

// TRB control bits (dword3): cycle bit0, type [15:10].
const TRB_CYCLE: u32 = 1 << 0;
const TRB_TYPE_SHIFT: u32 = 10;
// Link TRB (type 6): Toggle Cycle is dword3 bit1.
const TRB_LINK_TC: u32 = 1 << 1;
// TRB type IDs.
const TRB_LINK: u32 = 6;
const TRB_CMD_NOOP: u32 = 23;
const TRB_COMPLETION: u32 = 33;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDR_DEV: u32 = 11;
const TRB_CONFIG_EP: u32 = 12;
const TRB_EVAL_CTX: u32 = 13;
// Address Device BSR (Block Set Address Request), command dword3 bit 9.
// BSR=0: HC performs the SET_ADDRESS transaction. BSR=1: contexts only,
// no USB transaction (isolation probe for Code 4).
const TRB_BSR: u32 = 1 << 9;
// Photo-visible addressing diagnostics (pre-Address snapshot, failure
// detail, BSR=1 probe). Boot-quiet when false.
const XHCI_ADDR_DEBUG: bool = true;
// After a BSR=0 Code 4, re-issue Address Device with BSR=1 on the same
// slot + input context (bounded 500ms). Success => contexts valid, device
// stayed silent. Failure => input/slot context fault.
const XHCI_BSR_PROBE: bool = true;
// Transfer TRBs.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_TRANSFER: u32 = 32;
// Port Status Change event (xHCI 6.4.2.4): dword0[31:24] = root port id.
const TRB_PORT_STATUS_CHANGE: u32 = 34;
// Setup TRB: TRT (transfer type) in dword2? No â€” TRT is dword3 [17:16].
const TRT_IN: u32 = 3;
const TRT_OUT: u32 = 2;
const TRT_NONE: u32 = 0;
// Data/Status control bits (dword3): chain bit4, IOC bit5, IDT bit6, DIR bit16.
const TRB_CHAIN: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR: u32 = 1 << 16;
// Completion codes (event status [31:24]).
const COMP_SUCCESS: u32 = 1;
const COMP_SHORT_PACKET: u32 = 13;

// Root-hub PORTSC (op + 0x400 + (n-1)*0x10, n 1-based, xHCI 5.4.8).
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
// Connect Status Change (W1C): a hot-plug arrived or departed.
const PORT_CSC: u32 = 1 << 17;
// W1C change bits cleared on PORTSC writes (xHCI 5.4.8).
const PORT_CHANGE_BITS: u32 =
    (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);
// Port speeds (PORTSC [13:10], reused in slot contexts verbatim).
const SPEED_FS: u32 = 1;
const SPEED_LS: u32 = 2;
const SPEED_HS: u32 = 3;
const SPEED_SS: u32 = 4;

// Slot context (6.2.1.1): route [19:0], speed [23:20], LAST_CTX [31:27];
// dev_info2: root port [23:16]; tt_info: hub slot [7:0], hub port [15:8].
// (MTT/HUB bits intentionally left clear: HUB-bit handling is deferred to
// hub-path debugging if a hub's downstream splits ever fail.)
const SLOT_LAST_CTX_SHIFT: u32 = 27;
const SLOT_SPEED_SHIFT: u32 = 20;
// Input control context add flags.
const ADD_SLOT: u32 = 1 << 0;
const ADD_EP0: u32 = 1 << 1;
// Endpoint context: type CTRL=4 [5:3], error count [2:1], maxpacket [31:16].
const EP_TYPE_CTRL: u32 = 4;
const EP_TYPE_INT_IN: u32 = 7;
// Hub port status (USB 2.0 hub GET_STATUS): LS bit9, HS bit10.
const HUB_PS_LS: u16 = 1 << 9;
const HUB_PS_HS: u16 = 1 << 10;

// Extended capabilities (xHCI 7.x). xECP = HCCPARAMS1[31:16], byte offset
// from MMIO base is xECP << 2; each header: ID [7:0], next [15:8] (DWORDs).
const EXT_CAP_LEGACY: u32 = 1;
// USBLEGSUP (7.1.1): BIOS owned bit16, OS owned bit24 (dword at +0x0).
const LEG_BIOS_OWNED: u32 = 1 << 16;
const LEG_OS_OWNED: u32 = 1 << 24;
// LEGCTLSTS (+0x4): SMI enable bits cleared, status bits W1C (like Linux).
const LEG_DISABLE_SMI: u32 = (0x7 << 1) | (0xFF << 5) | (0x7 << 17);
const LEG_SMI_EVENTS: u32 = 0x7 << 29;

/// xHCI host controller (running; owns addressed/configured device slots).
/// Fields kept for later phases (HID claiming, rescan) are used then.
#[allow(dead_code)]
pub struct XhciController {
    pci_bus: u8,
    pci_dev: u8,
    pci_func: u8,
    max_slots: u32,
    max_ports: usize,
    ctx_is_64: bool,
    op: usize,
    db: usize,
    ir0: usize,
    // Command ring (producer).
    cmd_virt: u64,
    cmd_phys: u64,
    cmd_idx: usize,
    cmd_cycle: u32,
    // Event ring (consumer).
    evt_virt: u64,
    evt_phys: u64,
    evt_idx: usize,
    evt_cycle: u32,
    // TRB address of the last submitted command (for completion matching).
    last_cmd_trb: u64,
    // Device Context Base Address Array (for per-slot entries).
    dcbaa_virt: u64,
    // Addressed/configured devices (slots owned by this driver).
    slots: Vec<XhciSlot>,
}

/// One addressed xHCI device slot. EP0 ring + contexts live in leaked DMA
/// pages for driver lifetime. Interrupt endpoints arrive in Phase 3.
#[allow(dead_code)]
pub struct XhciSlot {
    pub id: u32,
    /// 1-based root-hub port number.
    pub port: usize,
    /// PORTSC speed encoding (1 FS, 2 LS, 3 HS, 4 SS).
    pub speed: u32,
    pub maxpacket0: u8,
    pub addr: u8,
    pub vid: u16,
    pub pid: u16,
    pub dev_class: u8,
    pub configured: bool,
    /// Full config descriptor (kept for Phase 3 HID claiming).
    pub cfg: Vec<u8>,
    ep0_virt: u64,
    ep0_phys: u64,
    ep0_idx: usize,
    ep0_cycle: u32,
    in_ctx_virt: u64,
    in_ctx_phys: u64,
    out_ctx_virt: u64,
    out_ctx_phys: u64,
    // ---- Phase 3 HID interrupt endpoint (claimed keyboards only) ----
    pub hid_claimed: bool,
    hid_ep: u8,
    hid_dci: u32,
    hid_maxpacket: u16,
    hid_interval: u8,
    int_virt: u64,
    int_phys: u64,
    int_idx: usize,
    int_cycle: u32,
    hid_buf_virt: u64,
    hid_buf_phys: u64,
    hid_pending_trb: u64,
}

unsafe impl Send for XhciController {}

/// Read one TRB (4 dwords, volatile).
unsafe fn trb_read(ring_virt: u64, idx: usize) -> [u32; 4] {
    let p = (ring_virt + (idx as u64) * TRB_SIZE) as *const u32;
    [
        core::ptr::read_volatile(p),
        core::ptr::read_volatile(p.add(1)),
        core::ptr::read_volatile(p.add(2)),
        core::ptr::read_volatile(p.add(3)),
    ]
}

/// Write one TRB (4 dwords, volatile).
unsafe fn trb_write(ring_virt: u64, idx: usize, t: [u32; 4]) {
    let p = (ring_virt + (idx as u64) * TRB_SIZE) as *mut u32;
    core::ptr::write_volatile(p, t[0]);
    core::ptr::write_volatile(p.add(1), t[1]);
    core::ptr::write_volatile(p.add(2), t[2]);
    core::ptr::write_volatile(p.add(3), t[3]);
}

impl XhciController {
    /// Take over the controller from firmware and bring it to running state.
    /// `phys_offset` is the bootloader physical-memory mapping offset.
    pub fn new(
        pci: crate::drivers::pci::PciDevice,
        phys_offset: u64,
    ) -> Result<Self, &'static str> {
        pci.enable_bus_mastering();
        // VGA photo proof of power state (Apple EFI boot, no serial).
        match pci.pm_info() {
            Some((cap, st)) => crate::println!(
                "[usb] xHCI {:02x}:{:02x}.{} PM cap {:#x} state D{}",
                pci.bus, pci.device, pci.function, cap, st
            ),
            None => crate::println!(
                "[usb] xHCI {:02x}:{:02x}.{} no PM cap",
                pci.bus, pci.device, pci.function
            ),
        }

        let mmio_phys = pci.mmio_base().ok_or("xHCI BAR not memory")?;
        if mmio_phys == 0 {
            return Err("xHCI BAR zero");
        }
// Fixed non-destructive mapping: xHCI MMIO is caps + op + runtime +
        // doorbells (typical DBOFF 0x2000 / RTSOFF 0x3000). Map 256 KB up
        // front instead of destructively sizing the 64-bit BAR with all-1s
        // writes while decode is enabled. Every failure returns Err so the
        // existing VGA "init failed" line fires instead of stalling silent.
        // Increase to 256KB to cover doorbells (DBOFF=0x3000) + runtime (RTSOFF=0x2000) + doorbells
        let base = crate::drivers::pci::map_mmio_region(
            mmio_phys,
            0x40000,  // 256 KB
            phys_offset,
            &mut crate::memory::frame_allocator::frame_allocator(),
        )
        .ok_or("xHCI MMIO map failed")?;

        // Ensure PCI COMMAND memory-space enable (bit 1) so MMIO reads work.
        // Firmware normally sets it; harmless if already set.
        let cmd = pci.read_config(0x04);
        if cmd & 0x02 == 0 {
            pci.write_config(0x04, cmd | 0x02);
            crate::serial_println!("[usb] xHCI {:02x}:{:02x}.{} COMMAND mem-enable set", pci.bus, pci.device, pci.function);
        }

        let caplen = unsafe { mmio_r8(base, CAP_CAPLENGTH) } as usize;
        // Real Panther Point 1e31 reports 0x80 (CAP0=0x01000080, HCIv1.0);
        // QEMU reports small. Accept up to 0x400, 4-byte aligned.
        if caplen == 0 || caplen == 0xFF || caplen > 0x400 || caplen & 3 != 0 {
            // VGA-visible diagnostics for real HW (MacBookAir5,2) photo debug.
            // 0/FF (or huge/misaligned) means BAR unmapped, MEM disabled,
            // D3-powered-down MMIO, or WB-aliased MMIO returning garbage.
            // (0x80 itself is legitimate on Panther Point, accepted above.)
            let cmd = pci.read_config(0x04);
            let b0 = pci.read_config(0x10);
            let b1 = pci.read_config(0x14);
            let pss = pci.read_config(0xD8);
            let c0 = unsafe { mmio_r32(base, 0x00) };
            let c1 = unsafe { mmio_r32(base, 0x04) };
            let c4 = unsafe { mmio_r32(base, 0x10) };
            crate::serial_println!(
                "[usb] xHCI {:02x}:{:02x}.{} BAD CAPLENGTH {:#x} BAR0={:#x} BAR1={:#x} CMD={:#x} PSSEN={:#x} phys={:#x} base={:#x} cap0={:#x} cap1={:#x} cap10={:#x}",
                pci.bus, pci.device, pci.function,
                caplen, b0, b1, cmd, pss, mmio_phys, base as u64, c0, c1, c4
            );
            crate::println!(
                "[usb] xHCI {:02x}:{:02x}.{} BAD CAPLENGTH {:#x} BAR1={:#x} PSSEN={:#x} cap0={:#x}",
                pci.bus, pci.device, pci.function, caplen, b1, pss, c0
            );
            return Err("xHCI bad CAPLENGTH");
        }
        // HCIVERSION lives at offset 0x02: halfword register, so a u32 read
        // would fault on alignment (base+2 is never 4-aligned).
        let version = unsafe { mmio_r16(base, CAP_HCIVERSION) };
        let hcs1 = unsafe { mmio_r32(base, CAP_HCSPARAMS1) };
        if hcs1 == 0 || hcs1 == 0xFFFF_FFFF {
            return Err("xHCI bad HCSPARAMS1");
        }
        let hcs2 = unsafe { mmio_r32(base, CAP_HCSPARAMS2) };
        let hcc1 = unsafe { mmio_r32(base, CAP_HCCPARAMS1) };
        let max_slots = hcs1 & 0xFF;
        let max_ports = ((hcs1 >> 24) & 0xFF) as usize;
        if max_slots == 0 || max_ports == 0 || max_ports > 127 {
            return Err("xHCI bad slots/ports");
        }
        let ac64 = hcc1 & 1 != 0;
        let ctx_is_64 = hcc1 & (1 << 2) != 0;
        let dboff = (unsafe { mmio_r32(base, CAP_DBOFF) } & !3) as usize;
        let rtsoff = (unsafe { mmio_r32(base, CAP_RTSOFF) } & !0x1F) as usize;
        // The fixed 128 KB window must cover the doorbell + runtime arrays.
        if dboff >= 0x20000 || rtsoff >= 0x20000 {
            return Err("xHCI offsets beyond mapped window");
        }
        crate::println!(
            "[usb] xHCI {:02x}:{:02x}.{} ver={:#x} slots={} ports={} ac64={} ctx64={} dboff={:#x} rtsoff={:#x}",
            pci.bus,
            pci.device,
            pci.function,
            version,
            max_slots,
            max_ports,
            ac64 as u8,
            ctx_is_64 as u8,
            dboff,
            rtsoff
        );

        let op = base + caplen;
        let db = base + dboff;
        let ir0 = base + rtsoff + 0x20;

        // VGA-visible bring-up markers: everything between the VER line
        // above and RUN/NOOP below was previously serial-only, so a stall
        // looked like a hang at VER on machines without serial capture.
        crate::println!("[usb] xHCI handoff...");
        // BIOS handoff via USBLEGSUP extended capability (xHCI 7.1).
        Self::bios_handoff(base, hcc1)?;
        crate::println!("[usb] xHCI handoff done, resetting...");

        // HC reset, then wait for reset-clear and Controller-Not-Ready clear.
        unsafe {
            let cmd = mmio_r32(op, OP_USBCMD);
            mmio_w32(op, OP_USBCMD, cmd | CMD_HCRST);
        }
        let mut ok = false;
        for _ in 0..1000 {
            if unsafe { mmio_r32(op, OP_USBCMD) } & CMD_HCRST == 0 {
                ok = true;
                break;
            }
            delay_ms(1);
        }
        if !ok {
            return Err("xHCI reset timeout");
        }
        crate::println!("[usb] xHCI reset done, waiting CNR...");
        ok = false;
        for _ in 0..1000 {
            if unsafe { mmio_r32(op, OP_USBSTS) } & STS_CNR == 0 {
                ok = true;
                break;
            }
            delay_ms(1);
        }
        if !ok {
            return Err("xHCI CNR timeout");
        }
        crate::println!("[usb] xHCI CNR clear, setting up rings...");

        // 4K pages are the minimum; our allocator only does 4K.
        if unsafe { mmio_r32(op, OP_PAGESIZE) } & 0x1 == 0 {
            return Err("xHCI no 4K pages");
        }

// MaxSlotsEn (CONFIG[7:0]); leave U3E/CIE zeroed.
        unsafe {
            mmio_w32(op, OP_CONFIG, max_slots);
        }
        // Verify MaxSlotsEn readback
        let config_readback = unsafe { mmio_r32(op, OP_CONFIG) };
        crate::serial_println!("[usb] xHCI MaxSlotsEn: wrote={} readback={}", max_slots, config_readback & 0xFF);
        crate::println!("[usb] xHCI MaxSlotsEn: wrote={} readback={}", max_slots, config_readback & 0xFF);

        // Scratchpad buffers if the controller demands any.
        let max_sp = (((hcs2 >> 21) & 0x1F) << 5) | ((hcs2 >> 27) & 0x1F);
        let scratch_array_phys = if max_sp > 0 {
            if (max_sp as usize) * 8 > 4096 {
                return Err("xHCI too many scratchpads");
            }
            let (arr_virt, arr_phys, _) = dma_page().ok_or("scratchpad array alloc")?;
            for i in 0..max_sp as usize {
                let (_, pg_phys, _) = dma_page().ok_or("scratchpad page alloc")?;
                unsafe {
                    core::ptr::write_volatile((arr_virt + (i as u64) * 8) as *mut u64, pg_phys);
                }
            }
            crate::serial_println!("[usb] xHCI {} scratchpad pages", max_sp);
            arr_phys
        } else {
            0
        };

        // DCBAA entries are always 64-bit pointers; CSZ only controls context
        // sizes and does not change DCBAA entry width.
        if ((max_slots as u64) + 1) * 8 > 4096 {
            return Err("xHCI DCBAA too large");
        }
        let (dcbaa_virt, dcbaa_phys, _) = dma_page().ok_or("DCBAA alloc")?;
unsafe {
            core::ptr::write_volatile(dcbaa_virt as *mut u64, scratch_array_phys);
            mmio_w64(op, OP_DCBAAP, dcbaa_phys);
        }
        // Verify DCBAAP readback
        let dcbaap_readback = unsafe { mmio_r64(op, OP_DCBAAP) };
        crate::serial_println!("[usb] xHCI DCBAAP: wrote={:#x} readback={:#x} match={}", dcbaa_phys, dcbaap_readback, dcbaap_readback == dcbaa_phys);
        crate::println!("[usb] xHCI DCBAAP: wrote={:#x} readback={:#x} match={}", dcbaa_phys, dcbaap_readback, dcbaap_readback == dcbaa_phys);

        // Command ring: 127 command TRBs + Link TRB back to base.
        // Initialize all TRBs to 0 (cycle=0) except the link TRB which has cycle=1.
        let (cmd_virt, cmd_phys, cmd_ptr) = dma_page().ok_or("cmd ring alloc")?;
        unsafe {
            // Zero all TRBs (128 TRBs * 16 bytes = 2048 bytes, half the page)
            core::ptr::write_bytes(cmd_ptr, 0, RING_TRBS * 16);
            // Link TRB at the end: points back to ring base, cycle=1, TC=1
            let link_lo = (cmd_phys & 0xFFFF_FFFF) as u32;
            let link_hi = (cmd_phys >> 32) as u32;
            trb_write(
                cmd_virt,
                LINK_IDX,
                [
                    link_lo,
                    link_hi,
                    0,
                    TRB_CYCLE | TRB_LINK_TC | (TRB_LINK << TRB_TYPE_SHIFT),
                ],
            );
}
// Verify command ring physical address is in 32-bit address space (required for CTX64=0)
        if cmd_phys >= 0x1_0000_0000 {
            crate::serial_println!(
                "[usb] xHCI CRCR ERROR: cmd_phys={:#x} >= 4GB, CTX64=0 requires <4GB",
                cmd_phys
            );
            crate::println!("[usb] xHCI CRCR ERROR: cmd_phys={:#x} >= 4GB, CTX64=0 requires <4GB", cmd_phys);
            return Err("xHCI command ring >= 4GB");
        }

        // Controller-state snapshot before touching CRCR: CRCR writes are
        // only honored while halted and ready. Full op-register dump so one
        // photo shows whether the failure is CRCR-only or wider.
        let pre_sts = unsafe { mmio_r32(op, OP_USBSTS) };
        let pre_cmd = unsafe { mmio_r32(op, OP_USBCMD) };
        let pre_cfg = unsafe { mmio_r32(op, OP_CONFIG) };
        let pre_crcr_lo = unsafe { mmio_r32(op, OP_CRCR) };
        let pre_crcr_hi = unsafe { mmio_r32(op, OP_CRCR + 4) };
        let pre_dcba_lo = unsafe { mmio_r32(op, OP_DCBAAP) };
        let pre_dcba_hi = unsafe { mmio_r32(op, OP_DCBAAP + 4) };
        crate::println!(
            "[usb] xHCI preCRCR: STS={:#x} HCH={} CNR={} CMD={:#x} CFG={:#x}",
            pre_sts,
            pre_sts & STS_HCHALTED != 0,
            (pre_sts & STS_CNR) != 0,
            pre_cmd,
            pre_cfg
        );
        crate::println!(
            "[usb] xHCI preCRCR: CRCR=lo:{:#x} hi:{:#x} DCBAAP=lo:{:#x} hi:{:#x}",
            pre_crcr_lo, pre_crcr_hi, pre_dcba_lo, pre_dcba_hi
        );

        // CRCR write with retry + warn-continue
        let expected_crcr = cmd_phys | 0x1;
        let mut crcr_ok = false;
        for attempt in 0..4 {
            unsafe {
                if attempt == 2 {
                    // Single 64-bit qword write (8-byte aligned: page base +
                    // 4-aligned caplen + 0x18 stays 8-aligned for typical
                    // caplen values; guard anyway).
                    let addr = (op + OP_CRCR) as *mut u64;
                    if (addr as usize) & 0x7 == 0 {
                        core::ptr::write_volatile(addr, expected_crcr);
                    } else {
                        mmio_w64(op, OP_CRCR, expected_crcr);
                    }
                } else {
                    mmio_w64(op, OP_CRCR, expected_crcr);
                }
            }
            let crcr_check = unsafe { mmio_r64(op, OP_CRCR) };
            // Compare ignoring RO/status low nibble: pass iff pointer equal and RCS set
            if (crcr_check & !0xF) == (expected_crcr & !0xF) && (crcr_check & 1) == 1 {
                crcr_ok = true;
                break;
            }
            // Retry with opposite dword order (some controllers latch on high write)
            if attempt == 0 {
                unsafe {
                    // Write high dword first, then low dword
                    core::ptr::write_volatile((op + OP_CRCR + 4) as *mut u32, (expected_crcr >> 32) as u32);
                    core::ptr::write_volatile((op + OP_CRCR) as *mut u32, expected_crcr as u32);
                }
            } else if attempt == 1 {
                unsafe {
                    // Standard low-first order
                    core::ptr::write_volatile((op + OP_CRCR) as *mut u32, expected_crcr as u32);
                    core::ptr::write_volatile((op + OP_CRCR + 4) as *mut u32, (expected_crcr >> 32) as u32);
                }
            }
            // Small delay between retries
            crate::drivers::time_source::delay_ms(1);
        }

        let crcr_check = unsafe { mmio_r64(op, OP_CRCR) };
        let crcr_ok_final = (crcr_check & !0xF) == (expected_crcr & !0xF) && (crcr_check & 1) == 1;
        // Also read back DCBAAP for diagnostic
        let dcbaap_check = unsafe { mmio_r64(op, OP_DCBAAP) };
        crate::serial_println!(
            "[usb] xHCI CRCR: cmd_phys={:#x} expected={:#x} read={:#x} ok={} | DCBAAP wrote={:#x} read={:#x} match={}",
            cmd_phys, expected_crcr, crcr_check, crcr_ok_final,
            dcbaa_phys, dcbaap_check, dcbaap_check == dcbaa_phys
        );
        crate::println!("[usb] xHCI CRCR: cmd={:#x} expected={:#x} read={:#x} ok={} | DCBAAP wrote={:#x} read={:#x} match={}", 
            cmd_phys, expected_crcr, crcr_check, crcr_ok_final,
            dcbaa_phys, dcbaap_check, dcbaap_check == dcbaa_phys);

        if !crcr_ok_final {
            let dcbaap_check = unsafe { mmio_r64(op, OP_DCBAAP) };
            crate::serial_println!(
                "[usb] xHCI CRCR MISMATCH: cmd_phys={:#x} expected={:#x} got={:#x} diff={:#x} — WARN: continuing | DCBAAP wrote={:#x} read={:#x} match={}",
                cmd_phys, expected_crcr, crcr_check, crcr_check ^ expected_crcr,
                dcbaa_phys, dcbaap_check, dcbaap_check == dcbaa_phys
            );
            crate::println!(
                "[usb] xHCI CRCR WARN: cmd={:#x} exp={:#x} got={:#x} diff={:#x} — continuing | DCBAAP wrote={:#x} read={:#x} match={}",
                cmd_phys, expected_crcr, crcr_check, crcr_check ^ expected_crcr,
                dcbaa_phys, dcbaap_check, dcbaap_check == dcbaa_phys
            );
            // Warn and continue — NOOP test is the real functional arbiter
        }
        // Event ring: single 128-TRB segment, consumer cycle 1.
        let (evt_virt, evt_phys, _) = dma_page().ok_or("event ring alloc")?;
        let (erst_virt, erst_phys, _) = dma_page().ok_or("ERST alloc")?;
crate::serial_println!(
            "[usb] xHCI DMA dcbaa={:#x} cmd={:#x} evt={:#x} erst={:#x} ac64={}",
            dcbaa_phys,
            cmd_phys,
            evt_phys,
            erst_phys,
            ac64 as u8
        );
        crate::println!("[usb] xHCI DMA: dcbaa={:#x} cmd={:#x} evt={:#x} erst={:#x} ac64={}", dcbaa_phys, cmd_phys, evt_phys, erst_phys, ac64 as u8);
        unsafe {
            core::ptr::write_volatile(erst_virt as *mut u64, evt_phys);
            core::ptr::write_volatile((erst_virt + 8) as *mut u32, RING_TRBS as u32);
            core::ptr::write_volatile((erst_virt + 12) as *mut u32, 0);
            mmio_w32(ir0, IR_ERSTSZ, 1);
            mmio_w64(ir0, IR_ERSTBA, erst_phys);
            // Self-test: read ERSTBA back. A mismatch means our register
            // programming isn't landing (wrong offsets/BAR) â€” fail loudly
            // instead of timing out later with no completions.
            if mmio_r64(ir0, IR_ERSTBA) != erst_phys {
                return Err("xHCI ERSTBA mismatch");
            }
            // ERDP = base with EHB set (Linux writes it set, like here).
            mmio_w64(ir0, IR_ERDP, evt_phys | ERDP_EHB);
            // Clear stale pending state. Event-ring production is independent
            // of the interrupt enable bit; this driver consumes events by
            // polling and does not install an xHCI interrupt handler yet.
            mmio_w32(ir0, IR_IMAN, IMAN_IP);
        }

        // Poll the event ring manually. xHCI has no EHCI-style USBINTR
        // register at operational offset 0x08; that offset is PAGESIZE and
        // must not be overwritten after validation.
        unsafe {
            mmio_w32(op, OP_DNCTRL, 0);
        }

        let mut ctl = Self {
            pci_bus: pci.bus,
            pci_dev: pci.device,
            pci_func: pci.function,
            max_slots,
            max_ports,
            ctx_is_64,
            op,
            db,
            ir0,
            cmd_virt,
            cmd_phys,
            cmd_idx: 0,
            cmd_cycle: 1,
            evt_virt,
            evt_phys,
            evt_idx: 0,
            evt_cycle: 1,
            last_cmd_trb: 0,
            dcbaa_virt,
            slots: Vec::new(),
        };

        // Run.
        unsafe {
            let cmd = mmio_r32(op, OP_USBCMD);
            mmio_w32(op, OP_USBCMD, cmd | CMD_RS);
        }
        ok = false;
        for _ in 0..1000 {
            if unsafe { mmio_r32(op, OP_USBSTS) } & STS_HCHALTED == 0 {
                ok = true;
                break;
            }
            delay_ms(1);
        }
        if !ok {
            return Err("xHCI start timeout");
        }
        // Wait for CNR (Controller Not Ready, USBSTS bit 11) to clear —
        // controller must be ready to accept commands before we submit NOOP.
        // (Bit 3 is Port Change Detect, not readiness.)
        ok = false;
        for _ in 0..1000 {
            let sts = unsafe { mmio_r32(op, OP_USBSTS) };
            if sts & STS_CNR == 0 {
                ok = true;
                break;
            }
            delay_ms(1);
        }
        if !ok {
            let sts = unsafe { mmio_r32(op, OP_USBSTS) };
            crate::println!("[usb] xHCI CNR stuck set (STS={:#x})", sts);
            return Err("xHCI CNR timeout");
        }
// Verify CRCR after controller starts
        let crcr_after_start = unsafe { mmio_r64(op, OP_CRCR) };
        crate::serial_println!(
            "[usb] xHCI running (STS={:#x} CRCR={:#x})",
            unsafe { mmio_r32(op, OP_USBSTS) },
            crcr_after_start
        );
        crate::println!("[usb] xHCI running (STS={:#x} CRCR={:#x}), testing NOOP...", 
            unsafe { mmio_r32(op, OP_USBSTS) }, crcr_after_start);

        // Prove command + event rings end-to-end with a NO-OP command.
        ctl.submit_noop();
        match ctl.wait_completion(500) {
            Some((COMP_SUCCESS, _)) => {
                crate::println!(
                    "[usb] xHCI {:02x}:{:02x}.{} RUN, NOOP ok ({} ports)",
                    pci.bus,
                    pci.device,
                    pci.function,
                    max_ports
                );
            }
            Some((code, _)) => {
                crate::serial_println!("[usb] xHCI NOOP failed, code {}", code);
                crate::println!("[usb] xHCI NOOP failed, code {}", code);
                return Err("xHCI NOOP failed");
            }
            None => {
                ctl.dump_command_state();
                // VGA-visible failure state: without serial capture a bare
                // "init failed" line cannot distinguish lost doorbell/completions
                // from wedged rings. STS + CRCR answer that in one line.
                let usbsts = unsafe { mmio_r32(op, OP_USBSTS) };
                let crcr = unsafe { mmio_r64(op, OP_CRCR) };
                crate::println!(
                    "[usb] xHCI NOOP timeout (STS={:#x} CRCR={:#x})",
                    usbsts,
                    crcr
                );
                return Err("xHCI NOOP timeout");
            }
        }

        Ok(ctl)
    }

    /// BIOS/OS ownership handoff via the USBLEGSUP extended capability.
    /// Walks at most 16 caps; absence is fine (continue without handoff).
    fn bios_handoff(base: usize, hcc1: u32) -> Result<(), &'static str> {
        let xecp = ((hcc1 >> 16) & 0xFFFF) as usize;
        if xecp == 0 {
            return Ok(());
        }
        // xECP is in DWORDs from MMIO base.
        let mut off = xecp << 2;
        for _ in 0..16 {
            if off < 0x40 || off > 0xF000 {
                break;
            }
            let hdr = unsafe { mmio_r32(base, off) };
            if hdr == 0 || hdr == 0xFFFF_FFFF {
                break;
            }
            let id = hdr & 0xFF;
            let next = ((hdr >> 8) & 0xFF) as usize;
            if id == EXT_CAP_LEGACY {
                unsafe {
                    let sup = mmio_r32(base, off);
                    mmio_w32(base, off, sup | LEG_OS_OWNED);
                }
                let mut ok = false;
                for _ in 0..1000 {
                    if unsafe { mmio_r32(base, off) } & LEG_BIOS_OWNED == 0 {
                        ok = true;
                        break;
                    }
                    delay_ms(1);
                }
                unsafe {
                    // Clear SMI enables, then W1C the SMI status bits.
                    let ctl = mmio_r32(base, off + 4);
                    mmio_w32(base, off + 4, ctl & !LEG_DISABLE_SMI);
                    mmio_w32(base, off + 4, LEG_SMI_EVENTS);
                }
                if !ok {
                    crate::println!("[usb] xHCI BIOS handoff timeout, continuing");
                } else {
                    crate::serial_println!("[usb] xHCI OS ownership acquired");
                }
                return Ok(());
            }
            if next == 0 {
                break;
            }
            off += next << 2;
        }
        Ok(())
    }

    /// Ring doorbell 0 (host controller: poke the command ring).
    fn ring_db(&self) {
        unsafe {
            mmio_w32(self.db, 0, 0);
        }
    }

    /// Enqueue one command TRB (dwords 0-2 payload, dword3 type/flags without
    /// cycle) and ring the doorbell. Remembers its address for completion.
    fn submit_cmd(&mut self, d0: u32, d1: u32, d2: u32, d3: u32) {
        let d3 = d3 | (self.cmd_cycle & 0x1);
        unsafe {
            trb_write(self.cmd_virt, self.cmd_idx, [d0, d1, d2, d3]);
        }
        self.last_cmd_trb = self.cmd_phys + (self.cmd_idx as u64) * TRB_SIZE;
        self.cmd_idx += 1;
        if self.cmd_idx == LINK_IDX {
            // The Link TRB belongs to the current producer cycle.  The
            // producer toggles its cycle only after traversing the link.
            let link_lo = (self.cmd_phys & 0xFFFF_FFFF) as u32;
            let link_hi = (self.cmd_phys >> 32) as u32;
            unsafe {
                trb_write(
                    self.cmd_virt,
                    LINK_IDX,
                    [
                        link_lo,
                        link_hi,
                        0,
                        (self.cmd_cycle & 0x1) | TRB_LINK_TC | (TRB_LINK << TRB_TYPE_SHIFT),
                    ],
                );
            }
            self.cmd_cycle ^= 1;
            self.cmd_idx = 0;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.ring_db();
    }

    fn submit_noop(&mut self) {
        self.submit_cmd(0, 0, 0, TRB_CMD_NOOP << TRB_TYPE_SHIFT);
    }

    /// Drain pending events. Returns (completion code, slot id) when a
    /// Command Completion event for our last command arrives, else None.
    /// Other events (transfer, port change, etc.) are consumed and ignored.
    fn drain_events(&mut self) -> Option<(u32, u32)> {
        let mut found = None;
        let mut consumed = false;
        loop {
            let t = unsafe { trb_read(self.evt_virt, self.evt_idx) };
            if t[3] & TRB_CYCLE != self.evt_cycle {
                break;
            }
            consumed = true;
            let ty = (t[3] >> TRB_TYPE_SHIFT) & 0x3F;
            if ty == TRB_COMPLETION {
                let ptr = (t[0] as u64) | ((t[1] as u64) << 32);
                if ptr == self.last_cmd_trb {
                    found = Some(((t[2] >> 24) & 0xFF, (t[3] >> 24) & 0xFF));
                }
            }
            self.evt_idx += 1;
            if self.evt_idx == RING_TRBS {
                self.evt_idx = 0;
                self.evt_cycle ^= 1;
            }
            // Keep draining any backlog; `found` already holds our answer.
        }
        if consumed {
            unsafe {
                mmio_w64(
                    self.ir0,
                    IR_ERDP,
                    self.evt_phys + (self.evt_idx as u64) * TRB_SIZE | ERDP_EHB,
                );
                mmio_w32(self.ir0, IR_IMAN, IMAN_IP);
                // EINT (USBSTS bit3) is W1C; clear so status stays tidy.
                mmio_w32(self.op, 0x04, 1 << 3);
            }
        }
        found
    }

    /// Wait for a command completion without starving the virtual/device event
    /// machinery. HLT yields execution when interrupts are enabled, which is
    /// important for emulators where a USB control request may complete
    /// asynchronously.
    fn wait_completion(&mut self, timeout_ms: usize) -> Option<(u32, u32)> {
        // IRQ-independent deadline: legacy PIC IRQ0 is dead on UEFI/APIC
        // machines (proven by the 0->0 PIT tick check), so `uptime_millis()`
        // freezes and any `hlt()` here would sleep until an interrupt that
        // never arrives (polled rings, no keyboard attached). Polled PIT
        // channel 2 works with interrupts in any state and fails open when
        // no PIT exists. Never nest: channel 2 is single-shared, and these
        // waits always run sequentially.
        let mut deadline =
            crate::drivers::pit::timeout_ms(timeout_ms as u32);
// Backstop so a missing PIT (instant-true poll) still bounds the
        // loop instead of spinning on a wedged controller indefinitely.
        let max_iters = timeout_ms.saturating_mul(50_000).max(500_000);
        let mut iters = 0usize;

        loop {
            if let Some(code) = self.drain_events() {
                return Some(code);
            }

            if iters >= max_iters {
                break;
            }
            crate::drivers::time_source::delay_ms(1);
            iters = iters.saturating_add(1);
        }

        self.drain_events()
    }

    /// Dump state when a command produces no completion event.
    fn dump_command_state(&self) {
        let usbcmd = unsafe { mmio_r32(self.op, OP_USBCMD) };
        let usbsts = unsafe { mmio_r32(self.op, OP_USBSTS) };
        let crcr = unsafe { mmio_r64(self.op, OP_CRCR) };
        let dcbaap = unsafe { mmio_r64(self.op, OP_DCBAAP) };
        let iman = unsafe { mmio_r32(self.ir0, IR_IMAN) };
        let erstsz = unsafe { mmio_r32(self.ir0, IR_ERSTSZ) };
        let erstba = unsafe { mmio_r64(self.ir0, IR_ERSTBA) };
        let erdp = unsafe { mmio_r64(self.ir0, IR_ERDP) };
        let cmd = unsafe { trb_read(self.cmd_virt, 0) };
        let evt = unsafe { trb_read(self.evt_virt, self.evt_idx) };
        crate::serial_println!(
            "[usb] xHCI NOOP state cmd={:#x} sts={:#x} crcr={:#x} dcbaap={:#x}",
            usbcmd,
            usbsts,
            crcr,
            dcbaap
        );
        crate::serial_println!(
            "[usb] xHCI NOOP ring iman={:#x} erstsz={} erstba={:#x} erdp={:#x} last={:#x}",
            iman,
            erstsz,
            erstba,
            erdp,
            self.last_cmd_trb
        );
        crate::serial_println!(
            "[usb] xHCI NOOP trb cmd=[{:#x},{:#x},{:#x},{:#x}] evt[{}]=[{:#x},{:#x},{:#x},{:#x}] cycle={}",
            cmd[0],
            cmd[1],
            cmd[2],
            cmd[3],
            self.evt_idx,
            evt[0],
            evt[1],
            evt[2],
            evt[3],
            self.evt_cycle
        );
    }

    /// Poll completed interrupt-IN transfers for claimed HID keyboards.
    /// Drains Transfer Events, pushes HID reports, and re-primes endpoints.
    /// Never blocks; safe to call from the shell loop at high rate.
    pub fn poll(&mut self) {
        use alloc::vec::Vec;
        let mut matched: Vec<(usize, u32)> = Vec::new();
        let mut port_events: Vec<u32> = Vec::new();
        let mut consumed = false;
        loop {
            let t = unsafe { trb_read(self.evt_virt, self.evt_idx) };
            if t[3] & TRB_CYCLE != self.evt_cycle {
                break;
            }
            consumed = true;
            let ty = (t[3] >> TRB_TYPE_SHIFT) & 0x3F;
            if ty == TRB_PORT_STATUS_CHANGE {
                // Root-hub port id in dword0[31:24] (xHCI 6.4.2.4).
                let port = (t[0] >> 24) & 0xFF;
                if port != 0 {
                    port_events.push(port);
                }
            } else if ty == TRB_TRANSFER {
                let ptr = (t[0] as u64) | ((t[1] as u64) << 32);
                let code = (t[2] >> 24) & 0xFF;
                let slot_id = (t[3] >> 24) & 0xFF;
                let epid = (t[3] >> 16) & 0x1F;
                // Match by pending TRB pointer (unique per ring). Fall back
                // to slot+ep match for controllers that rewrite the pointer.
                let mut hit: Option<usize> = None;
                for (i, s) in self.slots.iter().enumerate() {
                    if !s.hid_claimed || s.hid_pending_trb == 0 {
                        continue;
                    }
                    if s.hid_pending_trb == ptr {
                        // Strong match; verify slot when available.
                        if s.id == slot_id || slot_id == 0 {
                            hit = Some(i);
                            break;
                        }
                        hit = Some(i);
                        break;
                    }
                }
                if hit.is_none() {
                    for (i, s) in self.slots.iter().enumerate() {
                        if s.hid_claimed && s.id == slot_id && s.hid_dci == epid {
                            hit = Some(i);
                            break;
                        }
                    }
                }
                if let Some(i) = hit {
                    matched.push((i, code));
                }
            }
            self.evt_idx += 1;
            if self.evt_idx == RING_TRBS {
                self.evt_idx = 0;
                self.evt_cycle ^= 1;
            }
        }
        if consumed {
            self.sync_events();
        } else {
            // Preserve Phase-1 behavior: don't leave IP stuck when no
            // consumable event was found (spurious or race).
            let iman = unsafe { mmio_r32(self.ir0, IR_IMAN) };
            if iman & IMAN_IP != 0 {
                unsafe {
                    mmio_w32(self.ir0, IR_IMAN, IMAN_IP);
                }
            }
            return;
        }
        for (idx, code) in matched {
            self.handle_hid_completion(idx, code);
        }
        // Service hot-plug port events after the ring is synced: reset,
        // address (strict FS maxpacket0=8), and HID-claim without reboot.
        // Bounded: at most one rescan per signalled port per poll.
        for port in port_events {
            self.rescan_one_port(port as usize);
        }
    }

    fn handle_hid_completion(&mut self, idx: usize, code: u32) {
        if idx >= self.slots.len() {
            return;
        }
        let (buf_virt, claimed) = {
            let s = &self.slots[idx];
            (s.hid_buf_virt, s.hid_claimed)
        };
        if !claimed || buf_virt == 0 {
            return;
        }
        if code == COMP_SUCCESS || code == COMP_SHORT_PACKET {
            let mut report = [0u8; 8];
            unsafe {
                let src = buf_virt as *const u8;
                for i in 0..8 {
                    report[i] = core::ptr::read_volatile(src.add(i));
                }
            }
            // Idle (all-zero) reports are filtered inside push_usb_report
            // via previous-report tracking; still forward them to keep
            // release detection accurate.
            crate::drivers::keyboard::push_usb_report(report);
        } else {
            crate::serial_println!(
                "[usb] xHCI HID xfer code {} slot {}",
                code,
                self.slots[idx].id
            );
        }
        // Always re-prime so a single error cannot stall the endpoint.
        self.prime_interrupt(idx);
    }

    // -----------------------------------------------------------------------
    // Phase 2: enumeration (ports -> slots -> addressed -> configured)
    // -----------------------------------------------------------------------

    /// Enumerate all connected root-hub ports. Prints one summary line.
    pub fn enumerate(&mut self) {
        for port in 1..=self.max_ports {
            self.scan_port(port);
        }
        let n = self.slots.iter().filter(|s| s.configured).count();
        crate::println!(
            "[usb] xHCI {:02x}:{:02x}.{} {} configured device(s)",
            self.pci_bus,
            self.pci_dev,
            self.pci_func,
            n
        );
    }

    /// Throttled hot-plug rescan over all root ports (called from usb::poll).
    /// Only ports with CSC set are touched; quiet otherwise. Handles both
    /// connect (reset -> address with strict FS maxpacket0=8 -> HID claim)
    /// and disconnect (slot teardown) without reboot.
    pub fn rescan_ports(&mut self) {
        for port in 1..=self.max_ports {
            let sc = self.portsc(port);
            if sc & PORT_CSC == 0 {
                continue;
            }
            self.rescan_one_port(port);
        }
    }

    /// Service one hot-plug port: clear CSC (W1C), then connect or disconnect.
    /// Never blocks long; all port waits stay inside bounded reset_port().
    fn rescan_one_port(&mut self, port: usize) {
        if port == 0 || port > self.max_ports {
            return;
        }
        let sc = self.portsc(port);
        // Clear connect-status-change (W1C) so the next plug re-fires.
        // scan/reset also clear change bits; this covers event-only paths.
        if sc & PORT_CSC != 0 {
            self.set_portsc(port, sc | PORT_CHANGE_BITS);
        }
        if sc & PORT_CCS == 0 {
            // Departed: tear down every slot owned by this root port so a
            // later plug can ENABLE_SLOT fresh instead of leaking slots.
            if self.slots.iter().any(|s| s.port == port) {
                let ids: Vec<u32> =
                    self.slots.iter().filter(|s| s.port == port).map(|s| s.id).collect();
                for id in ids {
                    self.disable_slot(id);
                }
                self.slots.retain(|s| s.port != port);
                crate::println!("[usb] xHCI port{} unplugged, slots released", port);
            }
            return;
        }
        // Connected: skip if a live slot already owns this port.
        if self.slots.iter().any(|s| s.port == port && s.configured) {
            return;
        }
        // Drop stale unconfigured remnants for this port, then enumerate.
        if self.slots.iter().any(|s| s.port == port) {
            let ids: Vec<u32> =
                self.slots.iter().filter(|s| s.port == port).map(|s| s.id).collect();
            for id in ids {
                self.disable_slot(id);
            }
            self.slots.retain(|s| s.port != port);
        }
        let sc = self.portsc(port);
        if sc & PORT_CCS == 0 {
            return;
        }
        let Some(speed) = self.reset_port(port) else {
            return;
        };
        if self.identify(port, speed, None, 0, 0).is_some() {
            // claim_hid_keyboards() is idempotent: skips claimed slots.
            self.claim_hid_keyboards();
        }
    }

    fn portsc(&self, port: usize) -> u32 {
        unsafe { mmio_r32(self.op, 0x400 + (port - 1) * 0x10) }
    }

    fn set_portsc(&self, port: usize, val: u32) {
        unsafe { mmio_w32(self.op, 0x400 + (port - 1) * 0x10, val) };
    }

    fn ctx_size(&self) -> u64 {
        if self.ctx_is_64 {
            64
        } else {
            32
        }
    }

    fn dcbaa_set(&self, slot: u32, phys: u64) {
        unsafe {
            core::ptr::write_volatile((self.dcbaa_virt + slot as u64 * 8) as *mut u64, phys);
        }
    }

    fn ctx_w32(virt: u64, off: u64, val: u32) {
        unsafe {
            core::ptr::write_volatile((virt + off) as *mut u32, val);
        }
    }

    fn ctx_r32(virt: u64, off: u64) -> u32 {
        unsafe { core::ptr::read_volatile((virt + off) as *const u32) }
    }

    /// Reset one connected root port; returns its PORTSC speed on success.
    /// SuperSpeed ports are used as-is when already enabled, else deferred.
    fn reset_port(&self, port: usize) -> Option<u32> {
        let sc = self.portsc(port);
        if sc & PORT_CCS == 0 {
            return None;
        }
        let speed = (sc >> 10) & 0xF;
        if speed >= SPEED_SS {
            if sc & PORT_PED != 0 {
                return Some(speed);
            }
            crate::println!("[usb] xHCI port{} SS not enabled, deferred", port);
            return None;
        }
        if speed != SPEED_FS && speed != SPEED_LS && speed != SPEED_HS {
            crate::println!("[usb] xHCI port{} unknown speed {}, skipped", port, speed);
            return None;
        }
        // Preserve the live connection/speed fields during the reset pulse.
        // Firmware uses this form with QEMU and real xHCI controllers; W1C
        // change bits are cleared separately after reset completes.
        self.set_portsc(port, sc | PORT_PP);
        delay_ms(10);
        let sc = self.portsc(port) & !PORT_CHANGE_BITS;
        self.set_portsc(port, sc | PORT_PR);
        // Phase 1: wait for reset-clear (PR -> 0), bounded.
        let mut pr_cleared = false;
        for _ in 0..500 {
            let s = self.portsc(port);
            if s & PORT_PR == 0 {
                pr_cleared = true;
                break;
            }
            delay_ms(1);
        }
        if !pr_cleared {
            crate::println!("[usb] xHCI port{} reset timeout", port);
            return None;
        }
        // Phase 2: explicitly verify Port Enabled (PED bit 1) + Connected.
        // Slow FS transceivers may clear PR before PED asserts; poll PED
        // bounded instead of sampling once, or ADDRESS_DEVICE will take
        // USB Transaction Error (code 4) on a non-enabled port.
        for _ in 0..100 {
            let s = self.portsc(port);
            if s & PORT_PED != 0 && s & PORT_CCS != 0 {
                self.set_portsc(port, s | PORT_CHANGE_BITS);
                // USB 2.0 TDRSTR: the device needs >=10ms reset recovery
                // before SET_ADDRESS. A slow FS micro addressed immediately
                // stays silent and the HC reports Transaction Error (code 4).
                // Use 100ms for slow real hardware (Apple, etc.).
                delay_ms(100);
                // Speed is re-latched after reset; re-read it.
                return Some((self.portsc(port) >> 10) & 0xF);
            }
            delay_ms(1);
        }
        let s = self.portsc(port);
        self.set_portsc(port, s | PORT_CHANGE_BITS);
        crate::println!(
            "[usb] xHCI port{} reset: PED not set ({:#x}), not enabled",
            port,
            s
        );
        None
    }

    /// Scan one root port: reset, then address + identify (once per speed
    /// class the port reports; xHCI gives us the speed, no guessing needed).
    fn scan_port(&mut self, port: usize) {
        let sc = self.portsc(port);
        if sc & PORT_CCS == 0 {
            return;
        }
        let Some(speed) = self.reset_port(port) else {
            return;
        };
        let name = match speed {
            SPEED_FS => "FS",
            SPEED_LS => "LS",
            SPEED_HS => "HS",
            SPEED_SS => "SS",
            _ => "?",
        };
        crate::println!("[usb] xHCI port{} reset OK speed={}", port, name);
        self.identify(port, speed, None, 0, 0);
    }

    /// Enable Slot command; returns the HC-assigned slot id.
    fn enable_slot(&mut self) -> Option<u32> {
        if self.slots.len() >= self.max_slots as usize {
            return None;
        }
        self.submit_cmd(0, 0, 0, TRB_ENABLE_SLOT << TRB_TYPE_SHIFT);
        match self.wait_completion(500) {
            Some((COMP_SUCCESS, slot)) if slot != 0 => Some(slot),
            Some((code, _)) => {
                crate::serial_println!("[usb] xHCI enable slot failed, code {}", code);
                None
            }
            None => {
                crate::serial_println!("[usb] xHCI enable slot timeout");
                None
            }
        }
    }

    /// Best-effort slot release (bounded wait, result ignored by callers).
    fn disable_slot(&mut self, slot: u32) {
        self.submit_cmd(0, 0, 0, (slot << 24) | (TRB_DISABLE_SLOT << TRB_TYPE_SHIFT));
        let _ = self.wait_completion(500);
    }

    /// Drop slots from `from` on (disabling each in HW first). Used to unwind
    /// a failed enumeration, including hub children pushed after the parent.
    fn drop_slots_from(&mut self, from: usize) {
        let ids: Vec<u32> = self.slots.iter().skip(from).map(|s| s.id).collect();
        for id in ids {
            self.disable_slot(id);
        }
        self.slots.truncate(from);
    }

    /// Shared Address-Device failure path: pre-address snapshot plus the
    /// BSR=1 isolation probe (both VGA). The caller unwinds via its own
    /// `fail` closure afterwards.
    fn address_failed_probe(
        &mut self,
        port: usize,
        slot_id: u32,
        code: u32,
        out_virt: u64,
        in_phys: u64,
    ) {
        if XHCI_ADDR_DEBUG {
            let addr_field = Self::ctx_r32(out_virt, 12) & 0xFF;
            crate::println!(
                "[usb] xHCI port{} addr dbg: slot {} out.addr {:#x} in {:#x}",
                port,
                slot_id,
                addr_field,
                in_phys
            );
        }
        if XHCI_BSR_PROBE && code == 4 {
            // Isolation probe: same slot + input context, BSR=1 does
            // no USB transaction. Legal from Enabled state after a
            // failed BSR=0; bounded 500ms; unwind follows regardless.
            self.submit_cmd(
                (in_phys & 0xFFFF_FFFF) as u32,
                (in_phys >> 32) as u32,
                0,
                (slot_id << 24) | TRB_BSR | (TRB_ADDR_DEV << TRB_TYPE_SHIFT),
            );
            match self.wait_completion(500) {
                Some((COMP_SUCCESS, _)) => {
                    crate::println!(
                        "[usb] xHCI port{} BSR=1 probe OK (ctx valid, dev silent)",
                        port
                    );
                }
                Some((pcode, _)) => {
                    crate::println!(
                        "[usb] xHCI port{} BSR=1 probe code {} (ctx fault?)",
                        port,
                        pcode
                    );
                }
                None => {
                    crate::println!("[usb] xHCI port{} BSR=1 probe timeout", port);
                }
            }
        }
    }

    /// Full address + identify flow for one device. `tt`/`route` describe hub
    /// attachment (None/0 for root ports). Returns the slot index on success
    /// (device left addressed; configured if HID/hub), else None.
    fn identify(
        &mut self,
        port: usize,
        speed: u32,
        tt: Option<(u8, u8)>,
        route: u32,
        depth: u8,
    ) -> Option<usize> {
        let my_start = self.slots.len();
        let fail = |ctl: &mut Self| {
            ctl.drop_slots_from(my_start);
            None
        };

        let Some(slot_id) = self.enable_slot() else {
            crate::println!("[usb] xHCI port{}: no slot free", port);
            return None;
        };

        // Output device context page; publish in DCBAA before addressing.
        let (out_virt, out_phys, _) = dma_page()?;
        self.dcbaa_set(slot_id, out_phys);
        // Input context page (reused for the later maxpacket evaluate).
        let (in_virt, in_phys, _) = dma_page()?;
        // EP0 transfer ring (Link TRB, DCS=1).
        let (ep0_virt, ep0_phys, _) = dma_page()?;
        unsafe {
            trb_write(
                ep0_virt,
                LINK_IDX,
                [
                    (ep0_phys & 0xFFFF_FFFF) as u32,
                    (ep0_phys >> 32) as u32,
                    0,
                    TRB_CYCLE | TRB_LINK_TC | (TRB_LINK << TRB_TYPE_SHIFT),
                ],
            );
        }
        let esz = self.ctx_size();

        // Input and output device contexts are controller-owned DMA memory.
        // Zero the complete pages so every reserved field starts at zero.
        unsafe {
            core::ptr::write_bytes(out_virt as *mut u8, 0, 4096);
            core::ptr::write_bytes(in_virt as *mut u8, 0, 4096);
        }

        // Input Control Context: drop nothing, add Slot + EP0. Both must be
        // flagged valid for the BSR=0 Address Device below, and Slot
        // Context Entries (Last Ctx) must be >= 1 so the HC evaluates EP0.
        Self::ctx_w32(in_virt, 0x00, 0);
        Self::ctx_w32(in_virt, 0x04, ADD_SLOT | ADD_EP0);

        let tt_info = match tt {
            Some((hub_slot, hub_port)) if speed == SPEED_FS || speed == SPEED_LS => {
                (hub_slot as u32) | ((hub_port as u32) << 8)
            }
            _ => 0,
        };

        // Slot Context. For a root-port device the route string is zero.
        // Context Entries = 1 because only EP0 is being added.
        let dev_info = (route & 0x000F_FFFF)
            | ((speed & 0xF) << SLOT_SPEED_SHIFT)
            | (1 << SLOT_LAST_CTX_SHIFT);

        Self::ctx_w32(in_virt, esz + 0x00, dev_info);
        Self::ctx_w32(in_virt, esz + 0x04, (port as u32) << 16);
        Self::ctx_w32(in_virt, esz + 0x08, tt_info);
        Self::ctx_w32(in_virt, esz + 0x0C, 0);

        // EP0 default max packet size for the initial Address Device command.
        // Strict: FS/LS control EP0 is always 8 bytes. Assuming 64 here
        // makes the HC's SET_ADDRESS fail with Transaction Error (code 4)
        // on Intel PCH full-speed keyboards.
        let ep0_max_packet: u32 = match speed {
            SPEED_LS => 8,
            SPEED_FS => 8,
            SPEED_HS => 64,
            SPEED_SS => 512,
            _ => return fail(self),
        };
        debug_assert!(
            speed != SPEED_FS || ep0_max_packet == 8,
            "FS maxpacket0 must be 8"
        );

        let ep0 = esz * 2;

        // Endpoint State = 0 (disabled), MaxPStreams = 0, LSA = 0,
        // Interval = 0, Max ESIT Payload = 0.
        let ep0_dword1 = (3 << 1) | (EP_TYPE_CTRL << 3) | (ep0_max_packet << 16);
        Self::ctx_w32(in_virt, ep0 + 0x00, 0);
        Self::ctx_w32(in_virt, ep0 + 0x04, ep0_dword1);

        // EP0 TR Dequeue Pointer + DCS=1.
        unsafe {
            core::ptr::write_volatile(
                (in_virt + ep0 + 0x08) as *mut u64,
                ep0_phys | 1,
            );
        }

        // Average TRB Length.
        Self::ctx_w32(in_virt, ep0 + 0x10, 8);

        // Pre-Address snapshot: proves what the HC will actually evaluate.
        // One photo line distinguishes FS-maxpacket, speed/port, and flag
        // faults from silent-device faults.
        if XHCI_ADDR_DEBUG {
            crate::println!(
                "[usb] xHCI port{} addr: slot {} speed {} maxp0 {} devinfo {:#x} ep0d1 {:#x} deq {:#x} add {:#x}",
                port,
                slot_id,
                speed,
                ep0_max_packet,
                dev_info,
                ep0_dword1,
                ep0_phys | 1,
                ADD_SLOT | ADD_EP0
            );
        }

        // Address Device (BSR=0: the HC assigns the address).
        let mut addr_retries = 0;
        let mut addr_delay_ms = 50;
        loop {
            self.submit_cmd(
                (in_phys & 0xFFFF_FFFF) as u32,
                (in_phys >> 32) as u32,
                0,
                (slot_id << 24) | (TRB_ADDR_DEV << TRB_TYPE_SHIFT),
            );
            match self.wait_completion(1000) {
                Some((COMP_SUCCESS, _)) => {
                    break;
                }
                Some((code, _)) if code == 4 && addr_retries < 3 => {
                    // Exponential backoff retry for Transaction Error (code 4):
                    // slow microcontrollers and wireless dongles can stay silent if
                    // SET_ADDRESS arrives during reset recovery.
                    // Backoff: 50ms, 100ms, 200ms (max 3 retries).
                    addr_retries += 1;
                    crate::println!(
                        "[usb] xHCI port{} address failed (code {}), retry {}/3 ({}ms)...",
                        port,
                        code,
                        addr_retries,
                        addr_delay_ms
                    );
                    crate::drivers::time_source::delay_ms(addr_delay_ms);
                    addr_delay_ms = addr_delay_ms.saturating_mul(2).min(200);
                    continue;
                }
                Some((code, _)) => {
                    crate::println!("[usb] xHCI port{} address failed (code {})", port, code);
                    self.address_failed_probe(port, slot_id, code, out_virt, in_phys);
                    return fail(self);
                }
                None => {
                    crate::println!("[usb] xHCI port{} address timeout", port);
                    return fail(self);
                }
            }
        }
        let addr = (Self::ctx_r32(out_virt, 12) & 0xFF) as u8;

        self.slots.push(XhciSlot {
            id: slot_id,
            port,
            speed,
            maxpacket0: ep0_max_packet as u8,
            addr,
            vid: 0,
            pid: 0,
            dev_class: 0,
            configured: false,
            cfg: Vec::new(),
            ep0_virt,
            ep0_phys,
            ep0_idx: 0,
            ep0_cycle: 1,
            in_ctx_virt: in_virt,
            in_ctx_phys: in_phys,
            out_ctx_virt: out_virt,
            out_ctx_phys: out_phys,
            hid_claimed: false,
            hid_ep: 0,
            hid_dci: 0,
            hid_maxpacket: 0,
            hid_interval: 0,
            int_virt: 0,
            int_phys: 0,
            int_idx: 0,
            int_cycle: 1,
            hid_buf_virt: 0,
            hid_buf_phys: 0,
            hid_pending_trb: 0,
        });
        let idx = self.slots.len() - 1;

        // 8-byte descriptor to learn the real EP0 maxpacket.
        let setup_get8 = setup_packet(0x80, 6, 0x0100, 0, 8);
        let Some(d8) = self.control_xfer(idx, "desc8", setup_get8, 8) else {
            crate::println!("[usb] xHCI port{} desc8 failed", port);
            return fail(self);
        };
        if d8.len() < 8 {
            crate::println!("[usb] xHCI port{} desc8 short", port);
            return fail(self);
        }
        let maxp = d8[7].clamp(8, 64);
        if XHCI_ADDR_DEBUG && maxp == 8 {
            crate::println!("[usb] xHCI port{} EP0 maxp stays 8 (FS), no eval", port);
        }
        if maxp != 8 && !self.eval_maxpacket(idx, maxp) {
            crate::println!("[usb] xHCI port{} maxpacket eval failed", port);
            return fail(self);
        }
        self.slots[idx].maxpacket0 = maxp;

        // Full device + config descriptors.
        let setup_full = setup_packet(0x80, 6, 0x0100, 0, 18);
        let Some(full) = self.control_xfer(idx, "desc18", setup_full, 18) else {
            crate::println!("[usb] xHCI port{} desc18 failed", port);
            return fail(self);
        };
        if full.len() < 18 {
            crate::println!("[usb] xHCI port{} desc18 short", port);
            return fail(self);
        }
        let vid = (full[8] as u16) | ((full[9] as u16) << 8);
        let pid = (full[10] as u16) | ((full[11] as u16) << 8);
        let class = full[4];
        self.slots[idx].vid = vid;
        self.slots[idx].pid = pid;
        self.slots[idx].dev_class = class;

        let setup_cfg9 = setup_packet(0x80, 6, 0x0200, 0, 9);
        let Some(cfg9) = self.control_xfer(idx, "cfg9", setup_cfg9, 9) else {
            crate::println!("[usb] xHCI port{} cfg9 failed", port);
            return fail(self);
        };
        if cfg9.len() < 9 {
            crate::println!("[usb] xHCI port{} cfg9 short", port);
            return fail(self);
        }
        let total = (cfg9[2] as usize) | ((cfg9[3] as usize) << 8);
        if total < 9 || total > 512 {
            crate::println!("[usb] xHCI port{} bad cfg len {}", port, total);
            return fail(self);
        }
        let setup_cfg = setup_packet(0x80, 6, 0x0200, 0, total as u16);
        let Some(cfg) = self.control_xfer(idx, "cfg", setup_cfg, total as u16) else {
            crate::println!("[usb] xHCI port{} cfg failed", port);
            return fail(self);
        };

        if class == 9 {
            if depth > 0 {
                crate::println!("[usb] xHCI nested hub deferred");
                return fail(self);
            }
            if !self.hub_flow(idx) {
                return fail(self);
            }
            self.slots[idx].configured = true;
            return Some(idx);
        }
        if find_hid_keyboard(&cfg).is_some() {
            let cfg_value = cfg.get(5).copied().unwrap_or(1);
            let setup_setcfg = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
            if self.control_xfer(idx, "setcfg", setup_setcfg, 0).is_none() {
                crate::println!("[usb] xHCI {:04x}:{:04x} setcfg failed", vid, pid);
                return fail(self);
            }
            self.slots[idx].cfg = cfg;
            self.slots[idx].configured = true;
            crate::println!(
                "[usb] xHCI {:04x}:{:04x} HID keyboard configured (slot {})",
                vid,
                pid,
                slot_id
            );
            return Some(idx);
        }
        crate::println!("[usb] xHCI {:04x}:{:04x} class {} skipped", vid, pid, class);
        fail(self)
    }

    /// Evaluate Context to update EP0 maxpacket after the 8-byte probe.
    fn eval_maxpacket(&mut self, idx: usize, maxp: u8) -> bool {
        let esz = self.ctx_size();
        let (in_virt, in_phys, out_virt, out_phys, maxp_cur) = {
            let s = &self.slots[idx];
            (s.in_ctx_virt, s.in_ctx_phys, s.out_ctx_virt, s.out_ctx_phys, s.maxpacket0)
        };
        if maxp_cur == maxp {
            return true;
        }
        // Input slot context = copy of current output slot context.
        for i in 0..4 {
            let v = Self::ctx_r32(out_virt, (i * 4) as u64);
            Self::ctx_w32(in_virt, esz + (i * 4) as u64, v);
        }
        // Input EP0 context with the real maxpacket.
        Self::ctx_w32(in_virt, esz * 2, 0);
        Self::ctx_w32(
            in_virt,
            esz * 2 + 4,
            (EP_TYPE_CTRL << 3) | (3 << 1) | ((maxp as u32) << 16),
        );
        let (ep0_phys, ep0_idx, ep0_cycle) = {
            let s = &self.slots[idx];
            (s.ep0_phys, s.ep0_idx, s.ep0_cycle)
        };
        unsafe {
            core::ptr::write_volatile(
                (in_virt + esz * 2 + 8) as *mut u64,
                ep0_phys | (ep0_cycle & 0x1) as u64,
            );
        }
        let _ = ep0_idx;
        Self::ctx_w32(in_virt, esz * 2 + 16, 8);
        // Control: drop nothing, add EP0 only.
        Self::ctx_w32(in_virt, 0, 0);
        Self::ctx_w32(in_virt, 4, ADD_EP0);
        let slot_id = self.slots[idx].id;
        self.submit_cmd(
            (in_phys & 0xFFFF_FFFF) as u32,
            (in_phys >> 32) as u32,
            0,
            (slot_id << 24) | (TRB_EVAL_CTX << TRB_TYPE_SHIFT),
        );
        matches!(self.wait_completion(1000), Some((COMP_SUCCESS, _)))
    }

    /// Hub enumeration behind an addressed hub slot (one level).
    fn hub_flow(&mut self, idx: usize) -> bool {
        // SET_CONFIGURATION so the hub ports power on.
        let setup_setcfg = setup_packet(0x00, 9, 1, 0, 0);
        if self
            .control_xfer(idx, "hubsetcfg", setup_setcfg, 0)
            .is_none()
        {
            crate::println!("[usb] xHCI hub setcfg failed");
            return false;
        }
        // GET hub descriptor for the port count.
        let setup_hub = setup_packet(0xA0, 6, 0x2900, 0, 8);
        let Some(hubd) = self.control_xfer(idx, "hubdesc", setup_hub, 8) else {
            crate::println!("[usb] xHCI hub desc failed");
            return false;
        };
        if hubd.len() < 3 {
            return false;
        }
        let nports = hubd[2].min(8);
        crate::println!("[usb] xHCI hub ports={}", nports);
        let hub_slot = self.slots[idx].id;
        for hp in 1..=nports {
            // Power, then reset the hub port.
            let setup_pwr = setup_packet(0x23, 3, 8, hp as u16, 0);
            let _ = self.control_xfer(idx, "hubpwr", setup_pwr, 0);
            delay_ms(30);
            let setup_rst = setup_packet(0x23, 3, 4, hp as u16, 0);
            let _ = self.control_xfer(idx, "hubrst", setup_rst, 0);
            delay_ms(100);
            // Port status carries the speed (USB 2.0 hub: LS bit9, HS bit10).
            let setup_st = setup_packet(0xA3, 0, 0, hp as u16, 4);
            let Some(st) = self.control_xfer(idx, "hubstat", setup_st, 4) else {
                continue;
            };
            if st.len() < 2 || st[0] & 0x01 == 0 {
                continue; // nothing connected
            }
            let wps = (st[0] as u16) | ((st[1] as u16) << 8);
            let speed = if wps & HUB_PS_HS != 0 {
                SPEED_HS
            } else if wps & HUB_PS_LS != 0 {
                SPEED_LS
            } else {
                SPEED_FS
            };
            let root_port = self.slots[idx].port;
            // Route string: hub port in the low nibble (single hub level;
            // root ports are not counted â€” Linux usb_alloc_dev semantics).
            let route = (hp as u32).min(15);
            self.identify(root_port, speed, Some((hub_slot as u8, hp)), route, 1);
            if self.slots.iter().filter(|s| s.configured).count() >= 8 {
                break;
            }
        }
        true
    }

    /// One control transfer through a slot's EP0 ring. Returns received bytes
    /// (empty for OUT / no-data stages), or None on timeout/error.
    fn control_xfer(
        &mut self,
        idx: usize,
        stage: &str,
        setup: [u8; 8],
        in_len: u16,
    ) -> Option<Vec<u8>> {
        let dir_in = setup[0] & 0x80 != 0 && in_len > 0;
        let (ep0_virt, ep0_phys, maxp, slot_id) = {
            let s = &self.slots[idx];
            (s.ep0_virt, s.ep0_phys, s.maxpacket0, s.id)
        };
        let (data_virt, data_phys) = if in_len > 0 {
            let (dvirt, dphys, _) = dma_page()?;
            // Zeroed page doubles as the receive buffer.
            (dvirt, dphys)
        } else {
            (0, 0)
        };
        // Setup stage (setup bytes carried inline, IDT set, chained).
        let d0 = (setup[0] as u32)
            | ((setup[1] as u32) << 8)
            | ((setup[2] as u32) << 16)
            | ((setup[3] as u32) << 24);
        let d1 = (setup[4] as u32)
            | ((setup[5] as u32) << 8)
            | ((setup[6] as u32) << 16)
            | ((setup[7] as u32) << 24);
        let trt = if in_len == 0 {
            TRT_NONE
        } else if dir_in {
            TRT_IN
        } else {
            TRT_OUT
        };
        let mut cy = self.slots[idx].ep0_cycle;
        let mut ei = self.slots[idx].ep0_idx;
        Self::ring_put(
            ep0_virt,
            ep0_phys,
            &mut ei,
            &mut cy,
            d0,
            d1,
            8,
            TRB_CHAIN | TRB_IDT | (trt << 16) | (TRB_SETUP << TRB_TYPE_SHIFT),
        );
        // Data stage (single TRB; all Phase 2 reads fit one page).
        if in_len > 0 {
            let dir = if dir_in { TRB_DIR } else { 0 };
            Self::ring_put(
                ep0_virt,
                ep0_phys,
                &mut ei,
                &mut cy,
                (data_phys & 0xFFFF_FFFF) as u32,
                (data_phys >> 32) as u32,
                in_len as u32,
                TRB_CHAIN | dir | (TRB_DATA << TRB_TYPE_SHIFT),
            );
        }
        // Status stage (direction opposite the data stage, IOC set).
        let status_dir = if dir_in { 0 } else { TRB_DIR };
        let status_phys = Self::ring_put(
            ep0_virt,
            ep0_phys,
            &mut ei,
            &mut cy,
            0,
            0,
            0,
            status_dir | TRB_IOC | (TRB_STATUS << TRB_TYPE_SHIFT),
        );
        self.slots[idx].ep0_idx = ei;
        self.slots[idx].ep0_cycle = cy;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            mmio_w32(self.db, (slot_id as usize) * 4, 1); // ring EP0 (DCI 1)
        }
        let (code, residual) = self.wait_xfer(status_phys, 2000)?;
        if code != COMP_SUCCESS && !(dir_in && code == COMP_SHORT_PACKET) {
            crate::println!("[usb] xHCI {} failed (code {})", stage, code);
            return None;
        }
        if !dir_in {
            return Some(Vec::new());
        }
        let got = (in_len as u32).saturating_sub(residual & 0xFF_FFFF) as usize;
        let got = got.min(in_len as usize);
        let mut out = Vec::with_capacity(got);
        unsafe {
            let src = data_virt as *const u8;
            for i in 0..got {
                out.push(core::ptr::read_volatile(src.add(i)));
            }
        }
        Some(out)
    }

    /// Enqueue one transfer-ring TRB; returns its physical address.
    /// Same wrap discipline as the command ring: Link TRB (TC set) is
    /// followed unconditionally, both sides toggle cycle state each lap
    /// (Linux xhci-ring.c cycle rules), so rewrite it and wrap.
    fn ring_put(
        virt: u64,
        phys: u64,
        idx: &mut usize,
        cycle: &mut u32,
        d0: u32,
        d1: u32,
        d2: u32,
        d3: u32,
    ) -> u64 {
        if *idx == LINK_IDX {
            let link_lo = (phys & 0xFFFF_FFFF) as u32;
            let link_hi = (phys >> 32) as u32;
            unsafe {
                trb_write(
                    virt,
                    LINK_IDX,
                    [
                        link_lo,
                        link_hi,
                        0,
                        (*cycle & 0x1) | TRB_LINK_TC | (TRB_LINK << TRB_TYPE_SHIFT),
                    ],
                );
            }
            *cycle ^= 1;
            *idx = 0;
        }
        let addr = phys + (*idx as u64) * TRB_SIZE;
        unsafe {
            trb_write(virt, *idx, [d0, d1, d2, d3 | (*cycle & 0x1)]);
        }
        *idx += 1;
        addr
    }

    /// Wait for a Transfer Event without busy-spinning the virtual CPU.
    /// HLT allows asynchronous USB completion work to run in the emulator.
    fn wait_xfer(&mut self, status_phys: u64, timeout_ms: usize) -> Option<(u32, u32)> {
        // Same IRQ-independent scheme as wait_completion (see above): PIT
// channel 2 is the only clock that works with a dead legacy PIC.
        let max_iters = timeout_ms.saturating_mul(50_000).max(500_000);
        let mut iters = 0usize;

        loop {
            if let Some(r) = self.drain_xfer(status_phys) {
                return Some(r);
            }

            if iters >= max_iters {
                break;
            }
            crate::drivers::time_source::delay_ms(1);
            iters = iters.saturating_add(1);
        }

        self.drain_xfer(status_phys)
    }

    fn drain_xfer(&mut self, status_phys: u64) -> Option<(u32, u32)> {
        let mut found = None;
        let mut consumed = false;
        loop {
            let t = unsafe { trb_read(self.evt_virt, self.evt_idx) };
            if t[3] & TRB_CYCLE != self.evt_cycle {
                break;
            }
            consumed = true;
            let ty = (t[3] >> TRB_TYPE_SHIFT) & 0x3F;
            if ty == TRB_TRANSFER {
                let ptr = (t[0] as u64) | ((t[1] as u64) << 32);
                if ptr == status_phys {
                    found = Some(((t[2] >> 24) & 0xFF, t[2] & 0xFF_FFFF));
                }
            }
            self.evt_idx += 1;
            if self.evt_idx == RING_TRBS {
                self.evt_idx = 0;
                self.evt_cycle ^= 1;
            }
        }
        if consumed {
            self.sync_events();
        }
        found
    }

    /// Advance ERDP past consumed events, clear IP/EINT (shared helper).
    fn sync_events(&mut self) {
        unsafe {
            mmio_w64(
                self.ir0,
                IR_ERDP,
                self.evt_phys + (self.evt_idx as u64) * TRB_SIZE | ERDP_EHB,
            );
            mmio_w32(self.ir0, IR_IMAN, IMAN_IP);
            mmio_w32(self.op, 0x04, 1 << 3);
        }
    }

    // -----------------------------------------------------------------------
    // Phase 3: HID boot-keyboard claiming + interrupt-IN polling
    // -----------------------------------------------------------------------

    /// Number of claimed HID keyboards on this controller.
    pub fn hid_keyboard_count(&self) -> usize {
        self.slots.iter().filter(|s| s.hid_claimed).count()
    }

    /// Claim all configured HID keyboards: SET_PROTOCOL + SET_IDLE +
    /// Configure Endpoint for the interrupt-IN endpoint, then prime it.
    /// Called once after `enumerate()`; safe to call again (skips claimed).
    pub fn claim_hid_keyboards(&mut self) {
        let nslots = self.slots.len();
        for idx in 0..nslots {
            // Snapshot what we need without holding a borrow across transfers.
            let (configured, claimed, vid, pid, slot_id) = {
                let s = &self.slots[idx];
                (s.configured, s.hid_claimed, s.vid, s.pid, s.id)
            };
            if !configured || claimed {
                continue;
            }
            let (iface, ep_addr, max_raw) = {
                let s = &self.slots[idx];
                if s.cfg.is_empty() {
                    continue;
                }
                match find_hid_keyboard(&s.cfg) {
                    Some(v) => v,
                    None => continue,
                }
            };
            let binterval = {
                let s = &self.slots[idx];
                Self::hid_binterval(&s.cfg, ep_addr)
            };
            // SET_PROTOCOL boot (0) â€” like EHCI; continue on fail.
            let setup_proto = setup_packet(0x21, 0x0B, 0, iface as u16, 0);
            if self.control_xfer(idx, "hid-proto", setup_proto, 0).is_none() {
                crate::println!(
                    "[usb] xHCI {:04x}:{:04x} SET_PROTOCOL failed, continuing",
                    vid,
                    pid
                );
            }
            // SET_IDLE (0) â€” QEMU accepts; ignore failure.
            let setup_idle = setup_packet(0x21, 0x0A, 0, iface as u16, 0);
            let _ = self.control_xfer(idx, "hid-idle", setup_idle, 0);

            if self.configure_interrupt_ep(idx, iface, ep_addr, max_raw, binterval) {
                crate::println!(
                    "[usb] xHCI {:04x}:{:04x} HID keyboard claimed (slot {}, ep {:#x})",
                    vid,
                    pid,
                    slot_id,
                    ep_addr
                );
            } else {
                crate::println!(
                    "[usb] xHCI {:04x}:{:04x} HID claim failed (slot {})",
                    vid,
                    pid,
                    slot_id
                );
            }
        }
    }

    /// Find bInterval for `ep_addr` in a config descriptor. Defaults to 8ms.
    fn hid_binterval(cfg: &[u8], ep_addr: u8) -> u8 {
        let mut i = 0;
        while i + 2 <= cfg.len() {
            let len = cfg[i] as usize;
            let dtype = cfg[i + 1];
            if len == 0 || i + len > cfg.len() {
                break;
            }
            if dtype == 5 && len >= 7 && cfg[i + 2] == ep_addr {
                let v = cfg[i + 6];
                if v != 0 {
                    return v;
                }
            }
            i += len;
        }
        8
    }

    /// xHCI Interval field (125us units exponent) for an interrupt endpoint.
    fn xhci_interval(speed: u32, binterval: u8) -> u32 {
        if speed == SPEED_HS || speed == SPEED_SS {
            // HS/SS interrupt: Period = 2^(bInterval-1) microframes.
            let b = (binterval as u32).clamp(1, 16);
            b - 1
        } else {
            // FS/LS interrupt: bInterval in frames -> microframes, log2.
            let frames = (binterval as u32).max(1).min(255);
            let uframes = frames.saturating_mul(8).max(8);
            // floor(log2(ufames))
            let mut exp = 32 - uframes.leading_zeros() - 1;
            if exp < 3 {
                exp = 3;
            }
            if exp > 10 {
                exp = 10;
            }
            exp
        }
    }

    /// Configure the interrupt-IN endpoint via Configure Endpoint command,
    /// then prime it with one interrupt-IN transfer. Returns true on success.
    fn configure_interrupt_ep(
        &mut self,
        idx: usize,
        _iface: u8,
        ep_addr: u8,
        max_raw: u16,
        binterval: u8,
    ) -> bool {
        let ep_num = (ep_addr & 0x0F) as u32;
        let dir_in = ep_addr & 0x80 != 0;
        if !dir_in || ep_num == 0 || ep_num > 15 {
            crate::serial_println!(
                "[usb] xHCI HID bad ep {:#x} slot {}",
                ep_addr,
                self.slots[idx].id
            );
            crate::println!("[usb] xHCI HID bad ep {:#x}", ep_addr);
            return false;
        }
        let dci = ep_num * 2 + 1;
        if dci < 2 || dci > 31 {
            return false;
        }
        let (slot_id, speed, in_virt, in_phys, out_virt) = {
            let s = &self.slots[idx];
            (
                s.id,
                s.speed,
                s.in_ctx_virt,
                s.in_ctx_phys,
                s.out_ctx_virt,
            )
        };
        // Allocate interrupt transfer ring + HID report buffer (leaked).
        let (int_virt, int_phys, _) = match dma_page() {
            Some(v) => v,
            None => {
                crate::serial_println!("[usb] xHCI HID ring DMA fail");
                crate::println!("[usb] xHCI HID ring DMA fail");
                return false;
            }
        };
        let (buf_virt, buf_phys, _) = match dma_page() {
            Some(v) => v,
            None => {
                crate::serial_println!("[usb] xHCI HID buf DMA fail");
                crate::println!("[usb] xHCI HID buf DMA fail");
                return false;
            }
        };
        unsafe {
            trb_write(
                int_virt,
                LINK_IDX,
                [
                    (int_phys & 0xFFFF_FFFF) as u32,
                    (int_phys >> 32) as u32,
                    0,
                    TRB_CYCLE | TRB_LINK_TC | (TRB_LINK << TRB_TYPE_SHIFT),
                ],
            );
        }
        let esz = self.ctx_size();
        let maxpacket = ((max_raw & 0x7FF).max(8)) as u32;
        let max_burst = if speed == SPEED_HS {
            ((max_raw >> 11) & 0x3) as u32
        } else {
            0
        };
        let max_esit = maxpacket.saturating_mul(max_burst + 1).max(8).min(1024);
        let interval = Self::xhci_interval(speed, binterval);

        // Build Input Context: zero, copy Slot + EP0, add new EP.
        unsafe {
            core::ptr::write_bytes(in_virt as *mut u8, 0, 4096);
        }
        let dwords = (esz / 4) as usize;
        for i in 0..dwords {
            let v = Self::ctx_r32(out_virt, (i * 4) as u64);
            Self::ctx_w32(in_virt, esz + (i * 4) as u64, v);
        }
        // Update Last Context to include the new DCI.
        {
            let dev_info = Self::ctx_r32(in_virt, esz);
            let old_last = (dev_info >> SLOT_LAST_CTX_SHIFT) & 0x1F;
            let new_last = old_last.max(dci);
            let dev_info = (dev_info & !(0x1F << SLOT_LAST_CTX_SHIFT))
                | (new_last << SLOT_LAST_CTX_SHIFT);
            Self::ctx_w32(in_virt, esz, dev_info);
        }
        // Copy EP0 context (output DCI1 at esz) -> input (esz*2).
        for i in 0..dwords {
            let v = Self::ctx_r32(out_virt, esz + (i * 4) as u64);
            Self::ctx_w32(in_virt, esz * 2 + (i * 4) as u64, v);
        }
        // New endpoint context at input offset (DCI+1)*esz.
        let ep_off = esz * (dci as u64 + 1);
        Self::ctx_w32(in_virt, ep_off, interval << 16);
        Self::ctx_w32(
            in_virt,
            ep_off + 4,
            (EP_TYPE_INT_IN << 3) | (3 << 1) | (max_burst << 8) | (maxpacket << 16),
        );
        unsafe {
            core::ptr::write_volatile((in_virt + ep_off + 8) as *mut u64, int_phys | 1);
        }
        Self::ctx_w32(
            in_virt,
            ep_off + 16,
            (max_esit & 0xFFFF) | ((max_esit & 0xFFFF) << 16),
        );
        // Control: drop nothing, add Slot + new endpoint.
        // QEMU hcd-xhci.c xhci_configure_slot requires Add[1:0]==01
        // (Slot set, EP0 clear) or it returns CC_TRB_ERROR. Linux sets
        // SLOT_FLAG | new_ep_flag; omitting SLOT was the CLAIM FAILED cause.
        Self::ctx_w32(in_virt, 0, 0);
        Self::ctx_w32(in_virt, 4, (1 << dci) | ADD_SLOT);

        self.submit_cmd(
            (in_phys & 0xFFFF_FFFF) as u32,
            (in_phys >> 32) as u32,
            0,
            (slot_id << 24) | (TRB_CONFIG_EP << TRB_TYPE_SHIFT),
        );
        match self.wait_completion(1000) {
            Some((COMP_SUCCESS, _)) => {}
            Some((code, _)) => {
                crate::serial_println!(
                    "[usb] xHCI HID config ep failed code {} slot {} dci {} add {:#x}",
                    code,
                    slot_id,
                    dci,
                    (1u32 << dci) | ADD_SLOT
                );
                crate::println!(
                    "[usb] xHCI HID config failed code {} slot {}",
                    code,
                    slot_id
                );
                return false;
            }
            None => {
                crate::serial_println!("[usb] xHCI HID config ep timeout slot {}", slot_id);
                crate::println!("[usb] xHCI HID config timeout slot {}", slot_id);
                return false;
            }
        }
        // Publish ring + buffer in the slot, then prime.
        {
            let s = &mut self.slots[idx];
            s.hid_ep = ep_addr;
            s.hid_dci = dci;
            s.hid_maxpacket = maxpacket as u16;
            s.hid_interval = binterval;
            s.int_virt = int_virt;
            s.int_phys = int_phys;
            s.int_idx = 0;
            s.int_cycle = 1;
            s.hid_buf_virt = buf_virt;
            s.hid_buf_phys = buf_phys;
            s.hid_pending_trb = 0;
            s.hid_claimed = true;
        }
        self.prime_interrupt(idx);
        self.slots[idx].hid_claimed
    }

    /// Enqueue one interrupt-IN Normal TRB and ring the endpoint doorbell.
    fn prime_interrupt(&mut self, idx: usize) -> bool {
        let (int_virt, int_phys, buf_phys, slot_id, dci) = {
            let s = &self.slots[idx];
            if !s.hid_claimed || s.int_virt == 0 || s.hid_buf_phys == 0 {
                return false;
            }
            (s.int_virt, s.int_phys, s.hid_buf_phys, s.id, s.hid_dci)
        };
        let (mut ei, mut cy) = {
            let s = &self.slots[idx];
            (s.int_idx, s.int_cycle)
        };
        let trb_phys = Self::ring_put(
            int_virt,
            int_phys,
            &mut ei,
            &mut cy,
            (buf_phys & 0xFFFF_FFFF) as u32,
            (buf_phys >> 32) as u32,
            8,
            TRB_IOC | (TRB_NORMAL << TRB_TYPE_SHIFT),
        );
        {
            let s = &mut self.slots[idx];
            s.int_idx = ei;
            s.int_cycle = cy;
            s.hid_pending_trb = trb_phys;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            mmio_w32(self.db, (slot_id as usize) * 4, dci);
        }
        true
    }
}

