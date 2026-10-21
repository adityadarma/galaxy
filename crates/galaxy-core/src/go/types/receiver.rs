//! Finds the expression before a `.` in the buffer, without a syntax tree:
//! the code around the cursor is usually incomplete while typing.

use std::ops::Range;

/// The text range of the expression before the `.` at or just before
/// `byte`. The identifier being typed after the dot is skipped. `None` when
/// there is no dot or nothing in front of it.
pub(super) fn receiver_range(src: &str, byte: usize) -> Option<Range<usize>> {
    let bytes = src.as_bytes();
    let mut i = byte.min(bytes.len());
    if !src.is_char_boundary(i) {
        return None;
    }
    // Skip the partial member name.
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    let dot = i - 1;
    let start = expression_start(bytes, dot)?;
    (start < dot).then_some(start..dot)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Walks back from `end` over one primary expression: identifiers, selector
/// dots and balanced `()` `[]` groups. Stops at whitespace or an operator.
fn expression_start(bytes: &[u8], end: usize) -> Option<usize> {
    let mut i = end;
    let mut start = end;
    while i > 0 {
        let b = bytes[i - 1];
        if is_ident_byte(b) || b == b'.' {
            i -= 1;
            start = i;
        } else if b == b')' || b == b']' || b == b'}' {
            let open = matching_open(bytes, i - 1)?;
            // `[]T`, `[3]T` and `map[K]V` in front of a type are not part of
            // the expression: an index or type argument list follows a name
            // or a closer, and `[]` is never an index.
            if b == b']' && is_type_prefix(bytes, open, i - 1) {
                break;
            }
            i = open;
            start = i;
        } else {
            break;
        }
    }
    // A leading `*`, `&` or `<-` belongs to the expression only inside parens
    // (handled above); keep the range to the primary expression.
    (start < end).then_some(start)
}

/// Whether the `[...]` from `open` to `close` is a slice, array or map type
/// prefix rather than an index.
fn is_type_prefix(bytes: &[u8], open: usize, close: usize) -> bool {
    if close == open + 1 {
        return true;
    }
    if bytes[..open].ends_with(b"map")
        && !bytes[..open - 3].last().is_some_and(|b| is_ident_byte(*b))
    {
        return true;
    }
    match open.checked_sub(1).map(|i| bytes[i]) {
        Some(b) => !(is_ident_byte(b) || matches!(b, b')' | b']')),
        None => true,
    }
}

/// Index of the opener matching the closer at `close`; strings and comments
/// are not tracked, so unbalanced text gives `None` rather than a guess.
fn matching_open(bytes: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close + 1;
    let mut steps = 0usize;
    while i > 0 {
        i -= 1;
        steps += 1;
        if steps > 4096 {
            return None;
        }
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str) -> Option<String> {
        let byte = src.find("/*|*/").unwrap();
        let clean = src.replace("/*|*/", "");
        receiver_range(&clean, byte).map(|r| clean[r].to_owned())
    }

    #[test]
    fn type_prefixes_are_not_part_of_the_receiver() {
        assert_eq!(at("var a []models./*|*/").as_deref(), Some("models"));
        assert_eq!(at("var a [3]models.Acc/*|*/").as_deref(), Some("models"));
        assert_eq!(
            at("var a map[string]models./*|*/").as_deref(),
            Some("models")
        );
        assert_eq!(
            at("var a map[string][]models./*|*/").as_deref(),
            Some("models")
        );
        assert_eq!(at("func f() []models./*|*/").as_deref(), Some("models"));
    }

    #[test]
    fn indexes_stay_part_of_the_receiver() {
        assert_eq!(at("x := items[0]./*|*/").as_deref(), Some("items[0]"));
        assert_eq!(at("x := m[k][1].Na/*|*/").as_deref(), Some("m[k][1]"));
        assert_eq!(at("x := f()[0]./*|*/").as_deref(), Some("f()[0]"));
    }

    #[test]
    fn simple_and_partial() {
        assert_eq!(at("x := req./*|*/").as_deref(), Some("req"));
        assert_eq!(at("x := req.Hea/*|*/").as_deref(), Some("req"));
    }

    #[test]
    fn chains_calls_and_indexes() {
        assert_eq!(at("a.b().c./*|*/").as_deref(), Some("a.b().c"));
        assert_eq!(at("x[i]./*|*/").as_deref(), Some("x[i]"));
        assert_eq!(at("f(a, g(b))./*|*/").as_deref(), Some("f(a, g(b))"));
    }

    #[test]
    fn parenthesized_and_prefixed() {
        assert_eq!(at("(*p)./*|*/").as_deref(), Some("(*p)"));
        assert_eq!(at("y := &x./*|*/").as_deref(), Some("x"));
    }

    #[test]
    fn no_dot_or_nothing_before() {
        assert_eq!(at("x := req/*|*/"), None);
        assert_eq!(at("x := ./*|*/"), None);
        assert_eq!(at("(a./*|*/").as_deref(), Some("a"));
    }

    #[test]
    fn unbalanced_is_none() {
        assert_eq!(at("a)./*|*/"), None);
    }
}
