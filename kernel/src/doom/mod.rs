//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Doom compatibility layer (classic doomgeneric-first path to Doom64).
//!
//! v1 scope: WAD inspection over the streaming FS range API (no whole-file
//! load), software framebuffer helpers (320x200 paletted -> RGBA), input
//! mapping from MFK `Key` events, PIT-based timing, silent audio stubs.
//!
//! The full doomgeneric game loop drops in later behind these shims; this
//! module proves the WAD pipeline (extra disk -> SimplFS -> lump reads) and
//! the desktop blit path first.
//!
//! The vendored engine lives behind [`engine`] (C sources in
//! `vendor/doomgeneric`, platform layer in `kernel/doomc`).

pub mod engine;

use alloc::string::String;
use alloc::vec::Vec;

/// Classic Doom software framebuffer size (doomgeneric standard).
pub const DOOM_W: usize = 320;
/// Classic Doom software framebuffer size (doomgeneric standard).
pub const DOOM_H: usize = 200;
/// Pixels per frame.
pub const DOOM_PIXELS: usize = DOOM_W * DOOM_H;

/// Doom game tick rate (35 Hz).
pub const TICK_HZ: u64 = 35;
/// Milliseconds per game tick.
pub const MS_PER_TICK: u64 = 1000 / TICK_HZ;

/// WAD magic for commercial Doom (`IWAD`) and patches (`PWAD`).
pub const WAD_MAGIC_IWAD: &[u8; 4] = b"IWAD";
/// WAD magic for patch WADs.
pub const WAD_MAGIC_PWAD: &[u8; 4] = b"PWAD";

/// WAD header (12 bytes, little-endian), read via range API.
#[derive(Debug, Clone, Copy)]
pub struct WadHeader {
    /// `IWAD` or `PWAD`.
    pub magic: [u8; 4],
    /// Number of lumps in the directory.
    pub num_lumps: u32,
    /// File offset of the lump directory.
    pub dir_offset: u32,
}

/// One WAD directory entry (16 bytes, little-endian).
#[derive(Debug, Clone)]
pub struct WadLump {
    /// File offset of lump data.
    pub file_pos: u32,
    /// Lump size in bytes.
    pub size: u32,
    /// Lump name (up to 8 ASCII chars, NUL-padded).
    pub name: [u8; 8],
}

impl WadLump {
    /// Lump name as a string (trailing NULs trimmed).
    pub fn name_str(&self) -> String {
        let len = self
            .name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..len]).into_owned()
    }
}

/// Summary returned by `wad_info` for the `doominfo` shell command.
#[derive(Debug, Clone)]
pub struct WadInfo {
    /// `IWAD` or `PWAD`.
    pub magic: [u8; 4],
    /// Total lumps in the directory.
    pub num_lumps: u32,
    /// File size in bytes (from the mounted FS inode).
    pub file_size: u64,
    /// First few lump names (bounded, for display).
    pub first_lumps: Vec<String>,
}

fn read_exact_at(path: &str, offset: u64, out: &mut [u8]) -> Result<(), &'static str> {
    if !crate::shell::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let mut device = crate::shell::mounted_device();
    let mut done = 0usize;
    while done < out.len() {
        let n = crate::shell::read_file_chunk(
            path,
            &mut device,
            offset + done as u64,
            &mut out[done..],
        )?;
        if n == 0 {
            return Err("Unexpected EOF reading WAD");
        }
        done += n;
    }
    Ok(())
}

/// Parse the WAD header + first directory entries via streaming range reads.
/// Never loads the whole WAD: safe for multi-MB files on extra disks.
pub fn wad_info(path: &str, preview: usize) -> Result<WadInfo, &'static str> {
    if !crate::shell::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let mut device = crate::shell::mounted_device();
    let file_size = {
        let guard_size = crate::shell::mounted_file_size(path)?;
        guard_size
    };

    let mut hdr = [0u8; 12];
    read_exact_at(path, 0, &mut hdr)?;
    let magic: [u8; 4] = [hdr[0], hdr[1], hdr[2], hdr[3]];
    if &magic != WAD_MAGIC_IWAD && &magic != WAD_MAGIC_PWAD {
        return Err("Not a Doom WAD (bad IWAD/PWAD magic)");
    }
    let num_lumps = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let dir_offset = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    if num_lumps == 0 || num_lumps > 10_000 {
        return Err("WAD lump count out of range");
    }
    if (dir_offset as u64) >= file_size {
        return Err("WAD directory offset beyond EOF");
    }

    let show = (preview.min(16) as u32).min(num_lumps);
    let mut first_lumps = Vec::new();
    let mut entry = [0u8; 16];
    for i in 0..show {
        let off = dir_offset as u64 + (i as u64) * 16;
        read_exact_at(path, off, &mut entry)?;
        let mut name = [0u8; 8];
        name.copy_from_slice(&entry[8..16]);
        let lump = WadLump {
            file_pos: u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]),
            size: u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]),
            name,
        };
        // Skip device re-resolve: read_exact_at handles it.
        let _ = &mut device;
        first_lumps.push(lump.name_str());
    }
    Ok(WadInfo {
        magic,
        num_lumps,
        file_size,
        first_lumps,
    })
}

/// Parse a full header struct (for engine use; validates magic).
pub fn wad_header(path: &str) -> Result<WadHeader, &'static str> {
    let mut hdr = [0u8; 12];
    read_exact_at(path, 0, &mut hdr)?;
    let magic: [u8; 4] = [hdr[0], hdr[1], hdr[2], hdr[3]];
    if &magic != WAD_MAGIC_IWAD && &magic != WAD_MAGIC_PWAD {
        return Err("Not a Doom WAD (bad IWAD/PWAD magic)");
    }
    Ok(WadHeader {
        magic,
        num_lumps: u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]),
        dir_offset: u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]),
    })
}

/// Read one lump's bytes by directory index (streaming; caller bounds `max`).
pub fn wad_lump_by_index(
    path: &str,
    header: &WadHeader,
    index: u32,
    max: usize,
) -> Result<(WadLump, Vec<u8>), &'static str> {
    if index >= header.num_lumps {
        return Err("Lump index out of range");
    }
    let mut entry = [0u8; 16];
    read_exact_at(
        path,
        header.dir_offset as u64 + index as u64 * 16,
        &mut entry,
    )?;
    let lump = WadLump {
        file_pos: u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]),
        size: u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]),
        name: {
            let mut n = [0u8; 8];
            n.copy_from_slice(&entry[8..16]);
            n
        },
    };
    let take = (lump.size as usize).min(max);
    let mut data = Vec::new();
    data.try_reserve_exact(take)
        .map_err(|_| "Out of memory reading lump")?;
    data.resize(take, 0);
    let mut done = 0usize;
    while done < take {
        if !crate::shell::is_mounted() {
            return Err("Filesystem not mounted");
        }
        let mut device = crate::shell::mounted_device();
        let n = crate::shell::read_file_chunk(
            path,
            &mut device,
            lump.file_pos as u64 + done as u64,
            &mut data[done..],
        )?;
        if n == 0 {
            break;
        }
        done += n;
    }
    data.truncate(done);
    Ok((lump, data))
}

// ── Framebuffer helpers ──────────────────────────────────────────────

/// Convert an 8-bit paletted Doom frame to RGBA (opaque) for `blit_rgba`.
/// `frame` must be `DOOM_PIXELS` bytes; `palette` 256×3 RGB bytes.
pub fn paletted_to_rgba(frame: &[u8], palette: &[[u8; 3]; 256], out: &mut [u8]) -> Result<(), &'static str> {
    if frame.len() < DOOM_PIXELS {
        return Err("Frame smaller than 320x200");
    }
    if out.len() < DOOM_PIXELS * 4 {
        return Err("RGBA buffer too small");
    }
    for (i, &px) in frame.iter().take(DOOM_PIXELS).enumerate() {
        let rgb = palette[px as usize];
        out[i * 4] = rgb[0];
        out[i * 4 + 1] = rgb[1];
        out[i * 4 + 2] = rgb[2];
        out[i * 4 + 3] = 255;
    }
    Ok(())
}

/// Nearest-neighbor ×2 upscale (320×200 → 640×400) for desktop windows.
pub fn upscale_x2_rgba(src: &[u8], dst: &mut [u8]) -> Result<(), &'static str> {
    if src.len() < DOOM_PIXELS * 4 || dst.len() < DOOM_PIXELS * 4 * 4 {
        return Err("Upscale buffer too small");
    }
    for y in 0..DOOM_H {
        for x in 0..DOOM_W {
            let s = (y * DOOM_W + x) * 4;
            for dy in 0..2 {
                for dx in 0..2 {
                    let d = ((y * 2 + dy) * (DOOM_W * 2) + (x * 2 + dx)) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
        }
    }
    Ok(())
}

// ── Input mapping ────────────────────────────────────────────────────

/// Engine-level Doom buttons (doomgeneric `DG_GetKey` style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoomKey {
    /// Fire / attack.
    Fire,
    /// Use / open door.
    Use,
    /// Move forward.
    Forward,
    /// Move backward.
    Back,
    /// Turn left.
    Left,
    /// Turn right.
    Right,
    /// Strafe modifier.
    Strafe,
    /// Strafe left.
    StrafeLeft,
    /// Strafe right.
    StrafeRight,
    /// Run modifier.
    Run,
    /// Menu / escape.
    Menu,
    /// Weapon slots 1-7.
    Weapon(u8),
}

/// Map an MFK key event to a Doom button. Returns `None` for unbound keys.
/// Binding: arrows + WASD move, `Ctrl` fire, `Space` use, `Shift` run,
/// `Alt` strafe, `1-7` weapons, `Esc` menu.
pub fn map_key(key: crate::drivers::keyboard::Key) -> Option<DoomKey> {
    use crate::drivers::keyboard::Key;
    match key {
        Key::Ctrl(_) => Some(DoomKey::Fire),
        Key::Char(' ') => Some(DoomKey::Use),
        Key::ArrowUp | Key::Char('w') | Key::Char('W') => Some(DoomKey::Forward),
        Key::ArrowDown | Key::Char('s') | Key::Char('S') => Some(DoomKey::Back),
        Key::ArrowLeft | Key::Char('a') | Key::Char('A') => Some(DoomKey::Left),
        Key::ArrowRight | Key::Char('d') | Key::Char('D') => Some(DoomKey::Right),
        Key::Char(',') => Some(DoomKey::StrafeLeft),
        Key::Char('.') => Some(DoomKey::StrafeRight),
        Key::Char('1'..='7') => {
            let digit = match key {
                Key::Char(c) => c as u8 - b'0',
                _ => 1,
            };
            Some(DoomKey::Weapon(digit))
        }
        Key::Esc => Some(DoomKey::Menu),
        _ => None,
    }
}

// ── Timing + audio stubs ─────────────────────────────────────────────

/// Milliseconds since boot (engine clock source).
pub fn ticks_ms() -> u64 {
    crate::time::uptime_millis()
}

/// Busy-wait sleep used by the engine frame pacer (cooperative: pumps net).
pub fn sleep_ms(ms: u32) {
    crate::drivers::pit::sleep_ms(ms);
}

/// Silent audio backend (v1): all SFX/music calls are no-ops by design.
/// A PC-speaker/HDA driver plugs in here later without touching game code.
pub mod audio {
    /// Initialize audio (silent v1: no-op).
    pub fn init() {}
    /// Start a sound effect (silent v1: no-op).
    pub fn play_sfx(_id: u32) {}
    /// Start music track (silent v1: no-op).
    pub fn play_music(_id: u32) {}
    /// Stop all audio (silent v1: no-op).
    pub fn stop() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paletted_to_rgba_maps_palette() {
        let mut pal = [[0u8; 3]; 256];
        pal[1] = [10, 20, 30];
        let mut frame = [0u8; DOOM_PIXELS];
        frame[0] = 1;
        let mut out = [0u8; DOOM_PIXELS * 4];
        paletted_to_rgba(&frame, &pal, &mut out).unwrap();
        assert_eq!(&out[0..4], &[10, 20, 30, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn upscale_x2_replicates_pixels() {
        let mut src = [0u8; DOOM_PIXELS * 4];
        src[0..4].copy_from_slice(&[9, 8, 7, 255]);
        let mut dst = [0u8; DOOM_PIXELS * 4 * 4];
        upscale_x2_rgba(&src, &mut dst).unwrap();
        // Top-left 2x2 block replicates src[0].
        assert_eq!(&dst[0..4], &[9, 8, 7, 255]);
        assert_eq!(&dst[4..8], &[9, 8, 7, 255]);
        let row = DOOM_W * 2 * 4;
        assert_eq!(&dst[row..row + 4], &[9, 8, 7, 255]);
    }

    #[test]
    fn keymap_covers_core_bindings() {
        use crate::drivers::keyboard::Key;
        assert_eq!(map_key(Key::ArrowUp), Some(DoomKey::Forward));
        assert_eq!(map_key(Key::Char(' ')), Some(DoomKey::Use));
        assert_eq!(map_key(Key::Esc), Some(DoomKey::Menu));
        assert_eq!(map_key(Key::Char('3')), Some(DoomKey::Weapon(3)));
        assert_eq!(map_key(Key::Tab), None);
    }
}
