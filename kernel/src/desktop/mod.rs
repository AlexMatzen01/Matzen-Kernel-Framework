//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Modern pixel desktop with retained-mode scene graph and double-buffered compositor.
//!
//! Features:
//! - Retained-mode scene graph with dirty-rect tracking
//! - Double-buffered compositor for tear-free rendering
//! - Widget toolkit (Panel, Button, Label, TextInput)
//! - Theme system with colors, fonts, metrics
//! - Window management (drag, focus, close, z-order)

mod anim;
mod compositor;
pub(crate) mod cursor;
mod cursor_data;
pub(crate) mod doom;
pub(crate) mod drives;
pub(crate) mod files;
mod scene;
pub(crate) mod settings;
mod theme;
pub(crate) mod viewer;
pub(crate) mod wallpaper;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::drivers::keyboard::Key;
use crate::drivers::{keyboard, mouse, vga};
use crate::print;
use crate::println;
use spin::Mutex;

const SHELL_INPUT_MAX: usize = 256;
const SHELL_OUTPUT_MAX: usize = 4096;

struct ShellApp {
    window: crate::desktop::scene::WindowId,
    output_widget: crate::desktop::scene::WidgetId,
    output: String,
    input: [u8; SHELL_INPUT_MAX],
    input_len: usize,
}

static EDITOR_MIRROR_RECT: Mutex<Option<(usize, usize, usize, usize)>> = Mutex::new(None);

fn shell_window_bounds(sw: usize, sh: usize) -> crate::desktop::scene::Rect {
    let width = (sw.saturating_sub(80)).min(800).max(520);
    let max_height = sh.saturating_sub(76);
    let height = max_height.min(500).max(360);
    crate::desktop::scene::Rect::new(
        sw.saturating_sub(width) as i32 / 2,
        sh.saturating_sub(height + 36) as i32 / 2,
        width as u32,
        height as u32,
    )
}

fn create_shell_app(scene: &mut crate::desktop::scene::Scene, sw: usize, sh: usize) -> ShellApp {
    let bounds = shell_window_bounds(sw, sh);
    let window = scene.create_window(String::from("MFK Shell"), bounds);
    let root_id = scene.windows.get(&window).unwrap().root_widget;
    let output_widget = crate::desktop::scene::WidgetId::new();
    let initial = alloc::format!(
        "MFK interactive shell\nType 'help' for commands.\n\n{}|",
        crate::shell::desktop_prompt()
    );
    let mut label = crate::desktop::scene::Widget::label(
        crate::desktop::scene::Rect::new(
            12,
            8,
            bounds.w.saturating_sub(24),
            bounds.h.saturating_sub(48),
        ),
        initial,
        &scene.theme,
    );
    label.id = output_widget;
    if let Some(root) = scene.widgets.get_mut(&root_id) {
        root.children.push(output_widget);
    }
    scene.widgets.insert(output_widget, label);
    ShellApp {
        window,
        output_widget,
        output: String::from("MFK interactive shell\nType 'help' for commands.\n\n"),
        input: [0; SHELL_INPUT_MAX],
        input_len: 0,
    }
}

fn terminal_display_text(app: &ShellApp) -> String {
    let prompt = crate::shell::desktop_prompt();
    let current_line = core::str::from_utf8(&app.input[..app.input_len]).unwrap_or("");
    let mut text = alloc::format!(
        "{}{}{}|",
        tail_terminal_output(&app.output),
        prompt,
        current_line
    );
    if text.len() > SHELL_OUTPUT_MAX {
        let mut start = text.len() - SHELL_OUTPUT_MAX;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text = text[start..].into();
    }
    text
}

fn tail_terminal_output(output: &str) -> &str {
    const TAIL_BYTES: usize = 3000;
    if output.len() <= TAIL_BYTES {
        return output;
    }
    let mut start = output.len() - TAIL_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let next_line = output[start..]
        .find('\n')
        .map(|n| start + n + 1)
        .unwrap_or(start);
    &output[next_line..]
}

fn refresh_shell_app(scene: &mut crate::desktop::scene::Scene, app: &ShellApp) {
    if let Some(label) = scene.widgets.get_mut(&app.output_widget) {
        label.text = terminal_display_text(app);
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

fn resize_shell_app_content(scene: &mut crate::desktop::scene::Scene, app: &ShellApp) {
    let (Some(win), Some(label)) = (
        scene.windows.get(&app.window),
        scene.widgets.get_mut(&app.output_widget),
    ) else {
        return;
    };
    label.bounds.w = win.bounds.w.saturating_sub(24);
    label.bounds.h = win.bounds.h.saturating_sub(48);
}

fn run_shell_command(scene: &mut crate::desktop::scene::Scene, app: &mut ShellApp) {
    let command = core::str::from_utf8(&app.input[..app.input_len])
        .unwrap_or("")
        .trim()
        .to_string();
    app.output.push_str(&crate::shell::desktop_prompt());
    app.output.push_str(&command);
    app.output.push('\n');
    app.input = [0; SHELL_INPUT_MAX];
    app.input_len = 0;

    if command == "clear" || command == "cls" {
        app.output.clear();
    } else if !command.is_empty() {
        let is_editor = matches!(
            command.split_whitespace().next(),
            Some("nano" | "edit" | "mfkedit" | "install")
        );
        if is_editor {
            if let Some(win) = scene.windows.get(&app.window) {
                let border = scene.theme.metrics.window_border as usize;
                let title = scene.theme.metrics.titlebar_height as usize;
                *EDITOR_MIRROR_RECT.lock() = Some((
                    win.bounds.x.max(0) as usize + border + 6,
                    win.bounds.y.max(0) as usize + title + 4,
                    (win.bounds.w as usize).saturating_sub(border * 2 + 12),
                    (win.bounds.h as usize).saturating_sub(title + border + 8),
                ));
            }
        }
        vga::begin_output_capture();
        crate::shell::execute_command(&command);
        let response = vga::end_output_capture();
        *EDITOR_MIRROR_RECT.lock() = None;
        if !response.is_empty() {
            app.output.push_str(&response);
            if !response.ends_with('\n') {
                app.output.push('\n');
            }
        }
    }
    if app.output.len() > SHELL_OUTPUT_MAX {
        let mut excess = app.output.len() - SHELL_OUTPUT_MAX;
        while !app.output.is_char_boundary(excess) {
            excess += 1;
        }
        let trim_at = app.output[excess..]
            .find('\n')
            .map(|n| excess + n + 1)
            .unwrap_or(excess);
        app.output.drain(..trim_at);
    }
    refresh_shell_app(scene, app);
}

/// Runs a fullscreen text tool (installer/editor) with its VGA output
/// mirrored into `rect_px` (usually the calling window's interior) so it
/// stays visible on the pixel framebuffer. Restores normal rendering after.
pub(crate) fn run_fullscreen_in(rect_px: (usize, usize, usize, usize), f: impl FnOnce()) {
    *EDITOR_MIRROR_RECT.lock() = Some(rect_px);
    f();
    *EDITOR_MIRROR_RECT.lock() = None;
}

/// Repaints the legacy VGA text grid inside the active desktop terminal
/// while a full-screen tool uses it as its screen backend.
pub(crate) fn refresh_terminal_editor() {
    let Some((x, y, w, h)) = *EDITOR_MIRROR_RECT.lock() else {
        return;
    };
    let (cells, cursor_row, cursor_col, cursor_visible) = vga::text_grid_snapshot();
    if cells.is_empty() {
        return;
    }
    crate::drivers::fb_gfx::set_draw_clip(Some((x, y, w, h)));
    crate::drivers::fb_gfx::fill_rect_px(x, y, w, h, 0, 0, 0);
    const COLS: usize = 80;
    const ROWS: usize = 25;
    for row in 0..ROWS {
        let mut col = 0;
        while col < COLS {
            let bg = color_rgb(cells[row * COLS + col].2);
            let start = col;
            col += 1;
            while col < COLS && color_rgb(cells[row * COLS + col].2) == bg {
                col += 1;
            }
            crate::drivers::fb_gfx::fill_rect_px(
                x + start * 8,
                y + row * 16,
                (col - start) * 8,
                16,
                bg.0,
                bg.1,
                bg.2,
            );
        }

        let mut col = 0;
        while col < COLS {
            while col < COLS && cells[row * COLS + col].0 == b' ' {
                col += 1;
            }
            if col >= COLS {
                break;
            }
            let start = col;
            let fg = color_rgb(cells[row * COLS + col].1);
            let mut run = [0u8; COLS];
            while col < COLS
                && cells[row * COLS + col].0 != b' '
                && color_rgb(cells[row * COLS + col].1) == fg
            {
                let ch = cells[row * COLS + col].0;
                run[col - start] = if ch.is_ascii_graphic() { ch } else { b' ' };
                col += 1;
            }
            if let Ok(text) = core::str::from_utf8(&run[..col - start]) {
                crate::drivers::fb_gfx::draw_text_bb(
                    x + start * 8,
                    y + row * 16,
                    text,
                    fg,
                    Some((x, y, w, h)),
                );
            }
        }
    }
    if cursor_visible && cursor_row < ROWS && cursor_col < COLS {
        crate::drivers::fb_gfx::fill_rect_px(
            x + cursor_col * 8,
            y + cursor_row * 16,
            2,
            16,
            240,
            240,
            240,
        );
    }
    crate::drivers::fb_gfx::set_draw_clip(None);
}

/// First image under /wallpapers (or /) for the View launcher when no
/// pending path is queued. Returns the full FS path, if any.
fn viewer_browse_first_image() -> Option<String> {
    for dir in [
        crate::desktop::wallpaper::WALLPAPER_DIR,
        "/",
    ] {
        let (_, entries) = crate::shell::gui_list_dir(dir).ok()?;
        for fi in &entries {
            if !fi.is_directory && viewer::is_image_path(&fi.name) {
                if dir == "/" {
                    return Some(alloc::format!("/{}", fi.name));
                } else {
                    return Some(alloc::format!("{}/{}", dir, fi.name));
                }
            }
        }
    }
    None
}

fn color_rgb(color: vga::Color) -> (u8, u8, u8) {
    match color {
        vga::Color::Black => (0, 0, 0),
        vga::Color::Blue => (0, 0, 170),
        vga::Color::Green => (0, 170, 0),
        vga::Color::Cyan => (0, 170, 170),
        vga::Color::Red => (170, 0, 0),
        vga::Color::Magenta => (170, 0, 170),
        vga::Color::Brown => (170, 85, 0),
        vga::Color::LightGray => (170, 170, 170),
        vga::Color::DarkGray => (85, 85, 85),
        vga::Color::LightBlue => (85, 85, 255),
        vga::Color::LightGreen => (85, 255, 85),
        vga::Color::LightCyan => (85, 255, 255),
        vga::Color::LightRed => (255, 85, 85),
        vga::Color::Pink => (255, 85, 255),
        vga::Color::Yellow => (255, 255, 85),
        vga::Color::White => (255, 255, 255),
    }
}

/// Desktop entry point: `desktop` shell command.
pub fn run() {
    let Some((sw, sh)) = crate::drivers::fb_gfx::framebuffer_size() else {
        println!("desktop: no pixel framebuffer found.");
        println!("The graphical desktop needs a bootloader framebuffer;");
        println!("VGA text mode cannot show the mouse cursor.");
        return;
    };

    // Initialize theme
    crate::desktop::theme::init_theme();
    let theme = crate::desktop::theme::current_theme();

    crate::serial_println!("[desktop] entering {}x{} px", sw, sh);
    crate::serial_println!("[desktop] damage-clipped direct framebuffer rendering");
    vga::hide_cursor();
    crate::drivers::mouse::set_bounds(sw.saturating_sub(1), sh.saturating_sub(1));
    crate::drivers::mouse::set_position(sw / 2, sh / 2);
    // Drain stale motion so the cursor does not jump on entry.
    while crate::drivers::mouse::read_event().is_some() {}

    // Initialize scene graph
    let mut scene = crate::desktop::scene::Scene::new(sw as u32, sh as u32);

    // Launch the real shell in a terminal window; its taskbar launcher remains
    // available after the window is closed or minimized. Files + Drives
    // launchers behave the same (lazy single-instance windows).
    let mut shell_app = Some(create_shell_app(&mut scene, sw, sh));
    let mut explorer_app: Option<files::ExplorerApp> = None;
    let mut drive_app: Option<drives::DriveApp> = None;
    let mut settings_app: Option<settings::SettingsApp> = None;
    let mut doom_app: Option<doom::DoomApp> = None;
    let mut viewer_app: Option<viewer::ViewerApp> = None;
    let about_win_id = scene.create_window(
        alloc::string::String::from("About MFK"),
        scene::Rect::new(220, 130, 400, 300),
    );

    // Add widgets to about window
    if let Some(about_win) = scene.windows.get_mut(&about_win_id) {
        let root_id = about_win.root_widget;

        let label_id = crate::desktop::scene::WidgetId::new();
        let label_bounds = scene::Rect::new(20, 40, 360, 200);
        let mut label = crate::desktop::scene::Widget::label(label_bounds,
            alloc::string::String::from("Matzen Kernel Framework v0.1.0\n\nTaskbar: Shell | Files | Drive\n| Settings | Doom | View\nFiles: browse, Enter opens, type name\nfor New File/Dir, click selects.\nDouble-click / Enter on PNG/JPG/BMP/\nGIF/QOI opens the Viewer.\nViewer: +/- zoom, F fit, 0 100%,\narrows drag-pan, N/P next/prev.\nDrives: pick disk (Up/Down, 1-8),\nFormat (2-click) + Mount + Install.\nSettings: wallpaper PNG/JPG/BMP/GIF + Fit/\nFill/Stretch/Center/Tile + colors + cursor.\nDoom: needs WAD disk mounted\n(mount drive, Doom launcher).\n\nDrag windows by titlebar.\nPress Esc to exit."),
            theme);
        label.id = label_id;
        scene.widgets.insert(label_id, label);
        if let Some(root) = scene.widgets.get_mut(&root_id) {
            root.children.push(label_id);
        } else {
            crate::serial_println!("[desktop] warning: about root widget missing");
        }

        about_win.root_widget = root_id;
    }

    // Initialize compositor
    let mut compositor = crate::desktop::compositor::Compositor::new(sw as u32, sh as u32);

    // Get framebuffer info for compositor
    if let Some(fb_info) =
        crate::drivers::fb::pixel_size().and_then(|_| crate::drivers::fb::with_lock(|st| st.info))
    {
        compositor.set_fb_info(fb_info);
    }

    crate::drivers::mouse::set_bounds(sw.saturating_sub(1), sh.saturating_sub(1));
    crate::drivers::mouse::set_position(sw / 2, sh / 2);
    while crate::drivers::mouse::read_event().is_some() {}

    // Boot fade + open animations for the initial windows.
    crate::desktop::anim::boot_begin();
    if let Some(app) = &shell_app {
        scene.focus_window(app.window);
        crate::desktop::anim::start_open(&mut scene, app.window);
    }
    crate::desktop::anim::start_open(&mut scene, about_win_id);

    // Restore persisted personalization (solid fallback when the FS is
    // not mounted or no settings were saved yet).
    settings::load_persisted(&mut scene, sw, sh);

    // `doom run` queues a WAD and enters here: open the game immediately
    // so the shell command works without a mouse click.
    if let Some(wad) = doom::pending_wad() {
        match doom::create_doom_app(&mut scene, sw, sh, &wad) {
            Ok(app) => {
                scene.focus_window(app.window);
                crate::desktop::anim::start_open(&mut scene, app.window);
                crate::serial_println!("[desktop] Doom auto-opened ({})", wad);
                doom_app = Some(app);
            }
            Err(e) => {
                crate::serial_println!("[desktop] Doom: {}", e);
                if let Some(shell) = shell_app.as_mut() {
                    shell.output.push_str("Doom: ");
                    shell.output.push_str(&e);
                    shell.output.push('\n');
                    refresh_shell_app(&mut scene, shell);
                }
            }
        }
    }

    // `view <path>` queues an image and enters here: open the viewer
    // immediately so the shell command works without a mouse click.
    // The pending slot is consumed (take) so a stale path cannot reopen.
    if let Some(path) = viewer::take_pending_image() {
        match viewer::create_viewer_app(&mut scene, sw, sh, &path) {
            Ok(app) => {
                scene.focus_window(app.window);
                crate::desktop::anim::start_open(&mut scene, app.window);
                crate::serial_println!("[desktop] Viewer auto-opened ({})", path);
                viewer_app = Some(app);
            }
            Err(e) => {
                crate::serial_println!("[desktop] Viewer: {}", e);
                if let Some(shell) = shell_app.as_mut() {
                    shell.output.push_str("Viewer: ");
                    shell.output.push_str(&e);
                    shell.output.push('\n');
                    refresh_shell_app(&mut scene, shell);
                }
            }
        }
    }

    // Mark full screen dirty for initial render
    scene.mark_dirty_full();

    crate::serial_println!(
        "[desktop] Scene initialized with {} windows",
        scene.windows.len()
    );

    'outer: loop {
        crate::net::process_packets();
        #[cfg(feature = "usb")]
        crate::drivers::usb::poll();

        // Mouse: position is authoritative (updated in IRQ12)
        while crate::drivers::mouse::read_event().is_some() {}
        let (mx_usize, my_usize) = crate::drivers::mouse::position();
        let mx = mx_usize as i32;
        let my = my_usize as i32;
        let buttons = crate::drivers::mouse::buttons();
        let left = buttons & 0x01 != 0;

        // Handle mouse events
        static mut PREV_LEFT: bool = false;
        static mut DRAG_WINDOW: Option<crate::desktop::scene::WindowId> = None;
        static mut DRAG_OFFSET: (i32, i32) = (0, 0);
        static mut RESIZE_WINDOW: Option<(
            crate::desktop::scene::WindowId,
            i32,
            i32,
            crate::desktop::scene::Rect,
            bool,
            bool,
        )> = None;
        static mut LAST_CURSOR: (i32, i32, u8) = (0, 0, 0);

        // Cursor motion repaints only the old + new cursor bitmap bounds.
        // Button changes go full: hover states across
        // windows may flip. Motion alone used to repaint the whole
        // screen, which cost seconds per frame unbuffered.
        // Cursor cells always repaint on motion; button press/release,
        // hover flips, focus and drag mark their own rects below, so a
        // button change never needs a full repaint anymore.
        let last_cursor = unsafe { LAST_CURSOR };
        if (mx, my, buttons) != last_cursor {
            unsafe {
                LAST_CURSOR = (mx, my, buttons);
            }
            scene.mark_dirty(cursor_cell(last_cursor.0, last_cursor.1));
            scene.mark_dirty(cursor_cell(mx, my));
        }

        // Button hover follows the cursor.
        update_button_hover(&mut scene, mx, my);

        let prev_left = unsafe { PREV_LEFT };
        // Double-click tracking for explorer list (open on 2nd click).
        static mut LAST_CLICK_TICK: u64 = 0;
        static mut LAST_CLICK_WIN: Option<crate::desktop::scene::WindowId> = None;
        static mut LAST_CLICK_IDX: Option<usize> = None;
        if left && !prev_left {
            // Mouse down
            if scene.shell_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &shell_app {
                    let lr = scene.shell_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else {
                    let app = create_shell_app(&mut scene, sw, sh);
                    scene.focus_window(app.window);
                    crate::desktop::anim::start_open(&mut scene, app.window);
                    shell_app = Some(app);
                }
            } else if scene.files_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &explorer_app {
                    let lr = scene.files_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else {
                    let app = files::create_explorer_app(&mut scene, sw, sh);
                    scene.focus_window(app.window);
                    crate::desktop::anim::start_open(&mut scene, app.window);
                    crate::serial_println!("[desktop] Files opened");
                    explorer_app = Some(app);
                }
            } else if scene.drive_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &drive_app {
                    let lr = scene.drive_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else {
                    let app = drives::create_drive_app(&mut scene, sw, sh);
                    scene.focus_window(app.window);
                    crate::desktop::anim::start_open(&mut scene, app.window);
                    crate::serial_println!("[desktop] Drives opened");
                    drive_app = Some(app);
                }
            } else if scene.settings_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &settings_app {
                    let lr = scene.settings_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else {
                    let app = settings::create_settings_app(&mut scene, sw, sh);
                    scene.focus_window(app.window);
                    crate::desktop::anim::start_open(&mut scene, app.window);
                    crate::serial_println!("[desktop] Settings opened");
                    settings_app = Some(app);
                }
            } else if scene.doom_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &doom_app {
                    let lr = scene.doom_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else {
                    let wad = doom::take_launch_wad();
                    match doom::create_doom_app(&mut scene, sw, sh, &wad) {
                        Ok(app) => {
                            scene.focus_window(app.window);
                            crate::desktop::anim::start_open(&mut scene, app.window);
                            crate::serial_println!("[desktop] Doom opened");
                            doom_app = Some(app);
                        }
                        Err(e) => {
                            crate::serial_println!("[desktop] Doom: {}", e);
                            if let Some(shell) = shell_app.as_mut() {
                                shell.output.push_str("Doom: ");
                                shell.output.push_str(&e);
                                shell.output.push_str("\n(mount the WAD disk, or run 'doom <path>')\n");
                                refresh_shell_app(&mut scene, shell);
                            }
                        }
                    }
                }
            } else if scene.viewer_launcher_rect().contains_point(mx, my) {
                if let Some(app) = &viewer_app {
                    let lr = scene.viewer_launcher_rect();
                    let wid = app.window;
                    crate::desktop::anim::start_restore(&mut scene, wid, lr);
                } else if let Some(path) = viewer::take_pending_image() {
                    match viewer::create_viewer_app(&mut scene, sw, sh, &path) {
                        Ok(app) => {
                            scene.focus_window(app.window);
                            crate::desktop::anim::start_open(&mut scene, app.window);
                            crate::serial_println!("[desktop] Viewer opened ({})", path);
                            viewer_app = Some(app);
                        }
                        Err(e) => {
                            crate::serial_println!("[desktop] Viewer: {}", e);
                            if let Some(shell) = shell_app.as_mut() {
                                shell.output.push_str("Viewer: ");
                                shell.output.push_str(&e);
                                shell.output.push('\n');
                                refresh_shell_app(&mut scene, shell);
                            }
                        }
                    }
                } else {
                    // No pending image: browse /wallpapers for the first
                    // image so the launcher always does something useful.
                    let pick = viewer_browse_first_image();
                    match pick {
                        Some(path) => match viewer::create_viewer_app(&mut scene, sw, sh, &path) {
                            Ok(app) => {
                                scene.focus_window(app.window);
                                crate::desktop::anim::start_open(&mut scene, app.window);
                                crate::serial_println!("[desktop] Viewer opened ({})", path);
                                viewer_app = Some(app);
                            }
                            Err(e) => {
                                crate::serial_println!("[desktop] Viewer: {}", e);
                                if let Some(shell) = shell_app.as_mut() {
                                    shell.output.push_str("Viewer: ");
                                    shell.output.push_str(&e);
                                    shell.output.push_str("\n(copy a PNG/JPG/BMP/GIF/QOI to /wallpapers, or run 'view <path>')\n");
                                    refresh_shell_app(&mut scene, shell);
                                }
                            }
                        },
                        None => {
                            if let Some(shell) = shell_app.as_mut() {
                                shell.output.push_str("Viewer: no image found (copy one to /wallpapers, or run 'view <path>')\n");
                                refresh_shell_app(&mut scene, shell);
                            }
                        }
                    }
                }
            } else if let Some((win_id, btn_id)) = button_at(&scene, mx, my) {
                // Clicked a button: focus, press, then dispatch app actions.
                // Explorer/Drive buttons are dispatched here (their
                // on_click is None; they need app state from run()).
                scene.focus_window(win_id);
                press_button(&mut scene, btn_id);
                let mut handled = false;
                if let Some(app) = explorer_app.as_mut() {
                    if app.window == win_id && files::explorer_owns_button(app, btn_id) {
                        files::explorer_button(&mut scene, app, btn_id);
                        // Mount state may have changed elsewhere; drive view
                        // refreshes lazily on next open/refresh.
                        handled = true;
                    }
                }
                if !handled {
                    if let Some(app) = drive_app.as_mut() {
                        if app.window == win_id && drives::drive_owns_button(app, btn_id) {
                            let tick = crate::shell::get_tick_count();
                            drives::drive_button(&mut scene, app, btn_id, tick);
                            // After mount/format, refresh explorer listing too.
                            if let Some(exp) = explorer_app.as_mut() {
                                files::refresh_explorer(&mut scene, exp);
                            }
                            handled = true;
                        }
                    }
                }
                if !handled {
                    if let Some(app) = settings_app.as_mut() {
                        if app.window == win_id && settings::settings_owns_button(app, btn_id) {
                            settings::settings_button(&mut scene, app, btn_id, sw, sh);
                            handled = true;
                        }
                    }
                }
                if !handled {
                    if let Some(app) = viewer_app.as_mut() {
                        if app.window == win_id && viewer::viewer_owns_button(app, btn_id) {
                            viewer::viewer_button(&mut scene, app, btn_id);
                            handled = true;
                        }
                    }
                }
                if let Some(win) = scene.windows.get(&win_id) {
                    let bounds = win.bounds;
                    scene.mark_dirty(bounds);
                }
                let _ = handled;
            } else if let Some(win_id) = scene.window_at(mx, my) {
                scene.focus_window(win_id);

                // Check if clicking titlebar
                if let Some(win) = scene.windows.get(&win_id) {
                    let tb = win.titlebar_rect(&scene.theme);
                    if tb.contains_point(mx, my) {
                        let close_btn = win.close_button_rect(&scene.theme);
                        let maximize_btn = win.maximize_button_rect(&scene.theme);
                        let minimize_btn = win.minimize_button_rect(&scene.theme);
                        if minimize_btn.contains_point(mx, my) {
                            // Slide toward this window's taskbar launcher.
                            let launcher =
                                if shell_app.as_ref().map(|a| a.window) == Some(win_id) {
                                    scene.shell_launcher_rect()
                                } else if explorer_app.as_ref().map(|a| a.window) == Some(win_id) {
                                    scene.files_launcher_rect()
                                } else if drive_app.as_ref().map(|a| a.window) == Some(win_id) {
                                    scene.drive_launcher_rect()
                                } else if settings_app.as_ref().map(|a| a.window) == Some(win_id) {
                                    scene.settings_launcher_rect()
                                } else if viewer_app.as_ref().map(|a| a.window) == Some(win_id) {
                                    scene.viewer_launcher_rect()
                                } else {
                                    scene.doom_launcher_rect()
                                };
                            crate::desktop::anim::start_minimize(&mut scene, win_id, launcher);
                        } else if maximize_btn.contains_point(mx, my) {
                            scene.toggle_maximize(win_id);
                            if let Some(app) = &shell_app {
                                if app.window == win_id {
                                    resize_shell_app_content(&mut scene, app);
                                }
                            }
                            if let Some(app) = &explorer_app {
                                if app.window == win_id {
                                    files::resize_explorer_content(&mut scene, app);
                                }
                            }
                            if let Some(app) = &drive_app {
                                if app.window == win_id {
                                    drives::resize_drive_content(&mut scene, app);
                                }
                            }
                            if let Some(app) = &settings_app {
                                if app.window == win_id {
                                    settings::resize_settings_content(&mut scene, app);
                                }
                            }
                            if let Some(app) = viewer_app.as_mut() {
                                if app.window == win_id {
                                    viewer::resize_viewer_rebuild(&mut scene, app);
                                }
                            }
                            if let Some(app) = &doom_app {
                                if app.window == win_id {
                                    doom::resize_doom_content(&mut scene, app);
                                }
                            }
                        } else if close_btn.contains_point(mx, my) {
                            if doom_app.as_ref().map(|app| app.window) == Some(win_id) {
                                if let Some(app) = doom_app.as_mut() {
                                    doom::doom_release_all(app);
                                    doom::close_doom(app);
                                }
                            }
                            // Animated close keeps a shrinking ghost; the
                            // tick() destroys it and app slots are reaped below.
                            if !crate::desktop::anim::start_close(&mut scene, win_id) {
                                crate::desktop::anim::cancel(win_id);
                                scene.destroy_window(win_id);
                                if shell_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    shell_app = None;
                                }
                                if explorer_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    explorer_app = None;
                                }
                                if drive_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    drive_app = None;
                                }
                                if doom_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    doom_app = None;
                                }
                                if settings_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    settings_app = None;
                                }
                                if viewer_app.as_ref().map(|app| app.window) == Some(win_id) {
                                    viewer_app = None;
                                }
                            }
                        } else {
                            // Start drag
                            unsafe {
                                DRAG_WINDOW = Some(win_id);
                            }
                            unsafe {
                                DRAG_OFFSET = (mx - win.bounds.x, my - win.bounds.y);
                            }
                        }
                    } else if let Some((resize_x, resize_y)) = resize_hit(win.bounds, mx, my) {
                        unsafe {
                            RESIZE_WINDOW = Some((win_id, mx, my, win.bounds, resize_x, resize_y));
                        }
                    } else {
                        // Body click: explorer list selection (+double-click open).
                        if let Some(app) = explorer_app.as_mut() {
                            if app.window == win_id {
                                if let Some(rect) = files::explorer_list_rect(&scene, app) {
                                    if rect.contains_point(mx, my) {
                                        let row = ((my - rect.y) / 16) as usize;
                                        let idx = app.scroll + row;
                                        let now = crate::shell::get_tick_count();
                                        let last_tick = unsafe { LAST_CLICK_TICK };
                                        let last_win = unsafe { LAST_CLICK_WIN };
                                        let last_idx = unsafe { LAST_CLICK_IDX };
                                        let is_double = last_win == Some(win_id)
                                            && last_idx == Some(idx)
                                            && now.wrapping_sub(last_tick) < 500;
                                        files::explorer_click_row(&mut scene, app, my);
                                        unsafe {
                                            LAST_CLICK_TICK = now;
                                            LAST_CLICK_WIN = Some(win_id);
                                            LAST_CLICK_IDX = Some(idx);
                                        }
                                        if is_double {
                                            files::open_selected(&mut scene, app);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if !left && prev_left {
            // Mouse up
            unsafe {
                DRAG_WINDOW = None;
            }
            unsafe {
                RESIZE_WINDOW = None;
            }
            release_buttons(&mut scene);
        }

        if left {
            if let Some((win_id, start_x, start_y, original, resize_x, resize_y)) =
                unsafe { RESIZE_WINDOW }
            {
                let old = scene.windows.get(&win_id).map(|win| win.bounds);
                if let Some(old) = old {
                    let min_w = 420u32;
                    let min_h = 320u32;
                    let max_w = sw.saturating_sub(original.x.max(0) as usize) as u32;
                    let max_h = sh.saturating_sub(original.y.max(0) as usize) as u32;
                    let w = if resize_x {
                        (original.w as i32 + mx - start_x)
                            .clamp(min_w as i32, max_w.max(min_w) as i32)
                            as u32
                    } else {
                        original.w
                    };
                    let h = if resize_y {
                        (original.h as i32 + my - start_y)
                            .clamp(min_h as i32, max_h.max(min_h) as i32)
                            as u32
                    } else {
                        original.h
                    };
                    let new = crate::desktop::scene::Rect::new(original.x, original.y, w, h);
                    if new != old {
                        scene.set_window_bounds(win_id, new);
                        scene.mark_dirty(old);
                        scene.mark_dirty(new);
                        if let Some(app) = &shell_app {
                            if app.window == win_id {
                                resize_shell_app_content(&mut scene, app);
                            }
                        }
                        if let Some(app) = &explorer_app {
                            if app.window == win_id {
                                files::resize_explorer_content(&mut scene, app);
                            }
                        }
                        if let Some(app) = &drive_app {
                            if app.window == win_id {
                                drives::resize_drive_content(&mut scene, app);
                            }
                        }
                        if let Some(app) = &settings_app {
                            if app.window == win_id {
                                settings::resize_settings_content(&mut scene, app);
                            }
                        }
                        if let Some(app) = viewer_app.as_mut() {
                            if app.window == win_id {
                                viewer::resize_viewer_rebuild(&mut scene, app);
                            }
                        }
                        if let Some(app) = &doom_app {
                            if app.window == win_id {
                                doom::resize_doom_content(&mut scene, app);
                            }
                        }
                    }
                }
            }
        }

        if left && unsafe { DRAG_WINDOW }.is_some() {
            // Dragging: repaint only the vacated rect plus the new one.
            // The compositor repaints every window intersecting those
            // rects, so revealed areas are restored correctly.
            if let Some(win_id) = unsafe { DRAG_WINDOW } {
                let rects = if let Some(win) = scene.windows.get_mut(&win_id) {
                    let old = win.bounds;
                    let offset = unsafe { DRAG_OFFSET };
                    let new_x = mx - offset.0;
                    let new_y = my - offset.1;
                    win.bounds.x = new_x.clamp(0, sw as i32 - win.bounds.w as i32);
                    win.bounds.y = new_y.clamp(0, sh as i32 - win.bounds.h as i32);
                    win.dirty = true;
                    Some((old, win.bounds))
                } else {
                    // Window closed mid-drag: stop tracking it.
                    unsafe {
                        DRAG_WINDOW = None;
                    }
                    None
                };
                if let Some((old, new)) = rects {
                    scene.set_window_bounds(win_id, new);
                    scene.mark_dirty(old);
                    scene.mark_dirty(new);
                    if let Some(app) = &shell_app {
                        if app.window == win_id {
                            resize_shell_app_content(&mut scene, app);
                        }
                    }
                    if let Some(app) = &explorer_app {
                        if app.window == win_id {
                            files::resize_explorer_content(&mut scene, app);
                        }
                    }
                    if let Some(app) = &drive_app {
                        if app.window == win_id {
                            drives::resize_drive_content(&mut scene, app);
                        }
                    }
                    if let Some(app) = &settings_app {
                        if app.window == win_id {
                            settings::resize_settings_content(&mut scene, app);
                        }
                    }
                    if let Some(app) = &doom_app {
                        if app.window == win_id {
                            doom::resize_doom_content(&mut scene, app);
                        }
                    }
                }
            }
        }
        unsafe {
            PREV_LEFT = left;
        }

        // Keyboard: route by focused window (Shell | Files | Drive | Doom).
        // While Doom is focused, every key goes to the game (Esc opens
        // the in-game menu); desktop exit is via the taskbar/close box.
        for _ in 0..8 {
            let Some(ev) = crate::drivers::keyboard::read_key() else {
                break;
            };
            let focused = scene.focused_window;
            let is_shell = shell_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            let is_explorer = explorer_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            let is_drive = drive_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            let is_settings = settings_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            let is_viewer = viewer_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            let is_doom = doom_app
                .as_ref()
                .map(|a| Some(a.window) == focused)
                .unwrap_or(false);
            // Doom consumes everything while focused (including Esc).
            if is_doom {
                if let Some(app) = doom_app.as_mut() {
                    // Ctrl+C in Doom: release stuck keys instead of firing.
                    if ev.key == crate::drivers::keyboard::Key::Ctrl('C') {
                        doom::doom_release_all(app);
                        crate::shell::clear_interrupt();
                    } else {
                        doom::doom_key(app, ev.key);
                    }
                }
                continue;
            }
            match ev.key {
                crate::drivers::keyboard::Key::Esc => {
                    break 'outer; // Exit desktop loop
                }
                crate::drivers::keyboard::Key::Char(c) if c.is_ascii() && !c.is_control() => {
                    if is_shell {
                        if let Some(app) = shell_app.as_mut() {
                            if app.input_len < SHELL_INPUT_MAX - 1 {
                                app.input[app.input_len] = c as u8;
                                app.input_len += 1;
                                refresh_shell_app(&mut scene, app);
                            }
                        }
                    } else if is_explorer {
                        if let Some(app) = explorer_app.as_mut() {
                            files::explorer_key(&mut scene, app, ev.key);
                        }
                    } else if is_drive {
                        if let Some(app) = drive_app.as_mut() {
                            let tick = crate::shell::get_tick_count();
                            drives::drive_key(&mut scene, app, ev.key, tick);
                        }
                    } else if is_settings {
                        if let Some(app) = settings_app.as_mut() {
                            settings::settings_key(&mut scene, app, ev.key, sw, sh);
                        }
                    } else if is_viewer {
                        if let Some(app) = viewer_app.as_mut() {
                            viewer::viewer_key(&mut scene, app, ev.key);
                        }
                    }
                }
                crate::drivers::keyboard::Key::Backspace => {
                    if is_shell {
                        if let Some(app) = shell_app.as_mut() {
                            if app.input_len > 0 {
                                app.input_len -= 1;
                                app.input[app.input_len] = 0;
                                refresh_shell_app(&mut scene, app);
                            }
                        }
                    } else if is_explorer {
                        if let Some(app) = explorer_app.as_mut() {
                            files::explorer_key(&mut scene, app, ev.key);
                        }
                    } else if is_settings {
                        if let Some(app) = settings_app.as_mut() {
                            settings::settings_key(&mut scene, app, ev.key, sw, sh);
                        }
                    } else if is_viewer {
                        if let Some(app) = viewer_app.as_mut() {
                            viewer::viewer_key(&mut scene, app, ev.key);
                        }
                    }
                }
                crate::drivers::keyboard::Key::Enter => {
                    if is_shell {
                        if let Some(app) = shell_app.as_mut() {
                            run_shell_command(&mut scene, app);
                            crate::shell::clear_interrupt();
                        }
                    } else if is_explorer {
                        if let Some(app) = explorer_app.as_mut() {
                            files::explorer_key(&mut scene, app, ev.key);
                        }
                    } else if is_drive {
                        if let Some(app) = drive_app.as_mut() {
                            let tick = crate::shell::get_tick_count();
                            drives::drive_key(&mut scene, app, ev.key, tick);
                        }
                    } else if is_settings {
                        if let Some(app) = settings_app.as_mut() {
                            settings::settings_key(&mut scene, app, ev.key, sw, sh);
                        }
                    } else if is_viewer {
                        if let Some(app) = viewer_app.as_mut() {
                            viewer::viewer_key(&mut scene, app, ev.key);
                        }
                    }
                }
                crate::drivers::keyboard::Key::Tab => {
                    if is_shell {
                        if let Some(app) = shell_app.as_mut() {
                            vga::begin_output_capture();
                            crate::shell::complete_desktop_input(
                                &mut app.input,
                                &mut app.input_len,
                            );
                            let completion = vga::end_output_capture();
                            if !completion.is_empty() {
                                app.output.push('\n');
                                app.output.push_str(&completion);
                                if !completion.ends_with('\n') {
                                    app.output.push('\n');
                                }
                            }
                            refresh_shell_app(&mut scene, app);
                        }
                    }
                }
                crate::drivers::keyboard::Key::Ctrl('C') => {
                    if is_shell {
                        if let Some(app) = shell_app.as_mut() {
                            app.input = [0; SHELL_INPUT_MAX];
                            app.input_len = 0;
                            crate::shell::clear_interrupt();
                            refresh_shell_app(&mut scene, app);
                        }
                    }
                }
                // Arrows: Files owns PgUp/PgDn; Up/Down also drive disk selection.
                crate::drivers::keyboard::Key::ArrowUp
                | crate::drivers::keyboard::Key::ArrowDown
                | crate::drivers::keyboard::Key::PageUp
                | crate::drivers::keyboard::Key::PageDown => {
                    if is_explorer {
                        if let Some(app) = explorer_app.as_mut() {
                            files::explorer_key(&mut scene, app, ev.key);
                        }
                    } else if is_drive {
                        if let Some(app) = drive_app.as_mut() {
                            let tick = crate::shell::get_tick_count();
                            drives::drive_key(&mut scene, app, ev.key, tick);
                        }
                    } else if is_viewer {
                        if let Some(app) = viewer_app.as_mut() {
                            viewer::viewer_key(&mut scene, app, ev.key);
                        }
                    }
                }
                _ => {}
            }
        }

        // Files double-click / Open on an image queues a viewer path:
        // open (or focus) the Viewer here, outside the button borrow.
        if let Some(path) = viewer::take_pending_image() {
            if let Some(app) = &viewer_app {
                let wid = app.window;
                scene.focus_window(wid);
                // Reuse the open window: load the new image into it.
                let lr = scene.viewer_launcher_rect();
                if let Some(app) = viewer_app.as_mut() {
                    if viewer::open_image(&mut scene, app, &path).is_ok() {
                        crate::desktop::anim::start_restore(&mut scene, wid, lr);
                    }
                }
            } else {
                match viewer::create_viewer_app(&mut scene, sw, sh, &path) {
                    Ok(app) => {
                        scene.focus_window(app.window);
                        crate::desktop::anim::start_open(&mut scene, app.window);
                        crate::serial_println!("[desktop] Viewer opened ({})", path);
                        viewer_app = Some(app);
                    }
                    Err(e) => {
                        crate::serial_println!("[desktop] Viewer: {}", e);
                        if let Some(shell) = shell_app.as_mut() {
                            shell.output.push_str("Viewer: ");
                            shell.output.push_str(&e);
                            shell.output.push('\n');
                            refresh_shell_app(&mut scene, shell);
                        }
                    }
                }
            }
        }

        // Drag-pan inside the viewer image: left-held motion pans the
        // zoomed image (titlebar drags and resizes take precedence).
        {
            static mut LAST_PAN: (i32, i32, bool) = (0, 0, false);
            let dragging_chrome = unsafe { DRAG_WINDOW.is_some() }
                || unsafe {
                    RESIZE_WINDOW
                        .map(|_| true)
                        .unwrap_or(false)
                };
            if left && !dragging_chrome {
                if let Some(app) = viewer_app.as_mut() {
                    let inside = viewer::viewer_image_rect(&scene, app)
                        .map(|r| r.contains_point(mx, my))
                        .unwrap_or(false);
                    let (lx, ly, was) = unsafe { LAST_PAN };
                    if inside {
                        if was {
                            let dx = mx - lx;
                            let dy = my - ly;
                            if dx != 0 || dy != 0 {
                                viewer::viewer_drag_pan(&mut scene, app, dx, dy);
                            }
                        }
                        unsafe {
                            LAST_PAN = (mx, my, true);
                        }
                    } else {
                        unsafe {
                            LAST_PAN = (mx, my, false);
                        }
                    }
                }
            } else {
                unsafe {
                    LAST_PAN = (mx, my, false);
                }
            }
        }

        // Pump the Doom engine (35Hz paced inside; skips minimized).
        if doom_app.is_some() {
            let visible = doom_app
                .as_ref()
                .and_then(|app| scene.windows.get(&app.window))
                .map(|win| win.visible && !win.minimized)
                .unwrap_or(false);
            if visible {
                if let Some(app) = doom_app.as_mut() {
                    match doom::pump_doom(&mut scene, app) {
                        doom::DoomPump::Quit => {
                            let win = app.window;
                            doom::close_doom(app);
                            scene.destroy_window(win);
                            doom_app = None;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Advance open/close/minimize tweens (marks old+new dirty).
        // Reap app slots whose ghost windows just finished closing.
        let _anim_active = crate::desktop::anim::tick(&mut scene);
        if let Some(app) = &shell_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                shell_app = None;
            }
        }
        if let Some(app) = &explorer_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                explorer_app = None;
            }
        }
        if let Some(app) = &drive_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                drive_app = None;
            }
        }
        if let Some(app) = &settings_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                settings_app = None;
            }
        }
        if let Some(app) = &doom_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                doom_app = None;
            }
        }
        if let Some(app) = &viewer_app {
            if scene.windows.get(&app.window).is_none() {
                crate::desktop::anim::cancel(app.window);
                viewer_app = None;
            }
        }

        // Render frame
        compositor.render(&mut scene, mx, my, true);

        // Boot wipe-up reveal (cheap stepped fill + single present).
        let boot_a = crate::desktop::anim::boot_overlay_alpha();
        if boot_a > 0 {
            let h = (sh as u32 * boot_a as u32 / 255) as usize;
            if h > 0 {
                crate::drivers::fb_gfx::fill_rect_px(0, 0, sw, h, 0, 0, 0);
                crate::drivers::fb_gfx::present(&[(0, 0, sw, h)]);
            }
        }

        // Small delay to prevent 100% CPU (~60 FPS)
        crate::drivers::pit::sleep_ms(16);
    }
    'outer: {}

    // Cleanup
    crate::drivers::vga::clear_screen();
    crate::drivers::vga::set_cursor_pos(crate::drivers::vga::VGA_HEIGHT - 1, 0);
    crate::drivers::vga::show_cursor();
    crate::serial_println!("[desktop] leaving");
}

/// Dirty rect covering the cursor bitmap at its hotspot-relative origin.
/// Clip negative left/top edges here; `mark_dirty` clips right/bottom.
///
/// Sized from the maximum cursor dimension (not the current bitmap): the
/// selection can change size at runtime, and the old (larger) bitmap must
/// still be fully repainted. Hotspot is unknown here, so the rect spans the
/// max extent in every direction (at most ~128px — one extra tile repaint).
fn cursor_cell(x: i32, y: i32) -> crate::desktop::scene::Rect {
    let m = crate::desktop::cursor::max_bundled_dim() as i32;
    let left = x - m;
    let top = y - m;
    let ox = left.max(0);
    let oy = top.max(0);
    let right = (x + m).max(ox);
    let bottom = (y + m).max(oy);
    crate::desktop::scene::Rect::new(ox, oy, (right - ox) as u32, (bottom - oy) as u32)
}

fn resize_hit(bounds: crate::desktop::scene::Rect, x: i32, y: i32) -> Option<(bool, bool)> {
    if !bounds.contains_point(x, y) {
        return None;
    }
    let right = bounds.x + bounds.w as i32;
    let bottom = bounds.y + bounds.h as i32;
    let resize_x = x >= right - 10;
    let resize_y = y >= bottom - 10;
    (resize_x || resize_y).then_some((resize_x, resize_y))
}

/// Absolute screen rects of every visible button, front window first.
fn button_rects(
    scene: &crate::desktop::scene::Scene,
) -> Vec<(crate::desktop::scene::WidgetId, crate::desktop::scene::Rect)> {
    let mut out = Vec::new();
    for &win_id in scene.windows_z_order().iter().rev() {
        let win = match scene.windows.get(&win_id) {
            Some(w) if w.visible => w,
            _ => continue,
        };
        let mut stack = alloc::vec![(win.root_widget, win.bounds.x, win.bounds.y,)];
        while let Some((wid, ox, oy)) = stack.pop() {
            let w = match scene.widgets.get(&wid) {
                Some(w) if w.visible => w,
                _ => continue,
            };
            let abs = crate::desktop::scene::Rect::new(
                ox + w.bounds.x,
                oy + w.bounds.y,
                w.bounds.w,
                w.bounds.h,
            );
            if w.kind == crate::desktop::scene::WidgetKind::Button {
                out.push((wid, abs));
            }
            for &c in w.children.iter().rev() {
                stack.push((c, abs.x, abs.y));
            }
        }
    }
    out
}

/// Topmost button under the cursor, if any.
fn button_at(
    scene: &crate::desktop::scene::Scene,
    mx: i32,
    my: i32,
) -> Option<(
    crate::desktop::scene::WindowId,
    crate::desktop::scene::WidgetId,
)> {
    for &win_id in scene.windows_z_order().iter().rev() {
        let win = match scene.windows.get(&win_id) {
            Some(w) if w.visible => w,
            _ => continue,
        };
        if !win.bounds.contains_point(mx, my) {
            continue;
        }
        let mut stack = alloc::vec![(win.root_widget, win.bounds.x, win.bounds.y,)];
        while let Some((wid, ox, oy)) = stack.pop() {
            let w = match scene.widgets.get(&wid) {
                Some(w) if w.visible => w,
                _ => continue,
            };
            let abs = crate::desktop::scene::Rect::new(
                ox + w.bounds.x,
                oy + w.bounds.y,
                w.bounds.w,
                w.bounds.h,
            );
            if w.kind == crate::desktop::scene::WidgetKind::Button && abs.contains_point(mx, my) {
                return Some((win_id, wid));
            }
            for &c in w.children.iter().rev() {
                stack.push((c, abs.x, abs.y));
            }
        }
    }
    None
}

/// Hover highlight for the button under the cursor (others reset).
/// Repaints only buttons whose state flipped (cursor motion alone no
/// longer repaints the screen, so state changes must mark their own).
fn update_button_hover(scene: &mut crate::desktop::scene::Scene, mx: i32, my: i32) {
    let rects = button_rects(scene);
    let mut hovered = None;
    for (id, r) in &rects {
        if r.contains_point(mx, my) {
            hovered = Some(*id);
            break; // rects are front-first
        }
    }
    let mut flipped = Vec::new();
    for (id, r) in &rects {
        if let Some(b) = scene.widgets.get_mut(id) {
            if b.state == crate::desktop::scene::WidgetState::Pressed {
                continue;
            }
            let want = if Some(*id) == hovered {
                crate::desktop::scene::WidgetState::Hover
            } else {
                crate::desktop::scene::WidgetState::Normal
            };
            if b.state != want {
                b.state = want;
                flipped.push(*r);
            }
        }
    }
    for r in flipped {
        scene.mark_dirty(r);
    }
}

/// Mark pressed, fire `on_click`, restore the callback.
fn press_button(scene: &mut crate::desktop::scene::Scene, btn_id: crate::desktop::scene::WidgetId) {
    let cb = match scene.widgets.get_mut(&btn_id) {
        Some(b) => {
            b.state = crate::desktop::scene::WidgetState::Pressed;
            core::mem::replace(&mut b.on_click, None)
        }
        None => None,
    };
    if let Some(f) = cb {
        f(scene);
        if let Some(b) = scene.widgets.get_mut(&btn_id) {
            b.on_click = Some(f);
        }
    }
}

/// Reset all pressed buttons (on mouse-up), repainting each one.
fn release_buttons(scene: &mut crate::desktop::scene::Scene) {
    let rects = button_rects(scene);
    for (id, r) in &rects {
        let reset = match scene.widgets.get_mut(id) {
            Some(b) if b.state == crate::desktop::scene::WidgetState::Pressed => {
                b.state = crate::desktop::scene::WidgetState::Normal;
                true
            }
            _ => false,
        };
        if reset {
            scene.mark_dirty(*r);
        }
    }
}
