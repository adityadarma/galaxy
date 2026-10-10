//! Grapheme cluster boundaries over the rope (UAX #29).
//!
//! Backspace, delete, and cursor movement step by grapheme so that emoji
//! sequences (ZWJ families, flags, skin tones) and combining marks are
//! treated as one user-perceived character, like VSCode does.

use ropey::Rope;
use ropey::str_utils::byte_to_char_idx;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use crate::document::Document;

impl Document {
    /// Char offset of the grapheme boundary before `char_idx` (clamped).
    pub fn prev_grapheme_boundary(&self, char_idx: usize) -> usize {
        let rope = self.rope();
        prev_boundary(rope, char_idx.min(rope.len_chars()))
    }

    /// Char offset of the grapheme boundary after `char_idx` (clamped).
    pub fn next_grapheme_boundary(&self, char_idx: usize) -> usize {
        let rope = self.rope();
        next_boundary(rope, char_idx.min(rope.len_chars()))
    }
}

// Both walkers follow the chunked-iteration pattern recommended by ropey:
// feed the cursor one rope chunk at a time and supply context on demand,
// so no line or document is ever copied into a String.

fn prev_boundary(rope: &Rope, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let byte_idx = rope.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte, mut chunk_char, _) = rope.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, rope.len_bytes(), true);
    loop {
        match cursor.prev_boundary(chunk, chunk_byte) {
            Ok(None) => return 0,
            Ok(Some(n)) => return chunk_char + byte_to_char_idx(chunk, n - chunk_byte),
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, chunk_byte, chunk_char, _) = rope.chunk_at_byte(chunk_byte - 1);
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = rope.chunk_at_byte(n - 1).0;
                cursor.provide_context(ctx, n - ctx.len());
            }
            // Not produced by prev_boundary; fall back to one char.
            Err(_) => return char_idx - 1,
        }
    }
}

fn next_boundary(rope: &Rope, char_idx: usize) -> usize {
    let len = rope.len_chars();
    if char_idx >= len {
        return len;
    }
    let byte_idx = rope.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte, mut chunk_char, _) = rope.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, rope.len_bytes(), true);
    loop {
        match cursor.next_boundary(chunk, chunk_byte) {
            Ok(None) => return len,
            Ok(Some(n)) => return chunk_char + byte_to_char_idx(chunk, n - chunk_byte),
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte += chunk.len();
                (chunk, _, chunk_char, _) = rope.chunk_at_byte(chunk_byte);
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = rope.chunk_at_byte(n - 1).0;
                cursor.provide_context(ctx, n - ctx.len());
            }
            Err(_) => return char_idx + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILY: &str = "👨\u{200d}👩\u{200d}👧"; // 5 chars, 1 grapheme
    const FLAG: &str = "🇮🇩"; // 2 chars
    const E_ACUTE: &str = "e\u{301}"; // 2 chars

    #[test]
    fn steps_over_whole_clusters() {
        let text = format!("a{FAMILY}{FLAG}{E_ACUTE}\nb");
        let d = Document::from_text(&text);
        let mut forward = vec![0];
        while *forward.last().unwrap() < d.len_chars() {
            forward.push(d.next_grapheme_boundary(*forward.last().unwrap()));
        }
        // a | family | flag | é | \n | b
        assert_eq!(forward, vec![0, 1, 6, 8, 10, 11, 12]);

        let mut backward = vec![d.len_chars()];
        while *backward.last().unwrap() > 0 {
            backward.push(d.prev_grapheme_boundary(*backward.last().unwrap()));
        }
        backward.reverse();
        assert_eq!(backward, forward);
    }

    #[test]
    fn clusters_spanning_rope_chunks() {
        // Chunks are ~1 KB; a long run of 4-byte-per-char ZWJ sequences
        // guarantees clusters straddle chunk boundaries.
        let text = FAMILY.repeat(2_000);
        let d = Document::from_text(&text);
        let per = FAMILY.chars().count();
        let mut pos = 0;
        for i in 1..=2_000 {
            pos = d.next_grapheme_boundary(pos);
            assert_eq!(pos, i * per);
        }
        for i in (0..2_000).rev() {
            pos = d.prev_grapheme_boundary(pos);
            assert_eq!(pos, i * per);
        }
    }

    #[test]
    fn clamps_at_edges() {
        let d = Document::from_text("ab");
        assert_eq!(d.prev_grapheme_boundary(0), 0);
        assert_eq!(d.next_grapheme_boundary(2), 2);
        assert_eq!(d.next_grapheme_boundary(99), 2);
        assert_eq!(d.prev_grapheme_boundary(99), 1);
    }
}
