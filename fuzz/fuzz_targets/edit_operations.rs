#![no_main]

use galaxy_core::document::Document;
use galaxy_core::edit;
use galaxy_core::history::{EditKind, Editor};
use galaxy_core::selection::{Selection, SelectionSet};
use libfuzzer_sys::fuzz_target;
use std::time::Instant;

fuzz_target!(|input: (String, Vec<(u8, u8, u8)>)| {
    let mut editor = Editor::new(Document::from_text(&input.0));
    for (op, a, b) in input.1.into_iter().take(256) {
        let len = editor.doc.len_chars();
        let start = usize::from(a) % (len + 1);
        let end = start + usize::from(b) % (len.saturating_sub(start) + 1);
        let selection = SelectionSet::new(vec![Selection::new(start, end)], 0, &editor.doc);
        editor.set_selections(selection);
        let tx = match op % 5 {
            0 => edit::insert_text(&editor.doc, &editor.sel, "x"),
            1 => edit::backspace(&editor.doc, &editor.sel, &edit::IndentConfig::default()),
            2 => edit::newline(&editor.doc, &editor.sel, &edit::IndentConfig::default()),
            3 => edit::indent(&editor.doc, &editor.sel, &edit::IndentConfig::default()),
            _ => edit::delete_forward(&editor.doc, &editor.sel),
        };
        editor.apply(tx, EditKind::Other, Instant::now());
        assert!(editor.sel.primary().head <= editor.doc.len_chars());
    }
});
