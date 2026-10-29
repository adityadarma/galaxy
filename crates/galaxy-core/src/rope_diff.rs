//! Byte-level diff of two ropes by common prefix and suffix.
//!
//! Used where an edit path only knows "before" and "after" text: fold
//! range updates and incremental Tree-sitter reparses. Whole chunk
//! slices are compared with memcmp, so unchanged text costs no allocation,
//! and rope clones share nodes, so holding "before" is cheap.

use ropey::Rope;

/// The changed span: `start..old_end` in the old text was replaced by
/// `start..new_end` in the new text. Byte offsets, on UTF-8 boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteChange {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

/// `None` when the texts are equal.
pub fn changed_bytes(old: &Rope, new: &Rope) -> Option<ByteChange> {
    let old_len = old.len_bytes();
    let new_len = new.len_bytes();
    let mut prefix = common_prefix_bytes(old, new);
    if prefix == old_len && prefix == new_len {
        return None;
    }
    let max_suffix = old_len.min(new_len) - prefix;
    let mut suffix = common_suffix_bytes(old, new).min(max_suffix);
    // Both sides share these bytes, so a UTF-8 boundary on one side is a
    // boundary on the other.
    while prefix > 0 && !is_char_boundary(old, prefix) {
        prefix -= 1;
    }
    while suffix > 0 && !is_char_boundary(old, old_len - suffix) {
        suffix -= 1;
    }
    Some(ByteChange {
        start: prefix,
        old_end: old_len - suffix,
        new_end: new_len - suffix,
    })
}

fn is_char_boundary(rope: &Rope, byte_idx: usize) -> bool {
    byte_idx >= rope.len_bytes() || rope.byte(byte_idx) & 0xC0 != 0x80
}

fn common_prefix_bytes(a: &Rope, b: &Rope) -> usize {
    let mut a_chunks = a.chunks().map(str::as_bytes);
    let mut b_chunks = b.chunks().map(str::as_bytes);
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut total = 0;
    loop {
        if x.is_empty() {
            match a_chunks.next() {
                Some(chunk) => x = chunk,
                None => return total,
            }
            continue;
        }
        if y.is_empty() {
            match b_chunks.next() {
                Some(chunk) => y = chunk,
                None => return total,
            }
            continue;
        }
        let n = x.len().min(y.len());
        if x[..n] == y[..n] {
            total += n;
            x = &x[n..];
            y = &y[n..];
        } else {
            return total + x.iter().zip(y).take_while(|(p, q)| p == q).count();
        }
    }
}

fn common_suffix_bytes(a: &Rope, b: &Rope) -> usize {
    let mut a_chunks = a
        .chunks_at_byte(a.len_bytes())
        .0
        .reversed()
        .map(str::as_bytes);
    let mut b_chunks = b
        .chunks_at_byte(b.len_bytes())
        .0
        .reversed()
        .map(str::as_bytes);
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut total = 0;
    loop {
        if x.is_empty() {
            match a_chunks.next() {
                Some(chunk) => x = chunk,
                None => return total,
            }
            continue;
        }
        if y.is_empty() {
            match b_chunks.next() {
                Some(chunk) => y = chunk,
                None => return total,
            }
            continue;
        }
        let n = x.len().min(y.len());
        let (xs, ys) = (&x[x.len() - n..], &y[y.len() - n..]);
        if xs == ys {
            total += n;
            x = &x[..x.len() - n];
            y = &y[..y.len() - n];
        } else {
            return total
                + xs.iter()
                    .rev()
                    .zip(ys.iter().rev())
                    .take_while(|(p, q)| p == q)
                    .count();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, new: &str) -> String {
        let (o, n) = (Rope::from_str(old), Rope::from_str(new));
        match changed_bytes(&o, &n) {
            None => old.to_owned(),
            Some(c) => format!(
                "{}{}{}",
                &old[..c.start],
                &new[c.start..c.new_end],
                &old[c.old_end..]
            ),
        }
    }

    #[test]
    fn reconstructs_new_text() {
        for (old, new) in [
            ("", "abc"),
            ("abc", ""),
            ("abc", "abc"),
            ("aaaa", "aaaaa"),
            ("a😀b", "a😁b"),
            ("中文", "中x文"),
            ("x\ny\nz", "z"),
        ] {
            assert_eq!(apply(old, new), new, "{old:?} -> {new:?}");
        }
    }

    #[test]
    fn spans_chunk_boundaries() {
        let base = "0123456789".repeat(50_000);
        let old = Rope::from_str(&base);
        let mut new = old.clone();
        new.insert(250_000, "edit");
        let change = changed_bytes(&old, &new).unwrap();
        assert_eq!(
            (change.start, change.old_end, change.new_end),
            (250_000, 250_000, 250_004)
        );
    }
}
