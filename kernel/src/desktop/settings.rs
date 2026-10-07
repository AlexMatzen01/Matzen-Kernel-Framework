//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.
//!
//! Desktop Settings app: personalization (wallpaper + theme colors +
//! mouse cursor).
//!
//! Same pattern as Files/Drives: buttons carry no `on_click` closures
//! (they can't capture app state); clicks are dispatched in
//! `desktop::mod` via widget-id comparison. Single instance; reopen via
//! the taskbar Settings launcher restores it.
//!
//! Two tab pages share one window: Wallpaper and Cursor. Wallpaper images
//! come from the filesystem (`/wallpapers` by default, any typed path
//! works) as PNG/JPG/JPEG/BMP/GIF via `wallpaper::decode_auto`.
//! Modes: Solid / Fit / Fill / Stretch / Center / Tile. Landscape and
//! portrait share the same aspect math.
//!
//! Cursors come kernel-bundled (every image in `cursors/`, converted by
//! `tools/convert_cursor.py`) or from `/cursors` files (PNG/JPG/JPEG/BMP/
//! GIF via `wallpaper::decode_cursor`, max 128px). Hotspot defaults to
//! top-left unless a `<name>.hotspot` sidecar (`x,y`) is present.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use crate::desktop::{cursor, wallpaper};
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

/// Active tab page of the Settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Wallpaper,
    Cursor,
}

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
    tab_wallpaper: WidgetId,
    tab_cursor: WidgetId,
    accent_label: WidgetId,
    bg_label: WidgetId,
    cursor_info: WidgetId,
    cursor_preview: WidgetId,
    cursor_prev: WidgetId,
    cursor_next: WidgetId,
    cursor_hint: WidgetId,
    page: SettingsPage,
    path_buf: [u8; PATH_MAX],
    path_len: usize,
    next_idx: usize,
    cursor_next_idx: usize,
    status: String,
}

/// Height of the tab row; page content is shifted down by this.
const TAB_SHIFT: i32 = 34;

fn settings_bounds(sw: usize, sh: usize) -> Rect {
    let w = (sw.saturating_sub(100)).min(600).max(500);
    let h = (sh.saturating_sub(100)).min(554).max(474);
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
///
/// Row 0 is the Wallpaper/Cursor tab switcher; the remaining rows belong to
/// the active page (the other page's widgets are hidden via `visible`).
pub fn create_settings_app(scene: &mut Scene, sw: usize, sh: usize) -> SettingsApp {
    let bounds = settings_bounds(sw, sh);
    let window = scene.create_window(String::from("Settings"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;
    let inner = (bounds.w as i32 - 20).max(100);
    let s = TAB_SHIFT;

    // Row 0: tab switcher (shared by both pages).
    let tab_wallpaper = WidgetId::new();
    let tab_cursor = WidgetId::new();
    let tab_w = ((inner - 8) / 2).max(80) as u32;
    push_child(
        scene,
        root,
        tab_wallpaper,
        Widget::button(Rect::new(10, 6, tab_w, 24), String::from("Wallpaper"), &theme),
    );
    push_child(
        scene,
        root,
        tab_cursor,
        Widget::button(
            Rect::new(10 + tab_w as i32 + 8, 6, tab_w, 24),
            String::from("Cursor"),
            &theme,
        ),
    );

    // Row 1: image path label (text depends on the active page).
    let path_widget = WidgetId::new();
    push_child(
        scene,
        root,
        path_widget,
        Widget::label(
            Rect::new(10, 6 + s, inner as u32, 22),
            String::from("Image: (none)"),
            &theme,
        ),
    );

    // Row 2: dims / orientation / mode info (wallpaper page).
    let info_widget = WidgetId::new();
    push_child(
        scene,
        root,
        info_widget,
        Widget::label(
            Rect::new(10, 30 + s, inner as u32, 20),
            String::from(""),
            &theme,
        ),
    );

    // Row 3: preview (left) + 6 mode buttons (right, 2 cols x 3 rows).
    let preview_widget = WidgetId::new();
    let mut pv = Widget::new(
        crate::desktop::scene::WidgetKind::Image,
        Rect::new(10, 54 + s, PREVIEW_W, PREVIEW_H),
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
        let cy = 54 + s + (i as i32 / 2) * 32;
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

    // Row 4: accent label + 6 swatches.
    let accent_label = WidgetId::new();
    push_child(
        scene,
        root,
        accent_label,
        Widget::label(
            Rect::new(10, 192 + s, inner as u32, 18),
            String::from("Accent:"),
            &theme,
        ),
    );
    let mut accent_btns = [WidgetId::new(); 6];
    let sw_w = ((inner - 5 * 6) / 6).max(60) as u32;
    for (i, id) in accent_btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        let mut b = Widget::button(
            Rect::new(10 + i as i32 * (sw_w as i32 + 6), 212 + s, sw_w, 26),
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

    // Row 5: background label + 6 swatches.
    let bg_label = WidgetId::new();
    push_child(
        scene,
        root,
        bg_label,
        Widget::label(
            Rect::new(10, 244 + s, inner as u32, 18),
            String::from("Background:"),
            &theme,
        ),
    );
    let mut bg_btns = [WidgetId::new(); 6];
    for (i, id) in bg_btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        let mut b = Widget::button(
            Rect::new(10 + i as i32 * (sw_w as i32 + 6), 264 + s, sw_w, 26),
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

    // Cursor page: selection info + preview + bundled Prev/Next + hint.
    // Same content area as the wallpaper rows above (mutually exclusive).
    let cursor_info = WidgetId::new();
    push_child(
        scene,
        root,
        cursor_info,
        Widget::label(
            Rect::new(10, 30 + s, inner as u32, 20),
            String::from(""),
            &theme,
        ),
    );
    let cursor_preview = WidgetId::new();
    let mut cp = Widget::new(
        crate::desktop::scene::WidgetKind::Image,
        Rect::new(10, 54 + s, PREVIEW_W, PREVIEW_H),
        crate::desktop::scene::Style::default_panel(&theme),
    );
    cp.id = cursor_preview;
    scene.widgets.insert(cursor_preview, cp);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(cursor_preview);
    }
    let cursor_prev = WidgetId::new();
    let cursor_next = WidgetId::new();
    push_child(
        scene,
        root,
        cursor_prev,
        Widget::button(Rect::new(grid_x, 54 + s, cell_w, 26), String::from("< Prev"), &theme),
    );
    push_child(
        scene,
        root,
        cursor_next,
        Widget::button(
            Rect::new(grid_x, 86 + s, cell_w, 26),
            String::from("Next >"),
            &theme,
        ),
    );
    let cursor_hint = WidgetId::new();
    push_child(
        scene,
        root,
        cursor_hint,
        Widget::label(
            Rect::new(10, 192 + s, inner as u32, 96),
            String::from(
                "Bundled cursors ship in the kernel (add images to\ncursors/ + re-run convert_cursor.py).\nOr drop PNG/JPG/BMP/GIF in /cursors;\noptional <name>.hotspot file holds `x,y`.",
            ),
            &theme,
        ),
    );

    // Row 6: actions Browse / Next / Clear / Save (shared; Browse/Next/
    // Clear act on the active page, Save persists wallpaper + cursor).
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
                Rect::new(10 + i as i32 * (act_w as i32 + 8), 298 + s, act_w, 28),
                String::from(*label),
                &theme,
            ),
        );
    }

    // Row 7: status (fills the rest, shared).
    let status_widget = WidgetId::new();
    let status_h = (bounds.h as i32 - 332 - s).max(60) as u32;
    push_child(
        scene,
        root,
        status_widget,
        Widget::label(
            Rect::new(10, 332 + s, inner as u32, status_h),
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
        tab_wallpaper,
        tab_cursor,
        accent_label,
        bg_label,
        cursor_info,
        cursor_preview,
        cursor_prev,
        cursor_next,
        cursor_hint,
        page: SettingsPage::Wallpaper,
        path_buf: [0; PATH_MAX],
        path_len: 0,
        next_idx: 0,
        cursor_next_idx: 0,
        status: String::from("Type a path, then Browse. Next cycles /wallpapers."),
    };
    apply_page_visibility(scene, &app);
    refresh_settings(scene, &mut app, sw, sh);
    app
}

/// Show the active page's widgets, hide the other's.
fn set_visible(scene: &mut Scene, id: WidgetId, visible: bool) {
    if let Some(w) = scene.widgets.get_mut(&id) {
        w.visible = visible;
    }
}

fn apply_page_visibility(scene: &mut Scene, app: &SettingsApp) {
    let wp = app.page == SettingsPage::Wallpaper;
    for id in [app.info_widget, app.preview_widget] {
        set_visible(scene, id, wp);
    }
    for id in app.mode_btns {
        set_visible(scene, id, wp);
    }
    for id in app.accent_btns {
        set_visible(scene, id, wp);
    }
    for id in app.bg_btns {
        set_visible(scene, id, wp);
    }
    set_visible(scene, app.accent_label, wp);
    set_visible(scene, app.bg_label, wp);
    set_visible(scene, app.cursor_info, !wp);
    set_visible(scene, app.cursor_preview, !wp);
    set_visible(scene, app.cursor_prev, !wp);
    set_visible(scene, app.cursor_next, !wp);
    set_visible(scene, app.cursor_hint, !wp);
}

/// Does this widget id belong to the Settings window buttons?
pub fn settings_owns_button(app: &SettingsApp, btn: WidgetId) -> bool {
    btn == app.btn_browse
        || btn == app.btn_next
        || btn == app.btn_clear
        || btn == app.btn_save
        || btn == app.tab_wallpaper
        || btn == app.tab_cursor
        || btn == app.cursor_prev
        || btn == app.cursor_next
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

fn update_cursor_preview(scene: &mut Scene, app: &SettingsApp) {
    if let Some((rgba, w, h)) = cursor::preview_rgba(PREVIEW_W, PREVIEW_H) {
        if let Some(pv) = scene.widgets.get_mut(&app.cursor_preview) {
            pv.image_data = Some(rgba);
            pv.image_w = w;
            pv.image_h = h;
        }
    } else if let Some(pv) = scene.widgets.get_mut(&app.cursor_preview) {
        pv.image_data = None;
        pv.image_w = 0;
        pv.image_h = 0;
    }
}

/// Refresh all labels from the wallpaper/cursor stores.
pub fn refresh_settings(scene: &mut Scene, app: &mut SettingsApp, _sw: usize, _sh: usize) {
    let cfg = wallpaper::current_config();
    let buf = typed_path(app);
    if app.page == SettingsPage::Cursor {
        let shown = if !buf.is_empty() {
            buf
        } else {
            cursor::describe()
        };
        if let Some(w) = scene.widgets.get_mut(&app.path_widget) {
            let mut t = alloc::format!("Cursor: {}", shown);
            if t.len() > 72 {
                t.truncate(72);
            }
            w.text = t;
        }
        if let Some(w) = scene.widgets.get_mut(&app.cursor_info) {
            let total = cursor::bundled_count();
            let pos = cursor::selected_index().map(|i| i + 1).unwrap_or(0);
            w.text = if pos > 0 {
                alloc::format!("bundled {}/{} | {}", pos, total, cursor::describe())
            } else {
                alloc::format!("custom file | {}", cursor::describe())
            };
        }
        update_cursor_preview(scene, app);
    } else {
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
        update_preview(scene, app);
    }
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        let buf2 = typed_path(app);
        w.text = alloc::format!("{}\nType path: {}_", app.status, buf2);
    }
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
        app.status = String::from("Need .png / .jpg / .jpeg / .bmp / .gif extension");
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

    // Tab switcher.
    if btn == app.tab_wallpaper {
        if app.page != SettingsPage::Wallpaper {
            app.page = SettingsPage::Wallpaper;
            apply_page_visibility(scene, app);
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
        }
        return;
    }
    if btn == app.tab_cursor {
        if app.page != SettingsPage::Cursor {
            app.page = SettingsPage::Cursor;
            apply_page_visibility(scene, app);
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
        }
        return;
    }
    // Bundled cursor Prev/Next.
    if btn == app.cursor_prev {
        step_bundled_cursor(scene, app, sw, sh, false);
        return;
    }
    if btn == app.cursor_next {
        step_bundled_cursor(scene, app, sw, sh, true);
        return;
    }

    if btn == app.btn_browse {
        if app.page == SettingsPage::Cursor {
            let p = typed_path(app);
            if p.is_empty() {
                cycle_cursor(scene, app, sw, sh);
            } else {
                load_cursor_path(scene, app, &p, sw, sh);
            }
        } else {
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
        }
    } else if btn == app.btn_next {
        if app.page == SettingsPage::Cursor {
            cycle_cursor(scene, app, sw, sh);
        } else {
            cycle_wallpaper(scene, app, sw, sh);
        }
    } else if btn == app.btn_clear {
        if app.page == SettingsPage::Cursor {
            cursor::select_default();
            app.status = alloc::format!("Cursor: {} (default)", cursor::default_name());
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
        } else {
            wallpaper::clear_wallpaper(sw as u32, sh as u32);
            let (bg, accent) = wallpaper::current_colors();
            apply_theme_live(scene, bg, accent);
            app.status = String::from("Wallpaper cleared (solid color)");
            scene.mark_dirty_full();
            refresh_settings(scene, app, sw, sh);
        }
    } else if btn == app.btn_save {
        let cfg = wallpaper::current_config();
        match crate::shell::gui_save_settings(&wallpaper::format_settings(&cfg)) {
            Ok(msg) => app.status = alloc::format!("Saved: {}", msg),
            Err(e) => app.status = alloc::format!("Save failed: {} (mount FS first?)", e),
        }
        refresh_settings(scene, app, sw, sh);
    }
}

/// Step through the kernel-bundled cursors (wraps around).
fn step_bundled_cursor(
    scene: &mut Scene,
    app: &mut SettingsApp,
    sw: usize,
    sh: usize,
    forward: bool,
) {
    let total = cursor::bundled_count();
    if total == 0 {
        app.status = String::from("No bundled cursors");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    let cur = cursor::selected_index().unwrap_or(if forward { total - 1 } else { 0 });
    let next = if forward {
        (cur + 1) % total
    } else {
        (cur + total - 1) % total
    };
    cursor::select_bundled(next);
    app.status = alloc::format!("Cursor: {}", cursor::describe());
    crate::serial_println!("[settings] cursor '{}'", cursor::describe());
    scene.mark_dirty_full();
    refresh_settings(scene, app, sw, sh);
}

/// Sibling `<name>.hotspot` sidecar path for a cursor image path
/// (`/cursors/arrow.png` -> `/cursors/arrow.hotspot`).
fn hotspot_path_for(path: &str) -> Option<String> {
    let t = path.trim();
    if t.is_empty() {
        return None;
    }
    let dot = t.rfind('.')?;
    // Only strip real image extensions; keep dots in directory names.
    let slash = t.rfind('/').unwrap_or(0);
    if dot < slash {
        return None;
    }
    Some(alloc::format!("{}.hotspot", &t[..dot]))
}

fn read_cursor_hotspot(path: &str) -> (u32, u32) {
    let Some(hp) = hotspot_path_for(path) else {
        return (0, 0);
    };
    match crate::shell::gui_read_file(&hp) {
        Ok(bytes) => match core::str::from_utf8(&bytes) {
            Ok(t) => cursor::parse_hotspot(t).unwrap_or((0, 0)),
            Err(_) => (0, 0),
        },
        Err(_) => (0, 0),
    }
}

/// Read-failure status shared by wallpaper + cursor loaders.
fn read_fail_status(path: &str) -> String {
    if !crate::shell::is_mounted() {
        alloc::format!(
            "Read '{}' failed. Mount FS via Drive first?",
            path,
        )
    } else {
        match crate::shell::gui_file_size(path) {
            Ok(size) => alloc::format!("Read '{}' failed ({} bytes)", path, size),
            Err(e) => alloc::format!("Read '{}' failed: {}", path, e),
        }
    }
}

/// Load a cursor: bundled name match wins (no FS needed), otherwise a
/// filesystem image (PNG/JPG/BMP/GIF, max 128px) plus optional sidecar.
fn load_cursor_path(scene: &mut Scene, app: &mut SettingsApp, path: &str, sw: usize, sh: usize) {
    let path = path.trim();
    if path.is_empty() {
        app.status = String::from("Type a cursor path first (e.g. /cursors/arrow.png)");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    if let Some(idx) = cursor::find_bundled(path) {
        cursor::select_bundled(idx);
        app.status = alloc::format!("Cursor: {}", cursor::describe());
        crate::serial_println!("[settings] cursor '{}'", cursor::describe());
        scene.mark_dirty_full();
        refresh_settings(scene, app, sw, sh);
        return;
    }
    if !wallpaper::has_supported_extension(path) {
        app.status = String::from("Need .png / .jpg / .jpeg / .bmp / .gif extension");
        refresh_settings(scene, app, sw, sh);
        return;
    }
    match crate::shell::gui_read_file(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                app.status = alloc::format!("'{}' is empty", path);
            } else {
                let (hx, hy) = read_cursor_hotspot(path);
                match cursor::set_custom_bytes(&bytes, path, hx, hy) {
                    Ok((w, h)) => {
                        app.status = alloc::format!(
                            "Cursor {}x{} '{}' hotspot {},{}",
                            w, h, path, hx, hy
                        );
                        crate::serial_println!(
                            "[settings] cursor '{}' {}x{} hotspot {},{}",
                            path, w, h, hx, hy
                        );
                    }
                    Err(e) => {
                        app.status = alloc::format!("Cursor decode failed: {}", e);
                    }
                }
            }
        }
        Err(_) => {
            app.status = read_fail_status(path);
        }
    }
    scene.mark_dirty_full();
    refresh_settings(scene, app, sw, sh);
}

/// Load the next supported cursor image from /cursors (picker without a list).
fn cycle_cursor(scene: &mut Scene, app: &mut SettingsApp, sw: usize, sh: usize) {
    let entries = match crate::shell::gui_list_dir(cursor::CURSOR_DIR) {
        Ok((_, e)) => e,
        Err(e) => {
            // No /cursors dir: fall back to stepping bundled cursors so Next
            // always does something useful.
            if cursor::bundled_count() > 0 {
                step_bundled_cursor(scene, app, sw, sh, true);
            } else {
                app.status = alloc::format!(
                    "List {} failed: {}. Create it via Files?",
                    cursor::CURSOR_DIR,
                    e
                );
                refresh_settings(scene, app, sw, sh);
            }
            return;
        }
    };
    let mut cands: Vec<String> = Vec::new();
    for fi in &entries {
        if !fi.is_directory && wallpaper::has_supported_extension(&fi.name) {
            cands.push(alloc::format!("{}/{}", cursor::CURSOR_DIR, fi.name));
        }
    }
    if cands.is_empty() {
        // Filesystem dir exists but holds no images: step bundled instead.
        step_bundled_cursor(scene, app, sw, sh, true);
        return;
    }
    app.cursor_next_idx %= cands.len();
    let pick = cands[app.cursor_next_idx].clone();
    app.cursor_next_idx = (app.cursor_next_idx + 1) % cands.len();
    // Mirror into the type buffer so Browse repeats it.
    let b = pick.as_bytes();
    let n = b.len().min(app.path_buf.len());
    app.path_buf[..n].copy_from_slice(&b[..n]);
    app.path_len = n;
    load_cursor_path(scene, app, &pick, sw, sh);
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
        app.status = String::from(
            "No .png/.jpg/.bmp/.gif in /wallpapers. Copy one via Files or `write`.",
        );
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
            if app.page == SettingsPage::Cursor {
                if p.is_empty() {
                    cycle_cursor(scene, app, sw, sh);
                } else {
                    load_cursor_path(scene, app, &p, sw, sh);
                }
            } else if p.is_empty() {
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
    restore_cursor(&cfg.cursor);
    scene.mark_dirty_full();
}

/// Restore the persisted cursor selection. Bundled names apply without the
/// FS; filesystem paths need a mounted drive (fail soft to the default).
fn restore_cursor(saved: &str) {
    let t = saved.trim();
    if t.is_empty() {
        cursor::select_default();
        return;
    }
    if let Some(idx) = cursor::find_bundled(t) {
        cursor::select_bundled(idx);
        crate::serial_println!("[settings] restored cursor '{}'", cursor::describe());
        return;
    }
    // Filesystem cursor: candidates are the saved path itself and
    // `/cursors/<saved>` for bare filenames from older saves.
    let mut cands: Vec<String> = Vec::new();
    if t.starts_with('/') {
        cands.push(String::from(t));
    } else {
        cands.push(alloc::format!("{}/{}", cursor::CURSOR_DIR, t));
    }
    for path in &cands {
        let img = match crate::shell::gui_read_file(path) {
            Ok(b) if !b.is_empty() => b,
            _ => continue,
        };
        let (hx, hy) = match hotspot_path_for(path) {
            Some(hp) => match crate::shell::gui_read_file(&hp) {
                Ok(hb) => core::str::from_utf8(&hb)
                    .ok()
                    .and_then(cursor::parse_hotspot)
                    .unwrap_or((0, 0)),
                Err(_) => (0, 0),
            },
            None => (0, 0),
        };
        match cursor::set_custom_bytes(&img, path, hx, hy) {
            Ok((w, h)) => {
                crate::serial_println!(
                    "[settings] restored cursor '{}' {}x{} hotspot {},{}",
                    path, w, h, hx, hy
                );
                return;
            }
            Err(e) => {
                crate::serial_println!("[settings] saved cursor unreadable: {}", e);
            }
        }
    }
    crate::serial_println!("[settings] saved cursor missing, using default");
}

/// Stretch child widths after a window resize.
pub fn resize_settings_content(scene: &mut Scene, app: &SettingsApp) {
    let win_w = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.w as i32,
        None => return,
    };
    let inner = (win_w - 20).max(100) as u32;
    for id in [
        app.path_widget,
        app.info_widget,
        app.status_widget,
        app.cursor_info,
        app.cursor_hint,
    ] {
        if let Some(w) = scene.widgets.get_mut(&id) {
            w.bounds.w = inner;
        }
    }
    // Tab buttons split the row.
    let tab_w = ((inner as i32 - 8) / 2).max(80) as u32;
    if let Some(b) = scene.widgets.get_mut(&app.tab_wallpaper) {
        b.bounds.w = tab_w;
    }
    if let Some(b) = scene.widgets.get_mut(&app.tab_cursor) {
        b.bounds.x = 10 + tab_w as i32 + 8;
        b.bounds.w = tab_w;
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
    if let Some(b) = scene.widgets.get_mut(&app.cursor_prev) {
        b.bounds.x = grid_x;
        b.bounds.w = cell_w;
    }
    if let Some(b) = scene.widgets.get_mut(&app.cursor_next) {
        b.bounds.x = grid_x;
        b.bounds.w = cell_w;
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
