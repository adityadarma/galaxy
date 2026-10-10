//! Property tests for the editing core (M1-06).
//!
//! These exercise only the public API, the way the FFI layer does: random
//! documents, random cursor layouts, and random sequences of edit commands,
//! checking invariants that must hold for every input.

use std::time::{Duration, Instant};

use galaxy_core::change::{Assoc, Change, ChangeSet};
use galaxy_core::document::{Document, Encoding, LineEnding};
use galaxy_core::edit::{self, IndentConfig, Transaction};
use galaxy_core::history::{EditKind, Editor, HistoryConfig};
use galaxy_core::position::{Position, Utf8Position, Utf16Position};
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::viewport;
use proptest::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

/// Pieces chosen to hit the hard cases: multi-unit chars, clusters,
/// combining marks, tabs, brackets, blank lines, and every line ending.
const PIECES: &[&str] = &[
    "a",
    "b",
    "xyz",
    " ",
    "    ",
    "\t",
    "\n",
    "\n\n",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "😀",
    "日本",
    "e\u{301}",
    "👨\u{200d}👩\u{200d}👧",
    "🇮🇩",
    "é",
    "\r\n",
    "\r",
];

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..60).prop_map(|v| v.concat())
}

#[derive(Debug, Clone)]
enum Op {
    Insert(String),
    Paste(String),
    Backspace,
    DeleteForward,
    Newline,
    Indent,
    Outdent,
    /// Replace selections with (anchor, head) seeds, scaled to the document.
    Select(Vec<(u16, u16)>),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => prop::sample::select(PIECES).prop_map(|p| Op::Insert(p.to_string())),
        1 => text_strategy().prop_map(Op::Paste),
        3 => Just(Op::Backspace),
        2 => Just(Op::DeleteForward),
        2 => Just(Op::Newline),
        1 => Just(Op::Indent),
        1 => Just(Op::Outdent),
        2 => prop::collection::vec((any::<u16>(), any::<u16>()), 1..6).prop_map(Op::Select),
    ]
}

fn cfg_strategy() -> impl Strategy<Value = IndentConfig> {
    (any::<bool>(), 1usize..9).prop_map(|(use_tabs, tab_size)| IndentConfig { use_tabs, tab_size })
}

fn scale(seed: u16, len: usize) -> usize {
    seed as usize % (len + 1)
}

fn selection_from(seeds: &[(u16, u16)], doc: &Document) -> SelectionSet {
    let len = doc.len_chars();
    let ranges = seeds
        .iter()
        .map(|&(a, h)| Selection::new(scale(a, len), scale(h, len)))
        .collect();
    SelectionSet::new(ranges, 0, doc)
}

/// Builds the transaction for `op`, or `None` for a pure selection change.
fn transaction(op: &Op, ed: &Editor, cfg: &IndentConfig) -> Option<(Transaction, EditKind)> {
    let (doc, sel) = (&ed.doc, &ed.sel);
    Some(match op {
        Op::Insert(t) => (edit::insert_text(doc, sel, t), EditKind::Other),
        Op::Paste(t) => (edit::paste(doc, sel, t), EditKind::Other),
        Op::Backspace => (edit::backspace(doc, sel, cfg), EditKind::Other),
        Op::DeleteForward => (edit::delete_forward(doc, sel), EditKind::Other),
        Op::Newline => (edit::newline(doc, sel, cfg), EditKind::Other),
        Op::Indent => (edit::indent(doc, sel, cfg), EditKind::Other),
        Op::Outdent => (edit::outdent(doc, sel, cfg), EditKind::Other),
        Op::Select(_) => return None,
    })
}

/// Invariants every `SelectionSet` must satisfy against its document.
fn assert_selection_invariants(ed: &Editor) {
    let len = ed.doc.len_chars();
    let ranges = ed.sel.as_slice();
    assert!(!ranges.is_empty(), "selection set is empty");
    assert!(ed.sel.primary_index() < ranges.len());
    for s in ranges {
        assert!(
            s.anchor <= len && s.head <= len,
            "selection out of bounds: {s:?} > {len}"
        );
    }
    for pair in ranges.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(a.start() <= b.start(), "unsorted: {a:?} {b:?}");
        assert!(a.end() <= b.start(), "overlapping: {a:?} {b:?}");
        assert!(!a.should_merge(b), "mergeable neighbors: {a:?} {b:?}");
    }
}

fn assert_doc_invariants(doc: &Document) {
    let text = doc.text();
    assert!(!text.contains('\r'), "CR leaked into the rope: {text:?}");
    assert_eq!(doc.len_chars(), text.chars().count());
    assert_eq!(doc.len_bytes(), text.len());
    assert_eq!(doc.line_count(), text.matches('\n').count() + 1);
}

fn new_editor(text: &str, seeds: &[(u16, u16)]) -> Editor {
    let mut ed = Editor::new(Document::from_text(text));
    ed.sel = selection_from(seeds, &ed.doc);
    ed
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any op sequence keeps the document and selections well-formed, and
    /// undoing step by step walks back through exactly the same texts.
    #[test]
    fn edits_keep_invariants_and_undo_walks_back(
        text in text_strategy(),
        seeds in prop::collection::vec((any::<u16>(), any::<u16>()), 1..6),
        cfg in cfg_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..25),
    ) {
        let mut ed = new_editor(&text, &seeds);
        let initial = ed.doc.text();
        let mut now = Instant::now();
        // Text before each recorded undo step.
        let mut before_steps: Vec<String> = Vec::new();
        let mut last_revision = ed.doc.revision();

        for op in &ops {
            match transaction(op, &ed, &cfg) {
                Some((tx, kind)) => {
                    let steps = ed.history.undo_len();
                    let before = ed.doc.text();
                    now += Duration::from_millis(10);
                    ed.apply(tx, kind, now);
                    if ed.history.undo_len() > steps {
                        before_steps.push(before);
                    }
                }
                None => {
                    if let Op::Select(seeds) = op {
                        ed.set_selections(selection_from(seeds, &ed.doc));
                    }
                }
            }
            assert_doc_invariants(&ed.doc);
            assert_selection_invariants(&ed);
            prop_assert!(ed.doc.revision() >= last_revision);
            last_revision = ed.doc.revision();
        }

        let after = ed.doc.text();
        for expected in before_steps.iter().rev() {
            prop_assert!(ed.undo());
            prop_assert_eq!(&ed.doc.text(), expected);
            assert_doc_invariants(&ed.doc);
            assert_selection_invariants(&ed);
        }
        prop_assert!(!ed.undo());
        prop_assert_eq!(ed.doc.text(), initial);

        while ed.redo() {
            assert_doc_invariants(&ed.doc);
            assert_selection_invariants(&ed);
        }
        prop_assert_eq!(ed.doc.text(), after);
    }

    /// With grouping enabled, undoing everything still restores the start.
    #[test]
    fn grouped_typing_undoes_to_original(
        text in text_strategy(),
        seeds in prop::collection::vec((any::<u16>(), any::<u16>()), 1..4),
        cfg in cfg_strategy(),
        typed in prop::collection::vec(prop::sample::select(&["a", " ", "é", "😀", "\n"][..]), 1..30),
        deletes in 0usize..10,
    ) {
        let mut ed = new_editor(&text, &seeds);
        let initial = ed.doc.text();
        let mut now = Instant::now();
        for t in &typed {
            now += Duration::from_millis(50);
            let tx = edit::insert_text(&ed.doc, &ed.sel, t);
            ed.apply(tx, EditKind::Insert, now);
        }
        for _ in 0..deletes {
            now += Duration::from_millis(50);
            let tx = edit::backspace(&ed.doc, &ed.sel, &cfg);
            ed.apply(tx, EditKind::Delete, now);
        }
        assert_doc_invariants(&ed.doc);
        assert_selection_invariants(&ed);
        while ed.undo() {}
        prop_assert_eq!(ed.doc.text(), initial);
    }

    /// Single caret typing matches a plain `String` model.
    #[test]
    fn single_caret_insert_matches_string_model(
        text in text_strategy(),
        at in any::<u16>(),
        piece in prop::sample::select(PIECES),
    ) {
        let mut ed = new_editor(&text, &[]);
        let idx = scale(at, ed.doc.len_chars());
        ed.sel = SelectionSet::single(Selection::caret(idx));

        let mut model: Vec<char> = ed.doc.text().chars().collect();
        let normalized = piece.replace("\r\n", "\n").replace('\r', "\n");
        let inserted: Vec<char> = normalized.chars().collect();
        model.splice(idx..idx, inserted.iter().copied());

        let tx = edit::insert_text(&ed.doc, &ed.sel, piece);
        ed.apply(tx, EditKind::Insert, Instant::now());
        prop_assert_eq!(ed.doc.text(), model.iter().collect::<String>());
        prop_assert_eq!(ed.sel.primary(), Selection::caret(idx + inserted.len()));
    }

    /// Backspace removes a whole grapheme (never half of one) when the
    /// caret is not in leading whitespace.
    #[test]
    fn backspace_never_splits_a_grapheme(
        text in text_strategy().prop_filter("needs non-space text", |t| t.chars().any(|c| !c.is_whitespace())),
        at in any::<u16>(),
    ) {
        let doc = Document::from_text(&text);
        let idx = scale(at, doc.len_chars());
        let prev = doc.prev_grapheme_boundary(idx);
        let full = doc.text();
        // The removed slice must be a prefix-aligned grapheme boundary pair.
        let bounds: Vec<usize> = full
            .grapheme_indices(true)
            .map(|(b, _)| full[..b].chars().count())
            .chain(std::iter::once(doc.len_chars()))
            .collect();
        prop_assert!(bounds.contains(&prev), "{prev} not a boundary in {bounds:?}");
    }

    /// Grapheme stepping agrees with `unicode-segmentation` on the whole
    /// string, forwards and backwards.
    #[test]
    fn graphemes_match_reference(text in text_strategy()) {
        let doc = Document::from_text(&text);
        let full = doc.text();
        let mut expected: Vec<usize> = full
            .grapheme_indices(true)
            .map(|(b, _)| full[..b].chars().count())
            .collect();
        expected.push(doc.len_chars());
        expected.dedup();

        let mut forward = vec![0];
        while *forward.last().unwrap() < doc.len_chars() {
            let next = doc.next_grapheme_boundary(*forward.last().unwrap());
            prop_assert!(next > *forward.last().unwrap());
            forward.push(next);
        }
        prop_assert_eq!(&forward, &expected);

        let mut backward = vec![doc.len_chars()];
        while *backward.last().unwrap() > 0 {
            let prev = doc.prev_grapheme_boundary(*backward.last().unwrap());
            prop_assert!(prev < *backward.last().unwrap());
            backward.push(prev);
        }
        backward.reverse();
        prop_assert_eq!(backward, expected);
    }

    /// Char <-> (line, col) conversions round-trip in every unit, and
    /// out-of-range input clamps instead of panicking.
    #[test]
    fn positions_round_trip_and_clamp(text in text_strategy(), big in any::<u32>()) {
        let doc = Document::from_text(&text);
        for i in 0..=doc.len_chars() {
            prop_assert_eq!(doc.position_to_char(doc.char_to_position(i)), i);
            prop_assert_eq!(doc.utf16_to_char(doc.char_to_utf16(i)), i);
            prop_assert_eq!(doc.utf8_to_char(doc.char_to_utf8(i)), i);
            prop_assert_eq!(doc.byte_to_char(doc.char_to_byte(i)), i);
        }
        let big = big as usize;
        let end = doc.len_chars();
        prop_assert!(doc.position_to_char(Position::new(big, big)) <= end);
        prop_assert!(doc.utf16_to_char(Utf16Position::new(big, big)) <= end);
        prop_assert!(doc.utf8_to_char(Utf8Position::new(big, big)) <= end);
        // Clamped columns never cross into the next line.
        for line in 0..doc.line_count() {
            let p = doc.position_to_char(Position::new(line, big));
            prop_assert_eq!(p, doc.line_end_char(line));
        }
    }

    /// UTF-16 columns equal what `encode_utf16` reports for the line prefix.
    #[test]
    fn utf16_columns_match_reference(text in text_strategy()) {
        let doc = Document::from_text(&text);
        let chars: Vec<char> = doc.text().chars().collect();
        for i in 0..=chars.len() {
            let line_start = chars[..i].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1);
            let expected: usize = chars[line_start..i].iter().map(|c| c.len_utf16()).sum();
            let got = doc.char_to_utf16(i);
            prop_assert_eq!(got.col, expected);
        }
    }

    /// `apply` followed by the inverse restores the text, and `map` is
    /// monotonic for both associations.
    #[test]
    fn change_set_inverse_and_map(
        text in text_strategy(),
        raw in prop::collection::vec((any::<u16>(), any::<u16>(), prop::sample::select(PIECES)), 0..8),
    ) {
        let mut doc = Document::from_text(&text);
        let original = doc.text();
        let len = doc.len_chars();
        let changes: Vec<Change> = raw
            .iter()
            .map(|&(a, b, t)| Change::new(scale(a, len), scale(b, len), t))
            .collect();
        let set = ChangeSet::new(changes, len);

        let inverse = doc.apply(&set);
        assert_doc_invariants(&doc);
        prop_assert_eq!(doc.len_chars(), set.len_after(len));
        doc.apply(&inverse);
        prop_assert_eq!(doc.text(), original);

        for assoc in [Assoc::Before, Assoc::After] {
            let mut prev = 0;
            for pos in 0..=len {
                let mapped = set.map(pos, assoc);
                prop_assert!(mapped >= prev, "map not monotonic at {pos}");
                prop_assert!(mapped <= set.len_after(len));
                prev = mapped;
            }
        }
    }

    /// Load/save keeps text, encoding, BOM, and line ending stable.
    #[test]
    fn bytes_round_trip(
        text in text_strategy(),
        encoding in prop::sample::select(&[Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be][..]),
        line_ending in prop::sample::select(&[LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr][..]),
        bom in any::<bool>(),
    ) {
        let mut doc = Document::from_text(&text);
        doc.set_encoding(encoding);
        doc.set_line_ending(line_ending);
        // UTF-16 always carries a BOM (set_encoding), so only UTF-8 varies.
        if encoding == Encoding::Utf8 {
            doc.set_has_bom(bom);
        }
        let bytes = doc.to_bytes();
        match Document::from_bytes(&bytes) {
            Ok(loaded) => {
                prop_assert_eq!(loaded.text(), doc.text());
                prop_assert_eq!(loaded.encoding(), doc.encoding());
                prop_assert_eq!(loaded.has_bom(), doc.has_bom());
                prop_assert_eq!(loaded.to_bytes(), bytes);
            }
            Err(e) => {
                // Only genuinely undetectable input may be rejected.
                prop_assert!(false, "rejected its own output: {e} ({encoding:?}, {bytes:?})");
            }
        }
    }

    /// A full-document snapshot reproduces the text, and every reported
    /// column lies within its line.
    #[test]
    fn viewport_matches_document(
        text in text_strategy(),
        seeds in prop::collection::vec((any::<u16>(), any::<u16>()), 1..6),
        first in 0usize..10,
        count in 0usize..20,
    ) {
        let ed = new_editor(&text, &seeds);
        let all = viewport::snapshot(&ed.doc, &ed.sel, 0, usize::MAX);
        let joined: Vec<&str> = all.lines.iter().map(|l| l.text.as_str()).collect();
        prop_assert_eq!(joined.join("\n"), ed.doc.text());
        prop_assert_eq!(all.line_count, ed.doc.line_count());

        let mut carets = 0;
        for l in &all.lines {
            let len16 = l.text.encode_utf16().count();
            for &c in &l.carets {
                prop_assert!(c <= len16);
                carets += 1;
            }
            for sp in &l.selections {
                prop_assert!(sp.start <= sp.end && sp.end <= len16);
            }
        }
        prop_assert_eq!(carets, ed.sel.len());

        // A partial snapshot is a slice of the full one.
        let part = viewport::snapshot(&ed.doc, &ed.sel, first, count);
        for l in &part.lines {
            prop_assert_eq!(l, &all.lines[l.line]);
        }
    }

    /// Several cursors edit identically to applying each one in turn from
    /// the bottom up (the classic multi-cursor equivalence).
    #[test]
    fn multi_cursor_insert_matches_sequential(
        text in text_strategy(),
        seeds in prop::collection::vec(any::<u16>(), 1..8),
        piece in prop::sample::select(&["x", "日", "😀", "ab"][..]),
    ) {
        let doc = Document::from_text(&text);
        let len = doc.len_chars();
        let carets: Vec<Selection> = seeds.iter().map(|&s| Selection::caret(scale(s, len))).collect();
        let set = SelectionSet::new(carets, 0, &doc);

        let mut model: Vec<char> = doc.text().chars().collect();
        for s in set.as_slice().iter().rev() {
            model.splice(s.head..s.head, piece.chars());
        }

        let mut ed = Editor::with_history(doc, HistoryConfig::default());
        ed.sel = set;
        let tx = edit::insert_text(&ed.doc, &ed.sel, piece);
        ed.apply(tx, EditKind::Insert, Instant::now());
        prop_assert_eq!(ed.doc.text(), model.iter().collect::<String>());
        assert_selection_invariants(&ed);
    }
}
