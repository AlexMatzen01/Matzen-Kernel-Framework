//! Single-line command editor: cursor movement, kill operations, history.
//!
//! Pure buffer logic (no rendering). Rendering lives in `super` (shell)
//! because it must drive three backends: VGA text, framebuffer, serial.

use alloc::vec::Vec;

/// Maximum bytes in one command line.
pub const MAX_LINE: usize = 256;
/// Maximum remembered commands for Up/Down recall.
pub const HISTORY_MAX: usize = 32;

/// Editable command line with history navigation.
pub struct LineEditor {
    buf: [u8; MAX_LINE],
    len: usize,
    cursor: usize,
    history: Vec<Vec<u8>>,
    /// Index into `history` while navigating with Up/Down, else None.
    hist_idx: Option<usize>,
    /// Line being typed before Up was first pressed (restored by Down-past-newest).
    draft: Vec<u8>,
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            buf: [0; MAX_LINE],
            len: 0,
            cursor: 0,
            history: Vec::new(),
            hist_idx: None,
            draft: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    fn break_nav(&mut self) {
        self.hist_idx = None;
        self.draft.clear();
    }

    /// Insert byte at cursor. Returns false when the line is full.
    pub fn insert(&mut self, b: u8) -> bool {
        if self.len >= MAX_LINE {
            return false;
        }
        let mut i = self.len;
        while i > self.cursor {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[self.cursor] = b;
        self.len += 1;
        self.cursor += 1;
        self.break_nav();
        true
    }

    /// Append byte at end of line (used by tab completion).
    pub fn append(&mut self, b: u8) -> bool {
        if self.len >= MAX_LINE {
            return false;
        }
        self.buf[self.len] = b;
        self.len += 1;
        self.cursor = self.len;
        self.break_nav();
        true
    }

    /// Delete the byte before the cursor. Returns false at line start.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.delete_at()
    }

    /// Delete the byte under the cursor. Returns false at line end.
    pub fn delete_at(&mut self) -> bool {
        if self.cursor >= self.len {
            return false;
        }
        let mut i = self.cursor;
        while i + 1 < self.len {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        self.break_nav();
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor > 0 {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor < self.len {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    pub fn home(&mut self) -> bool {
        if self.cursor != 0 {
            self.cursor = 0;
            true
        } else {
            false
        }
    }

    pub fn end(&mut self) -> bool {
        if self.cursor != self.len {
            self.cursor = self.len;
            true
        } else {
            false
        }
    }

    /// Ctrl+U: cut everything before the cursor.
    pub fn kill_to_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let n = self.cursor;
        let mut i = 0;
        while i + n < self.len {
            self.buf[i] = self.buf[i + n];
            i += 1;
        }
        self.len -= n;
        self.cursor = 0;
        self.break_nav();
        true
    }

    /// Ctrl+K: cut everything from the cursor to the end.
    pub fn kill_to_end(&mut self) -> bool {
        if self.cursor >= self.len {
            return false;
        }
        self.len = self.cursor;
        self.break_nav();
        true
    }

    /// Replace the whole line (history recall). Cursor goes to end.
    pub fn set_line(&mut self, line: &[u8]) {
        let n = line.len().min(MAX_LINE);
        self.buf[..n].copy_from_slice(&line[..n]);
        self.len = n;
        self.cursor = n;
    }

    /// Up: recall older command. Saves the in-progress draft on first press.
    /// Returns true when the visible line changed.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let idx = match self.hist_idx {
            None => {
                let cur = self.as_slice().to_vec();
                self.draft.clear();
                self.draft.extend_from_slice(&cur);
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(idx);
        let line = self.history[idx].clone();
        self.set_line(&line);
        true
    }

    /// Down: recall newer command, or restore the draft past the newest.
    /// Returns true when the visible line changed.
    pub fn history_next(&mut self) -> bool {
        let idx = match self.hist_idx {
            None => return false,
            Some(i) => i,
        };
        if idx + 1 < self.history.len() {
            let next = idx + 1;
            self.hist_idx = Some(next);
            let line = self.history[next].clone();
            self.set_line(&line);
        } else {
            self.hist_idx = None;
            let draft = self.draft.clone();
            self.set_line(&draft);
            self.draft.clear();
        }
        true
    }

    /// Record a submitted line. Skips empty lines and consecutive duplicates,
    /// evicts the oldest entry when full.
    pub fn push_history(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        if let Some(last) = self.history.last() {
            if last.as_slice() == line {
                return;
            }
        }
        if self.history.len() >= HISTORY_MAX {
            self.history.remove(0);
        }
        self.history.push(line.to_vec());
    }

    /// Clear the line and any history navigation (Enter / Ctrl+C).
    pub fn reset(&mut self) {
        self.len = 0;
        self.cursor = 0;
        self.hist_idx = None;
        self.draft.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed_of(s: &[u8]) -> LineEditor {
        let mut ed = LineEditor::new();
        for &b in s {
            assert!(ed.insert(b));
        }
        ed
    }

    #[test]
    fn insert_and_cursor_advance() {
        let mut ed = LineEditor::new();
        assert!(ed.insert(b'a'));
        assert!(ed.insert(b'b'));
        assert_eq!(ed.as_slice(), b"ab");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn insert_mid_line_shifts_right() {
        let mut ed = ed_of(b"ac");
        assert!(ed.move_left());
        assert!(ed.insert(b'b'));
        assert_eq!(ed.as_slice(), b"abc");
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn insert_full_line_rejected() {
        let mut ed = LineEditor::new();
        for _ in 0..MAX_LINE {
            assert!(ed.insert(b'x'));
        }
        assert!(!ed.insert(b'y'));
        assert_eq!(ed.len(), MAX_LINE);
    }

    #[test]
    fn backspace_deletes_before_cursor() {
        let mut ed = ed_of(b"abc");
        ed.move_left();
        assert!(ed.backspace());
        assert_eq!(ed.as_slice(), b"ac");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn backspace_at_start_does_nothing() {
        let mut ed = ed_of(b"a");
        ed.home();
        assert!(!ed.backspace());
        assert_eq!(ed.as_slice(), b"a");
    }

    #[test]
    fn delete_at_removes_under_cursor() {
        let mut ed = ed_of(b"abc");
        ed.home();
        assert!(ed.delete_at());
        assert_eq!(ed.as_slice(), b"bc");
        assert_eq!(ed.cursor(), 0);
        // At end: nothing to delete.
        ed.end();
        assert!(!ed.delete_at());
    }

    #[test]
    fn cursor_movement_clamps() {
        let mut ed = ed_of(b"ab");
        assert!(!ed.move_right());
        assert!(ed.move_left());
        assert!(ed.move_left());
        assert!(!ed.move_left());
        assert!(ed.home() == false); // already home
        assert!(ed.end());
        assert!(ed.home());
    }

    #[test]
    fn kill_ops() {
        let mut ed = ed_of(b"hello");
        ed.move_left();
        ed.move_left();
        assert!(ed.kill_to_start());
        assert_eq!(ed.as_slice(), b"lo");
        assert_eq!(ed.cursor(), 0);
        assert!(!ed.kill_to_start());

        let mut ed = ed_of(b"hello");
        ed.move_left();
        ed.move_left();
        assert!(ed.kill_to_end());
        assert_eq!(ed.as_slice(), b"hel");
        assert!(!ed.kill_to_end());
    }

    #[test]
    fn history_up_down_with_draft_restore() {
        let mut ed = LineEditor::new();
        ed.push_history(b"first");
        ed.push_history(b"second");
        for &b in b"dr" {
            ed.insert(b);
        }
        assert!(ed.history_prev());
        assert_eq!(ed.as_slice(), b"second");
        assert!(ed.history_prev());
        assert_eq!(ed.as_slice(), b"first");
        assert!(!ed.history_prev()); // oldest: no change
        assert!(ed.history_next());
        assert_eq!(ed.as_slice(), b"second");
        assert!(ed.history_next());
        assert_eq!(ed.as_slice(), b"dr"); // draft restored
        assert!(!ed.history_next()); // back to live line
    }

    #[test]
    fn history_skips_empty_and_dupes_and_caps() {
        let mut ed = LineEditor::new();
        ed.push_history(b"");
        assert_eq!(ed.history_len(), 0);
        ed.push_history(b"a");
        ed.push_history(b"a");
        assert_eq!(ed.history_len(), 1);
        for i in 0..40u32 {
            let s = alloc::format!("cmd{}", i);
            ed.push_history(s.as_bytes());
        }
        assert_eq!(ed.history_len(), HISTORY_MAX);
        // Oldest evicted ("a" and early cmds gone); newest kept.
        assert!(ed.history_prev());
        assert_eq!(ed.as_slice(), b"cmd39");
    }

    #[test]
    fn edit_breaks_history_nav() {
        let mut ed = LineEditor::new();
        ed.push_history(b"old");
        for &b in b"new" {
            ed.insert(b);
        }
        assert!(ed.history_prev());
        assert_eq!(ed.as_slice(), b"old");
        assert!(ed.insert(b'!'));
        // Nav was broken by the edit: Up starts over from current line.
        assert!(ed.history_prev());
        assert_eq!(ed.as_slice(), b"old");
    }

    #[test]
    fn reset_clears_all_state() {
        let mut ed = LineEditor::new();
        ed.push_history(b"x");
        for &b in b"y" {
            ed.insert(b);
        }
        assert!(ed.history_prev());
        ed.reset();
        assert!(ed.is_empty());
        assert_eq!(ed.cursor(), 0);
        assert!(!ed.history_next());
    }
}
