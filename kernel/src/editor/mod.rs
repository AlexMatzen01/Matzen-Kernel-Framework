//! Nano-like editor – main loop & integration
//! Provides `run(filename: Option<&str>)` called from shell

pub mod buffer;
pub mod viewport;
pub mod ui;
pub mod serial_render;

use buffer::TextBuffer;
use viewport::Viewport;
use crate::drivers::keyboard::{Key, KeyEvent};
use crate::drivers::vga;
use alloc::string::String;
use alloc::vec::Vec;

/// Entry: takes over screen, returns when user exits editor
pub fn run(args: &str) {
    let args = args.trim();
    // Parse args: handle -h/--help, filename; ignore -m etc for compat
    if args == "-h" || args == "--help" || args == "help" {
        show_help_brief();
        return;
    }

    let filename: Option<String> = if args.is_empty() {
        None
    } else {
        // Take first token as filename (strip quotes?)
        let tok = args.split_whitespace().next().unwrap_or("");
        if tok.is_empty() { None } else { Some(String::from(tok)) }
    };

    // Check mount for existing file load? Allow even if not mounted but warn
    // We still enter editor even without mount – save will fail with status error
    let mut buf = TextBuffer::load_or_new(filename);
    let mut vp = Viewport::new();
    let mut status_msg: Option<(String, bool, u64)> = None; // (msg, is_error, expire_tick)
    let mut status_is_error = false;

    // Take over screen – add diagnostic serial log for QEMU freeze diagnosis (visible via `serial` log)
    crate::serial_println!("editor: enter run args={:?}", args);
    crate::serial_println!("editor: buf lines={} filename={:?}", buf.lines.len(), buf.filename);
    vga::hide_cursor();
    crate::serial_println!("editor: hide_cursor done, before first draw");
    draw_and_position(&buf, &mut vp, &status_msg);
    crate::serial_println!("editor: first draw done");

    // Show initial message like nano
    let init_msg = if buf.filename.is_some() {
        if crate::shell::is_mounted() {
            alloc::format!(" [ Read {} lines ]  File: {}", buf.lines.len(), buf.filename.as_ref().unwrap())
        } else {
            String::from(" [ Filesystem not mounted – save will fail ]")
        }
    } else {
        String::from(" [ New File ]")
    };
    crate::serial_println!("editor: init_msg {:?}", init_msg);
    status_msg = Some((init_msg, false, crate::shell::get_tick_count() + 3000));
    draw_and_position(&buf, &mut vp, &status_msg);
    crate::serial_println!("editor: second draw done, entering loop");

    // Main loop
    loop {
        // Keep tick progressing even though shell::run not running
        crate::shell::increment_tick();

        // Check status expiry
        if let Some((_, _, exp)) = &status_msg {
            if crate::shell::get_tick_count() > *exp {
                status_msg = None;
                crate::serial_println!("editor: status expiry -> redraw");
                draw_and_position(&buf, &mut vp, &status_msg);
            }
        }

        crate::net::process_packets();

        let Some(ev) = crate::drivers::keyboard::read_key() else {
            x86_64::instructions::hlt();
            continue;
        };

        let mut needs_redraw = true;
        let mut new_status: Option<(String,bool)> = None;
        let mut status_error = false;

        match ev.key {
            Key::Ctrl('X') | Key::F(2) => {
                // Exit
                if buf.dirty {
                    // Use UI confirm overlay
                    ui::draw_frame(&buf, &vp, Some("Save modified buffer?"), true);
                    // Use confirm prompt on status bar? Already frame shows; now prompt
                    if let Some(answer) = ui::confirm("Save modified buffer?") {
                        if answer {
                            // Try save
                            let target = buf.filename.clone();
                            if let Some(name) = target {
                                match buf.save_as(&name) {
                                    Ok(n) => {
                                        new_status = Some((alloc::format!("Wrote {} bytes to {}", n, name), false));
                                    }
                                    Err(e) => {
                                        new_status = Some((alloc::format!("Error: {}", e), true));
                                        status_error = true;
                                        // Don't exit; show error
                                        needs_redraw = true;
                                        status_msg = new_status.map(|(m,_)| (m, true, crate::shell::get_tick_count()+4000));
                                        draw_and_position(&buf, &mut vp, &status_msg);
                                        continue;
                                    }
                                }
                            } else {
                                // No filename – prompt
                                if let Some(fname) = ui::prompt_with_input("File Name to Write:", None) {
                                    if !fname.is_empty() {
                                        match buf.save_as(&fname) {
                                            Ok(n) => { new_status = Some((alloc::format!("Wrote {} bytes to {}", n, fname), false)); }
                                            Err(e) => {
                                                new_status = Some((alloc::format!("Error: {}", e), true));
                                                status_error = true;
                                                status_msg = new_status.map(|(m,_)| (m, true, crate::shell::get_tick_count()+4000));
                                                draw_and_position(&buf, &mut vp, &status_msg);
                                                continue;
                                            }
                                        }
                                    } else {
                                        // Cancel save but still exit? In nano, cancel stays. We treat empty as cancel
                                        new_status = Some((String::from("Cancelled"), false));
                                        status_msg = new_status.map(|(m,_)| (m, false, crate::shell::get_tick_count()+2000));
                                        draw_and_position(&buf, &mut vp, &status_msg);
                                        continue;
                                    }
                                } else {
                                    // Cancel
                                    draw_and_position(&buf, &mut vp, &status_msg);
                                    continue;
                                }
                            }
                        } else {
                            // No – discard, exit without save
                        }
                    } else {
                        // Cancel (Ctrl+C/Esc)
                        draw_and_position(&buf, &mut vp, &status_msg);
                        continue;
                    }
                }
                break;
            }
            Key::Ctrl('O') | Key::F(3) | Key::Ctrl('S') => {
                // Write Out
                let default = buf.filename.clone().unwrap_or_default();
                let prompt_default = if default.is_empty() { None } else { Some(default.as_str()) };
                if let Some(fname) = ui::prompt_with_input("File Name to Write:", prompt_default) {
                    if fname.is_empty() {
                        new_status = Some((String::from("Cancelled"), false));
                    } else {
                        match buf.save_as(&fname) {
                            Ok(n) => new_status = Some((alloc::format!("Wrote {} lines ({} bytes) to {}", buf.lines.len(), n, fname), false)),
                            Err(e) => { new_status = Some((alloc::format!("Error writing file: {}", e), true)); status_error = true; }
                        }
                    }
                } else {
                    new_status = Some((String::from("Cancelled"), false));
                }
            }
            Key::Ctrl('G') | Key::F(1) => {
                ui::show_help_overlay();
                // After help, need full redraw
                needs_redraw = true;
            }
            Key::Ctrl('W') | Key::F(6) => {
                // Where Is – search
                if let Some(needle) = ui::prompt_with_input("Search:", None) {
                    if !needle.is_empty() {
                        let start_row = buf.cursor_row;
                        let start_col = buf.cursor_col + 1; // start after cursor like nano
                        let sc = if start_col > buf.lines[start_row].len() { 0 } else { start_col };
                        let sr = if sc == 0 { (start_row + 1).min(buf.lines.len()-1) } else { start_row };
                        // Adjust: if col beyond line, move to next line start
                        let (srow, scol) = if start_col > buf.lines[start_row].len() {
                            ((start_row +1).min(buf.lines.len()-1), 0)
                        } else { (start_row, start_col) };
                        if let Some((r,c)) = buf.find_next(needle.as_bytes(), srow, scol) {
                            buf.goto_row_col(r, c);
                            vp.ensure_cursor_visible(&buf);
                            new_status = Some((alloc::format!("Found \"{}\" at line {}, col {}", needle, r+1, c+1), false));
                        } else {
                            new_status = Some((alloc::format!("\"{}\" not found", needle), true));
                            status_error = true;
                        }
                    }
                }
            }
            Key::Ctrl(c) if c == '\\' => {
                // Replace: actually Ctrl+\ sends Ctrl+'\'? Our map: Ctrl+'\\' => 28? Need handle. For now also Alt+R via fallback
                // We'll treat Ctrl+\ as Replace as well as Alt+R
                goto_replace(&mut buf, &mut vp, &mut new_status, &mut status_error);
            }
            Key::Char(c) if ev.alt && (c == 'r' || c == 'R') => {
                goto_replace(&mut buf, &mut vp, &mut new_status, &mut status_error);
            }
            // Alt+R already handled, Ctrl+\ fallback
            Key::Ctrl('K') | Key::F(9) => {
                // Cut – if mark set cut selection else line
                if buf.mark.is_some() {
                    if buf.copy_selection() { /* actually cut */ }
                    // Need to cut selection: reuse cut_selection_or_line
                }
                buf.cut_selection_or_line();
                new_status = Some((alloc::format!("Cut {} line(s) → buffer ({} fragments)", buf.clipboard.len(), buf.clipboard.len()), false));
            }
            Key::Ctrl('U') | Key::F(10) => {
                if buf.clipboard.is_empty() {
                    new_status = Some((String::from("Cutbuffer empty"), true)); status_error = true;
                } else {
                    let pre_lines = buf.lines.len();
                    buf.uncut();
                    vp.ensure_cursor_visible(&buf);
                    new_status = Some((alloc::format!("Uncut (pasted) {} fragment(s) [{}→{} lines]", buf.clipboard.len(), pre_lines, buf.lines.len()), false));
                }
            }
            Key::Ctrl('C') | Key::F(11) => {
                // Cur Pos
                let total_chars: usize = buf.lines.iter().map(|l| l.len() + 1).sum::<usize>().saturating_sub(1);
                let cur_line_len = buf.lines[buf.cursor_row].len();
                let pct = if buf.lines.len() > 0 { (buf.cursor_row * 100)/ buf.lines.len() } else {0};
                new_status = Some((alloc::format!("line {}/{}, col {}/{} ({}%) char {}/{}", buf.cursor_row+1, buf.lines.len(), buf.cursor_col+1, cur_line_len+1, pct, total_chars, total_chars), false));
            }
            Key::Ctrl('_') | Key::F(13) => {
                // Go to line – also Alt+G fallback handled below
                if let Some(inp) = ui::prompt_with_input("Enter line number, column number:", None) {
                    // Parse "line" or "line,col"
                    let parts: Vec<&str> = inp.split(',').collect();
                    let line_no: usize = parts.get(0).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                    let col_no: usize = parts.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                    if line_no >=1 && line_no <= buf.lines.len() {
                        buf.goto_row_col(line_no-1, col_no.saturating_sub(1));
                        vp.ensure_cursor_visible(&buf);
                        new_status = Some((alloc::format!("Go to line {}", line_no), false));
                    } else if line_no > buf.lines.len() {
                        buf.goto_row_col(buf.lines.len()-1, 0);
                        vp.ensure_cursor_visible(&buf);
                        new_status = Some((alloc::format!("Goto past EOF – at line {}", buf.lines.len()), true)); status_error = true;
                    } else {
                        new_status = Some((String::from("Invalid line number"), true)); status_error=true;
                    }
                }
            }
            Key::Ctrl('J') | Key::F(4) => {
                // Justify paragraph
                let width = 80 - 2;
                buf.justify_paragraph(width);
                vp.ensure_cursor_visible(&buf);
                new_status = Some((String::from("Justified paragraph"), false));
            }
            Key::Ctrl('R') | Key::F(5) => {
                // Read file (insert file)
                if let Some(fname) = ui::prompt_with_input("File to insert [from current dir]:", None) {
                    if !fname.is_empty() {
                        if !crate::shell::is_mounted() {
                            new_status = Some((String::from("Filesystem not mounted"), true)); status_error=true;
                        } else {
                            let mut dev = crate::drivers::block::AtaBlockDevice::new();
                            if let Some(data) = crate::shell::read_file_contents(&fname, &mut dev) {
                                // Split into lines and insert at cursor
                                let mut extra: Vec<Vec<u8>> = Vec::new();
                                let mut cur: Vec<u8> = Vec::new();
                                for &b in &data {
                                    if b == b'\n' { extra.push(cur); cur = Vec::new(); } else if b != b'\r' { cur.push(b); }
                                }
                                extra.push(cur);
                                // Insert extra lines at cursor_row after splitting current line
                                let row = buf.cursor_row;
                                let col = buf.cursor_col;
                                let tail = buf.lines[row][col..].to_vec();
                                buf.lines[row].truncate(col);
                                // First inserted line appends to current
                                if !extra.is_empty() {
                                    buf.lines[row].extend_from_slice(&extra[0]);
                                    // Insert remaining
                                    for (i, l) in extra.iter().skip(1).enumerate() {
                                        buf.lines.insert(row+1+i, l.clone());
                                    }
                                    // Last line gets tail
                                    let last_idx = row + extra.len() -1;
                                    buf.lines[last_idx].extend_from_slice(&tail);
                                    buf.cursor_row = last_idx;
                                    buf.cursor_col = extra.last().map(|l| l.len()).unwrap_or(0);
                                    buf.desired_col = buf.cursor_col;
                                    buf.dirty = true;
                                    vp.ensure_cursor_visible(&buf);
                                    new_status = Some((alloc::format!("Inserted {} lines from {}", extra.len(), fname), false));
                                }
                            } else {
                                new_status = Some((alloc::format!("Failed to read \"{}\"", fname), true)); status_error=true;
                            }
                        }
                    }
                }
            }
            Key::Ctrl('Z') => { // Undo alias also Ctrl+Z
                if buf.undo() {
                    vp.ensure_cursor_visible(&buf);
                    new_status = Some((String::from("Undone"), false));
                } else {
                    new_status = Some((String::from("Nothing to undo"), true)); status_error=true;
                }
            }
            Key::Ctrl('Y') => {
                if buf.redo() {
                    vp.ensure_cursor_visible(&buf);
                    new_status = Some((String::from("Redone"), false));
                } else {
                    new_status = Some((String::from("Nothing to redo"), true)); status_error=true;
                }
            }
            Key::Char(c) if ev.alt && (c == 'u' || c == 'U') => {
                if buf.undo() { vp.ensure_cursor_visible(&buf); new_status = Some((String::from("Undone (Alt+U)"), false)); }
                else { new_status = Some((String::from("Nothing to undo"), true)); status_error=true; }
            }
            Key::Char(c) if ev.alt && (c == 'e' || c == 'E') => {
                if buf.redo() { vp.ensure_cursor_visible(&buf); new_status = Some((String::from("Redone (Alt+E)"), false)); }
                else { new_status = Some((String::from("Nothing to redo"), true)); status_error=true; }
            }
            Key::Ctrl('^') | Key::Ctrl('6') => { // Ctrl+6 is 0x1E -> '^' (also handle '6' for robustness)
                buf.toggle_mark();
                new_status = Some((if buf.mark.is_some() {String::from("Mark Set (^6)")} else {String::from("Mark Unset")}, false));
            }
            Key::Char(c) if ev.alt && (c == 'a' || c == 'A' || c == 'g' || c == 'G') => {
                // Alt+A mark, Alt+G also goto line (nano supports both) – we treat Alt+G as goto, Alt+A as mark
                if c == 'g' || c == 'G' {
                    // Goto line via Alt+G
                    if let Some(inp) = ui::prompt_with_input("Enter line number, column number:", None) {
                        let parts: Vec<&str> = inp.split(',').collect();
                        let line_no: usize = parts.get(0).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                        let col_no: usize = parts.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                        if line_no >=1 && line_no <= buf.lines.len() {
                            buf.goto_row_col(line_no-1, col_no.saturating_sub(1));
                            vp.ensure_cursor_visible(&buf);
                            new_status = Some((alloc::format!("Go to line {} (Alt+G)", line_no), false));
                        } else {
                            new_status = Some((String::from("Invalid line number"), true)); status_error=true;
                        }
                    }
                } else {
                    buf.toggle_mark();
                    new_status = Some((if buf.mark.is_some() {String::from("Mark Set (Alt+A)")} else {String::from("Mark Unset")}, false));
                }
            }
            // Navigation
            Key::ArrowLeft => buf.move_left(),
            Key::ArrowRight => buf.move_right(),
            Key::ArrowUp => buf.move_up(),
            Key::ArrowDown => buf.move_down(),
            Key::Home => buf.move_home(),
            Key::End => buf.move_end(),
            Key::PageUp => vp.page_up(&mut buf),
            Key::PageDown => vp.page_down(&mut buf),
            Key::Delete => buf.delete_next(),
            Key::Backspace => buf.delete_prev(),
            Key::Enter => buf.insert_newline(),
            Key::Tab => buf.insert_tab(),
            Key::Char(c) => {
                // Filter control chars – else insert
                if !c.is_control() {
                    buf.insert_char(c as u8);
                } else {
                    needs_redraw = false; // ignore
                }
            }
            Key::Esc => {
                // In nano Esc alone does nothing but cancel mark? We'll clear mark
                if buf.mark.is_some() {
                    buf.mark = None;
                    new_status = Some((String::from("Mark cleared (ESC)"), false));
                } else {
                    needs_redraw = false;
                }
            }
            _ => {
                needs_redraw = false;
            }
        }

        // Also handle Ctrl+Arrow word-wise via checking ctrl with arrow?
        // Our read_key maps Ctrl+Arrow as Arrow with shift/alt flags? Actually MOD_STATE for arrows still pushes Arrow key with alt flag but ctrl flag not yet differentiated
        // We didn't set separate ctrl flag for arrows – we could inspect MOD_STATE but we lost after; for now handle extra:
        // Check if ev was Ctrl+Arrow by looking at MOD_STATE at time? Our BUFFER already set shift/alt but ctrl for arrows not specially mapped to Ctrl+Arrow -> still Arrow with ctrl flag set
        // Let's handle word moves via detecting if original ev had ctrl flag (we stored in KeyEvent.ctrl not used). The above match only checked Key variant, so we need second pass:
        // If original event had ctrl flag and arrow, treat as word.
        // For now we can check if we just processed Arrow but ev had `alt`? Not. Simpler: check ctrl word handling before above match?
        // We'll adjust: if ctrl flag is set for arrows, move word
        // Need to know if ev had ctrl – but we didn't handle above. Let's handle after:
        if ev.ctrl_held() && matches!(ev.key, Key::ArrowLeft | Key::ArrowRight) {
            // Undo previous single char move and do word move
            // We already did single-step; revert and word-move for better UX? For simplicity we do word move additionally
            // But we already moved one; undo by moving opposite then word
            match ev.key {
                Key::ArrowLeft => { buf.move_right(); buf.move_word_left(); }
                Key::ArrowRight => { buf.move_left(); buf.move_word_right(); }
                _ => {}
            }
            needs_redraw = true;
        }

        if let Some((m,_)) = &new_status {
            // Set timed status
            status_msg = Some((m.clone(), status_error, crate::shell::get_tick_count()+3500));
        } else if needs_redraw {
            // Update viewport ensure
            vp.ensure_cursor_visible(&buf);
        }

        if needs_redraw {
            draw_and_position(&buf, &mut vp, &status_msg);
        } else if new_status.is_some() {
            draw_and_position(&buf, &mut vp, &status_msg);
        }
    }

    // Restore shell screen – both VGA and serial (ANSI reset)
    vga::clear_screen();
    crate::serial_print!("\x1b[2J\x1b[H\x1b[0m\x1b[?25h");
    crate::println!();
    crate::println!("Exited nano. Type 'help' for commands.");
}

/// Helper to check ctrl held
trait CtrlHeld { fn ctrl_held(&self)->bool; }
impl CtrlHeld for KeyEvent {
    fn ctrl_held(&self)->bool { self.ctrl }
}

fn draw_and_position(buf: &TextBuffer, vp: &mut Viewport, status: &Option<(String,bool,u64)>) {
    // Draw frame with optional status – dual render: VGA (VBox GUI) + Serial ANSI (QEMU -display none)
    match status {
        Some((msg, is_error, _)) => {
            ui::draw_frame(buf, vp, Some(msg.as_str()), *is_error);
            serial_render::draw_frame_ansi(buf, vp, Some(msg.as_str()), *is_error);
        },
        None => {
            ui::draw_frame(buf, vp, None, false);
            serial_render::draw_frame_ansi(buf, vp, None, false);
        },
    }
    // Position hardware cursor
    if let Some(screen_row) = vp.buffer_to_screen_row(buf.cursor_row) {
        let screen_col = (buf.cursor_col.saturating_sub(vp.left_col)).min(79);
        vga::set_cursor_pos(screen_row, screen_col);
        vga::show_cursor();
    } else {
        // Cursor not visible – hide or at edge
        vga::hide_cursor();
    }
    // Serial cursor is already positioned inside draw_frame_ansi, but also ensure show state kept
}

fn goto_replace(buf: &mut TextBuffer, vp: &mut Viewport, new_status: &mut Option<(String,bool)>, status_error: &mut bool) {
    if let Some(needle) = ui::prompt_with_input("Search (to replace):", None) {
        if needle.is_empty() { return; }
        if let Some(repl) = ui::prompt_with_input("Replace with:", None) {
            // Find and prompt each?
            let start_row = buf.cursor_row;
            let start_col = buf.cursor_col;
            // For simplicity, replace first occurrence after cursor, and if not found wrap
            let (srow, scol) = if start_col + 1 <= buf.lines[start_row].len() { (start_row, start_col+1) } else { ((start_row+1).min(buf.lines.len()-1), 0) };
            if let Some((r,c)) = buf.find_next(needle.as_bytes(), srow, scol) {
                // Highlight found – move cursor there, show confirm prompt
                buf.goto_row_col(r,c);
                vp.ensure_cursor_visible(buf);
                draw_and_position(buf, vp, &None);
                // Show replace confirm?
                if let Some(yes) = ui::confirm(&alloc::format!("Replace \"{}\" with \"{}\"?", needle, repl)) {
                    if yes {
                        buf.replace_next(needle.as_bytes(), repl.as_bytes(), r, c);
                        vp.ensure_cursor_visible(buf);
                        *new_status = Some((alloc::format!("Replaced \"{}\" at {}:{}", needle, r+1, c+1), false));
                    } else {
                        *new_status = Some((String::from("Not replaced"), false));
                    }
                }
            } else {
                *new_status = Some((alloc::format!("\"{}\" not found", needle), true));
                *status_error = true;
            }
        }
    }
}

fn show_help_brief() {
    crate::println!("Usage: nano [file]");
    crate::println!("       edit [file]   (alias)");
    crate::println!("       mfkedit [file] (alias)");
    crate::println!();
    crate::println!("Nano-like editor for MFK. Full-screen 80x25.");
    crate::println!("Controls (nano compatible):");
    crate::println!("  ^G  Help      ^O  WriteOut  ^W  WhereIs   ^K  Cut");
    crate::println!("  ^X  Exit      ^R  ReadFile  ^\\  Replace   ^U  Uncut");
    crate::println!("  ^C  CurPos    ^_  GoToLine  ^J  Justify   ^Z  Undo");
    crate::println!("  ^Y  Redo      ^6  Mark(Set) Alt+A Mark   Tab=4spaces");
    crate::println!("  Arrows/Home/End/PgUp/PgDn, Ctrl+S save, ESC clear mark");
    crate::println!();
    crate::println!("Filesystem must be mounted ('mount') before editing.");
}
