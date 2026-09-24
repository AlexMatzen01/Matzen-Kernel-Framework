//! Editor TextBuffer – core data model for nano-like editor
//! Manages lines, cursor, dirty flag, clipboard, undo (bounded), FS load/save

use alloc::string::String;
use alloc::vec::Vec;

/// Max undo history entries (to bound heap usage)
const MAX_UNDO: usize = 256;

/// Single undo action
#[derive(Debug, Clone)]
pub enum EditAction {
    InsertChar { row: usize, col: usize, ch: u8 },
    DeleteChar { row: usize, col: usize, ch: u8 },
    InsertNewline { row: usize, col: usize },
    DeleteNewline { row: usize, col: usize },
    InsertLine { row: usize, line: Vec<u8> },
    DeleteLine { row: usize, line: Vec<u8> },
}

/// Main buffer
pub struct TextBuffer {
    pub lines: Vec<Vec<u8>>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub desired_col: usize, // for vertical move memory
    pub dirty: bool,
    pub filename: Option<String>,
    pub clipboard: Vec<Vec<u8>>, // cut buffer (lines or partial)
    pub clipboard_is_lines: bool,
    pub mark: Option<(usize, usize)>, // (row,col) start of selection
    undo_stack: Vec<EditAction>,
    redo_stack: Vec<EditAction>,
    last_action_was_cut: bool,
}

impl TextBuffer {
    pub fn new_empty(filename: Option<String>) -> Self {
        Self {
            lines: alloc::vec![Vec::new()],
            cursor_row: 0,
            cursor_col: 0,
            desired_col: 0,
            dirty: false,
            filename,
            clipboard: Vec::new(),
            clipboard_is_lines: false,
            mark: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_action_was_cut: false,
        }
    }

    /// Load from FS; if not found create empty. Returns TextBuffer.
    pub fn load_or_new(filename: Option<String>) -> Self {
        if let Some(ref name) = filename {
            // Try to read from filesystem if mounted
            // We need to duplicate FS logic but use same device
            // Attempt to load; on failure return empty with filename
            if let Some(data) = Self::try_read_file(name) {
                return Self::from_bytes(data, Some(name.clone()));
            }
        }
        Self::new_empty(filename)
    }

    /// Try reading file via FS; returns None if not mounted or file not found
    fn try_read_file(name: &str) -> Option<Vec<u8>> {
        // We must not hold FILESYSTEM lock while doing device ops that re-lock?
        // Use same pattern as shell cat: check is_some outside, then device read inside
        // We replicate shell's logic but without prints
        // Return None if not mounted
        // Use crate::shell::is_mounted() helper if exists, else check via try_lock
        // Quick check: use shell::is_filesystem_mounted if exposed, else attempt lock
        // We'll attempt to use crate::shell::filesystem_mounted()
        // For now, directly attempt operation and swallow errors
        let mounted = crate::shell::is_mounted();
        if !mounted {
            return None;
        }
        let mut device = crate::shell::mounted_device();
        // Need to get file data via FS
        // We need access to FILESYSTEM – we will expose a helper in shell
        crate::shell::read_file_contents(name, &mut device)
    }

    pub fn from_bytes(data: Vec<u8>, filename: Option<String>) -> Self {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        for &b in &data {
            if b == b'\n' {
                lines.push(cur);
                cur = Vec::new();
            } else if b == b'\r' {
                // handle CRLF: ignore \r if followed by \n? We'll just skip \r
                continue;
            } else {
                // Clamp non-printable? Keep as is – editor will render placeholder
                cur.push(b);
            }
        }
        lines.push(cur);
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Self {
            lines,
            cursor_row: 0,
            cursor_col: 0,
            desired_col: 0,
            dirty: false,
            filename,
            clipboard: Vec::new(),
            clipboard_is_lines: false,
            mark: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_action_was_cut: false,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            out.extend_from_slice(line);
            if i + 1 < self.lines.len() {
                out.push(b'\n');
            }
        }
        out
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn current_line_len(&self) -> usize {
        self.lines
            .get(self.cursor_row)
            .map(|l| l.len())
            .unwrap_or(0)
    }

    // ── Cursor movement ─────────────────────────────────────

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    pub fn move_right(&mut self) {
        let len = self.current_line_len();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let len = self.lines[self.cursor_row].len();
            self.cursor_col = core::cmp::min(self.desired_col, len);
        }
        self.last_action_was_cut = false;
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let len = self.lines[self.cursor_row].len();
            self.cursor_col = core::cmp::min(self.desired_col, len);
        }
        self.last_action_was_cut = false;
    }

    pub fn move_home(&mut self) {
        // Nano home: toggle between column 0 and first non-blank
        let line = &self.lines[self.cursor_row];
        let first_nonblank = line
            .iter()
            .position(|&b| b != b' ' && b != b'\t')
            .unwrap_or(0);
        if self.cursor_col == first_nonblank {
            self.cursor_col = 0;
        } else {
            self.cursor_col = first_nonblank;
        }
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.current_line_len();
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    pub fn goto_row_col(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.lines.len().saturating_sub(1));
        let len = self.lines[self.cursor_row].len();
        self.cursor_col = col.min(len);
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    pub fn goto_line(&mut self, line_one_based: usize) {
        if line_one_based == 0 {
            return;
        }
        let row = (line_one_based - 1).min(self.lines.len().saturating_sub(1));
        self.cursor_row = row;
        self.cursor_col = 0;
        self.desired_col = 0;
        self.last_action_was_cut = false;
    }

    // ── Editing ────────────────────────────────────────────

    fn push_undo(&mut self, act: EditAction) {
        if self.undo_stack.len() >= MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(act);
        self.redo_stack.clear();
    }

    pub fn insert_char(&mut self, ch: u8) {
        // Handle tab as 4 spaces for consistency unless user prefers raw tab
        // We'll insert raw byte; rendering will expand tab
        let row = self.cursor_row;
        let col = self.cursor_col;
        self.push_undo(EditAction::InsertChar { row, col, ch });
        self.lines[row].insert(col, ch);
        self.cursor_col += 1;
        self.desired_col = self.cursor_col;
        self.dirty = true;
        self.last_action_was_cut = false;
    }

    pub fn insert_tab(&mut self) {
        // Insert 4 spaces (nano style) – but keep undo as single? For simplicity insert 4 chars
        for _ in 0..4 {
            self.insert_char(b' ');
        }
        // Coalesce undo: remove last 3 extra entries and keep one combined? Skip for now
        self.last_action_was_cut = false;
    }

    pub fn insert_newline(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        self.push_undo(EditAction::InsertNewline { row, col });
        let current = &mut self.lines[row];
        let right = current.split_off(col);
        self.lines.insert(row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.desired_col = 0;
        self.dirty = true;
        self.last_action_was_cut = false;
    }

    pub fn delete_prev(&mut self) {
        // Backspace
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = self.cursor_col - 1;
            let ch = self.lines[row][col];
            self.push_undo(EditAction::DeleteChar { row, col, ch });
            self.lines[row].remove(col);
            self.cursor_col -= 1;
            self.desired_col = self.cursor_col;
            self.dirty = true;
        } else if self.cursor_row > 0 {
            let row = self.cursor_row;
            let col = self.lines[row - 1].len();
            self.push_undo(EditAction::DeleteNewline { row: row - 1, col });
            let right = self.lines.remove(row);
            self.lines[row - 1].extend_from_slice(&right);
            self.cursor_row -= 1;
            self.cursor_col = col;
            self.desired_col = col;
            self.dirty = true;
        }
        self.last_action_was_cut = false;
    }

    pub fn delete_next(&mut self) {
        // Delete key
        let row = self.cursor_row;
        let col = self.cursor_col;
        let len = self.lines[row].len();
        if col < len {
            let ch = self.lines[row][col];
            self.push_undo(EditAction::DeleteChar { row, col, ch });
            self.lines[row].remove(col);
            self.dirty = true;
        } else if row + 1 < self.lines.len() {
            self.push_undo(EditAction::DeleteNewline { row, col });
            let right = self.lines.remove(row + 1);
            self.lines[row].extend_from_slice(&right);
            self.dirty = true;
        }
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
    }

    // ── Cut / Uncut ───────────────────────────────────────
    // Nano Ctrl+K cuts whole line (or selection). Ctrl+U pastes.
    // Behavior: consecutive Ctrl+K appends to clipboard; first cut replaces clipboard.

    pub fn cut_current_line(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        let row = self.cursor_row;
        let line = self.lines[row].clone();
        if !self.last_action_was_cut {
            self.clipboard.clear();
            self.clipboard_is_lines = true;
        }
        self.clipboard.push(line.clone());
        self.push_undo(EditAction::DeleteLine { row, line });
        self.lines.remove(row);
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
            self.cursor_row = 0;
            self.cursor_col = 0;
        } else if row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col = 0;
        }
        self.desired_col = 0;
        self.dirty = true;
        self.last_action_was_cut = true;
    }

    // Cut selection if mark set, else cut line
    pub fn cut_selection_or_line(&mut self) -> bool {
        if let Some((mr, mc)) = self.mark {
            if self.cut_selection(mr, mc) {
                return true;
            }
        }
        self.cut_current_line();
        true
    }

    fn cut_selection(&mut self, mr: usize, mc: usize) -> bool {
        // Normalize order
        let (sr, sc, er, ec) = if (mr, mc) <= (self.cursor_row, self.cursor_col) {
            (mr, mc, self.cursor_row, self.cursor_col)
        } else {
            (self.cursor_row, self.cursor_col, mr, mc)
        };
        if sr == er && sc == ec {
            return false;
        }
        // Extract
        let mut cut_data: Vec<Vec<u8>> = Vec::new();
        if sr == er {
            let line = &mut self.lines[sr];
            let seg: Vec<u8> = line[sc..ec].to_vec();
            cut_data.push(seg.clone());
            line.drain(sc..ec);
            self.cursor_row = sr;
            self.cursor_col = sc;
        } else {
            // Multi-line cut – tricky: we need to splice
            // Save tail of end line
            let end_tail = self.lines[er][ec..].to_vec();
            let start_head = self.lines[sr][..sc].to_vec();
            // Collect middle
            cut_data.push(self.lines[sr][sc..].to_vec());
            for r in (sr + 1)..er {
                cut_data.push(self.lines[r].clone());
            }
            cut_data.push(self.lines[er][..ec].to_vec());
            // Rebuild: keep sr, merge start_head + end_tail, remove middle+end
            self.lines[sr].truncate(sc);
            self.lines[sr].extend_from_slice(&end_tail);
            // Remove intermediate lines
            for _ in 0..(er - sr) {
                self.lines.remove(sr + 1);
            }
            self.cursor_row = sr;
            self.cursor_col = sc;
        }
        self.clipboard = cut_data;
        self.clipboard_is_lines = false;
        self.mark = None;
        self.dirty = true;
        self.desired_col = self.cursor_col;
        self.last_action_was_cut = false;
        true
    }

    pub fn copy_selection(&mut self) -> bool {
        if let Some((mr, mc)) = self.mark {
            let (sr, sc, er, ec) = if (mr, mc) <= (self.cursor_row, self.cursor_col) {
                (mr, mc, self.cursor_row, self.cursor_col)
            } else {
                (self.cursor_row, self.cursor_col, mr, mc)
            };
            if sr == er && sc == ec {
                return false;
            }
            let mut data: Vec<Vec<u8>> = Vec::new();
            if sr == er {
                data.push(self.lines[sr][sc..ec].to_vec());
            } else {
                data.push(self.lines[sr][sc..].to_vec());
                for r in sr + 1..er {
                    data.push(self.lines[r].clone());
                }
                data.push(self.lines[er][..ec].to_vec());
            }
            self.clipboard = data;
            self.clipboard_is_lines = false;
            self.mark = None;
            self.last_action_was_cut = false;
            return true;
        }
        false
    }

    pub fn uncut(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        if self.clipboard_is_lines {
            // Insert each clipboard line after current row
            let row = self.cursor_row;
            // If current line empty and clipboard lines, replace? Nano inserts at cursor line
            // We insert lines starting at cursor_row
            // Undo: we should push InsertLine actions
            let mut insert_at = row;
            // If current line is not empty, we split? Simpler: insert clipboard lines starting at row+1 if line not empty?
            // Nano behavior: uncut inserts at cursor line; if multiple lines, they appear starting at cursor
            // For single empty line buffer, replace
            if self.lines.len() == 1 && self.lines[0].is_empty() && self.clipboard.len() == 1 {
                let line = self.clipboard[0].clone();
                self.lines[0] = line;
                self.cursor_col = self.lines[0].len();
            } else {
                // If we cut lines, we stored each line as Vec<u8>; when pasting, we want to insert them
                // Insert first clipboard line at cursor position splitting current line
                // For simplicity emulate: insert clipboard lines as new lines at cursor_row
                // If current line empty, replace it with first clipboard line, then insert rest
                let cur_line = core::mem::replace(&mut self.lines[insert_at], Vec::new());
                let first = &self.clipboard[0];
                let mut new_first = cur_line[..self.cursor_col].to_vec();
                new_first.extend_from_slice(first);
                let after = cur_line[self.cursor_col..].to_vec();
                // If clipboard has only one line, merge after
                if self.clipboard.len() == 1 {
                    new_first.extend_from_slice(&after);
                    let first_len = first.len();
                    let first_clone = first.clone();
                    self.lines[insert_at] = new_first;
                    self.push_undo(EditAction::InsertLine {
                        row: insert_at,
                        line: first_clone,
                    });
                    self.cursor_col = first_len;
                } else {
                    // Multiple lines: first line is prefix+first, last line gets suffix, middle lines verbatim
                    self.lines[insert_at] = new_first;
                    for (i, clip_line) in self.clipboard.iter().enumerate().skip(1) {
                        insert_at += 1;
                        if i == self.clipboard.len() - 1 {
                            let mut last = clip_line.clone();
                            last.extend_from_slice(&after);
                            self.lines.insert(insert_at, last);
                        } else {
                            self.lines.insert(insert_at, clip_line.clone());
                        }
                    }
                    self.cursor_row = insert_at;
                    self.cursor_col = self.clipboard.last().map(|l| l.len()).unwrap_or(0);
                }
            }
            self.desired_col = self.cursor_col;
        } else {
            // Clipboard is selection fragment(s) – insert as inline or multi-line insert
            if self.clipboard.len() == 1 {
                let frag = self.clipboard[0].clone();
                let row = self.cursor_row;
                for (i, &b) in frag.iter().enumerate() {
                    self.lines[row].insert(self.cursor_col + i, b);
                }
                self.cursor_col += frag.len();
            } else {
                // Multi-line fragment: split current line at cursor, insert middle fragments as lines
                let row = self.cursor_row;
                let col = self.cursor_col;
                let suffix = self.lines[row][col..].to_vec();
                self.lines[row].truncate(col);
                self.lines[row].extend_from_slice(&self.clipboard[0]);
                let mut insert_at = row;
                for cli in self.clipboard.iter().skip(1).take(self.clipboard.len() - 2) {
                    insert_at += 1;
                    self.lines.insert(insert_at, cli.clone());
                }
                // Last fragment + suffix
                insert_at += 1;
                let mut last = self.clipboard.last().unwrap().clone();
                last.extend_from_slice(&suffix);
                self.lines.insert(insert_at, last);
                self.cursor_row = insert_at;
                self.cursor_col = self.clipboard.last().unwrap().len();
            }
            self.desired_col = self.cursor_col;
        }
        self.dirty = true;
        self.last_action_was_cut = false;
    }

    pub fn toggle_mark(&mut self) {
        if self.mark.is_some() {
            self.mark = None;
        } else {
            self.mark = Some((self.cursor_row, self.cursor_col));
        }
        self.last_action_was_cut = false;
    }

    // ── Search ────────────────────────────────────────────
    pub fn find_next(
        &self,
        needle: &[u8],
        start_row: usize,
        start_col: usize,
    ) -> Option<(usize, usize)> {
        if needle.is_empty() {
            return None;
        }
        for r in start_row..self.lines.len() {
            let line = &self.lines[r];
            let search_start = if r == start_row { start_col } else { 0 };
            if search_start > line.len() {
                continue;
            }
            if let Some(pos) = find_subslice(&line[search_start..], needle) {
                return Some((r, search_start + pos));
            }
        }
        // Wrap: search from start to start_row
        for r in 0..=start_row {
            let line = &self.lines[r];
            let end = if r == start_row {
                start_col
            } else {
                line.len()
            };
            if end == 0 {
                continue;
            }
            if let Some(pos) = find_subslice(&line[..end], needle) {
                return Some((r, pos));
            }
        }
        None
    }

    pub fn replace_next(
        &mut self,
        needle: &[u8],
        replacement: &[u8],
        start_row: usize,
        start_col: usize,
    ) -> Option<(usize, usize)> {
        if let Some((r, c)) = self.find_next(needle, start_row, start_col) {
            // Delete needle, insert replacement
            let line = &mut self.lines[r];
            line.drain(c..c + needle.len());
            for (i, &b) in replacement.iter().enumerate() {
                line.insert(c + i, b);
            }
            self.cursor_row = r;
            self.cursor_col = c + replacement.len();
            self.desired_col = self.cursor_col;
            self.dirty = true;
            return Some((r, c));
        }
        None
    }

    // ── Word-wise (for Ctrl+Arrow) ────────────────────────
    pub fn move_word_left(&mut self) {
        // Skip spaces, then skip word, then skip spaces again? Simple: move to start of previous word
        if self.cursor_col == 0 && self.cursor_row == 0 {
            return;
        }
        // If at start of line, go to prev line end
        if self.cursor_col == 0 {
            self.move_left();
        }
        let row = self.cursor_row;
        let line = &self.lines[row];
        let mut col = self.cursor_col;
        // Skip trailing spaces
        while col > 0 && is_space(line[col - 1]) {
            col -= 1;
        }
        // Skip word chars
        while col > 0 && !is_space(line[col - 1]) {
            col -= 1;
        }
        self.cursor_col = col;
        self.desired_col = col;
        self.last_action_was_cut = false;
    }

    pub fn move_word_right(&mut self) {
        let row = self.cursor_row;
        let line = &self.lines[row];
        let mut col = self.cursor_col;
        let len = line.len();
        if col >= len {
            // Move to next line start if exists
            if row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
                self.desired_col = 0;
            }
            return;
        }
        // Skip word
        while col < len && !is_space(line[col]) {
            col += 1;
        }
        // Skip spaces
        while col < len && is_space(line[col]) {
            col += 1;
        }
        self.cursor_col = col;
        self.desired_col = col;
        self.last_action_was_cut = false;
    }

    // ── Justify (Ctrl+J) – reflow paragraph ───────────────
    pub fn justify_paragraph(&mut self, width: usize) {
        // Find paragraph bounds: consecutive non-empty lines from cursor
        let mut start = self.cursor_row;
        while start > 0 && !self.lines[start - 1].is_empty() {
            start -= 1;
        }
        let mut end = self.cursor_row;
        while end + 1 < self.lines.len() && !self.lines[end + 1].is_empty() {
            end += 1;
        }
        // Extract words
        let mut words: Vec<Vec<u8>> = Vec::new();
        for r in start..=end {
            let line = &self.lines[r];
            let mut i = 0;
            while i < line.len() {
                while i < line.len() && is_space(line[i]) {
                    i += 1;
                }
                if i >= line.len() {
                    break;
                }
                let s = i;
                while i < line.len() && !is_space(line[i]) {
                    i += 1;
                }
                words.push(line[s..i].to_vec());
            }
        }
        if words.is_empty() {
            return;
        }
        // Rebuild lines
        let mut new_lines: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        for w in words {
            if cur.is_empty() {
                cur = w;
            } else if cur.len() + 1 + w.len() <= width {
                cur.push(b' ');
                cur.extend_from_slice(&w);
            } else {
                new_lines.push(cur);
                cur = w;
            }
        }
        new_lines.push(cur);
        // Replace range start..=end with new_lines
        let drain_len = end - start + 1;
        for _ in 0..drain_len {
            self.lines.remove(start);
        }
        for (i, nl) in new_lines.into_iter().enumerate() {
            self.lines.insert(start + i, nl);
        }
        self.cursor_row = start;
        self.cursor_col = 0;
        self.desired_col = 0;
        self.dirty = true;
        self.last_action_was_cut = false;
    }

    // ── Undo / Redo ───────────────────────────────────────
    pub fn undo(&mut self) -> bool {
        if let Some(act) = self.undo_stack.pop() {
            // Invert
            match act.clone() {
                EditAction::InsertChar { row, col, .. } => {
                    if row < self.lines.len() && col < self.lines[row].len() {
                        self.lines[row].remove(col);
                        if self.cursor_row == row && self.cursor_col > col {
                            self.cursor_col -= 1;
                        }
                    }
                }
                EditAction::DeleteChar { row, col, ch } => {
                    if row < self.lines.len() {
                        self.lines[row].insert(col, ch);
                        if self.cursor_row == row && self.cursor_col >= col {
                            self.cursor_col += 1;
                        }
                    }
                }
                EditAction::InsertNewline { row, col } => {
                    // Merge
                    if row + 1 < self.lines.len() {
                        let right = self.lines.remove(row + 1);
                        self.lines[row].extend_from_slice(&right);
                        self.cursor_row = row;
                        self.cursor_col = col;
                    }
                }
                EditAction::DeleteNewline { row, .. } => {
                    let cur = self.lines[row].len();
                    // We stored col = len before split, need to split
                    // Actually DeleteNewline stored col = original col before merge
                    // Insertion: split at col
                    let col = act_col(&act);
                    let right = self.lines[row].split_off(col);
                    self.lines.insert(row + 1, right);
                    self.cursor_row = row + 1;
                    self.cursor_col = 0;
                }
                EditAction::DeleteLine { row, line } => {
                    self.lines.insert(row, line);
                }
                EditAction::InsertLine { row, .. } => {
                    if row < self.lines.len() {
                        self.lines.remove(row);
                    }
                    if self.lines.is_empty() {
                        self.lines.push(Vec::new());
                    }
                }
            }
            self.redo_stack.push(act);
            self.dirty = true;
            self.last_action_was_cut = false;
            return true;
        }
        false
    }

    pub fn redo(&mut self) -> bool {
        if let Some(act) = self.redo_stack.pop() {
            // Re-apply original
            match act.clone() {
                EditAction::InsertChar { row, col, ch } => {
                    if row < self.lines.len() {
                        self.lines[row].insert(col, ch);
                    }
                }
                EditAction::DeleteChar { row, col, .. } => {
                    if row < self.lines.len() && col < self.lines[row].len() {
                        self.lines[row].remove(col);
                    }
                }
                EditAction::InsertNewline { row, col } => {
                    if row < self.lines.len() {
                        let right = self.lines[row].split_off(col);
                        self.lines.insert(row + 1, right);
                    }
                }
                EditAction::DeleteNewline { row, .. } => {
                    if row + 1 < self.lines.len() {
                        let right = self.lines.remove(row + 1);
                        self.lines[row].extend_from_slice(&right);
                    }
                }
                EditAction::DeleteLine { row, .. } => {
                    if row < self.lines.len() {
                        self.lines.remove(row);
                    }
                    if self.lines.is_empty() {
                        self.lines.push(Vec::new());
                    }
                }
                EditAction::InsertLine { row, line } => {
                    self.lines.insert(row, line);
                }
            }
            self.undo_stack.push(act);
            self.dirty = true;
            return true;
        }
        false
    }

    // ── Save ──────────────────────────────────────────────
    pub fn save(&mut self) -> Result<usize, &'static str> {
        let name = self.filename.clone().ok_or("No filename")?;
        self.save_as(&name)
    }

    pub fn save_as(&mut self, name: &str) -> Result<usize, &'static str> {
        if name.len() >= crate::fs::MAX_FILENAME_LEN {
            return Err("Filename too long");
        }
        if !crate::shell::is_mounted() {
            return Err("Filesystem not mounted");
        }
        let data = self.to_bytes();
        // Use shell's helper to write – it creates if not exists
        let mut device = crate::shell::mounted_device();
        crate::shell::write_file_contents(name, &data, &mut device)?;
        self.filename = Some(String::from(name));
        self.dirty = false;
        Ok(data.len())
    }
}

fn act_col(a: &EditAction) -> usize {
    match a {
        EditAction::DeleteNewline { col, .. } => *col,
        _ => 0,
    }
}

fn is_space(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
