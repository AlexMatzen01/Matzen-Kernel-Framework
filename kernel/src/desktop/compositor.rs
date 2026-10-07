//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Desktop compositor with double buffering and dirty-rect rendering.
//!
//! Renders the scene graph to the framebuffer using double buffering
//! to prevent screen tearing.

use crate::desktop::scene::{Rect, Scene, Widget, WidgetKind, WidgetState, Window, WindowId};
use crate::desktop::theme::{Theme, ThemeColors};
use crate::drivers::fb::{FrameBufferInfo, PixelFormat};
use crate::drivers::fb_gfx;
use crate::drivers::vga::Color;
use crate::serial_println;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cmp;

/// Compositor for rendering the desktop scene.
pub struct Compositor {
    /// Screen dimensions
    screen_w: u32,
    screen_h: u32,
    /// Framebuffer info for bounds
    fb_info: Option<FrameBufferInfo>,
    /// Frame counter for stats
    frame_count: u64,
    /// Aggregate render cycles for periodic serial diagnostics.
    render_cycles: u64,
    /// Aggregate damaged area for periodic serial diagnostics.
    damaged_pixels: u64,
}

impl Compositor {
    /// Create a new compositor.
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        Self {
            screen_w,
            screen_h,
            fb_info: None,
            frame_count: 0,
            render_cycles: 0,
            damaged_pixels: 0,
        }
    }

    /// Set framebuffer info for bounds checking.
    pub fn set_fb_info(&mut self, info: FrameBufferInfo) {
        self.fb_info = Some(info);
    }

    /// Render the entire scene to the framebuffer.
    pub fn render(
        &mut self,
        scene: &mut Scene,
        cursor_x: i32,
        cursor_y: i32,
        cursor_visible: bool,
    ) {
        let start_cycles = unsafe { core::arch::x86_64::_rdtsc() };

        // Get dirty regions
        let dirty_regions = scene.take_dirty_regions();

        if dirty_regions.is_empty() {
            // Nothing to draw
            return;
        }

        self.frame_count += 1;
        for region in &dirty_regions {
            let clip = (
                region.x.max(0) as usize,
                region.y.max(0) as usize,
                region.w as usize,
                region.h as usize,
            );
            fb_gfx::set_draw_clip(Some(clip));

            // Rebuild only this damaged area in correct z-order. The drawing
            // API clips every fill, glyph and sprite to `clip`, so moving the
            // pointer no longer redraws whole windows/taskbar to GOP memory.
            self.paint_wallpaper(
                region.x.max(0) as u32,
                region.y.max(0) as u32,
                region.w,
                region.h,
                &scene.theme,
            );

            for &win_id in scene.windows_z_order() {
                if let Some(window) = scene.windows.get(&win_id) {
                    if window.visible && window.bounds.intersects(region) {
                        self.render_window(scene, window);
                    }
                }
            }

            // Taskbar is redrawn only if the damaged area reaches it.
            let taskbar = Rect::new(
                0,
                self.screen_h
                    .saturating_sub(scene.theme.metrics.taskbar_height) as i32,
                self.screen_w,
                scene.theme.metrics.taskbar_height,
            );
            if region.intersects(&taskbar) {
                self.render_taskbar(scene);
            }

            // Cursor is the topmost layer and is clipped to the damage area.
            // Bounds use the max cursor dimension: the bitmap is
            // user-selectable (bundled or /cursors file, up to 128px).
            if cursor_visible {
                let m = crate::desktop::cursor::max_bundled_dim() as i32;
                let cursor = Rect::new(cursor_x - m, cursor_y - m, (m * 2) as u32, (m * 2) as u32);
                if region.intersects(&cursor) {
                    self.render_cursor(cursor_x, cursor_y);
                }
            }
            self.damaged_pixels += region.w as u64 * region.h as u64;
        }
        fb_gfx::set_draw_clip(None);

        // Publish exactly the regions we painted. The scene list was drained
        // above, so use the local copy.
        let dirty_px: Vec<(usize, usize, usize, usize)> = dirty_regions
            .iter()
            .map(|r| (r.x as usize, r.y as usize, r.w as usize, r.h as usize))
            .collect();
        crate::drivers::fb_gfx::present(&dirty_px);

        self.render_cycles += unsafe { core::arch::x86_64::_rdtsc() } - start_cycles;
        if self.frame_count % 120 == 0 {
            crate::serial_println!(
                "[desktop] 120 rendered frames: avg {} TSC cycles, avg {} damaged pixels",
                self.render_cycles / 120,
                self.damaged_pixels / 120
            );
            self.render_cycles = 0;
            self.damaged_pixels = 0;
        }
    }

    /// Clear a region of the back buffer to a solid color.
    fn clear_back_buffer(&self, x: u32, y: u32, w: u32, h: u32, r: u8, g: u8, b: u8) {
        fb_gfx::fill_rect_px(x as usize, y as usize, w as usize, h as usize, r, g, b);
    }

    /// Wallpaper background: image cache when a wallpaper is set, otherwise
    /// a modern dark gradient (bg -> black) plus a bottom scrim so the
    /// glass taskbar stays readable. Image path returns early (single blit).
    fn paint_wallpaper(&self, x: u32, y: u32, w: u32, h: u32, theme: &Theme) {
        if crate::desktop::wallpaper::paint_cached_fullscreen(self.screen_w, self.screen_h) {
            return;
        }
        let top = theme.colors.bg;
        fb_gfx::paint_wallpaper_bb(
            x as usize,
            y as usize,
            w as usize,
            h as usize,
            (
                ((top >> 16) & 0xFF) as u8,
                ((top >> 8) & 0xFF) as u8,
                (top & 0xFF) as u8,
            ),
        );
        // Bottom scrim: 90px gradient from bg to near-black for taskbar legibility.
        let sh = self.screen_h as usize;
        let scrim_h = 90usize.min(sh);
        let scrim_y = sh.saturating_sub(scrim_h);
        let ry0 = (y as usize).max(scrim_y);
        let ry1 = (y as usize + h as usize).min(sh);
        for row in ry0..ry1 {
            let t = ((row - scrim_y) * 255 / scrim_h.max(1)) as u8;
            // Blend bg -> black@55% progressively.
            let a = (t as u32 * 140 / 255) as u8;
            let c = fb_gfx::blend_u32(0x000000, top, a);
            let (r, g, b) = fb_gfx::split_rgb(c);
            fb_gfx::fill_rect_px(x as usize, row, w as usize, 1, r, g, b);
        }
    }

    /// Bottom taskbar: glass strip, accent top line, pill launchers.
    fn render_taskbar(&self, scene: &Scene) {
        let sw = self.screen_w as usize;
        let sh = self.screen_h as usize;
        let tb_h = scene.theme.metrics.taskbar_height as usize;
        if tb_h == 0 || sh <= tb_h {
            return;
        }
        let y0 = sh - tb_h;
        // Glass strip (gradient) + accent line + top glass highlight.
        fb_gfx::fill_gradient_px(
            0,
            y0,
            sw,
            tb_h,
            scene.theme.colors.taskbar_bg,
            0x05070A,
            0,
        );
        let accent = scene.theme.colors.accent;
        self.fill_rect(0, y0, sw, 2, accent);
        fb_gfx::glass_top_line_px(
            0,
            y0 + 2,
            sw,
            0,
            fb_gfx::blend_u32(0xFFFFFF, scene.theme.colors.taskbar_bg, 28),
        );

        let clip = Some((0, y0, sw, tb_h));
        // Start orb: accent pill with "MFK".
        fb_gfx::fill_rounded_rect_px(10, y0 + 6, 52, tb_h - 12, 8, accent);
        self.draw_text(
            18,
            y0 + (tb_h.saturating_sub(16)) / 2,
            "MFK",
            0xFFFFFF,
            14,
            clip,
        );
        // Launchers: pill buttons, focused launcher glows accent.
        let launchers = [
            (scene.shell_launcher_rect(), "Shell"),
            (scene.files_launcher_rect(), "Files"),
            (scene.drive_launcher_rect(), "Drive"),
            (scene.settings_launcher_rect(), "Settings"),
            (scene.doom_launcher_rect(), "Doom"),
            (scene.viewer_launcher_rect(), "View"),
        ];
        for (launcher, name) in launchers {
            let lx = launcher.x.max(0) as usize;
            let ly = launcher.y.max(0) as usize;
            let lw = launcher.w as usize;
            let lh = launcher.h as usize;
            let focused_title = scene
                .focused_window
                .and_then(|id| scene.windows.get(&id))
                .map(|w| w.title.as_str().contains(name))
                .unwrap_or(false);
            let bg = if focused_title {
                fb_gfx::blend_u32(accent, 0x1D2632, 120)
            } else {
                0x1D2632
            };
            fb_gfx::fill_rounded_rect_px(lx, ly, lw, lh, 8, bg);
            if focused_title {
                // Accent underline dot for the active app.
                let (r, g, b) = fb_gfx::split_rgb(accent);
                fb_gfx::fill_rect_px(lx + lw / 2 - 8, ly + lh - 3, 16, 2, r, g, b);
            }
            self.draw_text(
                lx + 10,
                ly + lh.saturating_sub(16) / 2,
                name,
                0xFFFFFF,
                12,
                clip,
            );
        }
        // Focused/active window titles as task buttons.
        let last_launcher = scene.viewer_launcher_rect();
        let mut tx = last_launcher.x as usize + last_launcher.w as usize + 12;
        for &win_id in scene.windows_z_order() {
            if let Some(win) = scene.windows.get(&win_id) {
                let label = ellipsize(&win.title, 18);
                let col = if Some(win_id) == scene.focused_window {
                    0xFFFFFF
                } else {
                    scene.theme.colors.text_muted
                };
                self.draw_text(
                    tx,
                    y0 + (tb_h.saturating_sub(16)) / 2,
                    &label,
                    col,
                    12,
                    clip,
                );
                tx += label.len() * 8 + 24;
                if tx + 20 * 8 >= sw {
                    break;
                }
            }
        }
        // Exit hint, right-aligned.
        let hint = "Esc: exit";
        let hx = sw.saturating_sub(hint.len() * 8 + 12);
        self.draw_text(
            hx,
            y0 + (tb_h.saturating_sub(16)) / 2,
            hint,
            scene.theme.colors.text_muted,
            12,
            clip,
        );
    }

    /// Render a window: drop shadow, rounded body, gradient titlebar,
    /// glass highlight, pill controls.
    fn render_window(&self, scene: &Scene, window: &Window) {
        let theme = &scene.theme;
        let rad = theme.metrics.panel_radius as usize;
        let wx = window.bounds.x.max(0) as usize;
        let wy = window.bounds.y.max(0) as usize;
        let ww = window.bounds.w as usize;
        let wh = window.bounds.h as usize;
        if ww == 0 || wh == 0 {
            return;
        }

        // Soft shadow first (paints under the window; clipped to damage).
        fb_gfx::shadow_rounded_px(wx, wy, ww, wh, rad);

        // Window body.
        fb_gfx::fill_rounded_rect_px(wx, wy, ww, wh, rad, theme.colors.panel_bg);

        // Subtle border via inset outline (rounded corners keep 1px).
        {
            let (r, g, b) = fb_gfx::split_rgb(theme.colors.panel_border);
            // Top/bottom/left/right 1px lines inside the rounded shape.
            fb_gfx::fill_rect_px(wx + rad, wy, ww.saturating_sub(rad * 2), 1, r, g, b);
            fb_gfx::fill_rect_px(
                wx + rad,
                wy + wh - 1,
                ww.saturating_sub(rad * 2),
                1,
                r,
                g,
                b,
            );
            fb_gfx::fill_rect_px(wx, wy + rad, 1, wh.saturating_sub(rad * 2), r, g, b);
            fb_gfx::fill_rect_px(
                wx + ww - 1,
                wy + rad,
                1,
                wh.saturating_sub(rad * 2),
                r,
                g,
                b,
            );
        }

        // Titlebar gradient.
        let tb_h = scene.theme.metrics.titlebar_height as usize;
        let (tb_top, tb_bot) = if window.focused {
            (
                scene.theme.colors.title_active_top,
                scene.theme.colors.title_active,
            )
        } else {
            (
                scene.theme.colors.title_inactive_top,
                scene.theme.colors.title_inactive,
            )
        };
        fb_gfx::fill_gradient_px(wx, wy, ww, tb_h, tb_top, tb_bot, rad);
        // Glass highlight + focused accent underline.
        fb_gfx::glass_top_line_px(
            wx,
            wy + 1,
            ww,
            rad,
            fb_gfx::blend_u32(0xFFFFFF, tb_top, 36),
        );
        if window.focused {
            let (r, g, b) = fb_gfx::split_rgb(scene.theme.colors.accent_glow);
            fb_gfx::fill_rect_px(wx + rad, wy + tb_h - 2, ww.saturating_sub(rad * 2), 2, r, g, b);
        }

        // Title text, truncated with ellipsis when too long for the bar.
        let minimize_rect = window.minimize_button_rect(&scene.theme);
        let maximize_rect = window.maximize_button_rect(&scene.theme);
        let close_rect = window.close_button_rect(&scene.theme);
        let title_clip = (
            window.bounds.x as usize,
            window.bounds.y as usize,
            window.bounds.w as usize,
            tb_h,
        );
        let title_avail_px = (window.bounds.w as usize)
            .saturating_sub((close_rect.w + maximize_rect.w + minimize_rect.w) as usize + 34);
        let title = ellipsize(&window.title, title_avail_px / 8);
        self.draw_text(
            window.bounds.x as usize + 10,
            window.bounds.y as usize + (tb_h.saturating_sub(16)) / 2,
            &title,
            0xFFFFFF,
            14,
            Some(title_clip),
        );

        // Pill controls: minimize / maximize / close.
        let pill_r = 7usize;
        fb_gfx::fill_rounded_rect_px(
            minimize_rect.x.max(0) as usize,
            minimize_rect.y.max(0) as usize,
            minimize_rect.w as usize,
            minimize_rect.h as usize,
            pill_r,
            0x2A3441,
        );
        self.fill_rect(
            minimize_rect.x as usize + 7,
            minimize_rect.y as usize + minimize_rect.h as usize / 2,
            minimize_rect.w as usize - 14,
            2,
            0xFFFFFF,
        );

        fb_gfx::fill_rounded_rect_px(
            maximize_rect.x.max(0) as usize,
            maximize_rect.y.max(0) as usize,
            maximize_rect.w as usize,
            maximize_rect.h as usize,
            pill_r,
            0x2A3441,
        );
        let mx = maximize_rect.x as usize + 8;
        let my = maximize_rect.y as usize + 8;
        let mw = (maximize_rect.w as usize).saturating_sub(16);
        let mh = (maximize_rect.h as usize).saturating_sub(16);
        self.draw_rect_outline(mx, my, mw.max(2), mh.max(2), 0xFFFFFF, 1);

        let cb_x = close_rect.x.max(0) as usize;
        let cb_y = close_rect.y.max(0) as usize;
        let cb_w = close_rect.w as usize;
        let cb_h = close_rect.h as usize;

        fb_gfx::fill_rounded_rect_px(cb_x, cb_y, cb_w, cb_h, pill_r, 0xD64949);
        fb_gfx::glass_top_line_px(
            cb_x,
            cb_y,
            cb_w,
            pill_r,
            fb_gfx::blend_u32(0xFFFFFF, 0xD64949, 70),
        );
        self.draw_text(
            cb_x + cb_w / 2 - 4,
            cb_y + (cb_h.saturating_sub(16)) / 2,
            "X",
            0xFFFFFF,
            12,
            Some(title_clip),
        );

        // Widget content is clipped to the window body (below titlebar,
        // inside the border) so text can never spill onto the desktop.
        let bw = window.style.border_width as usize;
        let body_clip = (
            (window.bounds.x as usize).saturating_add(bw),
            (window.bounds.y as usize).saturating_add(tb_h),
            (window.bounds.w as usize).saturating_sub(bw * 2),
            (window.bounds.h as usize).saturating_sub(tb_h + bw),
        );
        // Render widgets recursively
        if scene.widgets.get(&window.root_widget).is_some() {
            self.render_widget_tree(
                scene,
                window.root_widget,
                window.bounds.x,
                window.bounds.y,
                body_clip,
            );
        }

        // Small resize grip in the lower-right corner (hidden while maximized).
        if window.restore_bounds.is_none() {
            let gx =
                window.bounds.x.max(0) as usize + (window.bounds.w as usize).saturating_sub(13);
            let gy =
                window.bounds.y.max(0) as usize + (window.bounds.h as usize).saturating_sub(13);
            for offset in [0usize, 4, 8] {
                fb_gfx::fill_rect_px(
                    gx + offset,
                    gy + 8usize.saturating_sub(offset),
                    2,
                    2,
                    150,
                    160,
                    170,
                );
            }
        }
    }

    /// Recursively render widget tree. `clip` is the window body rect in
    /// screen pixels; every text draw below is clipped to it.
    fn render_widget_tree(
        &self,
        scene: &Scene,
        widget_id: crate::desktop::scene::WidgetId,
        parent_x: i32,
        parent_y: i32,
        clip: (usize, usize, usize, usize),
    ) {
        // Clone the child list first so the borrow ends before recursion.
        let (visible, abs_x, abs_y, children) = match scene.widgets.get(&widget_id) {
            Some(w) => (
                w.visible,
                parent_x + w.bounds.x,
                parent_y + w.bounds.y,
                w.children.clone(),
            ),
            None => return,
        };
        if !visible {
            return;
        }
        if let Some(widget) = scene.widgets.get(&widget_id) {
            self.render_widget(scene, widget, abs_x, abs_y, clip);
        }
        // Render children
        for child_id in children {
            self.render_widget_tree(scene, child_id, abs_x, abs_y, clip);
        }
    }

    /// Render a single widget.
    fn render_widget(
        &self,
        scene: &Scene,
        widget: &Widget,
        x: i32,
        y: i32,
        clip: (usize, usize, usize, usize),
    ) {
        let theme = &scene.theme;
        let x = x as usize;
        let y = y as usize;
        let w = widget.bounds.w as usize;
        let h = widget.bounds.h as usize;

        match widget.kind {
            WidgetKind::Panel => {
                self.fill_rect(x, y, w, h, widget.style.bg_color);
                if widget.style.border_width > 0 {
                    self.draw_rect_outline(
                        x,
                        y,
                        w,
                        h,
                        widget.style.border_color,
                        widget.style.border_width as usize,
                    );
                }
                if widget.style.radius > 0 {
                    // Draw rounded corners approximation
                    self.draw_rounded_rect(
                        x,
                        y,
                        w,
                        h,
                        widget.style.radius as usize,
                        widget.style.bg_color,
                    );
                }
            }

            WidgetKind::Button => {
                let (bg, top) = match widget.state {
                    WidgetState::Pressed => (
                        theme.colors.button_press,
                        theme.colors.button_press,
                    ),
                    WidgetState::Hover => (
                        theme.colors.button_hover,
                        fb_gfx::blend_u32(0xFFFFFF, theme.colors.button_hover, 30),
                    ),
                    _ => (
                        widget.style.bg_color,
                        fb_gfx::blend_u32(0xFFFFFF, widget.style.bg_color, 22),
                    ),
                };
                let rad = widget.style.radius as usize;
                // Pill with vertical sheen + hover glow underline.
                fb_gfx::fill_gradient_px(x, y, w, h, top, bg, rad);
                fb_gfx::glass_top_line_px(
                    x,
                    y,
                    w,
                    rad,
                    fb_gfx::blend_u32(0xFFFFFF, top, 50),
                );
                if widget.state == WidgetState::Hover {
                    let (r, g, b) = fb_gfx::split_rgb(theme.colors.accent_glow);
                    fb_gfx::fill_rect_px(
                        x + rad.min(w / 2),
                        y + h.saturating_sub(2),
                        w.saturating_sub(rad.min(w / 2) * 2),
                        1,
                        r,
                        g,
                        b,
                    );
                }

                // Button text (centered, clipped to the button).
                if !widget.text.is_empty() {
                    let text_w = widget.text.len() * 8;
                    let text_x = x + (w.saturating_sub(text_w)) / 2;
                    let text_y = y + (h.saturating_sub(16)) / 2;
                    let btn_clip = (x.max(0) as usize, y.max(0) as usize, w, h);
                    self.draw_text(
                        text_x,
                        text_y,
                        &widget.text,
                        widget.style.fg_color,
                        widget.style.font_size,
                        Some(btn_clip),
                    );
                }
            }

            WidgetKind::Label => {
                if !widget.text.is_empty() {
                    // Word-wrap to the widget width and clip to its height.
                    let max_chars =
                        (w.saturating_sub(widget.style.padding_x as usize * 2) / 8).max(1);
                    let max_lines = h.saturating_sub(widget.style.padding_y as usize * 2) / 16;
                    let lines = wrap_text(&widget.text, max_chars);
                    for (i, line) in lines.iter().enumerate() {
                        if i >= max_lines.max(1) {
                            break;
                        }
                        self.draw_text(
                            x + widget.style.padding_x as usize,
                            y + widget.style.padding_y as usize + i * 16,
                            line,
                            widget.style.fg_color,
                            widget.style.font_size,
                            Some(clip),
                        );
                    }
                }
            }

            WidgetKind::TextInput => {
                self.fill_rect(x, y, w, h, widget.style.bg_color);
                self.draw_rect_outline(
                    x,
                    y,
                    w,
                    h,
                    widget.style.border_color,
                    widget.style.border_width as usize,
                );
                if widget.style.radius > 0 {
                    self.draw_rounded_rect(
                        x,
                        y,
                        w,
                        h,
                        widget.style.radius as usize,
                        widget.style.bg_color,
                    );
                }

                // Text content (single line, clipped to the field).
                if !widget.text.is_empty() {
                    let field_clip = (x.max(0) as usize, y.max(0) as usize, w, h);
                    self.draw_text(
                        x + widget.style.padding_x as usize,
                        y + (h.saturating_sub(16)) / 2,
                        &widget.text,
                        widget.style.fg_color,
                        widget.style.font_size,
                        Some(field_clip),
                    );
                }

                // Cursor
                if widget.focused {
                    let cursor_x = x
                        + widget.style.padding_x as usize
                        + widget.cursor_pos * (widget.style.font_size as usize * 6 / 10);
                    let cursor_y = y + (h.saturating_sub(widget.style.font_size as usize)) / 2;
                    let cursor_h = widget.style.font_size as usize;
                    fb_gfx::fill_rect_px(cursor_x, cursor_y, 2, cursor_h, 255, 255, 255);
                }

                // Placeholder
                if widget.text.is_empty() && !widget.focused {
                    if let Some(placeholder) = widget.text.as_str().strip_prefix("placeholder:") {
                        // This is a hack - in real impl we'd store placeholder separately
                    }
                }
            }

            WidgetKind::Image => {
                if let Some(ref data) = widget.image_data {
                    crate::drivers::fb_gfx::blit_rgba(
                        x,
                        y,
                        widget.image_w as usize,
                        widget.image_h as usize,
                        data,
                    );
                }
            }

            WidgetKind::Custom => {
                // Custom widget - would call Lua callback in full implementation
            }
        }
    }

    /// Fill a rectangle with a solid color.
    fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        fb_gfx::fill_rect_px(x, y, w, h, r, g, b);
    }

    /// Draw a rectangle outline.
    fn draw_rect_outline(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        color: u32,
        thickness: usize,
    ) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        fb_gfx::fill_rect_px(x, y, w, thickness, r, g, b);
        fb_gfx::fill_rect_px(x, y + h - thickness, w, thickness, r, g, b);
        fb_gfx::fill_rect_px(x, y, thickness, h, r, g, b);
        fb_gfx::fill_rect_px(x + w - thickness, y, thickness, h, r, g, b);
    }

    /// Draw a rounded rectangle (approximation).
    fn draw_rounded_rect(&self, x: usize, y: usize, w: usize, h: usize, radius: usize, color: u32) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        let radius = radius.min(w / 2).min(h / 2);

        // Top bar
        fb_gfx::fill_rect_px(x + radius, y, w - 2 * radius, radius, r, g, b);
        // Bottom bar
        fb_gfx::fill_rect_px(x + radius, y + h - radius, w - 2 * radius, radius, r, g, b);
        // Middle
        fb_gfx::fill_rect_px(x, y + radius, w, h - 2 * radius, r, g, b);
        // Left bar
        fb_gfx::fill_rect_px(x, y + radius, radius, h - 2 * radius, r, g, b);
        // Right bar
        fb_gfx::fill_rect_px(x + w - radius, y + radius, radius, h - 2 * radius, r, g, b);
        // Four corners (squares for approximation)
        fb_gfx::fill_rect_px(x, y, radius, radius, r, g, b);
        fb_gfx::fill_rect_px(x + w - radius, y, radius, radius, r, g, b);
        fb_gfx::fill_rect_px(x, y + h - radius, radius, radius, r, g, b);
        fb_gfx::fill_rect_px(x + w - radius, y + h - radius, radius, radius, r, g, b);
    }

    /// Draw text at exact pixel position with a transparent background.
    /// `color` is 24-bit RGB; `clip` bounds the glyph pixels (window body).
    fn draw_text(
        &self,
        x: usize,
        y: usize,
        text: &str,
        color: u32,
        _font_size: u32,
        clip: Option<(usize, usize, usize, usize)>,
    ) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        fb_gfx::draw_text_bb(x, y, text, (r, g, b), clip);
    }

    /// Render the mouse cursor (currently selected bundled or custom one).
    fn render_cursor(&self, x: i32, y: i32) {
        // `x,y` are the pointer hotspot coordinates from the guest input
        // device; the bitmap origin is offset from its declared tip hotspot.
        crate::desktop::cursor::with_current(|w, h, hx, hy, rgba| {
            crate::drivers::fb_gfx::blit_rgba_signed(
                x - hx as i32,
                y - hy as i32,
                w,
                h,
                rgba,
            );
        });
    }
}

/// Truncate `s` to `max_chars` (ASCII-safe), appending "..." when cut.
fn ellipsize(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.len() <= max_chars {
        return s.into();
    }
    if max_chars <= 3 {
        return s[..max_chars].into();
    }
    let mut out: String = s[..max_chars - 3].into();
    out += "...";
    out
}

/// Greedy word-wrap: split on newlines, then wrap each paragraph to
/// `max_chars` columns. Long words are hard-broken. ASCII-only (the
/// framebuffer font has no Unicode glyphs), so byte indices are safe.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut lines: Vec<String> = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in para.split(' ') {
            if word.is_empty() {
                // Collapse runs of spaces to one.
                if !cur.is_empty() {
                    cur += " ";
                }
                continue;
            }
            // Hard-break overlong words first.
            let mut rest = word;
            while rest.len() > max_chars {
                if !cur.is_empty() {
                    lines.push(core::mem::take(&mut cur));
                }
                lines.push(rest[..max_chars].into());
                rest = &rest[max_chars..];
            }
            if cur.is_empty() {
                cur = rest.into();
            } else if cur.len() + 1 + rest.len() <= max_chars {
                cur += " ";
                cur += rest;
            } else {
                lines.push(core::mem::take(&mut cur));
                cur = rest.into();
            }
        }
        lines.push(cur);
    }
    lines
}
