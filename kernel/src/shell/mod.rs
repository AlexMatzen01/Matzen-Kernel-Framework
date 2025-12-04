//! Terminal Shell
//!
//! A simple command-line shell for the Matzen Kernel Framework.

use crate::drivers::{keyboard, vga};
use crate::{print, println};

/// Maximum length of a command line
const MAX_CMD_LENGTH: usize = 256;

/// Shell prompt string
const PROMPT: &str = "mfk> ";

/// Runs the shell loop
pub fn run() -> ! {
    use crate::serial_println;
    serial_println!("Shell run() function started");
    
    let mut cmd_buffer: [u8; MAX_CMD_LENGTH] = [0; MAX_CMD_LENGTH];
    let mut cmd_len: usize = 0;

    serial_println!("About to print prompt...");
    print!("{}", PROMPT);
    serial_println!("Prompt printed, entering main loop");

    loop {
        if let Some(c) = keyboard::read_char() {
            match c {
                '\n' => {
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
        // Don't use hlt() here - it sleeps the CPU waiting for interrupts,
        // but interrupts aren't enabled yet, causing the kernel to hang
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
        "uptime" => cmd_uptime(),
        "mem" | "memory" => cmd_memory(),
        "reboot" => cmd_reboot(),
        "halt" | "shutdown" => cmd_halt(),
        "date" => cmd_date(),
        "whoami" => cmd_whoami(),
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
    println!("  uptime    - Show system uptime (simulated)");
    println!("  memory    - Display memory information");
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
    println!("Uptime: System has been running since boot.");
    println!("(Precise timing not yet implemented)");
}

/// Displays memory information
fn cmd_memory() {
    println!("Memory Information:");
    println!("  VGA Buffer: 0xb8000 (4KB)");
    println!("  (Detailed memory info not yet implemented)");
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

/// Displays the date
fn cmd_date() {
    println!("Date/time functionality not yet implemented.");
    println!("(RTC driver required)");
}

/// Displays current user
fn cmd_whoami() {
    println!("root");
}
