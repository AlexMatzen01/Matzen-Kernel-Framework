//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Networking debug output gate.
//!
//! The network stack and E1000 driver are chatty on the serial console
//! (per-packet RX/TX, ARP, TCP state). That output is gated behind this
//! flag and only shown when the invoked shell command carries `-d` or
//! `--debug`. Use `crate::net_log!` (same syntax as
//! `crate::serial_println!`) for anything per-packet or per-poll.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

static NET_DEBUG: AtomicBool = AtomicBool::new(false);

/// Enable or disable networking debug output.
pub fn set_debug(on: bool) {
    NET_DEBUG.store(on, Ordering::Relaxed);
}

/// Whether networking debug output is currently enabled.
pub fn debug_enabled() -> bool {
    NET_DEBUG.load(Ordering::Relaxed)
}

/// RAII guard: enables debug for a command invocation and restores the
/// previous state on drop (including early returns / Ctrl+C paths).
pub struct DebugGuard {
    prev: bool,
}

impl DebugGuard {
    /// Strip `-d` / `--debug` tokens from `args`, enable debug if any was
    /// present, and return the guard plus the cleaned argument string.
    pub fn acquire(args: &str) -> (Self, String) {
        let mut debug = false;
        let mut cleaned = String::new();
        for tok in args.split_whitespace() {
            if tok == "-d" || tok == "--debug" {
                debug = true;
            } else {
                if !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                cleaned.push_str(tok);
            }
        }
        let prev = debug_enabled();
        set_debug(debug);
        (DebugGuard { prev }, cleaned)
    }
}

impl Drop for DebugGuard {
    fn drop(&mut self) {
        set_debug(self.prev);
    }
}

/// Gated serial logging for networking code. Syntax matches
/// `crate::serial_println!`; output appears only with `-d`/`--debug`.
#[macro_export]
macro_rules! net_log {
    ($($arg:tt)*) => {{
        if $crate::net::debug::debug_enabled() {
            $crate::serial_println!($($arg)*);
        }
    }};
}

/// Gated serial logging without a trailing newline (matches
/// `crate::serial_print!`).
#[macro_export]
macro_rules! net_print {
    ($($arg:tt)*) => {{
        if $crate::net::debug::debug_enabled() {
            $crate::serial_print!($($arg)*);
        }
    }};
}
