//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.
//!
//! Subtle window animation engine for the desktop.
//!
//! CPU-only, integer math, dirty-rect friendly:
//! - Open: scale 82% -> 100% from center + rise 10px, 170ms easeOutCubic.
//! - Close: shrink to 82% + sink, then destroy (ghost kept alive).
//! - Minimize: slide/scale toward taskbar launcher, then hide.
//! - Restore: reverse of minimize.
//! - Boot fade: first ~300ms overlays black that steps out.
//! All driven by `shell::get_tick_count()` ms; `tick()` interpolates
//! `Window.bounds` directly and marks old+new dirty.

use crate::desktop::scene::{Rect, Scene, WindowId};
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    Open,
    Close,
    Minimize,
    Restore,
}

struct Anim {
    win: WindowId,
    kind: AnimKind,
    t0: u64,
    dur: u64,
    from: Rect,
    to: Rect,
    /// Launcher target for minimize (for dirty coverage at the end).
    target: Option<Rect>,
    close_after: bool,
    hide_after: bool,
}

static ANIMS: Mutex<Vec<Anim>> = Mutex::new(Vec::new());
static BOOT_T0: Mutex<Option<u64>> = Mutex::new(None);

const OPEN_MS: u64 = 170;
const CLOSE_MS: u64 = 150;
const MIN_MS: u64 = 200;

/// easeOutCubic in 0..256 fixed point: 1-(1-t)^3.
fn ease_out(t01: u32) -> u32 {
    let t = t01.min(256);
    let u = 256 - t;
    // 256 - u^3/256^2
    256 - (u * u * u) / (256 * 256)
}

fn lerp(a: i32, b: i32, t256: u32) -> i32 {
    a + ((b - a) * t256 as i32) / 256
}

fn lerp_rect(from: Rect, to: Rect, t256: u32) -> Rect {
    // Interpolate center + size so windows grow from their center.
    let fc = (from.x + from.w as i32 / 2, from.y + from.h as i32 / 2);
    let tc = (to.x + to.w as i32 / 2, to.y + to.h as i32 / 2);
    let cx = lerp(fc.0, tc.0, t256);
    let cy = lerp(fc.1, tc.1, t256);
    let w = lerp(from.w as i32, to.w as i32, t256).max(40) as u32;
    let h = lerp(from.h as i32, to.h as i32, t256).max(40) as u32;
    Rect::new(cx - w as i32 / 2, cy - h as i32 / 2, w, h)
}

fn centered_scaled(r: Rect, num: u32, den: u32) -> Rect {
    let w = (r.w * num / den).max(40);
    let h = (r.h * num / den).max(40);
    Rect::new(
        r.x + (r.w as i32 - w as i32) / 2,
        r.y + (r.h as i32 - h as i32) / 2,
        w,
        h,
    )
}

fn is_animating(win: WindowId) -> bool {
    ANIMS.lock().iter().any(|a| a.win == win)
}

fn push_anim(a: Anim) {
    let mut q = ANIMS.lock();
    q.retain(|e| e.win != a.win);
    q.push(a);
}

/// Begin boot fade (call once on desktop entry).
pub fn boot_begin() {
    *BOOT_T0.lock() = Some(crate::shell::get_tick_count());
}

/// 0..255 black overlay alpha for the boot fade; 0 when done (~320ms).
pub fn boot_overlay_alpha() -> u8 {
    let Some(t0) = *BOOT_T0.lock() else {
        return 0;
    };
    let dt = crate::shell::get_tick_count().wrapping_sub(t0);
    if dt >= 320 {
        return 0;
    }
    // Step in 4 bands to keep present cost down (no per-frame alpha).
    let step = dt / 80; // 0..3
    (255 - step * 64) as u8
}

/// Animate a newly created window from 82% centered to its bounds.
pub fn start_open(scene: &mut Scene, win: WindowId) {
    let Some(w) = scene.windows.get(&win) else {
        return;
    };
    let to = w.bounds;
    let mut from = centered_scaled(to, 82, 100);
    from.y += 10; // rise effect
    let t0 = crate::shell::get_tick_count();
    // Snap to start immediately so first frame shows motion.
    if let Some(wm) = scene.windows.get(&win) {
        let old = wm.bounds;
        scene.mark_dirty(old);
    }
    scene.set_window_bounds(win, from);
    scene.mark_dirty(from);
    scene.mark_dirty(to);
    push_anim(Anim {
        win,
        kind: AnimKind::Open,
        t0,
        dur: OPEN_MS,
        from,
        to,
        target: None,
        close_after: false,
        hide_after: false,
    });
}

/// Animate close: shrink toward center, destroy at end.
/// Returns true if an animation was started (caller must NOT destroy yet).
pub fn start_close(scene: &mut Scene, win: WindowId) -> bool {
    if is_animating(win) {
        return false;
    }
    let Some(w) = scene.windows.get(&win) else {
        return false;
    };
    if w.restore_bounds.is_some() {
        return false; // maximized: fade path would need alpha; skip
    }
    let from = w.bounds;
    let mut to = centered_scaled(from, 82, 100);
    to.y += 8;
    push_anim(Anim {
        win,
        kind: AnimKind::Close,
        t0: crate::shell::get_tick_count(),
        dur: CLOSE_MS,
        from,
        to,
        target: None,
        close_after: true,
        hide_after: false,
    });
    scene.mark_dirty(from);
    scene.mark_dirty(to);
    true
}

/// Minimize toward a taskbar launcher rect; hides at end.
pub fn start_minimize(scene: &mut Scene, win: WindowId, launcher: Rect) {
    if is_animating(win) {
        // Fall back to instant hide to avoid stuck ghosts.
        scene.minimize_window(win);
        return;
    }
    let Some(w) = scene.windows.get(&win) else {
        return;
    };
    let from = w.bounds;
    push_anim(Anim {
        win,
        kind: AnimKind::Minimize,
        t0: crate::shell::get_tick_count(),
        dur: MIN_MS,
        from,
        to: launcher,
        target: Some(launcher),
        close_after: false,
        hide_after: true,
    });
    scene.mark_dirty(from);
}

/// Restore from launcher to full bounds.
pub fn start_restore(scene: &mut Scene, win: WindowId, launcher: Rect) {
    // Make visible first at the launcher, then grow.
    if let Some(w) = scene.windows.get_mut(&win) {
        w.visible = true;
        w.minimized = false;
    } else {
        return;
    }
    let to = scene.windows.get(&win).map(|w| w.bounds).unwrap_or(launcher);
    scene.set_window_bounds(win, launcher);
    scene.mark_dirty(launcher);
    scene.mark_dirty(to);
    // Focus after making visible (focus_window skips invisible).
    scene.focus_window(win);
    push_anim(Anim {
        win,
        kind: AnimKind::Restore,
        t0: crate::shell::get_tick_count(),
        dur: MIN_MS,
        from: launcher,
        to,
        target: None,
        close_after: false,
        hide_after: false,
    });
}

/// Advance all animations. Returns true if anything is still active
/// (caller should present even without other dirty rects).
/// Finished close windows are destroyed here; finished minimizes hidden.
pub fn tick(scene: &mut Scene) -> bool {
    let now = crate::shell::get_tick_count();
    // Drain finished work after the loop to avoid borrow issues.
    let mut destroy: Vec<WindowId> = alloc::vec::Vec::new();
    let mut hide: Vec<WindowId> = alloc::vec::Vec::new();
    let active = {
        let mut q = ANIMS.lock();
        if q.is_empty() {
            return false;
        }
        let mut i = 0;
        while i < q.len() {
            let a = &q[i];
            let dt = now.wrapping_sub(a.t0);
            let done = dt >= a.dur;
            let t = if done {
                256
            } else {
                ease_out((dt * 256 / a.dur.max(1)) as u32)
            };
            // For close, easing runs forward (shrink); others lerp from->to.
            let rect = lerp_rect(a.from, a.to, t);
            let win = a.win;
            let kind = a.kind;
            let target = a.target;
            if done {
                if a.close_after {
                    destroy.push(win);
                } else if a.hide_after {
                    hide.push(win);
                } else if kind == AnimKind::Restore || kind == AnimKind::Open {
                    // Snap exact final bounds.
                    if scene.windows.get(&win).is_some() {
                        let old = scene.windows.get(&win).map(|w| w.bounds);
                        scene.set_window_bounds(win, a.to);
                        if let Some(o) = old {
                            scene.mark_dirty(o);
                        }
                        scene.mark_dirty(a.to);
                    }
                } else if kind == AnimKind::Minimize {
                    hide.push(win);
                }
                let _ = target;
                q.remove(i);
            } else {
                if scene.windows.get(&win).is_some() {
                    let old = scene.windows.get(&win).map(|w| w.bounds).unwrap();
                    scene.set_window_bounds(win, rect);
                    scene.mark_dirty(old);
                    scene.mark_dirty(rect);
                    // Minimize path also dirties the launcher so the
                    // taskbar pill highlights during the slide.
                    if let Some(t) = target {
                        scene.mark_dirty(t);
                    }
                } else {
                    q.remove(i);
                    continue;
                }
                i += 1;
            }
        }
        !q.is_empty()
    };
    let did_hide = !hide.is_empty();
    let did_destroy = !destroy.is_empty();
    for win in hide {
        // Direct hide without extra focus churn (we already focused on restore).
        if let Some(w) = scene.windows.get_mut(&win) {
            let b = w.bounds;
            w.visible = false;
            w.minimized = true;
            w.focused = false;
            scene.mark_dirty(b);
        }
        if scene.focused_window == Some(win) {
            scene.focused_window = None;
        }
    }
    for win in destroy {
        scene.destroy_window(win);
    }
    // Return true if work remains OR we just finished some (one more present).
    active || did_hide || did_destroy
}

/// Cancel any animation for a window (e.g. it was force-destroyed).
pub fn cancel(win: WindowId) {
    ANIMS.lock().retain(|a| a.win != win);
}
