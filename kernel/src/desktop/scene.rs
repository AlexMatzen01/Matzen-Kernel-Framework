//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Scene graph for the desktop compositor.
//!
//! Retained-mode scene graph with dirty-rect tracking for efficient rendering.

use crate::desktop::theme::Theme;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::RefCell;

/// Rectangle in pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w as i32
            && self.x + self.w as i32 > other.x
            && self.y < other.y + other.h as i32
            && self.y + self.h as i32 > other.y
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.w as i32).max(other.x + other.w as i32);
        let y2 = (self.y + self.h as i32).max(other.y + other.h as i32);
        Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32)
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w as i32 && y >= self.y && y < self.y + self.h as i32
    }
}

/// Widget state for interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetState {
    Normal,
    Hover,
    Pressed,
    Focused,
    Disabled,
}

/// Widget kind enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    Panel,
    Button,
    Label,
    TextInput,
    Image,
    Custom,
}

/// Widget identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WidgetId(pub u64);

static NEXT_WIDGET_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

impl WidgetId {
    pub fn new() -> Self {
        Self(NEXT_WIDGET_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed))
    }
}

/// Window identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

static NEXT_WINDOW_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

impl WindowId {
    pub fn new() -> Self {
        Self(NEXT_WINDOW_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed))
    }
}

/// Style definition for widgets.
#[derive(Debug, Clone)]
pub struct Style {
    pub bg_color: u32,
    pub fg_color: u32,
    pub border_color: u32,
    pub border_width: u32,
    pub radius: u32,
    pub padding_x: u32,
    pub padding_y: u32,
    pub font_size: u32,
}

impl Style {
    pub fn default_panel(theme: &crate::desktop::theme::Theme) -> Self {
        Self {
            bg_color: theme.colors.panel_bg,
            fg_color: theme.colors.text,
            border_color: theme.colors.panel_border,
            border_width: 1,
            radius: theme.metrics.panel_radius,
            padding_x: 8,
            padding_y: 4,
            font_size: theme.fonts.ui.size,
        }
    }

    pub fn default_button(theme: &crate::desktop::theme::Theme) -> Self {
        Self {
            bg_color: theme.colors.button_bg,
            fg_color: theme.colors.button_text,
            border_color: theme.colors.panel_border,
            border_width: 1,
            radius: theme.metrics.panel_radius,
            padding_x: theme.metrics.button_padding_x,
            padding_y: theme.metrics.button_padding_y,
            font_size: theme.fonts.ui.size,
        }
    }

    pub fn default_label(theme: &crate::desktop::theme::Theme) -> Self {
        Self {
            bg_color: 0,
            fg_color: theme.colors.text,
            border_color: 0,
            border_width: 0,
            radius: 0,
            padding_x: 4,
            padding_y: 2,
            font_size: theme.fonts.ui.size,
        }
    }

    pub fn default_text_input(theme: &crate::desktop::theme::Theme) -> Self {
        Self {
            bg_color: theme.colors.panel_bg,
            fg_color: theme.colors.text,
            border_color: theme.colors.panel_border,
            border_width: 1,
            radius: theme.metrics.panel_radius,
            padding_x: 8,
            padding_y: 4,
            font_size: theme.fonts.ui.size,
        }
    }
}

/// Widget data structure.
pub struct Widget {
    pub id: WidgetId,
    pub kind: WidgetKind,
    pub bounds: Rect,
    pub style: Style,
    pub state: WidgetState,
    pub visible: bool,
    pub parent: Option<WidgetId>,
    pub children: Vec<WidgetId>,
    // Kind-specific data
    pub text: alloc::string::String,
    pub on_click: Option<alloc::boxed::Box<dyn Fn(&mut Scene) + Send + Sync>>,
    pub on_change:
        Option<alloc::boxed::Box<dyn Fn(&mut Scene, &alloc::string::String) + Send + Sync>>,
    // Text input specific
    pub cursor_pos: usize,
    pub focused: bool,
    // Image specific
    pub image_data: Option<alloc::vec::Vec<u8>>,
    pub image_w: u32,
    pub image_h: u32,
}

impl Widget {
    pub fn new(kind: WidgetKind, bounds: Rect, style: Style) -> Self {
        Self {
            id: WidgetId::new(),
            kind,
            bounds,
            style,
            state: WidgetState::Normal,
            visible: true,
            parent: None,
            children: Vec::new(),
            text: alloc::string::String::new(),
            on_click: None,
            on_change: None,
            cursor_pos: 0,
            focused: false,
            image_data: None,
            image_w: 0,
            image_h: 0,
        }
    }

    pub fn panel(bounds: Rect, theme: &crate::desktop::theme::Theme) -> Self {
        Self::new(WidgetKind::Panel, bounds, Style::default_panel(theme))
    }

    pub fn button(
        bounds: Rect,
        text: alloc::string::String,
        theme: &crate::desktop::theme::Theme,
    ) -> Self {
        let mut w = Self::new(WidgetKind::Button, bounds, Style::default_button(theme));
        w.text = text;
        w
    }

    pub fn label(
        bounds: Rect,
        text: alloc::string::String,
        theme: &crate::desktop::theme::Theme,
    ) -> Self {
        let mut w = Self::new(WidgetKind::Label, bounds, Style::default_label(theme));
        w.text = text;
        w
    }

    pub fn text_input(bounds: Rect, theme: &crate::desktop::theme::Theme) -> Self {
        Self::new(
            WidgetKind::TextInput,
            bounds,
            Style::default_text_input(theme),
        )
    }

    pub fn get_absolute_bounds(&self, scene: &Scene) -> Rect {
        let mut bounds = self.bounds;
        let mut current = self.parent;
        while let Some(parent_id) = current {
            if let Some(parent) = scene.widgets.get(&parent_id) {
                bounds.x += parent.bounds.x;
                bounds.y += parent.bounds.y;
                current = parent.parent;
            } else {
                break;
            }
        }
        bounds
    }

    pub fn contains_point(&self, scene: &Scene, x: i32, y: i32) -> bool {
        let abs = self.get_absolute_bounds(scene);
        abs.contains_point(x, y)
    }

    pub fn mark_dirty(&self, scene: &mut Scene) {
        let abs = self.get_absolute_bounds(scene);
        scene.mark_dirty(abs);
    }
}

/// Window data structure.
pub struct Window {
    pub id: WindowId,
    pub title: alloc::string::String,
    pub bounds: Rect,
    pub style: Style,
    pub root_widget: WidgetId,
    pub focused: bool,
    pub visible: bool,
    pub minimized: bool,
    pub z_order: i32,
    pub dirty: bool,
    pub restore_bounds: Option<Rect>,
}

impl Window {
    pub fn new(
        title: alloc::string::String,
        bounds: Rect,
        theme: &crate::desktop::theme::Theme,
    ) -> Self {
        let root_id = WidgetId::new();
        let mut root = Widget::panel(bounds, theme);
        root.id = root_id;

        Self {
            id: WindowId::new(),
            title,
            bounds,
            style: Style::default_panel(theme),
            root_widget: root_id,
            focused: false,
            visible: true,
            minimized: false,
            z_order: 0,
            dirty: true,
            restore_bounds: None,
        }
    }

    pub fn get_absolute_bounds(&self) -> Rect {
        self.bounds
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        self.bounds.contains_point(x, y)
    }

    pub fn titlebar_rect(&self, theme: &crate::desktop::theme::Theme) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.w,
            theme.metrics.titlebar_height,
        )
    }

    pub fn close_button_rect(&self, theme: &crate::desktop::theme::Theme) -> Rect {
        let tb = self.titlebar_rect(theme);
        let btn_size = theme.metrics.titlebar_height - 4;
        Rect::new(
            tb.x + tb.w as i32 - btn_size as i32 - 2,
            tb.y + 2,
            btn_size,
            btn_size,
        )
    }

    pub fn maximize_button_rect(&self, theme: &crate::desktop::theme::Theme) -> Rect {
        let close = self.close_button_rect(theme);
        let gap = 2;
        Rect::new(close.x - close.w as i32 - gap, close.y, close.w, close.h)
    }

    pub fn minimize_button_rect(&self, theme: &crate::desktop::theme::Theme) -> Rect {
        let maximize = self.maximize_button_rect(theme);
        let gap = 2;
        Rect::new(
            maximize.x - maximize.w as i32 - gap,
            maximize.y,
            maximize.w,
            maximize.h,
        )
    }
}

/// Scene graph containing all windows and widgets.
pub struct Scene {
    pub windows: BTreeMap<WindowId, Window>,
    pub widgets: BTreeMap<WidgetId, Widget>,
    pub window_z_order: Vec<WindowId>, // Back to front
    pub focused_window: Option<WindowId>,
    pub dirty_regions: Vec<Rect>,
    pub screen_bounds: Rect,
    pub theme: Theme,
    pub next_z_order: i32,
}

impl Scene {
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        let theme = crate::desktop::theme::Theme::default();
        Self {
            windows: BTreeMap::new(),
            widgets: BTreeMap::new(),
            window_z_order: Vec::new(),
            focused_window: None,
            dirty_regions: Vec::new(),
            screen_bounds: Rect::new(0, 0, screen_w, screen_h),
            theme,
            next_z_order: 0,
        }
    }

    pub fn create_window(&mut self, title: alloc::string::String, bounds: Rect) -> WindowId {
        let window = Window::new(title, bounds, &self.theme);
        let id = window.id;
        // Window::new mints a root widget id but cannot register it (no
        // Scene access). Register it here so `widgets.get(&root_id)` is
        // always Some; missing roots panicked desktop/mod.rs unwraps.
        // The root panel lives *below* the titlebar (window-relative coords)
        // so it can never paint over the title strip or close button.
        let root_id = window.root_widget;
        let tb = self.theme.metrics.titlebar_height as i32;
        let mut root = Widget::panel(
            Rect::new(0, tb, bounds.w, bounds.h.saturating_sub(tb as u32)),
            &self.theme,
        );
        root.id = root_id;
        self.widgets.insert(root_id, root);
        self.windows.insert(id, window);
        self.window_z_order.push(id);
        self.next_z_order += 1;
        self.mark_dirty_full();
        id
    }

    pub fn destroy_window(&mut self, id: WindowId) {
        if let Some(window) = self.windows.remove(&id) {
            let bounds = window.bounds;
            // Remove all widgets in this window
            self.collect_widget_ids(window.root_widget)
                .into_iter()
                .for_each(|id| {
                    self.widgets.remove(&id);
                });
            self.window_z_order.retain(|&wid| wid != id);
            if self.focused_window == Some(id) {
                self.focused_window = None;
                let next = self
                    .window_z_order
                    .iter()
                    .rev()
                    .copied()
                    .find(|wid| self.windows.get(wid).map(|w| w.visible).unwrap_or(false));
                if let Some(next) = next {
                    self.focus_window(next);
                }
            }
            // Only the vacated area plus the taskbar (title list) change.
            self.mark_dirty(bounds);
            self.mark_dirty(self.taskbar_rect());
        }
    }

    fn collect_widget_ids(&self, root_id: WidgetId) -> Vec<WidgetId> {
        let mut ids = Vec::new();
        let mut stack = Vec::new();
        stack.push(root_id);
        while let Some(id) = stack.pop() {
            ids.push(id);
            if let Some(widget) = self.widgets.get(&id) {
                for &child_id in &widget.children {
                    stack.push(child_id);
                }
            }
        }
        ids
    }

    pub fn add_widget(&mut self, parent_id: WidgetId, mut widget: Widget) -> WidgetId {
        let id = widget.id;
        widget.parent = Some(parent_id);
        self.widgets.insert(id, widget);
        if let Some(parent) = self.widgets.get_mut(&parent_id) {
            parent.children.push(id);
        }
        id
    }

    pub fn remove_widget(&mut self, id: WidgetId) {
        if let Some(widget) = self.widgets.remove(&id) {
            if let Some(parent_id) = widget.parent {
                if let Some(parent) = self.widgets.get_mut(&parent_id) {
                    parent.children.retain(|&cid| cid != id);
                }
            }
            // Recursively remove children
            for child_id in widget.children {
                self.remove_widget(child_id);
            }
        }
    }

    pub fn get_widget(&self, id: WidgetId) -> Option<&Widget> {
        self.widgets.get(&id)
    }

    pub fn get_widget_mut(&mut self, id: WidgetId) -> Option<&mut Widget> {
        self.widgets.get_mut(&id)
    }

    pub fn focus_window(&mut self, id: WindowId) {
        if !self.windows.get(&id).map(|w| w.visible).unwrap_or(false) {
            return;
        }
        if self.focused_window == Some(id) {
            return;
        }

        // Move to front of z-order
        self.window_z_order.retain(|&wid| wid != id);
        self.window_z_order.push(id);

        if let Some(old_id) = self.focused_window {
            if let Some(old_win) = self.windows.get_mut(&old_id) {
                old_win.focused = false;
                old_win.dirty = true;
            }
        }

        if let Some(new_win) = self.windows.get_mut(&id) {
            new_win.focused = true;
            new_win.dirty = true;
        }

        // Only titlebars (active/inactive colors flip) plus the taskbar
        // (focus highlight) change — never the whole screen. The previously
        // focused window was frontmost before `id` was pushed above.
        let old_id = self.focused_window;
        self.focused_window = Some(id);
        let mut ids = Vec::new();
        ids.push(id);
        if let Some(old) = old_id {
            if old != id {
                ids.push(old);
            }
        }
        for wid in ids {
            if let Some(win) = self.windows.get(&wid) {
                let tb = win.titlebar_rect(&self.theme);
                self.mark_dirty(tb);
            }
        }
        self.mark_dirty(self.taskbar_rect());
    }

    pub fn minimize_window(&mut self, id: WindowId) {
        let old_bounds = match self.windows.get_mut(&id) {
            Some(win) if win.visible => {
                win.visible = false;
                win.minimized = true;
                win.focused = false;
                win.bounds
            }
            _ => return,
        };
        if self.focused_window == Some(id) {
            self.focused_window = None;
            let next = self
                .window_z_order
                .iter()
                .rev()
                .copied()
                .find(|wid| self.windows.get(wid).map(|w| w.visible).unwrap_or(false));
            if let Some(next) = next {
                self.focus_window(next);
            }
        }
        self.mark_dirty(old_bounds);
        self.mark_dirty(self.taskbar_rect());
    }

    pub fn restore_window(&mut self, id: WindowId) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.visible = true;
            win.minimized = false;
        } else {
            return;
        }
        self.focus_window(id);
        if let Some(win) = self.windows.get(&id) {
            self.mark_dirty(win.bounds);
        }
        self.mark_dirty(self.taskbar_rect());
    }

    pub fn toggle_maximize(&mut self, id: WindowId) {
        let old_bounds = match self.windows.get(&id) {
            Some(win) if win.visible => win.bounds,
            _ => return,
        };
        let new_bounds = {
            let win = self.windows.get_mut(&id).unwrap();
            if let Some(restore) = win.restore_bounds.take() {
                restore
            } else {
                win.restore_bounds = Some(win.bounds);
                Rect::new(
                    0,
                    0,
                    self.screen_bounds.w,
                    self.screen_bounds
                        .h
                        .saturating_sub(self.theme.metrics.taskbar_height),
                )
            }
        };
        self.set_window_bounds(id, new_bounds);
        self.mark_dirty(old_bounds);
        self.mark_dirty(new_bounds);
        self.mark_dirty(self.taskbar_rect());
    }

    pub fn set_window_bounds(&mut self, id: WindowId, bounds: Rect) {
        let root_id = match self.windows.get_mut(&id) {
            Some(win) => {
                win.bounds = bounds;
                win.dirty = true;
                win.root_widget
            }
            None => return,
        };
        if let Some(root) = self.widgets.get_mut(&root_id) {
            root.bounds.w = bounds.w;
            root.bounds.h = bounds.h.saturating_sub(self.theme.metrics.titlebar_height);
        }
    }

    /// Screen strip occupied by the taskbar (repaints on focus change and
    /// window open/close because it lists window titles).
    fn taskbar_rect(&self) -> Rect {
        let tb_h = self.theme.metrics.taskbar_height as i32;
        Rect::new(
            0,
            (self.screen_bounds.h as i32).saturating_sub(tb_h),
            self.screen_bounds.w,
            tb_h.max(0) as u32,
        )
    }

    pub fn shell_launcher_rect(&self) -> Rect {
        let tb_h = self.theme.metrics.taskbar_height;
        Rect::new(
            68,
            (self.screen_bounds.h.saturating_sub(tb_h)) as i32 + 5,
            60,
            tb_h.saturating_sub(10),
        )
    }

    pub fn files_launcher_rect(&self) -> Rect {
        let tb_h = self.theme.metrics.taskbar_height;
        let s = self.shell_launcher_rect();
        Rect::new(
            s.x + s.w as i32 + 6,
            (self.screen_bounds.h.saturating_sub(tb_h)) as i32 + 5,
            60,
            tb_h.saturating_sub(10),
        )
    }

    pub fn drive_launcher_rect(&self) -> Rect {
        let tb_h = self.theme.metrics.taskbar_height;
        let f = self.files_launcher_rect();
        Rect::new(
            f.x + f.w as i32 + 6,
            (self.screen_bounds.h.saturating_sub(tb_h)) as i32 + 5,
            60,
            tb_h.saturating_sub(10),
        )
    }

    pub fn settings_launcher_rect(&self) -> Rect {
        let tb_h = self.theme.metrics.taskbar_height;
        let d = self.drive_launcher_rect();
        Rect::new(
            d.x + d.w as i32 + 6,
            (self.screen_bounds.h.saturating_sub(tb_h)) as i32 + 5,
            70,
            tb_h.saturating_sub(10),
        )
    }

    pub fn bring_to_front(&mut self, id: WindowId) {
        self.window_z_order.retain(|&wid| wid != id);
        self.window_z_order.push(id);
        if let Some(win) = self.windows.get_mut(&id) {
            win.dirty = true;
        }
        self.mark_dirty_full();
    }

    pub fn mark_dirty(&mut self, rect: Rect) {
        // Clip to screen bounds
        let clipped = Rect::new(
            rect.x.max(self.screen_bounds.x),
            rect.y.max(self.screen_bounds.y),
            rect.w
                .min(self.screen_bounds.w.saturating_sub(rect.x as u32)),
            rect.h
                .min(self.screen_bounds.h.saturating_sub(rect.y as u32)),
        );
        if clipped.w > 0 && clipped.h > 0 {
            self.dirty_regions.push(clipped);
        }
    }

    pub fn mark_dirty_full(&mut self) {
        self.dirty_regions.push(self.screen_bounds);
    }

    /// Merge overlapping dirty regions to reduce redraw count.
    pub fn coalesce_dirty_regions(&mut self) {
        if self.dirty_regions.len() <= 1 {
            return;
        }

        // Simple O(n^2) merge for small region counts
        let mut merged = Vec::new();
        let mut regions = core::mem::take(&mut self.dirty_regions);

        while let Some(r) = regions.pop() {
            let mut merged_r = r;
            let mut i = 0;
            while i < regions.len() {
                if merged_r.intersects(&regions[i]) {
                    merged_r = merged_r.union(&regions[i]);
                    regions.swap_remove(i);
                } else {
                    i += 1;
                }
            }
            merged.push(merged_r);
        }

        self.dirty_regions = merged;
    }

    /// Find window at screen position (top-most first).
    pub fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        for &id in self.window_z_order.iter().rev() {
            if let Some(win) = self.windows.get(&id) {
                if win.visible && win.contains_point(x, y) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Find widget at screen position within a window (top-most first).
    pub fn widget_at(&self, window_id: WindowId, x: i32, y: i32) -> Option<WidgetId> {
        if let Some(window) = self.windows.get(&window_id) {
            let mut candidates = Vec::new();
            self.collect_widgets_at(window.root_widget, x, y, &mut candidates);
            candidates.into_iter().next()
        } else {
            None
        }
    }

    fn collect_widgets_at(&self, root_id: WidgetId, x: i32, y: i32, out: &mut Vec<WidgetId>) {
        let mut stack = Vec::new();
        stack.push(root_id);
        while let Some(id) = stack.pop() {
            if let Some(widget) = self.widgets.get(&id) {
                if widget.visible && widget.contains_point(self, x, y) {
                    out.push(id);
                }
                // Add children in reverse order for top-most first
                for &child_id in widget.children.iter().rev() {
                    stack.push(child_id);
                }
            }
        }
    }

    /// Get all dirty regions and clear the list.
    pub fn take_dirty_regions(&mut self) -> Vec<Rect> {
        self.coalesce_dirty_regions();
        core::mem::take(&mut self.dirty_regions)
    }

    /// Get windows in z-order (back to front).
    pub fn windows_z_order(&self) -> &[WindowId] {
        &self.window_z_order
    }
}
