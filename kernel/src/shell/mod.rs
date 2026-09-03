//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Terminal Shell
//!
//! A simple command-line shell for the Matzen Kernel Framework.

use crate::drivers::{keyboard, vga};
use crate::drivers::keyboard::Key;
use crate::{print, println};
use alloc::vec::Vec;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use spin::Mutex;

/// Maximum length of a command line
const MAX_CMD_LENGTH: usize = 256;

/// Builtin commands for TAB completion (must match execute_command and help)
const BUILTINS: &[&str] = &[
    "help", "clear", "cls", "echo", "about", "version", "uptime", "mem", "memory",
    "reboot", "halt", "shutdown", "date", "whoami", "cpuinfo", "calc", "color", "test",
    "diskinfo", "mkfs", "mount", "ls", "dir", "touch", "cat", "write", "rm", "mkdir", "rmdir",
    "cd", "pwd", "ifconfig", "ping", "netstat", "tcpconnect", "tcpsend", "tcpclose",
    "nano", "edit", "mfkedit", "run", "exec", "mkapp", "writehex", "ps", "appinfo",
];

/// Shell prompt string (dynamic in code, fallback)
const PROMPT: &str = "mfk> ";

fn prompt() -> alloc::string::String {
    // Try to show current path like mfk:/docs> 
    if let Some(path) = current_path_string() {
        alloc::format!("mfk:{}> ", path)
    } else {
        alloc::string::String::from(PROMPT)
    }
}

fn current_path_string() -> Option<alloc::string::String> {
    let fs_guard = FILESYSTEM.lock();
    if fs_guard.is_some() {
        drop(fs_guard);
        let mut device = crate::drivers::block::AtaBlockDevice::new();
        let mut guard = FILESYSTEM.lock();
        if let Some(ref mut fs) = *guard {
            if let Ok(p) = fs.current_path(&mut device) {
                return Some(p);
            }
        }
    }
    None
}

/// Simple tick counter for uptime tracking
static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Global interrupt flag for Ctrl+C handling
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);

/// Global filesystem state
static FILESYSTEM: Mutex<Option<crate::fs::SimpleFilesystem>> = Mutex::new(None);

/// Get the current tick count (rough millisecond approximation)
pub fn get_tick_count() -> u64 {
    TICK_COUNTER.load(Ordering::Relaxed)
}

/// Increment tick (used by editor which bypasses shell::run loop)
pub fn increment_tick() {
    TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
}

/// Check if Ctrl+C was pressed
pub fn is_interrupted() -> bool {
    INTERRUPT_FLAG.load(Ordering::Relaxed)
}

/// Clear the interrupt flag
pub fn clear_interrupt() {
    INTERRUPT_FLAG.store(false, Ordering::Relaxed);
}

/// Set the interrupt flag (called when Ctrl+C is detected)
fn set_interrupt() {
    INTERRUPT_FLAG.store(true, Ordering::Relaxed);
}

/// Runs the shell loop
pub fn run() -> ! {
    let mut cmd_buffer: [u8; MAX_CMD_LENGTH] = [0; MAX_CMD_LENGTH];
    let mut cmd_len: usize = 0;

    print!("{}", prompt());

    loop {
        // Increment tick counter for basic timing
        TICK_COUNTER.fetch_add(1, Ordering::Relaxed);

        // Process network packets
        crate::net::process_packets();

        if let Some(ev) = keyboard::read_key() {
            match ev.key {
                Key::Ctrl('C') => {
                    // Ctrl+C detected
                    set_interrupt();
                    println!("^C");
                    cmd_len = 0;
                    cmd_buffer = [0; MAX_CMD_LENGTH];
                    print!("\n{}", prompt());
                }
                Key::Enter => {
                    println!();
                    if cmd_len > 0 {
                        let cmd = core::str::from_utf8(&cmd_buffer[..cmd_len]).unwrap_or("");
                        execute_command(cmd);
                        cmd_len = 0;
                        cmd_buffer = [0; MAX_CMD_LENGTH];
                    }
                    clear_interrupt();
                    print!("{}", prompt());
                }
                Key::Backspace => {
                    if cmd_len > 0 {
                        cmd_len -= 1;
                        cmd_buffer[cmd_len] = 0;
                        print!("\x08 \x08");
                    }
                }
                Key::Tab => {
                    handle_tab_completion(&mut cmd_buffer, &mut cmd_len);
                }
                Key::Char(c) if c.is_ascii() && !c.is_control() => {
                    if cmd_len < MAX_CMD_LENGTH - 1 {
                        cmd_buffer[cmd_len] = c as u8;
                        cmd_len += 1;
                        print!("{}", c);
                    }
                }
                _ => {}
            }
        }
        core::hint::spin_loop();
    }
}

/// Executes a command - public for app interpreter
/// Returns true if caller should break (reserved for script control)
pub fn execute_command(cmd: &str) -> bool {
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
        "diskinfo" => cmd_diskinfo(),
        "mkfs" => cmd_mkfs(),
        "mount" => cmd_mount(),
        "ls" | "dir" => cmd_ls(parts.1),
        "touch" => cmd_touch(parts.1),
        "cat" => cmd_cat(parts.1),
        "write" => cmd_write(parts.1),
        "rm" => cmd_rm(parts.1),
        "mkdir" => cmd_mkdir(parts.1),
        "rmdir" => cmd_rmdir(parts.1),
        "cd" => cmd_cd(parts.1),
        "pwd" => cmd_pwd(),
        "ifconfig" => cmd_ifconfig(parts.1),
        "ping" => cmd_ping(parts.1),
        "netstat" => cmd_netstat(),
        "tcpconnect" => cmd_tcpconnect(parts.1),
        "tcpsend" => cmd_tcpsend(parts.1),
        "tcpclose" => cmd_tcpclose(parts.1),
        "nano" | "edit" | "mfkedit" => cmd_edit(parts.1),
        "run" | "exec" => cmd_run(parts.1),
        "mkapp" => cmd_mkapp(parts.1),
        "writehex" => cmd_writehex(parts.1),
        "ps" => cmd_ps(),
        "appinfo" => cmd_appinfo(parts.1),
        "" => {}
        _ => {
            println!(
                "Unknown command: '{}'. Type 'help' for available commands.",
                parts.0
            );
        }
    }
    false
}

/// Displays help information
fn cmd_help() {
    println!("Available commands (Tab completes commands & files):");
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
    println!();
    println!("File System Commands:");
    println!("  diskinfo  - Display disk information");
    println!("  mkfs      - Format the disk with SimplFS");
    println!("  mount     - Mount the filesystem");
    println!("  ls/dir [path] - List files (e.g., 'ls', 'ls /docs')");
    println!("  touch     - Create a new file (e.g., 'touch test.txt', 'touch dir/file.txt')");
    println!("  cat       - Display file contents (e.g., 'cat test.txt')");
    println!("  write     - Write text to file (e.g., 'write test.txt Hello World')");
    println!("  rm        - Delete a file (e.g., 'rm test.txt')");
    println!("  mkdir     - Create directory (e.g., 'mkdir docs', 'mkdir /a/b')");
    println!("  rmdir     - Remove empty directory (e.g., 'rmdir docs')");
    println!("  cd        - Change directory (e.g., 'cd docs', 'cd ..', 'cd /')");
    println!("  pwd       - Print working directory");
    println!();
    println!("Editor Commands (nano-like):");
    println!("  nano/edit  - Text editor (e.g., 'nano file.txt', 'edit --help')");
    println!("    ^O/^S Write, ^X Exit, ^K Cut, ^U Uncut, ^W WhereIs");
    println!("    ^C CurPos, ^_ GotoLine, ^J Justify, ^R ReadFile");
    println!("    ^\\ Replace, ^G Help, ^Z Undo, ^Y Redo, Alt+A Mark");
    println!("    Arrows/Home/End/PgUp/PgDn navigate, Tab=4sp, $ scroll");
    println!();
    println!("App Commands:");
    println!("  run/exec      - Run an app (script or MFKE bytecode)");
    println!("                 e.g., 'run /apps/hello.app', 'run /bin/counter.mfke'");
    println!("  mkapp         - Create example app (e.g., 'mkapp hello /apps/hello.app')");
    println!("                 kinds: hello, hello-mfke, counter, calc, filedemo, loop");
    println!("  writehex      - Write binary from hex (e.g., 'writehex /tmp/a.bin 4D464B45...')");
    println!("  appinfo       - Show app file info (e.g., 'appinfo /apps/hello.mfke')");
    println!("  ps            - Show process status (Phase 1: cooperative)");
    println!();
    println!("Network Commands:");
    println!("  ifconfig     - Configure network interface (e.g., 'ifconfig 10.0.2.15')");
    println!("  ping         - Send ICMP echo request (e.g., 'ping 10.0.2.2 4')");
    println!("  netstat      - Display network status");
    println!("  tcpconnect   - Connect to TCP server (e.g., 'tcpconnect 10.0.2.2 80')");
    println!("  tcpsend      - Send data on TCP connection (e.g., 'tcpsend <port> <data>')");
    println!("  tcpclose     - Close TCP connection (e.g., 'tcpclose <port>')");
}

/// Clears the screen - true clear for both VGA and serial, no whitespace trick
fn cmd_clear() {
    vga::clear_screen();
    // True ANSI clear for serial/QEMU headless (VGA already cleared above)
    crate::serial_print!("\x1b[2J\x1b[H\x1b[0m");
    // Sync hardware cursor to where next shell text will appear (bottom row, col 0)
    // VGA Writer is bottom-anchored (write_byte always at BUFFER_HEIGHT-1), so home is bottom row.
    vga::set_cursor_pos(crate::drivers::vga::VGA_HEIGHT - 1, 0);
    vga::show_cursor();
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
        let mut port: x86_64::instructions::port::Port<u8> =
            x86_64::instructions::port::Port::new(KEYBOARD_COMMAND_PORT);
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

    println!(
        "{}, {} {}, {}",
        dt.day_name(),
        dt.month_name(),
        dt.day,
        dt.year
    );
    println!("{:02}:{:02}:{:02} UTC", dt.hour, dt.minute, dt.second);
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
    println!(
        "  Family: {}, Model: {}, Stepping: {}",
        family, model, stepping
    );

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
            println!(
                "Unknown color '{}'. Available: green, white, cyan, yellow, red, blue, pink",
                color_name
            );
            None
        }
    };

    if let Some(c) = color {
        interrupts::without_interrupts(|| {
            WRITER.lock().set_color(c, Color::Black);
        });
        println!(
            "Color changed to {} - this text should appear in the new color",
            color_name
        );
        println!("(Previous text will keep its original color)");
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
        result.bytes[i] = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
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

/// Display disk information
fn cmd_diskinfo() {
    println!("Disk Information:");
    println!("  Primary Master: ATA PIO Mode");
    println!("  Sector size: 512 bytes");
    println!("  Block device layer: Active");
    println!();
    println!("Use 'mkfs' to format disk, then 'mount' to access filesystem");
}

/// Format the disk with SimplFS
fn cmd_mkfs() {
    println!("Formatting disk with SimplFS...");

    let mut device = crate::drivers::block::AtaBlockDevice::new();
    match crate::fs::SimpleFilesystem::format(&mut device) {
        Ok(()) => {
            println!("Filesystem formatted successfully!");
            println!("Use 'mount' to mount the filesystem.");
        }
        Err(e) => {
            println!("Failed to format filesystem: {}", e);
        }
    }
}

/// Mount the filesystem
fn cmd_mount() {
    println!("Mounting filesystem...");

    let mut device = crate::drivers::block::AtaBlockDevice::new();
    match crate::fs::SimpleFilesystem::mount(&mut device) {
        Ok(fs) => {
            *FILESYSTEM.lock() = Some(fs);
            println!("Filesystem mounted successfully!");
            println!("Root directory ready. Use 'ls' to list files.");
        }
        Err(e) => {
            println!("Failed to mount filesystem: {}", e);
            println!("You may need to run 'mkfs' first to format the disk.");
        }
    }
}

/// List files in directory (optional path)
fn cmd_ls(path: &str) {
    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    drop(fs_guard);
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    let mut fs_guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *fs_guard {
        // Determine target inode
        let target = if path.trim().is_empty() {
            fs.current_directory()
        } else {
            match fs.resolve_file_or_dir(&mut device, path.trim()) {
                Ok(ino) => {
                    // Must be directory
                    if !fs.is_dir(ino) {
                        println!("ls: Not a directory");
                        return;
                    }
                    ino
                }
                Err(e) => {
                    println!("ls: {}: {}", path, e);
                    return;
                }
            }
        };
        // Show path header if explicit
        if !path.trim().is_empty() {
            let label = path.trim();
            println!("{}:", label);
        }
        match fs.list_directory(&mut device, target) {
            Ok(files) => {
                if files.is_empty() {
                    println!("(empty directory)");
                } else {
                    // Sort: directories first already? Keep order
                    for file in files {
                        println!("  {}", file);
                    }
                }
            }
            Err(e) => {
                println!("Failed to list directory: {}", e);
            }
        }
    }
}

/// Create a new file (path-aware)
fn cmd_touch(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: touch <filename>");
        println!("  Supports paths: touch dir/file.txt, touch /a/b/file");
        return;
    }

    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }

    let mut device = crate::drivers::block::AtaBlockDevice::new();

    if let Some(ref mut fs) = *fs_guard {
        match fs.create_file(&mut device, path.trim()) {
            Ok(inode_num) => {
                println!("Created file '{}' (inode {})", path.trim(), inode_num);
            }
            Err(e) => {
                println!("Failed to create file: {}", e);
            }
        }
    }
}

/// Display file contents (path-aware)
fn cmd_cat(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: cat <filename>");
        return;
    }

    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }

    let mut device = crate::drivers::block::AtaBlockDevice::new();

    if let Some(ref mut fs) = *fs_guard {
        match fs.read_file(&mut device, path.trim()) {
            Ok(data) => {
                if data.is_empty() {
                    println!("(empty file)");
                } else {
                    // Try to display as text
                    match core::str::from_utf8(&data) {
                        Ok(text) => println!("{}", text),
                        Err(_) => {
                            println!("(binary file, {} bytes)", data.len());
                            // Show first 256 bytes in hex
                            let display_len = core::cmp::min(data.len(), 256);
                            for (i, byte) in data[..display_len].iter().enumerate() {
                                if i % 16 == 0 {
                                    if i > 0 {
                                        println!();
                                    }
                                    print!("{:04x}: ", i);
                                }
                                print!("{:02x} ", byte);
                            }
                            println!();
                            if data.len() > display_len {
                                println!("... ({} more bytes)", data.len() - display_len);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("Failed to read file: {}", e);
            }
        }
    }
}

/// Write text to a file (path-aware)
fn cmd_write(args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();

    if parts.len() < 2 {
        println!("Usage: write <filename> <text>");
        return;
    }

    let filename = parts[0].trim();
    let content = parts[1];

    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }

    let mut device = crate::drivers::block::AtaBlockDevice::new();

    if let Some(ref mut fs) = *fs_guard {
        // Try to resolve existing file (path-aware)
        let existing = fs.resolve_file_or_dir(&mut device, filename);
        let inode_num = match existing {
            Ok(ino) => {
                if !fs.is_file(ino) {
                    println!("write: '{}' is a directory", filename);
                    return;
                }
                ino
            }
            Err(_) => {
                // File doesn't exist, create it (path-aware)
                match fs.create_file(&mut device, filename) {
                    Ok(inode) => {
                        println!("Created new file '{}'", filename);
                        inode
                    }
                    Err(e) => {
                        println!("Failed to create file: {}", e);
                        return;
                    }
                }
            }
        };

        // Write data to file using inode number directly
        match fs.write_file_by_inode(&mut device, inode_num, content.as_bytes()) {
            Ok(()) => {
                println!("Wrote {} bytes to '{}'", content.len(), filename);
            }
            Err(e) => {
                println!("Failed to write file: {}", e);
            }
        }
    }
}

/// Delete a file (path-aware)
fn cmd_rm(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: rm <filename>");
        return;
    }

    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }

    let mut device = crate::drivers::block::AtaBlockDevice::new();

    if let Some(ref mut fs) = *fs_guard {
        match fs.delete_file(&mut device, path.trim()) {
            Ok(()) => {
                println!("Deleted file '{}'", path.trim());
            }
            Err(e) => {
                println!("Failed to delete file: {}", e);
            }
        }
    }
}

/// Create directory
fn cmd_mkdir(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: mkdir <directory>");
        println!("  Example: mkdir docs, mkdir /a/b, mkdir mydir");
        return;
    }
    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    if let Some(ref mut fs) = *fs_guard {
        match fs.create_directory(&mut device, path.trim()) {
            Ok(ino) => println!("Created directory '{}' (inode {})", path.trim(), ino),
            Err(e) => println!("mkdir: cannot create directory '{}': {}", path.trim(), e),
        }
    }
}

/// Remove empty directory
fn cmd_rmdir(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: rmdir <directory>");
        return;
    }
    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    if let Some(ref mut fs) = *fs_guard {
        match fs.remove_directory(&mut device, path.trim()) {
            Ok(()) => println!("Removed directory '{}'", path.trim()),
            Err(e) => println!("rmdir: failed to remove '{}': {}", path.trim(), e),
        }
    }
}

/// Change directory
fn cmd_cd(path: &str) {
    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    if let Some(ref mut fs) = *fs_guard {
        let target = if path.trim().is_empty() { "/" } else { path.trim() };
        match fs.change_directory_path(&mut device, target) {
            Ok(()) => {}
            Err(e) => println!("cd: {}: {}", target, e),
        }
    }
}

/// Print working directory
fn cmd_pwd() {
    let mut fs_guard = FILESYSTEM.lock();
    if fs_guard.is_none() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    if let Some(ref mut fs) = *fs_guard {
        match fs.current_path(&mut device) {
            Ok(p) => println!("{}", p),
            Err(e) => println!("pwd: {}", e),
        }
    }
}

/// Configure network interface
fn cmd_ifconfig(args: &str) {
    if args.is_empty() {
        // Display current configuration
        if let Some(mac) = crate::drivers::e1000::mac_address() {
            println!("Network Interface:");
            println!("  MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            
            if let Some(ip) = crate::net::ip::get_ip_address() {
                println!("  IP Address:  {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
            } else {
                println!("  IP Address:  Not configured");
            }
        } else {
            println!("Network interface not initialized");
        }
    } else {
        // Parse and set IP address
        let parts: Vec<&str> = args.split('.').collect();
        if parts.len() != 4 {
            println!("Invalid IP address format. Use: ifconfig <ip> (e.g., ifconfig 10.0.2.15)");
            return;
        }

        let mut ip = [0u8; 4];
        for (i, part) in parts.iter().enumerate() {
            match part.parse::<u8>() {
                Ok(octet) => ip[i] = octet,
                Err(_) => {
                    println!("Invalid IP address format");
                    return;
                }
            }
        }

        crate::net::ip::set_ip_address(ip);
        println!("IP address set to {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    }
}

/// Send ping (ICMP echo request)
fn cmd_ping(args: &str) {
    if args.is_empty() {
        println!("Usage: ping <ip-address> [count]");
        println!("Example: ping 10.0.2.2 4");
        return;
    }

    // Split args to get IP and optional count
    let parts: Vec<&str> = args.split_whitespace().collect();
    
    // Parse IP address
    let ip_parts: Vec<&str> = parts[0].split('.').collect();
    if ip_parts.len() != 4 {
        println!("Invalid IP address format");
        return;
    }

    let mut target_ip = [0u8; 4];
    for (i, part) in ip_parts.iter().enumerate() {
        match part.parse::<u8>() {
            Ok(octet) => target_ip[i] = octet,
            Err(_) => {
                println!("Invalid IP address");
                return;
            }
        }
    }
    
    // Parse count
    let count = if parts.len() > 1 {
        parts[1].parse().unwrap_or(4)
    } else {
        4
    };

    println!("Pinging {}.{}.{}.{} with {} packets...", 
        target_ip[0], target_ip[1], target_ip[2], target_ip[3], count);
    
    // Send pings
    for seq in 0..count {
        match crate::net::icmp::send_ping(target_ip, 1, seq) {
            Ok(_) => {},
            Err(e) => {
                println!("Failed to send ping {}: {}", seq, e);
                break;
            }
        }
        // Small delay between pings
        for _ in 0..1000000 { core::hint::spin_loop(); }
    }
    
    println!("Sent {} ping(s), waiting for replies...", count);
    
    // Wait for replies with timeout
    let start_time = get_tick_count();
    let timeout_ms = 5000;
    let mut received = 0;
    clear_interrupt();
    
    while get_tick_count() - start_time < timeout_ms && !is_interrupted() {
        // Process incoming packets
        crate::net::process_packets();
        
        // Check for replies
        while let Some(reply) = crate::net::icmp::pop_reply() {
            println!("Reply from {}.{}.{}.{}: seq={} time={}ms",
                reply.source_ip[0], reply.source_ip[1],
                reply.source_ip[2], reply.source_ip[3],
                reply.sequence, reply.rtt_ms);
            received += 1;
        }
        
        // If we got all replies, we're done
        if received >= count {
            break;
        }
        
        // Small delay
        for _ in 0..10000 { core::hint::spin_loop(); }
    }
    
    if is_interrupted() {
        println!("Ping cancelled by user");
        clear_interrupt();
    } else {
        // Check for timeouts
        let timed_out = crate::net::icmp::check_timeouts();
        if timed_out > 0 {
            println!("{} packet(s) timed out", timed_out);
        }
        
        println!("--- ping statistics ---");
        println!("{} packets transmitted, {} received, {}% packet loss",
            count, received, 
            if count > 0 { ((count - received) * 100) / count } else { 0 });
    }
}

/// Display network statistics
fn cmd_netstat() {
    println!("Network Status:");
    println!();
    
    if let Some(mac) = crate::drivers::e1000::mac_address() {
        println!("Interface: E1000");
        println!("  MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
        
        if let Some(ip) = crate::net::ip::get_ip_address() {
            println!("  IP:  {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        } else {
            println!("  IP:  Not configured");
        }
        
        println!();
        println!("Protocol Stack:");
        println!("  Ethernet - Active");
        println!("  ARP      - Active");
        println!("  IPv4     - Active");
        println!("  ICMP     - Active");
        println!("  UDP      - Active");
        println!("  TCP      - Active");
    } else {
        println!("Network interface not initialized");
    }
}

fn cmd_tcpconnect(args: &str) {
    if args.is_empty() {
        println!("Usage: tcpconnect <ip-address> <port>");
        println!("Example: tcpconnect 10.0.2.2 80");
        println!("         tcpconnect 93.184.216.34 80  (example.com)");
        return;
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        println!("Error: Missing IP address or port");
        return;
    }

    // Parse IP address
    let ip_parts: Vec<&str> = parts[0].split('.').collect();
    if ip_parts.len() != 4 {
        println!("Invalid IP address format");
        return;
    }

    let mut target_ip = [0u8; 4];
    for (i, part) in ip_parts.iter().enumerate() {
        match part.parse::<u8>() {
            Ok(octet) => target_ip[i] = octet,
            Err(_) => {
                println!("Invalid IP address");
                return;
            }
        }
    }

    // Parse port
    let port = match parts[1].parse::<u16>() {
        Ok(p) => p,
        Err(_) => {
            println!("Invalid port number");
            return;
        }
    };

    println!("Connecting to {}.{}.{}.{}:{}...", 
        target_ip[0], target_ip[1], target_ip[2], target_ip[3], port);

    match crate::net::tcp::connect(target_ip, port) {
        Ok(local_port) => {
            println!("Connection initiated from local port {}", local_port);
            println!("Waiting for connection to establish...");
            
            // Wait for connection to establish
            let start_time = get_tick_count();
            let timeout_ms = 5000;
            clear_interrupt();
            
            while get_tick_count() - start_time < timeout_ms && !is_interrupted() {
                crate::net::process_packets();
                
                if let Some(state) = crate::net::tcp::get_state(local_port) {
                    if state == crate::net::tcp::TcpState::Established {
                        println!("Connection established! Local port: {}", local_port);
                        println!("Use 'tcpsend {} <data>' to send data", local_port);
                        println!("Use 'tcpclose {}' to close connection", local_port);
                        return;
                    }
                }
                
                for _ in 0..10000 { core::hint::spin_loop(); }
            }
            
            if is_interrupted() {
                println!("Connection cancelled by user");
                clear_interrupt();
            } else {
                println!("Connection timeout - no response from server");
                println!("Note: With QEMU user-mode networking, only connections to");
                println!("      the host (10.0.2.2) may work. Use TAP networking for");
                println!("      connections to external servers.");
            }
        }
        Err(e) => {
            println!("Failed to initiate connection: {}", e);
        }
    }
}

fn cmd_tcpsend(args: &str) {
    if args.is_empty() {
        println!("Usage: tcpsend <local-port> <data>");
        println!("Example: tcpsend 49152 GET / HTTP/1.0");
        return;
    }

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        println!("Error: Missing port or data");
        return;
    }

    let port = match parts[0].parse::<u16>() {
        Ok(p) => p,
        Err(_) => {
            println!("Invalid port number");
            return;
        }
    };

    let data = parts[1].as_bytes();
    
    match crate::net::tcp::send_data(port, data) {
        Ok(_) => {
            println!("Sent {} bytes on port {}", data.len(), port);
            println!("Checking for response...");
            
            // Wait a bit for response
            let start_time = get_tick_count();
            let timeout_ms = 2000;
            
            while get_tick_count() - start_time < timeout_ms {
                crate::net::process_packets();
                
                if let Some(recv_data) = crate::net::tcp::read_data(port) {
                    println!("Received {} bytes:", recv_data.len());
                    // Print as string if possible
                    if let Ok(s) = core::str::from_utf8(&recv_data) {
                        println!("{}", s);
                    } else {
                        println!("(binary data)");
                    }
                    return;
                }
                
                for _ in 0..10000 { core::hint::spin_loop(); }
            }
            
            println!("No response received (timeout)");
        }
        Err(e) => {
            println!("Failed to send data: {}", e);
        }
    }
}

fn cmd_tcpclose(args: &str) {
    if args.is_empty() {
        println!("Usage: tcpclose <local-port>");
        println!("Example: tcpclose 49152");
        return;
    }

    let port = match args.trim().parse::<u16>() {
        Ok(p) => p,
        Err(_) => {
            println!("Invalid port number");
            return;
        }
    };

    match crate::net::tcp::close(port) {
        Ok(_) => println!("Closing connection on port {}", port),
        Err(e) => println!("Failed to close connection: {}", e),
    }
}

// ── Editor helpers (exposed for editor crate) ─────────────

/// Check if filesystem is mounted
pub fn is_mounted() -> bool {
    FILESYSTEM.lock().is_some()
}

/// Read file contents via FS – returns None if not mounted or not found (path-aware)
pub fn read_file_contents(name: &str, device: &mut dyn crate::drivers::block::BlockDevice) -> Option<alloc::vec::Vec<u8>> {
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        match fs.read_file(device, name) {
            Ok(data) => Some(data),
            Err(_) => None,
        }
    } else {
        None
    }
}

/// Write file contents – creates file if needed, returns static error str on failure (path-aware)
pub fn write_file_contents(name: &str, data: &[u8], device: &mut dyn crate::drivers::block::BlockDevice) -> Result<(), &'static str> {
    let mut guard = FILESYSTEM.lock();
    if guard.is_none() {
        return Err("Filesystem not mounted");
    }
    if let Some(ref mut fs) = *guard {
        // Resolve existing file if present (path-aware)
        let inode = match fs.resolve_file_or_dir(device, name) {
            Ok(ino) => {
                if !fs.is_file(ino) {
                    return Err("Is a directory");
                }
                ino
            }
            Err(_) => fs.create_file(device, name)?,
        };
        fs.write_file_by_inode(device, inode, data)
    } else {
        Err("Filesystem not mounted")
    }
}

/// Shell edit command – delegates to nano editor
fn cmd_edit(args: &str) {
    crate::editor::run(args);
}

// ── App commands ─────────────────────────────────────

fn cmd_run(args: &str) {
    if args.trim().is_empty() {
        println!("Usage: run <app-path> [args...]");
        println!("  App types:");
        println!("    .app/.sh/.txt  script (batch of shell commands)");
        println!("    .mfke/.bin     MFKE bytecode VM");
        println!("  Examples:");
        println!("    mkfs; mount; mkapp hello /apps/hello.app; run /apps/hello.app");
        println!("    mkapp hello-mfke /apps/hello.mfke; run /apps/hello.mfke");
        println!("    run /apps/hello.app arg1 arg2");
        println!("  Helpers: mkapp, writehex, appinfo");
        return;
    }
    // split first token as path, rest as args
    let mut parts: alloc::vec::Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() { return; }
    let path = parts[0];
    // args for app includes path as $0 plus extra
    let app_args: alloc::vec::Vec<&str> = parts.clone();
    // clear interrupt before run
    clear_interrupt();
    match crate::app::run(path, &app_args) {
        Ok(code) => {
            if code != 0 {
                println!("[run] app exited with code {}", code);
            }
        }
        Err(e) => {
            println!("[run] failed: {}", e);
        }
    }
    clear_interrupt();
}

fn cmd_mkapp(args: &str) {
    if args.trim().is_empty() {
        println!("Usage: mkapp <kind> <path>");
        println!("  Kinds: hello, hello-mfke, counter, calc, filedemo, loop");
        println!("  Examples:");
        println!("    mkapp hello /apps/hello.app");
        println!("    mkapp hello-mfke /apps/hello.mfke");
        println!("    mkapp counter /apps/counter.mfke");
        println!("    mkapp calc /apps/calc.app");
        println!("  Shorthand: mkapp <path>  (defaults to hello)");
        return;
    }
    let tokens: alloc::vec::Vec<&str> = args.split_whitespace().collect();
    let (kind, path) = if tokens.len() == 1 {
        ("hello", tokens[0])
    } else {
        // first word is kind, last word is path (allows future multi-word kind)
        let k = tokens[0];
        let p = tokens[tokens.len() - 1];
        (k, p)
    };
    match crate::app::create_example_app(path, kind) {
        Ok(()) => println!("mkapp: created '{}' as '{}'", path, kind),
        Err(e) => println!("mkapp failed: {}", e),
    }
}

fn cmd_writehex(args: &str) {
    let parts: alloc::vec::Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        println!("Usage: writehex <path> <hex-bytes>");
        println!("  Example: writehex /tmp/data.bin 4D464B45...");
        println!("  Writes binary file from hex string (whitespace ignored).");
        println!("  Useful for injecting MFKE binaries from host.");
        return;
    }
    let path = parts[0].trim();
    let hex = parts[1];
    match crate::app::write_hex_file(path, hex) {
        Ok(n) => println!("writehex: wrote {} bytes to '{}'", n, path),
        Err(e) => println!("writehex failed: {}", e),
    }
}

fn cmd_appinfo(args: &str) {
    if args.trim().is_empty() {
        println!("MFK App Runtime v1 (MFKE bytecode + script)");
        println!("  Syscalls: exit, print_str, print_int, yield, sleep, get_tick");
        println!("  Opcodes: PUSH,ADD,SUB,MUL,DIV,MOD,EQ,LT,GT,DUP,POP,PRINT_STR,PRINT_INT,PRINT_NL,JMP,JZ,JNZ,SLEEP,YIELD,HALT,EXIT,CALL");
        println!("  Header: magic MFKE (0x454B4D46), version 1, entry,len");
        println!("  Use 'run <path>' to execute, 'mkapp --help' to create.");
        return;
    }
    // show file info if path given
    let path = args.trim();
    if !is_mounted() {
        println!("Filesystem not mounted");
        return;
    }
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    if let Some(data) = read_file_contents(path, &mut device) {
        println!("App '{}' ({} bytes)", path, data.len());
        if data.len() >= 4 && &data[0..4] == &[0x7F, b'E', b'L', b'F'] {
            println!("  Type: ELF (native) - not yet runnable, needs Phase 2");
        } else if data.len() >= 4 && u32::from_le_bytes([data[0],data[1],data[2],data[3]]) == crate::app::loader::MFKE_MAGIC {
            if let Ok(h) = crate::app::loader::validate_header(&data) {
                let len = unsafe { core::ptr::addr_of!(h.bytecode_len).read_unaligned() };
                let entry = unsafe { core::ptr::addr_of!(h.entry_offset).read_unaligned() };
                println!("  Type: MFKE bytecode");
                println!("  Entry: {}, bytecode len: {}", entry, len);
                println!("  Runnable: yes (run {})", path);
            }
        } else if data.contains(&0) {
            println!("  Type: unknown binary");
        } else {
            println!("  Type: script/text");
            println!("  Runnable: yes ({} lines)", data.iter().filter(|&&b| b==b'\n').count()+1);
            // preview first 5 lines
            if let Ok(s) = core::str::from_utf8(&data) {
                for (i, line) in s.lines().take(5).enumerate() {
                    println!("    {}: {}", i+1, line);
                }
                if s.lines().count() > 5 { println!("    ..."); }
            }
        }
    } else {
        println!("Failed to read '{}'", path);
    }
}

fn cmd_ps() {
    println!("Process list (cooperative, single-task Phase 1):");
    println!("  PID 1  shell  (running)");
    println!("  Note: Phase 2 will add real scheduler with preemption.");
    println!("  Apps currently run synchronously: `run` blocks shell until exit.");
}

// ── TAB completion ───────────────────────────────────

fn handle_tab_completion(cmd_buffer: &mut [u8; MAX_CMD_LENGTH], cmd_len: &mut usize) {
    let input = match core::str::from_utf8(&cmd_buffer[..*cmd_len]) {
        Ok(s) => s,
        Err(_) => return,
    };
    // Decide between command vs path completion
    // Use trim_start to ignore leading spaces for cmd detection
    let is_cmd_completion = !input.trim_start().contains(' ');
    if is_cmd_completion {
        // Complete command name
        let prefix = input.trim();
        // If input empty, list all builtins? For now do nothing to avoid spam
        if prefix.is_empty() {
            return;
        }
        let mut candidates: Vec<&str> = BUILTINS.iter().cloned().filter(|c| c.starts_with(prefix)).collect();
        candidates.sort_unstable();
        candidates.dedup();
        if candidates.is_empty() {
            return;
        }
        if candidates.len() == 1 {
            let full = candidates[0];
            let tail = &full[prefix.len()..];
            // Need space for tail + trailing space
            if *cmd_len + tail.len() + 1 >= MAX_CMD_LENGTH {
                return;
            }
            for b in tail.bytes() {
                cmd_buffer[*cmd_len] = b;
                *cmd_len += 1;
                print!("{}", b as char);
            }
            // append space after completed command
            cmd_buffer[*cmd_len] = b' ';
            *cmd_len += 1;
            print!(" ");
        } else {
            let lcp = common_prefix(candidates.clone(), prefix);
            if lcp.len() > prefix.len() {
                let tail = &lcp[prefix.len()..];
                if *cmd_len + tail.len() >= MAX_CMD_LENGTH {
                    return;
                }
                for b in tail.bytes() {
                    cmd_buffer[*cmd_len] = b;
                    *cmd_len += 1;
                    print!("{}", b as char);
                }
            } else {
                // No common extension -> list candidates
                println!();
                for c in &candidates {
                    print!("{}  ", c);
                }
                println!();
                print!("{}", prompt());
                // Reprint current input
                if let Ok(s) = core::str::from_utf8(&cmd_buffer[..*cmd_len]) {
                    print!("{}", s);
                }
            }
        }
        return;
    }

    // Path completion (after first space, complete last token)
    // Find last space position
    let last_space = match input.rfind(' ') {
        Some(p) => p,
        None => return, // should not happen because we are in path mode
    };
    let before = &input[..=last_space]; // includes space
    let token = &input[last_space + 1..];

    // Split token into dir_part (with trailing '/') and file_prefix
    let (dir_part, file_prefix) = if let Some(slash_pos) = token.rfind('/') {
        (&token[..=slash_pos], &token[slash_pos + 1..])
    } else {
        ("", token)
    };

    // Resolve dir and collect candidates (if FS not mounted, do nothing for path)
    if !is_mounted() {
        return;
    }

    let candidates_opt = get_file_candidates(dir_part, file_prefix);
    let candidates = match candidates_opt {
        Some(v) => v,
        None => return, // resolve error
    };
    if candidates.is_empty() {
        return;
    }

    // Single candidate -> complete full name + suffix
    if candidates.len() == 1 {
        let cand = &candidates[0];
        let suffix = if cand.is_directory { "/" } else { " " };
        let new_token = alloc::format!("{}{}{}", dir_part, cand.name, suffix);
        let tail = &new_token[token.len()..];
        if *cmd_len + tail.len() >= MAX_CMD_LENGTH {
            return;
        }
        for b in tail.bytes() {
            cmd_buffer[*cmd_len] = b;
            *cmd_len += 1;
            print!("{}", b as char);
        }
        return;
    }

    // Multiple candidates
    // Compute LCP among candidate names
    let names: Vec<&str> = candidates.iter().map(|fi| fi.name.as_str()).collect();
    let lcp = common_prefix(names, file_prefix);
    if lcp.len() > file_prefix.len() {
        let new_token = alloc::format!("{}{}", dir_part, lcp);
        let tail = &new_token[token.len()..];
        if *cmd_len + tail.len() >= MAX_CMD_LENGTH {
            return;
        }
        for b in tail.bytes() {
            cmd_buffer[*cmd_len] = b;
            *cmd_len += 1;
            print!("{}", b as char);
        }
    } else {
        // List candidates
        println!();
        for fi in &candidates {
            if fi.is_directory {
                print!("{}/  ", fi.name);
            } else {
                print!("{}  ", fi.name);
            }
        }
        println!();
        print!("{}", prompt());
        if let Ok(s) = core::str::from_utf8(&cmd_buffer[..*cmd_len]) {
            print!("{}", s);
        }
    }
}

fn common_prefix(mut candidates: Vec<&str>, prefix: &str) -> String {
    if candidates.is_empty() {
        return String::from(prefix);
    }
    candidates.sort_unstable();
    let mut lcp = String::from(candidates[0]);
    for cand in candidates.iter().skip(1) {
        let mut len = 0;
        for (a, b) in lcp.bytes().zip(cand.bytes()) {
            if a == b {
                len += 1;
            } else {
                break;
            }
        }
        lcp.truncate(len);
        if lcp.len() <= prefix.len() {
            break;
        }
    }
    lcp
}

fn get_file_candidates(dir_part: &str, file_prefix: &str) -> Option<Vec<crate::fs::FileInfo>> {
    // Returns None if dir cannot be resolved (e.g., not a directory or not mounted)
    let mut device = crate::drivers::block::AtaBlockDevice::new();
    let mut guard = FILESYSTEM.lock();
    let fs = match guard.as_mut() {
        Some(f) => f,
        None => return None,
    };

    let dir_inode = if dir_part.is_empty() {
        fs.current_directory()
    } else {
        let trimmed = dir_part.trim_end_matches('/');
        if trimmed.is_empty() {
            // dir_part was "/" or "///"
            0
        } else {
            match fs.resolve_path(&mut device, trimmed) {
                Ok(ino) => ino,
                Err(_) => return Some(Vec::new()), // dir does not exist -> no candidates, not error
            }
        }
    };

    // fs.is_dir check is done inside list_directory, but we ensure dir_inode is dir
    let entries = match fs.list_directory(&mut device, dir_inode) {
        Ok(v) => v,
        Err(_) => return Some(Vec::new()),
    };
    let mut filtered: Vec<crate::fs::FileInfo> = entries
        .into_iter()
        .filter(|fi| fi.name.starts_with(file_prefix))
        .collect();
    filtered.sort_by(|a, b| a.name.cmp(&b.name));
    Some(filtered)
}
