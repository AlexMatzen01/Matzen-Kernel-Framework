//! Viewport – maps TextBuffer cursor to screen coordinates
//! Handles vertical/horizontal scroll, ensures cursor visible

use super::buffer::TextBuffer;

pub const SCREEN_WIDTH: usize = 80;
pub const SCREEN_HEIGHT: usize = 25;
pub const HEADER_HEIGHT: usize = 1;
pub const STATUS_HEIGHT: usize = 1;
pub const HELP_HEIGHT: usize = 2;
pub const EDIT_TOP: usize = HEADER_HEIGHT; // row 1
pub const EDIT_HEIGHT: usize = SCREEN_HEIGHT - HEADER_HEIGHT - STATUS_HEIGHT - HELP_HEIGHT; // 21
pub const EDIT_BOTTOM: usize = EDIT_TOP + EDIT_HEIGHT - 1; // 21

#[derive(Debug, Clone)]
pub struct Viewport {
    pub top_line: usize, // first buffer line visible
    pub left_col: usize, // first column visible (horizontal scroll)
    pub width: usize,    // usually 80
    pub height: usize,   // EDIT_HEIGHT
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            top_line: 0,
            left_col: 0,
            width: SCREEN_WIDTH,
            height: EDIT_HEIGHT,
        }
    }

    /// Ensure cursor is within viewport, adjusting top_line/left_col
    pub fn ensure_cursor_visible(&mut self, buf: &TextBuffer) {
        let row = buf.cursor_row;
        let col = buf.cursor_col;

        // Vertical
        if row < self.top_line {
            self.top_line = row;
        } else if row >= self.top_line + self.height {
            self.top_line = row + 1 - self.height;
        }

        // Horizontal – keep cursor with margin 5
        let margin = 5;
        if col < self.left_col + margin {
            self.left_col = col.saturating_sub(margin);
        } else if col >= self.left_col + self.width - margin {
            self.left_col = (col + margin + 1).saturating_sub(self.width);
        }
        // Clamp left_col to avoid underflow large
        // Also don't scroll beyond line length too far
    }

    pub fn ensure_line_visible(&mut self, row: usize) {
        if row < self.top_line {
            self.top_line = row;
        } else if row >= self.top_line + self.height {
            self.top_line = row + 1 - self.height;
        }
    }

    /// Convert buffer row to screen row
    pub fn buffer_to_screen_row(&self, buf_row: usize) -> Option<usize> {
        if buf_row < self.top_line {
            return None;
        }
        let rel = buf_row - self.top_line;
        if rel >= self.height {
            return None;
        }
        Some(EDIT_TOP + rel)
    }

    pub fn screen_row(&self, buf_row: usize) -> usize {
        EDIT_TOP + (buf_row.saturating_sub(self.top_line)).min(self.height - 1)
    }

    pub fn screen_col(&self, buf_col: usize) -> usize {
        buf_col.saturating_sub(self.left_col)
    }

    pub fn is_visible(&self, buf_row: usize) -> bool {
        buf_row >= self.top_line && buf_row < self.top_line + self.height
    }

    pub fn page_up(&mut self, buf: &mut TextBuffer) {
        let jump = self.height.saturating_sub(1);
        if buf.cursor_row >= jump {
            buf.cursor_row -= jump;
        } else {
            buf.cursor_row = 0;
        }
        let len = buf.lines[buf.cursor_row].len();
        buf.cursor_col = core::cmp::min(buf.desired_col, len);
        self.ensure_cursor_visible(buf);
    }

    pub fn page_down(&mut self, buf: &mut TextBuffer) {
        let jump = self.height.saturating_sub(1);
        buf.cursor_row = (buf.cursor_row + jump).min(buf.lines.len().saturating_sub(1));
        let len = buf.lines[buf.cursor_row].len();
        buf.cursor_col = core::cmp::min(buf.desired_col, len);
        self.ensure_cursor_visible(buf);
    }
}
