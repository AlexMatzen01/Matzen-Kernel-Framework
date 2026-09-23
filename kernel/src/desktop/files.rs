//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.

//! Desktop File Explorer app.
//!
//! Native GUI window reusing the shell FS bridge (`crate::shell::gui_*`).
//! Design constraints (no_std, 8x16 text, Label-only list):
//! - Directory listing rendered as one multi-line Label, paged via scroll.
//! - Single `name_buf` typed when window focused; used by New File/Dir.
//! - Buttons have no `on_click` closures (they can't capture app state);
//!   clicks are dispatched in `desktop::mod` via widget-id comparison.
//! - Single instance; reopen via taskbar restores it.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Visible list rows per page (16px rows).
pub const EXPLORER_PAGE: usize = 14;
const NAME_MAX: usize = 48;
const PREVIEW_MAX: usize = 384;

/// Explorer window state (kept on desktop run() stack, not global).
pub struct ExplorerApp {
    pub window: WindowId,
    pub path_widget: WidgetId,
    pub list_widget: WidgetId,
    pub status_widget: WidgetId,
    pub btn_up: WidgetId,
    pub btn_refresh: WidgetId,
    pub btn_open: WidgetId,
    pub btn_new_file: WidgetId,
    pub btn_new_dir: WidgetId,
    pub btn_delete: WidgetId,
    pub cur_path: String,
    pub entries: Vec<crate::fs::FileInfo>,
    pub selected: Option<usize>,
    pub scroll: usize,
    name_buf: [u8; 64],
    name_len: usize,
    status: String,
    auto_counter: u32,
}

fn explorer_bounds(sw: usize, sh: usize) -> Rect {
    let w = (sw.saturating_sub(120)).min(680).max(480);
    let h = (sh.saturating_sub(120)).min(460).max(340);
    Rect::new(
        (sw.saturating_sub(w) as i32 / 2).max(10),
        (sh.saturating_sub(h + 36) as i32 / 2).max(10),
        w as u32,
        h as u32,
    )
}

/// Create explorer window + child widgets. Returns app with first listing loaded.
pub fn create_explorer_app(scene: &mut Scene, sw: usize, sh: usize) -> ExplorerApp {
    let bounds = explorer_bounds(sw, sh);
    let window = scene.create_window(String::from("Files"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;

    // Layout is window-relative (root origin is below titlebar).
    let w = bounds.w as i32;
    // Row 0: path label (full width)
    let path_widget = WidgetId::new();
    let mut pw = Widget::label(
        Rect::new(10, 6, (w - 20).max(100) as u32, 22),
        String::from("/"),
        &theme,
    );
    pw.id = path_widget;
    scene.widgets.insert(path_widget, pw);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(path_widget);
    }
    // Row 1: buttons (6 x ~100px)
    let btn_y = 30;
    let btn_w = 100u32;
    let btn_h = 26u32;
    let labels = ["Up", "Refresh", "Open", "New File", "New Dir", "Delete"];
    let mut ids = [
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
    ];
    for (i, id) in ids.iter_mut().enumerate() {
        // Re-mint so each button gets a fresh id (initial array values discarded).
        *id = WidgetId::new();
        let mut b = Widget::button(
            Rect::new(10 + i as i32 * (btn_w as i32 + 6), btn_y, btn_w, btn_h),
            String::from(labels[i]),
            &theme,
        );
        b.id = *id;
        scene.widgets.insert(*id, b);
        if let Some(r) = scene.widgets.get_mut(&root) {
            r.children.push(*id);
        }
    }
    // Row 2: file list (left, ~60%) and preview/status (right stacked below list?)
    // Simple vertical stack: list, then status (2 lines incl. name buffer).
    let list_y = 62;
    let list_h = (bounds.h as i32 - list_y - 70).max(120) as u32;
    let list_widget = WidgetId::new();
    let mut lw = Widget::label(
        Rect::new(10, list_y, (w - 20).max(100) as u32, list_h),
        String::from("(loading)"),
        &theme,
    );
    lw.id = list_widget;
    scene.widgets.insert(list_widget, lw);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(list_widget);
    }
    let status_widget = WidgetId::new();
    let mut sw_ = Widget::label(
        Rect::new(10, list_y + list_h as i32 + 4, (w - 20).max(100) as u32, 56),
        String::from(""),
        &theme,
    );
    sw_.id = status_widget;
    scene.widgets.insert(status_widget, sw_);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(status_widget);
    }

    let mut app = ExplorerApp {
        window,
        path_widget,
        list_widget,
        status_widget,
        btn_up: ids[0],
        btn_refresh: ids[1],
        btn_open: ids[2],
        btn_new_file: ids[3],
        btn_new_dir: ids[4],
        btn_delete: ids[5],
        cur_path: String::from("/"),
        entries: Vec::new(),
        selected: None,
        scroll: 0,
        name_buf: [0; 64],
        name_len: 0,
        status: String::from(""),
        auto_counter: 1,
    };
    refresh_explorer(scene, &mut app);
    app
}

fn join_path(base: &str, name: &str) -> String {
    if base == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

fn parent_of(path: &str) -> String {
    let t = path.trim_end_matches('/');
    if t.is_empty() || t == "/" {
        return String::from("/");
    }
    match t.rfind('/') {
        Some(0) => String::from("/"),
        Some(i) => String::from(&t[..i]),
        None => String::from("/"),
    }
}

fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    !name.contains('/') && !name.contains('\\') && !name.as_bytes().contains(&0)
}

/// Reload listing via shell bridge; updates all three labels.
pub fn refresh_explorer(scene: &mut Scene, app: &mut ExplorerApp) {
    match crate::shell::gui_list_dir(&app.cur_path) {
        Ok((disp, entries)) => {
            app.cur_path = disp;
            app.entries = entries;
            // Clamp selection/scroll
            if app.entries.is_empty() {
                app.selected = None;
                app.scroll = 0;
            } else {
                if let Some(s) = app.selected {
                    if s >= app.entries.len() {
                        app.selected = Some(app.entries.len() - 1);
                    }
                }
                if app.scroll + EXPLORER_PAGE > app.entries.len()
                    && app.entries.len() > EXPLORER_PAGE
                {
                    app.scroll = app.entries.len() - EXPLORER_PAGE;
                } else if app.entries.len() <= EXPLORER_PAGE {
                    app.scroll = 0;
                }
            }
            if app.status.is_empty() {
                app.status = String::from("Arrows select, Enter opens, type name + New File/Dir");
            }
        }
        Err(e) => {
            app.entries.clear();
            app.selected = None;
            app.scroll = 0;
            app.status = alloc::format!("Error: {}", e);
        }
    }
    render_explorer_labels(scene, app);
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

fn render_explorer_labels(scene: &mut Scene, app: &ExplorerApp) {
    // Path label
    if let Some(w) = scene.widgets.get_mut(&app.path_widget) {
        w.text = alloc::format!("Path: {}", app.cur_path);
    }
    // List label: visible page with selection marker
    let mut text = String::new();
    if app.entries.is_empty() {
        text = String::from("(empty directory)");
    } else {
        let end = (app.scroll + EXPLORER_PAGE).min(app.entries.len());
        for (i, fi) in app.entries[app.scroll..end].iter().enumerate() {
            let idx = app.scroll + i;
            let mark = if Some(idx) == app.selected { ">" } else { " " };
            let kind = if fi.is_directory { "d" } else { "-" };
            // Truncate long names to fit (~48 chars)
            let mut name = fi.name.clone();
            if name.len() > 40 {
                name.truncate(40);
            }
            text.push_str(&alloc::format!(
                "{} {} {:>6} {}\n",
                mark,
                kind,
                fi.size,
                name
            ));
            let _ = i;
        }
        if app.entries.len() > EXPLORER_PAGE {
            text.push_str(&alloc::format!(
                "-- {}/{} (PgUp/PgDn scroll) --",
                app.scroll + 1,
                app.entries.len()
            ));
        }
    }
    if let Some(w) = scene.widgets.get_mut(&app.list_widget) {
        w.text = text;
    }
    // Status label: status + name buffer + selection preview hint
    let buf = core::str::from_utf8(&app.name_buf[..app.name_len]).unwrap_or("");
    let sel_info = match app.selected.and_then(|i| app.entries.get(i)) {
        Some(fi) => alloc::format!(
            "sel: {} ({})",
            fi.name,
            if fi.is_directory { "dir" } else { "file" }
        ),
        None => String::from("sel: -"),
    };
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        w.text = alloc::format!("{}\nName: {}_ | {}", app.status, buf, sel_info);
    }
}

/// Handle logical button id. Returns true if listing changed.
pub fn explorer_button(scene: &mut Scene, app: &mut ExplorerApp, btn: WidgetId) -> bool {
    if btn == app.btn_up {
        app.cur_path = parent_of(&app.cur_path);
        app.selected = None;
        app.scroll = 0;
        app.status = String::from("Up");
        refresh_explorer(scene, app);
        true
    } else if btn == app.btn_refresh {
        app.status = crate::shell::gui_fs_status();
        refresh_explorer(scene, app);
        true
    } else if btn == app.btn_open {
        open_selected(scene, app);
        true
    } else if btn == app.btn_new_file {
        create_new(scene, app, false);
        true
    } else if btn == app.btn_new_dir {
        create_new(scene, app, true);
        true
    } else if btn == app.btn_delete {
        delete_selected(scene, app);
        true
    } else {
        false
    }
}

/// Check if widget id belongs to this explorer's buttons.
pub fn explorer_owns_button(app: &ExplorerApp, btn: WidgetId) -> bool {
    btn == app.btn_up
        || btn == app.btn_refresh
        || btn == app.btn_open
        || btn == app.btn_new_file
        || btn == app.btn_new_dir
        || btn == app.btn_delete
}

/// Absolute screen rect of the list widget (for click-to-select).
/// Children are relative to the root panel, which sits below the
/// titlebar, so include titlebar height (matches compositor +
/// button_at traversal).
pub fn explorer_list_rect(scene: &Scene, app: &ExplorerApp) -> Option<Rect> {
    let win = scene.windows.get(&app.window)?;
    let w = scene.widgets.get(&app.list_widget)?;
    let tb = scene.theme.metrics.titlebar_height as i32;
    Some(Rect::new(
        win.bounds.x + w.bounds.x,
        win.bounds.y + tb + w.bounds.y,
        w.bounds.w,
        w.bounds.h,
    ))
}

/// Click in list: select row. Returns true if handled.
pub fn explorer_click_row(scene: &mut Scene, app: &mut ExplorerApp, my: i32) -> bool {
    let rect = match explorer_list_rect(scene, app) {
        Some(r) => r,
        None => return false,
    };
    if my < rect.y {
        return false;
    }
    let row = ((my - rect.y) / 16) as usize;
    let idx = app.scroll + row;
    if idx < app.entries.len() {
        app.selected = Some(idx);
        // Show preview of file size in status
        if let Some(fi) = app.entries.get(idx) {
            if fi.is_directory {
                app.status = alloc::format!("Selected dir '{}'", fi.name);
            } else {
                app.status =
                    alloc::format!("Selected file '{}' ({}B) - Open previews", fi.name, fi.size);
            }
        }
        render_explorer_labels(scene, app);
        if let Some(win) = scene.windows.get(&app.window) {
            scene.mark_dirty(win.bounds);
        }
        true
    } else {
        false
    }
}

/// Open selected entry (dir navigates, file previews first bytes in status).
pub fn open_selected(scene: &mut Scene, app: &mut ExplorerApp) {
    let idx = match app.selected {
        Some(i) => i,
        None => {
            // Nothing selected: if name_buf looks like a path, try Go
            if app.name_len > 0 {
                go_to_buffer(scene, app);
            } else {
                app.status = String::from("Nothing selected");
                render_explorer_labels(scene, app);
            }
            return;
        }
    };
    let fi = match app.entries.get(idx) {
        Some(f) => f.clone(),
        None => return,
    };
    if fi.is_directory {
        app.cur_path = join_path(&app.cur_path, &fi.name);
        app.selected = None;
        app.scroll = 0;
        app.status = String::from("Opened dir");
        refresh_explorer(scene, app);
    } else {
        let full = join_path(&app.cur_path, &fi.name);
        match crate::shell::gui_read_file(&full) {
            Ok(data) => {
                let n = data.len().min(PREVIEW_MAX);
                match core::str::from_utf8(&data[..n]) {
                    Ok(t) => {
                        let mut prev: String = t.chars().take(160).collect();
                        prev = prev.replace('\n', " ");
                        app.status = alloc::format!("{} ({}B): {}", fi.name, data.len(), prev);
                    }
                    Err(_) => {
                        app.status = alloc::format!("{} ({}B binary)", fi.name, data.len());
                    }
                }
                render_explorer_labels(scene, app);
                if let Some(win) = scene.windows.get(&app.window) {
                    scene.mark_dirty(win.bounds);
                }
            }
            Err(e) => {
                app.status = alloc::format!("Read failed: {}", e);
                render_explorer_labels(scene, app);
            }
        }
    }
}

fn go_to_buffer(scene: &mut Scene, app: &mut ExplorerApp) {
    let buf = core::str::from_utf8(&app.name_buf[..app.name_len])
        .unwrap_or("")
        .trim()
        .to_string();
    if buf.is_empty() {
        return;
    }
    // Support "..", "/", absolute or child name
    let candidate = if buf == ".." {
        parent_of(&app.cur_path)
    } else if buf.starts_with('/') {
        buf.clone()
    } else {
        join_path(&app.cur_path, &buf)
    };
    match crate::shell::gui_list_dir(&candidate) {
        Ok(_) => {
            app.cur_path = candidate;
            app.selected = None;
            app.scroll = 0;
            app.name_len = 0;
            app.status = String::from("Go");
            refresh_explorer(scene, app);
        }
        Err(e) => {
            app.status = alloc::format!("Go failed: {}", e);
            render_explorer_labels(scene, app);
        }
    }
}

fn create_new(scene: &mut Scene, app: &mut ExplorerApp, is_dir: bool) {
    let buf = core::str::from_utf8(&app.name_buf[..app.name_len])
        .unwrap_or("")
        .trim()
        .to_string();
    let name = if valid_name(&buf) {
        buf.clone()
    } else if !buf.is_empty() {
        app.status = String::from("Bad name (no / \\, max 48ch, not . or ..)");
        render_explorer_labels(scene, app);
        return;
    } else {
        app.auto_counter += 1;
        if is_dir {
            alloc::format!("dir{}", app.auto_counter)
        } else {
            alloc::format!("file{}.txt", app.auto_counter)
        }
    };
    let full = join_path(&app.cur_path, &name);
    let res = if is_dir {
        crate::shell::gui_create_dir(&full).map(|_| ())
    } else {
        crate::shell::gui_create_file(&full).map(|_| ())
    };
    match res {
        Ok(()) => {
            app.name_len = 0;
            app.status = alloc::format!("Created '{}'", name);
            refresh_explorer(scene, app);
        }
        Err(e) => {
            app.status = alloc::format!("Create failed: {}", e);
            render_explorer_labels(scene, app);
        }
    }
}

fn delete_selected(scene: &mut Scene, app: &mut ExplorerApp) {
    let idx = match app.selected {
        Some(i) => i,
        None => {
            app.status = String::from("Select a file/dir first (click or arrows)");
            render_explorer_labels(scene, app);
            return;
        }
    };
    let name = match app.entries.get(idx) {
        Some(f) => f.name.clone(),
        None => return,
    };
    let full = join_path(&app.cur_path, &name);
    match crate::shell::gui_delete_path(&full) {
        Ok(()) => {
            app.status = alloc::format!("Deleted '{}'", name);
            app.selected = None;
            refresh_explorer(scene, app);
        }
        Err(e) => {
            app.status = alloc::format!("Delete failed: {}", e);
            render_explorer_labels(scene, app);
        }
    }
}

/// Keyboard handling when explorer focused. Returns true if key consumed.
pub fn explorer_key(
    scene: &mut Scene,
    app: &mut ExplorerApp,
    key: crate::drivers::keyboard::Key,
) -> bool {
    use crate::drivers::keyboard::Key;
    match key {
        Key::ArrowUp => {
            if !app.entries.is_empty() {
                let next = match app.selected {
                    Some(0) | None => 0,
                    Some(i) => i - 1,
                };
                app.selected = Some(next);
                if next < app.scroll {
                    app.scroll = next;
                }
                render_explorer_labels(scene, app);
                if let Some(win) = scene.windows.get(&app.window) {
                    scene.mark_dirty(win.bounds);
                }
            }
            true
        }
        Key::ArrowDown => {
            if !app.entries.is_empty() {
                let next = match app.selected {
                    None => 0,
                    Some(i) => (i + 1).min(app.entries.len() - 1),
                };
                app.selected = Some(next);
                if next >= app.scroll + EXPLORER_PAGE {
                    app.scroll = next + 1 - EXPLORER_PAGE;
                }
                render_explorer_labels(scene, app);
                if let Some(win) = scene.windows.get(&app.window) {
                    scene.mark_dirty(win.bounds);
                }
            }
            true
        }
        Key::PageUp => {
            app.scroll = app.scroll.saturating_sub(EXPLORER_PAGE);
            render_explorer_labels(scene, app);
            if let Some(win) = scene.windows.get(&app.window) {
                scene.mark_dirty(win.bounds);
            }
            true
        }
        Key::PageDown => {
            if app.entries.len() > EXPLORER_PAGE {
                app.scroll = (app.scroll + EXPLORER_PAGE).min(app.entries.len() - EXPLORER_PAGE);
                render_explorer_labels(scene, app);
                if let Some(win) = scene.windows.get(&app.window) {
                    scene.mark_dirty(win.bounds);
                }
            }
            true
        }
        Key::Enter => {
            open_selected(scene, app);
            true
        }
        Key::Backspace => {
            if app.name_len > 0 {
                app.name_len -= 1;
                app.name_buf[app.name_len] = 0;
                render_explorer_labels(scene, app);
                if let Some(win) = scene.windows.get(&app.window) {
                    scene.mark_dirty(win.bounds);
                }
            } else {
                // Empty buffer + Backspace = go up (like Dolphin/Nautilus)
                app.cur_path = parent_of(&app.cur_path);
                app.selected = None;
                app.scroll = 0;
                app.status = String::from("Up");
                refresh_explorer(scene, app);
            }
            true
        }
        Key::Char(c)
            if c.is_ascii() && !c.is_control() && app.name_len < app.name_buf.len() - 1 =>
        {
            app.name_buf[app.name_len] = c as u8;
            app.name_len += 1;
            render_explorer_labels(scene, app);
            if let Some(win) = scene.windows.get(&app.window) {
                scene.mark_dirty(win.bounds);
            }
            true
        }
        _ => false,
    }
}

/// Adjust child widths after window resize.
pub fn resize_explorer_content(scene: &mut Scene, app: &ExplorerApp) {
    let win_w = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.w as i32,
        None => return,
    };
    let inner = (win_w - 20).max(100) as u32;
    // Reposition buttons row (wrap-safe: keep single row, shrink)
    let btn_w = ((inner.saturating_sub(5 * 6)) / 6).max(60);
    for (i, id) in [
        app.btn_up,
        app.btn_refresh,
        app.btn_open,
        app.btn_new_file,
        app.btn_new_dir,
        app.btn_delete,
    ]
    .iter()
    .enumerate()
    {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = 10 + i as i32 * (btn_w as i32 + 6);
            b.bounds.w = btn_w;
        }
    }
    for id in [app.path_widget, app.list_widget, app.status_widget] {
        if let Some(w) = scene.widgets.get_mut(&id) {
            w.bounds.w = inner;
        }
    }
    // Stretch list height to fill
    if let (Some(win), Some(list), Some(_status)) = (
        scene.windows.get(&app.window),
        scene.widgets.get(&app.list_widget).map(|w| w.bounds),
        scene.widgets.get(&app.status_widget),
    ) {
        let list_y = 62;
        let list_h = (win.bounds.h as i32 - list_y - 70).max(120);
        if let Some(lw) = scene.widgets.get_mut(&app.list_widget) {
            lw.bounds.h = list_h as u32;
        }
        if let Some(s2) = scene.widgets.get_mut(&app.status_widget) {
            s2.bounds.y = list_y + list_h + 4;
        }
        let _ = list;
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}
