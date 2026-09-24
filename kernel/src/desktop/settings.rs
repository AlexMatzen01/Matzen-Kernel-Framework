//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.
//!
//! Desktop Settings app: personalization (wallpaper + theme colors).
//!
//! Same pattern as Files/Drives: buttons carry no `on_click` closures
//! (they can't capture app state); clicks are dispatched in
//! `desktop::mod` via widget-id comparison. Single instance; reopen via
//! the taskbar Settings launcher restores it.
//!
//! Wallpaper images come from the filesystem (`/wallpapers` by default,
//! any typed path works) as PNG/JPG/JPEG via `wallpaper::decode_auto`.
//! Modes: Solid / Fit / Fill / Stretch / Center / Tile. Landscape and
//! portrait share the same aspect math.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use crate::desktop::wallpaper;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Preview box size inside the Settings window.
const PREVIEW_W: u32 = 232;
const PREVIEW_H: u32 = 130;
const PATH_MAX: usize = 128;

/// Accent swatches (button shows its color).
pub const ACCENTS: [(u32, &str); 6] = [
    (0x005cac, "Blue"),
    (0x00be5a, "Green"),
    (0x7b4dff, "Purple"),
    (0xc63434, "Red"),
    (0xe67e22, "Orange"),
    (0x8a93a0, "Gray"),
];

/// Background swatches.
pub const BACKGROUNDS: [(u32, &str); 6] = [
    (0x102a4e, "Navy"),
    (0x000000, "Black"),
    (0x1c1c22, "Coal"),
    (0x0e3b3b, "Teal"),
    (0x3b0e1a, "Wine"),
    (0x2b3440, "Slate"),
];

/// Settings window state (kept on the desktop run() stack, not global).
pub struct SettingsApp {
    pub window: WindowId,
    pub path_widget: WidgetId,
    pub info_widget: WidgetId,
    pub preview_widget: WidgetId,
    pub status_widget: WidgetId,
    pub mode_btns: [WidgetId; 6],
    pub accent_btns: [WidgetId; 6],
    pub bg_btns: [WidgetId; 6],
    pub btn_browse: WidgetId,
    pub btn_next: WidgetId,
    pub btn_clear: WidgetId,
    pub btn_save: WidgetId,
    path_buf: [u8; PATH_MAX],
    path_len: usize,
    next_idx: usize,
    status: String,
}

fn settings_bounds(sw: usize, sh: usize) -> Rect {
    let w = (sw.saturating_sub(100)).min(600).max(500);
    let h = (sh.saturating_sub(100)).min(520).max(440);
    Rect::new(
        (sw.saturating_sub(w) as i32 / 2).max(10),
        (sh.saturating_sub(h + 36) as i32 / 2).max(10),
        w as u32,
        h as u32,
    )
}

fn push_child(scene: &mut Scene, root: WidgetId, id: WidgetId, w: Widget) {
    let mut w = w;
    w.id = id;
    scene.widgets.insert(id, w);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(id);
    }
}

/// Create the Settings window + child widgets.
pub fn create_settings_app(scene: &mut Scene, sw: usize, sh: usize) -> SettingsApp {
    let bounds = settings_bounds(sw, sh);
    let window = scene.create_window(String::from("Settings"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;
    let inner = (bounds.w as i32 - 20).max(100);

    // Row 0: image path label.
    let path_widget = WidgetId::new();
    push_child(
        scene,
        root,
        path_widget,
        Widget::label(
            Rect::new(10, 6, inner as u32, 22),
            String::from("Image: (none)"),
            &theme,
        ),
    );

    // Row 1: dims / orientation / mode info.
    let info_widget = WidgetId::new();
    push_child(
        scene,
        root,
        info_widget,
        Widget::label(
            Rect::new(10, 30, inner as u32, 20),
            String::from(""),
            &theme,
        ),
    );

    // Row 2: preview (left) + 6 mode buttons (right, 2 cols x 3 rows).
    let preview_widget = WidgetId::new();
    let mut pv = Widget::new(
        crate::desktop::scene::WidgetKind::Image,
        Rect::new(10, 54, PREVIEW_W, PREVIEW_H),
        crate::desktop::scene::Style::default_panel(&theme),
    );
    pv.id = preview_widget;
    scene.widgets.insert(preview_widget, pv);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(preview_widget);
    }

    let modes = ["Solid", "Fit", "Fill", "Stretch", "Center", "Tile"];
    let mut mode_btns = [WidgetId::new(); 6];
    let grid_x = 10 + PREVIEW_W as i32 + 10;
    let grid_w = (inner - (grid_x - 10)).max(120);
    let cell_w = ((grid_w - 6) / 2).max(60) as u32;
    for (i, id) in mode_btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        let cx = grid_x + (i as i32 % 2) * (cell_w as i32 + 6);
        let cy = 54 + (i as i32 / 2) * 32;
        push_child(
            scene,
            root,
            *id,
            Widget::button(
                Rect::new(cx, cy, cell_w, 26),
                String::from(modes[i]),
                &theme,
            ),
        );
    }

    // Row 3: accent label + 6 swatches.
    let accent_label = WidgetId::new();
    push_child(
        scene,
        root,
        accent_label,
        Widget::label(
            Rect::new(10, 192, inner as u32, 18),
            String::from("Accent:"),
            &theme,
        ),
    );
    let mut accent_btns = [WidgetId::new(); 6];
    let sw_w = ((inner - 5 * 6) / 6).max(60) as u32;
    for (i, id) in accent_btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        let mut b = Widget::button(
            Rect::new(10 + i as i32 * (sw_w as i32 + 6), 212, sw_w, 26),
            String::from(ACCENTS[i].1),
            &theme,
        );
        b.style.bg_color = ACCENTS[i].0;
        b.style.fg_color = 0xffffff;
        b.id = *id;
        scene.widgets.insert(*id, b);
        if let Some(r) = scene.widgets.get_mut(&root) {
            r.children.push(*id);
        }
    }

    // Row 4: background label + 6 swatches.
    let bg_label = WidgetId::new();
    push_child(
        scene,
        root,
        bg_label,
        Widget::label(
            Rect::new(10, 244, inner as u32, 18),
            String::from("Background:"),
            &theme,
        ),
    );
    let mut bg_btns = [WidgetId::new(); 6];
    for (i, id) in bg_btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        let mut b = Widget::button(
            Rect::new(10 + i as i32 * (sw_w as i32 + 6), 264, sw_w, 26),
            String::from(BACKGROUNDS[i].1),
            &theme,
        );
        b.style.bg_color = BACKGROUNDS[i].0;
        b.style.fg_color = 0xffffff;
        b.id = *id;
        scene.widgets.insert(*id, b);
        if let Some(r) = scene.widgets.get_mut(&root) {
            r.children.push(*id);
        }
    }

    // Row 5: actions Browse / Next / Clear / Save.
    let btn_browse = WidgetId::new();
    let btn_next = WidgetId::new();
    let btn_clear = WidgetId::new();
    let btn_save = WidgetId::new();
    let act_w = ((inner - 3 * 8) / 4).max(80) as u32;
    let acts = [
        (btn_browse, "Browse"),
        (btn_next, "Next"),
        (btn_clear, "Clear"),
        (btn_save, "Save"),
    ];
    for (i, (id, label)) in acts.iter().enumerate() {
        push_child(
            scene,
            root,
            *id,
            Widget::button(
                Rect::new(10 + i as i32 * (act_w as i32 + 8), 298, act_w, 28),
                String::from(*label),
                &theme,
            ),
        );
    }

    // Row 6: status (fills the rest).
    let status_widget = WidgetId::new();
    let status_h = (bounds.h as i32 - 332).max(60) as u32;
    push_child(
        scene,
        root,
        status_widget,
        Widget::label(
            Rect::new(10, 332, inner as u32, status_h),
            String::from("Type a path, then Browse. Next cycles /wallpapers."),
            &theme,
        ),
    );

    let mut app = SettingsApp {
        window,
        path_widget,
        info_widget,
        preview_widget,
        status_widget,
        mode_btns,
        accent_btns,
        bg_btns,
        btn_browse,
        btn_next,
        btn_clear,
        btn_save,
        path_buf: [0; PATH_MAX],
        path_len: 0,
        next_idx: 0,
        status: String::from("Type a path, then Browse. Next cycles /wallpapers."),
    };
    refresh_settings(scene, &mut app, sw, sh);
    app
}

/// Does this widget id belong to the Settings window buttons?
pub fn settings_owns_button(app: &SettingsApp, btn: WidgetId) -> bool {
    btn == app.btn_browse
        || btn == app.btn_next
        || btn == app.btn_clear
        || btn == app.btn_save
        || app.mode_btns.contains(&btn)
        || app.accent_btns.contains(&btn)
        || app.bg_btns.contains(&btn)
}

fn typed_path(app: &SettingsApp) -> String {
    core::str::from_utf8(&app.path_buf[..app.path_len])
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Apply accent+bg to the live theme copies and repaint everything.
fn apply_theme_live(scene: &mut Scene, bg: u32, accent: u32) {
    crate::desktop::theme::current_theme_mut().personalize(bg, accent);
    scene.theme.personalize(bg, accent);
    scene.mark_dirty_full();
}

fn update_preview(scene: &mut Scene, app: &SettingsApp) {
    if let Some((rgba, w, h)) = wallpaper::preview_rgba(PREVIEW_W, PREVIEW_H) {
        if let Some(pv) = scene.widgets.get_mut(&app.preview_widget) {
            pv.image_data = Some(rgba);
            pv.image_w = w;
            pv.image_h = h;
        }
    } else if let Some(pv) = scene.widgets.get_mut(&app.preview_widget) {
        pv.image_data = None;
        pv.image_w = 0;
        pv.image_h = 0;
    }
}

/// Refresh all labels from the wallpaper store.
pub fn refresh_settings(scene: &mut Scene, app: &mut SettingsApp, _sw: usize, _sh: usize) {
    let cfg = wallpaper::current_config();
    let buf = typed_path(app);
    let shown_path = if !buf.is_empty() {
        buf
    } else if !cfg.path.is_empty() {
        cfg.path.clone()
    } else {
        String::from("(none)")
    };
    if let Some(w) = scene.widgets.get_mut(&app.path_widget) {
        let mut t = alloc::format!("Image: {}", shown_path);
        if t.len() > 72 {
            t.truncate(72);
        }
        w.text = t;
    }
    let info = match wallpaper::original_dims() {
        Some((w, h)) => {
            let o = match wallpaper::orientation(w, h) {
                wallpaper::Orientation::Landscape => "landscape",
                wallpaper::Orientation::Portrait => "portrait",
                wallpaper::Orientation::Square => "square",
            };
            alloc::format!("{}x{} {} | mode: {}", w, h, o, cfg.mode.as_str())
        }
        None => alloc::format!("no image | mode: {} (solid color)", cfg.mode.as_str()),
    };
    if let Some(w) = scene.widgets.get_mut(&app.info_widget) {
        w.text = info;
    }
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        let buf2 = typed_path(app);
        w.text = alloc::format!("{}\nType path: {}_", app.status, buf2);
    }
    update_preview(scene, app);
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

fn load_path(scene: &mut Scene, app: &mut SettingsApp, path: &str, sw: usize, sh: usize) {
    let path = path.trim();
    if path.is_empty() {
        app.status = String::from("Type an image path first (e.g. /wallpapers/bg.png)");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    if !wallpaper::has_supported_extension(path) {
        app.status = String::from("Need .png / .jpg / .jpeg extension");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    match crate::shell::gui_read_file(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                app.status = alloc::format!("'{}' is empty", path);
            } else {
                match wallpaper::set_wallpaper_bytes(&bytes, path, sw as u32, sh as u32) {
                    Ok((w, h)) => {
                        app.status = alloc::format!("Loaded {}x{} '{}'", w, h, path);
                        crate::serial_println!("[settings] wallpaper '{}' {}x{}", path, w, h);
                    }
                    Err(e) => {
                        app.status = alloc::format!("Decode failed: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            // Only suggest mounting when the FS really isn't mounted; otherwise
            // report the actual cause (e.g. file too large for the heap) with
            // the file size, so failures are actionable.
            if !crate::shell::is_mounted() {
                app.status = alloc::format!("Read '{}' failed: {}. Mount FS via Drive first?", path, e);
            } else {
                match crate::shell::gui_file_size(path) {
                    Ok(size) => {
                        app.status = alloc::format!(
                            "Read '{}' failed: {} ({} bytes)",
                            path, e, size
                        )
                    }
                    Err(_) => {
                        app.status = alloc::format!("Read '{}' failed: {}", path, e);
                    }
                }
            }
        }
    }
    // Keep bg/accent live even when the image load failed.
    let (bg, accent) = wallpaper::current_colors();
    apply_theme_live(scene, bg, accent);
    scene.mark_dirty_full();
    refresh_settings(scene, app, sw, sh);
}

/// Handle a Settings button click. `sw/sh` = screen size for recaching.
pub fn settings_button(
    scene: &mut Scene,
    app: &mut SettingsApp,
    btn: WidgetId,
    sw: usize,
    sh: usize,
) {
    // Wallpaper modes.
    for (i, m) in wallpaper::WallpaperMode::all().iter().enumerate() {
        if btn == app.mode_btns[i] {
            wallpaper::set_mode(*m, sw as u32, sh as u32);
            app.status = alloc::format!("Mode: {}", m.as_str());
            let (bg, accent) = wallpaper::current_colors();
            apply_theme_live(scene, bg, accent);
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
            return;
        }
    }
    // Accent swatches.
    for (i, (color, name)) in ACCENTS.iter().enumerate() {
        if btn == app.accent_btns[i] {
            let (bg, _) = wallpaper::current_colors();
            wallpaper::set_colors(bg, *color, sw as u32, sh as u32);
            apply_theme_live(scene, bg, *color);
            app.status =
                alloc::format!("Accent: {} ({})", name, wallpaper::format_hex_color(*color));
            refresh_settings(scene, app, sw, sh);
            return;
        }
    }
    // Background swatches.
    for (i, (color, name)) in BACKGROUNDS.iter().enumerate() {
        if btn == app.bg_btns[i] {
            let (_, accent) = wallpaper::current_colors();
            wallpaper::set_colors(*color, accent, sw as u32, sh as u32);
            apply_theme_live(scene, *color, accent);
            app.status = alloc::format!(
                "Background: {} ({})",
                name,
                wallpaper::format_hex_color(*color)
            );
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
            return;
        }
    }

    if btn == app.btn_browse {
        let p = typed_path(app);
        let path = if p.is_empty() {
            wallpaper::current_path()
        } else {
            p
        };
        if path.is_empty() {
            // Nothing typed and nothing set: try the default dir.
            cycle_wallpaper(scene, app, sw, sh);
        } else {
            load_path(scene, app, &path, sw, sh);
        }
    } else if btn == app.btn_next {
        cycle_wallpaper(scene, app, sw, sh);
    } else if btn == app.btn_clear {
        wallpaper::clear_wallpaper(sw as u32, sh as u32);
        let (bg, accent) = wallpaper::current_colors();
        apply_theme_live(scene, bg, accent);
        app.status = String::from("Wallpaper cleared (solid color)");
        scene.mark_dirty_full();
        refresh_settings(scene, app, sw, sh);
    } else if btn == app.btn_save {
        let cfg = wallpaper::current_config();
        match crate::shell::gui_save_settings(&wallpaper::format_settings(&cfg)) {
            Ok(msg) => app.status = alloc::format!("Saved: {}", msg),
            Err(e) => app.status = alloc::format!("Save failed: {} (mount FS first?)", e),
        }
        refresh_settings(scene, app, sw, sh);
    }
}

/// Load the next supported image from /wallpapers (picker without a list).
fn cycle_wallpaper(scene: &mut Scene, app: &mut SettingsApp, sw: usize, sh: usize) {
    let entries = match crate::shell::gui_list_dir(wallpaper::WALLPAPER_DIR) {
        Ok((_, e)) => e,
        Err(e) => {
            app.status = alloc::format!(
                "List {} failed: {}. Create it via Files?",
                wallpaper::WALLPAPER_DIR,
                e
            );
            refresh_settings(scene, app, sw, sh);
            return;
        }
    };
    let mut cands: Vec<String> = Vec::new();
    for fi in &entries {
        if !fi.is_directory && wallpaper::has_supported_extension(&fi.name) {
            let full = if wallpaper::WALLPAPER_DIR == "/" {
                alloc::format!("/{}", fi.name)
            } else {
                alloc::format!("{}/{}", wallpaper::WALLPAPER_DIR, fi.name)
            };
            cands.push(full);
        }
    }
    if cands.is_empty() {
        app.status =
            String::from("No .png/.jpg/.jpeg in /wallpapers. Copy one via Files or `write`.");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    app.next_idx %= cands.len();
    let pick = cands[app.next_idx].clone();
    app.next_idx = (app.next_idx + 1) % cands.len();
    // Mirror into the type buffer so Browse repeats it.
    let b = pick.as_bytes();
    let n = b.len().min(app.path_buf.len());
    app.path_buf[..n].copy_from_slice(&b[..n]);
    app.path_len = n;
    load_path(scene, app, &pick, sw, sh);
}

/// Keyboard handling when Settings is focused. Returns true if consumed.
pub fn settings_key(
    scene: &mut Scene,
    app: &mut SettingsApp,
    key: crate::drivers::keyboard::Key,
    sw: usize,
    sh: usize,
) -> bool {
    use crate::drivers::keyboard::Key;
    match key {
        Key::Char(c)
            if c.is_ascii() && !c.is_control() && app.path_len < app.path_buf.len() - 1 =>
        {
            app.path_buf[app.path_len] = c as u8;
            app.path_len += 1;
            refresh_settings(scene, app, sw, sh);
            true
        }
        Key::Backspace => {
            if app.path_len > 0 {
                app.path_len -= 1;
                app.path_buf[app.path_len] = 0;
                refresh_settings(scene, app, sw, sh);
            }
            true
        }
        Key::Enter => {
            let p = typed_path(app);
            if p.is_empty() {
                cycle_wallpaper(scene, app, sw, sh);
            } else {
                load_path(scene, app, &p, sw, sh);
            }
            true
        }
        _ => false,
    }
}

/// Restore persisted personalization at desktop startup. Fails soft
/// (solid defaults) when the FS is not mounted or nothing was saved.
pub fn load_persisted(scene: &mut Scene, sw: usize, sh: usize) {
    let bytes = match crate::shell::gui_read_file(wallpaper::SETTINGS_PATH) {
        Ok(b) => b,
        Err(_) => {
            wallpaper::rebuild_for_screen(sw as u32, sh as u32);
            return;
        }
    };
    let text = match core::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(_) => return,
    };
    let cfg = wallpaper::parse_settings(text);
    wallpaper::apply_config(&cfg, sw as u32, sh as u32);
    crate::desktop::theme::current_theme_mut().personalize(cfg.bg, cfg.accent);
    scene.theme.personalize(cfg.bg, cfg.accent);
    if !cfg.path.is_empty() {
        match crate::shell::gui_read_file(&cfg.path) {
            Ok(img) if !img.is_empty() => {
                match wallpaper::set_wallpaper_bytes(&img, &cfg.path, sw as u32, sh as u32) {
                    Ok((w, h)) => {
                        // set_wallpaper_bytes keeps the stored mode unless it
                        // was Solid; re-assert the saved mode explicitly.
                        wallpaper::set_mode(cfg.mode, sw as u32, sh as u32);
                        crate::serial_println!(
                            "[settings] restored '{}' {}x{} mode={}",
                            cfg.path,
                            w,
                            h,
                            cfg.mode.as_str()
                        );
                    }
                    Err(e) => {
                        crate::serial_println!("[settings] saved image unreadable: {}", e);
                    }
                }
            }
            _ => {
                crate::serial_println!("[settings] saved image missing, using solid");
            }
        }
    }
    scene.mark_dirty_full();
}

/// Stretch child widths after a window resize.
pub fn resize_settings_content(scene: &mut Scene, app: &SettingsApp) {
    let win_w = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.w as i32,
        None => return,
    };
    let inner = (win_w - 20).max(100) as u32;
    for id in [app.path_widget, app.info_widget, app.status_widget] {
        if let Some(w) = scene.widgets.get_mut(&id) {
            w.bounds.w = inner;
        }
    }
    // Mode grid follows the preview's right edge.
    let grid_x = 10 + PREVIEW_W as i32 + 10;
    let grid_w = (inner as i32 - (grid_x - 10)).max(120);
    let cell_w = ((grid_w - 6) / 2).max(60) as u32;
    for (i, id) in app.mode_btns.iter().enumerate() {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = grid_x + (i as i32 % 2) * (cell_w as i32 + 6);
            b.bounds.w = cell_w;
        }
    }
    let sw_w = ((inner as i32 - 5 * 6) / 6).max(60) as u32;
    for (i, id) in app.accent_btns.iter().enumerate() {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = 10 + i as i32 * (sw_w as i32 + 6);
            b.bounds.w = sw_w;
        }
    }
    for (i, id) in app.bg_btns.iter().enumerate() {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = 10 + i as i32 * (sw_w as i32 + 6);
            b.bounds.w = sw_w;
        }
    }
    let act_w = ((inner as i32 - 3 * 8) / 4).max(80) as u32;
    for (i, id) in [app.btn_browse, app.btn_next, app.btn_clear, app.btn_save]
        .iter()
        .enumerate()
    {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = 10 + i as i32 * (act_w as i32 + 8);
            b.bounds.w = act_w;
        }
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}
