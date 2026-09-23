//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.

//! Desktop Drive app: GUI wrapper around `mkfs` + `mount`.
//!
//! Same pattern as Files: buttons dispatched in `desktop::mod` via
//! widget-id comparison (no `on_click` capture needed). Format is
//! two-step confirm to avoid accidental erasure.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use alloc::string::String;

/// Drive window state (single instance, on desktop stack).
pub struct DriveApp {
    pub window: WindowId,
    pub status_widget: WidgetId,
    pub log_widget: WidgetId,
    pub btn_format: WidgetId,
    pub btn_mount: WidgetId,
    pub btn_refresh: WidgetId,
    confirm_armed: bool,
    confirm_tick: u64,
    log: String,
}

fn drive_bounds(sw: usize, sh: usize) -> Rect {
    let w = 460u32.min(sw as u32 - 40).max(360);
    let h = 300u32.min(sh as u32 - 80).max(260);
    Rect::new(
        (sw as u32).saturating_sub(w) as i32 / 2 + 120,
        (sh as u32).saturating_sub(h + 36) as i32 / 2,
        w,
        h,
    )
}

/// Create drive window + widgets.
pub fn create_drive_app(scene: &mut Scene, sw: usize, sh: usize) -> DriveApp {
    let bounds = drive_bounds(sw, sh);
    let window = scene.create_window(String::from("Drives"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;
    let w = bounds.w as i32;

    let status_widget = WidgetId::new();
    let mut sv = Widget::label(
        Rect::new(10, 6, (w - 20).max(100) as u32, 66),
        String::from(""),
        &theme,
    );
    sv.id = status_widget;
    scene.widgets.insert(status_widget, sv);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(status_widget);
    }

    let btn_y = 78;
    let ids = [WidgetId::new(), WidgetId::new(), WidgetId::new()];
    let labels = ["Format (mkfs)", "Mount", "Refresh"];
    let widths = [130u32, 90, 90];
    let mut x = 10;
    for (i, id) in ids.iter().enumerate() {
        let mut b = Widget::button(
            Rect::new(x, btn_y, widths[i], 28),
            String::from(labels[i]),
            &theme,
        );
        b.id = *id;
        scene.widgets.insert(*id, b);
        if let Some(r) = scene.widgets.get_mut(&root) {
            r.children.push(*id);
        }
        x += widths[i] as i32 + 8;
    }

    let log_widget = WidgetId::new();
    let mut lv = Widget::label(
        Rect::new(
            10,
            112,
            (w - 20).max(100) as u32,
            (bounds.h as i32 - 122).max(60) as u32,
        ),
        String::from("Format erases the disk. Mount makes it usable."),
        &theme,
    );
    lv.id = log_widget;
    scene.widgets.insert(log_widget, lv);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(log_widget);
    }

    let mut app = DriveApp {
        window,
        status_widget,
        log_widget,
        btn_format: ids[0],
        btn_mount: ids[1],
        btn_refresh: ids[2],
        confirm_armed: false,
        confirm_tick: 0,
        log: String::from("Format erases the disk. Mount makes it usable."),
    };
    refresh_drive(scene, &mut app);
    app
}

/// Refresh status label from shell bridge.
pub fn refresh_drive(scene: &mut Scene, app: &mut DriveApp) {
    let summary = crate::shell::gui_disk_summary();
    let mount = crate::shell::gui_fs_status();
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        w.text = alloc::format!("{}\n{}", summary, mount);
    }
    if let Some(w) = scene.widgets.get_mut(&app.log_widget) {
        w.text = app.log.clone();
    }
    // Update format button text to show armed state
    if let Some(b) = scene.widgets.get_mut(&app.btn_format) {
        b.text = if app.confirm_armed {
            String::from("Confirm ERASE?")
        } else {
            String::from("Format (mkfs)")
        };
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

pub fn drive_owns_button(app: &DriveApp, btn: WidgetId) -> bool {
    btn == app.btn_format || btn == app.btn_mount || btn == app.btn_refresh
}

/// Handle drive button. `now_tick` = shell tick for confirm timeout.
pub fn drive_button(scene: &mut Scene, app: &mut DriveApp, btn: WidgetId, now_tick: u64) {
    if btn == app.btn_refresh {
        app.confirm_armed = false;
        app.log = String::from("Refreshed.");
        refresh_drive(scene, app);
    } else if btn == app.btn_mount {
        app.confirm_armed = false;
        match crate::shell::gui_mount_fs() {
            Ok(msg) => app.log = alloc::format!("mount: {}", msg),
            Err(e) => app.log = alloc::format!("mount failed: {}. Try Format first.", e),
        }
        refresh_drive(scene, app);
    } else if btn == app.btn_format {
        // Two-step confirm; auto-disarm after ~8s of ticks (tick ~ms-ish loop count)
        if app.confirm_armed && now_tick.wrapping_sub(app.confirm_tick) < 8000 {
            match crate::shell::gui_format_disk() {
                Ok(msg) => app.log = alloc::format!("mkfs: {}. Now press Mount.", msg),
                Err(e) => app.log = alloc::format!("mkfs failed: {}", e),
            }
            app.confirm_armed = false;
        } else {
            app.confirm_armed = true;
            app.confirm_tick = now_tick;
            app.log =
                String::from("WARNING: Format ERASES all files. Click Format again to confirm.");
        }
        refresh_drive(scene, app);
    }
}

/// Keyboard shortcuts when drive focused: M mount, F format-arm, R refresh.
pub fn drive_key(
    scene: &mut Scene,
    app: &mut DriveApp,
    key: crate::drivers::keyboard::Key,
    now_tick: u64,
) -> bool {
    use crate::drivers::keyboard::Key;
    match key {
        Key::Char('m') | Key::Char('M') => {
            drive_button(scene, app, app.btn_mount, now_tick);
            true
        }
        Key::Char('f') | Key::Char('F') => {
            drive_button(scene, app, app.btn_format, now_tick);
            true
        }
        Key::Char('r') | Key::Char('R') => {
            drive_button(scene, app, app.btn_refresh, now_tick);
            true
        }
        Key::Enter => {
            drive_button(scene, app, app.btn_mount, now_tick);
            true
        }
        _ => false,
    }
}

/// Resize handler: stretch labels.
pub fn resize_drive_content(scene: &mut Scene, app: &DriveApp) {
    let win_w = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.w as i32,
        None => return,
    };
    let inner = (win_w - 20).max(100) as u32;
    for id in [app.status_widget, app.log_widget] {
        if let Some(w) = scene.widgets.get_mut(&id) {
            w.bounds.w = inner;
        }
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}
