//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Desktop theme system - pure Rust implementation.
//!
//! Provides theming for the desktop compositor without external dependencies.

use crate::drivers::vga::Color;

/// Theme color palette (24-bit RGB values).
#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub bg: u32,
    pub panel_bg: u32,
    pub panel_border: u32,
    pub accent: u32,
    pub title_active: u32,
    pub title_inactive: u32,
    pub text: u32,
    pub text_muted: u32,
    pub button_bg: u32,
    pub button_hover: u32,
    pub button_press: u32,
    pub button_text: u32,
    pub cursor: u32,
    pub scrollbar_bg: u32,
    pub scrollbar_thumb: u32,
    pub scrollbar_hover: u32,
}

impl ThemeColors {
    pub const fn default() -> Self {
        Self {
            bg: 0x102a4e,
            panel_bg: 0x1c4476,
            panel_border: 0xebebeb,
            accent: 0x00be5a,
            title_active: 0x005cac,
            title_inactive: 0x696969,
            text: 0xffffff,
            text_muted: 0xaaaaaa,
            button_bg: 0x005cac,
            button_hover: 0x0078d4,
            button_press: 0x003d7a,
            button_text: 0xffffff,
            cursor: 0xffffffff,
            scrollbar_bg: 0x1c4476,
            scrollbar_thumb: 0x696969,
            scrollbar_hover: 0xaaaaaa,
        }
    }
}

/// Font specifications.
#[derive(Debug, Clone, Copy)]
pub struct FontSpec {
    pub name: &'static str,
    pub size: u32,
}

/// Theme font configuration.
#[derive(Debug, Clone, Copy)]
pub struct ThemeFonts {
    pub ui: FontSpec,
    pub mono: FontSpec,
    pub title: FontSpec,
}

impl ThemeFonts {
    pub const fn default() -> Self {
        Self {
            ui: FontSpec { name: "ui", size: 14 },
            mono: FontSpec { name: "mono", size: 12 },
            title: FontSpec { name: "title", size: 16 },
        }
    }
}

/// Theme metrics (pixel values).
#[derive(Debug, Clone, Copy)]
pub struct ThemeMetrics {
    pub window_border: u32,
    pub titlebar_height: u32,
    pub button_padding_x: u32,
    pub button_padding_y: u32,
    pub panel_radius: u32,
    pub taskbar_height: u32,
    pub icon_size: u32,
}

impl ThemeMetrics {
    pub const fn default() -> Self {
        Self {
            window_border: 2,
            titlebar_height: 28,
            button_padding_x: 16,
            button_padding_y: 8,
            panel_radius: 4,
            taskbar_height: 36,
            icon_size: 48,
        }
    }
}

/// Theme layout parameters.
#[derive(Debug, Clone, Copy)]
pub struct ThemeLayout {
    pub icon_spacing: u32,
    pub icon_margin: u32,
    pub window_gap: u32,
}

impl ThemeLayout {
    pub const fn default() -> Self {
        Self {
            icon_spacing: 16,
            icon_margin: 16,
            window_gap: 8,
        }
    }
}

/// Complete theme configuration.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub colors: ThemeColors,
    pub fonts: ThemeFonts,
    pub metrics: ThemeMetrics,
    pub layout: ThemeLayout,
}

impl Theme {
    pub const fn default() -> Self {
        Self {
            colors: ThemeColors::default(),
            fonts: ThemeFonts::default(),
            metrics: ThemeMetrics::default(),
            layout: ThemeLayout::default(),
        }
    }
    
    /// Convert a u32 color (0xRRGGBB) to VGA Color enum approximation.
    pub fn to_vga_color(color: u32) -> Color {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        
        // Simple nearest-match to VGA 16 colors
        match (r, g, b) {
            (0..=85, 0..=85, 0..=85) => Color::Black,
            (0..=85, 0..=85, 170..=255) => Color::Blue,
            (0..=85, 170..=255, 0..=85) => Color::Green,
            (0..=85, 170..=255, 170..=255) => Color::Cyan,
            (170..=255, 0..=85, 0..=85) => Color::Red,
            (170..=255, 0..=85, 170..=255) => Color::Magenta,
            (170..=255, 85..=170, 0..=85) => Color::Brown,
            (170..=255, 170..=255, 170..=255) => Color::LightGray,
            (85..=170, 85..=170, 85..=170) => Color::DarkGray,
            (85..=170, 85..=170, 255) => Color::LightBlue,
            (85..=170, 255, 85..=170) => Color::LightGreen,
            (85..=170, 255, 255) => Color::LightCyan,
            (255, 85..=170, 85..=170) => Color::LightRed,
            (255, 85..=170, 255) => Color::Pink,
            (255, 255, 85..=170) => Color::Yellow,
            (255, 255, 255) => Color::White,
            _ => Color::White,
        }
    }
    
    /// Alpha blend two colors (0-255 alpha).
    pub fn alpha_blend(fg: u32, bg: u32, alpha: u8) -> u32 {
        let fg_r = ((fg >> 16) & 0xFF) as u16;
        let fg_g = ((fg >> 8) & 0xFF) as u16;
        let fg_b = (fg & 0xFF) as u16;
        let bg_r = ((bg >> 16) & 0xFF) as u16;
        let bg_g = ((bg >> 8) & 0xFF) as u16;
        let bg_b = (bg & 0xFF) as u16;
        let a = alpha as u16;
        let inv_a = 255 - a;
        
        let r = (fg_r * a + bg_r * inv_a) / 255;
        let g = (fg_g * a + bg_g * inv_a) / 255;
        let b = (fg_b * a + bg_b * inv_a) / 255;
        
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }
}

/// Global theme instance (initialized at boot).
static mut CURRENT_THEME: Option<Theme> = None;

/// Initialize the global theme.
pub fn init_theme() {
    unsafe {
        CURRENT_THEME = Some(Theme::default());
    }
}

/// Get the current theme (panics if not initialized).
pub fn current_theme() -> &'static Theme {
    unsafe {
        CURRENT_THEME.as_ref().expect("Theme not initialized")
    }
}

/// Get a mutable reference to the current theme (for hot-reload).
pub fn current_theme_mut() -> &'static mut Theme {
    unsafe {
        CURRENT_THEME.as_mut().expect("Theme not initialized")
    }
}