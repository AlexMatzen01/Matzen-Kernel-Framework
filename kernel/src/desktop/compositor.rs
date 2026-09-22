//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Desktop compositor with double buffering and dirty-rect rendering.
//!
//! Renders the scene graph to the framebuffer using double buffering
//! to prevent screen tearing.

use crate::desktop::scene::{Scene, Rect, Widget, WidgetKind, WidgetState, Window, WindowId};
use crate::desktop::theme::{Theme, ThemeColors};
use crate::drivers::fb::{FrameBufferInfo, PixelFormat};
use crate::drivers::fb_gfx;
use crate::drivers::vga::Color;
use crate::serial_println;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
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
    /// Last frame time (for FPS calculation)
    last_frame_time: u64,
}

impl Compositor {
    /// Create a new compositor.
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        Self {
            screen_w,
            screen_h,
            fb_info: None,
            frame_count: 0,
            last_frame_time: 0,
        }
    }
    
    /// Set framebuffer info for bounds checking.
    pub fn set_fb_info(&mut self, info: FrameBufferInfo) {
        self.fb_info = Some(info);
    }
    
    /// Render the entire scene to the framebuffer.
    pub fn render(&mut self, scene: &mut Scene, cursor_x: i32, cursor_y: i32, cursor_visible: bool) {
        self.frame_count += 1;
        
        // Get dirty regions
        let dirty_regions = scene.take_dirty_regions();
        
        if dirty_regions.is_empty() {
            // Nothing to draw
            return;
        }
        
        // Clear back buffer for dirty regions only (or full if too many)
        if dirty_regions.len() > 10 || dirty_regions.iter().any(|r| r.w >= self.screen_w / 2 && r.h >= self.screen_h / 2) {
            // Full clear - too many dirty regions or large regions
            self.clear_back_buffer(0, 0, self.screen_w, self.screen_h, 
                ((scene.theme.colors.bg >> 16) & 0xFF) as u8,
                ((scene.theme.colors.bg >> 8) & 0xFF) as u8,
                (scene.theme.colors.bg & 0xFF) as u8);
        } else {
            // Clear only dirty regions
            for region in &dirty_regions {
                self.clear_back_buffer(region.x as u32, region.y as u32, region.w, region.h,
                    ((scene.theme.colors.bg >> 16) & 0xFF) as u8,
                    ((scene.theme.colors.bg >> 8) & 0xFF) as u8,
                    (scene.theme.colors.bg & 0xFF) as u8);
            }
        }
        
        // Render windows in z-order (back to front)
        for &win_id in scene.windows_z_order() {
            if let Some(window) = scene.windows.get(&win_id) {
                if window.visible {
                    self.render_window(scene, window);
                }
            }
        }
        
        // Render cursor on top
        if cursor_visible {
            self.render_cursor(cursor_x, cursor_y);
        }
        
        // Swap buffers (present to hardware)
        let dirty_regions_for_swap: Vec<(usize, usize, usize, usize)> = 
            scene.dirty_regions.iter().map(|r| (r.x as usize, r.y as usize, r.w as usize, r.h as usize)).collect();
        crate::drivers::fb_gfx::swap_buffers(&dirty_regions_for_swap);
        crate::drivers::fb_gfx::present_to_hardware();
    }
    
    /// Clear a region of the back buffer to a solid color.
    fn clear_back_buffer(&self, x: u32, y: u32, w: u32, h: u32, r: u8, g: u8, b: u8) {
        fb_gfx::fill_rect_px(x as usize, y as usize, w as usize, h as usize, r, g, b);
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
            fb_gfx::fill_rect_px(x, y, w, bw, 
                ((bc >> 16) & 0xFF) as u8, ((bc >> 8) & 0xFF) as u8, (bc & 0xFF) as u8);
            // Bottom
            fb_gfx::fill_rect_px(x, y + h - bw, w, bw,
                ((bc >> 16) & 0xFF) as u8, ((bc >> 8) & 0xFF) as u8, (bc & 0xFF) as u8);
            // Left
            fb_gfx::fill_rect_px(x, y, bw, h,
                ((bc >> 16) & 0xFF) as u8, ((bc >> 8) & 0xFF) as u8, (bc & 0xFF) as u8);
            // Right
            fb_gfx::fill_rect_px(x + w - bw, y, bw, h,
                ((bc >> 16) & 0xFF) as u8, ((bc >> 8) & 0xFF) as u8, (bc & 0xFF) as u8);
        }
        
        // Titlebar
        let tb_h = scene.theme.metrics.titlebar_height as usize;
        let tb_color = if window.focused {
            window.style.bg_color // Use active title color
        } else {
            window.style.bg_color // Use inactive title color
        };
        
        self.fill_rect(
            window.bounds.x as usize,
            window.bounds.y as usize,
            window.bounds.w as usize,
            tb_h,
            tb_color,
        );
        
        // Title text
        self.draw_text(
            window.bounds.x as usize + 10,
            window.bounds.y as usize + 4,
            &window.title,
            0xFFFFFF, // White text
            14,
        );
        
        // Close button
        let close_rect = window.close_button_rect(&scene.theme);
        let cb_x = close_rect.x as usize;
        let cb_y = close_rect.y as usize;
        let cb_w = close_rect.w as usize;
        let cb_h = close_rect.h as usize;
        
        self.fill_rect(cb_x, cb_y, cb_w, cb_h, 0xC63434); // Red close button
        self.draw_text(cb_x + cb_w / 2 - 3, cb_y + 2, "X", 0xFFFFFF, 12);
        
        // Render widgets recursively
        if let Some(root_widget) = scene.widgets.get(&window.root_widget) {
            self.render_widget_tree(scene, window.root_widget, window.bounds.x as i32, window.bounds.y as i32);
        }
    }
    
    /// Recursively render widget tree.
    fn render_widget_tree(&self, scene: &Scene, widget_id: crate::desktop::scene::WidgetId, parent_x: i32, parent_y: i32) {
        if let Some(widget) = scene.widgets.get(&widget_id) {
            if !widget.visible {
                return;
            }
            
            let abs_x = parent_x + widget.bounds.x;
            let abs_y = parent_y + widget.bounds.y;
            
            self.render_widget(scene, widget, abs_x, abs_y);
            
            // Render children
            for &child_id in &widget.children {
                self.render_widget_tree(scene, child_id, abs_x, abs_y);
            }
        }
    }
    
    /// Render a single widget.
    fn render_widget(&self, scene: &Scene, widget: &Widget, x: i32, y: i32) {
        let theme = &scene.theme;
        let x = x as usize;
        let y = y as usize;
        let w = widget.bounds.w as usize;
        let h = widget.bounds.h as usize;
        
        match widget.kind {
            WidgetKind::Panel => {
                self.fill_rect(x, y, w, h, widget.style.bg_color);
                if widget.style.border_width > 0 {
                    self.draw_rect_outline(x, y, w, h, widget.style.border_color, widget.style.border_width as usize);
                }
                if widget.style.radius > 0 {
                    // Draw rounded corners approximation
                    self.draw_rounded_rect(x, y, w, h, widget.style.radius as usize, widget.style.bg_color);
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
                    self.draw_rect_outline(x, y, w, h, widget.style.border_color, widget.style.border_width as usize);
                }
                if widget.style.radius > 0 {
                    self.draw_rounded_rect(x, y, w, h, widget.style.radius as usize, bg);
                }
                
                // Button text
                if !widget.text.is_empty() {
                    let text_w = widget.text.len() * (widget.style.font_size as usize * 6 / 10);
                    let text_x = x + (w.saturating_sub(text_w)) / 2;
                    let text_y = y + (h.saturating_sub(widget.style.font_size as usize)) / 2;
                    self.draw_text(text_x, text_y, &widget.text, widget.style.fg_color, widget.style.font_size);
                }
            }
            
            WidgetKind::Label => {
                if !widget.text.is_empty() {
                    self.draw_text(x + widget.style.padding_x as usize, 
                        y + widget.style.padding_y as usize, 
                        &widget.text, widget.style.fg_color, widget.style.font_size);
                }
            }
            
            WidgetKind::TextInput => {
                self.fill_rect(x, y, w, h, widget.style.bg_color);
                self.draw_rect_outline(x, y, w, h, widget.style.border_color, widget.style.border_width as usize);
                if widget.style.radius > 0 {
                    self.draw_rounded_rect(x, y, w, h, widget.style.radius as usize, widget.style.bg_color);
                }
                
                // Text content
                if !widget.text.is_empty() {
                    self.draw_text(x + widget.style.padding_x as usize, 
                        y + (h.saturating_sub(widget.style.font_size as usize)) / 2,
                        &widget.text, widget.style.fg_color, widget.style.font_size);
                }
                
                // Cursor
                if widget.focused {
                    let cursor_x = x + widget.style.padding_x as usize + widget.cursor_pos * (widget.style.font_size as usize * 6 / 10);
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
                    crate::drivers::fb_gfx::blit_rgba(x, y, widget.image_w as usize, widget.image_h as usize, data);
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
    fn draw_rect_outline(&self, x: usize, y: usize, w: usize, h: usize, color: u32, thickness: usize) {
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
    
    /// Draw text at position.
    fn draw_text(&self, x: usize, y: usize, text: &str, color: u32, font_size: u32) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        
        // Use the framebuffer text API for now
        // In a real implementation, we'd use fontdue to rasterize glyphs
        let row = y / 16;
        let col = x / 8;
        crate::drivers::fb_gfx::write_str_at_fb(row, col, text, 
            crate::drivers::vga::Color::White, crate::drivers::vga::Color::Black);
    }
    
    /// Render the mouse cursor.
    fn render_cursor(&self, x: i32, y: i32) {
        // Use the existing PNG cursor blitting
        let cx = x as usize;
        let cy = y as usize;
        // The cursor data is in cursor_data module
        crate::drivers::fb_gfx::blit_rgba(cx, cy, 32, 32, &crate::desktop::cursor_data::CURSOR_RGBA);
    }
}