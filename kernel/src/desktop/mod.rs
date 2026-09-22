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

mod cursor_data;
mod theme;
mod scene;
mod compositor;

use alloc::string::String;
use alloc::vec::Vec;
use cursor_data::{CURSOR_H, CURSOR_HOTSPOT_X, CURSOR_HOTSPOT_Y, CURSOR_RGBA, CURSOR_W};

use crate::drivers::keyboard::Key;
use crate::drivers::{keyboard, mouse, vga};
use crate::print;
use crate::println;

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
    vga::hide_cursor();
    crate::drivers::mouse::set_bounds(sw.saturating_sub(1), sh.saturating_sub(1));
    crate::drivers::mouse::set_position(sw / 2, sh / 2);
    // Drain stale motion so the cursor does not jump on entry.
    while crate::drivers::mouse::read_event().is_some() {}

// Initialize scene graph
        let mut scene = crate::desktop::scene::Scene::new(sw as u32, sh as u32);
    
    // Create initial windows
    let shell_win_id = scene.create_window(
        alloc::string::String::from("Shell"),
        scene::Rect::new(100, 100, 600, 400),
    );
    let about_win_id = scene.create_window(
        alloc::string::String::from("About MFK"),
        scene::Rect::new(200, 200, 400, 300),
    );
    
    // Add widgets to shell window
    if let Some(shell_win) = scene.windows.get_mut(&shell_win_id) {
        let root_id = shell_win.root_widget;
        
        // Add a panel
        let panel_id = crate::desktop::scene::WidgetId::new();
        let panel_bounds = scene::Rect::new(10, 40, 580, 340);
        let mut panel = crate::desktop::scene::Widget::panel(panel_bounds, theme);
        panel.id = panel_id;
        scene.widgets.insert(panel_id, panel);
        scene.widgets.get_mut(&root_id).unwrap().children.push(panel_id);
        
        // Add a button
        let btn_id = crate::desktop::scene::WidgetId::new();
        let btn_bounds = scene::Rect::new(20, 50, 120, 40);
        let mut btn = crate::desktop::scene::Widget::button(btn_bounds, alloc::string::String::from("Click Me"), theme);
        btn.id = btn_id;
        btn.on_click = Some(alloc::boxed::Box::new(|scene: &mut crate::desktop::scene::Scene| {
            crate::serial_println!("[desktop] Button clicked!");
        }));
        scene.widgets.insert(btn_id, btn);
        scene.widgets.get_mut(&panel_id).unwrap().children.push(btn_id);
        
        // Add a label
        let label_id = crate::desktop::scene::WidgetId::new();
        let label_bounds = scene::Rect::new(20, 100, 300, 30);
        let mut label = crate::desktop::scene::Widget::label(label_bounds, alloc::string::String::from("Welcome to MFK Desktop!"), theme);
        label.id = label_id;
        scene.widgets.insert(label_id, label);
        scene.widgets.get_mut(&panel_id).unwrap().children.push(label_id);
        
        // Add a text input
        let input_id = crate::desktop::scene::WidgetId::new();
        let input_bounds = scene::Rect::new(20, 150, 300, 32);
        let mut input = crate::desktop::scene::Widget::text_input(input_bounds, theme);
        input.id = input_id;
        input.text = alloc::string::String::from("Type here...");
        scene.widgets.insert(input_id, input);
        scene.widgets.get_mut(&panel_id).unwrap().children.push(input_id);
        
        shell_win.root_widget = root_id;
    }
    
    // Add widgets to about window
    if let Some(about_win) = scene.windows.get_mut(&about_win_id) {
        let root_id = about_win.root_widget;
        
        let label_id = crate::desktop::scene::WidgetId::new();
        let label_bounds = scene::Rect::new(20, 40, 360, 200);
        let mut label = crate::desktop::scene::Widget::label(label_bounds, 
            alloc::string::String::from("Matzen Kernel Framework v0.1.0\n\nPixel desktop with:\n- Retained-mode scene graph\n- Double-buffered compositor\n- Dirty-rect rendering\n- Widget toolkit (Panel, Button, Label, TextInput)\n- Theme system\n\nClick and drag windows by titlebar.\nPress Esc to exit."), 
            theme);
        label.id = label_id;
        scene.widgets.insert(label_id, label);
        scene.widgets.get_mut(&root_id).unwrap().children.push(label_id);
        
        about_win.root_widget = root_id;
    }
    
// Initialize compositor
        let mut compositor = crate::desktop::compositor::Compositor::new(sw as u32, sh as u32);
    
    // Get framebuffer info for compositor
    if let Some(fb_info) = crate::drivers::fb::pixel_size().and_then(|_| {
        crate::drivers::fb::with_lock(|st| st.info)
    }) {
        compositor.set_fb_info(fb_info);
    }
    
    crate::drivers::mouse::set_bounds(sw.saturating_sub(1), sh.saturating_sub(1));
    crate::drivers::mouse::set_position(sw / 2, sh / 2);
    while crate::drivers::mouse::read_event().is_some() {}

    // Focus shell window initially
    scene.focus_window(shell_win_id);
    
    crate::serial_println!("[desktop] Scene initialized with {} windows", scene.windows.len());

    'outer: loop {
        crate::net::process_packets();
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
        
        let prev_left = unsafe { PREV_LEFT };
        if left && !prev_left {
            // Mouse down
            if let Some(win_id) = scene.window_at(mx, my) {
                scene.focus_window(win_id);
                
                // Check if clicking titlebar
                if let Some(win) = scene.windows.get(&win_id) {
                    let tb = win.titlebar_rect(&scene.theme);
                    if tb.contains_point(mx, my) {
                        // Check close button
                        let close_btn = win.close_button_rect(&scene.theme);
                        if close_btn.contains_point(mx, my) {
                            scene.destroy_window(win_id);
                        } else {
                            // Start drag
                            unsafe { DRAG_WINDOW = Some(win_id); }
                            unsafe { DRAG_OFFSET = (mx - win.bounds.x, my - win.bounds.y); }
                        }
                    }
                }
            }
        } else if !left && prev_left {
            // Mouse up
            unsafe { DRAG_WINDOW = None; }
        }
        
        if left && unsafe { DRAG_WINDOW }.is_some() {
            // Dragging
            if let Some(win_id) = unsafe { DRAG_WINDOW } {
                if let Some(win) = scene.windows.get_mut(&win_id) {
                    let offset = unsafe { DRAG_OFFSET };
                    let new_x = mx - offset.0;
                    let new_y = my - offset.1;
                    win.bounds.x = new_x.clamp(0, sw as i32 - win.bounds.w as i32);
                    win.bounds.y = new_y.clamp(0, sh as i32 - win.bounds.h as i32);
                    win.dirty = true;
                }
            }
        }
        unsafe { PREV_LEFT = left; }
        
        // Keyboard
        for _ in 0..8 {
            let Some(ev) = crate::drivers::keyboard::read_key() else {
                break;
            };
            match ev.key {
                crate::drivers::keyboard::Key::Esc => {
                    break 'outer; // Exit desktop loop
                }
                _ => {}
            }
        }
        
        // Render frame
        compositor.render(&mut scene, mx, my, true);
        
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