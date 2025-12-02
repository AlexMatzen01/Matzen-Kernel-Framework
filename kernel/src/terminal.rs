//! Terminal Shell Module
//!
//! This module provides a simple command-line shell interface for the kernel.
//! It supports basic commands like help, clear, echo, and about.

use core::fmt::Write;
use crate::drivers::keyboard;
use crate::drivers::vga::VgaTextWriter;
use spin::Mutex;

/// Maximum command line buffer size
const MAX_COMMAND_LEN: usize = 256;

/// Terminal state
pub struct Terminal {
    buffer: [u8; MAX_COMMAND_LEN],
    cursor: usize,
    writer: VgaTextWriter,
}

impl Terminal {
    /// Create a new terminal instance
    pub const fn new() -> Self {
        Self {
            buffer: [0u8; MAX_COMMAND_LEN],
            cursor: 0,
            writer: VgaTextWriter::new(),
        }
    }

    /// Print a string to the terminal
    pub fn print(&self, s: &str) {
        let _ = self.writer.lock().write_str(s);
    }

    /// Print a string followed by a newline
    pub fn println(&self, s: &str) {
        let mut writer = self.writer.lock();
        let _ = writer.write_str(s);
        let _ = writer.write_str("\n");
    }

    /// Print the shell prompt
    pub fn print_prompt(&self) {
        self.print("mfk> ");
    }

    /// Handle a character input
    pub fn handle_char(&mut self, ch: char) {
        match ch {
            '\n' => {
                self.print("\n");
                if self.cursor > 0 {
                    self.execute_command();
                }
                self.cursor = 0;
                self.print_prompt();
            }
            '\x08' => {
                // Backspace
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.print("\x08 \x08"); // Move back, clear, move back
                }
            }
            ch if ch.is_ascii() && !ch.is_control() => {
                if self.cursor < MAX_COMMAND_LEN - 1 {
                    self.buffer[self.cursor] = ch as u8;
                    self.cursor += 1;
                    // Echo character
                    let mut writer = self.writer.lock();
                    let _ = write!(writer, "{}", ch);
                }
            }
            _ => {}
        }
    }

    /// Execute the current command in the buffer
    fn execute_command(&mut self) {
        let cmd_str = core::str::from_utf8(&self.buffer[..self.cursor]).unwrap_or("");
        let cmd_str = cmd_str.trim();
        
        if cmd_str.is_empty() {
            return;
        }

        // Parse command and arguments
        let mut parts = cmd_str.splitn(2, ' ');
        let command = parts.next().unwrap_or("");
        let args = parts.next().unwrap_or("").trim();

        match command {
            "help" => self.cmd_help(),
            "clear" => self.cmd_clear(),
            "echo" => self.cmd_echo(args),
            "about" => self.cmd_about(),
            "version" => self.cmd_version(),
            "uptime" => self.cmd_uptime(),
            "reboot" => self.cmd_reboot(),
            "halt" => self.cmd_halt(),
            "mem" => self.cmd_mem(),
            "" => {}
            _ => {
                self.print("Unknown command: ");
                self.println(command);
                self.println("Type 'help' for a list of commands.");
            }
        }
    }

    // Command implementations

    fn cmd_help(&self) {
        self.println("Available commands:");
        self.println("  help    - Show this help message");
        self.println("  clear   - Clear the screen");
        self.println("  echo    - Print text to the screen");
        self.println("  about   - Show information about MFK");
        self.println("  version - Show kernel version");
        self.println("  uptime  - Show system uptime (placeholder)");
        self.println("  mem     - Show memory information");
        self.println("  reboot  - Reboot the system");
        self.println("  halt    - Halt the system");
    }

    fn cmd_clear(&self) {
        // Clear screen by printing 25 newlines
        for _ in 0..25 {
            self.println("");
        }
    }

    fn cmd_echo(&self, args: &str) {
        self.println(args);
    }

    fn cmd_about(&self) {
        self.println("======================================");
        self.println("   Matzen Kernel Framework (MFK)");
        self.println("======================================");
        self.println("");
        self.println("A forward-looking Rust micro-kernel");
        self.println("designed for experimentation on");
        self.println("bare-metal x86_64 targets.");
        self.println("");
        self.println("Features:");
        self.println("  - Pure Rust no_std kernel");
        self.println("  - VGA text mode display");
        self.println("  - PS/2 keyboard input");
        self.println("  - Simple shell interface");
        self.println("");
        self.println("https://github.com/AlexMatzen01/");
        self.println("         Matzen-Kernel-Framework");
    }

    fn cmd_version(&self) {
        self.println("Matzen Kernel Framework v0.1.0");
        self.println("Built with Rust nightly-2023-11-15");
    }

    fn cmd_uptime(&self) {
        self.println("System uptime: (not yet implemented)");
    }

    fn cmd_mem(&self) {
        self.println("Memory information:");
        self.println("  (Memory manager not yet implemented)");
        self.println("  Use BootInfo for memory map details");
    }

    fn cmd_reboot(&self) {
        self.println("Rebooting system...");
        // Triple fault to reboot (simple method)
        unsafe {
            // Write to PS/2 controller to pulse CPU reset line
            let mut port: x86_64::instructions::port::Port<u8> = x86_64::instructions::port::Port::new(0x64);
            port.write(0xFE);
        }
        // If that didn't work, halt
        loop {
            x86_64::instructions::hlt();
        }
    }

    fn cmd_halt(&self) {
        self.println("Halting system...");
        loop {
            x86_64::instructions::hlt();
        }
    }

    /// Display the welcome banner
    pub fn show_banner(&self) {
        self.println("");
        self.println("========================================");
        self.println("  Welcome to Matzen Kernel Framework!");
        self.println("========================================");
        self.println("");
        self.println("Type 'help' for a list of commands.");
        self.println("");
    }
}

/// Global terminal instance
static TERMINAL: Mutex<Terminal> = Mutex::new(Terminal::new());

/// Initialize the terminal
pub fn init() {
    let term = TERMINAL.lock();
    term.show_banner();
    term.print_prompt();
}

/// Process keyboard input (call this from the main loop)
pub fn process_input() {
    if let Some(ch) = keyboard::read_char() {
        TERMINAL.lock().handle_char(ch);
    }
}

/// Run the terminal main loop
pub fn run() -> ! {
    init();
    loop {
        process_input();
        // Small pause to prevent busy-waiting
        x86_64::instructions::hlt();
    }
}
