//! Copyright (c) Alexander Matzen. All rights reserved.
//! Licensed under the MIT license.

//! Desktop Image Viewer app: open + zoom + pan.
//!
//! Native GUI window reusing `wallpaper::decode_auto` (PNG/JPG/BMP/GIF)
//! plus a small built-in QOI decoder. Same pattern as Files/Drives/
//! Settings: buttons carry no `on_click` closures; clicks are dispatched
//! in `desktop::mod` via widget-id comparison. Single instance; reopen
//! via the taskbar View launcher restores it.
//!
//! Zoom model: `Percent` shows `orig * zoom/100` cropped to the viewport
//! (no huge allocation at 800%: only viewport pixels are materialized).
//! `Fit` scales the whole image into the viewport. Pan offsets are in
//! scaled-image pixels and clamped. Rendering is nearest-neighbor.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetId, WindowId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Zoom limits (percent).
pub const ZOOM_MIN: u32 = 25;
pub const ZOOM_MAX: u32 = 800;
pub const ZOOM_DEFAULT: u32 = 100;
/// Discrete steps cycled by the +/- buttons and keys.
pub const ZOOM_STEPS: [u32; 10] = [25, 50, 75, 100, 150, 200, 300, 400, 600, 800];
/// Largest scaled axis we track (pan math only; buffer stays viewport-size).
const MAX_SCALED_AXIS: u32 = 8192;

/// Pending image path queued by Files double-click / `view` shell command.
static PENDING_IMAGE: spin::Mutex<Option<String>> = spin::Mutex::new(None);

/// Queue an image path for the desktop viewer to open.
pub fn set_pending_image(path: &str) {
    let t = path.trim();
    if t.is_empty() {
        return;
    }
    let mut s = String::new();
    let n = t.len().min(256);
    s.push_str(&t[..n]);
    *PENDING_IMAGE.lock() = Some(s);
}

/// Take a queued image path, if any.
pub fn take_pending_image() -> Option<String> {
    PENDING_IMAGE.lock().take()
}

/// Peek at a queued image path (for auto-open on desktop entry).
pub fn pending_image() -> Option<String> {
    PENDING_IMAGE.lock().clone()
}

/// Case-insensitive image-extension check (superset of wallpaper's).
pub fn is_image_path(path: &str) -> bool {
    if crate::desktop::wallpaper::has_supported_extension(path) {
        return true;
    }
    let t = path.trim();
    let lower = t.to_ascii_lowercase();
    lower.ends_with(".qoi")
}

/// Viewer zoom mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerMode {
    Fit,
    Percent,
}

/// Viewer window state (kept on the desktop run() stack, not global).
pub struct ViewerApp {
    pub window: WindowId,
    pub path_widget: WidgetId,
    pub image_widget: WidgetId,
    pub status_widget: WidgetId,
    pub btn_zoom_out: WidgetId,
    pub btn_zoom_in: WidgetId,
    pub btn_fit: WidgetId,
    pub btn_100: WidgetId,
    pub btn_prev: WidgetId,
    pub btn_next: WidgetId,
    cur_path: String,
    orig_rgba: Vec<u8>,
    orig_w: u32,
    orig_h: u32,
    zoom: u32,
    mode: ViewerMode,
    pan_x: u32,
    pan_y: u32,
    /// Effective zoom of the last render (Fit computes it).
    eff_zoom: u32,
    status: String,
}

// ──────────────────────────────────────────────
// Pure helpers (unit-testable, no kernel deps)
// ──────────────────────────────────────────────

/// Next zoom step above `z` (clamped to ZOOM_MAX).
pub fn zoom_step_in(z: u32) -> u32 {
    for &s in &ZOOM_STEPS {
        if s > z {
            return s.min(ZOOM_MAX);
        }
    }
    ZOOM_MAX
}

/// Next zoom step below `z` (clamped to ZOOM_MIN).
pub fn zoom_step_out(z: u32) -> u32 {
    let mut prev = ZOOM_MIN;
    for &s in &ZOOM_STEPS {
        if s >= z {
            return prev;
        }
        prev = s;
    }
    prev
}

/// Scaled dimensions for `orig * zoom/100`, clamped to MAX_SCALED_AXIS.
pub fn scaled_dims(ow: u32, oh: u32, zoom: u32) -> (u32, u32) {
    if ow == 0 || oh == 0 {
        return (0, 0);
    }
    let sw = ((ow as u64 * zoom as u64) / 100).min(MAX_SCALED_AXIS as u64) as u32;
    let sh = ((oh as u64 * zoom as u64) / 100).min(MAX_SCALED_AXIS as u64) as u32;
    (sw.max(1), sh.max(1))
}

/// Fit zoom (percent) that puts `orig` inside `viewport`. Never below 1%.
pub fn fit_zoom(ow: u32, oh: u32, vw: u32, vh: u32) -> u32 {
    if ow == 0 || oh == 0 || vw == 0 || vh == 0 {
        return 100;
    }
    let zx = (vw as u64 * 100) / ow as u64;
    let zy = (vh as u64 * 100) / oh as u64;
    zx.min(zy).clamp(1, ZOOM_MAX as u64) as u32
}

/// Clamp pan so the viewport stays inside the scaled image.
pub fn clamp_pan(px: u32, py: u32, sw: u32, sh: u32, vw: u32, vh: u32) -> (u32, u32) {
    let mx = sw.saturating_sub(vw);
    let my = sh.saturating_sub(vh);
    (px.min(mx), py.min(my))
}

/// Nearest-neighbor scale+pan of RGBA `src` (ow x oh) into a
/// `dw x dh` buffer showing scaled-image origin `(pan_x, pan_y)`.
/// `scaled_w/h` is the full scaled extent; `dw/dh <= viewport`.
pub fn scale_pan_nearest(
    src: &[u8],
    ow: u32,
    oh: u32,
    scaled_w: u32,
    scaled_h: u32,
    pan_x: u32,
    pan_y: u32,
    dw: u32,
    dh: u32,
) -> Vec<u8> {
    let mut out = Vec::new();
    if ow == 0 || oh == 0 || scaled_w == 0 || scaled_h == 0 || dw == 0 || dh == 0 {
        return out;
    }
    out.resize(dw as usize * dh as usize * 4, 0);
    for y in 0..dh {
        let sy = (((pan_y + y) as u64 * oh as u64) / scaled_h as u64).min(oh as u64 - 1) as u32;
        for x in 0..dw {
            let sx = (((pan_x + x) as u64 * ow as u64) / scaled_w as u64).min(ow as u64 - 1) as u32;
            let si = ((sy * ow + sx) as usize) * 4;
            let di = ((y * dw + x) as usize) * 4;
            if si + 3 < src.len() && di + 3 < out.len() {
                out[di] = src[si];
                out[di + 1] = src[si + 1];
                out[di + 2] = src[si + 2];
                out[di + 3] = 255;
            }
        }
    }
    out
}

// ──────────────────────────────────────────────
// QOI decoder (tiny, no_std friendly)
// ──────────────────────────────────────────────

fn read_u32be(b: &[u8], off: usize) -> Option<u32> {
    if off + 4 > b.len() {
        return None;
    }
    Some(
        ((b[off] as u32) << 24)
            | ((b[off + 1] as u32) << 16)
            | ((b[off + 2] as u32) << 8)
            | (b[off + 3] as u32),
    )
}

pub fn is_qoi(bytes: &[u8]) -> bool {
    bytes.len() >= 14
        && bytes[0] == b'q'
        && bytes[1] == b'o'
        && bytes[2] == b'i'
        && bytes[3] == b'f'
}

/// Decode QOI (https://qoiformat.org). Returns RGBA8 row-major.
pub fn decode_qoi(bytes: &[u8]) -> Result<crate::desktop::wallpaper::DecodedImage, &'static str> {
    use crate::desktop::wallpaper::{DecodedImage, MAX_IMAGE_DIM, MAX_IMAGE_PIXELS};
    if !is_qoi(bytes) {
        return Err("not a QOI file");
    }
    if bytes.len() < 22 {
        return Err("qoi: truncated file");
    }
    let w = read_u32be(bytes, 4).ok_or("qoi: bad header")?;
    let h = read_u32be(bytes, 8).ok_or("qoi: bad header")?;
    let channels = bytes[12];
    if w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
        return Err("qoi: bad dimensions");
    }
    if (w as u64) * (h as u64) > MAX_IMAGE_PIXELS {
        return Err("qoi: image has too many pixels");
    }
    if channels != 3 && channels != 4 {
        return Err("qoi: bad channels (need 3 or 4)");
    }
    let npix = (w as usize) * (h as usize);
    let mut rgba = Vec::new();
    rgba.resize(npix * 4, 0);
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0u8, 0u8, 255u8];
    let mut pos = 14usize;
    let mut out = 0usize;
    let mut run = 0u32;
    // Data ends 8 bytes before EOF (end marker).
    let end = bytes.len().saturating_sub(8);
    while out < npix {
        if run > 0 {
            run -= 1;
        } else if pos < end {
            let b = bytes[pos];
            pos += 1;
            match b {
                0xFE => {
                    // QOI_OP_RGB
                    if pos + 2 >= end + 8 && pos + 2 > bytes.len() {
                        return Err("qoi: truncated chunk");
                    }
                    if pos + 3 > bytes.len() {
                        return Err("qoi: truncated chunk");
                    }
                    px[0] = bytes[pos];
                    px[1] = bytes[pos + 1];
                    px[2] = bytes[pos + 2];
                    pos += 3;
                }
                0xFF => {
                    // QOI_OP_RGBA
                    if pos + 4 > bytes.len() {
                        return Err("qoi: truncated chunk");
                    }
                    px[0] = bytes[pos];
                    px[1] = bytes[pos + 1];
                    px[2] = bytes[pos + 2];
                    px[3] = bytes[pos + 3];
                    pos += 4;
                }
                b if (b & 0xC0) == 0x00 => {
                    // QOI_OP_INDEX
                    px = index[(b & 0x3F) as usize];
                }
                b if (b & 0xC0) == 0x40 => {
                    // QOI_OP_DIFF: 2-bit per channel bias -2
                    px[0] = px[0].wrapping_add(((b >> 4) & 0x03).wrapping_sub(2));
                    px[1] = px[1].wrapping_add(((b >> 2) & 0x03).wrapping_sub(2));
                    px[2] = px[2].wrapping_add((b & 0x03).wrapping_sub(2));
                }
                b if (b & 0xC0) == 0x80 => {
                    // QOI_OP_LUMA
                    if pos >= bytes.len() {
                        return Err("qoi: truncated luma");
                    }
                    let b2 = bytes[pos];
                    pos += 1;
                    let dg = ((b & 0x3F).wrapping_sub(32)) as i16;
                    let dr = dg + (((b2 >> 4) & 0x0F).wrapping_sub(8)) as i16;
                    let db = dg + ((b2 & 0x0F).wrapping_sub(8)) as i16;
                    px[0] = px[0].wrapping_add(dr as u8);
                    px[1] = px[1].wrapping_add(dg as u8);
                    px[2] = px[2].wrapping_add(db as u8);
                }
                b if (b & 0xC0) == 0xC0 => {
                    // QOI_OP_RUN
                    run = (b & 0x3F) as u32;
                }
                _ => return Err("qoi: bad chunk"),
            }
            let hidx = (px[0] as usize * 3
                + px[1] as usize * 5
                + px[2] as usize * 7
                + px[3] as usize * 11)
                % 64;
            index[hidx] = px;
        } else {
            return Err("qoi: truncated pixels");
        }
        let o = out * 4;
        rgba[o] = px[0];
        rgba[o + 1] = px[1];
        rgba[o + 2] = px[2];
        rgba[o + 3] = px[3];
        out += 1;
    }
    // Validate end marker 00*7 + 01.
    let n = bytes.len();
    if n < 8 || bytes[n - 8..] != [0, 0, 0, 0, 0, 0, 0, 1] {
        return Err("qoi: bad end marker");
    }
    Ok(DecodedImage { rgba, w, h })
}

fn is_tiff(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && ((bytes[0] == b'I' && bytes[1] == b'I' && bytes[2] == 42 && bytes[3] == 0)
            || (bytes[0] == b'M' && bytes[1] == b'M' && bytes[2] == 0 && bytes[3] == 42))
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && bytes[0] == b'R'
        && bytes[1] == b'I'
        && bytes[2] == b'F'
        && bytes[3] == b'F'
        && bytes[8] == b'W'
        && bytes[9] == b'E'
        && bytes[10] == b'B'
        && bytes[11] == b'P'
}

fn is_svg(bytes: &[u8]) -> bool {
    let n = bytes.len().min(512);
    let head = &bytes[..n];
    let mut i = 0;
    while i < n && (head[i] == b' ' || head[i] == b'\t' || head[i] == b'\n' || head[i] == b'\r') {
        i += 1;
    }
    let h = &head[i.min(n)..];
    h.starts_with(b"<svg")
        || h.starts_with(b"<?xml")
        || h.windows(5).any(|w| w == b"<svg ")
        || h.windows(4).any(|w| w == b"<svg")
}

/// Decode any viewer-supported image: QOI first, then the wallpaper set.
/// TIFF/WebP/SVG are detected and rejected with an actionable message.
pub fn decode_image(bytes: &[u8]) -> Result<crate::desktop::wallpaper::DecodedImage, &'static str> {
    if is_qoi(bytes) {
        return decode_qoi(bytes);
    }
    if is_tiff(bytes) {
        return Err("TIFF not decoded in-kernel (convert to PNG/QOI on host)");
    }
    if is_webp(bytes) {
        return Err("WebP not decoded in-kernel (convert to PNG/QOI on host)");
    }
    if is_svg(bytes) {
        return Err("SVG needs rasterizing (convert to PNG on host)");
    }
    crate::desktop::wallpaper::decode_auto(bytes)
}

/// One-line description for status bars / shell `imginfo`.
pub fn describe_image(
    path: &str,
    img: &crate::desktop::wallpaper::DecodedImage,
    zoom: u32,
) -> String {
    let o = crate::desktop::wallpaper::orientation(img.w, img.h);
    let os = match o {
        crate::desktop::wallpaper::Orientation::Landscape => "landscape",
        crate::desktop::wallpaper::Orientation::Portrait => "portrait",
        crate::desktop::wallpaper::Orientation::Square => "square",
    };
    alloc::format!("{}: {}x{} {} @ {}%", path, img.w, img.h, os, zoom)
}

// ──────────────────────────────────────────────
// Window construction + rendering
// ──────────────────────────────────────────────

fn viewer_bounds(sw: usize, sh: usize) -> Rect {
    let w = (sw.saturating_sub(100)).min(720).max(500);
    let h = (sh.saturating_sub(110)).min(540).max(380);
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

/// Viewport (image widget) size for the current window size.
fn viewport_for(window_w: u32, window_h: u32) -> (u32, u32) {
    // Layout: path(22) + buttons(26) + image + status(56) + margins.
    let vw = window_w.saturating_sub(24).max(120);
    let vh = window_h.saturating_sub(22 + 8 + 26 + 8 + 56 + 24).max(120);
    (vw, vh)
}

/// Create the viewer window and load `path`. Fails soft with an Err string.
pub fn create_viewer_app(
    scene: &mut Scene,
    sw: usize,
    sh: usize,
    path: &str,
) -> Result<ViewerApp, String> {
    let bounds = viewer_bounds(sw, sh);
    let window = scene.create_window(String::from("Viewer"), bounds);
    let root = scene.windows.get(&window).unwrap().root_widget;
    let theme = scene.theme;
    let inner = (bounds.w as i32 - 20).max(100);

    let path_widget = WidgetId::new();
    push_child(
        scene,
        root,
        path_widget,
        Widget::label(
            Rect::new(10, 6, inner as u32, 22),
            String::from(path),
            &theme,
        ),
    );

    // 6 buttons: -  +  Fit  100%  Prev  Next
    let labels = ["- Zoom", "+ Zoom", "Fit", "100%", "< Prev", "Next >"];
    let mut btns = [
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
        WidgetId::new(),
    ];
    let btn_w = ((inner - 5 * 6) / 6).max(60) as u32;
    for (i, id) in btns.iter_mut().enumerate() {
        *id = WidgetId::new();
        push_child(
            scene,
            root,
            *id,
            Widget::button(
                Rect::new(10 + i as i32 * (btn_w as i32 + 6), 30, btn_w, 26),
                String::from(labels[i]),
                &theme,
            ),
        );
    }

    let (vw, vh) = viewport_for(bounds.w, bounds.h);
    let image_widget = WidgetId::new();
    let mut iw = Widget::new(
        crate::desktop::scene::WidgetKind::Image,
        Rect::new(10, 62, vw, vh),
        crate::desktop::scene::Style::default_panel(&theme),
    );
    iw.id = image_widget;
    scene.widgets.insert(image_widget, iw);
    if let Some(r) = scene.widgets.get_mut(&root) {
        r.children.push(image_widget);
    }

    let status_widget = WidgetId::new();
    push_child(
        scene,
        root,
        status_widget,
        Widget::label(
            Rect::new(10, 62 + vh as i32 + 6, inner as u32, 56),
            String::from("Loading..."),
            &theme,
        ),
    );

    let mut app = ViewerApp {
        window,
        path_widget,
        image_widget,
        status_widget,
        btn_zoom_out: btns[0],
        btn_zoom_in: btns[1],
        btn_fit: btns[2],
        btn_100: btns[3],
        btn_prev: btns[4],
        btn_next: btns[5],
        cur_path: String::from(path),
        orig_rgba: Vec::new(),
        orig_w: 0,
        orig_h: 0,
        zoom: ZOOM_DEFAULT,
        mode: ViewerMode::Fit,
        pan_x: 0,
        pan_y: 0,
        eff_zoom: 100,
        status: String::from("Loading..."),
    };
    if let Err(e) = open_image(scene, &mut app, path) {
        // Keep the window open showing the error (like Doom fatal state).
        app.status = String::from(e);
        render_status(scene, &app);
    }
    Ok(app)
}

/// Load `path` from the FS into the viewer (resets zoom/pan to Fit).
pub fn open_image(
    scene: &mut Scene,
    app: &mut ViewerApp,
    path: &str,
) -> Result<(u32, u32), &'static str> {
    let bytes = crate::shell::gui_read_file(path).map_err(|e| e)?;
    if bytes.is_empty() {
        return Err("empty file");
    }
    let img = decode_image(&bytes)?;
    if img.w == 0 || img.h == 0 {
        return Err("bad image dimensions");
    }
    app.cur_path = String::from(path);
    app.orig_w = img.w;
    app.orig_h = img.h;
    app.orig_rgba = img.rgba;
    app.zoom = ZOOM_DEFAULT;
    app.mode = ViewerMode::Fit;
    app.pan_x = 0;
    app.pan_y = 0;
    app.status = String::from("Loaded");
    rebuild_view(scene, app);
    crate::serial_println!("[viewer] opened '{}' {}x{}", path, app.orig_w, app.orig_h);
    Ok((app.orig_w, app.orig_h))
}

/// Re-render the viewport from `orig` + zoom/mode/pan.
pub fn rebuild_view(scene: &mut Scene, app: &mut ViewerApp) {
    let (vw, vh) = match scene.widgets.get(&app.image_widget) {
        Some(w) => (w.bounds.w.max(1), w.bounds.h.max(1)),
        None => return,
    };
    if app.orig_w == 0 || app.orig_h == 0 || app.orig_rgba.is_empty() {
        return;
    }
    let zoom = match app.mode {
        ViewerMode::Fit => fit_zoom(app.orig_w, app.orig_h, vw, vh),
        ViewerMode::Percent => app.zoom.clamp(ZOOM_MIN, ZOOM_MAX),
    };
    app.eff_zoom = zoom;
    let (sw, sh) = scaled_dims(app.orig_w, app.orig_h, zoom);
    let dw = sw.min(vw).max(1);
    let dh = sh.min(vh).max(1);
    (app.pan_x, app.pan_y) = clamp_pan(app.pan_x, app.pan_y, sw, sh, dw, dh);
    let data = scale_pan_nearest(
        &app.orig_rgba,
        app.orig_w,
        app.orig_h,
        sw,
        sh,
        app.pan_x,
        app.pan_y,
        dw,
        dh,
    );
    if let Some(w) = scene.widgets.get_mut(&app.image_widget) {
        w.image_data = Some(data);
        w.image_w = dw;
        w.image_h = dh;
    }
    render_status(scene, app);
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

fn render_status(scene: &mut Scene, app: &ViewerApp) {
    let mode_s = match app.mode {
        ViewerMode::Fit => "fit",
        ViewerMode::Percent => "zoom",
    };
    let pan_s = if app.orig_w == 0 {
        String::from("")
    } else {
        let (sw, sh) = scaled_dims(app.orig_w, app.orig_h, app.eff_zoom);
        let (vw, vh) = match scene.widgets.get(&app.image_widget) {
            Some(w) => (w.bounds.w, w.bounds.h),
            None => (0, 0),
        };
        if sw > vw || sh > vh {
            alloc::format!(" pan {},{}", app.pan_x, app.pan_y)
        } else {
            String::from("")
        }
    };
    if let Some(w) = scene.widgets.get_mut(&app.path_widget) {
        let mut t = alloc::format!("Image: {}", app.cur_path);
        if t.len() > 76 {
            t.truncate(76);
        }
        w.text = t;
    }
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        if app.orig_w == 0 {
            w.text = app.status.clone();
        } else {
            w.text = alloc::format!(
                "{}x{} {} {}%{} | Arrows pan, +/- zoom, F fit, 0 100%",
                app.orig_w,
                app.orig_h,
                mode_s,
                app.eff_zoom,
                pan_s
            );
        }
    }
}

/// Absolute screen rect of the image widget (for drag-pan hit-testing).
pub fn viewer_image_rect(scene: &Scene, app: &ViewerApp) -> Option<Rect> {
    let win = scene.windows.get(&app.window)?;
    let w = scene.widgets.get(&app.image_widget)?;
    let tb = scene.theme.metrics.titlebar_height as i32;
    Some(Rect::new(
        win.bounds.x + w.bounds.x,
        win.bounds.y + tb + w.bounds.y,
        w.bounds.w,
        w.bounds.h,
    ))
}

/// Drag-pan by a screen delta (called while left-dragging inside the image).
pub fn viewer_drag_pan(scene: &mut Scene, app: &mut ViewerApp, dx: i32, dy: i32) -> bool {
    if app.orig_w == 0 {
        return false;
    }
    let (sw, sh) = scaled_dims(app.orig_w, app.orig_h, app.eff_zoom);
    let (vw, vh) = match scene.widgets.get(&app.image_widget) {
        Some(w) => (w.bounds.w, w.bounds.h),
        None => return false,
    };
    let dw = sw.min(vw);
    let dh = sh.min(vh);
    if sw <= dw && sh <= dh {
        return false;
    }
    let nx = (app.pan_x as i32 + dx).clamp(0, sw.saturating_sub(dw) as i32) as u32;
    let ny = (app.pan_y as i32 + dy).clamp(0, sh.saturating_sub(dh) as i32) as u32;
    if nx == app.pan_x && ny == app.pan_y {
        return false;
    }
    app.pan_x = nx;
    app.pan_y = ny;
    rebuild_view(scene, app);
    true
}

fn set_zoom(scene: &mut Scene, app: &mut ViewerApp, z: u32) {
    app.mode = ViewerMode::Percent;
    app.zoom = z.clamp(ZOOM_MIN, ZOOM_MAX);
    // Keep the view roughly centered: pan will be clamped in rebuild.
    rebuild_view(scene, app);
}

/// Parent directory of a path (`/a/b.png` -> `/a`).
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

/// Step to the previous/next sibling image in the same directory.
fn step_sibling(scene: &mut Scene, app: &mut ViewerApp, forward: bool) {
    let parent = parent_of(&app.cur_path);
    let entries = match crate::shell::gui_list_dir(&parent) {
        Ok((_, e)) => e,
        Err(e) => {
            app.status = alloc::format!("List failed: {}", e);
            render_status(scene, app);
            return;
        }
    };
    let mut cands: Vec<String> = Vec::new();
    for fi in &entries {
        if !fi.is_directory && is_image_path(&fi.name) {
            cands.push(if parent == "/" {
                alloc::format!("/{}", fi.name)
            } else {
                alloc::format!("{}/{}", parent.trim_end_matches('/'), fi.name)
            });
        }
    }
    if cands.is_empty() {
        app.status = String::from("No images in this folder");
        render_status(scene, app);
        return;
    }
    let cur = cands.iter().position(|c| c == &app.cur_path).unwrap_or(0);
    let next = if forward {
        (cur + 1) % cands.len()
    } else {
        (cur + cands.len() - 1) % cands.len()
    };
    let pick = cands[next].clone();
    match open_image(scene, app, &pick) {
        Ok(_) => {}
        Err(e) => {
            app.status = alloc::format!("Open failed: {}", e);
            render_status(scene, app);
        }
    }
}

/// Check if widget id belongs to this viewer's buttons.
pub fn viewer_owns_button(app: &ViewerApp, btn: WidgetId) -> bool {
    btn == app.btn_zoom_out
        || btn == app.btn_zoom_in
        || btn == app.btn_fit
        || btn == app.btn_100
        || btn == app.btn_prev
        || btn == app.btn_next
}

/// Handle a viewer button click.
pub fn viewer_button(scene: &mut Scene, app: &mut ViewerApp, btn: WidgetId) {
    if btn == app.btn_zoom_in {
        let z = zoom_step_in(match app.mode {
            ViewerMode::Fit => app.eff_zoom,
            ViewerMode::Percent => app.zoom,
        });
        set_zoom(scene, app, z);
    } else if btn == app.btn_zoom_out {
        let z = zoom_step_out(match app.mode {
            ViewerMode::Fit => app.eff_zoom,
            ViewerMode::Percent => app.zoom,
        });
        set_zoom(scene, app, z);
    } else if btn == app.btn_fit {
        app.mode = ViewerMode::Fit;
        app.pan_x = 0;
        app.pan_y = 0;
        rebuild_view(scene, app);
    } else if btn == app.btn_100 {
        app.mode = ViewerMode::Percent;
        app.zoom = 100;
        app.pan_x = 0;
        app.pan_y = 0;
        rebuild_view(scene, app);
    } else if btn == app.btn_prev {
        step_sibling(scene, app, false);
    } else if btn == app.btn_next {
        step_sibling(scene, app, true);
    }
}

/// Keyboard handling when the viewer is focused. Returns true if consumed.
pub fn viewer_key(
    scene: &mut Scene,
    app: &mut ViewerApp,
    key: crate::drivers::keyboard::Key,
) -> bool {
    use crate::drivers::keyboard::Key;
    if app.orig_w == 0 {
        return false;
    }
    let cur = match app.mode {
        ViewerMode::Fit => app.eff_zoom,
        ViewerMode::Percent => app.zoom,
    };
    match key {
        Key::Char('+') | Key::Char('=') => {
            set_zoom(scene, app, zoom_step_in(cur));
            true
        }
        Key::Char('-') | Key::Char('_') => {
            set_zoom(scene, app, zoom_step_out(cur));
            true
        }
        Key::Char('0') => {
            app.mode = ViewerMode::Percent;
            app.zoom = 100;
            app.pan_x = 0;
            app.pan_y = 0;
            rebuild_view(scene, app);
            true
        }
        Key::Char('f') | Key::Char('F') => {
            app.mode = ViewerMode::Fit;
            app.pan_x = 0;
            app.pan_y = 0;
            rebuild_view(scene, app);
            true
        }
        Key::Char('n') | Key::Char('N') | Key::PageDown => {
            step_sibling(scene, app, true);
            true
        }
        Key::Char('p') | Key::Char('P') | Key::PageUp => {
            step_sibling(scene, app, false);
            true
        }
        Key::ArrowLeft => {
            viewer_drag_pan(scene, app, -20, 0);
            true
        }
        Key::ArrowRight => {
            viewer_drag_pan(scene, app, 20, 0);
            true
        }
        Key::ArrowUp => {
            viewer_drag_pan(scene, app, 0, -20);
            true
        }
        Key::ArrowDown => {
            viewer_drag_pan(scene, app, 0, 20);
            true
        }
        _ => false,
    }
}

/// Adjust child widths after window resize.
pub fn resize_viewer_content(scene: &mut Scene, app: &ViewerApp) {
    let win_w = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.w as i32,
        None => return,
    };
    let win_h = match scene.windows.get(&app.window) {
        Some(w) => w.bounds.h as i32,
        None => return,
    };
    let inner = (win_w - 20).max(100) as u32;
    let btn_w = ((inner as i32 - 5 * 6) / 6).max(60) as u32;
    for (i, id) in [
        app.btn_zoom_out,
        app.btn_zoom_in,
        app.btn_fit,
        app.btn_100,
        app.btn_prev,
        app.btn_next,
    ]
    .iter()
    .enumerate()
    {
        if let Some(b) = scene.widgets.get_mut(id) {
            b.bounds.x = 10 + i as i32 * (btn_w as i32 + 6);
            b.bounds.w = btn_w;
        }
    }
    let (vw, vh) = viewport_for(win_w.max(0) as u32, win_h.max(0) as u32);
    if let Some(w) = scene.widgets.get_mut(&app.path_widget) {
        w.bounds.w = inner;
    }
    if let Some(w) = scene.widgets.get_mut(&app.image_widget) {
        w.bounds.w = vw;
        w.bounds.h = vh;
    }
    if let Some(w) = scene.widgets.get_mut(&app.status_widget) {
        w.bounds.w = inner;
        w.bounds.y = 62 + vh as i32 + 6;
    }
    if let Some(win) = scene.windows.get(&app.window) {
        scene.mark_dirty(win.bounds);
    }
}

/// Rebuild after an external resize (needs &mut for re-render).
pub fn resize_viewer_rebuild(scene: &mut Scene, app: &mut ViewerApp) {
    resize_viewer_content(scene, app);
    if app.orig_w != 0 {
        rebuild_view(scene, app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_move_monotonically() {
        assert_eq!(zoom_step_in(100), 150);
        assert_eq!(zoom_step_out(100), 75);
        assert_eq!(zoom_step_in(800), 800);
        assert_eq!(zoom_step_out(25), 25);
        assert_eq!(zoom_step_in(26), 50);
    }

    #[test]
    fn fit_zoom_picks_constraining_axis() {
        // 800x600 into 400x400 -> 50%.
        assert_eq!(fit_zoom(800, 600, 400, 400), 50);
        // Portrait 600x800 into 400x400 -> 50%.
        assert_eq!(fit_zoom(600, 800, 400, 400), 50);
        // Small image upscales to fit.
        assert_eq!(fit_zoom(100, 100, 400, 200), 200);
    }

    #[test]
    fn scaled_dims_clamp_axis() {
        let (w, h) = scaled_dims(1920, 1080, 800);
        assert!(w <= MAX_SCALED_AXIS && h <= MAX_SCALED_AXIS);
        assert_eq!(scaled_dims(100, 50, 100), (100, 50));
    }

    #[test]
    fn pan_clamps_to_image() {
        assert_eq!(clamp_pan(9999, 9999, 800, 600, 400, 400), (400, 200));
        assert_eq!(clamp_pan(10, 20, 800, 600, 400, 400), (10, 20));
        // Image smaller than viewport: no pan.
        assert_eq!(clamp_pan(5, 5, 100, 100, 400, 400), (0, 0));
    }

    #[test]
    fn nearest_maps_corners() {
        // 2x2 red/green/blue/white, 1:1 into 2x2.
        let src = alloc::vec![
            255, 0, 0, 255, 0, 255, 0, 255, // row 0
            0, 0, 255, 255, 255, 255, 255, 255, // row 1
        ];
        let out = scale_pan_nearest(&src, 2, 2, 2, 2, 0, 0, 2, 2);
        assert_eq!(
            out,
            alloc::vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
        // Pan right by 1 at 2x zoom (4x4 scaled): shows green column.
        let out = scale_pan_nearest(&src, 2, 2, 4, 4, 2, 0, 1, 1);
        assert_eq!(out, alloc::vec![0, 255, 0, 255]);
    }

    #[test]
    fn qoi_sniff_and_tiny_decode() {
        // Hand-built 1x1 red QOI: header + QOI_OP_RGB + end marker.
        let mut f = alloc::vec![
            b'q', b'o', b'i', b'f', 0, 0, 0, 1, // w=1
            0, 0, 0, 1, // h=1
            4, 0, // channels=4, colorspace=0
            0xFE, 255, 0, 0, // RGB(255,0,0)
            0, 0, 0, 0, 0, 0, 0, 1, // end
        ];
        assert!(is_qoi(&f));
        let img = decode_qoi(&f).expect("tiny qoi");
        assert_eq!((img.w, img.h), (1, 1));
        assert_eq!(img.rgba.len(), 4);
        assert_eq!(img.rgba[0], 255);
        let _ = &mut f;
    }

    #[test]
    fn rejects_webp_tiff_svg_with_hint() {
        assert!(decode_image(b"RIFF\x00\x00\x00\x00WEBP").is_err());
        assert!(decode_image(b"II*\x00").is_err());
        assert!(decode_image(b"<svg width='1'/>").is_err());
        assert!(is_image_path("/a/b.QOI"));
        assert!(is_image_path("/wallpapers/bg.png"));
        assert!(!is_image_path("/docs/readme.txt"));
    }
}
