//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! MFK installer — arrow-key menu to install MFK to disk.
//!
//! Install means a sector clone of the running boot disk (Primary Master)
//! onto a selected target disk, which then boots MFK on its own. The boot
//! disk itself is the payload, so no image needs to be embedded in the
//! kernel. The data disk (Primary Slave, SimplFS) is left untouched; use
//! the "Format data disk" entry (same as the `mkfs` command) to provision it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::println;

use crate::drivers::block::{BlockDevice, BLOCK_SIZE};
use crate::drivers::keyboard::{Key, KeyEvent};
use crate::drivers::vga::{self, Color, VGA_HEIGHT, VGA_WIDTH};

/// Drive slot we boot from and clone (Primary Master in all runner configs).
const SOURCE_INDEX: usize = 0;
/// Sectors per copy/verify step (16 KiB heap buffer, fits the u8 ATA count).
const CHUNK_SECTORS: usize = 32;

/// BlockDevice adapter for any probed ATA drive slot.
pub struct DriveBlockDevice {
    index: usize,
}

impl DriveBlockDevice {
    pub fn new(index: usize) -> Self {
        Self { index }
    }
}

impl BlockDevice for DriveBlockDevice {
    fn read_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        let count_u8 = u8::try_from(count).map_err(|_| "Too many blocks requested")?;
        if buffer.len() < count * BLOCK_SIZE {
            return Err("Buffer too small");
        }
        crate::drivers::ata::read_sectors_from(self.index, start_block, count_u8, buffer)
    }

    fn write_blocks(
        &mut self,
        start_block: u64,
        count: usize,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        let count_u8 = u8::try_from(count).map_err(|_| "Too many blocks requested")?;
        if buffer.len() < count * BLOCK_SIZE {
            return Err("Buffer too small");
        }
        crate::drivers::ata::write_sectors_to(self.index, start_block, count_u8, buffer)
    }

    fn block_count(&self) -> u64 {
        crate::drivers::ata::drive_info(self.index)
            .map(|info| info.total_sectors)
            .unwrap_or(0)
    }
}

/// Copies every sector from `src` to `dst` (starting at LBA 0, MBR included).
/// Calls `progress(done, total)` after each chunk. Returns sectors copied.
pub fn copy_disk(
    src: &mut dyn BlockDevice,
    dst: &mut dyn BlockDevice,
    mut progress: impl FnMut(u64, u64),
) -> Result<u64, &'static str> {
    let total = src.block_count();
    if total == 0 {
        return Err("Source disk has no sectors");
    }
    if dst.block_count() < total {
        return Err("Target disk too small for source image");
    }

    // Refuse to clone garbage: the source must look bootable (MBR signature).
    let mut mbr = [0u8; BLOCK_SIZE];
    src.read_blocks(0, 1, &mut mbr)
        .map_err(|_| "Source read failed")?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return Err("Source has no boot signature (not an MFK boot disk?)");
    }

    let mut buf: Vec<u8> = alloc::vec![0u8; CHUNK_SECTORS * BLOCK_SIZE];
    let mut lba: u64 = 0;
    while lba < total {
        let n = (total - lba).min(CHUNK_SECTORS as u64) as usize;
        let span = &mut buf[..n * BLOCK_SIZE];
        src.read_blocks(lba, n, span)
            .map_err(|_| "Source read failed")?;
        dst.write_blocks(lba, n, span)
            .map_err(|_| "Target write failed")?;
        lba += n as u64;
        progress(lba, total);
    }
    Ok(total)
}

/// Compares every sector of `dst` against `src`. Returns sectors verified.
/// The first mismatch LBA goes to the serial log; callers get a static str.
pub fn verify_disks(
    src: &mut dyn BlockDevice,
    dst: &mut dyn BlockDevice,
    mut progress: impl FnMut(u64, u64),
) -> Result<u64, &'static str> {
    let total = src.block_count();
    if total == 0 {
        return Err("Source disk has no sectors");
    }
    if dst.block_count() < total {
        return Err("Target disk too small for source image");
    }

    let mut a: Vec<u8> = alloc::vec![0u8; CHUNK_SECTORS * BLOCK_SIZE];
    let mut b: Vec<u8> = alloc::vec![0u8; CHUNK_SECTORS * BLOCK_SIZE];
    let mut lba: u64 = 0;
    while lba < total {
        let n = (total - lba).min(CHUNK_SECTORS as u64) as usize;
        src.read_blocks(lba, n, &mut a[..n * BLOCK_SIZE])
            .map_err(|_| "Source read failed")?;
        dst.read_blocks(lba, n, &mut b[..n * BLOCK_SIZE])
            .map_err(|_| "Target read failed")?;
        if a[..n * BLOCK_SIZE] != b[..n * BLOCK_SIZE] {
            crate::serial_println!("[install] verify mismatch at LBA {}", lba);
            return Err("Verification mismatch");
        }
        lba += n as u64;
        progress(lba, total);
    }
    Ok(total)
}

// ── Menu UI ─────────────────────────────────────────────

const MAIN_ITEMS: [&str; 5] = [
    "Install MFK to disk ...",
    "Format data disk (SimplFS)",
    "Verify installation ...",
    "Reboot",
    "Quit",
];

/// Short human name for a drive slot: "Primary Master", ...
fn slot_name(index: usize) -> &'static str {
    match index {
        0 => "Primary Master",
        1 => "Primary Slave",
        2 => "Secondary Master",
        3 => "Secondary Slave",
        _ => "Unknown",
    }
}

/// One-line description for the drive picker, e.g.
/// "Primary Slave: 20480 sectors (10 MB) [DATA]".
fn drive_label(index: usize) -> String {
    let base = slot_name(index);
    let Some(info) = crate::drivers::ata::drive_info(index) else {
        return format!("{}: probe data unavailable", base);
    };
    if !info.exists {
        return format!(
            "{}: absent ({})",
            base,
            info.last_error.unwrap_or("not detected")
        );
    }
    let mut label = format!(
        "{}: {} sectors ({} MB)",
        base,
        info.total_sectors,
        info.total_sectors / 2048
    );
    if index == SOURCE_INDEX {
        label.push_str(" [BOOT SOURCE - locked]");
    } else if index == 1 {
        label.push_str(" [DATA - SimplFS]");
    }
    label
}

/// Target candidates: existing drives other than the boot source.
fn target_candidates() -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..4 {
        if i == SOURCE_INDEX {
            continue;
        }
        if let Some(info) = crate::drivers::ata::drive_info(i) {
            if info.exists {
                out.push(i);
            }
        }
    }
    out
}

/// Whether the boot source looks like an MFK boot disk (present + MBR sig).
fn source_ready() -> (bool, u64) {
    let sectors = DriveBlockDevice::new(SOURCE_INDEX).block_count();
    if sectors == 0 {
        return (false, 0);
    }
    let mut mbr = [0u8; BLOCK_SIZE];
    let mut src = DriveBlockDevice::new(SOURCE_INDEX);
    if src.read_blocks(0, 1, &mut mbr).is_err() {
        return (false, 0);
    }
    (mbr[510] == 0x55 && mbr[511] == 0xAA, sectors)
}

fn clear_takeover() {
    vga::clear_screen();
    crate::serial_print!("\x1b[2J\x1b[H\x1b[0m");
    vga::hide_cursor();
}

/// Restore the normal shell screen state before returning.
fn restore_shell_screen() {
    vga::clear_screen();
    crate::serial_print!("\x1b[2J\x1b[H\x1b[0m");
    vga::set_cursor_pos(VGA_HEIGHT - 1, 0);
    vga::show_cursor();
    crate::shell::clear_interrupt();
}

/// Draw one fullscreen page: title, body lines, and a selectable list with
/// `selected` highlighted. Footer shows the navigation hint.
fn draw_page(title: &str, body: &[String], items: &[String], selected: usize, footer: &str) {
    clear_takeover();
    let mut row = 1usize;
    vga::write_str_at(row, 2, title, Color::Yellow, Color::Black);
    crate::serial_println!("");
    crate::serial_println!("== {} ==", title);
    row += 2;
    for line in body {
        if row + 1 >= VGA_HEIGHT {
            break;
        }
        let text: String = line.chars().take(VGA_WIDTH - 4).collect();
        vga::write_str_at(row, 2, &text, Color::White, Color::Black);
        crate::serial_println!("{}", text);
        row += 1;
    }
    row += 1;
    for (i, item) in items.iter().enumerate() {
        if row + 2 >= VGA_HEIGHT {
            break;
        }
        let text: String = item.chars().take(VGA_WIDTH - 6).collect();
        if i == selected {
            vga::fill_rect(row, 2, VGA_WIDTH - 4, 1, b' ', Color::White, Color::Blue);
            vga::write_str_at(row, 4, &text, Color::White, Color::Blue);
            crate::serial_println!("> {}", text);
        } else {
            vga::write_str_at(row, 4, &text, Color::LightGray, Color::Black);
            crate::serial_println!("  {}", text);
        }
        row += 1;
    }
    if row + 1 < VGA_HEIGHT {
        let hint: String = footer.chars().take(VGA_WIDTH - 4).collect();
        vga::write_str_at(VGA_HEIGHT - 2, 2, &hint, Color::DarkGray, Color::Black);
    }
    crate::serial_println!("{}", footer);
    crate::desktop::refresh_terminal_editor();
}

/// Blocking read of the next key, keeping USB/net alive (the shell loop is
/// suspended while the installer owns the screen).
fn next_key() -> KeyEvent {
    loop {
        #[cfg(feature = "usb")]
        crate::drivers::usb::poll();
        crate::net::process_packets();
        if let Some(ev) = crate::drivers::keyboard::read_key() {
            return ev;
        }
        x86_64::instructions::hlt();
    }
}

/// Simple "press any key" pause on top of the current screen.
fn wait_any_key(msg: &str) {
    crate::serial_println!("{}", msg);
    vga::write_str_at(VGA_HEIGHT - 1, 2, msg, Color::Yellow, Color::Black);
    crate::desktop::refresh_terminal_editor();
    let _ = next_key();
}

/// Y/N/Esc inline confirmation. Returns true only on Y.
fn confirm_action(summary: &[String]) -> bool {
    let items = alloc::vec![
        String::from("Yes, proceed (destructive!)"),
        String::from("No, go back"),
    ];
    let mut selected = 1usize;
    loop {
        draw_page(
            "Confirm",
            summary,
            &items,
            selected,
            "Up/Down + Enter, or Y/N, Esc cancels",
        );
        match next_key().key {
            Key::ArrowUp => selected = (selected + items.len() - 1) % items.len(),
            Key::ArrowDown => selected = (selected + 1) % items.len(),
            Key::Enter => return selected == 0,
            Key::Char('y') | Key::Char('Y') => return true,
            Key::Char('n') | Key::Char('N') | Key::Esc | Key::Ctrl('C') => return false,
            _ => {}
        }
    }
}

/// Drive picker. Returns the chosen slot index, or None on cancel.
/// Only `candidates` are selectable; every slot is shown for context.
fn pick_drive(title: &str, candidates: &[usize]) -> Option<usize> {
    if candidates.is_empty() {
        draw_page(
            title,
            &alloc::vec![
                String::from("No usable target disks found."),
                String::from("Attach another disk (e.g. Secondary Master) and retry.")
            ],
            &[],
            0,
            "Press any key to go back",
        );
        wait_any_key_no_redraw();
        return None;
    }
    let mut all: Vec<usize> = (0..4).collect();
    // Order: candidates first so arrows start on something usable.
    all.sort_by_key(|i| if candidates.contains(i) { 0 } else { 1 });
    let mut selected = 0usize;
    loop {
        let labels: Vec<String> = all.iter().map(|&i| drive_label(i)).collect();
        let body = alloc::vec![String::from("Select target disk:")];
        draw_page(
            title,
            &body,
            &labels,
            selected,
            "Up/Down/1-4 + Enter, Esc cancels",
        );
        match next_key().key {
            Key::ArrowUp => selected = (selected + all.len() - 1) % all.len(),
            Key::ArrowDown => selected = (selected + 1) % all.len(),
            Key::Enter => {
                let slot = all[selected];
                if candidates.contains(&slot) {
                    return Some(slot);
                }
            }
            Key::Char(c @ '1'..='4') => {
                let slot = (c as usize) - ('1' as usize);
                if candidates.contains(&slot) {
                    return Some(slot);
                }
            }
            Key::Esc | Key::Ctrl('C') => return None,
            _ => {}
        }
    }
}

/// wait_any_key variant that skips the footer draw (screen already complete).
fn wait_any_key_no_redraw() {
    let _ = next_key();
}

/// Copy with a live percentage display. Returns the copy result.
fn copy_with_progress(src_slot: usize, dst_slot: usize) -> Result<u64, &'static str> {
    let mut src = DriveBlockDevice::new(src_slot);
    let mut dst = DriveBlockDevice::new(dst_slot);
    let mut last_pct = u64::MAX;
    copy_disk(&mut src, &mut dst, |done, total| {
        let pct = done * 100 / total.max(1);
        if pct != last_pct {
            last_pct = pct;
            let line = format!("Copying... {}% ({}/{} sectors)", pct, done, total);
            vga::fill_rect(10, 2, VGA_WIDTH - 4, 1, b' ', Color::White, Color::Black);
            vga::write_str_at(10, 2, &line, Color::White, Color::Black);
            crate::desktop::refresh_terminal_editor();
            crate::serial_println!("[install] {}", line);
        }
    })
}

/// Verify with a live percentage display. Returns the verify result.
fn verify_with_progress(src_slot: usize, dst_slot: usize) -> Result<u64, &'static str> {
    let mut src = DriveBlockDevice::new(src_slot);
    let mut dst = DriveBlockDevice::new(dst_slot);
    let mut last_pct = u64::MAX;
    verify_disks(&mut src, &mut dst, |done, total| {
        let pct = done * 100 / total.max(1);
        if pct != last_pct {
            last_pct = pct;
            let line = format!("Verifying... {}% ({}/{} sectors)", pct, done, total);
            vga::fill_rect(10, 2, VGA_WIDTH - 4, 1, b' ', Color::White, Color::Black);
            vga::write_str_at(10, 2, &line, Color::White, Color::Black);
            crate::desktop::refresh_terminal_editor();
            crate::serial_println!("[install] {}", line);
        }
    })
}

fn action_install() {
    let (ready, sectors) = source_ready();
    if !ready {
        draw_page(
            "Install MFK",
            &alloc::vec![
                String::from("Boot source (Primary Master) not recognized:"),
                String::from("absent or missing boot signature. Cannot install."),
            ],
            &[],
            0,
            "Press any key to go back",
        );
        wait_any_key_no_redraw();
        return;
    }
    let candidates = target_candidates();
    let Some(dst) = pick_drive("Install MFK", &candidates) else {
        return;
    };
    let dst_sectors = DriveBlockDevice::new(dst).block_count();
    let summary = alloc::vec![
        format!(
            "Source: {} ({} sectors, {} MB)",
            slot_name(SOURCE_INDEX),
            sectors,
            sectors / 2048
        ),
        format!(
            "Target: {} ({} sectors, {} MB)",
            slot_name(dst),
            dst_sectors,
            dst_sectors / 2048
        ),
        String::from(""),
        String::from("ALL DATA ON THE TARGET DISK WILL BE DESTROYED."),
        String::from("The target will become a bootable MFK disk."),
    ];
    if !confirm_action(&summary) {
        return;
    }

    draw_page(
        "Install MFK",
        &alloc::vec![format!("Installing to {} ...", slot_name(dst))],
        &[],
        0,
        "Do not power off.",
    );
    match copy_with_progress(SOURCE_INDEX, dst) {
        Ok(done) => {
            crate::serial_println!("[install] copied {} sectors, verifying...", done);
            match verify_with_progress(SOURCE_INDEX, dst) {
                Ok(v) => {
                    let body = alloc::vec![
                        format!("Copied {} sectors, verified {} sectors.", done, v),
                        format!("{} is now a bootable MFK disk.", slot_name(dst)),
                        String::from("Reboot and boot from it to test."),
                    ];
                    draw_page("Install complete", &body, &[], 0, "Press any key");
                    wait_any_key_no_redraw();
                }
                Err(e) => {
                    let body = alloc::vec![
                        format!("Copied {} sectors, but verify failed: {}", done, e),
                        String::from("Do not boot from the target; retry install."),
                    ];
                    draw_page("Install FAILED", &body, &[], 0, "Press any key");
                    wait_any_key_no_redraw();
                }
            }
        }
        Err(e) => {
            let body = alloc::vec![
                format!("Install failed: {}", e),
                String::from("Target left in an unknown state; retry or re-pick."),
            ];
            draw_page("Install FAILED", &body, &[], 0, "Press any key");
            wait_any_key_no_redraw();
        }
    }
}

fn action_verify() {
    let (ready, sectors) = source_ready();
    if !ready {
        draw_page(
            "Verify installation",
            &alloc::vec![String::from(
                "Boot source not recognized; nothing to compare against."
            )],
            &[],
            0,
            "Press any key to go back",
        );
        wait_any_key_no_redraw();
        return;
    }
    // Any existing non-source disk can be checked against the source.
    let mut candidates = Vec::new();
    for i in 0..4 {
        if i == SOURCE_INDEX {
            continue;
        }
        if let Some(info) = crate::drivers::ata::drive_info(i) {
            if info.exists {
                candidates.push(i);
            }
        }
    }
    let Some(dst) = pick_drive("Verify installation", &candidates) else {
        return;
    };
    draw_page(
        "Verify installation",
        &alloc::vec![format!(
            "Comparing {} against source ({} sectors) ...",
            slot_name(dst),
            sectors
        )],
        &[],
        0,
        "Please wait.",
    );
    match verify_with_progress(SOURCE_INDEX, dst) {
        Ok(v) => {
            draw_page(
                "Verify OK",
                &alloc::vec![format!(
                    "{} matches source ({} sectors).",
                    slot_name(dst),
                    v
                )],
                &[],
                0,
                "Press any key",
            );
            wait_any_key_no_redraw();
        }
        Err(e) => {
            draw_page(
                "Verify FAILED",
                &alloc::vec![format!("{}: {}", slot_name(dst), e)],
                &[],
                0,
                "Press any key",
            );
            wait_any_key_no_redraw();
        }
    }
}

fn action_format_data() {
    // Leave fullscreen so mkfs output scrolls normally, then come back.
    restore_shell_screen();
    println!("MFK Installer: formatting data disk (Primary Slave) with SimplFS...");
    crate::shell::execute_command("mkfs");
    println!("Type 'mount' (or re-run install) to use it.");
    wait_any_key("Press any key to return to the installer...");
}

/// Installer entry point: fullscreen arrow-key menu. Returns to the shell
/// on Quit/Esc/Ctrl+C, restoring the normal screen state.
pub fn run() {
    clear_takeover();
    let items: Vec<String> = MAIN_ITEMS.iter().map(|s| String::from(*s)).collect();
    let mut selected = 0usize;
    loop {
        let (ready, sectors) = source_ready();
        let mut body = alloc::vec![String::from("Install MFK to a disk, or manage disks.")];
        if ready {
            body.push(format!(
                "Source: {} ({} sectors, {} MB).",
                slot_name(SOURCE_INDEX),
                sectors,
                sectors / 2048
            ));
        } else {
            body.push(String::from(
                "Source: boot disk not recognized (install disabled).",
            ));
        }
        draw_page(
            "MFK Installer",
            &body,
            &items,
            selected,
            "Up/Down navigate, Enter select, Esc quits",
        );
        match next_key().key {
            Key::ArrowUp => selected = (selected + items.len() - 1) % items.len(),
            Key::ArrowDown => selected = (selected + 1) % items.len(),
            Key::Enter => match selected {
                0 => action_install(),
                1 => action_format_data(),
                2 => action_verify(),
                3 => {
                    restore_shell_screen();
                    crate::shell::execute_command("reboot");
                    clear_takeover();
                }
                _ => {
                    restore_shell_screen();
                    return;
                }
            },
            Key::Char('q') | Key::Char('Q') | Key::Esc | Key::Ctrl('C') => {
                restore_shell_screen();
                return;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::block::RamDisk;

    /// RamDisk preloaded with a fake bootable image (MBR signature + pattern).
    fn bootable_ramdisk(sectors: u64) -> RamDisk {
        let mut disk = RamDisk::new(sectors);
        let mut buf: Vec<u8> = alloc::vec![0u8; sectors as usize * BLOCK_SIZE];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i.wrapping_mul(31) & 0xFF) as u8;
        }
        buf[510] = 0x55;
        buf[511] = 0xAA;
        disk.write_blocks(0, sectors as usize, &buf).unwrap();
        disk
    }

    fn null_progress(_done: u64, _total: u64) {}

    #[test]
    fn copy_clones_all_sectors() {
        let mut src = bootable_ramdisk(100);
        let mut dst = RamDisk::new(100);
        assert_eq!(copy_disk(&mut src, &mut dst, null_progress), Ok(100));
        let mut a = alloc::vec![0u8; 100 * BLOCK_SIZE];
        let mut b = alloc::vec![0u8; 100 * BLOCK_SIZE];
        src.read_blocks(0, 100, &mut a).unwrap();
        dst.read_blocks(0, 100, &mut b).unwrap();
        assert_eq!(a, b);
        assert_eq!(verify_disks(&mut src, &mut dst, null_progress), Ok(100));
    }

    #[test]
    fn copy_refuses_small_target() {
        let mut src = bootable_ramdisk(100);
        let mut dst = RamDisk::new(50);
        assert_eq!(
            copy_disk(&mut src, &mut dst, null_progress),
            Err("Target disk too small for source image")
        );
        assert_eq!(
            verify_disks(&mut src, &mut dst, null_progress),
            Err("Target disk too small for source image")
        );
    }

    #[test]
    fn copy_refuses_unsigned_source() {
        let mut src = RamDisk::new(100); // zeros: no MBR signature
        let mut dst = RamDisk::new(100);
        assert_eq!(
            copy_disk(&mut src, &mut dst, null_progress),
            Err("Source has no boot signature (not an MFK boot disk?)")
        );
    }

    #[test]
    fn copy_refuses_empty_source() {
        let mut src = RamDisk::new(0);
        let mut dst = RamDisk::new(100);
        assert_eq!(
            copy_disk(&mut src, &mut dst, null_progress),
            Err("Source disk has no sectors")
        );
    }

    #[test]
    fn verify_catches_mismatch() {
        let mut src = bootable_ramdisk(64);
        let mut dst = RamDisk::new(64);
        assert_eq!(copy_disk(&mut src, &mut dst, null_progress), Ok(64));
        // Corrupt one byte past the first chunk boundary region.
        let mut sec = [0u8; BLOCK_SIZE];
        dst.read_blocks(10, 1, &mut sec).unwrap();
        sec[0] ^= 0xFF;
        dst.write_blocks(10, 1, &sec).unwrap();
        assert_eq!(
            verify_disks(&mut src, &mut dst, null_progress),
            Err("Verification mismatch")
        );
    }
}
