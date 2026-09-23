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
            if cursor_visible {
                let cursor = Rect::new(cursor_x, cursor_y, 32, 32);
                if region.intersects(&cursor) {
                    self.render_cursor(cursor_x, cursor_y);
                }
            }
            self.damaged_pixels += region.w as u64 * region.h as u64;
        }
        fb_gfx::set_draw_clip(None);

        // Swap + present exactly the regions we painted. NOTE: the scene
        // list was already drained above, so use the local copy — the
        // scene list is empty here by design.
        let dirty_px: Vec<(usize, usize, usize, usize)> = dirty_regions
            .iter()
            .map(|r| (r.x as usize, r.y as usize, r.w as usize, r.h as usize))
            .collect();
        crate::drivers::fb_gfx::swap_buffers(&dirty_px);
        crate::drivers::fb_gfx::present_to_hardware(&dirty_px);

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

    /// Wallpaper background: image cache when a wallpaper is set (Fit /
    /// Fill / Stretch / Center / Tile pre-rendered by `wallpaper.rs`),
    /// otherwise the classic theme gradient. The image path is a single
    /// clipped blit; `fb_gfx` restricts it to the dirty rect.
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
    }

    /// Bottom taskbar: dark strip, accent top line, window buttons.
    fn render_taskbar(&self, scene: &Scene) {
        let sw = self.screen_w as usize;
        let sh = self.screen_h as usize;
        let tb_h = scene.theme.metrics.taskbar_height as usize;
        if tb_h == 0 || sh <= tb_h {
            return;
        }
        let y0 = sh - tb_h;
        // Dark strip + accent line.
        self.fill_rect(0, y0, sw, tb_h, 0x101418);
        let accent = scene.theme.colors.accent;
        self.fill_rect(0, y0, sw, 2, accent);

        let clip = Some((0, y0, sw, tb_h));
        // Start label.
        self.draw_text(
            12,
            y0 + (tb_h.saturating_sub(16)) / 2,
            "MFK",
            0xFFFFFF,
            14,
            clip,
        );
        // Launchers: Shell | Files | Drive | Settings
        let launchers = [
            (scene.shell_launcher_rect(), "Shell"),
            (scene.files_launcher_rect(), "Files"),
            (scene.drive_launcher_rect(), "Drive"),
            (scene.settings_launcher_rect(), "Settings"),
        ];
        for (launcher, name) in launchers {
            self.fill_rect(
                launcher.x as usize,
                launcher.y as usize,
                launcher.w as usize,
                launcher.h as usize,
                0x30363B,
            );
            self.draw_rect_outline(
                launcher.x as usize,
                launcher.y as usize,
                launcher.w as usize,
                launcher.h as usize,
                scene.theme.colors.accent,
                1,
            );
            self.draw_text(
                launcher.x as usize + 10,
                launcher.y as usize + (launcher.h as usize).saturating_sub(16) / 2,
                name,
                0xFFFFFF,
                12,
                clip,
            );
        }
        // Focused/active window titles as task buttons.
        let last_launcher = scene.settings_launcher_rect();
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

    /// Render a window and all its widgets.
    fn render_window(&self, scene: &Scene, window: &Window) {
        let theme = &scene.theme;

        // Window background
        self.fill_rect(
            window.bounds.x as usize,
            window.bounds.y as usize,
            window.bounds.w as usize,
            window.bounds.h as usize,
            window.style.bg_color,
        );

        // Window border
        if window.style.border_width > 0 {
            let bw = window.style.border_width as usize;
            let bc = window.style.border_color;
            let x = window.bounds.x as usize;
            let y = window.bounds.y as usize;
            let w = window.bounds.w as usize;
            let h = window.bounds.h as usize;

            // Top
            fb_gfx::fill_rect_px(
                x,
                y,
                w,
                bw,
                ((bc >> 16) & 0xFF) as u8,
                ((bc >> 8) & 0xFF) as u8,
                (bc & 0xFF) as u8,
            );
            // Bottom
            fb_gfx::fill_rect_px(
                x,
                y + h - bw,
                w,
                bw,
                ((bc >> 16) & 0xFF) as u8,
                ((bc >> 8) & 0xFF) as u8,
                (bc & 0xFF) as u8,
            );
            // Left
            fb_gfx::fill_rect_px(
                x,
                y,
                bw,
                h,
                ((bc >> 16) & 0xFF) as u8,
                ((bc >> 8) & 0xFF) as u8,
                (bc & 0xFF) as u8,
            );
            // Right
            fb_gfx::fill_rect_px(
                x + w - bw,
                y,
                bw,
                h,
                ((bc >> 16) & 0xFF) as u8,
                ((bc >> 8) & 0xFF) as u8,
                (bc & 0xFF) as u8,
            );
        }

        // Titlebar (contrasting strip so windows read as windows).
        let tb_h = scene.theme.metrics.titlebar_height as usize;
        let tb_color = if window.focused {
            scene.theme.colors.title_active
        } else {
            scene.theme.colors.title_inactive
        };

        self.fill_rect(
            window.bounds.x as usize,
            window.bounds.y as usize,
            window.bounds.w as usize,
            tb_h,
            tb_color,
        );

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

        // Minimize, maximize/restore, and close controls.
        self.fill_rect(
            minimize_rect.x as usize,
            minimize_rect.y as usize,
            minimize_rect.w as usize,
            minimize_rect.h as usize,
            0x394047,
        );
        self.fill_rect(
            minimize_rect.x as usize + 5,
            minimize_rect.y as usize + minimize_rect.h as usize / 2,
            minimize_rect.w as usize - 10,
            2,
            0xFFFFFF,
        );

        self.fill_rect(
            maximize_rect.x as usize,
            maximize_rect.y as usize,
            maximize_rect.w as usize,
            maximize_rect.h as usize,
            0x394047,
        );
        let mx = maximize_rect.x as usize + 6;
        let my = maximize_rect.y as usize + 6;
        let mw = maximize_rect.w as usize - 12;
        let mh = maximize_rect.h as usize - 12;
        self.draw_rect_outline(mx, my, mw, mh, 0xFFFFFF, 1);

        let cb_x = close_rect.x as usize;
        let cb_y = close_rect.y as usize;
        let cb_w = close_rect.w as usize;
        let cb_h = close_rect.h as usize;

        self.fill_rect(cb_x, cb_y, cb_w, cb_h, 0xC63434); // Red close button
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
                let bg = match widget.state {
                    WidgetState::Pressed => widget.style.bg_color, // Use press color
                    WidgetState::Hover => theme.colors.button_hover,
                    _ => widget.style.bg_color,
                };
                self.fill_rect(x, y, w, h, bg);
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
                    self.draw_rounded_rect(x, y, w, h, widget.style.radius as usize, bg);
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

    /// Render the mouse cursor.
    fn render_cursor(&self, x: i32, y: i32) {
        // `x,y` are the pointer hotspot coordinates from the guest input
        // device; the bitmap origin is offset from its declared tip hotspot.
        let cx = x - crate::desktop::cursor_data::CURSOR_HOTSPOT_X as i32;
        let cy = y - crate::desktop::cursor_data::CURSOR_HOTSPOT_Y as i32;
        crate::drivers::fb_gfx::blit_rgba_signed(
            cx,
            cy,
            crate::desktop::cursor_data::CURSOR_W,
            crate::desktop::cursor_data::CURSOR_H,
            &crate::desktop::cursor_data::CURSOR_RGBA,
        );
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
