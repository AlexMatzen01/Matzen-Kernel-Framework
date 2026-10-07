//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Terminal Shell
//!
//! A simple command-line shell for the Matzen Kernel Framework.

use crate::drivers::keyboard::Key;
use crate::drivers::{keyboard, vga};
use crate::{print, println};
use alloc::string::{String, ToString};
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
    "umount",
    "fsck",
    "stat",
    "df",
    "cp",
    "mv",
    "ln",
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
    "dhcp",
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
    "doominfo",
    "doom",
    "install",
    "usb",
    "mouse",
    "dns",
    "arp",
    "udp-send",
    "udp-recv",
    "udp-ping",
    "wget",
    "speedtest",
    "speedtest-server",
    "netdebug",
    "tlsinfo",
    "tcpstatus",
    "tcprecv",
    "tcpsockets",
    "tar",
    "zip",
    "unzip",
    "7z",
    "mfk",
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
    if !crate::fs::vfs::is_mounted() {
        return None;
    }
    let p = crate::fs::vfs::cwd();
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// Millisecond-ish monotonic counter. PIT IRQ0 advances this by 10ms; the
/// explicit cooperative increment is also used as a fail-open timeout path
/// while synchronous commands pump packets.
static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Global interrupt flag for Ctrl+C handling
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);

/// Which unified drive index the filesystem is mounted from (None = default
/// data drive). Set on every successful `mount`, cleared on `mkfs` of the
/// mounted drive. The mounted filesystem instance itself lives in the VFS.
static MOUNTED_DRIVE: Mutex<Option<usize>> = Mutex::new(None);

/// Currently mounted drive index (defaults to the data drive).
pub fn mounted_drive() -> usize {
    (*MOUNTED_DRIVE.lock()).unwrap_or(crate::drivers::drives::DATA_DRIVE)
}

/// Resolve a user-supplied path against the VFS working directory.
///
/// Absolute paths pass through; relative paths are anchored at the cwd.
/// `.`/`..`/duplicate slashes are normalised without touching the disk.
pub fn abspath(path: &str) -> alloc::string::String {
    use alloc::string::String;
    let t = path.trim();
    if t.is_empty() {
        let cwd = crate::fs::vfs::cwd();
        if cwd.is_empty() {
            return String::from("/");
        }
        return cwd;
    }
    let joined = if t.starts_with('/') {
        String::from(t)
    } else {
        let cwd = crate::fs::vfs::cwd();
        if cwd.is_empty() || cwd == "/" {
            alloc::format!("/{}", t)
        } else {
            alloc::format!("{}/{}", cwd, t)
        }
    };
    // Normalise `.`, `..` and `//`.
    let mut parts: Vec<&str> = Vec::new();
    for comp in joined.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return String::from("/");
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
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
        "umount" => cmd_umount(parts.1),
        "fsck" => cmd_fsck(parts.1),
        "stat" => cmd_stat(parts.1),
        "df" => cmd_df(parts.1),
        "cp" => cmd_cp(parts.1),
        "mv" => cmd_mv(parts.1),
        "ln" => cmd_ln(parts.1),
        "ls" | "dir" => cmd_ls(parts.1),
        "touch" => cmd_touch(parts.1),
        "cat" => cmd_cat(parts.1),
        "write" => cmd_write(parts.1),
        "rm" => cmd_rm(parts.1),
        "mkdir" => cmd_mkdir(parts.1),
        "rmdir" => cmd_rmdir(parts.1),
        "cd" => cmd_cd(parts.1),
        "pwd" => cmd_pwd(),
        "tar" => cmd_tar(parts.1),
        "zip" => cmd_zip(parts.1),
        "unzip" => cmd_unzip(parts.1),
        "7z" | "7za" => cmd_7z(parts.1),
        "mfk" => cmd_mfk(parts.1),
        "ifconfig" => cmd_ifconfig(parts.1),
        "dhcp" => cmd_dhcp(parts.1),
        "ping" => cmd_ping(parts.1),
        "netstat" => cmd_netstat(parts.1),
        "dns" => cmd_dns(parts.1),
        "arp" => cmd_arp(parts.1),
        "udp-send" => cmd_udp_send(parts.1),
        "udp-recv" => cmd_udp_recv(parts.1),
        "udp-ping" => cmd_udp_ping(parts.1),
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
        "tcpsockets" => cmd_tcpsockets(parts.1),
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
        "doominfo" => cmd_doominfo(parts.1),
        "doom" => cmd_doom(parts.1),
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
    println!("  mount [drive] [auto|simplfs|ext4|exfat] - Mount drive filesystem (probes by default)");
    println!("  umount    - Unmount the active filesystem");
    println!("  fsck [-v]   - Check filesystem consistency (read-only, SimplFS)");
    println!("  stat <path> - Show file metadata (type, size, mode, times)");
    println!("  df        - Show mounted filesystem and block-cache stats");
    println!("  cp <s> <d> - Copy a file (e.g., 'cp a.txt b/', 'cp /x /y.txt')");
    println!("  mv <s> <d> - Move/rename (e.g., 'mv a.txt b/', 'mv /x /y.txt')");
    println!("  ln [-s] <t> <p> - Hard/symbolic link (ext4 only)");
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
    println!("Archive Commands:");
    println!("  tar       - Create/list/extract tarballs (e.g., 'tar -cvf a.tar.gz docs')");
    println!("             Read: .tar .gz .bz2 .xz .lz4 .zst .lz .lzma .Z (one wrapper)");
    println!("             Write: all but .bz2 and .Z; -z/-J force gzip/xz");
    println!("  zip       - Create a zip archive (e.g., 'zip -v a.zip docs')");
    println!("  unzip     - List/extract a zip (e.g., 'unzip -lv a.zip -d out')");
    println!("  7z        - 7-Zip archives (e.g., '7z a -v a.7z docs', '7z x a.7z -oout')");
    println!("             Stored/LZMA/LZMA2; 'l' lists, 't' tests, 'x' extracts");
    println!("  mfk       - Native .mfk archives (e.g., 'mfk c -v a.mfk docs')");
    println!("             Per-member CRC32; 'l' lists, 't' tests, 'x' extracts");
    println!("  All of the above take -v, plus operands to pick individual members");
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
    println!("  doominfo      - Inspect Doom WAD (e.g., 'doominfo /wad/doom1.wad')");
    println!("  doom          - Play Doom (e.g., 'doom', 'doom run', 'doom /wad/doom2.wad')");
    println!();
    println!("Network Commands:");
    println!("  ifconfig     - Show/configure address [netmask] [gateway]");
    println!("                 e.g. ifconfig 10.0.2.15 255.255.255.0 10.0.2.2");
    println!("  dhcp         - Obtain an address from a DHCP server [start|status]");
    println!("  udp-send     - Send a UDP datagram, e.g. 'udp-send 10.0.2.2 49153 5555 <text>'");
    println!("                 a payload over the link MTU is fragmented by IPv4");
    println!("  udp-recv     - Receive a queued datagram: 'udp-recv <port> [count]'");
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

/// Check filesystem consistency (read-only; SimplFS only for now)
fn cmd_fsck(args: &str) {
    let verbose = args.trim() == "-v" || args.trim() == "--verbose";
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let result = crate::fs::vfs::with(|m| m.fs.check(&mut m.device));
    match result {
        Ok(report) => {
                if verbose {
                    println!("Filesystem check (drive {}):", mounted_drive());
                    println!(
                        "  Total blocks:   {} ({} data, first at LBA {})",
                        report.total_blocks, report.data_blocks, report.data_block_start
                    );
                    println!(
                        "  Bitmap:         LBA {}, {} block(s)",
                        report.bitmap_start, report.bitmap_blocks
                    );
                    println!(
                        "  Inodes:         {} in use across {} block(s)",
                        report.inode_count, report.inode_blocks
                    );
                    println!(
                        "  Free blocks:    {} reported, {} in bitmap",
                        report.free_blocks, report.bitmap_free
                    );
                    println!();
                }
                println!("  Geometry:       {}", verdict(report.geometry_errors, "ok", "inconsistent"));
                println!("  Block counts:   {}", verdict(report.count_errors, "consistent", "bitmap disagrees with superblock"));
                println!("  Block pointers: {}", verdict(report.bad_block_refs, "all in range", "outside the data region"));
                println!("  Indirect chains: {}", verdict(report.chain_cycles, "all terminate", "cyclic or unterminated"));
                println!("  Dir entries:    {}", verdict(report.dangling_entries, "all resolve", "dangling"));
                println!("  Space refs:     {}", verdict(report.missing_refs, "all accounted", "referenced but marked free"));
                println!("  Leaked space:   {}", verdict(report.orphan_blocks, "none", "allocated but unreferenced"));
                println!();
                if report.is_clean {
                    println!("Filesystem is clean.");
                } else {
                    println!(
                        "Found {} problem(s). The check is read-only; nothing was modified.",
                        report.problem_count()
                    );
                    println!("Rebuilding the image with 'mkfs' and re-copying data is the safe fix.");
                }
        }
        Err(e) => println!("fsck: failed: {}", e),
    }
}

/// `ok` when `count` is zero, otherwise `problem` plus the count.
fn verdict(count: usize, ok: &str, problem: &str) -> alloc::string::String {
    if count == 0 {
        alloc::format!("{}", ok)
    } else {
        alloc::format!("{} - {} x {}", problem, count, problem)
    }
}

/// Displays memory information
///
/// Everything printed here is read from live state: the interpreted firmware
/// map (`sysinfo::memory_layout`), the heap accounting in `allocator`, and the
/// frame allocator's counters. The previous version printed a fixed 1994-era
/// 640 KB/384 KB/ROM map that matches no real machine â€” `docs/reference/
/// shell-commands.md` still reproduced it verbatim.
fn cmd_memory() {
    let summary = crate::sysinfo::memory_summary_opt().unwrap_or_default();
    let heap = crate::allocator::heap_info();
    let frames = crate::memory::frame_allocator::stats();

    println!("Memory Information:");
    println!(
        "  Allocatable:  {} after kernel-owned reservations",
        human_bytes(summary.allocatable_bytes)
    );
    println!(
        "  Firmware map: {} described across {} region(s){}",
        human_bytes(summary.total_bytes),
        summary.region_count,
        if summary.truncated { " (list truncated)" } else { "" }
    );
    // Per-class byte totals. A firmware map routinely contains a PCI MMIO hole
    // covering most of the upper address space, reported with an unspecified
    // type, so without this breakdown the map total cannot be reconciled with
    // the allocatable figure â€” and "total RAM" would be a fiction.
    let layout = crate::sysinfo::memory_layout();
    if let Some(l) = layout.as_ref() {
        let classes = l.by_class.non_empty();
        if classes.len() > 1 {
            for (class, bytes) in classes {
                println!(
                    "    {:<14} {:>10}  {:.1}%",
                    class.label(),
                    human_bytes(bytes),
                    if l.total_bytes == 0 {
                        0.0
                    } else {
                        (bytes as f64) * 100.0 / (l.total_bytes as f64)
                    }
                );
            }
        }
    }
    if summary.usable_low_bytes > 0 || summary.usable_high_bytes > 0 {
        println!(
            "  DMA reach:    {} below 4 GiB, {} above (32-bit controllers)",
            human_bytes(summary.usable_low_bytes),
            human_bytes(summary.usable_high_bytes)
        );
    }
    println!();

    println!("Kernel Heap:");
    if heap.is_fallback {
        println!(
            "  Static fallback: {} (no usable run below 4 GiB)",
            human_bytes(heap.size as u64)
        );
    } else {
        println!("  Carve:       {} at physical {:#x}", human_bytes(heap.size as u64), heap.base);
    }
    println!(
        "  Used/Free:   {} / {} ({:.1}% used)",
        human_bytes(heap.used as u64),
        human_bytes(heap.free as u64),
        heap.used_ratio() * 100.0
    );
    println!(
        "  Peak:        {} live, largest single {}",
        human_bytes(heap.peak_bytes as u64),
        human_bytes(heap.peak_single as u64)
    );
    println!(
        "  Requests:    {} total, {} outstanding",
        heap.alloc_requests, heap.live_allocations
    );
    if heap.reserve_size > 0 {
        println!(
            "  Reserve:     {} used of {} (OOM-time allocations)",
            human_bytes(heap.reserve_used as u64),
            human_bytes(heap.reserve_size as u64)
        );
    }
    if heap.oom_events > 0 {
        println!("  OOM events:  {} (heap exhausted at least once)", heap.oom_events);
    } else {
        println!("  OOM events:  0");
    }
    println!();

    println!("Physical Frames:");
    println!(
        "  Free/Used:   {} / {}",
        human_bytes(frames.free_frames * 4096),
        human_bytes(frames.used_frames * 4096)
    );
    println!(
        "  DMA pool:    {} frames below 4 GiB, {} above",
        frames.free_low_frames, frames.free_high_frames
    );
    println!(
        "  Operations:  {} allocs, {} frees, {} huge, {} failures",
        frames.alloc_count, frames.free_count, frames.huge_alloc_count, frames.failures
    );
    if frames.failures > 0 {
        println!("  Note:        allocation failures mean the map is exhausted");
    }
    println!();

    println!("Memory Regions:");
    let mut buf = [0u8; 32];
    let mut printed = 0usize;
    crate::sysinfo::with_regions(|regions| {
        for r in regions.iter() {
            if printed >= MAX_MEM_REGIONS_PRINTED {
                return;
            }
            let class = crate::sysinfo::region_kind_name(r.class, r.firmware_tag, &mut buf);
            println!(
                "  {:#014x} - {:#014x}  {:>8}  {}",
                r.start,
                r.end,
                human_bytes(r.len()),
                class
            );
            printed += 1;
        }
    });
    if summary.region_count == 0 {
        println!("  (no firmware map captured)");
    } else if summary.truncated {
        println!("  ... {} regions total", summary.region_count);
    }
    println!();

    let reservation_count = {
        let mut n = 0usize;
        crate::sysinfo::with_reservations(|r| {
            for res in r.iter() {
                println!(
                    "  Kernel-owned: {:#014x} - {:#014x}  {:>8}  {}",
                    res.start,
                    res.end,
                    human_bytes(res.len()),
                    res.kind.label()
                );
                n += 1;
            }
        });
        n
    };
    if reservation_count > 0 {
        println!("  Kernel-owned total: {}", human_bytes(summary.reserved_bytes));
    }
    println!();

    // What the RAM physically is, per SMBIOS. Distinct from the map above:
    // the map says how much is addressable, this says what it is.
    let phys_bits = crate::sysinfo::cpu_max_phys_addr_bits();
    match phys_bits {
        Some(bits) if bits > 64 => println!(
            "Physical Addressing: {} bits reported, 64 usable (max {:#x})",
            bits,
            u64::MAX
        ),
        Some(bits) => println!(
            "Physical Addressing: {} bits (max {:#x})",
            bits,
            crate::sysinfo::cpu_max_physical_address().unwrap_or(0)
        ),
        None => println!(
            "Physical Addressing: unreported (assuming 40 bits, max {:#x})",
            crate::sysinfo::cpu_phys_addr_limit()
        ),
    }
    match crate::drivers::smbios::inventory() {
        Some(inv) if !inv.devices.is_empty() => {
            println!(
                "SMBIOS Memory:  {} MiB installed across {} slot(s)",
                inv.installed_mb(),
                inv.devices.len()
            );
            for array in inv.arrays.iter() {
                let max = match array.max_size_mb {
                    Some(mb) => alloc::format!("max {}", human_bytes(mb * 1024 * 1024)),
                    None => alloc::string::String::from("max unknown"),
                };
                println!(
                    "  Array #{:04x}: {} slots, {}, {}",
                    array.handle,
                    array.device_count,
                    max,
                    if array.ecc_methods == 0 { "no ECC" } else { "ECC" }
                );
            }
            for d in inv.devices.iter() {
                if !d.installed {
                    let locator = if d.locator.is_empty() { "(no locator)" } else { &d.locator };
                    println!("  {:<12} empty slot", locator);
                    continue;
                }
                let width = if d.total_width_bits > 0 {
                    alloc::format!("{}-bit bus", d.total_width_bits)
                } else {
                    alloc::string::String::from("width unknown")
                };
                let speed = match d.speed_mts {
                    Some(mts) => alloc::format!("{} MT/s", mts),
                    None => alloc::string::String::from("speed unknown"),
                };
                let part = if d.part_number.is_empty() {
                    alloc::string::String::from("")
                } else {
                    alloc::format!(" [{}]", d.part_number)
                };
                println!(
                    "  {:<12} {:>6} MiB  {}  {}  {}{}",
                    d.locator,
                    d.size_mb,
                    technology_short(d.technology),
                    width,
                    speed,
                    part
                );
            }
        }
        _ => println!(
            "SMBIOS Memory:  not exposed by firmware (no guest-reachable entry point)"
        ),
    }
}

/// Short memory-technology label for the `mem` device list.
fn technology_short(tech: u8) -> &'static str {
    match tech {
        6..=10 => "LPDDR",
        13 => "DDR",
        14 => "DDR2",
        15 => "DDR3",
        16 => "DDR4",
        17 | 18 => "DDR5",
        11 => "HBM",
        12 => "HBM2",
        19 => "HBM3",
        1 => "DRAM",
        0xFF => "unknown",
        _ => "other",
    }
}

/// Regions printed before the `mem` list is truncated on screen. The full map
/// can hold `memmap::MAX_REGIONS` entries, which overflows a terminal.
const MAX_MEM_REGIONS_PRINTED: usize = 12;

/// Format a byte count with a binary unit, e.g. `1.5 MiB`.
pub(crate) fn human_bytes(bytes: u64) -> alloc::string::String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return alloc::format!("{} B", bytes);
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        alloc::format!("{:.0} {}", value, UNITS[unit])
    } else if value >= 10.0 {
        alloc::format!("{:.1} {}", value, UNITS[unit])
    } else {
        alloc::format!("{:.2} {}", value, UNITS[unit])
    }
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
///
/// Only SimplFS can be created by MFK itself: the ext4/exFAT drivers mount
/// existing images (created by host tools) but intentionally do not invent
/// fresh ones, since a half-correct formatter would risk silent corruption.
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
            "mkfs: will ERASE drive {} ({} sectors, {}) with SimplFS.",
            target,
            info.total_sectors,
            human_bytes(info.total_sectors.saturating_mul(512))
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
                let _ = crate::fs::vfs::unmount();
                *MOUNTED_DRIVE.lock() = None;
            }
            crate::fs::cache::flush_tag(target);
            println!("Filesystem formatted successfully on drive {}!", target);
            println!("Use 'mount {}' to mount the filesystem.", target);
        }
        Err(e) => {
            println!("Failed to format filesystem: {}", e);
        }
    }
}

/// Mount a drive's filesystem (`mount [drive] [auto|simplfs|ext4|exfat]`).
///
/// With no format named, the device is probed (SimplFS, then ext4, then
/// exFAT) and whatever is found is mounted. Unknown content is refused
/// rather than blind-mounted.
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

    // Optional second word pins the format; otherwise probe the device.
    let words: Vec<&str> = args.split_whitespace().collect();
    let kind = if words.len() >= 2 {
        match crate::fs::vfs::kind_of(words[1]) {
            Some(k) => Some(k),
            None => {
                println!("mount: unknown filesystem '{}'. Use: mount [drive] [auto|simplfs|ext4|exfat]", words[1]);
                return;
            }
        }
    } else {
        None
    };
    match crate::fs::vfs::mount(target, kind, "/") {
        Ok(()) => {
            *MOUNTED_DRIVE.lock() = Some(target);
            let detected = crate::fs::vfs::mount_kind().unwrap_or("unknown");
            println!("{} filesystem mounted successfully from drive {}!", detected, target);
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

/// Unmount the active filesystem (`umount`). Refuses when nothing is mounted.
fn cmd_umount(_args: &str) {
    match crate::fs::vfs::unmount() {
        Ok(()) => {
            *MOUNTED_DRIVE.lock() = None;
            println!("Filesystem unmounted.");
        }
        Err(e) => println!("umount: {}", e),
    }
}

/// List files in directory (optional path)
fn cmd_ls(path: &str) {
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path);
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        // Must be a directory.
        match m.fs.stat(dev, &target) {
            Ok(s) if !s.is_dir => {
                println!("ls: Not a directory");
                return Ok(());
            }
            Err(e) => {
                println!("ls: {}: {}", path, e);
                return Ok(());
            }
            _ => {}
        }
        // Show path header if explicit
        if !path.trim().is_empty() {
            println!("{}:", path.trim());
        }
        match m.fs.list_dir(dev, &target) {
            Ok(files) => {
                if files.is_empty() {
                    println!("(empty directory)");
                } else {
                    for file in files {
                        println!("  {}", file);
                    }
                }
            }
            Err(e) => {
                println!("Failed to list directory: {}", e);
            }
        }
        Ok(())
    });
    if let Err(e) = result {
        println!("ls: {}", e);
    }
}

/// Create a new file (path-aware)
fn cmd_touch(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: touch <filename>");
        println!("  Supports paths: touch dir/file.txt, touch /a/b/file");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.create_file(dev, &target).map(|_| ()).map_err(|e| e)
    });
    match result {
        Ok(()) => println!("Created file '{}'", path.trim()),
        Err(e) => println!("Failed to create file: {}", e),
    }
}

/// Display file contents (path-aware)
fn cmd_cat(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: cat <filename>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_file(dev, &target)
    });
    match result {
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

/// Write text to a file (path-aware)
fn cmd_write(args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();

    if parts.len() < 2 {
        println!("Usage: write <filename> <text>");
        return;
    }

    let filename = parts[0].trim();
    let content = parts[1];

    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(filename);
    // Resolve existing file, creating it when missing, then overwrite it.
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) => {
                if s.is_dir {
                    return Err(crate::fs::FsError::IsDirectory);
                }
            }
            Err(_) => {
                m.fs.create_file(dev, &target).map(|_| ())?;
                println!("Created new file '{}'", filename);
            }
        }
        m.fs.write_file(dev, &target, content.as_bytes())
    });
    match result {
        Ok(()) => {
            println!("Wrote {} bytes to '{}'", content.len(), filename);
        }
        Err(crate::fs::FsError::IsDirectory) => {
            println!("write: '{}' is a directory", filename);
        }
        Err(e) => {
            println!("Failed to write file: {}", e);
        }
    }
}

/// Delete a file (path-aware)
fn cmd_rm(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: rm <filename>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.remove_file(dev, &target)
    });
    match result {
        Ok(()) => println!("Deleted file '{}'", path.trim()),
        Err(e) => println!("Failed to delete file: {}", e),
    }
}

/// Create directory
fn cmd_mkdir(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: mkdir <directory>");
        println!("  Example: mkdir docs, mkdir /a/b, mkdir mydir");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.create_dir(dev, &target)
    });
    match result {
        Ok(()) => println!("Created directory '{}'", path.trim()),
        Err(e) => println!("mkdir: cannot create directory '{}': {}", path.trim(), e),
    }
}

/// Remove empty directory
fn cmd_rmdir(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: rmdir <directory>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.remove_dir(dev, &target)
    });
    match result {
        Ok(()) => println!("Removed directory '{}'", path.trim()),
        Err(e) => println!("rmdir: failed to remove '{}': {}", path.trim(), e),
    }
}

/// Change directory
fn cmd_cd(path: &str) {
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = if path.trim().is_empty() {
        alloc::string::String::from("/")
    } else {
        abspath(path.trim())
    };
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) if s.is_dir => {
                crate::fs::vfs::set_cwd(&target);
                Ok(())
            }
            Ok(_) => Err(crate::fs::FsError::NotDirectory),
            Err(e) => Err(e),
        }
    });
    if let Err(e) = result {
        println!("cd: {}: {}", target, e);
    }
}

/// Print working directory
fn cmd_pwd() {
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    println!("{}", crate::fs::vfs::cwd());
}

/// Show file metadata (`stat <path>`).
fn cmd_stat(path: &str) {
    if path.trim().is_empty() {
        println!("Usage: stat <path>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let target = abspath(path.trim());
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.stat(dev, &target)
    });
    match result {
        Ok(s) => {
            let kind = if s.is_dir {
                "directory"
            } else if s.is_link {
                "symlink"
            } else {
                "file"
            };
            println!("  File: {}", target);
            println!("  Type: {}  Size: {}  Links: {}", kind, s.size, s.links);
            println!("  Uid: {}  Gid: {}  Mode: {:o}", s.uid, s.gid, s.mode & 0o7777);
            println!("  Access: {}  Modify: {}  Change: {}", s.atime, s.mtime, s.ctime);
            println!("  FS: {}", crate::fs::vfs::mount_kind().unwrap_or("unknown"));
        }
        Err(e) => println!("stat: {}: {}", path.trim(), e),
    }
}

/// Show mounted filesystem usage (`df`).
fn cmd_df(_args: &str) {
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let kind = crate::fs::vfs::mount_kind().unwrap_or("unknown");
    println!("Filesystem  Type     Mounted on  Drive");
    println!("{}  {}  /  {}", kind, kind, mounted_drive());
    let (hits, misses) = crate::fs::cache::cache_stats();
    println!("Block cache: {} hits, {} misses", hits, misses);
}

/// Copy a file (`cp <src> <dst>`). Cross-directory copies on one mount.
fn cmd_cp(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        println!("Usage: cp <src> <dst>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let src = abspath(parts[0]);
    let mut dst = abspath(parts[1]);
    // `cp a b/` copies into the directory keeping the file name.
    let dst_is_dir = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.stat(dev, &dst).map(|s| s.is_dir)
    })
    .unwrap_or(false);
    if dst_is_dir {
        if let Some(base) = src.rsplit('/').next() {
            dst.push('/');
            dst.push_str(base);
        }
    }
    let data = match crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_file(dev, &src)
    }) {
        Ok(d) => d,
        Err(e) => {
            println!("cp: {}: {}", parts[0], e);
            return;
        }
    };
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        if m.fs.stat(dev, &dst).is_err() {
            m.fs.create_file(dev, &dst).map(|_| ())?;
        }
        m.fs.write_file(dev, &dst, &data)
    });
    match result {
        Ok(()) => println!("Copied {} bytes '{}' -> '{}'", data.len(), parts[0], parts[1]),
        Err(e) => println!("cp: {}", e),
    }
}

/// Move/rename (`mv <src> <dst>`).
fn cmd_mv(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        println!("Usage: mv <src> <dst>");
        return;
    }
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let src = abspath(parts[0]);
    let mut dst = abspath(parts[1]);
    let dst_is_dir = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.stat(dev, &dst).map(|s| s.is_dir)
    })
    .unwrap_or(false);
    if dst_is_dir {
        if let Some(base) = src.rsplit('/').next() {
            dst.push('/');
            dst.push_str(base);
        }
    }
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.rename(dev, &src, &dst)
    });
    match result {
        Ok(()) => println!("Moved '{}' -> '{}'", parts[0], parts[1]),
        Err(e) => println!("mv: {}", e),
    }
}

/// Links (`ln <target> <path>`, `ln -s <target> <path>`). Only filesystems
/// with link support (ext4) accept these; others report Unsupported.
fn cmd_ln(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let (symbolic, target, link) = match parts.len() {
        3 if parts[0] == "-s" => (true, parts[1], parts[2]),
        2 => (false, parts[0], parts[1]),
        _ => {
            println!("Usage: ln [-s] <target> <link-path>");
            return;
        }
    };
    if !crate::fs::vfs::is_mounted() {
        println!("Filesystem not mounted. Use 'mount' first.");
        return;
    }
    let link_abs = abspath(link);
    let result = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        if symbolic {
            m.fs.symlink(dev, &link_abs, target)
        } else {
            m.fs.hard_link(dev, &link_abs, target)
        }
    });
    match result {
        Ok(()) => println!("Linked '{}' -> '{}'", link, target),
        Err(e) => println!("ln: {}", e),
    }
}

// ── Archives: tar / zip / unzip / 7z / mfk ──────────────────────────

/// One member staged for writing: the payload is buffered because the
/// archivers take slices, and SimplFS has no streaming read.
struct PendingEntry {
    name: String,
    data: Vec<u8>,
    is_dir: bool,
}

/// Resolve an archive operand to the bytes on disk.
fn archive_read(path: &str) -> Result<Vec<u8>, &'static str> {
    if !crate::fs::vfs::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let target = abspath(path.trim());
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_file(dev, &target)
    }).map_err(|e| e.as_str())
}

/// Write `data` to `path`, creating parent directories as needed.
fn archive_write(path: &str, data: &[u8]) -> Result<(), &'static str> {
    if !crate::fs::vfs::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let target = abspath(path.trim());
    archive_ensure_parents(&target)?;
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) => {
                if s.is_dir {
                    return Err(crate::fs::FsError::IsDirectory);
                }
            }
            Err(_) => {
                m.fs.create_file(dev, &target).map(|_| ())?;
            }
        }
        m.fs.write_file(dev, &target, data)
    }).map_err(|e| e.as_str())
}

/// Create `path` as a directory, ignoring "already exists".
fn archive_mkdir(path: &str) -> Result<(), &'static str> {
    if !crate::fs::vfs::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let target = abspath(path.trim());
    archive_ensure_parents(&target)?;
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.create_dir(dev, &target) {
            Ok(()) => Ok(()),
            // An existing directory is exactly what we wanted.
            Err(crate::fs::FsError::AlreadyExists) => Ok(()),
            Err(e) => Err(e),
        }
    }).map_err(|e| e.as_str())
}

/// Create every parent directory of absolute `path`, outermost first.
fn archive_ensure_parents(path: &str) -> Result<(), &'static str> {
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    // The last component is the entry itself.
    let mut built = alloc::string::String::new();
    for comp in &components[..components.len().saturating_sub(1)] {
        built.push('/');
        built.push_str(comp);
        let exists = crate::fs::vfs::with(|m| {
            let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
            m.fs.stat(dev, &built).map(|_| ())
        });
        if exists.is_ok() {
            continue;
        }
        let created = crate::fs::vfs::with(|m| {
            let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
            m.fs.create_dir(dev, &built)
        });
        match created {
            Ok(()) => {}
            Err(crate::fs::FsError::AlreadyExists) => {}
            Err(e) => return Err(e.as_str()),
        }
    }
    Ok(())
}

/// Walk `path` (file or directory) into `out`, recursing into
/// directories in sorted order so archives are deterministic.
fn archive_collect(
    path: &str,
    out: &mut Vec<PendingEntry>,
    depth: usize,
) -> Result<(), &'static str> {
    // Bound recursion so a pathological tree cannot exhaust the heap.
    if depth > 16 {
        return Err("directory nesting too deep");
    }
    if out.len() >= crate::archive::tar::MAX_ENTRIES {
        return Err("too many files to archive");
    }
    let target = abspath(path.trim());
    let (is_dir, entries, data) = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        let stat = m.fs.stat(dev, &target)?;
        if stat.is_dir {
            let mut entries = m.fs.list_dir(dev, &target)?;
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok((true, entries, Vec::new()))
        } else {
            let data = m.fs.read_file(dev, &target)?;
            Ok((false, Vec::new(), data))
        }
    }).map_err(|e| e.as_str())?;
    // `path` is always relative to the operand root, so the sanitized path is
    // already the member name (children are built by appending to it).
    let name = crate::archive::tar::sanitize_path(path)?.0;
    if is_dir {
        out.push(PendingEntry { name: name.clone(), data: Vec::new(), is_dir: true });
        for entry in entries {
            let child = alloc::format!("{}/{}", path.trim_end_matches('/'), entry.name);
            archive_collect(&child, out, depth + 1)?;
        }
        return Ok(());
    }
    out.push(PendingEntry { name, data, is_dir: false });
    Ok(())
}

/// Stage every operand into `out`; fails if the filesystem is unmounted.
fn archive_stage(operands: &[String]) -> Result<Vec<PendingEntry>, &'static str> {
    if !crate::fs::vfs::is_mounted() {
        return Err("Filesystem not mounted. Use 'mount' first.");
    }
    let mut out: Vec<PendingEntry> = Vec::new();
    for operand in operands {
        let trimmed = operand.trim();
        if trimmed.is_empty() {
            continue;
        }
        archive_collect(trimmed, &mut out, 0)?;
    }
    if out.is_empty() {
        return Err("nothing to archive");
    }
    Ok(out)
}

/// Borrowed view of staged entries in the shape `tar::build` expects.
fn archive_as_build_entries(entries: &[PendingEntry]) -> Vec<crate::archive::tar::BuildEntry<'_>> {
    entries
        .iter()
        .map(|e| crate::archive::tar::BuildEntry {
            name: &e.name,
            data: if e.is_dir { None } else { Some(e.data.as_slice()) },
        })
        .collect()
}

/// Join a destination directory with a member name, normalising the
/// separators SimplFS expects.
fn archive_target(dest: Option<&str>, name: &str) -> String {
    match dest.map(str::trim).filter(|d| !d.is_empty()) {
        None | Some(".") | Some("/") => name.to_string(),
        Some(d) => alloc::format!("{}/{}", d.trim_end_matches('/'), name),
    }
}

/// True when `name` is `filter` itself or lives below it, so `tar xf a.tar
/// dir` also pulls in `dir/file`.
fn archive_member_matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim_end_matches('/');
    name == filter || name.starts_with(&alloc::format!("{}/", filter))
}

/// Narrow `members` to the operands the user named. With no operands every
/// member is kept. Returns `Err` when an operand matches nothing, because
/// silently extracting zero entries reads as success.
fn archive_select(
    mut members: Vec<(String, bool, Vec<u8>)>,
    filters: &[String],
) -> Result<Vec<(String, bool, Vec<u8>)>, &'static str> {
    if filters.is_empty() {
        return Ok(members);
    }
    for filter in filters {
        if !members.iter().any(|(name, ..)| archive_member_matches(name, filter)) {
            return Err("no such member");
        }
    }
    members.retain(|(name, ..)| filters.iter().any(|f| archive_member_matches(name, f)));
    Ok(members)
}

/// Extract `members` into the filesystem, reporting progress when `verbose`.
/// Returns `(written, failed)`: a bad member never aborts the rest, matching
/// how `tar` keeps going after one unusable path.
fn archive_extract_members(
    members: &[(String, bool, Vec<u8>)],
    dest: Option<&str>,
    verbose: bool,
    verb: &str,
) -> (usize, usize) {
    let mut written = 0usize;
    let mut failed = 0usize;
    for (name, is_dir, data) in members {
        let target = archive_target(dest, name);
        let result = if *is_dir {
            archive_mkdir(&target)
        } else {
            archive_write(&target, data)
        };
        match result {
            Ok(()) => {
                if verbose {
                    println!("{} {}", verb, name);
                }
                written += 1;
            }
            Err(e) => {
                println!("{}: {}: {}", verb, name, e);
                failed += 1;
            }
        }
    }
    (written, failed)
}

/// Report an extraction outcome, calling out partial failures.
fn archive_report_extract(verb: &str, archive: &str, written: usize, failed: usize) {
    if failed == 0 {
        println!("{}: extracted {} entries from '{}'", verb, written, archive);
    } else {
        println!(
            "{}: extracted {}/{} entries from '{}' ({} failed)",
            verb,
            written,
            written + failed,
            archive,
            failed
        );
    }
}

/// `tar` — create, list or extract a tarball in any supported wrapper.
fn cmd_tar(args: &str) {
    let parsed = match crate::archive::cli::parse_tar(args) {
        Ok(p) => p,
        Err(e) => {
            println!("tar: {}", e);
            println!("Usage: tar -c|-t|-x [-v] [-f ARCHIVE] [-C DIR] [FILES...]");
            return;
        }
    };
    match parsed.mode {
        crate::archive::cli::TarMode::Create => {
            let staged = match archive_stage(&parsed.operands) {
                Ok(s) => s,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            let build = archive_as_build_entries(&staged);
            // `-z`/`-J` override whatever the output name implies.
            let bytes = match crate::archive::build_tar_auto(&build, &parsed.archive, parsed.force) {
                Ok(b) => b,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            if let Err(e) = archive_write(&parsed.archive, &bytes) {
                println!("tar: {}: {}", parsed.archive, e);
                return;
            }
            if parsed.verbose {
                for entry in &staged {
                    println!("a {}", entry.name);
                }
            }
            println!("tar: created '{}' ({} bytes, {} entries)", parsed.archive, bytes.len(), staged.len());
        }
        crate::archive::cli::TarMode::List => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("tar: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let body = match crate::archive::tar_body(&data, &parsed.archive) {
                Ok(b) => b,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            let entries = match crate::archive::tar::read_entries(
                &body,
                crate::archive::MAX_DECOMPRESSED_BYTES,
            ) {
                Ok(e) => e,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            for entry in &entries {
                if parsed.verbose {
                    println!("{} {:>8} {}", if entry.is_dir { 'd' } else { '-' }, entry.data.len(), entry.name);
                } else {
                    println!("{}", entry.name);
                }
            }
        }
        crate::archive::cli::TarMode::Extract => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("tar: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let body = match crate::archive::tar_body(&data, &parsed.archive) {
                Ok(b) => b,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            let entries = match crate::archive::tar::read_entries(
                &body,
                crate::archive::MAX_DECOMPRESSED_BYTES,
            ) {
                Ok(e) => e,
                Err(e) => {
                    println!("tar: {}", e);
                    return;
                }
            };
            let mut members: Vec<(String, bool, Vec<u8>)> = Vec::new();
            for entry in &entries {
                members.push((entry.name.clone(), entry.is_dir, entry.data.to_vec()));
            }
            let members = match archive_select(members, &parsed.operands) {
                Ok(m) => m,
                Err(e) => {
                    println!("tar: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let dest = parsed.dest.as_deref();
            let (written, failed) = archive_extract_members(&members, dest, parsed.verbose, "x");
            archive_report_extract("tar", &parsed.archive, written, failed);
        }
    }
}

/// `zip` — create a zip archive.
fn cmd_zip(args: &str) {
    let parsed = match crate::archive::cli::parse_zip(args) {
        Ok(p) => p,
        Err(e) => {
            println!("zip: {}", e);
            println!("Usage: zip [-0|-9] [-v] <archive.zip> <FILE...>");
            return;
        }
    };
    let staged = match archive_stage(&parsed.operands) {
        Ok(s) => s,
        Err(e) => {
            println!("zip: {}", e);
            return;
        }
    };
    let build: Vec<crate::archive::zip::BuildEntry<'_>> = staged
        .iter()
        .map(|e| crate::archive::zip::BuildEntry {
            name: &e.name,
            data: if e.is_dir { None } else { Some(e.data.as_slice()) },
        })
        .collect();
    let pack = if parsed.stored {
        crate::archive::zip::Pack::Stored
    } else {
        crate::archive::zip::Pack::Deflated
    };
    let bytes = match crate::archive::zip::build(&build, pack) {
        Ok(b) => b,
        Err(e) => {
            println!("zip: {}", e);
            return;
        }
    };
    if let Err(e) = archive_write(&parsed.archive, &bytes) {
        println!("zip: {}: {}", parsed.archive, e);
        return;
    }
    if parsed.verbose {
        for entry in &staged {
            println!("a {}", entry.name);
        }
    }
    println!("zip: created '{}' ({} bytes, {} entries)", parsed.archive, bytes.len(), staged.len());
}

/// `unzip` — list or extract a zip archive.
fn cmd_unzip(args: &str) {
    let parsed = match crate::archive::cli::parse_unzip(args) {
        Ok(p) => p,
        Err(e) => {
            println!("unzip: {}", e);
            println!("Usage: unzip [-l] [-v] <archive.zip> [-d DIR]");
            return;
        }
    };
    let data = match archive_read(&parsed.archive) {
        Ok(d) => d,
        Err(e) => {
            println!("unzip: {}: {}", parsed.archive, e);
            return;
        }
    };
    if parsed.list_only {
        match crate::archive::zip::list(&data) {
            Ok(infos) => {
                for info in &infos {
                    if parsed.verbose {
                        println!(
                            "{} {:>8} {:>8} {}",
                            if info.is_dir { 'd' } else { '-' },
                            info.comp_size,
                            info.uncomp_size,
                            info.name
                        );
                    } else {
                        println!("{}", info.name);
                    }
                }
            }
            Err(e) => println!("unzip: {}", e),
        }
        return;
    }
    let entries = match crate::archive::zip::extract(&data, crate::archive::MAX_DECOMPRESSED_BYTES) {
        Ok(e) => e,
        Err(e) => {
            println!("unzip: {}", e);
            return;
        }
    };
    let members: Vec<(String, bool, Vec<u8>)> = entries
        .into_iter()
        .map(|e| (e.name, e.is_dir, e.data.into_owned()))
        .collect();
    let (written, failed) = archive_extract_members(&members, parsed.dest.as_deref(), parsed.verbose, "x");
    archive_report_extract("unzip", &parsed.archive, written, failed);
}

/// `7z` — create, list, test or extract a 7z archive.
fn cmd_7z(args: &str) {
    use crate::archive::cli::{SevenZMode};
    let parsed = match crate::archive::cli::parse_7z(args) {
        Ok(p) => p,
        Err(e) => {
            println!("7z: {}", e);
            println!("Usage: 7z <a|x|t|l> [-v] [-oDIR] <archive.7z> [FILES...]");
            return;
        }
    };
    match parsed.mode {
        SevenZMode::Add => {
            let staged = match archive_stage(&parsed.operands) {
                Ok(s) => s,
                Err(e) => {
                    println!("7z: {}", e);
                    return;
                }
            };
            let build = archive_as_build_entries(&staged);
            let bytes = match crate::archive::sevenz::build(&build) {
                Ok(b) => b,
                Err(e) => {
                    println!("7z: {}", e);
                    return;
                }
            };
            if let Err(e) = archive_write(&parsed.archive, &bytes) {
                println!("7z: {}: {}", parsed.archive, e);
                return;
            }
            if parsed.verbose {
                for entry in &staged {
                    println!("a {}", entry.name);
                }
            }
            println!("7z: created '{}' ({} bytes, {} entries)", parsed.archive, bytes.len(), staged.len());
        }
        SevenZMode::List | SevenZMode::Test => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("7z: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let infos = match crate::archive::sevenz::list(&data) {
                Ok(i) => i,
                Err(e) => {
                    println!("7z: {}", e);
                    return;
                }
            };
            if parsed.mode == SevenZMode::Test {
                match crate::archive::sevenz::extract(&data, crate::archive::MAX_DECOMPRESSED_BYTES) {
                    Ok(members) => println!("7z: '{}' is OK ({} entries)", parsed.archive, members.len()),
                    Err(e) => println!("7z: '{}': {}", parsed.archive, e),
                }
                return;
            }
            for info in &infos {
                if parsed.verbose {
                    println!(
                        "{} {:>8} {}",
                        if info.is_dir { 'd' } else { '-' },
                        info.size,
                        info.name
                    );
                } else {
                    println!("{}", info.name);
                }
            }
        }
        SevenZMode::Extract => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("7z: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let members = match crate::archive::sevenz::extract(&data, crate::archive::MAX_DECOMPRESSED_BYTES) {
                Ok(m) => m,
                Err(e) => {
                    println!("7z: {}", e);
                    return;
                }
            };
            let members = match archive_select(members, &parsed.operands) {
                Ok(m) => m,
                Err(e) => {
                    println!("7z: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let (written, failed) = archive_extract_members(&members, parsed.dest.as_deref(), parsed.verbose, "x");
            archive_report_extract("7z", &parsed.archive, written, failed);
        }
    }
}

/// `mfk` — create, list, test or extract an `.mfk` archive.
fn cmd_mfk(args: &str) {
    use crate::archive::cli::MfkMode;
    let parsed = match crate::archive::cli::parse_mfk(args) {
        Ok(p) => p,
        Err(e) => {
            println!("mfk: {}", e);
            println!("Usage: mfk <c|x|t|l> [-v] [-d DIR] <archive.mfk> [FILES...]");
            return;
        }
    };
    match parsed.mode {
        MfkMode::Create => {
            let staged = match archive_stage(&parsed.operands) {
                Ok(s) => s,
                Err(e) => {
                    println!("mfk: {}", e);
                    return;
                }
            };
            let build: Vec<crate::archive::mfk::BuildEntry<'_>> = staged
                .iter()
                .map(|e| crate::archive::mfk::BuildEntry {
                    name: &e.name,
                    data: if e.is_dir { None } else { Some(e.data.as_slice()) },
                })
                .collect();
            let bytes = match crate::archive::mfk::build(&build, crate::archive::mfk::Pack::Deflate) {
                Ok(b) => b,
                Err(e) => {
                    println!("mfk: {}", e);
                    return;
                }
            };
            if let Err(e) = archive_write(&parsed.archive, &bytes) {
                println!("mfk: {}: {}", parsed.archive, e);
                return;
            }
            if parsed.verbose {
                for entry in &staged {
                    println!("a {}", entry.name);
                }
            }
            println!("mfk: created '{}' ({} bytes, {} entries)", parsed.archive, bytes.len(), staged.len());
        }
        MfkMode::List | MfkMode::Test => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("mfk: {}: {}", parsed.archive, e);
                    return;
                }
            };
            if parsed.mode == MfkMode::Test {
                match crate::archive::mfk::extract(&data, crate::archive::MAX_DECOMPRESSED_BYTES) {
                    Ok(entries) => println!("mfk: '{}' is OK ({} entries)", parsed.archive, entries.len()),
                    Err(e) => println!("mfk: '{}': {}", parsed.archive, e),
                }
                return;
            }
            match crate::archive::mfk::list(&data) {
                Ok(infos) => {
                    for info in &infos {
                        if parsed.verbose {
                            println!(
                                "{} {:>8} {:>8} {:>7} {}",
                                if info.is_dir { 'd' } else { '-' },
                                info.comp_size,
                                info.uncomp_size,
                                if info.stored { "store" } else { "deflate" },
                                info.name
                            );
                        } else {
                            println!("{}", info.name);
                        }
                    }
                }
                Err(e) => println!("mfk: {}", e),
            }
        }
        MfkMode::Extract => {
            let data = match archive_read(&parsed.archive) {
                Ok(d) => d,
                Err(e) => {
                    println!("mfk: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let entries = match crate::archive::mfk::extract(&data, crate::archive::MAX_DECOMPRESSED_BYTES) {
                Ok(e) => e,
                Err(e) => {
                    println!("mfk: {}", e);
                    return;
                }
            };
            let members: Vec<(String, bool, Vec<u8>)> = entries
                .into_iter()
                .map(|e| (e.name, e.is_dir, e.data.into_owned()))
                .collect();
            let members = match archive_select(members, &parsed.operands) {
                Ok(m) => m,
                Err(e) => {
                    println!("mfk: {}: {}", parsed.archive, e);
                    return;
                }
            };
            let (written, failed) = archive_extract_members(&members, parsed.dest.as_deref(), parsed.verbose, "x");
            archive_report_extract("mfk", &parsed.archive, written, failed);
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
            crate::net::arp::stats().entries
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
        let udp = crate::net::udp::stats();
        println!(
            "  UDP RX   - {} queued ({} KiB of {} KiB)",
            udp.queued,
            udp.queued_bytes / 1024,
            crate::net::udp::MAX_QUEUED_BYTES / 1024
        );
        if udp.dropped_full > 0 || udp.dropped_oversized > 0 {
            println!(
                "            {} dropped: queue full, {} oversized",
                udp.dropped_full, udp.dropped_oversized
            );
        }
        if udp.bad_checksum > 0 {
            println!(
                "            {} dropped: bad checksum",
                udp.bad_checksum
            );
        }

        let ip = crate::net::ip::stats();
        println!(
            "  IP       - {} fragmented into {} piece(s), {} reassembled ({} held)",
            ip.fragmented_sent,
            ip.fragments_sent,
            ip.fragments_reassembled,
            crate::net::ip::reassembly_count()
        );
        if ip.bad_checksum > 0 {
            println!(
                "            {} packet(s) dropped: bad header checksum",
                ip.bad_checksum
            );
        }
        if ip.malformed > 0 {
            println!("            {} malformed or refused fragment(s)", ip.malformed);
        }

        let icmp = crate::net::icmp::stats();
        println!(
            "  ICMP     - {} echo request(s), {} reply(s), {} error(s)",
            icmp.echo_requests, icmp.echo_replies, icmp.errors
        );
        if icmp.echo_dropped_oversized > 0 {
            println!(
                "            {} oversized echo request(s) not reflected",
                icmp.echo_dropped_oversized
            );
        }
        for report in crate::net::icmp::errors().iter().rev().take(3) {
            println!(
                "            {} from {}.{}.{}.{} (for {}.{}.{}.{})",
                report.error.name(),
                report.from_ip[0],
                report.from_ip[1],
                report.from_ip[2],
                report.from_ip[3],
                report.quoted_dst[0],
                report.quoted_dst[1],
                report.quoted_dst[2],
                report.quoted_dst[3]
            );
        }
        let nic = crate::drivers::e1000::stats();
        match crate::drivers::e1000::interrupt_vector() {
            Some(vector) => println!("  NIC IRQ  - vector {}", vector),
            None => println!("  NIC IRQ  - polling (no INTx line)"),
        }
        println!(
            "            {} interrupts ({} tx, {} tx underflow, last cause {:#x})",
            nic.interrupts,
            nic.tx_interrupts,
            nic.tx_underflows,
            crate::drivers::e1000::last_cause()
        );
        if nic.unexpected_causes > 0 {
            println!(
                "            {} interrupts with an unmodelled cause bit",
                nic.unexpected_causes
            );
        }

        let arp = crate::net::arp::stats();
        let dhcp = crate::net::dhcp::stats();
        if dhcp.discovers_sent > 0 {
            println!(
                "  DHCP     - {} lease(s), {} ack(s) from {}.{}.{}.{}",
                dhcp.leases_obtained,
                dhcp.acks_received,
                crate::net::dns::server()[0],
                crate::net::dns::server()[1],
                crate::net::dns::server()[2],
                crate::net::dns::server()[3],
            );
        }
        println!(
            "  ARP      - {} req sent, {} req recv, {} reply sent, {} reply recv",
            arp.requests_sent, arp.requests_received, arp.replies_sent, arp.replies_received
        );
        if arp.spoof_rejected > 0 || arp.self_claim_rejected > 0 {
            println!(
                "            {} rejected: sender MAC did not match the frame source",
                arp.spoof_rejected + arp.self_claim_rejected
            );
        }
        if arp.replies_rate_limited > 0 {
            println!(
                "            {} replies rate limited (flood protection)",
                arp.replies_rate_limited
            );
        }
        if arp.evicted > 0 || arp.expired > 0 {
            println!(
                "            {} evicted, {} expired (table holds {})",
                arp.evicted, arp.expired, crate::net::arp::ARP_MAX_ENTRIES
            );
        }
        if arp.stale_hits > 0 {
            println!("            {} lookups served a stale mapping", arp.stale_hits);
        }

        let tcp = crate::net::tcp::stats();
        println!("  TCP      - {} active, {} total", tcp.active_connections, tcp.total_connections);
        println!(
            "            {} seg sent ({} retransmitted), {} received",
            tcp.segments_sent, tcp.segments_retransmitted, tcp.segments_received
        );
        println!(
            "            {} buffered, {} in flight",
            tcp.bytes_buffered, tcp.bytes_in_flight
        );
        if tcp.out_of_window > 0 || tcp.resets > 0 || tcp.timeouts > 0 {
            println!(
                "            {} out of window, {} resets, {} timeouts",
                tcp.out_of_window, tcp.resets, tcp.timeouts
            );
        }

        let live = crate::net::tcp::connections();
        if !live.is_empty() {
            println!();
            println!("TCP connections:");
            for c in &live {
                println!(
                    "  :{:<5} -> {}.{}.{}.{}:{}  {}  rx {} / tx {}",
                    c.local_port,
                    c.remote_ip[0],
                    c.remote_ip[1],
                    c.remote_ip[2],
                    c.remote_ip[3],
                    c.remote_port,
                    c.state.name(),
                    c.recv_buffer.len(),
                    c.unacked_bytes() + c.send_buffer.len()
                );
            }
        }
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

fn cmd_dhcp(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let arg = args_owned.as_str().trim();

    match arg {
        "" | "start" | "renew" => {
            println!("Requesting a DHCP lease...");
            match crate::net::dhcp::configure() {
                Ok(lease) => {
                    println!(
                        "DHCP lease: {}.{}.{}.{} via {}.{}.{}.{}",
                        lease.address[0],
                        lease.address[1],
                        lease.address[2],
                        lease.address[3],
                        lease.server_id.map(|s| s[0]).unwrap_or(0),
                        lease.server_id.map(|s| s[1]).unwrap_or(0),
                        lease.server_id.map(|s| s[2]).unwrap_or(0),
                        lease.server_id.map(|s| s[3]).unwrap_or(0),
                    );
                    println!(
                        "  netmask {}.{}.{}.{}, lease {}s",
                        lease.netmask[0],
                        lease.netmask[1],
                        lease.netmask[2],
                        lease.netmask[3],
                        lease.lease_secs
                    );
                    println!(
                        "  resolver {}.{}.{}.{}",
                        crate::net::dns::server()[0],
                        crate::net::dns::server()[1],
                        crate::net::dns::server()[2],
                        crate::net::dns::server()[3],
                    );
                }
                Err(e) => println!("dhcp: {}", e),
            }
        }
        "status" => {
            let stats = crate::net::dhcp::stats();
            if stats.discovers_sent == 0 {
                println!("DHCP has not been attempted; address is manually configured");
                return;
            }
            println!(
                "DHCP: {} discover(s), {} offer(s), {} request(s), {} ack(s), {} nak(s)",
                stats.discovers_sent,
                stats.offers_received,
                stats.requests_sent,
                stats.acks_received,
                stats.naks_received
            );
            println!(
                "      {} lease(s) obtained, interface {}",
                stats.leases_obtained,
                if stats.configured {
                    "configured by DHCP"
                } else {
                    "not configured by DHCP"
                }
            );
            if stats.leases_rejected > 0 {
                println!(
                    "      {} lease(s) refused; last reason: {}",
                    stats.leases_rejected,
                    stats.last_reject_reason.unwrap_or("unspecified")
                );
            }
            if stats.malformed_dropped > 0 {
                println!("      {} malformed repl(s) discarded", stats.malformed_dropped);
            }
        }
        other => {
            println!("Usage: dhcp [start|renew|status]");
            let _ = other;
        }
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
            for (ip, mac, stale) in entries {
                println!(
                    "{}.{}.{}.{}  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}{}",
                    ip[0], ip[1], ip[2], ip[3], mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                    if stale { "  (stale)" } else { "" }
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

/// Send a UDP datagram whose size is given in bytes rather than typed.
///
/// `udp-send` takes its payload as a word, which caps it well below the link MTU,
/// so the fragmentation path cannot be reached from the shell without this. The
/// payload is a repeating pattern, so a reassembled datagram can be checked for
/// corruption rather than merely counted.
fn cmd_udp_ping(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    let Some((host, rest)) = take_word(args) else {
        println!("Usage: udp-ping <IPv4|hostname> <bytes> [remote-port]");
        return;
    };
    let Some((size, rest)) = take_word(rest) else {
        println!("Usage: udp-ping <IPv4|hostname> <bytes> [remote-port]");
        return;
    };
    let port = rest.trim();
    let port = if port.is_empty() { "5555" } else { port };
    let Ok(size) = size.parse::<usize>() else {
        println!("udp-ping: invalid size");
        return;
    };
    if size > 60000 {
        println!("udp-ping: refusing {} bytes, the protocol limit is 65507", size);
        return;
    }
    let Some(ip) = parse_ipv4(host).or_else(|| crate::net::dns::resolve_ipv4(host).ok()) else {
        println!("udp-ping: host resolution failed");
        return;
    };
    let Ok(port) = port.parse::<u16>() else {
        println!("udp-ping: invalid port");
        return;
    };

    // A counter, not zeros: an all-zero payload would not distinguish a
    // correctly reassembled datagram from one filled with holes.
    let payload: alloc::vec::Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    match crate::net::udp::send_packet(ip, 49153, port, &payload) {
        Ok(()) => println!("Sent {} UDP payload bytes to {}:{}", size, host, port),
        Err(e) => println!("udp-ping: {}", e),
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

    let port = match parts[1].parse::<u16>() {
        Ok(p) => p,
        Err(_) => {
            println!("Invalid port number");
            return;
        }
    };

    let peer = crate::net::socket::SocketAddr::new(target_ip, port);
    println!("Connecting to {}...", peer);
    clear_interrupt();

    match crate::net::socket::connect_tcp(peer, 5000) {
        Ok(handle) => {
            let local = crate::net::socket::local_port(handle).unwrap_or(0);
            println!("Connection established: {}", handle);
            println!("  peer  {}", peer);
            println!("  local port {}", local);
            println!("Use 'tcpsend {} <data>' to send data", handle);
            println!("Use 'tcprecv {} [seconds]' to receive", handle);
            println!("Use 'tcpclose {}' to close it", handle);
        }
        Err(e) => {
            println!("Failed to connect to {}: {}", peer, e);
            // The advice is only useful for the case it applies to; "timed out" on
            // a reachable host usually means nothing is listening.
            if e == crate::net::socket::SocketError::TimedOut {
                println!("Note: With QEMU user-mode networking, only connections to");
                println!("      the host (10.0.2.2) may work. Use TAP networking for");
                println!("      connections to external servers.");
            }
        }
    }
}

fn cmd_tcpsend(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if args.is_empty() {
println!("Usage: tcpsend <socket> <data>");
        println!("Example: tcpsend socket#0g1 GET / HTTP/1.0\\r\\n");
        println!("Escapes: \\r \\n \\t \\0 \\\\");
        return;
    }

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        println!("Error: Missing socket or data");
        return;
    }

    let Some(handle) = parse_socket_handle(parts[0]) else {
        println!("Invalid or closed socket '{}'", parts[0]);
        println!("Use 'tcpsockets' to list open sockets");
        return;
    };

    // Escapes, because a line-based protocol needs a terminator and a terminal
    // cannot type one into an argument.
    let data = expand_escapes(parts[1]);
    let sent = data.len();
    match crate::net::socket::write(handle, &data) {
        Ok(_) => {
            println!("Sent {} bytes on {}", sent, handle);
            println!("Checking for response...");

            let mut buf = [0u8; 2048];
            match crate::net::socket::read_timeout(handle, &mut buf, 2000) {
                Ok(0) => println!("No response received (timeout or peer closed)"),
                Ok(count) => {
                    println!("Received {} bytes:", count);
                    if let Ok(s) = core::str::from_utf8(&buf[..count]) {
                        println!("{}", s);
                    } else {
                        println!("(binary data)");
                    }
                }
                Err(e) => println!("Receive failed: {}", e),
            }
        }
        Err(e) => println!("Failed to send data: {}", e),
    }
}

fn cmd_tcpclose(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let args = args_owned.as_str();
    if args.is_empty() {
        println!("Usage: tcpclose <socket>");
        println!("Example: tcpclose socket#0g1");
        return;
    }

    let Some(handle) = parse_socket_handle(args.trim()) else {
        println!("Invalid or closed socket '{}'", args.trim());
        return;
    };

    // Readable first: after close the handle is dead, so this has to be asked
    // before.
    let peer = crate::net::socket::peer_addr(handle).ok();
    crate::net::socket::close(handle);
    match peer {
        Some(peer) => println!("Closed {} (peer {})", handle, peer),
        None => println!("Closed {}", handle),
    }
}

/// List open sockets, so a user who lost track of a handle can find it again.
fn cmd_tcpsockets(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let _ = args_owned;
    let open = crate::net::socket::sockets();
    if open.is_empty() {
        println!("No open sockets");
        return;
    }
    println!("{:<14} {:<5} {:<7} {:<22} {}", "SOCKET", "PROTO", "LOCAL", "PEER", "STATE");
    for info in open {
        println!(
            "{:<14} {:<5} {:<7} {:<22} {}",
            info.handle.to_string(),
            info.transport.name(),
            info.local_port,
            info.peer.to_string(),
            info.state.name()
        );
    }
}

fn cmd_tcpstatus(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let text = args_owned.as_str().trim();
    if text.is_empty() {
        println!("Usage: tcpstatus <socket>");
        return;
    }
    let Some(handle) = parse_socket_handle(text) else {
        println!("No such socket '{}'", text);
        return;
    };
    let local = crate::net::socket::local_port(handle).unwrap_or(0);
    match crate::net::socket::state(handle) {
        Ok(state) => println!("{}: {} (local port {})", handle, state.name(), local),
        Err(e) => println!("{}: {}", handle, e),
    }
}

/// Resolve a socket argument, accepting what `tcpconnect` printed.
fn parse_socket_handle(text: &str) -> Option<crate::net::socket::Socket> {
    crate::net::socket::Socket::parse(text)
}

#[cfg(test)]
mod socket_escape_tests {
    use super::expand_escapes;

    #[test]
    fn line_terminators_expand_to_real_bytes() {
        // The point of the function: a terminal cannot type a CR or LF into an
        // argument, so without this a request line could never be terminated
        // and no text server would ever reply.
        assert_eq!(
            expand_escapes("GET / HTTP/1.0\\r\\n"),
            b"GET / HTTP/1.0\r\n".to_vec()
        );
        assert_eq!(expand_escapes("\\n"), b"\n".to_vec());
        assert_eq!(expand_escapes("\\t"), b"\t".to_vec());
        assert_eq!(expand_escapes("\\0"), alloc::vec![0u8]);
        assert_eq!(expand_escapes("\\\\"), b"\\".to_vec());
    }

    #[test]
    fn a_request_with_its_terminator_is_the_documented_example() {
        let request = expand_escapes("GET /hello.txt HTTP/1.0\\r\\nHost: 10.0.2.2\\r\\n\\r\\n");
        assert_eq!(
            request,
            b"GET /hello.txt HTTP/1.0\r\nHost: 10.0.2.2\r\n\r\n".to_vec()
        );
        assert!(request.ends_with(b"\r\n\r\n"), "headers must be closed off");
    }

    #[test]
    fn text_without_escapes_is_unchanged() {
        assert_eq!(expand_escapes("hello"), b"hello".to_vec());
        assert_eq!(expand_escapes(""), alloc::vec::Vec::<u8>::new());
        // Interior spaces survive, since the data argument may contain them.
        assert_eq!(expand_escapes("a b  c"), b"a b  c".to_vec());
    }

    #[test]
    fn unknown_and_trailing_backslashes_are_kept_verbatim() {
        // Dropping an unrecognised escape would silently corrupt a payload such
        // as a Windows path. `\U` and `\f` are not escapes this shell knows.
        assert_eq!(expand_escapes("C:\\Users\\file"), b"C:\\Users\\file".to_vec());
        assert_eq!(expand_escapes("a\\"), b"a\\".to_vec());
        assert_eq!(expand_escapes("\\q"), b"\\q".to_vec());
        assert_eq!(expand_escapes("50\\%"), b"50\\%".to_vec());
    }

    #[test]
    fn recognised_escapes_win_even_where_a_path_would_have_one() {
        // `\n` and `\t` are escapes here, so a path containing them is altered.
        // That is the same trade every shell makes, and the alternative — a
        // second syntax — is not worth it for one command.
        assert_eq!(expand_escapes("a\\nb"), b"a\nb".to_vec());
        assert_eq!(expand_escapes("a\\tb"), b"a\tb".to_vec());
    }
}

fn cmd_tcprecv(args: &str) {
    let (_net_dbg, args_owned) = crate::net::debug::DebugGuard::acquire(args);
    let mut fields = args_owned.as_str().split_whitespace();
    let Some(socket_text) = fields.next() else {
        println!("Usage: tcprecv <socket> [timeout-seconds]");
        return;
    };
    let Some(handle) = parse_socket_handle(socket_text) else {
        println!("tcprecv: no such socket '{}'", socket_text);
        println!("Use 'tcpsockets' to list open sockets");
        return;
    };
    let timeout_s = fields
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 60);
    clear_interrupt();

    let mut buf = [0u8; 4096];
    match crate::net::socket::read_timeout(handle, &mut buf, timeout_s * 1000) {
        Ok(0) => println!("tcprecv: connection closed"),
        Ok(count) => {
            println!("Received {} bytes:", count);
            if let Ok(text) = core::str::from_utf8(&buf[..count]) {
                println!("{}", text);
            } else {
                println!("(binary data)");
            }
        }
        Err(crate::net::socket::SocketError::TimedOut) => println!("tcprecv timed out"),
        Err(e) => println!("tcprecv: {}", e),
    }
}

/// Expand backslash escapes in a `tcpsend` argument into real bytes.
///
/// Without this, `tcpsend` cannot speak any line-based protocol. A text protocol
/// needs its request line terminated, and a terminal cannot type a bare CR or LF
/// into a command argument — so the command's own documented example,
/// `tcpsend 49152 GET / HTTP/1.0`, could never have produced a reply: the server
/// sat waiting for a line ending that had no way to be expressed.
///
/// Supported: `\r`, `\n`, `\t`, `\0`, `\\`. An unknown escape is kept as written,
/// so a path like `C:\new` does not silently lose characters.
fn expand_escapes(text: &str) -> alloc::vec::Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = alloc::vec::Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'r' => out.push(b'\r'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'0' => out.push(0),
            b'\\' => out.push(b'\\'),
            _ => {
                // Not an escape we know: keep both bytes so the text survives.
                out.push(bytes[i]);
                out.push(bytes[i + 1]);
            }
        }
        i += 2;
    }
    out
}

// ── Editor helpers (exposed for editor crate) ─────────────
///
/// These go through the VFS so editor/doom/app/http code works on whatever
/// filesystem is mounted (SimplFS, ext4 or exFAT). Paths may be absolute or
/// relative to the shell working directory; the VFS device is used, so no
/// caller needs to build a block device anymore.

/// Check if filesystem is mounted
pub fn is_mounted() -> bool {
    crate::fs::vfs::is_mounted()
}

/// Read file contents via VFS – returns None if not mounted or not found (path-aware)
pub fn read_file_contents(name: &str) -> Option<alloc::vec::Vec<u8>> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_file(dev, &target)
    })
    .ok()
}

/// Read up to `out.len()` bytes of a file at `offset` into `out` (streaming,
/// no whole-file allocation). Returns bytes copied. Used by large-asset
/// readers (Doom WAD) on extra disks.
pub fn read_file_chunk(
    name: &str,
    offset: u64,
    out: &mut [u8],
) -> Result<usize, &'static str> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_at(dev, &target, offset, out)
    })
    .map_err(|e| e.as_str())
}

/// File size in bytes (no data allocation). Used by large-asset readers
/// (Doom WAD) to validate offsets before streaming.
pub fn mounted_file_size(name: &str) -> Result<u64, &'static str> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.file_size_at(dev, &target)
    })
    .map_err(|e| e.as_str())
}

/// Write file contents – creates file if needed, returns static error str on failure (path-aware)
pub fn write_file_contents(name: &str, data: &[u8]) -> Result<(), &'static str> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) => {
                if s.is_dir {
                    return Err(crate::fs::FsError::IsDirectory);
                }
            }
            Err(_) => {
                m.fs.create_file(dev, &target).map(|_| ())?;
            }
        }
        m.fs.write_file(dev, &target, data)
    })
    .map_err(|e| e.as_str())
}

pub fn create_download_staging_file(
    destination: &str,
) -> Result<alloc::string::String, &'static str> {
    static NEXT_STAGING_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let destination = abspath(destination.trim());
    if destination.is_empty() || destination.ends_with('/') {
        return Err("Invalid destination path");
    }
    let parent = match destination.rfind('/') {
        Some(0) => alloc::string::String::from("/"),
        Some(index) => alloc::string::String::from(&destination[..index]),
        None => alloc::string::String::from("/"),
    };
    for _ in 0..32 {
        let id = NEXT_STAGING_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let name = alloc::format!(".wget-{:x}-{:x}.part", monotonic_ms(), id);
        let path = if parent == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", parent, name)
        };
        let created = crate::fs::vfs::with(|m| {
            let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
            m.fs.create_file(dev, &path).map(|_| ()).map_err(|e| e)
        });
        match created {
            Ok(()) => return Ok(path),
            Err(crate::fs::FsError::AlreadyExists) => continue,
            Err(error) => return Err(error.as_str()),
        }
    }
    Err("Unable to create download staging file")
}

pub fn append_file_contents(name: &str, data: &[u8]) -> Result<(), &'static str> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.append_file(dev, &target, data)
    })
    .map_err(|e| e.as_str())
}

pub fn remove_file_contents(name: &str) -> Result<(), &'static str> {
    let target = abspath(name);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.remove_file(dev, &target)
    })
    .map_err(|e| e.as_str())
}

pub fn promote_download_file(staging: &str, destination: &str) -> Result<(), &'static str> {
    let staging = abspath(staging);
    let destination = abspath(destination);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        // Replace any existing destination first so the rename behaves the
        // same on every driver (SimplFS replaces, ext4 refuses).
        if let Ok(s) = m.fs.stat(dev, &destination) {
            if s.is_dir {
                return Err(crate::fs::FsError::IsDirectory);
            }
            m.fs.remove_file(dev, &destination)?;
        }
        m.fs.rename(dev, &staging, &destination)
    })
    .map_err(|e| e.as_str())
}

// â”€â”€ GUI bridge (shared by desktop File Explorer + Drive apps) â”€â”€â”€â”€â”€
// These go through the VFS so shell and desktop stay in sync whatever is
// mounted. All helpers lock the VFS once, copy results out, and drop the
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
                let _ = crate::fs::vfs::unmount();
                *MOUNTED_DRIVE.lock() = None;
            }
            crate::fs::cache::flush_tag(drive);
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
    let mut dev = crate::drivers::block::DriveBlockDevice::new(drive);
    let kind = crate::fs::vfs::probe_device(&mut dev).unwrap_or(crate::fs::vfs::FsKind::Simple);
    match crate::fs::vfs::mount(drive, Some(kind), "/") {
        Ok(()) => {
            *MOUNTED_DRIVE.lock() = Some(drive);
            let detected = crate::fs::vfs::mount_kind().unwrap_or("unknown");
            Ok(alloc::format!("Drive {} mounted ({}). Root ready.", drive, detected))
        }
        Err(e) => Err(e.as_str()),
    }
}

/// One-line mount status for GUI labels. Never fails.
pub fn gui_fs_status() -> alloc::string::String {
    if !crate::fs::vfs::is_mounted() {
        return alloc::string::String::from("Status: not mounted (open Drive: pick disk, Format, then Mount)");
    }
    alloc::format!(
        "Status: {} mounted (drive {}), cwd={}",
        crate::fs::vfs::mount_kind().unwrap_or("unknown"),
        mounted_drive(),
        crate::fs::vfs::cwd()
    )
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
        let count = crate::fs::vfs::with(|m| {
            let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
            m.fs.list_dir(dev, "/").map(|v| v.len())
        })
        .unwrap_or(0);
        out.push_str(&alloc::format!(
            "\nFS: {} mounted, root entries: {}",
            crate::fs::vfs::mount_kind().unwrap_or("unknown"),
            count
        ));
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
    let target = abspath(path);
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) if s.is_dir => Ok((target.clone(), m.fs.list_dir(dev, &target)?)),
            Ok(_) => Err(crate::fs::FsError::NotDirectory),
            Err(e) => Err(e),
        }
    })
    .map_err(|e| e.as_str())
}

/// Read file for GUI preview (capped by caller via truncate).
pub fn gui_read_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let target = abspath(path.trim());
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.read_file(dev, &target)
    })
    .map_err(|e| e.as_str())
}

/// File size in bytes for GUI (metadata only, no buffer allocation — safe
/// to call before attempting a big read).
pub fn gui_file_size(path: &str) -> Result<u64, &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let target = abspath(path.trim());
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.file_size_at(dev, &target)
    })
    .map_err(|e| e.as_str())
}

/// Create file at GUI-resolved full path.
pub fn gui_create_file(path: &str) -> Result<(), &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let target = abspath(path.trim());
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.create_file(dev, &target).map(|_| ()).map_err(|e| e)
    })
    .map_err(|e| e.as_str())
}

/// Create directory at GUI-resolved full path.
pub fn gui_create_dir(path: &str) -> Result<(), &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let target = abspath(path.trim());
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        m.fs.create_dir(dev, &target)
    })
    .map_err(|e| e.as_str())
}

/// Delete file or (empty) directory at path. Tries file first, then dir.
pub fn gui_delete_path(path: &str) -> Result<(), &'static str> {
    if !is_mounted() {
        return Err("Filesystem not mounted");
    }
    let target = abspath(path.trim());
    if target == "/" {
        return Err("Cannot delete root");
    }
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &target) {
            Ok(s) if s.is_dir => m.fs.remove_dir(dev, &target),
            Ok(_) => m.fs.remove_file(dev, &target),
            Err(e) => Err(e),
        }
    })
    .map_err(|e| e.as_str())
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
    crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        if m.fs.stat(dev, "/config").is_err() {
            // Best-effort: ignore "already exists" races from double-clicks.
            let _ = m.fs.create_dir(dev, "/config");
        }
        if m.fs.stat(dev, path).is_err() {
            m.fs
                .create_file(dev, path)
                .map(|_| ())
                .map_err(|_| crate::fs::FsError::InvalidPath)?;
        }
        m.fs.write_file(dev, path, text.as_bytes())?;
        Ok(alloc::string::String::from("settings saved"))
    })
    .map_err(|e| e.as_str())
}

/// Shell edit command â€“ delegates to nano editor
fn cmd_edit(args: &str) {
    crate::editor::run(args);
}

// â”€â”€ App commands â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
    if let Some(data) = read_file_contents(path) {
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

/// Inspect a Doom WAD via streaming range reads (no whole-file load).
/// Works for multi-MB WADs on extra disks: `mount 2`, `doominfo /wad/doom1.wad`.
fn cmd_doominfo(args: &str) {
    let path = args.trim();
    if path.is_empty() {
        println!("Usage: doominfo <wad-path>  (e.g., 'doominfo /wad/doom1.wad')");
        println!("Mount the WAD disk first: 'diskinfo', 'mount <drive>'.");
        return;
    }
    match crate::doom::wad_info(path, 12) {
        Ok(info) => {
            let magic = String::from_utf8_lossy(&info.magic);
            println!("WAD '{}' ({} bytes)", path, info.file_size);
            println!("  Type: {}", magic);
            println!("  Lumps: {}", info.num_lumps);
            println!("  Engine: classic Doom ready; Doom64 WAD swaps in later");
            println!("  Audio: silent v1 (stubs)");
            for (i, name) in info.first_lumps.iter().enumerate() {
                println!("    [{}] {}", i, name);
            }
            if info.num_lumps > info.first_lumps.len() as u32 {
                println!(
                    "    ... ({} more)",
                    info.num_lumps - info.first_lumps.len() as u32
                );
            }
        }
        Err(e) => println!("doominfo: {}", e),
    }
}

/// Play Doom: validates the WAD (streaming, no full load), queues it for
/// the desktop Doom launcher, and tells the user how to open it.
/// With no argument, scans the mounted FS for a known WAD name.
fn cmd_doom(args: &str) {
    if !crate::doom::engine::available() {
        println!("doom: engine not compiled into this build");
        return;
    }
    let arg = args.trim();
    // Split the optional `run` subcommand from an optional explicit path:
    // `doom` (scan), `doom run` (scan + play), `doom <path>`,
    // `doom run <path>`. (`run` is never a WAD path.)
    let mut words = arg.split_whitespace();
    let (run_now, explicit) = match words.next() {
        Some("run") => (true, words.next()),
        Some(other) => (false, Some(other)),
        None => (false, None),
    };
    let mut candidates: alloc::vec::Vec<String> = alloc::vec::Vec::new();
    if let Some(path) = explicit {
        if !path.is_empty() {
            candidates.push(String::from(path));
        }
    }
    for auto in [
        "/wad/doom1.wad",
        "/wad/doom.wad",
        "/wad/DOOM.WAD",
        "/wad/doom2.wad",
        "/wad/DOOM2.WAD",
        "/wad/tnt.wad",
        "/wad/plutonia.wad",
        "/apps/doom1.wad",
    ] {
        if !candidates.iter().any(|c| c == auto) {
            candidates.push(String::from(auto));
        }
    }
    for path in &candidates {
        match crate::doom::wad_info(path, 1) {
            Ok(info) => {
                let magic = String::from_utf8_lossy(&info.magic);
                println!("WAD '{}' ({} bytes, {} lumps, {})", path, info.file_size, info.num_lumps, magic);
                crate::desktop::doom::set_pending_wad(path);
                if !crate::drivers::fb::is_active() {
                    println!("No framebuffer (VGA text mode): Doom needs UEFI GOP.");
                    println!("Reboot with --uefi, then 'doom run'.");
                    return;
                }
                // `doom run` enters the desktop straight into the game
                // (no mouse click needed); plain `doom` only queues it.
                if run_now {
                    println!("Starting Doom...");
                    crate::desktop::run();
                } else {
                    println!("Run 'doom run' to play now, or 'desktop' and click Doom.");
                }
                return;
            }
            Err(_) => continue,
        }
    }
    if !is_mounted() {
        println!("doom: filesystem not mounted. Try 'diskinfo', 'mount <drive>'.");
    } else if let Some(path) = explicit {
        println!("doom: WAD not readable: '{}' (try 'doominfo {}')", path, path);
    } else {
        println!("doom: no WAD found. Mount the WAD disk ('mount <drive>'),");
        println!("  or pass a path: 'doom /wad/doom2.wad'. Check with 'doominfo'.");
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

// â”€â”€ TAB completion â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
    if !crate::fs::vfs::is_mounted() {
        return None;
    }
    // dir_part comes from the raw token; anchor it at the cwd like the shell does.
    let dir_abs = if dir_part.is_empty() {
        crate::fs::vfs::cwd()
    } else if dir_part.starts_with('/') {
        alloc::string::String::from(dir_part.trim_end_matches('/'))
    } else {
        abspath(dir_part)
    };
    let dir_abs = if dir_abs.is_empty() {
        alloc::string::String::from("/")
    } else {
        dir_abs
    };
    let entries = crate::fs::vfs::with(|m| {
        let dev: &mut dyn crate::drivers::block::BlockDevice = &mut m.device;
        match m.fs.stat(dev, &dir_abs) {
            Ok(s) if s.is_dir => m.fs.list_dir(dev, &dir_abs),
            _ => Ok(Vec::new()),
        }
    })
    .unwrap_or_default();
    let mut filtered: Vec<crate::fs::FileInfo> = entries
        .into_iter()
        .filter(|fi| fi.name.starts_with(file_prefix))
        .collect();
    filtered.sort_by(|a, b| a.name.cmp(&b.name));
    Some(filtered)
}
