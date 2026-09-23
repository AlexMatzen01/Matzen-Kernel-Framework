//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Drivers module
//!
//! Contains hardware drivers for the kernel.

pub mod acpi;
pub mod ata;
pub mod block;
pub mod e1000;
pub mod fb;
pub mod fb_gfx;
pub mod keyboard;
pub mod mouse;
pub mod pci;
pub mod pit;
pub mod pm_timer;
pub mod rtc;
pub mod serial;
pub mod time_source;
pub mod tsc;
pub mod vga;

#[cfg(feature = "usb")]
pub mod usb;

#[cfg(feature = "usb")]
pub mod xhci;
