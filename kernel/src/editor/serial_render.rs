//! Serial ANSI renderer for QEMU `-display none` + `serial stdio`.
//! Mirrors `ui::draw_frame` but emits ANSI escapes to COM1 so editor is visible
//! over serial. VBox GUI still uses VGA text buffer; QEMU headless uses this.

use alloc::string::String;
use super::buffer::TextBuffer;
use super::viewport::{Viewport, SCREEN_WIDTH, EDIT_TOP};
use crate::drivers::vga::Color;

/// Map VGA Color to ANSI foreground code
fn ansi_fg(c: Color) -> &'static str {
    match c {
        Color::Black => "\x1b[30m",
        Color::Blue => "\x1b[34m",
        Color::Green => "\x1b[32m",
        Color::Cyan => "\x1b[36m",
        Color::Red => "\x1b[31m",
        Color::Magenta => "\x1b[35m",
        Color::Brown => "\x1b[33m", // approximate
        Color::LightGray => "\x1b[37m",
        Color::DarkGray => "\x1b[90m",
        Color::LightBlue => "\x1b[94m",
        Color::LightGreen => "\x1b[92m",
        Color::LightCyan => "\x1b[96m",
        Color::LightRed => "\x1b[91m",
        Color::Pink => "\x1b[95m",
        Color::Yellow => "\x1b[33m",
        Color::White => "\x1b[97m",
    }
}

fn ansi_bg(c: Color) -> &'static str {
    match c {
        Color::Black => "\x1b[40m",
        Color::Blue => "\x1b[44m",
        Color::Green => "\x1b[42m",
        Color::Cyan => "\x1b[46m",
        Color::Red => "\x1b[41m",
        Color::Magenta => "\x1b[45m",
        Color::Brown => "\x1b[43m",
        Color::LightGray => "\x1b[47m",
        Color::DarkGray => "\x1b[100m",
        Color::LightBlue => "\x1b[104m",
        Color::LightGreen => "\x1b[102m",
        Color::LightCyan => "\x1b[106m",
        Color::LightRed => "\x1b[101m",
        Color::Pink => "\x1b[105m",
        Color::Yellow => "\x1b[43m",
        Color::White => "\x1b[107m",
    }
}

// VGA palette chosen for nano chrome
const TITLE_FG: Color = Color::Black;
const TITLE_BG: Color = Color::White;
const STATUS_FG: Color = Color::Black;
const STATUS_BG: Color = Color::Yellow;
const EDIT_FG: Color = Color::White;
const EDIT_BG: Color = Color::Black;

fn build_title(buf: &TextBuffer) -> String {
    let name = buf.filename.clone().unwrap_or_else(|| String::from("New Buffer"));
    let modif = if buf.dirty { " [Modified]" } else { "" };
    alloc::format!(" GNU nano 7.2  File: {}{}", name, modif)
}

/// Draw full frame to serial as ANSI.
/// Uses `crate::serial_print!` (which already does without_interrupts + SERIAL lock).
pub fn draw_frame_ansi(buf: &TextBuffer, vp: &Viewport, status_msg: Option<&str>, status_is_error: bool) {
    // Clear screen, hide cursor during draw to reduce flicker, home cursor
    crate::serial_print!("\x1b[2J\x1b[H\x1b[?25l");

    // Title bar (row 1)
    let title = build_title(buf);
    let tbytes = title.as_bytes();
    let start = (SCREEN_WIDTH.saturating_sub(tbytes.len())) / 2;
    // Title bar: black on white, full width
    crate::serial_print!("\x1b[1;1H"); // row1 col1 (1-indexed for ANSI)
    crate::serial_print!("{}{}", ansi_fg(TITLE_FG), ansi_bg(TITLE_BG));
    for _ in 0..start { crate::serial_print!(" "); }
    for &b in tbytes.iter().take(SCREEN_WIDTH - start) {
        let ch = if (0x20..=0x7e).contains(&b) { b as char } else { '?' };
        crate::serial_print!("{}", ch);
    }
    // pad rest
    let used = start + tbytes.len().min(SCREEN_WIDTH - start);
    for _ in used..SCREEN_WIDTH { crate::serial_print!(" "); }
    crate::serial_print!("\x1b[0m");

    // Edit area rows 2..22 (EDIT_HEIGHT=21)
    for srow in 0..vp.height {
        let buf_row = vp.top_line + srow;
        let screen_row_ansi = EDIT_TOP + srow + 1; // ANSI 1-indexed, EDIT_TOP is 0-indexed 1 => ansi 2
        crate::serial_print!("\x1b[{};1H", screen_row_ansi);
        // default edit colors
        crate::serial_print!("{}{}", ansi_fg(EDIT_FG), ansi_bg(EDIT_BG));

        if buf_row < buf.lines.len() {
            let line = &buf.lines[buf_row];
            let left = vp.left_col;
            let right = (left + SCREEN_WIDTH).min(line.len());
            // Build visible line with tab expansion and control handling
            // We render directly to serial to keep colors for selection per-char.
            // First fill line with spaces (to clear previous content) handled by moving cursor and printing.
            // We'll assemble a buffer of (char, fg, bg) for the row.
            let mut row_chars: [u8; 80] = [b' '; 80];
            let mut row_fg: [Color; 80] = [EDIT_FG; 80];
            let mut row_bg: [Color; 80] = [EDIT_BG; 80];

            // Fill with spaces already

            // Render text
            let mut col = 0usize;
            let mut idx = left;
            while idx < right && col < SCREEN_WIDTH {
                let b = line[idx];
                if b == b'\t' {
                    let spaces = 4 - (col % 4);
                    for _ in 0..spaces {
                        if col >= SCREEN_WIDTH { break; }
                        row_chars[col] = b' ';
                        col += 1;
                    }
                } else if b < 0x20 || b == 0x7F {
                    row_chars[col] = b'^';
                    row_fg[col] = Color::Yellow;
                    col += 1;
                } else {
                    row_chars[col] = b;
                    col += 1;
                }
                idx += 1;
            }
            // Scroll indicators
            if vp.left_col > 0 {
                row_chars[0] = b'$';
                row_fg[0] = Color::Yellow;
            }
            if line.len() > vp.left_col + SCREEN_WIDTH {
                row_chars[SCREEN_WIDTH - 1] = b'$';
                row_fg[SCREEN_WIDTH - 1] = Color::Yellow;
            }
            // Selection highlight
            if let Some((mr, mc)) = buf.mark {
                let (sr, sc, er, ec) = if (mr, mc) <= (buf.cursor_row, buf.cursor_col) {
                    (mr, mc, buf.cursor_row, buf.cursor_col)
                } else {
                    (buf.cursor_row, buf.cursor_col, mr, mc)
                };
                if buf_row >= sr && buf_row <= er {
                    let s = if buf_row == sr { sc } else { 0 };
                    let e = if buf_row == er { ec } else { line.len() };
                    for bcol in s..e {
                        if bcol < vp.left_col || bcol >= vp.left_col + SCREEN_WIDTH { continue; }
                        let scol = bcol - vp.left_col;
                        row_fg[scol] = Color::White;
                        row_bg[scol] = Color::Blue;
                    }
                }
            }
            // Emit row with color changes coalesced
            let mut cur_fg = EDIT_FG;
            let mut cur_bg = EDIT_BG;
            // Ensure starting color emitted
            // Already emitted at row start, cur reflects that
            for c in 0..SCREEN_WIDTH {
                if row_fg[c] != cur_fg || row_bg[c] != cur_bg {
                    crate::serial_print!("{}{}", ansi_fg(row_fg[c]), ansi_bg(row_bg[c]));
                    cur_fg = row_fg[c];
                    cur_bg = row_bg[c];
                }
                let ch = row_chars[c];
                // Serial expects char, for control we already converted to '^'
                // For byte 0xFE placeholder (non-printable) show '?'
                let out_ch = if ch == 0xFE { b'?' } else { ch };
                crate::serial_print!("{}", out_ch as char);
            }
            crate::serial_print!("\x1b[0m");
        } else {
            // empty line beyond EOF – just spaces in edit colors
            crate::serial_print!("{}{}", ansi_fg(EDIT_FG), ansi_bg(EDIT_BG));
            for _ in 0..SCREEN_WIDTH { crate::serial_print!(" "); }
            crate::serial_print!("\x1b[0m");
        }
    }

    // Status bar row 23 (ansi row EDIT_TOP+height+1)
    let status_row_ansi = EDIT_TOP + vp.height + 1;
    crate::serial_print!("\x1b[{};1H", status_row_ansi);
    let (sf, sb) = if status_is_error { (Color::White, Color::Red) } else { (STATUS_FG, STATUS_BG) };
    crate::serial_print!("{}{}", ansi_fg(sf), ansi_bg(sb));
    // Clear line and write message or default
    // Use erase to end of line: \x1b[K
    crate::serial_print!("\x1b[2K");
    if let Some(msg) = status_msg {
        crate::serial_print!(" {}", msg);
        // pad already cleared via erase
    } else {
        let fname = buf.filename.clone().unwrap_or_else(|| String::from("[New File]"));
        let dirty = if buf.dirty { " *" } else { "" };
        let left = alloc::format!(" {}{}", fname, dirty);
        let right = alloc::format!(" L{}/{} C{}", buf.cursor_row + 1, buf.lines.len(), buf.cursor_col + 1);
        // Left part
        let left_bytes = left.as_bytes();
        for &b in left_bytes.iter().take(SCREEN_WIDTH - 20) {
            crate::serial_print!("{}", b as char);
        }
        // Right aligned: move cursor to column SCREEN_WIDTH - right.len()
        let right_len = right.len();
        let start_col = SCREEN_WIDTH.saturating_sub(right_len + 1);
        // Need to move cursor to that col on same row
        // We already at col1, we could pad spaces then overwrite, simpler: emit spaces to pad, then right
        let used = left_bytes.len().min(SCREEN_WIDTH - 20);
        if start_col > used {
            for _ in used..start_col { crate::serial_print!(" "); }
        }
        crate::serial_print!("{}", right);
    }
    crate::serial_print!("\x1b[0m");

    // Help bar rows 24-25
    let help_row1_ansi = status_row_ansi + 1;
    let help_row2_ansi = help_row1_ansi + 1;
    const HELP_LINE1: &str = "^G Get Help  ^O Write Out ^W Where Is  ^K Cut       ^J Justify   ^C Cur Pos";
    const HELP_LINE2: &str = "^X Exit      ^R Read File ^\\ Replace   ^U Uncut     ^T To Spell  ^_ Go To Line";
    for (row, line) in [(help_row1_ansi, HELP_LINE1), (help_row2_ansi, HELP_LINE2)] {
        crate::serial_print!("\x1b[{};1H\x1b[37;40m\x1b[2K", row); // white on black, erase line
        // Render help with ^highlight yellow
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'^' && i + 1 < bytes.len() {
                crate::serial_print!("{}{}^{}{}", "\x1b[33m", "", "\x1b[33m", bytes[i + 1] as char); // yellow ^X
                crate::serial_print!("\x1b[37;40m"); // back to white
                i += 2;
            } else {
                crate::serial_print!("{}", bytes[i] as char);
                i += 1;
            }
        }
        crate::serial_print!("\x1b[0m");
    }

    // Move cursor to editor position and show it
    if let Some(screen_row) = {
        if buf.cursor_row < vp.top_line { None }
        else {
            let rel = buf.cursor_row - vp.top_line;
            if rel >= vp.height { None } else { Some(EDIT_TOP + rel) }
        }
    } {
        let ansi_row = screen_row + 1; // 1-indexed
        let ansi_col = (buf.cursor_col.saturating_sub(vp.left_col)).min(79) + 1;
        crate::serial_print!("\x1b[{};{}H\x1b[?25h", ansi_row, ansi_col);
    } else {
        crate::serial_print!("\x1b[?25l");
    }
}

/// Draw prompt line on serial status row (mirrors ui::prompt_with_input render)
pub fn draw_prompt_ansi(question: &str, input: &[u8], cursor: usize) {
    let status_row_ansi = super::viewport::EDIT_TOP + super::viewport::EDIT_HEIGHT + 1;
    // Status row is 23 in 1-indexed (EDIT_TOP=1, height=21 => status 23)
    crate::serial_print!("\x1b[{};1H\x1b[30;47m\x1b[2K", status_row_ansi); // black on white, erase
    crate::serial_print!("{} ", question);
    let qlen = question.len() + 1;
    let remaining = SCREEN_WIDTH.saturating_sub(qlen + 1);
    let display = &input[..input.len().min(remaining)];
    for &b in display {
        let ch = if (0x20..=0x7e).contains(&b) { b as char } else { '?' };
        crate::serial_print!("{}", ch);
    }
    // Move cursor
    let cur_col = qlen + cursor.min(remaining) + 1;
    crate::serial_print!("\x1b[{};{}H\x1b[?25h\x1b[0m", status_row_ansi, cur_col);
}

/// Draw confirm line on serial status row
pub fn draw_confirm_ansi(question: &str) {
    let status_row_ansi = super::viewport::EDIT_TOP + super::viewport::EDIT_HEIGHT + 1;
    crate::serial_print!("\x1b[{};1H\x1b[37;41m\x1b[2K", status_row_ansi); // white on red
    crate::serial_print!("{} (Y/N) ?", question);
    crate::serial_print!("\x1b[0m\x1b[{};{}H\x1b[?25h", status_row_ansi, question.len() + 8);
}

/// Help overlay for serial
pub fn draw_help_ansi(scroll: usize) {
    const HELP_TEXT: &[&str] = &[
        " GNU nano 7.2  Help Text",
        "",
        " Nano is a small and friendly text editor. This MFK port mimics nano.",
        "",
        " Shortcuts:",
        "  ^G  (F1) Display this help text",
        "  ^O  (F3) Write the current buffer to disk",
        "  ^W  (F6) Search for a string",
        "  ^\\  Replace string (Alt+R)",
        "  ^K  (F9) Cut current line (consecutive cuts append)",
        "  ^U  (F10) Uncut (paste) from cutbuffer",
        "  ^C  Show cursor position (line/col)",
        "  ^_  (F13/Alt+G) Go to line number",
        "  ^J  Justify current paragraph",
        "  ^R  Read (insert) file",
        "  ^X  (F2) Exit editor (prompts if modified)",
        "  ^Z  Undo (Alt+U)    ^Y Redo (Alt+E)",
        "  ^6  Set mark (Alt+A) – then ^K cuts selection",
        "  ESC or ^G to close help",
        "",
        " Status bar shows filename, modified [*], and line/col.",
        " Files limited to 6144 bytes (12 blocks) by SimplFS.",
        "",
        " Press any key to continue... (ESC/^G/^X to exit help)",
    ];
    // Clear and use blue background white text like VGA help
    crate::serial_print!("\x1b[2J\x1b[H\x1b[37;44m");
    // Title at 1,1
    crate::serial_print!("\x1b[1;1H\x1b[37;44m");
    let title = " MFK nano Help (ESC to exit) ";
    let pad = (SCREEN_WIDTH.saturating_sub(title.len())) / 2;
    for _ in 0..pad { crate::serial_print!(" "); }
    crate::serial_print!("{}", title);
    for _ in 0..(SCREEN_WIDTH - pad - title.len()) { crate::serial_print!(" "); }
    // Content rows 2..24
    let content_rows = crate::drivers::vga::VGA_HEIGHT - 2; // 23
    for i in 0..content_rows {
        let idx = scroll + i;
        if idx >= HELP_TEXT.len() { break; }
        let row = 2 + i;
        crate::serial_print!("\x1b[{};1H\x1b[37;44m\x1b[2K", row);
        crate::serial_print!("{}", HELP_TEXT[idx]);
    }
    // Footer at 25
    crate::serial_print!("\x1b[25;1H\x1b[30;47m\x1b[2K");
    let footer = " [ ESC / ^G / ^X to close Help ]  [ PgUp/PgDn or Arrows to scroll ] ";
    crate::serial_print!("{}", footer);
    crate::serial_print!("\x1b[0m\x1b[?25l");
}
