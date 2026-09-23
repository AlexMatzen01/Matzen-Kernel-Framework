//! USB host-controller integration (EHCI-first for A1466 built-in keyboard).
//!
//! Polling-only design: `init()` brings up EHCI controllers, enumerates
//! ports/hubs, claims HID boot keyboards, and links interrupt QHs into the
//! periodic schedule. `poll()` checks completed interrupt transfers and
//! pushes HID reports via `keyboard::push_usb_report()`.
//!
//! All waits are bounded so missing hardware cannot hang boot. Failures are
//! logged to framebuffer/serial for Mac photo diagnostics.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::drivers::pci::{PciDevice, PCI_VENDOR_INTEL};
use crate::drivers::xhci::XhciController;

// ---------------------------------------------------------------------------
// Physical memory offset + DMA helpers
// ---------------------------------------------------------------------------

static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

fn phys_offset() -> u64 {
    PHYS_OFFSET.load(Ordering::Relaxed)
}

fn translate_virt_to_phys(virt: u64) -> Option<u64> {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};
    use x86_64::VirtAddr;

    let offset = phys_offset();
    if offset == 0 {
        return None;
    }
    let (frame, _) = Cr3::read();
    let p4_virt = VirtAddr::new(offset.wrapping_add(frame.start_address().as_u64()));
    let page_table_ptr = p4_virt.as_mut_ptr() as *mut PageTable;
    let page_table = unsafe { &mut *page_table_ptr };
    let mapper = unsafe { OffsetPageTable::new(page_table, VirtAddr::new(offset)) };
    mapper
        .translate_addr(VirtAddr::new(virt))
        .map(|p| p.as_u64())
}

fn virt_to_phys_dma(virt: u64) -> Option<u64> {
    if let Some(phys) = translate_virt_to_phys(virt) {
        if phys < 0x1_0000_0000 {
            return Some(phys);
        }
        crate::serial_println!("[usb] DMA phys {:#x} >= 4GB, EHCI cannot reach", phys);
        return None;
    }
    // Legacy fallback: virt - offset (only valid for direct-mapped heap).
    let off = phys_offset();
    if off != 0 && virt >= off {
        let phys = virt - off;
        if phys < 0x1_0000_0000 {
            return Some(phys);
        }
    }
    None
}

/// Allocates a zeroed 4K page for DMA. Leaked for driver lifetime.
pub(crate) fn dma_page() -> Option<(u64, u64, *mut u8)> {
    use alloc::alloc::{alloc_zeroed, Layout};
    let layout = Layout::from_size_align(4096, 4096).ok()?;
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        crate::serial_println!("[usb] dma_page alloc failed");
        return None;
    }
    let virt = ptr as u64;
    let phys = virt_to_phys_dma(virt)?;
    // Ensure single-page (no cross): page-aligned base guarantees it.
    Some((virt, phys, ptr))
}

pub(crate) fn delay_ms(ms: usize) {
    if ms == 0 {
        return;
    }
    // Real-time via PIT channel 2: polled, IRQ-independent, fail-open when
    // no PIT decodes the ports. The old time+spin scheme ran ~35-1000x too
    // long on UEFI/APIC machines with a dead legacy PIC tick (proven 0->0),
    // stretching USB bring-up into minutes. Channel 2 is strictly
    // sequential-use (never inside a pit::Timeout window), so sharing is safe.
    crate::drivers::pit::sleep_ms(ms.min(u32::MAX as usize) as u32);
}

// ---------------------------------------------------------------------------
// MMIO helpers
// ---------------------------------------------------------------------------

#[inline]
pub(crate) unsafe fn mmio_r8(base: usize, off: usize) -> u8 {
    // Offsets are register indices (<4K). A huge `off` means a caller mixed
    // up base/offset (once panicked as `attempt to add with overflow` here).
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    core::ptr::read_volatile((base + off) as *const u8)
}

#[inline]
pub(crate) unsafe fn mmio_r32(base: usize, off: usize) -> u32 {
    // Offsets are register indices (<4K). A huge `off` means a caller mixed
    // up base/offset (once panicked as `attempt to add with overflow` here).
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    core::ptr::read_volatile((base + off) as *const u32)
}

/// 16-bit MMIO read for halfword registers (e.g. HCIVERSION at offset 0x02,
/// which a u32 read would fault on: base+2 is never 4-aligned).
#[inline]
pub(crate) unsafe fn mmio_r16(base: usize, off: usize) -> u16 {
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    core::ptr::read_volatile((base + off) as *const u16)
}

#[inline]
pub(crate) unsafe fn mmio_w32(base: usize, off: usize, val: u32) {
    // Offsets are register indices (<4K). A huge `off` means a caller mixed
    // up base/offset (once panicked as `attempt to add with overflow` here).
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    core::ptr::write_volatile((base + off) as *mut u32, val);
}

/// 64-bit xHCI MMIO write. The controller latches these register pairs when
/// the low dword is written, so publish the low dword before the high dword.
#[inline]
pub(crate) unsafe fn mmio_w64(base: usize, off: usize, val: u64) {
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    core::ptr::write_volatile((base + off) as *mut u32, val as u32);
    core::ptr::write_volatile((base + off + 4) as *mut u32, (val >> 32) as u32);
}

/// 64-bit MMIO read, low dword first.
#[inline]
pub(crate) unsafe fn mmio_r64(base: usize, off: usize) -> u64 {
    debug_assert!(off < 0x10_000, "USB MMIO offset out of range");
    let lo = core::ptr::read_volatile((base + off) as *const u32) as u64;
    let hi = core::ptr::read_volatile((base + off + 4) as *const u32) as u64;
    lo | (hi << 32)
}

// EHCI cap regs
const CAP_CAPLENGTH: usize = 0x00;
const CAP_HCIVERSION: usize = 0x02;
const CAP_HCSPARAMS: usize = 0x04;
const CAP_HCCPARAMS: usize = 0x08;

// EHCI op regs (relative to opbase = mmio + caplength)
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_USBINTR: usize = 0x08;
const OP_FRINDEX: usize = 0x0C;
const OP_PERIODICLISTBASE: usize = 0x14;
const OP_ASYNCLISTADDR: usize = 0x18;
const OP_CONFIGFLAG: usize = 0x40;
const OP_PORTSC_BASE: usize = 0x44;

// USBCMD bits
const CMD_RS: u32 = 1 << 0;
const CMD_HCRESET: u32 = 1 << 1;
const CMD_PSE: u32 = 1 << 4;
const CMD_ASE: u32 = 1 << 5;

// USBSTS bits
const STS_HCHALTED: u32 = 1 << 12;

// PORTSC bits
const PORT_CONNECTED: u32 = 1 << 0;
const PORT_ENABLED: u32 = 1 << 2;
const PORT_ENABLE_CHANGE: u32 = 1 << 3;
const PORT_RESET: u32 = 1 << 8;
const PORT_POWER: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
// W1C change bits (CSC bit1, PEC bit3, OCC bit5).
const PORT_CHANGE_BITS: u32 = (1 << 1) | (1 << 3) | (1 << 5);

const TERM: u32 = 0x0000_0001;
const QH_TYPE: u32 = 0x0000_0002; // bits 2:1 = 01 QH

// qTD PID
const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

// qTD token bits
const TOK_ACTIVE: u32 = 1 << 7;
const TOK_HALTED: u32 = 1 << 6;
const TOK_BUFERR: u32 = 1 << 5;
const TOK_BABBLE: u32 = 1 << 4;
const TOK_XACTERR: u32 = 1 << 3;
const TOK_IOC: u32 = 1 << 15;

// ---------------------------------------------------------------------------
// DMA structures
// ---------------------------------------------------------------------------

#[repr(C, align(32))]
struct Qh {
    hlink: u32,
    ep_chars: u32,
    ep_caps: u32,
    cur: u32,
    next: u32,
    alt: u32,
    token: u32,
    buf0: u32,
    buf1: u32,
    buf2: u32,
    buf3: u32,
    buf4: u32,
}

#[repr(C, align(32))]
struct Qtd {
    next: u32,
    alt: u32,
    token: u32,
    buf0: u32,
    buf1: u32,
    buf2: u32,
    buf3: u32,
    buf4: u32,
}

/// One claimed HID interrupt endpoint (keyboard or boot mouse). The poll
/// loops route by which vec owns the entry.
struct HidEp {
    addr: u8,
    /// Root port this endpoint was claimed on (for hotplug rescan tracking).
    port: u8,
    qh_virt: u64,
    qh_phys: u64,
    qtd_virt: u64,
    qtd_phys: u64,
    buf_virt: u64,
    buf_phys: u64,
    maxpacket: u16,
}

unsafe impl Send for HidEp {}

struct EhciController {
    pci_bus: u8,
    pci_dev: u8,
    pci_func: u8,
    mmio_virt: usize,
    op_base: usize,
    n_ports: u8,
    frame_virt: u64,
    frame_phys: u64,
    frame_ptr: *mut u32,
    async_qh_virt: u64,
    async_qh_phys: u64,
    next_addr: u8,
    keyboards: Vec<HidEp>,
    /// Claimed boot-mouse endpoints (polled like keyboards, parsed as mice).
    mice: Vec<HidEp>,
}

unsafe impl Send for EhciController {}

struct UsbState {
    controllers: Vec<EhciController>,
    xhci: Vec<XhciController>,
    ready: bool,
}

lazy_static! {
    static ref STATE: Mutex<UsbState> = Mutex::new(UsbState {
        controllers: Vec::new(),
        xhci: Vec::new(),
        ready: false,
    });
}

/// Last hotplug-rescan time (monotonic ms). Separate atomic so poll() stays
/// cheap when no rescan is due.
static LAST_RESCAN_MS: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Vendor-safe Intel PCH USB2 port routing
// ---------------------------------------------------------------------------

/// Early Intel PCH ownership handoff (delegates to PCI layer).
/// Runs BEFORE any xHCI MMIO/ring init so EHCI is stripped of USB2 ports
/// at millisecond zero. Non-Intel controllers bypass quirks entirely.
/// Returns true if ports were re-routed (caller settles with 300ms).
fn route_intel_pch_usb2_to_xhci(devices: &[PciDevice]) -> bool {
    // Visible skip log per non-Intel xHCI (pci helper skips silently).
    for xhci in devices.iter().filter(|d| d.is_xhci()) {
        let vendor = xhci.reread_vendor_id();
        if vendor != PCI_VENDOR_INTEL {
            crate::println!(
                "[usb] xHCI {:02x}:{:02x}.{} {:04x}:{:04x} non-Intel, skipping PCH port mux",
                xhci.bus,
                xhci.device,
                xhci.function,
                vendor,
                xhci.device_id,
            );
        }
    }
    crate::drivers::pci::claim_intel_xhci_ports(devices)
}

/// Initializes USB controller discovery without assuming a particular PCI ID.
pub fn init(phys_mem_offset: u64) {
    PHYS_OFFSET.store(phys_mem_offset, Ordering::Relaxed);

    let devices = crate::drivers::pci::enumerate_devices();
    let mut usb_total = 0;
    for device in devices
        .iter()
        .filter(|d| d.class_code == 0x0c && d.subclass == 0x03)
    {
        usb_total += 1;
        let kind = match device.prog_if {
            0x20 => "EHCI",
            0x30 => "xHCI",
            _ => "USB",
        };
        crate::println!(
            "[usb] {} {:04x}:{:04x} MMIO={:#x}",
            kind,
            device.vendor_id,
            device.device_id,
            device.mmio_base().unwrap_or(0)
        );
    }
    crate::println!("[usb] {} controller(s) found", usb_total);

    // PIT-tick proof: IRQ0-driven `time::ticks()` must advance across a
    // polled channel-2 wait. On UEFI/APIC machines with a dead legacy PIC,
    // ticks stay frozen and every `uptime_millis`+`hlt` wait would hang.
    // This line makes that visible on VGA without serial capture.
    {
        let t0 = crate::time::ticks();
        crate::drivers::pit::sleep_ms(50);
        let t1 = crate::time::ticks();
        crate::println!(
            "[usb] PIT tick check: {} -> {} (IRQs {})",
            t0,
            t1,
            if x86_64::instructions::interrupts::are_enabled() {
                "on"
            } else {
                "off"
            }
        );
    }

    // Vendor-safe Intel PCH step: only Intel xHCI devices are touched
    // (fresh VID check inside); AMD/VIA/ASMedia/QEMU skip the legacy USB2
    // mux entirely and fall through to standard xHCI bring-up below.
    // Non-Intel controllers must never see XUSB2PR/USB2PRM accesses.
    // Intel Panther Point: force switchable USB2 ports to xHCI so the
    // keyboard is claimed by the xHCI root hub.
    if route_intel_pch_usb2_to_xhci(&devices) {
        // Post-mux settle: slower legacy FS microcontrollers need ample
        // time to stabilize transceivers after a mux flip + root-port
        // reset. Strictly conditional: no reroute -> no delay (fast AMD boot).
        delay_ms(300);
    }

    // Bring up EHCI controllers first (built-in keyboard path on A1466).
    let ehci_devs: Vec<(u8, u8, u8)> = devices
        .iter()
        .filter(|d| d.class_code == 0x0c && d.subclass == 0x03 && d.prog_if == 0x20)
        .map(|d| (d.bus, d.device, d.function))
        .collect();

    // xHCI controllers (any vendor: standardized registers). Modern machines
    // route USB2/USB3 ports here, so this is the primary keyboard path.
    let xhci_devs: Vec<(u8, u8, u8)> = devices
        .iter()
        .filter(|d| d.class_code == 0x0c && d.subclass == 0x03 && d.prog_if == 0x30)
        .map(|d| (d.bus, d.device, d.function))
        .collect();

    if ehci_devs.is_empty() && xhci_devs.is_empty() {
        crate::println!("[usb] no USB controllers, HID transport pending");
        return;
    }

    // Re-lookup full PciDevice by location (enumerate again is cheap enough once).
    let all = crate::drivers::pci::enumerate_devices();
    for (bus, dev, func) in ehci_devs {
        let pci = all
            .iter()
            .find(|d| d.bus == bus && d.device == dev && d.function == func);
        let Some(pci) = pci else { continue };
        // Reconstruct owned copy via fields (PciDevice is not Clone).
        let dev_copy = crate::drivers::pci::PciDevice {
            bus: pci.bus,
            device: pci.device,
            function: pci.function,
            vendor_id: pci.vendor_id,
            device_id: pci.device_id,
            class_code: pci.class_code,
            subclass: pci.subclass,
            prog_if: pci.prog_if,
            bar0: pci.bar0,
            bar1: pci.bar1,
            irq_line: pci.irq_line,
        };
        match EhciController::new(dev_copy, phys_mem_offset) {
            Ok(mut ctl) => {
                ctl.enumerate_all();
                let kbd = ctl.keyboards.len();
                let ptr = ctl.mice.len();
                crate::println!(
                    "[usb] EHCI {:02x}:{:02x}.{} ready, {} keyboard(s), {} mouse(s)",
                    bus,
                    dev,
                    func,
                    kbd,
                    ptr
                );
                STATE.lock().controllers.push(ctl);
            }
            Err(e) => {
                crate::println!(
                    "[usb] EHCI {:02x}:{:02x}.{} init failed: {}",
                    bus,
                    dev,
                    func,
                    e
                );
            }
        }
    }

    let ehci_kbd: usize = STATE
        .lock()
        .controllers
        .iter()
        .map(|c| c.keyboards.len())
        .sum();
    let ehci_ptr: usize = STATE.lock().controllers.iter().map(|c| c.mice.len()).sum();

    // Bring up xHCI controllers (Phase 1: running + proven rings, Phase 2:
    // enumeration, Phase 3: HID claiming + interrupt-IN polling).
    for (bus, dev, func) in xhci_devs {
        let pci = all
            .iter()
            .find(|d| d.bus == bus && d.device == dev && d.function == func);
        let Some(pci) = pci else { continue };
        let dev_copy = crate::drivers::pci::PciDevice {
            bus: pci.bus,
            device: pci.device,
            function: pci.function,
            vendor_id: pci.vendor_id,
            device_id: pci.device_id,
            class_code: pci.class_code,
            subclass: pci.subclass,
            prog_if: pci.prog_if,
            bar0: pci.bar0,
            bar1: pci.bar1,
            irq_line: pci.irq_line,
        };
        match XhciController::new(dev_copy, phys_mem_offset) {
            Ok(mut ctl) => {
                // Phase 2: enumerate ports to configured devices, then
                // Phase 3: claim HID keyboards + mice/tablets and prime
                // interrupt endpoints.
                ctl.enumerate();
                ctl.claim_hid_keyboards();
                ctl.claim_hid_pointers();
                let claimed = ctl.hid_keyboard_count();
                let pointers = ctl.hid_pointer_count();
                crate::println!(
                    "[usb] xHCI {:02x}:{:02x}.{} running, polling ({} HID kbd, {} HID ptr)",
                    bus,
                    dev,
                    func,
                    claimed,
                    pointers
                );
                STATE.lock().xhci.push(ctl);
            }
            Err(e) => {
                crate::println!(
                    "[usb] xHCI {:02x}:{:02x}.{} init failed: {}",
                    bus,
                    dev,
                    func,
                    e
                );
            }
        }
    }

    let xhci_kbd: usize = STATE
        .lock()
        .xhci
        .iter()
        .map(|c| c.hid_keyboard_count())
        .sum();
    let total_kbd = ehci_kbd + xhci_kbd;
    let total_ptr = hid_pointer_count();
    if total_kbd == 0 && total_ptr == 0 {
        crate::println!("[usb] HID transport pending (no boot HID devices claimed)");
    } else {
        crate::println!(
            "[usb] HID ready: {} keyboard(s), {} pointer(s), polling",
            total_kbd,
            total_ptr
        );
    }
    STATE.lock().ready = true;
}

/// Physical address for DMA, or `None` when unreachable (>= 4 GiB).
pub fn virt_to_phys_for_dma(virt: u64) -> Option<u64> {
    virt_to_phys_dma(virt)
}

/// USB controller probe report, printed to the display AND serial
/// (`println!` mirrors to both). Lists every PCI USB controller by type
/// (UHCI/OHCI/EHCI/xHCI via prog-if) plus live driver status.
pub fn probe_report() {
    let devices = crate::drivers::pci::enumerate_devices();
    let mut uhci = 0;
    let mut ohci = 0;
    let mut ehci = 0;
    let mut xhci = 0;
    for d in devices
        .iter()
        .filter(|d| d.class_code == 0x0c && d.subclass == 0x03)
    {
        let kind = match d.prog_if {
            0x00 => {
                uhci += 1;
                "UHCI"
            }
            0x10 => {
                ohci += 1;
                "OHCI"
            }
            0x20 => {
                ehci += 1;
                "EHCI"
            }
            0x30 => {
                xhci += 1;
                "xHCI"
            }
            _ => "USB?",
        };
        // Raw BARs + COMMAND are config-space reads (always safe) and tell
        // firmware-assigned addresses apart from decode problems.
        let bar0 = d.read_config(0x10);
        let bar1 = d.read_config(0x14);
        let cmd = d.read_config(0x04);
        crate::println!(
            "[usb] {:<4} {:02x}:{:02x}.{} {:04x}:{:04x} MMIO={:#x} IRQ={} BAR0={:#x} BAR1={:#x} CMD={:#x}",
            kind,
            d.bus,
            d.device,
            d.function,
            d.vendor_id,
            d.device_id,
            d.mmio_base().unwrap_or(0),
            d.irq_line,
            bar0,
            bar1,
            cmd,
        );
    }
    if uhci + ohci + ehci + xhci == 0 {
        crate::println!("[usb] no PCI USB controllers found");
    }
    if uhci > 0 {
        crate::println!(
            "[usb] UHCI x{}: no kernel driver (unsupported) - PS/2 fallback",
            uhci
        );
    }
    if ohci > 0 {
        crate::println!(
            "[usb] OHCI x{}: no kernel driver (unsupported) - PS/2 fallback",
            ohci
        );
    }
    {
        let state = STATE.lock();
        let ehci_kbd: usize = state.controllers.iter().map(|c| c.keyboards.len()).sum();
        let ehci_ptr: usize = state.controllers.iter().map(|c| c.mice.len()).sum();
        let xhci_kbd: usize = state.xhci.iter().map(|c| c.hid_keyboard_count()).sum();
        let xhci_ptr: usize = state.xhci.iter().map(|c| c.hid_pointer_count()).sum();
        crate::println!(
            "[usb] status: EHCI x{} ({} kbd, {} ptr), xHCI x{} ({} kbd, {} ptr), PS/2 kbd active",
            state.controllers.len(),
            ehci_kbd,
            ehci_ptr,
            state.xhci.len(),
            xhci_kbd,
            xhci_ptr,
        );
    }
    {
        let present = crate::drivers::mouse::is_present();
        let (mx, my) = crate::drivers::mouse::position();
        let (usb_mice, usb_tablets) = crate::drivers::mouse::mouse_usb_stats();
        crate::println!(
            "[input] mouse: {} at ({}, {}), buttons {:#05b} (PS/2 + USB: {} rel + {} abs reports)",
            if present { "ready" } else { "absent" },
            mx,
            my,
            crate::drivers::mouse::buttons(),
            usb_mice,
            usb_tablets,
        );
    }
}

/// Number of claimed USB HID keyboards (EHCI + xHCI).
pub fn hid_keyboard_count() -> usize {
    let state = STATE.lock();
    state
        .controllers
        .iter()
        .map(|c| c.keyboards.len())
        .sum::<usize>()
        + state
            .xhci
            .iter()
            .map(|c| c.hid_keyboard_count())
            .sum::<usize>()
}

/// Number of claimed USB HID pointers (EHCI boot mice + xHCI mice/tablets).
pub fn hid_pointer_count() -> usize {
    let state = STATE.lock();
    state
        .controllers
        .iter()
        .map(|c| c.mice.len())
        .sum::<usize>()
        + state
            .xhci
            .iter()
            .map(|c| c.hid_pointer_count())
            .sum::<usize>()
}

/// Number of live controllers by type (ehci, xhci).
pub fn controller_counts() -> (usize, usize) {
    let state = STATE.lock();
    (state.controllers.len(), state.xhci.len())
}

/// Polls completed USB transfers. Called from shell loop; never blocks long.
pub fn poll() {
    // Fast path: avoid locking when no controllers.
    let has_usb = {
        let state = STATE.lock();
        state.controllers.is_empty() && state.xhci.is_empty()
    };
    if has_usb {
        return;
    }
    let mut state = STATE.lock();
    for ctl in state.controllers.iter_mut() {
        ctl.poll_keyboards();
        ctl.poll_mice();
    }
    for ctl in state.xhci.iter_mut() {
        ctl.poll();
    }
    // Periodic hotplug rescan: the boot scan is one-shot, so replugged or
    // slow-to-appear devices would otherwise never be seen. Throttled; the
    // per-port change bit (EHCI CSC / xHCI PORT_CSC) keeps it quiet when
    // nothing changed. xHCI event-ring port-change events are additionally
    // serviced inline in XhciController::poll() for low latency.
    if crate::time::is_initialized() {
        let now = crate::time::uptime_millis();
        if now.wrapping_sub(LAST_RESCAN_MS.load(Ordering::Relaxed)) >= 2000 {
            LAST_RESCAN_MS.store(now, Ordering::Relaxed);
            for ctl in state.controllers.iter_mut() {
                ctl.rescan_ports();
            }
            for ctl in state.xhci.iter_mut() {
                ctl.rescan_ports();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EHCI controller
// ---------------------------------------------------------------------------

impl EhciController {
    fn new(pci: crate::drivers::pci::PciDevice, phys_offset: u64) -> Result<Self, &'static str> {
        pci.enable_bus_mastering();
        // VGA photo proof of power state (Apple EFI boot, no serial).
        match pci.pm_info() {
            Some((cap, st)) => crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} PM cap {:#x} state D{}",
                pci.bus,
                pci.device,
                pci.function,
                cap,
                st
            ),
            None => crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} no PM cap",
                pci.bus,
                pci.device,
                pci.function
            ),
        }

        let mmio_phys = pci.mmio_base().ok_or("EHCI BAR not memory")?;
        if mmio_phys == 0 {
            return Err("EHCI BAR zero");
        }
        // Fixed non-destructive mapping (caps + op + ≤16 ports); failures
        // return Err so the VGA "init failed" line fires instead of stalling.
        let mmio_virt = crate::drivers::pci::map_mmio_region(
            mmio_phys,
            0x4000,
            phys_offset,
            &mut crate::memory::frame_allocator::frame_allocator(),
        )
        .ok_or("EHCI MMIO map failed")?;

        // Ensure PCI COMMAND memory-space enable (bit 1) so MMIO reads work.
        // Firmware normally sets it; harmless if already set.
        let cmd = pci.read_config(0x04);
        if cmd & 0x02 == 0 {
            pci.write_config(0x04, cmd | 0x02);
            crate::serial_println!(
                "[usb] EHCI {:02x}:{:02x}.{} COMMAND mem-enable set",
                pci.bus,
                pci.device,
                pci.function
            );
        }

        let caplength = unsafe { mmio_r8(mmio_virt, CAP_CAPLENGTH) } as usize;
        if caplength == 0 || caplength > 0x40 {
            crate::serial_println!("[usb] EHCI bad CAPLENGTH {}", caplength);
            let cmd = pci.read_config(0x04);
            crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} BAD CAPLENGTH {} BAR0={:#x} CMD={:#x} phys={:#x}",
                pci.bus,
                pci.device,
                pci.function,
                caplength,
                pci.bar0,
                cmd,
                mmio_phys
            );
            return Err("bad CAPLENGTH");
        }
        let op_base = mmio_virt + caplength;
        let hcsparams = unsafe { mmio_r32(mmio_virt, CAP_HCSPARAMS) };
        let hccparams = unsafe { mmio_r32(mmio_virt, CAP_HCCPARAMS) };
        let n_ports = (hcsparams & 0xF) as u8;
        if n_ports == 0 || n_ports > 16 {
            return Err("bad port count");
        }
        crate::println!(
            "[usb] EHCI {:02x}:{:02x}.{} ports={} HCC={:#x}",
            pci.bus,
            pci.device,
            pci.function,
            n_ports,
            hccparams
        );

        // BIOS handoff via EECP in HCCPARAMS[15:8] -> MMIO + eecp = LEGSUP.
        let eecp = ((hccparams >> 8) & 0xFF) as usize;
        if eecp != 0 && eecp < 0x1000 {
            unsafe {
                let legsup = mmio_r32(mmio_virt, eecp);
                // Set OS owned, clear SMI enables (high byte), keep rest.
                mmio_w32(mmio_virt, eecp, (legsup & 0x00FF_FFFF) | (1 << 24));
            }
            // Wait for BIOS owned (bit16) clear, bounded.
            let mut ok = false;
            for _ in 0..2000 {
                let v = unsafe { mmio_r32(mmio_virt, eecp) };
                if v & (1 << 16) == 0 {
                    ok = true;
                    break;
                }
                delay_ms(1);
            }
            // Clear SMI status in LEGCTLSTS (+4) by writing 1s.
            unsafe {
                mmio_w32(mmio_virt, eecp + 4, 0xFFFF_FFFF);
            }
            if !ok {
                crate::println!("[usb] EHCI BIOS handoff timeout, continuing");
            } else {
                crate::serial_println!("[usb] EHCI OS ownership acquired");
            }
        }

        // Halt. Op offsets below are relative to op_base.
        unsafe {
            let cmd = mmio_r32(op_base, OP_USBCMD);
            mmio_w32(op_base, OP_USBCMD, cmd & !CMD_RS);
        }
        let mut halted = false;
        for _ in 0..1000 {
            let sts = unsafe { mmio_r32(op_base, OP_USBSTS) };
            if sts & STS_HCHALTED != 0 {
                halted = true;
                break;
            }
            delay_ms(1);
        }
        if !halted {
            return Err("halt timeout");
        }

        // Reset.
        unsafe {
            let cmd = mmio_r32(op_base, OP_USBCMD);
            mmio_w32(op_base, OP_USBCMD, cmd | CMD_HCRESET);
        }
        let mut reset_done = false;
        for _ in 0..2000 {
            let cmd = unsafe { mmio_r32(op_base, OP_USBCMD) };
            if cmd & CMD_HCRESET == 0 {
                reset_done = true;
                break;
            }
            delay_ms(1);
        }
        if !reset_done {
            crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} HCRESET stuck",
                pci.bus,
                pci.device,
                pci.function
            );
            return Err("reset timeout");
        }

        // HCRESET can revert LEGSUP ownership / SMI enables on real HW
        // (Apple SMI re-acquires in the reset window and halts us again:
        // photo CMD=0x31 yet STS HALTED). Re-assert OS-owned + SMI-disable
        // after reset, before touching schedules.
        if eecp != 0 && eecp < 0x1000 {
            unsafe {
                let legsup = mmio_r32(mmio_virt, eecp);
                let ctlsts = mmio_r32(mmio_virt, eecp + 4);
                if legsup & (1 << 16) != 0 || legsup & 0xFF00_0000 != 0 {
                    crate::println!(
                        "[usb] EHCI {:02x}:{:02x}.{} re-handoff LEGSUP={:#x} CTLSTS={:#x}",
                        pci.bus,
                        pci.device,
                        pci.function,
                        legsup,
                        ctlsts
                    );
                }
                mmio_w32(mmio_virt, eecp, (legsup & 0x00FF_FFFF) | (1 << 24));
            }
            for _ in 0..2000 {
                let v = unsafe { mmio_r32(mmio_virt, eecp) };
                if v & (1 << 16) == 0 {
                    break;
                }
                delay_ms(1);
            }
            unsafe {
                mmio_w32(mmio_virt, eecp + 4, 0xFFFF_FFFF);
            }
        }

        // Frame list: 1024 entries, 4K aligned, all Terminate.
        let (fl_virt, fl_phys, fl_ptr) = dma_page().ok_or("frame list alloc")?;
        unsafe {
            let entries = core::slice::from_raw_parts_mut(fl_ptr as *mut u32, 1024);
            for e in entries.iter_mut() {
                *e = TERM;
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            mmio_w32(op_base, OP_PERIODICLISTBASE, (fl_phys & 0xFFFF_FFFF) as u32);
        }

        // Async reclaim head QH.
        let (qh_virt, qh_phys, qh_ptr) = dma_page().ok_or("async QH alloc")?;
        unsafe {
            let qh = &mut *(qh_ptr as *mut Qh);
            qh.hlink = TERM;
            // H=1, DTC=0, EPS=high(10b), maxpacket 64.
            qh.ep_chars = (1 << 15) | (1 << 14) | (2 << 12) | (64 << 16);
            qh.ep_caps = 1 << 30; // Mult=1
            qh.cur = 0;
            qh.next = TERM;
            qh.alt = TERM;
            qh.token = 0;
            qh.buf0 = 0;
            qh.buf1 = 0;
            qh.buf2 = 0;
            qh.buf3 = 0;
            qh.buf4 = 0;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            mmio_w32(op_base, OP_ASYNCLISTADDR, (qh_phys & 0xFFFF_FFFF) as u32);
        }

        // No interrupts (polling), route ports to EHCI, start schedules.
        // Order matters on real HW: clear W1C status (HSErr/PortChange stall
        // RS), then CONFIGFLAG=1, settle, FRINDEX=0, then RMW RS|PSE|ASE
        // preserving Interrupt Threshold etc. (exact 0x31 write wipes ITC;
        // QEMU tolerates, Intel may not). Retry once after clearing HSErr;
        // Mac photo showed CMD=0x31 yet STS HALTED.
        unsafe {
            mmio_w32(op_base, OP_USBSTS, 0x3F);
            mmio_w32(op_base, OP_USBINTR, 0);
            mmio_w32(op_base, OP_CONFIGFLAG, 1);
        }
        delay_ms(10);
        unsafe {
            mmio_w32(op_base, OP_FRINDEX, 0);
            // FLS=00 (1024); preserve ITC/park bits, set PSE+ASE+RS.
            let cmd = mmio_r32(op_base, OP_USBCMD);
            mmio_w32(op_base, OP_USBCMD, cmd | CMD_RS | CMD_PSE | CMD_ASE);
        }
        let mut running = false;
        for _ in 0..2000 {
            let sts = unsafe { mmio_r32(op_base, OP_USBSTS) };
            if sts & STS_HCHALTED == 0 {
                running = true;
                break;
            }
            delay_ms(1);
        }
        if !running {
            // One retry after clearing a latched Host System Error.
            unsafe {
                mmio_w32(op_base, OP_USBSTS, 0x3F);
                let cmd = mmio_r32(op_base, OP_USBCMD);
                mmio_w32(op_base, OP_USBCMD, cmd | CMD_RS | CMD_PSE | CMD_ASE);
            }
            for _ in 0..2000 {
                let sts = unsafe { mmio_r32(op_base, OP_USBSTS) };
                if sts & STS_HCHALTED == 0 {
                    running = true;
                    break;
                }
                delay_ms(1);
            }
        }
        if !running {
            let sts = unsafe { mmio_r32(op_base, OP_USBSTS) };
            let cmd = unsafe { mmio_r32(op_base, OP_USBCMD) };
            crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} start timeout CMD={:#x} STS={:#x}",
                pci.bus,
                pci.device,
                pci.function,
                cmd,
                sts
            );
            return Err("start timeout");
        }
        crate::serial_println!("[usb] EHCI started");

        Ok(Self {
            pci_bus: pci.bus,
            pci_dev: pci.device,
            pci_func: pci.function,
            mmio_virt,
            op_base,
            n_ports,
            frame_virt: fl_virt,
            frame_phys: fl_phys,
            frame_ptr: fl_ptr as *mut u32,
            async_qh_virt: qh_virt,
            async_qh_phys: qh_phys,
            next_addr: 1,
            keyboards: Vec::new(),
            mice: Vec::new(),
        })
    }

    fn portsc(&self, port: u8) -> u32 {
        unsafe { mmio_r32(self.op_base, OP_PORTSC_BASE + (port as usize) * 4) }
    }

    fn set_portsc(&self, port: u8, val: u32) {
        unsafe { mmio_w32(self.op_base, OP_PORTSC_BASE + (port as usize) * 4, val) };
    }

    fn reset_port(&self, port: u8) -> bool {
        // Panther Point PCH routes root ports through a Rate Matching Hub
        // (8087:0020). The RMH is a high-speed hub; the keyboard sits behind
        // it. A root reset that leaves CONNECTED set but ENABLED clear still
        // deserves a hub probe (QEMU has the keyboard directly on the root,
        // so ENABLED is set there). Retry once; real HW needs longer settle.
        for attempt in 0..2 {
            // Power + clear owner, then reset pulse.
            let mut v = self.portsc(port);
            v |= PORT_POWER;
            v &= !PORT_OWNER;
            // Clear change bits by writing 1s. Set them to clear.
            v |= PORT_ENABLE_CHANGE;
            self.set_portsc(port, v);
            delay_ms(20);

            // Assert reset (EHCI spec: 50ms).
            let mut v = self.portsc(port);
            // Preserve power; do not set change bits on assert.
            v = (v & !PORT_CHANGE_BITS) | PORT_POWER | PORT_RESET;
            v &= !PORT_OWNER;
            self.set_portsc(port, v);
            delay_ms(100);
            // Deassert via hardware clear: poll for reset clear.
            let mut done = false;
            for _ in 0..500 {
                let s = self.portsc(port);
                if s & PORT_RESET == 0 {
                    done = true;
                    break;
                }
                delay_ms(1);
            }
            if !done {
                crate::println!("[usb] EHCI port{} reset timeout (try {})", port, attempt);
                continue;
            }
            delay_ms(20);
            // Clear change bits (W1C), preserving power.
            let s = self.portsc(port);
            self.set_portsc(port, s | PORT_ENABLE_CHANGE | 0x2);
            let s2 = self.portsc(port);
            if s2 & PORT_CONNECTED == 0 {
                return false;
            }
            if s2 & PORT_ENABLED != 0 {
                return true;
            }
            // Connected but not enabled (RMH / full-speed behind hub):
            // let the hub probe decide instead of failing here.
            crate::println!(
                "[usb] EHCI port{} connected, not enabled ({:#x}), trying hub probe",
                port,
                s2
            );
            return true;
        }
        false
    }

    fn enumerate_all(&mut self) {
        // Controller running state first: proves MMIO read/write works and
        // the schedule is actually on (HCHALTED clear == RUN). On real HW
        // (MacBookAir5,2) SMI/BIOS can halt schedules after new(); re-assert
        // RS|PSE|ASE here instead of enumerating halted (photo: CMD=0x30).
        let mut sts = unsafe { mmio_r32(self.op_base, OP_USBSTS) };
        if sts & STS_HCHALTED != 0 {
            // Clear latched W1C errors first or RS will not stick.
            unsafe {
                mmio_w32(self.op_base, OP_USBSTS, 0x3F);
                mmio_w32(self.op_base, OP_CONFIGFLAG, 1);
                let cmd = mmio_r32(self.op_base, OP_USBCMD);
                mmio_w32(self.op_base, OP_USBCMD, cmd | CMD_RS | CMD_PSE | CMD_ASE);
            }
            for _ in 0..2000 {
                sts = unsafe { mmio_r32(self.op_base, OP_USBSTS) };
                if sts & STS_HCHALTED == 0 {
                    break;
                }
                delay_ms(1);
            }
            if sts & STS_HCHALTED != 0 {
                // Retry once after clearing a latched Host System Error.
                unsafe {
                    mmio_w32(self.op_base, OP_USBSTS, 0x3F);
                    let cmd = mmio_r32(self.op_base, OP_USBCMD);
                    mmio_w32(self.op_base, OP_USBCMD, cmd | CMD_RS | CMD_PSE | CMD_ASE);
                }
                for _ in 0..2000 {
                    sts = unsafe { mmio_r32(self.op_base, OP_USBSTS) };
                    if sts & STS_HCHALTED == 0 {
                        break;
                    }
                    delay_ms(1);
                }
            }
            crate::println!(
                "[usb] EHCI {:02x}:{:02x}.{} re-started, STS={:#x} {}",
                self.pci_bus,
                self.pci_dev,
                self.pci_func,
                sts,
                if sts & STS_HCHALTED == 0 {
                    "RUN"
                } else {
                    "HALTED"
                }
            );
        }
        let cmd = unsafe { mmio_r32(self.op_base, OP_USBCMD) };
        let sts = unsafe { mmio_r32(self.op_base, OP_USBSTS) };
        let cfgl = unsafe { mmio_r32(self.op_base, OP_CONFIGFLAG) };
        crate::println!(
            "[usb] EHCI {:02x}:{:02x}.{} CMD={:#x} STS={:#x} CF={:#x} {}",
            self.pci_bus,
            self.pci_dev,
            self.pci_func,
            cmd,
            sts,
            cfgl,
            if sts & STS_HCHALTED == 0 {
                "RUN"
            } else {
                "HALTED"
            }
        );
        for port in 0..self.n_ports {
            self.dump_port(port);
        }
        // Settle time for slow devices before the one-shot boot scan.
        delay_ms(200);
        for port in 0..self.n_ports {
            self.scan_port(port);
            if self.keyboards.len() >= 4 || self.next_addr > 20 {
                break;
            }
        }
    }

    /// Hotplug/slow-device rescan for one controller. Re-enumerates only
    /// root ports whose connect-change bit fired (replug or late appear),
    /// drops stale keyboards for changed ports, and clears the bit.
    /// Unchanged ports are untouched (no duplicate claims, no chatter).
    fn rescan_ports(&mut self) {
        const CSC: u32 = 0x2; // connect-status-change (W1C)
        for port in 0..self.n_ports {
            let sc = self.portsc(port);
            if sc & CSC == 0 {
                continue;
            }
            // Clear change bits (W1C), preserving the rest.
            self.set_portsc(port, sc | PORT_ENABLE_CHANGE | CSC);
            if sc & PORT_CONNECTED == 0 {
                // Unplugged: drop stale keyboards for this port.
                self.keyboards.retain(|k| k.port != port);
                continue;
            }
            if self.keyboards.len() >= 4 || self.next_addr > 20 {
                continue;
            }
            self.keyboards.retain(|k| k.port != port);
            self.scan_port(port);
        }
    }

    fn dump_port(&self, port: u8) {
        let sc = self.portsc(port);
        let ls = match (sc >> 10) & 0x3 {
            0 => "SE0",
            1 => "K",
            2 => "J",
            _ => "?",
        };
        crate::println!(
            "[usb] EHCI {:02x}:{:02x}.{} port{} PORTSC={:#x} pp={} own={} ls={}{}",
            self.pci_bus,
            self.pci_dev,
            self.pci_func,
            port,
            sc,
            (sc >> 12) & 0x1,
            (sc >> 13) & 0x1,
            ls,
            if sc & PORT_CONNECTED == 0 {
                ""
            } else {
                " CONNECTED"
            }
        );
    }

    /// Reset + enumerate one connected root port (boot scan and rescan share it).
    fn scan_port(&mut self, port: u8) {
        // Port reset cannot complete while HCHalted; report via serial only.
        // (VGA every 2s rescan flooded the shell in the Air photo.)
        let sts = unsafe { mmio_r32(self.op_base, OP_USBSTS) };
        if sts & STS_HCHALTED != 0 {
            crate::serial_println!("[usb] EHCI port{} skip reset, controller halted", port);
            return;
        }
        let sc = self.portsc(port);
        if sc & PORT_CONNECTED == 0 {
            return;
        }
        crate::println!("[usb] EHCI port{} connected ({:#x}), resetting", port, sc);
        if !self.reset_port(port) {
            crate::println!("[usb] EHCI port{} reset failed", port);
            return;
        }
        // Try high-speed first, then full, then low.
        for (eps, cflag, name) in [(2u8, 0u32, "high"), (0u8, 1u32, "full"), (1u8, 1u32, "low")] {
            if self.enumerate_device(port, None, eps, cflag) {
                crate::serial_println!("[usb] port{} {}-speed device enumerated", port, name);
                return;
            }
        }
        crate::println!("[usb] EHCI port{}: no device claimed", port);
    }

    /// Enumerates one device on a root port. hub=(addr) if behind a hub.
    fn enumerate_device(&mut self, port: u8, hub: Option<(u8, u8)>, eps: u8, cflag: u32) -> bool {
        // Get 8-byte device descriptor at address 0.
        let setup_get8 = setup_packet(0x80, 6, 0x0100, 0, 8);
        let data = self.control_transfer(0, 8, eps, cflag, hub, setup_get8, None, 8);
        let Some(data) = data else { return false };
        if data.len() < 8 {
            return false;
        }
        let maxpacket0 = data[7].clamp(8, 64);

        let addr = self.next_addr;
        if addr >= 127 {
            return false;
        }
        self.next_addr += 1;

        // SET_ADDRESS.
        let setup_addr = setup_packet(0x00, 5, addr as u16, 0, 0);
        if self
            .control_transfer(0, maxpacket0, eps, cflag, hub, setup_addr, None, 0)
            .is_none()
        {
            return false;
        }
        delay_ms(10);

        // Full device descriptor.
        let setup_full = setup_packet(0x80, 6, 0x0100, 0, 18);
        let full = self.control_transfer(addr, maxpacket0, eps, cflag, hub, setup_full, None, 18);
        let Some(full) = full else { return false };
        if full.len() < 18 {
            return false;
        }
        let dev_class = full[4];

        // Config header (9 bytes) to learn total length.
        let setup_cfg9 = setup_packet(0x80, 6, 0x0200, 0, 9);
        let cfg9 = self.control_transfer(addr, maxpacket0, eps, cflag, hub, setup_cfg9, None, 9);
        let Some(cfg9) = cfg9 else { return false };
        if cfg9.len() < 9 {
            return false;
        }
        let total = (cfg9[2] as usize) | ((cfg9[3] as usize) << 8);
        if total < 9 || total > 512 {
            return false;
        }
        let setup_cfg = setup_packet(0x80, 6, 0x0200, 0, total as u16);
        let cfg = self.control_transfer(addr, maxpacket0, eps, cflag, hub, setup_cfg, None, total);
        let Some(cfg) = cfg else { return false };

        // Hub?
        if dev_class == 9 {
            return self.enumerate_hub(port, addr, maxpacket0, eps, cflag, &cfg);
        }

        // Find HID boot keyboard or mouse interface + interrupt IN endpoint.
        let (iface, ep, ep_max, is_mouse) = match find_hid_keyboard(&cfg) {
            Some((iface, ep, max)) => (iface, ep, max, false),
            None => match find_hid_mouse(&cfg) {
                Some((iface, ep, max)) => (iface, ep, max, true),
                None => return false,
            },
        };

        // SET_CONFIGURATION (first config value).
        let cfg_value = cfg.get(5).copied().unwrap_or(1);
        let setup_setcfg = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
        if self
            .control_transfer(addr, maxpacket0, eps, cflag, hub, setup_setcfg, None, 0)
            .is_none()
        {
            return false;
        }

        // SET_PROTOCOL boot (0). Valid for keyboards and mice alike.
        let setup_proto = setup_packet(0x21, 0x0B, 0, iface as u16, 0);
        if self
            .control_transfer(addr, maxpacket0, eps, cflag, hub, setup_proto, None, 0)
            .is_none()
        {
            crate::println!("[usb] HID SET_PROTOCOL failed, continuing");
        }

        // SET_IDLE (0, report only on change) — like the xHCI claim path;
        // continue on failure, most devices default sensibly.
        let setup_idle = setup_packet(0x21, 0x0A, 0, iface as u16, 0);
        if self
            .control_transfer(addr, maxpacket0, eps, cflag, hub, setup_idle, None, 0)
            .is_none()
        {
            crate::println!("[usb] HID SET_IDLE failed, continuing");
        }

        if is_mouse {
            self.start_mouse_poll(port, addr, ep, ep_max.min(8), eps, cflag, hub);
            crate::println!(
                "[usb] HID mouse addr={} ep={:#x} max={} claimed (port{})",
                addr,
                ep,
                ep_max,
                port
            );
        } else {
            self.start_interrupt_poll(port, addr, ep, ep_max.min(8), eps, cflag, hub);
            crate::println!(
                "[usb] HID keyboard addr={} ep={:#x} max={} claimed (port{})",
                addr,
                ep,
                ep_max,
                port
            );
        }
        true
    }

    fn enumerate_hub(
        &mut self,
        root_port: u8,
        addr: u8,
        maxpacket0: u8,
        eps: u8,
        cflag: u32,
        cfg: &[u8],
    ) -> bool {
        // SET_CONFIGURATION first so hub ports power on.
        let cfg_value = cfg.get(5).copied().unwrap_or(1);
        let setup_setcfg = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
        if self
            .control_transfer(addr, maxpacket0, eps, cflag, None, setup_setcfg, None, 0)
            .is_none()
        {
            return false;
        }
        // GET hub descriptor (type 0x29) to learn port count.
        let setup_hub = setup_packet(0xA0, 6, 0x2900, 0, 8);
        let hubd = self.control_transfer(addr, maxpacket0, eps, cflag, None, setup_hub, None, 8);
        let Some(hubd) = hubd else { return false };
        if hubd.len() < 3 {
            return false;
        }
        let nports = hubd[2].min(8);
        crate::println!("[usb] hub addr={} ports={}", addr, nports);

        for port in 1..=nports {
            // Power port.
            let setup_pwr = setup_packet(0x23, 3, 8, port as u16, 0);
            let _ = self.control_transfer(addr, maxpacket0, eps, cflag, None, setup_pwr, None, 0);
            delay_ms(30);
            // Reset port.
            let setup_rst = setup_packet(0x23, 3, 4, port as u16, 0);
            let _ = self.control_transfer(addr, maxpacket0, eps, cflag, None, setup_rst, None, 0);
            delay_ms(60);

            // Probe downstream device: try speeds; hub split caps set below.
            let hub_info = Some((addr, port));
            let mut claimed = false;
            for (deps, dcflag) in [(2u8, 0u32), (0u8, 1u32), (1u8, 1u32)] {
                // Need a temporary address probe: enumerate_device allocates its own
                // address; attempt full enumeration via helper with hub caps.
                if self.enumerate_hub_port(root_port, addr, port, deps, dcflag) {
                    claimed = true;
                    break;
                }
            }
            let _ = (hub_info, claimed);
            if self.keyboards.len() >= 4 || self.next_addr > 24 {
                break;
            }
        }
        // Hub itself is not a keyboard, but enumeration succeeded.
        true
    }

    fn enumerate_hub_port(
        &mut self,
        root_port: u8,
        hub_addr: u8,
        hub_port: u8,
        eps: u8,
        cflag: u32,
    ) -> bool {
        // Same as enumerate_device but with hub split info. Reuse address-0 probe.
        let setup_get8 = setup_packet(0x80, 6, 0x0100, 0, 8);
        let hub = Some((hub_addr, hub_port));
        let data = self.control_transfer_hub(0, 8, eps, cflag, hub, setup_get8, 8);
        let Some(data) = data else { return false };
        if data.len() < 8 {
            return false;
        }
        let maxpacket0 = data[7].clamp(8, 64);
        let addr = self.next_addr;
        if addr >= 127 {
            return false;
        }
        self.next_addr += 1;

        let setup_addr = setup_packet(0x00, 5, addr as u16, 0, 0);
        if self
            .control_transfer_hub(0, maxpacket0, eps, cflag, hub, setup_addr, 0)
            .is_none()
        {
            return false;
        }
        delay_ms(10);

        let setup_full = setup_packet(0x80, 6, 0x0100, 0, 18);
        let full = self.control_transfer_hub(addr, maxpacket0, eps, cflag, hub, setup_full, 18);
        let Some(full) = full else { return false };
        if full.len() < 18 || full[4] == 9 {
            return false; // ignore nested hubs for v1
        }
        let setup_cfg9 = setup_packet(0x80, 6, 0x0200, 0, 9);
        let cfg9 = self.control_transfer_hub(addr, maxpacket0, eps, cflag, hub, setup_cfg9, 9);
        let Some(cfg9) = cfg9 else { return false };
        if cfg9.len() < 9 {
            return false;
        }
        let total = (cfg9[2] as usize) | ((cfg9[3] as usize) << 8);
        if total < 9 || total > 512 {
            return false;
        }
        let setup_cfg = setup_packet(0x80, 6, 0x0200, 0, total as u16);
        let cfg = self.control_transfer_hub(addr, maxpacket0, eps, cflag, hub, setup_cfg, total);
        let Some(cfg) = cfg else { return false };
        let (iface, ep, ep_max, is_mouse) = match find_hid_keyboard(&cfg) {
            Some((iface, ep, max)) => (iface, ep, max, false),
            None => match find_hid_mouse(&cfg) {
                Some((iface, ep, max)) => (iface, ep, max, true),
                None => return false,
            },
        };
        let cfg_value = cfg.get(5).copied().unwrap_or(1);
        let setup_setcfg = setup_packet(0x00, 9, cfg_value as u16, 0, 0);
        if self
            .control_transfer_hub(addr, maxpacket0, eps, cflag, hub, setup_setcfg, 0)
            .is_none()
        {
            return false;
        }
        let setup_proto = setup_packet(0x21, 0x0B, 0, iface as u16, 0);
        let _ = self.control_transfer_hub(addr, maxpacket0, eps, cflag, hub, setup_proto, 0);
        if is_mouse {
            self.start_interrupt_poll_hub(root_port, addr, ep, ep_max.min(8), eps, hub, true);
            crate::println!(
                "[usb] HID mouse addr={} via hub {} port {} claimed",
                addr,
                hub_addr,
                hub_port
            );
        } else {
            self.start_interrupt_poll_hub(root_port, addr, ep, ep_max.min(8), eps, hub, false);
            crate::println!(
                "[usb] HID keyboard addr={} via hub {} port {} claimed",
                addr,
                hub_addr,
                hub_port
            );
        }
        true
    }

    // Wrapper that maps hub split info into QH caps.
    fn control_transfer(
        &self,
        addr: u8,
        maxpacket: u8,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
        setup: [u8; 8],
        data_out: Option<&[u8]>,
        data_in_len: usize,
    ) -> Option<Vec<u8>> {
        self.control_transfer_hub(addr, maxpacket, eps, cflag, hub, setup, data_in_len)
            .map(|mut v| {
                // For OUT data stage, data was already sent; return empty.
                if data_out.is_some() {
                    v.clear();
                }
                v
            })
            .and_then(|v| {
                // Re-run OUT data payload if present (setup already defined direction).
                if let Some(payload) = data_out {
                    return self
                        .control_out_payload(addr, maxpacket, eps, cflag, hub, setup, payload);
                }
                Some(v)
            })
    }

    fn control_out_payload(
        &self,
        addr: u8,
        maxpacket: u8,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
        setup: [u8; 8],
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        // Rebuild full chain with OUT data stage.
        self.exec_control(addr, maxpacket, eps, cflag, hub, setup, Some(payload), 0)
    }

    fn control_transfer_hub(
        &self,
        addr: u8,
        maxpacket: u8,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
        setup: [u8; 8],
        data_in_len: usize,
    ) -> Option<Vec<u8>> {
        self.exec_control(addr, maxpacket, eps, cflag, hub, setup, None, data_in_len)
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_control(
        &self,
        addr: u8,
        maxpacket: u8,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
        setup: [u8; 8],
        data_out: Option<&[u8]>,
        data_in_len: usize,
    ) -> Option<Vec<u8>> {
        let is_in = setup[0] & 0x80 != 0;
        let data_len = data_out.map(|d| d.len()).unwrap_or(data_in_len);

        // DMA pages.
        let (_s_virt, s_phys, s_ptr) = dma_page()?;
        let (d_virt, d_phys, d_ptr) = if data_len > 0 {
            let p = dma_page()?;
            unsafe {
                if data_out.is_some() {
                    core::ptr::copy_nonoverlapping(
                        data_out.unwrap().as_ptr(),
                        p.2,
                        data_len.min(4096),
                    );
                }
            }
            Some(p)
        } else {
            None
        }
        .map(|(a, b, c)| (a, b, c))
        .or(Some((0, 0, core::ptr::null_mut())))
        .unwrap();
        let _ = d_virt;

        unsafe {
            core::ptr::copy_nonoverlapping(setup.as_ptr(), s_ptr, 8);
        }

        // Build qTD chain: setup -> data chunks -> status.
        struct TdAlloc {
            virt: u64,
            phys: u64,
            ptr: *mut u8,
        }
        let mut tds: Vec<TdAlloc> = Vec::new();
        let alloc_td = |tds: &mut Vec<TdAlloc>| -> Option<(u64, u64, *mut Qtd)> {
            let (virt, phys, ptr) = dma_page()?;
            tds.push(TdAlloc { virt, phys, ptr });
            Some((virt, phys, ptr as *mut Qtd))
        };

        // Setup TD.
        let (_sv, setup_phys, setup_td) = alloc_td(&mut tds)?;
        // Data TDs.
        let mut data_tds: Vec<(u64, u64)> = Vec::new();
        if data_len > 0 {
            let mut remaining = data_len;
            let mut offset = 0usize;
            let mut toggle = true; // DATA1 first
            while remaining > 0 {
                let chunk = remaining.min(maxpacket as usize).min(4096);
                let (_v, phys, _td) = alloc_td(&mut tds)?;
                data_tds.push((
                    phys,
                    chunk as u64 | ((toggle as u64) << 48) | ((offset as u64) << 32),
                ));
                let _ = offset;
                offset += chunk;
                remaining -= chunk;
                toggle = !toggle;
            }
        }
        let (_stv, status_phys, _status_td) = alloc_td(&mut tds)?;

        // Fill TDs. Layout: [setup][data...][status].
        unsafe {
            // Setup: PID SETUP, 8 bytes, toggle 0.
            let td = &mut *setup_td;
            let next_phys = if !data_tds.is_empty() {
                (data_tds[0].0 & 0xFFFF_FFFF) as u32
            } else {
                (status_phys & 0xFFFF_FFFF) as u32
            };
            td.next = next_phys;
            td.alt = TERM;
            td.token = TOK_ACTIVE | (3 << 10) | (PID_SETUP << 8) | (8 << 16);
            td.buf0 = (s_phys & 0xFFFF_FFFF) as u32;
            td.buf1 = 0;
            td.buf2 = 0;
            td.buf3 = 0;
            td.buf4 = 0;

            // Data TDs.
            for (idx, (phys, meta)) in data_tds.iter().enumerate() {
                let chunk = (meta & 0xFFFF_FFFF) as u32;
                let toggle = ((meta >> 48) & 1) as u32;
                let offset = ((meta >> 32) & 0xFFFF) as u32;
                let next = if idx + 1 < data_tds.len() {
                    // Next data TD phys is tds[idx+2] (0=setup, then data..., last=status).
                    // tds order: setup, data0..N, status.
                    let (_v, nphys, _p) = (tds[idx + 2].virt, tds[idx + 2].phys, tds[idx + 2].ptr);
                    (nphys & 0xFFFF_FFFF) as u32
                } else {
                    (status_phys & 0xFFFF_FFFF) as u32
                };
                let td_ptr = tds[idx + 1].ptr as *mut Qtd;
                let td = &mut *td_ptr;
                let pid = if is_in { PID_IN } else { PID_OUT };
                td.next = next;
                td.alt = TERM;
                td.token = TOK_ACTIVE | (3 << 10) | (pid << 8) | (chunk << 16) | (toggle << 31);
                let bphys = d_phys + offset as u64;
                td.buf0 = (bphys & 0xFFFF_FFFF) as u32;
                td.buf1 = 0;
                td.buf2 = 0;
                td.buf3 = 0;
                td.buf4 = 0;
                let _ = phys;
            }

            // Status: opposite direction, zero length, toggle 1, IOC.
            let std = &mut *(_status_td as *mut Qtd);
            let pid = if is_in || data_out.is_some() && !is_in {
                // IN data -> OUT status; OUT data or no data -> IN status.
                if is_in {
                    PID_OUT
                } else {
                    PID_IN
                }
            } else if is_in {
                PID_OUT
            } else {
                PID_IN
            };
            std.next = TERM;
            std.alt = TERM;
            std.token = TOK_ACTIVE | TOK_IOC | (3 << 10) | (pid << 8) | (1 << 31);
            std.buf0 = 0;
            std.buf1 = 0;
            std.buf2 = 0;
            std.buf3 = 0;
            std.buf4 = 0;

            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            // Program async QH for this device.
            let qh = &mut *(self.async_qh_virt as *mut Qh);
            let maxp = (maxpacket as u32) << 16;
            let eps_bits = (eps as u32 & 3) << 12;
            let dtc = 1 << 14;
            let hbit = 1 << 15;
            let cbit = cflag << 27;
            qh.ep_chars = (addr as u32) | hbit | dtc | eps_bits | maxp | cbit;
            qh.ep_caps = hub_caps(hub) | (1 << 30);
            qh.next = (setup_phys & 0xFFFF_FFFF) as u32;
            qh.alt = TERM;
            qh.token = 0;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }

        // Ensure async schedule enabled.
        unsafe {
            let cmd = mmio_r32(self.op_base, OP_USBCMD);
            if cmd & CMD_ASE == 0 {
                mmio_w32(self.op_base, OP_USBCMD, cmd | CMD_ASE);
            }
        }

        // Poll completion: all TDs inactive.
        let mut ok = false;
        for _ in 0..5000 {
            let mut all_done = true;
            unsafe {
                for td in tds.iter() {
                    let t = &*(td.ptr as *const Qtd);
                    let tok = core::ptr::read_volatile(&t.token);
                    if tok & TOK_ACTIVE != 0 {
                        all_done = false;
                        break;
                    }
                    if tok & (TOK_HALTED | TOK_BUFERR | TOK_BABBLE | TOK_XACTERR) != 0 {
                        all_done = false;
                        // Mark failed but keep waiting briefly? Fail fast.
                        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                        return None;
                    }
                }
            }
            if all_done {
                ok = true;
                break;
            }
            // Brief spin; ~0.1ms per 100 iterations roughly.
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        if !ok {
            return None;
        }

        if data_in_len > 0 && is_in {
            let mut out = Vec::new();
            unsafe {
                let src = core::slice::from_raw_parts(d_ptr as *const u8, data_in_len.min(4096));
                out.extend_from_slice(src);
            }
            // Trim to requested length (device may send short packet; we keep what HC wrote).
            out.truncate(data_in_len);
            Some(out)
        } else {
            Some(Vec::new())
        }
    }

    fn start_interrupt_poll(
        &mut self,
        port: u8,
        addr: u8,
        ep: u8,
        maxpacket: u16,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
    ) {
        self.start_interrupt_poll_hub(port, addr, ep, maxpacket, eps, hub, false);
        let _ = cflag;
    }

    /// Claim a boot-mouse interrupt endpoint (same machinery as keyboards,
    /// tracked in `mice` so the poll loop parses reports as mice).
    fn start_mouse_poll(
        &mut self,
        port: u8,
        addr: u8,
        ep: u8,
        maxpacket: u16,
        eps: u8,
        cflag: u32,
        hub: Option<(u8, u8)>,
    ) {
        self.start_interrupt_poll_hub(port, addr, ep, maxpacket, eps, hub, true);
        let _ = cflag;
    }

    fn start_interrupt_poll_hub(
        &mut self,
        port: u8,
        addr: u8,
        ep: u8,
        maxpacket: u16,
        eps: u8,
        hub: Option<(u8, u8)>,
        is_mouse: bool,
    ) {
        let (qh_virt, qh_phys, qh_ptr) = match dma_page() {
            Some(v) => v,
            None => return,
        };
        let (qtd_virt, qtd_phys, qtd_ptr) = match dma_page() {
            Some(v) => v,
            None => return,
        };
        let (buf_virt, buf_phys, _buf_ptr) = match dma_page() {
            Some(v) => v,
            None => return,
        };
        let ep_num = ep & 0x0F;
        let maxp = ((maxpacket.min(64)) as u32) << 16;
        let eps_bits = (eps as u32 & 3) << 12;
        unsafe {
            let qh = &mut *(qh_ptr as *mut Qh);
            // Link to previous head (or terminate).
            let prev_head = {
                let e0 = core::ptr::read_volatile(self.frame_ptr);
                if e0 & TERM == 0 {
                    e0
                } else {
                    TERM
                }
            };
            qh.hlink = prev_head;
            qh.ep_chars = (addr as u32) | ((ep_num as u32) << 8) | eps_bits | maxp;
            // DTC=0 for interrupt (HC manages toggle).
            qh.ep_caps = hub_caps(hub) | (1 << 30) | 0x01; // Mult=1, S-mask microframe0
            qh.cur = 0;
            qh.next = (qtd_phys & 0xFFFF_FFFF) as u32;
            qh.alt = TERM;
            qh.token = 0;
            qh.buf0 = 0;
            qh.buf1 = 0;
            qh.buf2 = 0;
            qh.buf3 = 0;
            qh.buf4 = 0;

            let qtd = &mut *(qtd_ptr as *mut Qtd);
            qtd.next = TERM;
            qtd.alt = TERM;
            qtd.token = TOK_ACTIVE | (3 << 10) | (PID_IN << 8) | ((8u32) << 16);
            qtd.buf0 = (buf_phys & 0xFFFF_FFFF) as u32;
            qtd.buf1 = 0;
            qtd.buf2 = 0;
            qtd.buf3 = 0;
            qtd.buf4 = 0;

            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            // Insert at head: all frames -> new QH.
            let entry = ((qh_phys & 0xFFFF_FFF0) as u32) | QH_TYPE;
            for i in 0..1024 {
                core::ptr::write_volatile(self.frame_ptr.add(i), entry);
            }
            // Ensure periodic schedule enabled.
            let cmd = mmio_r32(self.op_base, OP_USBCMD);
            if cmd & CMD_PSE == 0 {
                mmio_w32(self.op_base, OP_USBCMD, cmd | CMD_PSE);
            }
        }
        let ep_entry = HidEp {
            addr,
            port,
            qh_virt,
            qh_phys,
            qtd_virt,
            qtd_phys,
            buf_virt,
            buf_phys,
            maxpacket: maxpacket.min(8),
        };
        if is_mouse {
            self.mice.push(ep_entry);
        } else {
            self.keyboards.push(ep_entry);
        }
    }

    fn poll_keyboards(&mut self) {
        for kbd in self.keyboards.iter() {
            unsafe {
                let qtd = &*(kbd.qtd_virt as *const Qtd);
                let tok = core::ptr::read_volatile(&qtd.token);
                if tok & TOK_ACTIVE != 0 {
                    continue;
                }
                if tok & (TOK_HALTED | TOK_BUFERR | TOK_BABBLE | TOK_XACTERR) != 0 {
                    // Re-prime on error to avoid stuck endpoint.
                    Self::reprime(kbd);
                    continue;
                }
                // Completed IN transfer: 8-byte boot report.
                let report_ptr = kbd.buf_virt as *const u8;
                let mut report = [0u8; 8];
                core::ptr::copy_nonoverlapping(report_ptr, report.as_mut_ptr(), 8);
                // Only push non-idle to reduce noise? push_usb_report filters repeats.
                crate::drivers::keyboard::push_usb_report(report);
                Self::reprime(kbd);
            }
        }
    }

    fn poll_mice(&mut self) {
        for mouse in self.mice.iter() {
            unsafe {
                let qtd = &*(mouse.qtd_virt as *const Qtd);
                let tok = core::ptr::read_volatile(&qtd.token);
                if tok & TOK_ACTIVE != 0 {
                    continue;
                }
                if tok & (TOK_HALTED | TOK_BUFERR | TOK_BABBLE | TOK_XACTERR) != 0 {
                    // Re-prime on error to avoid stuck endpoint.
                    Self::reprime(mouse);
                    continue;
                }
                // Completed IN transfer: boot mouse report
                // (buttons, dx, dy [, wheel]). HID Y is screen-positive-down.
                let report_ptr = mouse.buf_virt as *const u8;
                let mut report = [0u8; 8];
                core::ptr::copy_nonoverlapping(report_ptr, report.as_mut_ptr(), 8);
                crate::drivers::mouse::push_usb_mouse(
                    report[0],
                    report[1] as i8 as i16,
                    report[2] as i8 as i16,
                );
                Self::reprime(mouse);
            }
        }
    }

    fn reprime(kbd: &HidEp) {
        unsafe {
            let qtd = &mut *(kbd.qtd_virt as *mut Qtd);
            core::ptr::write_volatile(&mut qtd.next, TERM);
            core::ptr::write_volatile(&mut qtd.alt, TERM);
            core::ptr::write_volatile(
                &mut qtd.token,
                TOK_ACTIVE | (3 << 10) | (PID_IN << 8) | ((8u32) << 16),
            );
            core::ptr::write_volatile(&mut qtd.buf0, (kbd.buf_phys & 0xFFFF_FFFF) as u32);
            core::ptr::write_volatile(&mut qtd.buf1, 0);
            core::ptr::write_volatile(&mut qtd.buf2, 0);
            core::ptr::write_volatile(&mut qtd.buf3, 0);
            core::ptr::write_volatile(&mut qtd.buf4, 0);
            let qh = &mut *(kbd.qh_virt as *mut Qh);
            core::ptr::write_volatile(&mut qh.next, (kbd.qtd_phys & 0xFFFF_FFFF) as u32);
            core::ptr::write_volatile(&mut qh.alt, TERM);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Standard 8-byte USB setup packet layout (transport-independent).
pub(crate) fn setup_packet(bm: u8, req: u8, value: u16, index: u16, len: u16) -> [u8; 8] {
    [
        bm,
        req,
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        (index & 0xFF) as u8,
        (index >> 8) as u8,
        (len & 0xFF) as u8,
        (len >> 8) as u8,
    ]
}

/// Walk a config descriptor for the first boot-keyboard interface with an
/// interrupt-IN endpoint. Returns (interface, endpoint, maxpacket).
pub(crate) fn find_hid_keyboard(cfg: &[u8]) -> Option<(u8, u8, u16)> {
    // Walk config descriptor for interface (9) + endpoint (7) records.
    let mut i = 0;
    let mut cur_iface: Option<(u8, u8, u8)> = None; // (num, subclass, proto)
    while i + 2 <= cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len == 0 || i + len > cfg.len() {
            break;
        }
        if dtype == 4 && len >= 9 {
            // Interface: bInterfaceNumber[2], class[5], subclass[6], proto[7].
            if cfg[i + 5] == 3 && cfg[i + 6] == 1 && cfg[i + 7] == 1 {
                cur_iface = Some((cfg[i + 2], cfg[i + 6], cfg[i + 7]));
            } else {
                cur_iface = None;
            }
        } else if dtype == 5 && len >= 7 {
            if let Some((iface, _sub, _proto)) = cur_iface {
                let ep_addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let max = (cfg[i + 4] as u16) | ((cfg[i + 5] as u16) << 8);
                if ep_addr & 0x80 != 0 && attr & 0x03 == 0x03 {
                    return Some((iface, ep_addr, max.max(8)));
                }
            }
        }
        i += len;
    }
    None
}

/// Pointer-device kinds a single HID interrupt endpoint can serve. One
/// xHCI slot (and one EHCI QH) carries exactly one claimed endpoint, so a
/// composite keyboard+mouse device claims across separate interfaces only
/// when they live on separate endpoints — the common QEMU case (usb-kbd
/// and usb-tablet are separate devices/slots).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HidPointerKind {
    /// Boot-protocol mouse: 3-byte (buttons, dx, dy) or 4-byte (+wheel).
    BootMouse,
    /// Absolute tablet/pointer (e.g. QEMU usb-tablet): buttons + LE X/Y.
    Tablet,
}

/// Absolute-tablet report layout decoded from its HID report descriptor.
/// All offsets/sizes in bytes and bits; X/Y are unsigned little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TabletLayout {
    /// Byte offset of the button byte (low 3 bits are buttons 1-3).
    pub buttons_at: usize,
    /// Byte offset of the 16-bit LE absolute X field.
    pub x_at: usize,
    /// Byte offset of the 16-bit LE absolute Y field.
    pub y_at: usize,
    /// Logical maximum of X (device units).
    pub x_max: u32,
    /// Logical maximum of Y (device units).
    pub y_max: u32,
    /// Total input report length in bytes.
    pub report_len: usize,
}

impl TabletLayout {
    /// QEMU usb-tablet fallback: [buttons, Xlo, Xhi, Ylo, Yhi], 0..32767.
    /// Used when the report descriptor is missing or unparseable; the
    /// descriptor dump in the claim log shows whether it applied.
    pub(crate) const fn qm_fallback() -> Self {
        Self {
            buttons_at: 0,
            x_at: 1,
            y_at: 3,
            x_max: 32767,
            y_max: 32767,
            report_len: 5,
        }
    }
}

/// Detects a HID pointer interface (boot mouse or absolute tablet) with an
/// interrupt-IN endpoint. Returns (interface, endpoint, maxpacket, kind).
/// Boot mice (subclass 1, protocol 2) match first; otherwise a subclass 0 /
/// protocol 0 HID interface with an interrupt-IN endpoint is treated as a
/// tablet candidate (its report descriptor is parsed at claim time, with a
/// QEMU-layout fallback).
pub(crate) fn find_hid_pointer(cfg: &[u8]) -> Option<(u8, u8, u16, HidPointerKind)> {
    // Pass 1: boot-protocol mouse (deterministic layout, preferred).
    if let Some((iface, ep, max)) = find_hid_mouse(cfg) {
        return Some((iface, ep, max, HidPointerKind::BootMouse));
    }
    // Pass 2: subclass-0/protocol-0 HID interface + interrupt-IN endpoint.
    let mut i = 0;
    let mut cur_iface: Option<u8> = None;
    while i + 2 <= cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len == 0 || i + len > cfg.len() {
            break;
        }
        if dtype == 4 && len >= 9 {
            if cfg[i + 5] == 3 && cfg[i + 6] == 0 && cfg[i + 7] == 0 {
                cur_iface = Some(cfg[i + 2]);
            } else {
                cur_iface = None;
            }
        } else if dtype == 5 && len >= 7 {
            if let Some(iface) = cur_iface {
                let ep_addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let max = (cfg[i + 4] as u16) | ((cfg[i + 5] as u16) << 8);
                if ep_addr & 0x80 != 0 && attr & 0x03 == 0x03 {
                    return Some((iface, ep_addr, max.max(8), HidPointerKind::Tablet));
                }
            }
        }
        i += len;
    }
    None
}

/// Minimal HID report-descriptor walker for absolute tablets.
///
/// Tracks Global items (Logical Minimum/Maximum, Report Size/Count, Usage
/// Page) and finds the first two 16-bit Generic-Desktop X (0x30) / Y (0x31)
/// fields plus the leading button byte. Returns None when no absolute X/Y
/// pair is found; callers fall back to [`TabletLayout::qemu_fallback`].
/// Short items only (QEMU emits no long items); bounded by descriptor len.
pub(crate) fn parse_tablet_layout(desc: &[u8]) -> Option<TabletLayout> {
    let mut usage_page: u32 = 0;
    let mut log_min: i32 = 0;
    let mut log_max: i32 = 0;
    let mut report_size: u32 = 0;
    let mut report_count: u32 = 0;
    // Byte offset accumulator for the input report under construction.
    let mut bit_offset: u32 = 0;
    let mut buttons_at: Option<usize> = None;
    let mut x_at: Option<usize> = None;
    let mut y_at: Option<usize> = None;
    let mut x_max: u32 = 0;
    let mut y_max: u32 = 0;
    // Pending local usages for the next Input/Main item.
    let mut usages: [u32; 8] = [0; 8];
    let mut n_usages: usize = 0;
    let mut usage_min: u32 = 0;
    let mut have_usage_min = false;

    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        i += 1;
        if prefix == 0xFE {
            // Long item: skip (length byte + tag + payload).
            if i + 2 > desc.len() {
                break;
            }
            let len = desc[i] as usize;
            i += 2 + len;
            if i > desc.len() {
                break;
            }
            continue;
        }
        let size_code = prefix & 0x03;
        let typ = (prefix >> 2) & 0x03;
        let tag = (prefix >> 4) & 0x0F;
        let data_len = match size_code {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if i + data_len > desc.len() {
            break;
        }
        let mut data: u32 = 0;
        for k in 0..data_len {
            data |= (desc[i + k] as u32) << (8 * k);
        }
        i += data_len;

        match (typ, tag) {
            // Global items.
            (1, 0x0) => {
                // Usage Page
                usage_page = data;
            }
            (1, 0x1) => {
                // Logical Minimum (sign-extend by size).
                log_min = match data_len {
                    1 => (data as u8) as i8 as i32,
                    2 => (data as u16) as i16 as i32,
                    _ => data as i32,
                };
            }
            (1, 0x2) => {
                // Logical Maximum (sign-extend by size).
                log_max = match data_len {
                    1 => (data as u8) as i8 as i32,
                    2 => (data as u16) as i16 as i32,
                    _ => data as i32,
                };
            }
            (1, 0x7) => report_size = data,
            (1, 0x9) => report_count = data.max(1),
            // Local items: Usage / Usage Minimum / Usage Maximum.
            (2, 0x0) => {
                let usage = if data_len == 4 {
                    // Extended usage: high 16 = page.
                    if (data >> 16) != 0 {
                        usage_page = data >> 16;
                    }
                    data & 0xFFFF
                } else {
                    data
                };
                if n_usages < usages.len() {
                    usages[n_usages] = (usage_page << 16) | usage;
                    n_usages += 1;
                }
                have_usage_min = false;
            }
            (2, 0x1) => {
                usage_min = (usage_page << 16) | data;
                have_usage_min = true;
                n_usages = 0;
            }
            (2, 0x2) => {
                let umax = (usage_page << 16) | data;
                n_usages = 0;
                // Expand button ranges (0x90001..=0x90003) explicitly; other
                // ranges are recorded by endpoints below.
                if usage_page == 0x09 && have_usage_min {
                    let lo = usage_min & 0xFFFF;
                    let hi = umax & 0xFFFF;
                    if lo <= 0x03 && hi >= 0x01 && buttons_at.is_none() {
                        buttons_at = Some((bit_offset / 8) as usize);
                    }
                }
            }
            // Main: Input (0x8).
            (0, 0x8) => {
                let is_variable = data & 0x02 != 0;
                let is_relative = data & 0x04 != 0;
                let is_constant = data & 0x01 != 0;
                let bits = report_size * report_count;
                if !is_constant && is_variable && !is_relative {
                    // Absolute data fields: match usages in order.
                    let mut field_bit = bit_offset;
                    for u in 0..n_usages {
                        let page = usages[u] >> 16;
                        let id = usages[u] & 0xFFFF;
                        let fsize = report_size;
                        if page == 0x01 && id == 0x30 && x_at.is_none() && fsize == 16 {
                            x_at = Some((field_bit / 8) as usize);
                            x_max = (log_max.max(0)) as u32;
                        } else if page == 0x01 && id == 0x31 && y_at.is_none() && fsize == 16 {
                            y_at = Some((field_bit / 8) as usize);
                            y_max = (log_max.max(0)) as u32;
                        } else if page == 0x09 && buttons_at.is_none() {
                            // Button usages without explicit min/max pair.
                            buttons_at = Some((field_bit / 8) as usize);
                        }
                        field_bit += fsize;
                    }
                    // Single-usage fields spanning the whole item (e.g. one
                    // X usage with count 2 is handled by callers pairing
                    // consecutive 16-bit halves; nothing extra here).
                    let _ = log_min;
                }
                bit_offset += bits;
                n_usages = 0;
                have_usage_min = false;
            }
            // Main: Collection / End Collection / Feature / Output.
            (0, _) => {
                n_usages = 0;
                have_usage_min = false;
                if tag == 0xC {
                    // End Collection: nothing to reset globally.
                }
            }
            _ => {}
        }
    }

    let (x_at, y_at) = (x_at?, y_at?);
    if x_max == 0 || y_max == 0 {
        return None;
    }
    let report_len = ((bit_offset + 7) / 8)
        .max((y_at as u32) + 2)
        .max((x_at as u32) + 2) as usize;
    Some(TabletLayout {
        buttons_at: buttons_at.unwrap_or(0),
        x_at,
        y_at,
        x_max,
        y_max,
        report_len: report_len.min(64),
    })
}

/// Detects a HID boot-protocol mouse interface (class 3, subclass 1,
/// protocol 2) with an interrupt-IN endpoint, mirroring
/// [`find_hid_keyboard`]. Claimed by the xHCI/EHCI pointer paths; reports
/// flow to [`crate::drivers::mouse::push_usb_mouse`].
pub(crate) fn find_hid_mouse(cfg: &[u8]) -> Option<(u8, u8, u16)> {
    let mut i = 0;
    let mut cur_iface: Option<u8> = None;
    while i + 2 <= cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len == 0 || i + len > cfg.len() {
            break;
        }
        if dtype == 4 && len >= 9 {
            if cfg[i + 5] == 3 && cfg[i + 6] == 1 && cfg[i + 7] == 2 {
                cur_iface = Some(cfg[i + 2]);
            } else {
                cur_iface = None;
            }
        } else if dtype == 5 && len >= 7 {
            if let Some(iface) = cur_iface {
                let ep_addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let max = (cfg[i + 4] as u16) | ((cfg[i + 5] as u16) << 8);
                if ep_addr & 0x80 != 0 && attr & 0x03 == 0x03 {
                    return Some((iface, ep_addr, max.max(8)));
                }
            }
        }
        i += len;
    }
    None
}

fn hub_caps(hub: Option<(u8, u8)>) -> u32 {
    match hub {
        None => 0,
        Some((haddr, hport)) => {
            // Mult=1, Hub Addr, Port, S-mask 0x01, C-mask 0x1C for FS/LS split.
            (1 << 30) | ((haddr as u32) << 16) | ((hport as u32) << 23) | 0x01 | (0x1C << 8)
        }
    }
}
