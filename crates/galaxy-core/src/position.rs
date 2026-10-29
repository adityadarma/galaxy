//! Position conversions (M1-02).
//!
//! The canonical position everywhere in the core is a **char offset** into
//! the document (Unicode scalar values, LF line breaks). Other coordinate
//! systems are derived on demand:
//!
//! - `Position` (line, col in chars): UI and editing.
//! - `Utf16Position` (line, col in UTF-16 code units): LSP `Position`.
//! - `Utf8Position` (line, col in bytes): tree-sitter `Point`.
//!
//! All `*_to_char` conversions clamp out-of-range input instead of
//! panicking: a stale position from the UI or a language engine must never
//! crash the editor. Columns past the end of a line clamp to the line end
//! (before its `\n`), and a column in the middle of a multi-unit character
//! rounds down to the start of that character.

use ropey::Rope;

use crate::document::Document;

/// Zero-based line and column, column counted in chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

/// Zero-based line and column, column counted in UTF-16 code units (LSP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Utf16Position {
    pub line: usize,
    pub col: usize,
}

/// Zero-based line and column, column counted in UTF-8 bytes (tree-sitter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Utf8Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl Utf16Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl Utf8Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl Document {
    /// Clamps a char offset to `0..=len_chars`.
    pub fn clamp_char(&self, char_idx: usize) -> usize {
        char_idx.min(self.rope().len_chars())
    }

    /// Char offset of the start of `line` (clamped to the last line).
    pub fn line_to_char(&self, line: usize) -> usize {
        let rope = self.rope();
        rope.line_to_char(line.min(last_line(rope)))
    }

    /// Line containing `char_idx` (clamped).
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        let rope = self.rope();
        rope.char_to_line(char_idx.min(rope.len_chars()))
    }

    /// Number of chars in `line`, excluding its `\n`.
    pub fn line_len_chars(&self, line: usize) -> usize {
        let rope = self.rope();
        let line = line.min(last_line(rope));
        line_end_char(rope, line) - rope.line_to_char(line)
    }

    /// Char offset of the end of `line`, before its `\n`.
    pub fn line_end_char(&self, line: usize) -> usize {
        let rope = self.rope();
        line_end_char(rope, line.min(last_line(rope)))
    }

    pub fn char_to_position(&self, char_idx: usize) -> Position {
        let rope = self.rope();
        let char_idx = char_idx.min(rope.len_chars());
        let line = rope.char_to_line(char_idx);
        Position::new(line, char_idx - rope.line_to_char(line))
    }

    pub fn position_to_char(&self, pos: Position) -> usize {
        let rope = self.rope();
        if pos.line > last_line(rope) {
            return rope.len_chars();
        }
        let start = rope.line_to_char(pos.line);
        let end = line_end_char(rope, pos.line);
        start.saturating_add(pos.col).min(end)
    }

    pub fn char_to_utf16(&self, char_idx: usize) -> Utf16Position {
        let rope = self.rope();
        let char_idx = char_idx.min(rope.len_chars());
        let line = rope.char_to_line(char_idx);
        let line_start = rope.line_to_char(line);
        let col = rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start);
        Utf16Position::new(line, col)
    }

    /// Converts an LSP position. A column inside a surrogate pair rounds
    /// down to the start of that char.
    pub fn utf16_to_char(&self, pos: Utf16Position) -> usize {
        let rope = self.rope();
        if pos.line > last_line(rope) {
            return rope.len_chars();
        }
        let start = rope.line_to_char(pos.line);
        let end = line_end_char(rope, pos.line);
        let start_cu = rope.char_to_utf16_cu(start);
        let end_cu = rope.char_to_utf16_cu(end);
        let cu = start_cu.saturating_add(pos.col).min(end_cu);
        rope.utf16_cu_to_char(cu)
    }

    /// Total length in UTF-16 code units.
    pub fn len_utf16(&self) -> usize {
        self.rope().len_utf16_cu()
    }

    /// Document-wide UTF-16 offset of a char offset (clamped). This is the
    /// coordinate system of `NSRange` in `NSTextInputClient`.
    pub fn char_to_utf16_offset(&self, char_idx: usize) -> usize {
        let rope = self.rope();
        rope.char_to_utf16_cu(char_idx.min(rope.len_chars()))
    }

    /// Char offset of a document-wide UTF-16 offset (clamped). An offset
    /// inside a surrogate pair rounds down to the start of that char.
    pub fn utf16_offset_to_char(&self, offset: usize) -> usize {
        let rope = self.rope();
        rope.utf16_cu_to_char(offset.min(rope.len_utf16_cu()))
    }

    pub fn char_to_utf8(&self, char_idx: usize) -> Utf8Position {
        let rope = self.rope();
        let char_idx = char_idx.min(rope.len_chars());
        let line = rope.char_to_line(char_idx);
        let col = rope.char_to_byte(char_idx) - rope.line_to_byte(line);
        Utf8Position::new(line, col)
    }

    /// Converts a byte-column position. A column inside a multi-byte char
    /// rounds down to the start of that char.
    pub fn utf8_to_char(&self, pos: Utf8Position) -> usize {
        let rope = self.rope();
        if pos.line > last_line(rope) {
            return rope.len_chars();
        }
        let start = rope.line_to_byte(pos.line);
        let end = rope.char_to_byte(line_end_char(rope, pos.line));
        rope.byte_to_char(start.saturating_add(pos.col).min(end))
    }

    /// Char offset to absolute UTF-8 byte offset (tree-sitter `InputEdit`).
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        let rope = self.rope();
        rope.char_to_byte(char_idx.min(rope.len_chars()))
    }

    /// Absolute byte offset to char offset, rounding down mid-char.
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        let rope = self.rope();
        rope.byte_to_char(byte_idx.min(rope.len_bytes()))
    }
}

fn last_line(rope: &Rope) -> usize {
    // A rope always has at least one line, so this never underflows.
    rope.len_lines() - 1
}

/// End of `line` before its line break. `line` must be valid.
fn line_end_char(rope: &Rope, line: usize) -> usize {
    let next = if line < last_line(rope) {
        rope.line_to_char(line + 1)
    } else {
        return rope.len_chars();
    };
    // Every line except the last ends with exactly one `\n` (CR is
    // normalized away on load and insert).
    next - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    // "a😀b" — 😀 is 1 char, 2 UTF-16 units, 4 UTF-8 bytes.
    // "日本" — each char is 1 UTF-16 unit, 3 UTF-8 bytes.
    const TEXT: &str = "a😀b\n日本\n\nlast";

    fn doc() -> Document {
        Document::from_text(TEXT)
    }

    #[test]
    fn char_position_round_trip_for_every_offset() {
        let d = doc();
        for i in 0..=d.len_chars() {
            assert_eq!(d.position_to_char(d.char_to_position(i)), i, "char {i}");
            assert_eq!(d.utf16_to_char(d.char_to_utf16(i)), i, "utf16 {i}");
            assert_eq!(d.utf8_to_char(d.char_to_utf8(i)), i, "utf8 {i}");
        }
    }

    #[test]
    fn columns_in_each_unit() {
        let d = doc();
        // Offset of 'b' on line 0.
        assert_eq!(d.char_to_position(2), Position::new(0, 2));
        assert_eq!(d.char_to_utf16(2), Utf16Position::new(0, 3));
        assert_eq!(d.char_to_utf8(2), Utf8Position::new(0, 5));
        // End of "日本".
        assert_eq!(d.char_to_position(6), Position::new(1, 2));
        assert_eq!(d.char_to_utf16(6), Utf16Position::new(1, 2));
        assert_eq!(d.char_to_utf8(6), Utf8Position::new(1, 6));
        // Empty line and last line.
        assert_eq!(d.char_to_position(7), Position::new(2, 0));
        assert_eq!(d.char_to_position(d.len_chars()), Position::new(3, 4));
    }

    #[test]
    fn mid_char_columns_round_down() {
        let d = doc();
        // UTF-16 col 2 is between the surrogates of 😀 -> start of 😀.
        assert_eq!(d.utf16_to_char(Utf16Position::new(0, 2)), 1);
        // UTF-8 cols 2..=4 are inside 😀; col 1 is its start.
        for col in 1..=4 {
            assert_eq!(d.utf8_to_char(Utf8Position::new(0, col)), 1);
        }
        assert_eq!(d.byte_to_char(2), 1);
    }

    #[test]
    fn out_of_range_input_is_clamped() {
        let d = doc();
        // Past line end clamps before the '\n', not onto the next line.
        assert_eq!(d.position_to_char(Position::new(0, 99)), 3);
        assert_eq!(d.utf16_to_char(Utf16Position::new(0, 99)), 3);
        assert_eq!(d.utf8_to_char(Utf8Position::new(1, 99)), 6);
        // Past last line clamps to document end.
        let end = d.len_chars();
        assert_eq!(d.position_to_char(Position::new(99, 0)), end);
        assert_eq!(d.utf16_to_char(Utf16Position::new(99, 0)), end);
        assert_eq!(d.utf8_to_char(Utf8Position::new(99, 0)), end);
        assert_eq!(d.char_to_position(usize::MAX), Position::new(3, 4));
        assert_eq!(d.position_to_char(Position::new(0, usize::MAX)), 3);
        assert_eq!(d.char_to_byte(usize::MAX), d.len_bytes());
    }

    #[test]
    fn document_wide_utf16_offsets() {
        // a(1) 😀(2) b(1) \n(1) 日(1) ...
        let d = doc();
        assert_eq!(d.len_utf16(), "a😀b\n日本\n\nlast".encode_utf16().count());
        assert_eq!(d.char_to_utf16_offset(0), 0);
        assert_eq!(d.char_to_utf16_offset(2), 3); // before 'b'
        assert_eq!(d.char_to_utf16_offset(99), d.len_utf16());
        for i in 0..=d.len_chars() {
            assert_eq!(d.utf16_offset_to_char(d.char_to_utf16_offset(i)), i);
        }
        // Inside the surrogate pair of 😀 rounds down to its start.
        assert_eq!(d.utf16_offset_to_char(2), 1);
        assert_eq!(d.utf16_offset_to_char(usize::MAX), d.len_chars());
    }

    #[test]
    fn line_helpers() {
        let d = doc();
        assert_eq!(d.line_len_chars(0), 3);
        assert_eq!(d.line_len_chars(2), 0);
        assert_eq!(d.line_len_chars(3), 4);
        assert_eq!(d.line_to_char(1), 4);
        assert_eq!(d.line_end_char(1), 6);
        assert_eq!(d.char_to_line(5), 1);
        assert_eq!(d.line_to_char(99), d.line_to_char(3));
    }

    #[test]
    fn trailing_newline_creates_empty_last_line() {
        let d = Document::from_text("ab\n");
        assert_eq!(d.line_count(), 2);
        assert_eq!(d.char_to_position(3), Position::new(1, 0));
        assert_eq!(d.position_to_char(Position::new(0, 9)), 2);
    }

    #[test]
    fn crlf_file_positions_ignore_cr() {
        // LSP positions on a CRLF file must not count the CR.
        let d = Document::from_bytes(b"ab\r\ncd").unwrap();
        assert_eq!(d.utf16_to_char(Utf16Position::new(1, 1)), 4);
        assert_eq!(d.position_to_char(Position::new(0, 9)), 2);
    }

    #[test]
    fn only_lf_breaks_lines() {
        // U+2028 and U+0085 are not line breaks for LSP; neither for us.
        let d = Document::from_text("a\u{2028}b\u{85}c\x0Cd");
        assert_eq!(d.line_count(), 1);
    }

    #[test]
    fn empty_document() {
        let d = Document::from_text("");
        assert_eq!(d.char_to_position(0), Position::new(0, 0));
        assert_eq!(d.position_to_char(Position::new(5, 5)), 0);
        assert_eq!(d.utf16_to_char(Utf16Position::new(0, 3)), 0);
        assert_eq!(d.line_len_chars(0), 0);
    }
}
