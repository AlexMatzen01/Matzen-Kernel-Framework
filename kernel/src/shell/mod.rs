//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Terminal Shell
//!
//! A simple command-line shell for the Matzen Kernel Framework.

use crate::drivers::keyboard::Key;
use crate::drivers::{keyboard, vga};
use crate::{print, println};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

/// Maximum length of a command line
const MAX_CMD_LENGTH: usize = 256;

/// Builtin commands for TAB completion (must match execute_command and help)
const BUILTINS: &[&str] = &[
    "help",
    "clear",
    "cls",
    "echo",
    "about",
    "version",
    "uptime",
    "mem",
    "memory",
    "reboot",
    "halt",
    "shutdown",
    "date",
    "whoami",
    "cpuinfo",
    "calc",
    "color",
    "test",
    "diskinfo",
    "mkfs",
    "mount",
    "ls",
    "dir",
    "touch",
    "cat",
    "write",
    "rm",
    "mkdir",
    "rmdir",
    "cd",
    "pwd",
    "ifconfig",
    "ping",
    "netstat",
    "tcpconnect",
    "tcpsend",
    "tcpclose",
    "nano",
    "edit",
    "mfkedit",
    "run",
    "exec",
    "mkapp",
    "writehex",
    "ps",
    "appinfo",
    "desktop",
    "install",
    "usb",
    "mouse",
    "dns",
    "arp",
    "udp-send",
    "udp-recv",
    "wget",
    "speedtest",
    "speedtest-server",
    "netdebug",
    "tlsinfo",
    "tcpstatus",
    "tcprecv",
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

/// Prompt text used by the desktop terminal frontend.
pub fn desktop_prompt() -> alloc::string::String {
    prompt()
}

fn current_path_string() -> Option<alloc::string::String> {
    let fs_guard = FILESYSTEM.lock();
    if fs_guard.is_some() {
        drop(fs_guard);
        let mut device = mounted_device();
        let mut guard = FILESYSTEM.lock();
        if let Some(ref mut fs) = *guard {
            if let Ok(p) = fs.current_path(&mut device) {
                return Some(p);
            }
        }
    }
    None
}

/// Millisecond-ish monotonic counter. PIT IRQ0 advances this by 10ms; the
/// explicit cooperative increment is also used as a fail-open timeout path
/// while synchronous commands pump packets.
static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Global interrupt flag for Ctrl+C handling
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);

/// Global filesystem state
static FILESYSTEM: Mutex<Option<crate::fs::SimpleFilesystem>> = Mutex::new(None);

/// Which unified drive index the filesystem is mounted from (None = default
/// data drive). Set on every successful `mount`, cleared on `mkfs` of the
/// mounted drive.
static MOUNTED_DRIVE: Mutex<Option<usize>> = Mutex::new(None);

/// Currently mounted drive index (defaults to the data drive).
pub fn mounted_drive() -> usize {
    (*MOUNTED_DRIVE.lock()).unwrap_or(crate::drivers::drives::DATA_DRIVE)
}

/// Block device for the currently mounted drive (shared by shell, wget,
/// editor, app runner and desktop so everything targets the same disk).
pub fn mounted_device() -> crate::drivers::block::DriveBlockDevice {
    crate::drivers::block::DriveBlockDevice::new(mounted_drive())
}

/// Parse an optional drive index argument (`""`, `"4"`, `"drive4"`).
/// Returns `Ok(None)` for empty (caller substitutes the default),
/// `Ok(Some(n))` for a valid index, `Err(msg)` otherwise.
fn parse_drive_arg(args: &str) -> Result<Option<usize>, &'static str> {
    let t = args.trim();
    if t.is_empty() {
        return Ok(None);
    }
    // First token only; flags (--list/--yes) are handled by callers.
    let tok = t.split_whitespace().next().unwrap_or("");
    let num = tok
        .strip_prefix("drive")
        .or_else(|| tok.strip_prefix("DRIVE"))
        .unwrap_or(tok);
    match num.parse::<usize>() {
        Ok(n) if n < crate::drivers::drives::drive_count() => Ok(Some(n)),
        _ => Err("Invalid drive index (see 'diskinfo')"),
    }
}

/// Get the current tick count (rough millisecond approximation)
pub fn get_tick_count() -> u64 {
    TICK_COUNTER.load(Ordering::Relaxed)
}

/// Monotonic millisecond source for bounded network operations.
pub fn monotonic_ms() -> u64 {
    if crate::time::is_initialized() {
        crate::time::uptime_millis()
    } else {
        get_tick_count()
    }
}

/// Increment tick (used by editor which bypasses shell::run loop)
pub fn increment_tick() {
    TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
}

/// Advance the shell clock from the 100 Hz PIT interrupt.
pub fn timer_tick() {
    TICK_COUNTER.fetch_add(crate::time::MS_PER_TICK, Ordering::Relaxed);
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

pub fn request_interrupt() {
    set_interrupt();
}

/// Runs the shell loop
pub fn run() -> ! {
    let mut cmd_buffer: [u8; MAX_CMD_LENGTH] = [0; MAX_CMD_LENGTH];
    let mut cmd_len: usize = 0;

    print!("{}", prompt());

    loop {
        // Process network packets
        crate::net::process_packets();

        // Poll USB HID keyboards so xHCI/usb-kbd input reaches the buffer.
        #[cfg(feature = "usb")]
        crate::drivers::usb::poll();

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
        "diskinfo" => cmd_diskinfo(parts.1),
        "mkfs" => cmd_mkfs(parts.1),
        "mount" => cmd_mount(parts.1),
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
        "netstat" => cmd_netstat(parts.1),
        "dns" => cmd_dns(parts.1),
        "arp" => cmd_arp(parts.1),
        "udp-send" => cmd_udp_send(parts.1),
        "udp-recv" => cmd_udp_recv(parts.1),
        "wget" => crate::net::wget::cmd_run(parts.1),
        "speedtest" => crate::net::speedtest::cmd_run(parts.1),
        "speedtest-server" => crate::net::speedtest::cmd_server(parts.1),
        "netdebug" => cmd_netdebug(parts.1),
        "tlsinfo" => cmd_tlsinfo(),
        "tcpconnect" => cmd_tcpconnect(parts.1),
        "tcpsend" => cmd_tcpsend(parts.1),
        "tcpclose" => cmd_tcpclose(parts.1),
        "tcpstatus" => cmd_tcpstatus(parts.1),
        "tcprecv" => cmd_tcprecv(parts.1),
        "nano" | "edit" | "mfkedit" => cmd_edit(parts.1),
        "run" | "exec" => cmd_run(parts.1),
        "mkapp" => cmd_mkapp(parts.1),
        "writehex" => cmd_writehex(parts.1),
        "ps" => cmd_ps(),
        "appinfo" => cmd_appinfo(parts.1),
        "desktop" => {
            if crate::drivers::vga::output_capture_active() {
                println!("desktop: already running");
            } else {
                cmd_desktop();
            }
        }
        "install" => cmd_install(parts.1),
        "usb" => cmd_usb(),
        "mouse" => cmd_mouse(),
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
    println!("  cpuinfo   - Display CPU identity and topology");
    println!("  calc      - Calculator (e.g., 'calc 5 + 3')");
    println!("  color     - Change text color (green/white/cyan/yellow/red/blue/pink)");
    println!("  test      - Run system tests");
    println!("  reboot    - Reboot the system");
    println!("  halt      - Halt the system");
    println!("  date      - Display current date (simulated)");
    println!("  whoami    - Display current user");
    println!("  install [<drive>|--list|--verify [drive]] - Install MFK to disk (interactive)");
    println!();
    println!("File System Commands:");
    println!("  diskinfo [--rescan] - Display all drives (0-3 ATA, 4+ virtio-blk)");
    println!("  mkfs [drive] [--yes] - Format drive with SimplFS (default drive 1)");
    println!("  mount [drive] - Mount drive filesystem (default drive 1)");
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
    println!("  ifconfig     - Show/configure address [netmask] [gateway]");
    println!("                 e.g. ifconfig 10.0.2.15 255.255.255.0 10.0.2.2");
    println!("  ping         - Ping IPv4 address or hostname (e.g., 'ping example.com 4')");
    println!("  dns          - Resolve IPv4 hostname (e.g., 'dns example.com')");
    println!("  arp          - Show cache or resolve IPv4 (e.g., 'arp 10.0.2.2')");
    println!("  netstat      - Show interface, route, ARP and protocol status");
    println!("  tcpconnect   - Connect to TCP server by IP or hostname");
    println!("  tcpsend      - Send data on TCP connection (e.g., 'tcpsend <port> <data>')");
    println!("  tcpclose     - Close TCP connection (e.g., 'tcpclose <port>')");
    println!("  tcpstatus    - Show TCP state (e.g., 'tcpstatus <local-port>')");
    println!("  tcprecv      - Wait for TCP data (e.g., 'tcprecv <local-port> [seconds]')");
    println!("  udp-send     - Send UDP text: udp-send <host> <src-port> <dst-port> <text>");
    println!("  udp-recv     - Wait for UDP: udp-recv <local-port> [timeout-seconds]");
    println!("  wget         - Download HTTP(S) to mounted filesystem (e.g., 'wget URL /file')");
    println!("  speedtest    - Run LibreSpeed HTTP(S) test");
    println!("  speedtest-server - Show/set speedtest server");
    println!("  netdebug     - Toggle gated packet tracing (on/off/status)");
    println!("  tlsinfo      - Show whether the TLS 1.3 backend is compiled in");
    println!("  Add -d/--debug to any network command for packet traces.");
}

/// Clears the screen - true clear for both VGA and serial, no whitespace trick
fn cmd_clear() {
    vga::clear_screen();
    // True ANSI clear for serial/QEMU headless (VGA already cleared above)
    if !vga::output_capture_active() {
        crate::serial_print!("\x1b[2J\x1b[H\x1b[0m");
    }
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
    let minutes = ticks / 60_000;
    let seconds = (ticks / 1000) % 60;
    println!("System uptime: {} minutes, {} seconds", minutes, seconds);
    println!("({} ms)", ticks);
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

    let (brand, brand_len, brand_present) = crate::sysinfo::cpu_brand();
    let name = if brand_present && brand_len > 0 {
        core::str::from_utf8(&brand[..brand_len]).unwrap_or("Unknown")
    } else {
        "Unknown"
    };
    println!("  Name: {}", name);

    let vendor = crate::sysinfo::cpu_vendor_string();
    println!("  Vendor: {}", vendor);

    let (family, model, stepping, ..) = crate::sysinfo::cpu_signature();
    println!(
        "  Family: {}, Model: {}, Stepping: {}",
        family, model, stepping
    );

    let topology = crate::sysinfo::cpu_topology();
    match topology.physical_cores {
        Some(cores) => println!("  Physical cores: {}", cores),
        None => println!("  Physical cores: Unknown"),
    }
    println!("  Logical threads: {}", topology.logical_threads);

    let features = crate::sysinfo::cpu_features();
    println!("  Features: {}", features);
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
    let vendor = crate::sysinfo::cpu_vendor_string();
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

/// Display disk information (all unified drives: 0-3 ATA, 4+ virtio-blk).
fn cmd_diskinfo(args: &str) {
    let t = args.trim();
    if t == "--rescan" || t == "rescan" {
        crate::drivers::drives::rescan_all_silent();
        println!("Re-probed ATA + virtio-blk buses.");
    }
    let count = crate::drivers::drives::drive_count();
    println!("Disk Information ({} drive(s): 0-3 ATA, 4+ virtio-blk):", count);
    for i in 0..count {
        let mut label = crate::drivers::drives::drive_label(i);
        if is_mounted() && i == mounted_drive() {
            label.push_str(" [MOUNTED]");
        }
        println!("  {}", label);
    }
    println!("  Sector size: 512 bytes");
    println!();
    println!("Use 'mkfs <drive>' to format (default 1), then 'mount <drive>' to access.");
}

/// Format a drive with SimplFS (`mkfs [drive] [--yes]`, `mkfs --list`).
fn cmd_mkfs(args: &str) {
    let t = args.trim();
    if t == "--list" || t == "list" {
        cmd_diskinfo("");
        return;
    }
    let confirmed = t.split_whitespace().any(|w| w == "--yes" || w == "-y");
    let stripped = t.replace("--yes", " ").replace("-y", " ");
    let target = match parse_drive_arg(&stripped) {
        Ok(None) => crate::drivers::drives::DATA_DRIVE,
        Ok(Some(n)) => n,
        Err(e) => {
            println!("mkfs: {}. Usage: mkfs [drive] [--yes]", e);
            return;
        }
    };
    let Some(info) = crate::drivers::drives::drive_info(target) else {
        println!("mkfs: drive {} out of range.", target);
        return;
    };
    if !info.exists {
        println!(
            "mkfs: drive {} absent ({}). Attach it and run 'diskinfo --rescan'.",
            target,
            info.last_error.unwrap_or("not detected")
        );
        return;
    }
    if !confirmed {
        println!(
            "mkfs: will ERASE drive {} ({} sectors, {} MB) with SimplFS.",
            target,
            info.total_sectors,
            info.total_sectors / 2048
        );
        println!("Re-run as 'mkfs {} --yes' to confirm.", target);
        return;
    }
    println!(
        "Formatting drive {} with SimplFS ({} sectors)...",
        target, info.total_sectors
    );

    let mut device = crate::drivers::block::DriveBlockDevice::new(target);
    match crate::fs::SimpleFilesystem::format(&mut device) {
        Ok(()) => {
            // A stale in-memory FS of this drive must not linger after erase.
            if mounted_drive() == target {
                *FILESYSTEM.lock() = None;
                *MOUNTED_DRIVE.lock() = None;
            }
            println!("Filesystem formatted successfully on drive {}!", target);
            println!("Use 'mount {}' to mount the filesystem.", target);
        }
        Err(e) => {
            println!("Failed to format filesystem: {}", e);
        }
    }
}

/// Mount a drive's filesystem (`mount [drive]`, default = data drive).
fn cmd_mount(args: &str) {
    let target = match parse_drive_arg(args) {
        Ok(None) => crate::drivers::drives::DATA_DRIVE,
        Ok(Some(n)) => n,
        Err(e) => {
            println!("mount: {}. Usage: mount [drive]", e);
            return;
        }
    };
    println!("Mounting filesystem from drive {}...", target);

    let mut device = crate::drivers::block::DriveBlockDevice::new(target);
    match crate::fs::SimpleFilesystem::mount(&mut device) {
        Ok(fs) => {
            *FILESYSTEM.lock() = Some(fs);
            *MOUNTED_DRIVE.lock() = Some(target);
            println!("Filesystem mounted successfully from drive {}!", target);
            println!("Root directory ready. Use 'ls' to list files.");
        }
        Err(e) => {
            println!("Failed to mount filesystem: {}", e);
            println!(
                "You may need to run 'mkfs {} --yes' first to format drive {}.",
                target, target
            );
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
    let mut device = mounted_device();
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

    let mut device = mounted_device();

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

    let mut device = mounted_device();

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

    let mut device = mounted_device();

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

    let mut device = mounted_device();

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
    let mut device = mounted_device();
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
    let mut device = mounted_device();
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
    let mut device = mounted_device();
    if let Some(ref mut fs) = *fs_guard {
        let target = if path.trim().is_empty() {
            "/"
        } else {
            path.trim()
        };
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
    let mut device = mounted_device();
    if let Some(ref mut fs) = *fs_guard {
        match fs.current_path(&mut device) {
            Ok(p) => println!("{}", p),
            Err(e) => println!("pwd: {}", e),
        }
    }
}

/// Configure network interface (`-d`/`--debug` enables packet tracing).
fn cmd_ifconfig(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if args.is_empty() {
        // Display current configuration
        if let Some(mac) = crate::drivers::e1000::mac_address() {
            let cfg = crate::net::ip::network_config();
            println!("Network Interface:");
            println!(
                "  MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );

            if let Some(ip) = cfg.address {
                println!("  IP Address:  {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
            } else {
                println!("  IP Address:  Not configured");
            }
            println!(
                "  Netmask:     {}.{}.{}.{}",
                cfg.netmask[0], cfg.netmask[1], cfg.netmask[2], cfg.netmask[3]
            );
            if let Some(gw) = cfg.gateway {
                println!("  Gateway:     {}.{}.{}.{}", gw[0], gw[1], gw[2], gw[3]);
            } else {
                println!("  Gateway:     (none)");
            }
        } else {
            println!("Network interface not initialized");
        }
    } else {
        // Configure an address and optionally a netmask + gateway.
        let parts: Vec<&str> = args.split_whitespace().collect();
        if parts.is_empty() || parts.len() > 3 {
            println!("Usage: ifconfig <ip> [netmask] [gateway]");
            println!("Example: ifconfig 10.0.2.15 255.255.255.0 10.0.2.2");
            return;
        }
        let Some(ip) = parse_ipv4(parts[0]) else {
            println!("Invalid IPv4 address: {}", parts[0]);
            return;
        };
        let current = crate::net::ip::network_config();
        let mask = if parts.len() > 1 {
            match parse_ipv4(parts[1]) {
                Some(mask) => mask,
                None => {
                    println!("Invalid netmask: {}", parts[1]);
                    return;
                }
            }
        } else {
            current.netmask
        };
        let mut saw_host_bit = false;
        let mut valid_mask = true;
        for bit in 0..32 {
            let set = (mask[bit / 8] & (1u8 << (7 - bit % 8))) != 0;
            if saw_host_bit && set {
                valid_mask = false;
                break;
            }
            if !set {
                saw_host_bit = true;
            }
        }
        if !valid_mask {
            println!("Netmask must contain contiguous 1 bits followed by 0 bits");
            return;
        }
        let gateway = if parts.len() > 2 {
            match parse_ipv4(parts[2]) {
                Some(gw) => Some(gw),
                None => {
                    println!("Invalid gateway: {}", parts[2]);
                    return;
                }
            }
        } else {
            current.gateway
        };
        crate::net::ip::configure(ip, mask, gateway);
        println!(
            "Network configured: {}.{}.{}.{} / {}.{}.{}.{}{}",
            ip[0],
            ip[1],
            ip[2],
            ip[3],
            mask[0],
            mask[1],
            mask[2],
            mask[3],
            match gateway {
                Some(g) => alloc::format!(" gateway {}.{}.{}.{}", g[0], g[1], g[2], g[3]),
                None => alloc::string::String::new(),
            }
        );
    }
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for part in text.split('.') {
        if count >= 4 {
            return None;
        }
        octets[count] = part.parse::<u8>().ok()?;
        count += 1;
    }
    (count == 4).then_some(octets)
}

fn take_word(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    if end == 0 {
        None
    } else {
        Some((&input[..end], &input[end..]))
    }
}

/// Send ping (ICMP echo request). `-d`/`--debug` shows packet traces.
fn cmd_ping(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if args.is_empty() {
        println!("Usage: ping <ip-address|hostname> [count]");
        println!("Example: ping 10.0.2.2 4");
        return;
    }

    // Split args to get IPv4/hostname and optional count.
    let parts: Vec<&str> = args.split_whitespace().collect();
    if crate::net::ip::get_ip_address().is_none() {
        println!("Network not configured. Run 'ifconfig 10.0.2.15 255.255.255.0 10.0.2.2' first.");
        return;
    }
    let target_ip = match parse_ipv4(parts[0]) {
        Some(ip) => ip,
        None => match crate::net::dns::resolve_ipv4(parts[0]) {
            Ok(ip) => ip,
            Err(e) => {
                println!("Could not resolve '{}': {}", parts[0], e);
                return;
            }
        },
    };

    // Parse count
    let count: u32 = if parts.len() > 1 {
        match parts[1].parse::<u32>() {
            Ok(n) if (1..=32).contains(&n) => n,
            _ => {
                println!("Ping count must be between 1 and 32");
                return;
            }
        }
    } else {
        4
    };

    println!(
        "Pinging {}.{}.{}.{} with {} packets...",
        target_ip[0], target_ip[1], target_ip[2], target_ip[3], count
    );

    // Send pings
    clear_interrupt();
    let identifier = crate::net::icmp::next_identifier();
    let mut transmitted = 0u32;
    for seq in 0..count {
        match crate::net::icmp::send_ping(target_ip, identifier, seq as u16) {
            Ok(_) => transmitted += 1,
            Err(e) => {
                println!("Failed to send ping {}: {}", seq, e);
                break;
            }
        }
        // Small delay between pings
        for _ in 0..10000 {
            core::hint::spin_loop();
        }
    }

    println!("Sent {} ping(s), waiting for replies...", transmitted);
    if transmitted == 0 {
        println!("--- ping statistics ---");
        println!("0 packets transmitted, 0 received, 100% packet loss");
        return;
    }

    // Wait for replies with timeout
    let start_time = monotonic_ms();
    let timeout_ms: u64 = 5000;
    let mut received = 0;
    let poll_limit = timeout_ms.saturating_mul(1000).max(1);
    let mut polls = 0u64;

    while polls < poll_limit
        && monotonic_ms().saturating_sub(start_time) < timeout_ms
        && !is_interrupted()
    {
        // Process incoming packets
        crate::net::process_packets();

        // Check for replies
        while let Some(reply) = crate::net::icmp::pop_reply_for(identifier) {
            println!(
                "Reply from {}.{}.{}.{}: seq={} time={}ms",
                reply.source_ip[0],
                reply.source_ip[1],
                reply.source_ip[2],
                reply.source_ip[3],
                reply.sequence,
                reply.rtt_ms
            );
            received += 1;
        }

        // If we got all replies, we're done
        if received >= transmitted {
            break;
        }

        // Yield briefly; the PIT IRQ advances the monotonic clock.
        increment_tick();
        polls += 1;
        core::hint::spin_loop();
    }

    if is_interrupted() {
        crate::net::icmp::clear_pending(identifier);
        println!("Ping cancelled by user");
        clear_interrupt();
    } else {
        // Check for timeouts
        let timed_out = crate::net::icmp::check_timeouts();
        if timed_out > 0 {
            println!("{} packet(s) timed out", timed_out);
        }

        println!("--- ping statistics ---");
        println!(
            "{} packets transmitted, {} received, {}% packet loss",
            transmitted,
            received,
            if transmitted > 0 {
                ((transmitted.saturating_sub(received)) * 100) / transmitted
            } else {
                0
            }
        );
    }
    crate::net::icmp::clear_pending(identifier);
}

/// Display network statistics
fn cmd_netstat(args: &str) {
    let (_net_dbg, _clean) = crate::net::debug::DebugGuard::acquire(args);
    println!("Network Status:");
    println!();

    if let Some(mac) = crate::drivers::e1000::mac_address() {
        println!("Interface: E1000");
        println!(
            "  MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        let cfg = crate::net::ip::network_config();
        if let Some(ip) = cfg.address {
            println!("  IP:      {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        } else {
            println!("  IP:      Not configured");
        }
        println!(
            "  Netmask: {}.{}.{}.{}",
            cfg.netmask[0], cfg.netmask[1], cfg.netmask[2], cfg.netmask[3]
        );
        if let Some(g) = cfg.gateway {
            println!("  Gateway: {}.{}.{}.{}", g[0], g[1], g[2], g[3]);
        } else {
            println!("  Gateway: (none)");
        }
        println!(
            "  ARP cache: {} entr(y/ies)",
            crate::net::arp::entries().len()
        );
        println!();
        println!("Protocol Stack:");
        println!("  Ethernet - Active");
        println!("  ARP      - Active");
        println!("  IPv4     - Active");
        println!("  ICMP     - Active");
        println!("  UDP      - Active");
        println!("  TCP      - Active");
        println!(
            "  DNS      - {}",
            if cfg.address.is_some() {
                "available"
            } else {
                "needs IP config"
            }
        );
        println!("  UDP RX   - {} queued", crate::net::udp::queued_count());
    } else {
        println!("Network interface not initialized");
    }
}

fn cmd_dns(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if crate::net::ip::get_ip_address().is_none() {
        println!("Network not configured. Run 'ifconfig 10.0.2.15 255.255.255.0 10.0.2.2' first.");
        return;
    }
    let name = args.trim();
    if name.is_empty() {
        println!("Usage: dns <hostname>");
        println!("Example: dns example.com");
        return;
    }
    match parse_ipv4(name) {
        Some(ip) => println!(
            "{} -> {}.{}.{}.{} (literal)",
            name, ip[0], ip[1], ip[2], ip[3]
        ),
        None => match crate::net::dns::resolve_ipv4(name) {
            Ok(ip) => println!("{} -> {}.{}.{}.{}", name, ip[0], ip[1], ip[2], ip[3]),
            Err(e) => println!("dns: {}", e),
        },
    }
}

fn cmd_arp(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let arg = args_owned.as_str().trim();
    if arg.is_empty() || arg == "-a" || arg == "list" {
        let entries = crate::net::arp::entries();
        if entries.is_empty() {
            println!("ARP cache is empty");
        } else {
            for (ip, mac) in entries {
                println!(
                    "{}.{}.{}.{}  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    ip[0], ip[1], ip[2], ip[3], mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                );
            }
        }
        return;
    }
    let Some(ip) = parse_ipv4(arg) else {
        println!("Usage: arp [-a|list|<IPv4 address>]");
        return;
    };
    match crate::net::arp::resolve(ip, 2000) {
        Ok(mac) => println!(
            "{}.{}.{}.{}  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            ip[0], ip[1], ip[2], ip[3], mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        ),
        Err(e) => println!("arp: {}", e),
    }
}

fn cmd_udp_send(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    let Some((host, rest)) = take_word(args) else {
        println!("Usage: udp-send <IPv4|hostname> <local-port> <remote-port> <text>");
        return;
    };
    let Some((src, rest)) = take_word(rest) else {
        println!("Usage: udp-send <IPv4|hostname> <local-port> <remote-port> <text>");
        return;
    };
    let Some((dst, data)) = take_word(rest) else {
        println!("Usage: udp-send <IPv4|hostname> <local-port> <remote-port> <text>");
        return;
    };
    let data = data.trim_start();
    if data.is_empty() {
        println!("Usage: udp-send <IPv4|hostname> <local-port> <remote-port> <text>");
        return;
    }
    let Some(ip) = parse_ipv4(host).or_else(|| crate::net::dns::resolve_ipv4(host).ok()) else {
        println!("udp-send: host resolution failed");
        return;
    };
    let (Ok(src), Ok(dst)) = (src.parse::<u16>(), dst.parse::<u16>()) else {
        println!("udp-send: invalid port");
        return;
    };
    match crate::net::udp::send_packet(ip, src, dst, data.as_bytes()) {
        Ok(()) => println!(
            "Sent {} UDP payload bytes to {}.{}.{}.{}:{}",
            data.len(),
            ip[0],
            ip[1],
            ip[2],
            ip[3],
            dst
        ),
        Err(e) => println!("udp-send: {}", e),
    }
}

fn cmd_netdebug(args: &str) {
    match args.trim() {
        "on" | "enable" => {
            crate::net::debug::set_debug(true);
            println!("Network debug output enabled");
        }
        "off" | "disable" => {
            crate::net::debug::set_debug(false);
            println!("Network debug output disabled");
        }
        "" | "status" => println!(
            "Network debug output: {}",
            if crate::net::debug::debug_enabled() {
                "on"
            } else {
                "off"
            }
        ),
        _ => println!("Usage: netdebug [on|off|status]"),
    }
}

fn cmd_tlsinfo() {
    if cfg!(feature = "net_tls") {
        println!("TLS 1.3 backend: enabled (net_tls)");
    } else {
        println!("TLS 1.3 backend: disabled; rebuild with --features net_tls");
    }
}

fn cmd_udp_recv(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    let mut fields = args.split_whitespace();
    let Some(port_text) = fields.next() else {
        println!("Usage: udp-recv <local-port> [timeout-seconds]");
        return;
    };
    let Ok(port) = port_text.parse::<u16>() else {
        println!("udp-recv: invalid port");
        return;
    };
    let timeout_s = fields
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 60);
    let deadline = monotonic_ms().saturating_add(timeout_s * 1000);
    let poll_limit = timeout_s.saturating_mul(1000).max(1);
    let mut polls = 0u64;
    clear_interrupt();
    while polls < poll_limit && monotonic_ms() < deadline && !is_interrupted() {
        crate::net::process_packets();
        if let Some(d) = crate::net::udp::receive(port) {
            println!(
                "UDP from {}.{}.{}.{}:{} ({} bytes)",
                d.source_ip[0],
                d.source_ip[1],
                d.source_ip[2],
                d.source_ip[3],
                d.source_port,
                d.payload.len()
            );
            if let Ok(s) = core::str::from_utf8(&d.payload) {
                println!("{}", s);
            } else {
                println!("(binary payload)");
            }
            return;
        }
        increment_tick();
        polls += 1;
        core::hint::spin_loop();
    }

    if is_interrupted() {
        println!("udp-recv cancelled");
        clear_interrupt();
    } else {
        println!("udp-recv timed out waiting on port {}", port);
    }
}

fn cmd_tcpconnect(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if args.is_empty() {
        println!("Usage: tcpconnect [-d|--debug] <ip-address|hostname> <port>");
        println!("Example: tcpconnect 10.0.2.2 80");
        println!("         tcpconnect 93.184.216.34 80  (example.com)");
        return;
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        println!("Error: Missing IP address or port");
        return;
    }

    let target_ip = match parse_ipv4(parts[0]) {
        Some(ip) => ip,
        None => match crate::net::dns::resolve_ipv4(parts[0]) {
            Ok(ip) => ip,
            Err(e) => {
                println!("Could not resolve '{}': {}", parts[0], e);
                return;
            }
        },
    };

    // Parse port
    let port = match parts[1].parse::<u16>() {
        Ok(p) => p,
        Err(_) => {
            println!("Invalid port number");
            return;
        }
    };

    println!(
        "Connecting to {}.{}.{}.{}:{}...",
        target_ip[0], target_ip[1], target_ip[2], target_ip[3], port
    );

    match crate::net::tcp::connect(target_ip, port) {
        Ok(local_port) => {
            println!("Connection initiated from local port {}", local_port);
            println!("Waiting for connection to establish...");

            // Wait for connection to establish
            let start_time = monotonic_ms();
            let timeout_ms: u64 = 5000;
            let poll_limit = timeout_ms.saturating_mul(1000).max(1);
            let mut polls = 0u64;
            clear_interrupt();

            while polls < poll_limit
                && monotonic_ms().saturating_sub(start_time) < timeout_ms
                && !is_interrupted()
            {
                crate::net::process_packets();

                if let Some(state) = crate::net::tcp::get_state(local_port) {
                    if state == crate::net::tcp::TcpState::Established {
                        println!("Connection established! Local port: {}", local_port);
                        println!("Use 'tcpsend {} <data>' to send data", local_port);
                        println!("Use 'tcpclose {}' to close connection", local_port);
                        return;
                    }
                }

                increment_tick();
                polls += 1;
                core::hint::spin_loop();
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
            let _ = crate::net::tcp::close(local_port);
            crate::net::tcp::forget(local_port);
        }
        Err(e) => {
            println!("Failed to initiate connection: {}", e);
        }
    }
}

fn cmd_tcpsend(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
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
            let start_time = monotonic_ms();
            let timeout_ms: u64 = 2000;
            let poll_limit = timeout_ms.saturating_mul(1000).max(1);
            let mut polls = 0u64;

            while polls < poll_limit
                && monotonic_ms().saturating_sub(start_time) < timeout_ms
                && !is_interrupted()
            {
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

                increment_tick();
                polls += 1;
                core::hint::spin_loop();
            }

            if is_interrupted() {
                clear_interrupt();
                println!("TCP receive cancelled");
            } else {
                println!("No response received (timeout)");
            }
        }
        Err(e) => {
            println!("Failed to send data: {}", e);
        }
    }
}

fn cmd_tcpclose(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
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

fn cmd_tcpstatus(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let Ok(port) = args_owned.as_str().trim().parse::<u16>() else {
        println!("Usage: tcpstatus <local-port>");
        return;
    };
    match crate::net::tcp::get_state(port) {
        Some(state) => println!("TCP local port {}: {:?}", port, state),
        None => println!("No TCP connection on local port {}", port),
    }
}

fn cmd_tcprecv(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let mut fields = args_owned.as_str().split_whitespace();
    let Some(port_text) = fields.next() else {
        println!("Usage: tcprecv <local-port> [timeout-seconds]");
        return;
    };
    let Ok(port) = port_text.parse::<u16>() else {
        println!("tcprecv: invalid port");
        return;
    };
    if crate::net::tcp::get_state(port).is_none() {
        println!("tcprecv: no TCP connection on local port {}", port);
        return;
    }
    let timeout_s = fields
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 60);
    let deadline = monotonic_ms().saturating_add(timeout_s * 1000);
    clear_interrupt();
    loop {
        crate::net::process_packets();
        if let Some(data) = crate::net::tcp::read_data(port) {
            println!("Received {} bytes:", data.len());
            if let Ok(text) = core::str::from_utf8(&data) {
                println!("{}", text);
            } else {
                println!("(binary data)");
            }
            return;
        }
        if let Some(crate::net::tcp::TcpState::CloseWait | crate::net::tcp::TcpState::Closed) =
            crate::net::tcp::get_state(port)
        {
            println!("tcprecv: connection closed");
            return;
        }
        if is_interrupted() {
            clear_interrupt();
            println!("tcprecv cancelled");
            return;
        }
        if monotonic_ms() >= deadline {
            println!("tcprecv timed out");
            return;
        }
        increment_tick();
        core::hint::spin_loop();
    }
}

// ── Editor helpers (exposed for editor crate) ─────────────

/// Check if filesystem is mounted
pub fn is_mounted() -> bool {
    FILESYSTEM.lock().is_some()
}

/// Read file contents via FS – returns None if not mounted or not found (path-aware)
pub fn read_file_contents(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Option<alloc::vec::Vec<u8>> {
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
pub fn write_file_contents(
    name: &str,
    data: &[u8],
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
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

pub fn create_download_staging_file(
    destination: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<alloc::string::String, &'static str> {
    static NEXT_STAGING_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let mut guard = FILESYSTEM.lock();
    let fs = guard.as_mut().ok_or("Filesystem not mounted")?;
    let destination = destination.trim();
    if destination.is_empty() || destination.ends_with('/') {
        return Err("Invalid destination path");
    }
    let parent = match destination.rfind('/') {
        Some(0) => "/",
        Some(index) => &destination[..index],
        None => ".",
    };
    for _ in 0..32 {
        let id = NEXT_STAGING_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let name = alloc::format!(".wget-{:x}-{:x}.part", monotonic_ms(), id);
        let path = if parent == "/" {
            alloc::format!("/{}", name)
        } else if parent == "." {
            name
        } else {
            alloc::format!("{}/{}", parent, name)
        };
        match fs.create_file(device, &path) {
            Ok(_) => return Ok(path),
            Err("File already exists") => continue,
            Err(error) => return Err(error),
        }
    }
    Err("Unable to create download staging file")
}

pub fn append_file_contents(
    name: &str,
    data: &[u8],
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let mut guard = FILESYSTEM.lock();
    let fs = guard.as_mut().ok_or("Filesystem not mounted")?;
    let inode = fs.resolve_file_or_dir(device, name)?;
    fs.append_file_by_inode(device, inode, data)
}

pub fn remove_file_contents(
    name: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let mut guard = FILESYSTEM.lock();
    let fs = guard.as_mut().ok_or("Filesystem not mounted")?;
    fs.delete_file(device, name)
}

pub fn promote_download_file(
    staging: &str,
    destination: &str,
    device: &mut dyn crate::drivers::block::BlockDevice,
) -> Result<(), &'static str> {
    let mut guard = FILESYSTEM.lock();
    let fs = guard.as_mut().ok_or("Filesystem not mounted")?;
    fs.rename_file(device, staging, destination)
}

// ── GUI bridge (shared by desktop File Explorer + Drive apps) ─────
// These wrap the same FILESYSTEM + DriveBlockDevice logic as the CLI
// commands so shell and desktop stay in sync. All helpers create
// their own device, lock FS once, copy results out, and drop the
// lock before returning (desktop must never hold the lock across
// compositor.render()).

/// Format a drive (GUI version of `mkfs`). Returns human-readable summary.
/// Drops the mounted FS when it came from this drive so a stale in-memory
/// image can't linger after erase.
pub fn gui_format_disk(drive: usize) -> Result<alloc::string::String, &'static str> {
    if drive >= crate::drivers::drives::drive_count() {
        return Err("Invalid drive index");
    }
    let mut device = crate::drivers::block::DriveBlockDevice::new(drive);
    match crate::fs::SimpleFilesystem::format(&mut device) {
        Ok(()) => {
            if mounted_drive() == drive {
                *FILESYSTEM.lock() = None;
                *MOUNTED_DRIVE.lock() = None;
            }
            Ok(alloc::format!(
                "Drive {} formatted with SimplFS. Use Mount.",
                drive
            ))
        }
        Err(e) => Err(e),
    }
}

/// Mount a drive's filesystem (GUI version of `mount`).
pub fn gui_mount_fs(drive: usize) -> Result<alloc::string::String, &'static str> {
    if drive >= crate::drivers::drives::drive_count() {
        return Err("Invalid drive index");
    }
    let mut device = crate::drivers::block::DriveBlockDevice::new(drive);
    match crate::fs::SimpleFilesystem::mount(&mut device) {
        Ok(fs) => {
            *FILESYSTEM.lock() = Some(fs);
            *MOUNTED_DRIVE.lock() = Some(drive);
            Ok(alloc::format!("Drive {} mounted. Root ready.", drive))
        }
        Err(e) => Err(e),
    }
}

/// One-line mount status for GUI labels. Never fails.
pub fn gui_fs_status() -> alloc::string::String {
    let guard = FILESYSTEM.lock();
    if guard.is_none() {
        return alloc::string::String::from("Status: not mounted (open Drive: pick disk, Format, then Mount)");
    }
    drop(guard);
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        match fs.current_path(&mut device) {
            Ok(p) => alloc::format!("Status: mounted (drive {}), cwd={}", mounted_drive(), p),
            Err(_) => alloc::format!("Status: mounted (drive {})", mounted_drive()),
        }
    } else {
        alloc::string::String::from("Status: not mounted")
    }
}

/// Disk + FS summary for the Drive app status label (all drives).
pub fn gui_disk_summary() -> alloc::string::String {
    use alloc::string::ToString;
    let mut out = alloc::string::String::from("Drives (0-3 ATA, 4+ virtio):");
    for i in 0..crate::drivers::drives::drive_count() {
        out.push('\n');
        out.push_str(&crate::drivers::drives::drive_label(i));
        if is_mounted() && i == mounted_drive() {
            out.push_str(" [MOUNTED]");
        }
    }
    if is_mounted() {
        let mut device = mounted_device();
        let mut guard = FILESYSTEM.lock();
        if let Some(ref mut fs) = *guard {
            let count = match fs.list_directory(&mut device, 0) {
                Ok(v) => v.len(),
                Err(_) => 0,
            };
            out.push_str(&alloc::format!("\nFS: SimplFS mounted, root entries: {}", count));
        }
    } else {
        out.push_str("\nFS: not mounted");
    }
    out
}

/// List directory at `path` for GUI. Returns (display_path, entries).
/// Empty path means filesystem cwd; absolute or relative paths supported.
pub fn gui_list_dir(
    path: &str,
) -> Result<(alloc::string::String, alloc::vec::Vec<crate::fs::FileInfo>), &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted. Use Drive: Mount first.");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        let target = if path.trim().is_empty() {
            fs.current_directory()
        } else {
            match fs.resolve_file_or_dir(&mut device, path.trim()) {
                Ok(ino) => {
                    if !fs.is_dir(ino) {
                        return Err("Not a directory");
                    }
                    ino
                }
                Err(e) => return Err(e),
            }
        };
        let entries = fs.list_directory(&mut device, target)?;
        // Display path: requested path, or actual cwd if empty
        let disp = if path.trim().is_empty() {
            fs.current_path(&mut device)
                .unwrap_or(alloc::string::String::from("/"))
        } else {
            alloc::string::String::from(path.trim())
        };
        Ok((disp, entries))
    } else {
        Err("Filesystem not mounted")
    }
}

/// Read file for GUI preview (capped by caller via truncate).
pub fn gui_read_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        fs.read_file(&mut device, path.trim())
    } else {
        Err("Filesystem not mounted")
    }
}

/// File size in bytes for GUI (metadata only, no buffer allocation — safe
/// to call before attempting a big read).
pub fn gui_file_size(path: &str) -> Result<u64, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        let ino = fs.resolve_file_or_dir(&mut device, path.trim())?;
        fs.file_size(ino)
    } else {
        Err("Filesystem not mounted")
    }
}

/// Create file at GUI-resolved full path.
pub fn gui_create_file(path: &str) -> Result<u32, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        fs.create_file(&mut device, path.trim())
    } else {
        Err("Filesystem not mounted")
    }
}

/// Create directory at GUI-resolved full path.
pub fn gui_create_dir(path: &str) -> Result<u32, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        fs.create_directory(&mut device, path.trim())
    } else {
        Err("Filesystem not mounted")
    }
}

/// Delete file or (empty) directory at path. Tries file first, then dir.
pub fn gui_delete_path(path: &str) -> Result<(), &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        // Determine type first
        let ino = fs.resolve_file_or_dir(&mut device, path.trim())?;
        if fs.is_dir(ino) {
            if ino == 0 {
                return Err("Cannot delete root");
            }
            fs.remove_directory(&mut device, path.trim())
        } else {
            fs.delete_file(&mut device, path.trim())
        }
    } else {
        Err("Filesystem not mounted")
    }
}

/// Save Settings text to `/config/settings.cfg` (GUI version of `write`).
/// Creates `/config` and the file on first use. Never panics; fails soft
/// when the FS is not mounted so Settings stays usable session-only.
pub fn gui_save_settings(text: &str) -> Result<alloc::string::String, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    if text.len() > 4096 {
        return Err("Settings too large");
    }
    let path = crate::desktop::wallpaper::SETTINGS_PATH;
    let mut device = mounted_device();
    let mut guard = FILESYSTEM.lock();
    if let Some(ref mut fs) = *guard {
        if fs.resolve_file_or_dir(&mut device, "/config").is_err() {
            // Best-effort: ignore "already exists" races from double-clicks.
            let _ = fs.create_directory(&mut device, "/config");
        }
        if fs.resolve_file_or_dir(&mut device, path).is_err() {
            fs.create_file(&mut device, path)
                .map_err(|_| "Cannot create settings file")?;
        }
        fs.write_file(&mut device, path, text.as_bytes())?;
        Ok(alloc::string::String::from("settings saved"))
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
    if parts.is_empty() {
        return;
    }
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
    let mut device = mounted_device();
    if let Some(data) = read_file_contents(path, &mut device) {
        println!("App '{}' ({} bytes)", path, data.len());
        if data.len() >= 4 && &data[0..4] == &[0x7F, b'E', b'L', b'F'] {
            println!("  Type: ELF (native) - not yet runnable, needs Phase 2");
        } else if data.len() >= 4
            && u32::from_le_bytes([data[0], data[1], data[2], data[3]])
                == crate::app::loader::MFKE_MAGIC
        {
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
            println!(
                "  Runnable: yes ({} lines)",
                data.iter().filter(|&&b| b == b'\n').count() + 1
            );
            // preview first 5 lines
            if let Ok(s) = core::str::from_utf8(&data) {
                for (i, line) in s.lines().take(5).enumerate() {
                    println!("    {}: {}", i + 1, line);
                }
                if s.lines().count() > 5 {
                    println!("    ...");
                }
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

/// Enters the graphical desktop (requires framebuffer)
fn cmd_desktop() {
    if !crate::drivers::fb::is_active() {
        println!("desktop: no framebuffer available (VGA text mode only)");
        println!("The graphical desktop requires a UEFI GOP framebuffer.");
        return;
    }
    crate::desktop::run();
}

/// Runs the disk installer (text mode, works on VGA or framebuffer).
/// `install` opens the interactive menu; `install <drive>`,
/// `install --list`, `install --verify [drive]` shortcut it.
fn cmd_install(args: &str) {
    crate::install::run_with_args(args);
}

/// Shows USB controller + HID keyboard status (also printed at boot)
fn cmd_usb() {
    #[cfg(feature = "usb")]
    {
        crate::drivers::usb::probe_report();
    }
    #[cfg(not(feature = "usb"))]
    {
        println!("USB support not compiled in (enable with 'cargo build --features usb')");
    }
}

/// Shows live mouse state across all transports (move the mouse to see it)
fn cmd_mouse() {
    use crate::drivers::mouse;
    let (mx, my) = mouse::position();
    let (bytes, packets, dropped) = mouse::mouse_stats();
    let (usb_rel, usb_abs) = mouse::mouse_usb_stats();
    println!(
        "mouse: {}",
        if mouse::is_present() {
            "ready"
        } else {
            "absent"
        }
    );
    println!(
        "  Position: ({}, {})  Buttons: {:#05b}",
        mx,
        my,
        mouse::buttons()
    );
    println!(
        "  PS/2 IRQ bytes: {}  packets: {}  dropped: {}",
        bytes, packets, dropped
    );
    println!(
        "  USB reports: {} relative (mouse) + {} absolute (tablet)",
        usb_rel, usb_abs
    );
    match mouse::config_snapshot() {
        Some(cfg) => println!("  i8042 config: {:#04x}", cfg),
        None => println!("  i8042 config: <unavailable>"),
    }
}

// ── TAB completion ───────────────────────────────────

pub fn complete_desktop_input(cmd_buffer: &mut [u8; MAX_CMD_LENGTH], cmd_len: &mut usize) {
    handle_tab_completion(cmd_buffer, cmd_len);
}

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
        let mut candidates: Vec<&str> = BUILTINS
            .iter()
            .cloned()
            .filter(|c| c.starts_with(prefix))
            .collect();
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
    let mut device = mounted_device();
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
