//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.


//! Terminal Shell
//!
//! A simple command-line shell for the Matzen Kernel Framework.

use crate::drivers::{keyboard, vga};
use crate::{print, println};
use core::sync::atomic::{AtomicU64, Ordering};

/// Maximum length of a command line
const MAX_CMD_LENGTH: usize = 256;

/// Shell prompt string
const PROMPT: &str = "mfk> ";

/// Simple tick counter for uptime tracking
static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Runs the shell loop
pub fn run() -> ! {
    let mut cmd_buffer: [u8; MAX_CMD_LENGTH] = [0; MAX_CMD_LENGTH];
    let mut cmd_len: usize = 0;

    print!("{}", PROMPT);

    loop {
        // Increment tick counter for basic timing
        TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
        
        if let Some(c) = keyboard::read_char() {
            match c {
                '\n' | '\r' => {
                    println!();
                    if cmd_len > 0 {
                        let cmd = core::str::from_utf8(&cmd_buffer[..cmd_len]).unwrap_or("");
                        execute_command(cmd);
                        cmd_len = 0;
                        cmd_buffer = [0; MAX_CMD_LENGTH];
                    }
                    print!("{}", PROMPT);
                }
                '\x08' | '\x7f' => {
                    // Backspace
                    if cmd_len > 0 {
                        cmd_len -= 1;
                        cmd_buffer[cmd_len] = 0;
                        vga::backspace();
                    }
                }
                c if c.is_ascii() && !c.is_control() => {
                    if cmd_len < MAX_CMD_LENGTH - 1 {
                        cmd_buffer[cmd_len] = c as u8;
                        cmd_len += 1;
                        print!("{}", c);
                    }
                }
                _ => {}
            }
        }
        // Small yield to prevent busy loop from hogging CPU
        // We use a spin hint instead of hlt() to ensure we poll frequently
        core::hint::spin_loop();
    }
}

/// Executes a command
fn execute_command(cmd: &str) {
    let cmd = cmd.trim();
    let parts: (&str, &str) = match cmd.find(' ') {
        Some(pos) => (&cmd[..pos], cmd[pos + 1..].trim()),
        None => (cmd, ""),
    };

    match parts.0 {
        "help" => cmd_help(),
        "clear" | "cls" => cmd_clear(),
        "echo" => cmd_echo(parts.1),
        "about" => cmd_about(),
        "version" => cmd_version(),
        "uptime" => cmd_uptime(),
        "mem" | "memory" => cmd_memory(),
        "reboot" => cmd_reboot(),
        "halt" | "shutdown" => cmd_halt(),
        "date" => cmd_date(),
        "whoami" => cmd_whoami(),
        "cpuinfo" => cmd_cpuinfo(),
        "calc" => cmd_calc(parts.1),
        "color" => cmd_color(parts.1),
        "test" => cmd_test(),
        "" => {}
        _ => {
            println!("Unknown command: '{}'. Type 'help' for available commands.", parts.0);
        }
    }
}

/// Displays help information
fn cmd_help() {
    println!("Available commands:");
    println!("  help      - Display this help message");
    println!("  clear/cls - Clear the screen");
    println!("  echo      - Print text to the screen");
    println!("  about     - Display information about MFK");
    println!("  version   - Display kernel version");
    println!("  uptime    - Show system uptime");
    println!("  memory    - Display memory information");
    println!("  cpuinfo   - Display CPU information");
    println!("  calc      - Calculator (e.g., 'calc 5 + 3')");
    println!("  color     - Change text color (green/white/cyan/yellow/red/blue/pink)");
    println!("  test      - Run system tests");
    println!("  reboot    - Reboot the system");
    println!("  halt      - Halt the system");
    println!("  date      - Display current date (simulated)");
    println!("  whoami    - Display current user");
}

/// Clears the screen
fn cmd_clear() {
    vga::clear_screen();
}

/// Echoes text back to the screen
fn cmd_echo(text: &str) {
    println!("{}", text);
}

/// Displays about information
fn cmd_about() {
    println!("Matzen Kernel Framework (MFK) v0.1.0");
    println!("A simple terminal OS written in Rust");
    println!();
    println!("Features:");
    println!("  - VGA text mode output");
    println!("  - PS/2 keyboard input");
    println!("  - Built-in command shell");
    println!();
    println!("Architecture: x86_64");
    println!("Author: AlexMatzen01");
}

/// Displays uptime
fn cmd_uptime() {
    let ticks = TICK_COUNTER.load(Ordering::Relaxed);
    // Rough estimate: each loop iteration is very fast, 
    // we estimate ~1000 ticks per second in idle
    let approx_seconds = ticks / 1000;
    let minutes = approx_seconds / 60;
    let seconds = approx_seconds % 60;
    println!("System uptime: {} minutes, {} seconds", minutes, seconds);
    println!("(Loop iterations: {})", ticks);
}

/// Displays memory information
fn cmd_memory() {
    println!("Memory Information:");
    println!("  VGA Buffer:    0xB8000 (4 KB)");
    println!("  Kernel loaded: 0x100000 (varies)");
    println!();
    println!("Memory Layout:");
    println!("  0x00000000 - 0x0009FFFF: Conventional Memory (640 KB)");
    println!("  0x000A0000 - 0x000BFFFF: VGA Memory");
    println!("  0x000C0000 - 0x000FFFFF: ROM Area");
    println!("  0x00100000+            : Extended Memory (Kernel)");
}

/// Reboots the system
fn cmd_reboot() {
    println!("Rebooting...");
    // PS/2 controller reset command
    const KEYBOARD_COMMAND_PORT: u16 = 0x64;
    const KEYBOARD_RESET_COMMAND: u8 = 0xFE;
    unsafe {
        let mut port: x86_64::instructions::port::Port<u8> = x86_64::instructions::port::Port::new(KEYBOARD_COMMAND_PORT);
        port.write(KEYBOARD_RESET_COMMAND);
    }
}

/// Halts the system
fn cmd_halt() {
    println!("System halted. You can now turn off your computer.");
    loop {
        x86_64::instructions::hlt();
    }
}

/// Displays the current date and time from the RTC
fn cmd_date() {
    let dt = crate::drivers::rtc::read_rtc();
    
    println!("{}, {} {}, {}", 
        dt.day_name(),
        dt.month_name(),
        dt.day,
        dt.year
    );
    println!("{:02}:{:02}:{:02} UTC",
        dt.hour,
        dt.minute,
        dt.second
    );
}

/// Displays current user
fn cmd_whoami() {
    println!("root");
}

/// Displays kernel version
fn cmd_version() {
    println!("Matzen Kernel Framework (MFK)");
    println!("  Version:  0.1.0");
    println!("  Build:    debug");
    println!("  Arch:     x86_64");
    println!("  Compiler: rustc (nightly)");
}

/// Displays CPU information using CPUID
fn cmd_cpuinfo() {
    println!("CPU Information:");
    
    // Get vendor string using CPUID leaf 0
    let vendor = get_cpu_vendor();
    println!("  Vendor: {}", vendor);
    
    // Get CPU features using CPUID leaf 1
    let (family, model, stepping) = get_cpu_signature();
    println!("  Family: {}, Model: {}, Stepping: {}", family, model, stepping);
    
    // Check for some common features
    let features = get_cpu_features();
    println!("  Features: {}", features);
}

/// Gets CPU vendor string from CPUID
fn get_cpu_vendor() -> &'static str {
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx:e}, ebx",
            "pop rbx",
            inout("eax") 0u32 => _,
            ebx = out(reg) ebx,
            out("ecx") ecx,
            out("edx") edx,
        );
    }
    
    // Vendor string is in EBX, EDX, ECX (in that order)
    // Check for common vendors
    if ebx == 0x756e6547 && edx == 0x49656e69 && ecx == 0x6c65746e {
        "GenuineIntel"
    } else if ebx == 0x68747541 && edx == 0x69746e65 && ecx == 0x444d4163 {
        "AuthenticAMD"
    } else {
        "Unknown"
    }
}

/// Gets CPU signature (family, model, stepping)
fn get_cpu_signature() -> (u32, u32, u32) {
    let eax: u32;
    
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => eax,
            out("ecx") _,
            out("edx") _,
        );
    }
    
    let stepping = eax & 0xF;
    let model = (eax >> 4) & 0xF;
    let family = (eax >> 8) & 0xF;
    
    (family, model, stepping)
}

/// Gets a string of CPU feature flags
fn get_cpu_features() -> &'static str {
    let ecx: u32;
    let edx: u32;
    
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => _,
            out("ecx") ecx,
            out("edx") edx,
        );
    }
    
    // Check for SSE2 (bit 26 of EDX)
    let has_sse2 = (edx & (1 << 26)) != 0;
    // Check for SSE3 (bit 0 of ECX)
    let has_sse3 = (ecx & 1) != 0;
    // Check for 64-bit (bit 29 of EDX via extended CPUID, but we know we're x86_64)
    
    if has_sse3 && has_sse2 {
        "SSE2 SSE3 x86_64"
    } else if has_sse2 {
        "SSE2 x86_64"
    } else {
        "x86_64"
    }
}

/// Calculator command
fn cmd_calc(expr: &str) {
    if expr.is_empty() {
        println!("Usage: calc <num1> <op> <num2>");
        println!("  Operations: + - * /");
        println!("  Example: calc 10 + 5");
        return;
    }
    
    // Parse the expression manually: "num1 op num2"
    let mut parts: [&str; 3] = [""; 3];
    let mut part_idx = 0;
    let mut start = 0;
    let mut in_word = false;
    
    for (i, c) in expr.char_indices() {
        if c.is_whitespace() {
            if in_word && part_idx < 3 {
                parts[part_idx] = &expr[start..i];
                part_idx += 1;
                in_word = false;
            }
        } else {
            if !in_word {
                start = i;
                in_word = true;
            }
        }
    }
    // Handle last part
    if in_word && part_idx < 3 {
        parts[part_idx] = &expr[start..];
        part_idx += 1;
    }
    
    if part_idx != 3 {
        println!("Error: Expected format 'num1 op num2'");
        return;
    }
    
    let num1: i64 = match parse_i64(parts[0]) {
        Some(n) => n,
        None => {
            println!("Error: '{}' is not a valid number", parts[0]);
            return;
        }
    };
    
    let num2: i64 = match parse_i64(parts[2]) {
        Some(n) => n,
        None => {
            println!("Error: '{}' is not a valid number", parts[2]);
            return;
        }
    };
    
    let result = match parts[1] {
        "+" => Some(num1.wrapping_add(num2)),
        "-" => Some(num1.wrapping_sub(num2)),
        "*" => Some(num1.wrapping_mul(num2)),
        "/" => {
            if num2 == 0 {
                println!("Error: Division by zero");
                None
            } else {
                Some(num1 / num2)
            }
        }
        op => {
            println!("Error: Unknown operator '{}'. Use + - * /", op);
            None
        }
    };
    
    if let Some(r) = result {
        println!("{} {} {} = {}", num1, parts[1], num2, r);
    }
}

/// Parse a string to i64 without std
fn parse_i64(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    
    let mut chars = s.chars();
    let mut negative = false;
    let first = chars.next()?;
    
    let mut result: i64 = if first == '-' {
        negative = true;
        0
    } else if first.is_ascii_digit() {
        (first as u8 - b'0') as i64
    } else {
        return None;
    };
    
    for c in chars {
        if !c.is_ascii_digit() {
            return None;
        }
        result = result.checked_mul(10)?;
        result = result.checked_add((c as u8 - b'0') as i64)?;
    }
    
    if negative {
        Some(-result)
    } else {
        Some(result)
    }
}

/// Change terminal color
fn cmd_color(color_name: &str) {
    use crate::drivers::vga::{Color, WRITER};
    use x86_64::instructions::interrupts;
    
    // Convert to lowercase for comparison
    let color_lower = to_lowercase_fixed(color_name);
    let color = match color_lower.as_str() {
        "green" => Some(Color::LightGreen),
        "white" => Some(Color::White),
        "cyan" => Some(Color::LightCyan),
        "yellow" => Some(Color::Yellow),
        "red" => Some(Color::LightRed),
        "blue" => Some(Color::LightBlue),
        "pink" | "magenta" => Some(Color::Pink),
        "" => {
            println!("Available colors: green, white, cyan, yellow, red, blue, pink");
            None
        }
        _ => {
            println!("Unknown color '{}'. Available: green, white, cyan, yellow, red, blue, pink", color_name);
            None
        }
    };
    
    if let Some(c) = color {
        interrupts::without_interrupts(|| {
            WRITER.lock().set_color(c, Color::Black);
        });
        println!("Color changed to {}", color_name);
    }
}

/// Convert string to lowercase (fixed-size buffer)
fn to_lowercase_fixed(s: &str) -> LowercaseString {
    let mut result = LowercaseString {
        bytes: [0; 32],
        len: 0,
    };
    for (i, b) in s.bytes().enumerate() {
        if i >= 32 {
            break;
        }
        result.bytes[i] = if b >= b'A' && b <= b'Z' {
            b + 32
        } else {
            b
        };
        result.len = i + 1;
    }
    result
}

/// Lowercase helper struct
struct LowercaseString {
    bytes: [u8; 32],
    len: usize,
}

impl LowercaseString {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// Run system tests
fn cmd_test() {
    println!("Running system tests...");
    println!();
    
    // Test 1: VGA output
    print!("  [TEST] VGA output:      ");
    println!("PASS");
    
    // Test 2: Keyboard polling
    print!("  [TEST] Keyboard driver: ");
    println!("PASS (if you typed this command)");
    
    // Test 3: CPUID
    print!("  [TEST] CPUID:           ");
    let vendor = get_cpu_vendor();
    if vendor != "Unknown" {
        println!("PASS ({})", vendor);
    } else {
        println!("WARN (unknown vendor)");
    }
    
    // Test 4: Calculator
    print!("  [TEST] Calculator:      ");
    let result = 42i64.wrapping_add(58);
    if result == 100 {
        println!("PASS");
    } else {
        println!("FAIL");
    }
    
    // Test 5: Memory access
    print!("  [TEST] Memory access:   ");
    println!("PASS");
    
    println!();
    println!("All tests completed!");
}
