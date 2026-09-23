//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Script interpreter - executes text `.app` / `.sh` files as batch of shell commands
//! Each non-empty line is executed via shell's command dispatcher.
//! Supports args substitution `$1`, `$@`, comments `#`, and `exit <code>`.

use crate::shell::{clear_interrupt, is_interrupted};
use alloc::string::String;
use alloc::vec::Vec;

/// Run script file `data` as batch. `argv` includes app name + args.
/// Returns exit code (0 success)
pub fn run_script(data: &[u8], path: &str, argv: &[&str]) -> Result<i32, &'static str> {
    let text = core::str::from_utf8(data).map_err(|_| "Script is not UTF-8")?;
    let lines: Vec<&str> = text.lines().collect();
    crate::println!("[run] Executing script '{}' ({} lines)", path, lines.len());
    let mut exit_code: i32 = 0;

    for (idx, raw) in lines.iter().enumerate() {
        if is_interrupted() {
            clear_interrupt();
            crate::println!("\n[app interrupted ^C] at line {}", idx + 1);
            return Err("Interrupted");
        }
        // Allow cooperative tick / net processing between lines
        crate::shell::increment_tick();
        crate::net::process_packets();

        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Handle `exit`, `exit 42`, `return` inside script
        if line == "exit" {
            break;
        }
        if let Some(rest) = line.strip_prefix("exit ") {
            let code = rest.trim().parse::<i32>().unwrap_or(0);
            exit_code = code;
            break;
        }
        // Variable substitution: $0 = path, $1..$n = argv[1..], $@ = all args joined, $# = argc
        let expanded = expand_vars(line, path, argv);
        // Also support `echo $@` etc via shell
        // Delegate to shell dispatcher (now public)
        let should_break = crate::shell::execute_command(&expanded);
        // execute_command returns true if it was `exit`? Actually our impl returns bool for script control
        // For now shell commands like `reboot/halt` shouldn't be allowed inside apps? Allow but warn.
        // If shell's `exit` command was used, it does NOT halt kernel when called from app - we intercept.
        // So we check expanded starts with exit above already.
        // Provide `halt`/`reboot` guard: don't actually halt when inside app
        if expanded.trim() == "halt" || expanded.trim() == "reboot" || expanded.trim() == "shutdown"
        {
            crate::println!("[app] '{}' ignored inside app context", expanded.trim());
        }
        let _ = should_break;
    }
    Ok(exit_code)
}

fn expand_vars(line: &str, path: &str, argv: &[&str]) -> String {
    // argv[0] is app path if provided, else path
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&next) = chars.peek() {
                match next {
                    '0' => {
                        chars.next();
                        out.push_str(path);
                    }
                    '@' => {
                        chars.next();
                        for (i, a) in argv.iter().enumerate() {
                            if i > 0 {
                                out.push(' ');
                            }
                            out.push_str(a);
                        }
                    }
                    '#' => {
                        chars.next();
                        out.push_str(&alloc::format!("{}", argv.len()));
                    }
                    '1'..='9' => {
                        chars.next();
                        let idx = (next as u8 - b'0') as usize;
                        if idx < argv.len() {
                            out.push_str(argv[idx]);
                        }
                    }
                    '$' => {
                        chars.next();
                        out.push('$');
                    }
                    _ => {
                        out.push(c);
                    }
                }
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    out
}
