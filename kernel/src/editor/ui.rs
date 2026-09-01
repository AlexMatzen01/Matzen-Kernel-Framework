//! UI rendering for nano-like editor
//! Draws titlebar, edit area, statusbar, helpbar

use alloc::string::String;
use alloc::vec::Vec;
use crate::drivers::vga::{Color, WRITER};
use super::buffer::TextBuffer;
use super::viewport::{Viewport, SCREEN_WIDTH, EDIT_TOP, HEADER_HEIGHT, STATUS_HEIGHT, HELP_HEIGHT};

const TITLE_FG: Color = Color::Black;
const TITLE_BG: Color = Color::White;
const STATUS_FG: Color = Color::Black;
const STATUS_BG: Color = Color::Yellow;
const EDIT_FG: Color = Color::White;
const EDIT_BG: Color = Color::Black;
const HELP_FG: Color = Color::White;
const HELP_BG: Color = Color::Black;

// Help bar lines (nano style, shortened to fit 80 cols)
const HELP_LINE1: &str = "^G Get Help  ^O Write Out ^W Where Is  ^K Cut       ^J Justify   ^C Cur Pos";
const HELP_LINE2: &str = "^X Exit      ^R Read File ^\\ Replace   ^U Uncut     ^T To Spell  ^_ Go To Line";
// Extended help line extras for sophisticated: ^Z Undo ^Y Redo  M-A Mark

/// Draw full frame
pub fn draw_frame(buf: &TextBuffer, vp: &Viewport, status_msg: Option<&str>, status_is_error: bool) {
    // Use WRITER lock via interrupts
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        // Title bar
        let title = build_title(buf);
        w.fill_rect(0, 0, SCREEN_WIDTH, 1, b' ', TITLE_FG, TITLE_BG);
        let tbytes = title.as_bytes();
        let start = (SCREEN_WIDTH.saturating_sub(tbytes.len())) / 2;
        for (i, &b) in tbytes.iter().enumerate() {
            if start + i >= SCREEN_WIDTH { break; }
            w.write_at(0, start + i, b, TITLE_FG, TITLE_BG);
        }

        // Edit area – clear with edit colors then paint viewport lines
        for srow in 0..vp.height {
            let buf_row = vp.top_line + srow;
            let screen_row = EDIT_TOP + srow;
            w.fill_rect(screen_row, 0, SCREEN_WIDTH, 1, b' ', EDIT_FG, EDIT_BG);
            if buf_row < buf.lines.len() {
                let line = &buf.lines[buf_row];
                // Horizontal scroll
                let left = vp.left_col;
                let right = (left + SCREEN_WIDTH).min(line.len());
                let visible = if left < line.len() { &line[left..right] } else { &[][..] };
                // Render with tab expansion? Show tabs as 4 spaces
                let mut col = 0usize;
                let mut idx = left;
                while idx < right && col < SCREEN_WIDTH {
                    let b = line[idx];
                    if b == b'\t' {
                        let spaces = 4 - (col % 4);
                        for _ in 0..spaces {
                            if col >= SCREEN_WIDTH { break; }
                            w.write_at(screen_row, col, b' ', EDIT_FG, EDIT_BG);
                            col += 1;
                        }
                    } else if b < 0x20 || b == 0x7F {
                        // Render control as highlighted ?
                        w.write_at(screen_row, col, b'^', Color::Yellow, EDIT_BG);
                        col += 1;
                    } else {
                        w.write_at(screen_row, col, b, EDIT_FG, EDIT_BG);
                        col += 1;
                    }
                    idx += 1;
                }
                // Horizontal scroll indicators: left $ if scrolled, right $ if truncated
                if vp.left_col > 0 {
                    w.write_at(screen_row, 0, b'$', Color::Yellow, EDIT_BG);
                }
                if line.len() > vp.left_col + SCREEN_WIDTH {
                    w.write_at(screen_row, SCREEN_WIDTH-1, b'$', Color::Yellow, EDIT_BG);
                }
                // If mark active, highlight selection
                if let Some((mr, mc)) = buf.mark {
                    highlight_selection(&mut w, buf, vp, mr, mc, screen_row, buf_row);
                }
            }
        }

        // Status bar (row 22)
        let status_row = EDIT_TOP + vp.height;
        let (sf, sb) = if status_is_error { (Color::White, Color::Red) } else { (STATUS_FG, STATUS_BG) };
        w.fill_rect(status_row, 0, SCREEN_WIDTH, 1, b' ', sf, sb);
        if let Some(msg) = status_msg {
            let msg_bytes = msg.as_bytes();
            let mut col = 1;
            for &b in msg_bytes.iter().take(SCREEN_WIDTH-2) {
                w.write_at(status_row, col, b, sf, sb);
                col += 1;
            }
        } else {
            // Default status: show filename, dirty, cursor pos helper?
            let fname = buf.filename.clone().unwrap_or_else(|| String::from("[New File]"));
            let dirty = if buf.dirty { " *" } else { "" };
            let line_col = alloc::format!(" L{}/{} C{}", buf.cursor_row+1, buf.lines.len(), buf.cursor_col+1);
            let left_part = alloc::format!(" {}{}", fname, dirty);
            // Left
            for (i, b) in left_part.bytes().enumerate().take(SCREEN_WIDTH-20) {
                w.write_at(status_row, i, b, sf, sb);
            }
            // Right aligned
            let rc_bytes = line_col.as_bytes();
            let start = SCREEN_WIDTH.saturating_sub(rc_bytes.len() + 1);
            for (i, &b) in rc_bytes.iter().enumerate() {
                w.write_at(status_row, start + i, b, sf, sb);
            }
        }

        // Help bar (rows 23,24)
        let help_row1 = status_row + 1;
        let help_row2 = help_row1 + 1;
        w.fill_rect(help_row1, 0, SCREEN_WIDTH, 1, b' ', HELP_FG, HELP_BG);
        w.fill_rect(help_row2, 0, SCREEN_WIDTH, 1, b' ', HELP_FG, HELP_BG);
        // Write with highlighting: ^X in distinct color?
        render_help_line(&mut w, help_row1, HELP_LINE1);
        render_help_line(&mut w, help_row2, HELP_LINE2);
    });
}

fn render_help_line(w: &mut crate::drivers::vga::Writer, row: usize, line: &str) {
    let mut col = 0usize;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && col < SCREEN_WIDTH {
        if bytes[i] == b'^' && i+1 < bytes.len() {
            // Render ^X in yellow/bold style
            w.write_at(row, col, b'^', Color::Yellow, Color::Black);
            col += 1;
            if col < SCREEN_WIDTH {
                w.write_at(row, col, bytes[i+1], Color::Yellow, Color::Black);
                col += 1;
            }
            i += 2;
        } else {
            w.write_at(row, col, bytes[i], Color::White, Color::Black);
            col += 1;
            i += 1;
        }
        // Add spacing already in string
    }
}

fn build_title(buf: &TextBuffer) -> String {
    let name = buf.filename.clone().unwrap_or_else(|| String::from("New Buffer"));
    let modif = if buf.dirty { " [Modified]" } else { "" };
    // Nano title format centered
    alloc::format!(" GNU nano 7.2  File: {}{}", name, modif)
}

fn highlight_selection(w: &mut crate::drivers::vga::Writer, buf: &TextBuffer, vp: &Viewport, mr: usize, mc: usize, screen_row: usize, buf_row: usize) {
    let (sr, sc, er, ec) = if (mr, mc) <= (buf.cursor_row, buf.cursor_col) {
        (mr, mc, buf.cursor_row, buf.cursor_col)
    } else {
        (buf.cursor_row, buf.cursor_col, mr, mc)
    };
    if buf_row < sr || buf_row > er { return; }
    let start = if buf_row == sr { sc } else { 0 };
    let end = if buf_row == er { ec } else { buf.lines[buf_row].len() };
    // Map to screen cols
    for bcol in start..end {
        if bcol < vp.left_col || bcol >= vp.left_col + SCREEN_WIDTH { continue; }
        let scol = bcol - vp.left_col;
        // Need original char to keep glyph, but invert colors
        let b = buf.lines[buf_row][bcol];
        let ch = if b == b'\t' { b' ' } else if b < 0x20 { b'^' } else { b };
        // Highlight: white on blue (nano selection)
        w.write_at(screen_row, scol, ch, Color::White, Color::Blue);
    }
}

/// Draw a prompt overlay on status bar: question + input buffer; returns input on Enter, None on Esc/Ctrl+C
/// This function handles its own input loop, blocking until done
pub fn prompt_with_input(question: &str, initial: Option<&str>) -> Option<String> {
    // We will do inline editing for the prompt: status bar becomes input field
    // Use keyboard::read_key blocking? We'll poll.
    let status_row = EDIT_TOP + super::viewport::EDIT_HEIGHT; // 22
    let mut input: Vec<u8> = initial.map(|s| s.as_bytes().to_vec()).unwrap_or_default();
    let mut cursor = input.len();

    // Helper to render – dual VGA + serial
    let render = |input: &Vec<u8>, cursor: usize| {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut w = WRITER.lock();
            w.fill_rect(status_row, 0, SCREEN_WIDTH, 1, b' ', Color::Black, Color::White);
            let qbytes = question.as_bytes();
            let mut col = 0;
            for &b in qbytes.iter().take(SCREEN_WIDTH) {
                w.write_at(status_row, col, b, Color::Black, Color::White);
                col += 1;
                if col >= SCREEN_WIDTH { break; }
            }
            // Show input after question + space
            if col < SCREEN_WIDTH-1 {
                w.write_at(status_row, col, b' ', Color::Black, Color::White);
                col += 1;
            }
            let remaining = SCREEN_WIDTH.saturating_sub(col + 1);
            let display: Vec<u8> = input.iter().take(remaining).cloned().collect();
            for (i, &b) in display.iter().enumerate() {
                w.write_at(status_row, col + i, b, Color::Black, Color::White);
            }
            // Cursor
            let cur_col = col + cursor.min(remaining);
            w.set_cursor_pos(status_row, cur_col);
            w.show_cursor();
        });
        // Also render to serial ANSI
        super::serial_render::draw_prompt_ansi(question, input, cursor);
    };

    render(&input, cursor);
    loop {
        crate::net::process_packets(); // keep net alive if needed
        if let Some(ev) = crate::drivers::keyboard::read_key() {
            use crate::drivers::keyboard::Key;
            match ev.key {
                Key::Enter => {
                    x86_64::instructions::interrupts::without_interrupts(|| { WRITER.lock().hide_cursor(); });
                    let s = String::from_utf8(input).unwrap_or_default();
                    return Some(s);
                }
                Key::Esc | Key::Ctrl('C') | Key::Ctrl('G') => {
                    x86_64::instructions::interrupts::without_interrupts(|| { WRITER.lock().hide_cursor(); });
                    return None;
                }
                Key::Backspace => {
                    if cursor > 0 {
                        cursor -= 1;
                        input.remove(cursor);
                    }
                }
                Key::Delete => {
                    if cursor < input.len() {
                        input.remove(cursor);
                    }
                }
                Key::ArrowLeft => { if cursor > 0 { cursor -= 1; } }
                Key::ArrowRight => { if cursor < input.len() { cursor += 1; } }
                Key::Home => { cursor = 0; }
                Key::End => { cursor = input.len(); }
                Key::Char(c) => {
                    if input.len() < 120 {
                        input.insert(cursor, c as u8);
                        cursor += 1;
                    }
                }
                Key::Ctrl('U') => { // clear line like nano
                    input.clear();
                    cursor = 0;
                }
                Key::Ctrl('K') => { // cut to end
                    input.truncate(cursor);
                }
                _ => {}
            }
            render(&input, cursor);
        }
        core::hint::spin_loop();
    }
}

/// Show a confirmation prompt on status bar: message + " (Y/N/Cancel? ^C)" -> returns Some(true) Y, Some(false) N, None Cancel
pub fn confirm(question: &str) -> Option<bool> {
    let status_row = EDIT_TOP + super::viewport::EDIT_HEIGHT;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        w.fill_rect(status_row, 0, SCREEN_WIDTH, 1, b' ', Color::White, Color::Red);
        let full = alloc::format!("{} (Y/N) ?", question);
        let bytes = full.as_bytes();
        for (i, &b) in bytes.iter().enumerate().take(SCREEN_WIDTH) {
            w.write_at(status_row, i, b, Color::White, Color::Red);
        }
        w.set_cursor_pos(status_row, bytes.len().min(SCREEN_WIDTH-1));
        w.show_cursor();
    });
    super::serial_render::draw_confirm_ansi(question);
    loop {
        crate::net::process_packets();
        if let Some(ev) = crate::drivers::keyboard::read_key() {
            use crate::drivers::keyboard::Key;
            match ev.key {
                Key::Char('y') | Key::Char('Y') => {
                    x86_64::instructions::interrupts::without_interrupts(|| { WRITER.lock().hide_cursor(); });
                    return Some(true);
                }
                Key::Char('n') | Key::Char('N') => {
                    x86_64::instructions::interrupts::without_interrupts(|| { WRITER.lock().hide_cursor(); });
                    return Some(false);
                }
                Key::Esc | Key::Ctrl('C') | Key::Ctrl('G') => {
                    x86_64::instructions::interrupts::without_interrupts(|| { WRITER.lock().hide_cursor(); });
                    return None;
                }
                _ => {}
            }
        }
        core::hint::spin_loop();
    }
}

/// Show help overlay full-screen scrollable
pub fn show_help_overlay() {
    // Clear and show help text
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
        "  Alt+# Toggle line numbers (not yet: placeholder)",
        "  Tab inserts 4 spaces; Home toggles start/indent",
        "  Arrows/Home/End/PgUp/PgDn  Navigate",
        "  Alt+W Word wrap / Justify  Alt+S Save alias",
        "  ESC or ^G to close help",
        "",
        " Status bar shows filename, modified [*], and line/col.",
        " Edit area shows $ at edges when line scrolled horizontally.",
        " Selection with mark is highlighted blue on white.",
        " Files limited to 6144 bytes (12 blocks) by SimplFS.",
        "",
        " Press any key to continue... (ESC/^G/^X to exit help)",
    ];

    let mut scroll: usize = 0;
    let max_visible = crate::editor::viewport::EDIT_HEIGHT + HEADER_HEIGHT + STATUS_HEIGHT + 1; // use most of screen minus help bar?

    loop {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut w = WRITER.lock();
            // Clear all
            for r in 0..crate::drivers::vga::VGA_HEIGHT {
                w.clear_row_with(r, Color::White, Color::Blue);
            }
            // Title
            w.fill_rect(0, 0, SCREEN_WIDTH, 1, b' ', Color::White, Color::Blue);
            let t = b" MFK nano Help (ESC to exit) ";
            for (i, &b) in t.iter().enumerate() {
                w.write_at(0, (SCREEN_WIDTH - t.len())/2 + i, b, Color::White, Color::Blue);
            }
            // Content rows 1..23
            let content_rows = crate::drivers::vga::VGA_HEIGHT - 2;
            for i in 0..content_rows {
                let idx = scroll + i;
                if idx >= HELP_TEXT.len() { break; }
                let line = HELP_TEXT[idx];
                for (c, b) in line.bytes().enumerate().take(SCREEN_WIDTH) {
                    w.write_at(1 + i, c, b, Color::White, Color::Blue);
                }
            }
            // Footer
            let fr = crate::drivers::vga::VGA_HEIGHT -1;
            w.fill_rect(fr, 0, SCREEN_WIDTH, 1, b' ', Color::Black, Color::White);
            let footer = b" [ ESC / ^G / ^X to close Help ]  [ PgUp/PgDn or Arrows to scroll ] ";
            for (i, &b) in footer.iter().enumerate().take(SCREEN_WIDTH) {
                w.write_at(fr, i, b, Color::Black, Color::White);
            }
        });
        super::serial_render::draw_help_ansi(scroll);

        // Wait for key
        let ev_opt = poll_key_blocking();
        if let Some(ev) = ev_opt {
            use crate::drivers::keyboard::Key;
            match ev.key {
                Key::Esc | Key::Ctrl('G') | Key::Ctrl('X') => break,
                Key::ArrowDown => { if scroll + max_visible < HELP_TEXT.len() { scroll += 1; } }
                Key::ArrowUp => { if scroll > 0 { scroll -= 1; } }
                Key::PageDown => { scroll = (scroll + 5).min(HELP_TEXT.len().saturating_sub(max_visible)); }
                Key::PageUp => { scroll = scroll.saturating_sub(5); }
                _ => break,
            }
        } else { break; }
    }
}

fn poll_key_blocking() -> Option<crate::drivers::keyboard::KeyEvent> {
    // blocking with net process
    loop {
        crate::net::process_packets();
        if let Some(ev) = crate::drivers::keyboard::read_key() { return Some(ev); }
        // also allow tick? spin
        for _ in 0..5000 { core::hint::spin_loop(); }
        // To avoid infinite lock if no input, we could timeout but for help we block
        // We break outer loop only on ESC; other keys close help
        // Instead we loop until key appears; but need to give chance to check maybe? Already.
    }
}

/// Simple status flash helper: set message for N ticks (caller stores deadline)
pub fn draw_status_message(msg: &str, is_error: bool, vp: &Viewport, buf: &TextBuffer) {
    draw_frame(buf, vp, Some(msg), is_error);
}

/// For debugging
pub fn clear_and_draw_status(msg: &str) {
    let row = EDIT_TOP + super::viewport::EDIT_HEIGHT;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        w.fill_rect(row, 0, SCREEN_WIDTH, 1, b' ', Color::Black, Color::Yellow);
        for (i, b) in msg.bytes().enumerate().take(SCREEN_WIDTH) {
            w.write_at(row, i, b, Color::Black, Color::Yellow);
        }
    });
}
