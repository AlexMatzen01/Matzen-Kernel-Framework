//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.

//! Desktop Drive app: GUI wrapper around indexed `mkfs` + `mount` + `install`.
//!
//! Same pattern as Files: buttons dispatched in `desktop::mod` via
//! widget-id comparison (no `on_click` capture needed). Format and
//! Install are two-step confirms to avoid accidental erasure.
//!
//! Drive selection covers the unified index space (0-3 ATA IDE,
//! 4+ virtio-blk): Up/Down arrows or digit keys `1`-`8` pick the disk,
//! then Format/Mount/Install act on it.

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
    pub btn_install: WidgetId,
    pub selected_drive: usize,
    confirm_armed: bool,
    confirm_tick: u64,
    install_armed: bool,
    log: String,
}

fn drive_bounds(sw: usize, sh: usize) -> Rect {
    let w = 480u32.min(sw as u32 - 40).max(380);
    let h = 380u32.min(sh as u32 - 80).max(320);
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
        Rect::new(10, 6, (w - 20).max(100) as u32, 150),
        String::from(""),
        &theme,
    );
    sv.id = status_widget;
    scene.widgets.insert(status_widget, sv);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(status_widget);
    }

    let btn_y = 162;
    let ids = [WidgetId::new(), WidgetId::new(), WidgetId::new(), WidgetId::new()];
    let labels = ["Format (mkfs)", "Mount", "Refresh", "Install..."];
    let widths = [130u32, 80, 90, 100];
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
            196,
            (w - 20).max(100) as u32,
            (bounds.h as i32 - 206).max(60) as u32,
        ),
        String::from("Pick a disk (Up/Down or 1-8). Format erases it. Mount makes it usable."),
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
        btn_install: ids[3],
        selected_drive: crate::shell::mounted_drive(),
        confirm_armed: false,
        confirm_tick: 0,
        install_armed: false,
        log: String::from("Pick a disk (Up/Down or 1-8). Format erases it. Mount makes it usable."),
    };
    // Clamp: the mounted drive may exceed the probed count early in boot.
    clamp_selection(&mut app);
    refresh_drive(scene, &mut app);
    app
}

fn clamp_selection(app: &mut DriveApp) {
    let count = crate::drivers::drives::drive_count().max(1);
    if app.selected_drive >= count {
        app.selected_drive = count - 1;
    }
}

/// One-line marker list of all drives for the status label.
fn drive_list_text(selected: usize) -> String {
    let mut out = String::from("Drives (0-3 ATA, 4+ virtio):");
    for i in 0..crate::drivers::drives::drive_count() {
        out.push('\n');
        out.push_str(if i == selected { "> " } else { "  " });
        out.push_str(&crate::drivers::drives::drive_label(i));
    }
    out
}

/// Refresh status label from shell bridge.
pub fn refresh_drive(scene: &mut Scene, app: &mut DriveApp) {
    clamp_selection(app);
    let mount = crate::shell::gui_fs_status();
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        w.text = alloc::format!("{}\n{}", drive_list_text(app.selected_drive), mount);
    }
    if let Some(w) = scene.widgets.get_mut(&app.log_widget) {
        w.text = app.log.clone();
    }
    // Update destructive button texts to show armed state
    if let Some(b) = scene.widgets.get_mut(&app.btn_format) {
        b.text = if app.confirm_armed {
            String::from("Confirm ERASE?")
        } else {
            String::from("Format (mkfs)")
        };
    }
    if let Some(b) = scene.widgets.get_mut(&app.btn_install) {
        b.text = if app.install_armed {
            String::from("Confirm INSTALL?")
        } else {
            String::from("Install...")
        };
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

pub fn drive_owns_button(app: &DriveApp, btn: WidgetId) -> bool {
    btn == app.btn_format
        || btn == app.btn_mount
        || btn == app.btn_refresh
        || btn == app.btn_install
}

/// Interior pixel rect of the drive window for fullscreen tool mirroring.
fn drive_interior(scene: &Scene, app: &DriveApp) -> Option<(usize, usize, usize, usize)> {
    let win = scene.windows.get(&app.window)?;
    let border = scene.theme.metrics.window_border as usize;
    let title = scene.theme.metrics.titlebar_height as usize;
    Some((
        win.bounds.x.max(0) as usize + border + 6,
        win.bounds.y.max(0) as usize + title + 4,
        (win.bounds.w as usize).saturating_sub(border * 2 + 12),
        (win.bounds.h as usize).saturating_sub(title + border + 8),
    ))
}

/// Handle drive button. `now_tick` = shell tick for confirm timeout.
pub fn drive_button(scene: &mut Scene, app: &mut DriveApp, btn: WidgetId, now_tick: u64) {
    if btn == app.btn_refresh {
        crate::drivers::drives::rescan_all_silent();
        app.confirm_armed = false;
        app.install_armed = false;
        clamp_selection(app);
        app.log = String::from("Re-probed buses. Pick a disk with Up/Down or 1-8.");
        refresh_drive(scene, app);
    } else if btn == app.btn_mount {
        app.confirm_armed = false;
        app.install_armed = false;
        let sel = app.selected_drive;
        match crate::shell::gui_mount_fs(sel) {
            Ok(msg) => app.log = alloc::format!("mount: {}", msg),
            Err(e) => app.log = alloc::format!("mount drive {} failed: {}. Try Format first.", sel, e),
        }
        refresh_drive(scene, app);
    } else if btn == app.btn_format {
        app.install_armed = false;
        let sel = app.selected_drive;
        // Two-step confirm; auto-disarm after ~8s of ticks (tick ~ms-ish loop count)
        if app.confirm_armed && now_tick.wrapping_sub(app.confirm_tick) < 8000 {
            match crate::shell::gui_format_disk(sel) {
                Ok(msg) => app.log = alloc::format!("mkfs: {}. Now press Mount.", msg),
                Err(e) => app.log = alloc::format!("mkfs drive {} failed: {}", sel, e),
            }
            app.confirm_armed = false;
        } else {
            app.confirm_armed = true;
            app.confirm_tick = now_tick;
            app.log = alloc::format!(
                "WARNING: Format ERASES drive {}. Click Format again to confirm.",
                sel
            );
        }
        refresh_drive(scene, app);
    } else if btn == app.btn_install {
        app.confirm_armed = false;
        let sel = app.selected_drive;
        if sel == crate::drivers::drives::BOOT_DRIVE {
            app.install_armed = false;
            app.log = String::from("Drive 0 is the boot source itself; pick another target.");
            refresh_drive(scene, app);
            return;
        }
        if app.install_armed {
            app.install_armed = false;
            // Mirror the fullscreen installer into this window while it runs.
            if let Some(rect) = drive_interior(scene, app) {
                crate::desktop::run_fullscreen_in(rect, || {
                    crate::install::run_with_args(&alloc::format!("{}", sel));
                });
            } else {
                crate::install::run_with_args(&alloc::format!("{}", sel));
            }
            app.log = alloc::format!("Installer finished for drive {}. See serial log.", sel);
        } else {
            app.install_armed = true;
            app.log = alloc::format!(
                "Install MFK to drive {}? ALL DATA THERE WILL BE DESTROYED. Click Install again to proceed.",
                sel
            );
        }
        refresh_drive(scene, app);
    }
}

/// Select an explicit drive index (digit keys).
pub fn drive_select(app: &mut DriveApp, index: usize) -> bool {
    if index >= crate::drivers::drives::drive_count() {
        return false;
    }
    app.selected_drive = index;
    app.confirm_armed = false;
    app.install_armed = false;
    true
}

/// Keyboard shortcuts when drive focused: arrows/digits select,
/// M mount, F format-arm, I install-arm, R refresh.
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
        Key::Char('i') | Key::Char('I') => {
            drive_button(scene, app, app.btn_install, now_tick);
            true
        }
        Key::Char('r') | Key::Char('R') => {
            drive_button(scene, app, app.btn_refresh, now_tick);
            true
        }
        Key::Char(c @ '1'..='8') => {
            let idx = (c as usize) - ('1' as usize);
            if drive_select(app, idx) {
                app.log = alloc::format!("Selected drive {}.", idx);
                refresh_drive(scene, app);
            }
            true
        }
        Key::ArrowUp => {
            let count = crate::drivers::drives::drive_count().max(1);
            app.selected_drive = (app.selected_drive + count - 1) % count;
            app.confirm_armed = false;
            app.install_armed = false;
            refresh_drive(scene, app);
            true
        }
        Key::ArrowDown => {
            let count = crate::drivers::drives::drive_count().max(1);
            app.selected_drive = (app.selected_drive + 1) % count;
            app.confirm_armed = false;
            app.install_armed = false;
            refresh_drive(scene, app);
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
