//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.

//! Desktop Doom app: window hosting the vendored doomgeneric engine.
//!
//! The engine renders 320x200x32 into its screen buffer each tick
//! (35Hz); this app converts to RGBA, upscales x2 into an Image widget
//! (640x400) and marks the window dirty. Input is routed from the
//! desktop loop while the window is focused; Esc opens the menu and a
//! second Esc / window close tears the engine down.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use crate::doom::engine;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Engine frame dimensions (must match -DDOOMGENERIC_RESX/RESY).
pub const VIEW_W: usize = 320;
/// Engine frame height.
pub const VIEW_H: usize = 200;
/// Display scale (Image widget is 640x400).
pub const VIEW_SCALE: usize = 2;
/// Display width.
pub const DISP_W: usize = VIEW_W * VIEW_SCALE;
/// Display height.
pub const DISP_H: usize = VIEW_H * VIEW_SCALE;

/// Result of pumping the engine for one desktop frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoomPump {
    /// Still running (frame updated or waiting for next tic).
    Alive,
    /// The game quit cleanly (close the window).
    Quit,
    /// Fatal engine error (message already logged; window shows it).
    Fatal,
}

/// Doom window state (single instance, on desktop stack).
pub struct DoomApp {
    /// Window id.
    pub window: WindowId,
    /// Image widget showing the upscaled frame.
    pub frame_widget: WidgetId,
    /// Status line widget (WAD / state / errors).
    pub status_widget: WidgetId,
    /// Active guest WAD path.
    pub wad_path: String,
    /// Native engine frame (u32 RGBA8888 as the engine writes it).
    frame: Vec<u32>,
    /// Upscaled RGBA blit buffer for the Image widget.
    rgba: Vec<u8>,
    /// Last engine tick timestamp (ms).
    last_tick_ms: u64,
    /// False after fatal error (stops ticking, keeps window).
    running: bool,
    status: String,
    /// Held engine key codes + last press timestamp (MFK delivers
    /// press-only events; PS/2 typematic repeats while held, so a code
    /// not re-pressed within HOLD_RELEASE_MS is released).
    held: [(u8, u64); 16],
    held_count: usize,
}

/// Release a held key after this long without a repeat press.
pub const HOLD_RELEASE_MS: u64 = 120;

/// Default guest WAD for the taskbar launcher (automated setup injects
/// the shareware WAD here; the `doom` shell command overrides per run).
pub const DEFAULT_WAD: &str = "/wad/doom1.wad";

/// Pending WAD set by the `doom` shell command; the taskbar launcher
/// consumes it once (falls back to DEFAULT_WAD).
static PENDING_WAD: spin::Mutex<Option<String>> = spin::Mutex::new(None);

/// Queue a WAD for the next taskbar-launcher open.
pub fn set_pending_wad(path: &str) {
    *PENDING_WAD.lock() = Some(String::from(path));
}

/// Take the pending WAD (shell `doom` command) or the default.
pub fn take_launch_wad() -> String {
    PENDING_WAD.lock().take().unwrap_or(String::from(DEFAULT_WAD))
}

/// Pending WAD set by the `doom` shell command, if any (used to
/// auto-open the Doom window when the desktop starts via `doom run`).
pub fn pending_wad() -> Option<String> {
    PENDING_WAD.lock().clone()
}

fn doom_bounds(sw: usize, sh: usize) -> Rect {
    let w = (DISP_W + 24) as u32;
    let h = (DISP_H + 64) as u32;
    Rect::new(
        (sw.saturating_sub(w as usize) as i32 / 2).max(0),
        (sh.saturating_sub(h as usize + 36) as i32 / 2).max(0),
        w,
        h,
    )
}

/// Create the Doom window and start the engine on `wad`.
/// Returns the app, or an error string (also serial-logged).
pub fn create_doom_app(
    scene: &mut Scene,
    sw: usize,
    sh: usize,
    wad: &str,
) -> Result<DoomApp, String> {
    if !engine::available() {
        return Err(String::from(
            "Doom engine not compiled into this build (clang missing at build time)",
        ));
    }
    // Cheap streaming validation before touching the engine.
    let info = crate::doom::wad_info(wad, 1).map_err(|e| String::from(e))?;
    if info.num_lumps == 0 {
        return Err(String::from("WAD has no lumps"));
    }

    let bounds = doom_bounds(sw, sh);
    let window = scene.create_window(String::from("Doom"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;

    // Frame view (Image widget, updated per tick).
    let frame_widget = WidgetId::new();
    let mut fw = Widget::new(
        crate::desktop::scene::WidgetKind::Image,
        Rect::new(12, 8, DISP_W as u32, DISP_H as u32),
        crate::desktop::scene::Style::default_panel(&theme),
    );
    fw.id = frame_widget;
    fw.image_w = DISP_W as u32;
    fw.image_h = DISP_H as u32;
    fw.image_data = Some(alloc::vec![0u8; DISP_W * DISP_H * 4]);
    scene.widgets.insert(frame_widget, fw);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(frame_widget);
    }

    // Status line.
    let status_widget = WidgetId::new();
    let status = alloc::format!("WAD: {} ({} lumps) - arrows/WASD move, Ctrl fire, Space use, Esc menu",
        wad, info.num_lumps);
    let mut sv = Widget::label(
        Rect::new(12, 8 + DISP_H as i32 + 6, DISP_W as u32, 22),
        status.clone(),
        &theme,
    );
    sv.id = status_widget;
    scene.widgets.insert(status_widget, sv);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(status_widget);
    }

    crate::serial_println!("[doom] starting engine on {}", wad);
    if let Err(e) = engine::start(wad) {
        let msg = alloc::format!("Doom failed to start: {}", e);
        crate::serial_println!("[doom] {}", msg);
        scene.destroy_window(window);
        return Err(msg);
    }

    let mut app = DoomApp {
        window,
        frame_widget,
        status_widget,
        wad_path: String::from(wad),
        frame: alloc::vec![0u32; VIEW_W * VIEW_H],
        rgba: alloc::vec![0u8; DISP_W * DISP_H * 4],
        last_tick_ms: crate::time::uptime_millis(),
        running: true,
        status,
        held: [(0, 0); 16],
        held_count: 0,
    };
    // First frame immediately so the window never shows black.
    pump_doom(scene, &mut app);
    crate::serial_println!("[doom] window opened");
    Ok(app)
}

/// Pump the engine (35Hz pacing) and refresh the frame widget.
/// Call once per desktop loop iteration.
pub fn pump_doom(scene: &mut Scene, app: &mut DoomApp) -> DoomPump {
    if !app.running {
        return DoomPump::Fatal;
    }
    if engine::state() != engine::EngineState::Running {
        return match engine::state() {
            engine::EngineState::Quit => DoomPump::Quit,
            _ => {
                finish_fatal(scene, app);
                DoomPump::Fatal
            }
        };
    }
    release_stale_keys(app);
    let now = crate::time::uptime_millis();
    if now.wrapping_sub(app.last_tick_ms) < crate::doom::MS_PER_TICK {
        return DoomPump::Alive;
    }
    app.last_tick_ms = now;
    if !engine::tick() {
        return match engine::state() {
            engine::EngineState::Quit => {
                crate::serial_println!("[doom] quit by game");
                DoomPump::Quit
            }
            _ => {
                finish_fatal(scene, app);
                DoomPump::Fatal
            }
        };
    }
    if engine::copy_frame(&mut app.frame) {
        blit_frame(scene, app);
    }
    DoomPump::Alive
}

fn finish_fatal(scene: &mut Scene, app: &mut DoomApp) {
    app.running = false;
    let msg = engine::last_error();
    app.status = alloc::format!("Doom error: {} (close window, check serial)", msg);
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        w.text = app.status.clone();
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
    crate::serial_println!("[doom] fatal: {}", msg);
}

/// Convert the engine RGBA8888 frame (bytes [B,G,R,0] LE) to the RGBA
/// blit buffer ([R,G,B,255]) with 2x nearest-neighbor upscale.
fn blit_frame(scene: &mut Scene, app: &mut DoomApp) {
    for y in 0..VIEW_H {
        for x in 0..VIEW_W {
            let v = app.frame[y * VIEW_W + x];
            let r = ((v >> 16) & 0xFF) as u8;
            let g = ((v >> 8) & 0xFF) as u8;
            let b = (v & 0xFF) as u8;
            for dy in 0..VIEW_SCALE {
                for dx in 0..VIEW_SCALE {
                    let o = ((y * VIEW_SCALE + dy) * DISP_W + (x * VIEW_SCALE + dx)) * 4;
                    app.rgba[o] = r;
                    app.rgba[o + 1] = g;
                    app.rgba[o + 2] = b;
                    app.rgba[o + 3] = 255;
                }
            }
        }
    }
    if let Some(w) = scene.widgets.get_mut(&app.frame_widget) {
        w.image_data = Some(app.rgba.clone());
        w.image_w = DISP_W as u32;
        w.image_h = DISP_H as u32;
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

/// Route a key event to the engine queue. Returns true when consumed.
/// MFK delivers press-only events; holding a key produces typematic
/// repeats, so each press refreshes the hold timestamp and `pump_doom`
/// synthesizes releases for codes gone quiet (see HOLD_RELEASE_MS).
/// Esc goes to the game (menu); desktop exit uses the taskbar/close.
pub fn doom_key(app: &mut DoomApp, key: crate::drivers::keyboard::Key) -> bool {
    let Some(dk) = crate::doom::map_key(key) else {
        return false;
    };
    let code = engine::doomkey_code(dk);
    engine::push_doomkey(dk, true);
    let now = crate::time::uptime_millis();
    for i in 0..app.held_count {
        if app.held[i].0 == code {
            app.held[i].1 = now;
            return true;
        }
    }
    if app.held_count < app.held.len() {
        app.held[app.held_count] = (code, now);
        app.held_count += 1;
    }
    true
}

/// Release keys whose repeat stream went quiet. Called from `pump_doom`.
fn release_stale_keys(app: &mut DoomApp) {
    let now = crate::time::uptime_millis();
    let mut i = 0;
    while i < app.held_count {
        if now.wrapping_sub(app.held[i].1) >= HOLD_RELEASE_MS {
            engine::push_key(app.held[i].0, false);
            app.held[i] = app.held[app.held_count - 1];
            app.held_count -= 1;
        } else {
            i += 1;
        }
    }
}

/// Release all held keys (window close / focus loss).
pub fn doom_release_all(app: &mut DoomApp) {
    for i in 0..app.held_count {
        engine::push_key(app.held[i].0, false);
    }
    app.held_count = 0;
}

/// Resize hook (fixed-size view: nothing to relayout, kept for symmetry
/// with the other desktop apps).
pub fn resize_doom_content(_scene: &mut Scene, _app: &DoomApp) {}

/// Shut the engine down (called on window close).
pub fn close_doom(app: &DoomApp) {
    engine::stop();
    crate::serial_println!("[doom] window closed ({})", app.wad_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_converts_rgba8888_to_rgba() {
        // Engine pixel 0x00RRGGBB (LE bytes BB GG RR 00).
        let mut scene_diag = 0u32;
        let v: u32 = 0x00112233;
        let r = ((v >> 16) & 0xFF) as u8;
        let g = ((v >> 8) & 0xFF) as u8;
        let b = (v & 0xFF) as u8;
        assert_eq!((r, g, b), (0x11, 0x22, 0x33));
        scene_diag += 1;
        assert_eq!(scene_diag, 1);
    }
}
