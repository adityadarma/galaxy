//! Document, editor, navigation, find, and clipboard integration regressions.

use super::*;

fn pos(line: u64, col: u64) -> TextPos {
    TextPos {
        line,
        col_utf16: col,
    }
}

fn caret(line: u64, col: u64) -> TextSelection {
    TextSelection {
        anchor: pos(line, col),
        head: pos(line, col),
    }
}

fn doc_with(text: &str) -> Arc<DocumentHandle> {
    Arc::new(DocumentHandle::new(
        Document::from_text(text),
        PathBuf::new(),
    ))
}

fn doc_at(path: &str, text: &str, caret_utf16: u64) -> Arc<DocumentHandle> {
    let doc = Arc::new(DocumentHandle::new(
        Document::from_text(text),
        PathBuf::from(path),
    ));
    doc.set_selections(vec![caret(0, caret_utf16)], 0);
    doc
}

fn type_text(doc: &DocumentHandle, text: &str) {
    doc.edit(EditCommand::InsertText { text: text.into() });
}

fn primary(doc: &DocumentHandle) -> (u64, u64) {
    let r = doc.selected_range();
    (r.location, r.length)
}

fn cfg(case_sensitive: bool, whole_word: bool, regex: bool) -> FindConfig {
    FindConfig {
        case_sensitive,
        whole_word,
        regex,
    }
}

fn file_doc(text: &str) -> (tempfile::TempDir, PathBuf, Arc<DocumentHandle>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, text).unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    (dir, path, doc)
}

fn write_later(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(later)
        .unwrap();
}

fn all_text(doc: &DocumentHandle) -> String {
    doc.lines(0, doc.line_count()).join("\n")
}

#[test]
fn folding_hides_lines_and_shrinks_the_row_map() {
    let doc = folding_doc();
    assert_eq!(doc.total_rows(), 7);
    assert_eq!(doc.fold_state(0), FoldState::Expanded);
    assert_eq!(doc.fold_state(1), FoldState::Plain);
    assert!(doc.toggle_fold(0));
    assert_eq!(doc.fold_state(0), FoldState::Folded);
    assert_eq!(doc.total_rows(), 5);
    assert_eq!(
        doc.row_of_line(3),
        1,
        "the closing brace follows the header"
    );
    assert_eq!(doc.line_at_row(1).line, 3);
    let snap = doc.viewport(0, 7);
    assert_eq!(
        snap.lines.iter().map(|l| l.line).collect::<Vec<_>>(),
        [0, 3, 4, 5, 6]
    );
    assert!(snap.lines[0].folded && snap.lines[0].foldable);
    assert!(!snap.lines[2].folded && snap.lines[2].foldable);
    doc.toggle_fold(4);
    doc.unfold_all();
    assert_eq!(doc.total_rows(), 7);
    assert!(!doc.has_folds());
    assert!(doc.toggle_fold(0));
    assert!(doc.toggle_fold(0));
    assert_eq!(doc.total_rows(), 7);
    assert!(!doc.toggle_fold(1), "nothing to fold on a leaf line");
}

#[test]
fn syntax_can_be_suspended_and_resumed_without_changing_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("idle.rs");
    std::fs::write(&path, "fn idle() {}\n").unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(doc.syntax_active());
    assert!(doc.suspend_syntax());
    assert!(!doc.syntax_active());
    assert_eq!(doc.lines(0, 1), ["fn idle() {}"]);
    assert!(doc.resume_syntax());
    assert!(doc.syntax_active());
}

#[test]
fn edit_viewport_undo_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.txt");
    std::fs::write(&path, "a😀\nb\n").unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    doc.set_selections(vec![caret(0, 3), caret(1, 1)], 0);
    doc.edit(EditCommand::InsertText { text: "!".into() });
    let snap = doc.viewport(0, 10);
    assert_eq!(snap.lines[0].text, "a😀!");
    assert_eq!(snap.lines[0].carets, vec![4]);
    assert_eq!(snap.lines[1].carets, vec![2]);
    assert_eq!(snap.selection_count, 2);
    assert_eq!(snap.primary.col, 3);
    assert_eq!(snap.primary.col_utf16, 4);
    assert!(doc.undo());
    assert_eq!(doc.lines(0, 2), vec!["a😀", "b"]);
    assert!(doc.redo());
    assert_eq!(doc.lines(0, 2), vec!["a😀!", "b!"]);
}

#[test]
fn selection_is_reported_in_utf16() {
    let doc = doc_with("😀ab\ncd");
    doc.set_selections(
        vec![TextSelection {
            anchor: pos(0, 2),
            head: pos(1, 1),
        }],
        0,
    );
    let snap = doc.viewport(0, 2);
    assert_eq!(
        snap.lines[0].selections,
        vec![LineSpan {
            start: 2,
            end: 4,
            includes_eol: true
        }]
    );
    assert_eq!(
        snap.lines[1].selections,
        vec![LineSpan {
            start: 0,
            end: 1,
            includes_eol: false
        }]
    );
    doc.set_selections(vec![caret(0, 1)], 0);
    assert_eq!(doc.viewport(0, 1).primary.col_utf16, 0);
}

#[test]
fn ime_composition_round_trip_over_ffi() {
    let doc = doc_with("😀");
    doc.set_selections(vec![caret(0, 2)], 0);
    let range = |location, length| Utf16Range { location, length };
    for preedit in ["k", "ka", "か"] {
        let n = preedit.encode_utf16().count() as u64;
        doc.set_marked_text(preedit.into(), range(n, 0), None);
        assert!(doc.has_marked_text());
    }
    assert_eq!(doc.marked_range(), Some(range(2, 1)));
    assert_eq!(doc.selected_range(), range(3, 0));
    assert_eq!(doc.text_in_range(range(0, 99)), "😀か");
    assert_eq!(doc.utf16_offset(pos(0, 3)), 3);
    assert_eq!(doc.position_at(3), pos(0, 3));
    assert_eq!(doc.position_at(1), pos(0, 0));
    assert_eq!(
        (
            doc.viewport(0, 1).lines[0].marked.unwrap().start,
            doc.viewport(0, 1).lines[0].marked.unwrap().end
        ),
        (2, 3)
    );
    doc.commit_text("漢".into(), None);
    assert!(!doc.has_marked_text());
    assert_eq!(doc.lines(0, 1), vec!["😀漢"]);
    assert!(doc.viewport(0, 1).lines[0].marked.is_none());
    assert!(doc.undo());
    assert_eq!(doc.lines(0, 1), vec!["😀"]);
}

#[test]
fn typing_gt_and_slash_close_tags_in_markup_files() {
    let html = doc_at("page.html", "<div", 4);
    type_text(&html, ">");
    assert_eq!(html.text(), "<div></div>");
    assert_eq!(primary(&html), (5, 0));
    let slash = doc_at("page.html", "<ul><li>a<", 10);
    type_text(&slash, "/");
    assert_eq!(slash.text(), "<ul><li>a</li>");
    let tsx = doc_at("view.tsx", "const a = <Foo bar={() => 1}", 28);
    type_text(&tsx, ">");
    assert_eq!(tsx.text(), "const a = <Foo bar={() => 1}></Foo>");
    let generic = doc_at("view.tsx", "let x: Array<string", 19);
    type_text(&generic, ">");
    assert_eq!(
        generic.text(),
        "let x: Array<string>",
        "generic stays plain"
    );
    let ts = doc_at("lib.ts", "<div", 4);
    type_text(&ts, ">");
    assert_eq!(ts.text(), "<div>", "TypeScript has no tags");
}

#[test]
fn navigation_selection_and_mouse() {
    let doc = doc_with("hello world\nfoo");
    doc.set_caret(pos(0, 6));
    doc.move_cursors(CursorMotion::Right, true);
    assert_eq!(primary(&doc), (6, 1));
    doc.move_cursors(CursorMotion::Left, false);
    assert_eq!(primary(&doc), (6, 0));
    doc.move_cursors(CursorMotion::Down, false);
    assert_eq!(
        primary(&doc),
        (15, 0),
        "column 6 clamps to the end of 'foo'"
    );
    doc.move_cursors(CursorMotion::DocStart, false);
    doc.move_cursors(CursorMotion::WordRight, true);
    assert_eq!(primary(&doc), (0, 5));
    doc.select_word_at(pos(0, 8));
    assert_eq!(primary(&doc), (6, 5));
    doc.select_line_at(pos(0, 3));
    assert_eq!(primary(&doc), (0, 12));
    doc.select_all();
    assert_eq!(primary(&doc), (0, 15));
    doc.set_caret(pos(0, 1));
    doc.add_caret(pos(1, 1));
    assert_eq!(doc.viewport(0, 2).selection_count, 2);
    doc.extend_selection_to(pos(1, 3));
    doc.set_caret(pos(0, 0));
    assert_eq!(
        doc.viewport(0, 2).selection_count,
        1,
        "a click resets to one caret"
    );
    doc.extend_selection_to(pos(0, 4));
    assert_eq!(primary(&doc), (0, 4));
}

#[test]
fn find_walks_matches_and_reports_status() {
    let doc = doc_with("cat Cat concat\ncat");
    let st = doc.set_find("cat".into(), cfg(false, true, false));
    assert_eq!((st.total, st.current, st.error.clone()), (3, None, None));
    assert!(doc.find_nearest());
    assert_eq!(primary(&doc), (0, 3));
    assert!(doc.find_next());
    assert_eq!(primary(&doc), (4, 3));
    assert!(doc.find_next());
    assert_eq!(primary(&doc), (15, 3));
    assert!(doc.find_next());
    assert_eq!(primary(&doc), (0, 3), "wraps around");
    assert!(doc.find_previous());
    assert_eq!(primary(&doc), (15, 3), "and backwards");
    assert_eq!(doc.find_status().current, Some(3));
    assert_eq!(doc.set_find("cat".into(), cfg(true, false, false)).total, 3);
    assert_eq!(doc.set_find("Cat".into(), cfg(true, false, false)).total, 1);
}

#[test]
fn replace_one_then_all_are_single_undo_steps() {
    let doc = doc_with("a1 a2 a3");
    doc.set_find(r"a(\d)".into(), cfg(true, false, true));
    assert!(!doc.replace_current("<$1>".into()));
    assert!(doc.replace_current("<$1>".into()));
    assert_eq!(doc.replace_all("[$1]".into()), 2);
    assert_eq!(doc.lines(0, 1), vec!["<1> [2] [3]"]);
    assert!(doc.undo());
    assert_eq!(doc.lines(0, 1), vec!["<1> a2 a3"]);
}

#[test]
fn clipboard_round_trip_including_linewise() {
    let doc = doc_with("one\ntwo\nthree");
    doc.set_caret(pos(1, 1));
    let copied = doc.copy();
    assert_eq!(
        copied,
        ClipboardText {
            text: "two\n".into(),
            linewise: true
        }
    );
    assert_eq!(
        doc.lines(0, 5).join("|"),
        "one|two|three",
        "copy does not edit"
    );
    assert!(!doc.is_modified());
    doc.paste_clipboard(copied.text.clone(), copied.linewise);
    assert_eq!(doc.lines(0, 5).join("|"), "one|two|two|three");
    let cut = doc.cut();
    assert!(cut.linewise);
    assert_eq!(doc.lines(0, 5).join("|"), "one|two|three");
    assert!(doc.undo());
    assert_eq!(doc.lines(0, 5).join("|"), "one|two|two|three");
    doc.select_word_at(pos(0, 1));
    let word = doc.cut();
    assert_eq!(
        word,
        ClipboardText {
            text: "one".into(),
            linewise: false
        }
    );
    doc.paste_clipboard("1".into(), false);
    assert_eq!(doc.lines(0, 1), vec!["1"]);
}

#[test]
fn external_changes_reload_as_one_undo_step() {
    let (_dir, path, doc) = file_doc("one\ntwo\n");
    assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
    write_later(&path, "one\nTWO\n");
    assert_eq!(doc.check_disk(), DiskStatus::Changed);
    assert!(doc.reload_from_disk().unwrap());
    assert_eq!(all_text(&doc), "one\nTWO\n");
    assert!(!doc.is_modified(), "reloaded text counts as saved");
    assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
    assert!(doc.undo());
    assert_eq!(all_text(&doc), "one\ntwo\n");
    assert!(doc.is_modified(), "undoing the reload differs from disk");
}

#[test]
fn is_modified_tracks_edits_and_saves() {
    let (_dir, _path, doc) = file_doc("hi");
    assert!(!doc.is_modified());
    doc.move_cursors(CursorMotion::DocEnd, false);
    assert!(!doc.is_modified(), "moving the cursor is not an edit");
    doc.edit(EditCommand::InsertText { text: "!".into() });
    assert!(doc.is_modified());
    doc.save().unwrap();
    assert!(!doc.is_modified());
    assert_eq!(std::fs::read_to_string(doc.path()).unwrap(), "hi!");
    assert!(doc.undo());
    assert!(doc.is_modified());
    assert!(doc.redo());
    assert!(!doc.is_modified());
}

#[test]
fn our_own_save_is_not_an_external_change() {
    let (_dir, _path, doc) = file_doc("a");
    doc.edit(EditCommand::InsertText { text: "b".into() });
    doc.save().unwrap();
    assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
}

#[test]
fn keeping_the_buffer_stops_reporting_and_stays_unsaved() {
    let (_dir, path, doc) = file_doc("a");
    doc.edit(EditCommand::InsertText {
        text: "mine ".into(),
    });
    write_later(&path, "theirs");
    assert_eq!(doc.check_disk(), DiskStatus::Changed);
    doc.keep_buffer_over_disk();
    assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
    assert!(doc.undo());
    assert!(
        doc.is_modified(),
        "even undone to the old text, the file differs"
    );
    doc.save().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a");
    assert!(!doc.is_modified());
}

#[test]
fn a_deleted_file_keeps_its_text_and_counts_as_unsaved() {
    let (_dir, path, doc) = file_doc("keep me");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(doc.check_disk(), DiskStatus::Deleted);
    assert!(doc.is_modified());
    assert_eq!(all_text(&doc), "keep me");
    std::fs::write(&path, "keep me").unwrap();
    assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
    assert!(!doc.is_modified());
    std::fs::remove_file(&path).unwrap();
    assert_eq!(doc.check_disk(), DiskStatus::Deleted);
    doc.save().unwrap();
    assert!(!doc.is_modified());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
}

#[test]
fn editing_a_tag_name_renames_its_partner_in_one_undo_step() {
    let doc = doc_at("page.html", "<div>x</div>", 4);
    type_text(&doc, "s");
    assert_eq!(doc.text(), "<divs>x</divs>");
    assert_eq!(primary(&doc), (5, 0));
    doc.edit(EditCommand::Backspace);
    assert_eq!(doc.text(), "<div>x</div>");
    assert!(doc.undo());
    assert_eq!(
        doc.text(),
        "<divs>x</divs>",
        "undo reverts both names together"
    );
    assert!(doc.undo());
    assert_eq!(doc.text(), "<div>x</div>");
    let off = doc_at("page.html", "<div>x</div>", 4);
    off.set_tag_editing(true, false);
    type_text(&off, "s");
    assert_eq!(off.text(), "<divs>x</div>");
    let text = doc_at("notes.md", "<div>x</div>", 4);
    type_text(&text, "s");
    assert_eq!(text.text(), "<divs>x</div>", "only markup languages rename");
}

#[test]
fn closing_a_tag_is_one_undo_step_and_can_be_switched_off() {
    let doc = doc_at("page.html", "<div", 4);
    type_text(&doc, ">");
    assert!(doc.undo());
    assert_eq!(doc.text(), "<div");
    let off = doc_at("page.html", "<div", 4);
    off.set_tag_editing(false, true);
    type_text(&off, ">");
    assert_eq!(off.text(), "<div>");
}

#[test]
fn long_lines_are_cut_and_matches_come_from_the_shown_part() {
    let cut = viewport::MAX_LINE_DISPLAY_CHARS;
    let long = format!("{}ab{}", "x".repeat(10), "y".repeat(cut + 100));
    let doc = doc_with(&format!("{long}ab\nab"));
    doc.set_find("ab".into(), cfg(true, false, false));
    let snap = doc.viewport(0, 2);
    assert!(snap.lines[0].truncated);
    assert_eq!(snap.lines[0].text.chars().count(), cut);
    let shown: Vec<_> = snap.lines[0].matches.iter().map(|m| m.start).collect();
    assert_eq!(shown, vec![10], "the match past the cut is not drawn");
    assert!(!snap.lines[1].truncated);
    assert_eq!(doc.display_line(0), snap.lines[0].text);
    assert_eq!(doc.display_line(99), "ab", "clamped to the last line");
    assert_eq!(doc.find_status().total, 3);
}

#[test]
fn rename_edits_use_utf16_ranges_and_one_undo_step() {
    let doc = doc_with("😀 old old");
    assert!(doc.apply_rename_edits(vec![
        RenameEdit {
            uri: String::new(),
            start_line: 0,
            start_character: 3,
            end_line: 0,
            end_character: 6,
            new_text: "new".into()
        },
        RenameEdit {
            uri: String::new(),
            start_line: 0,
            start_character: 7,
            end_line: 0,
            end_character: 10,
            new_text: "new".into()
        }
    ]));
    assert_eq!(all_text(&doc), "😀 new new");
    assert!(doc.undo());
    assert_eq!(all_text(&doc), "😀 old old");
}

#[test]
fn multi_cursor_commands_edit_every_cursor() {
    let doc = doc_with("foo bar\nfoo baz\nqux");
    doc.set_caret(pos(0, 1));
    doc.select_next_occurrence();
    assert_eq!(primary(&doc), (0, 3), "first press selects the word");
    doc.select_next_occurrence();
    assert_eq!(doc.viewport(0, 3).selection_count, 2);
    doc.edit(EditCommand::InsertText { text: "X".into() });
    assert_eq!(doc.lines(0, 3), vec!["X bar", "X baz", "qux"]);
    assert!(doc.undo());
    assert_eq!(
        doc.lines(0, 3),
        vec!["foo bar", "foo baz", "qux"],
        "one undo step for both"
    );
    doc.select_all_occurrences();
    assert_eq!(doc.viewport(0, 3).selection_count, 2);
    doc.set_caret(pos(0, 2));
    doc.add_cursor_vertical(false);
    doc.add_cursor_vertical(false);
    assert_eq!(doc.viewport(0, 3).selection_count, 3);
    doc.add_cursor_vertical(false);
    assert_eq!(
        doc.viewport(0, 3).selection_count,
        3,
        "no line below the last"
    );
    doc.add_cursor_vertical(true);
    assert_eq!(doc.viewport(0, 3).selection_count, 3);
    doc.column_select(pos(0, 4), pos(2, 6));
    assert_eq!(doc.viewport(0, 3).selection_count, 3);
    doc.edit(EditCommand::InsertText { text: "|".into() });
    assert_eq!(
        doc.lines(0, 3),
        vec!["foo |r", "foo |z", "qux|"],
        "the box is columns 4..6; the short line only gets a caret at its end"
    );
}

#[test]
fn column_select_keeps_columns_past_short_lines_and_counts_utf16() {
    let doc = doc_with("abcdef\nabcdef\nab");
    doc.column_select(pos(2, 5), pos(0, 1));
    assert_eq!(doc.viewport(0, 3).selection_count, 3);
    doc.edit(EditCommand::InsertText { text: "|".into() });
    assert_eq!(doc.lines(0, 3), vec!["a|f", "a|f", "a|"]);
    let doc = doc_with("a\u{1F600}bc\nabcd");
    doc.column_select(pos(0, 3), pos(1, 3));
    doc.edit(EditCommand::InsertText { text: "|".into() });
    assert_eq!(doc.lines(0, 2), vec!["a\u{1F600}|c", "ab|d"]);
}

#[test]
fn select_all_matches_allows_multi_cursor_editing() {
    let doc = doc_with("ab x ab\nab");
    doc.set_find("ab".into(), cfg(true, false, false));
    assert!(doc.select_all_matches());
    assert_eq!(doc.viewport(0, 2).selection_count, 3);
    doc.edit(EditCommand::InsertText { text: "Z".into() });
    assert_eq!(doc.lines(0, 2), vec!["Z x Z", "Z"]);
}

#[test]
fn indent_settings_round_trip() {
    let doc = doc_with("x");
    assert_eq!(
        doc.indent_settings(),
        IndentSettings {
            use_tabs: false,
            tab_size: 4
        }
    );
    doc.set_indent(true, 8);
    assert_eq!(
        doc.indent_settings(),
        IndentSettings {
            use_tabs: true,
            tab_size: 8
        }
    );
}

#[test]
fn invalid_regex_reports_an_error_and_keeps_no_search() {
    let doc = doc_with("a(b");
    let st = doc.set_find("(".into(), cfg(true, false, true));
    assert!(st.error.as_deref().is_some_and(|e| e.contains("unclosed")));
    assert_eq!(st.total, 0);
    assert!(!doc.find_next());
    assert_eq!(doc.set_find("(".into(), cfg(true, false, false)).total, 1);
    let cleared = doc.set_find(String::new(), cfg(true, false, false));
    assert_eq!((cleared.total, cleared.error), (0, None));
}

#[test]
fn find_matches_are_part_of_the_viewport_in_utf16_columns() {
    let doc = doc_with("\u{1F600}ab ab\nxx");
    doc.set_find("ab".into(), cfg(true, false, false));
    let snap = doc.viewport(0, 2);
    let cols: Vec<_> = snap.lines[0]
        .matches
        .iter()
        .map(|m| (m.start, m.end))
        .collect();
    assert_eq!(cols, vec![(2, 4), (5, 7)]);
    assert!(snap.lines[1].matches.is_empty());
    doc.clear_find();
    assert!(doc.viewport(0, 2).lines[0].matches.is_empty());
    assert!(!doc.find_next(), "no search, nothing to find");
}

#[test]
fn a_count_job_is_a_snapshot_and_can_be_cancelled() {
    let doc = doc_with("ab ab");
    doc.set_find("ab".into(), cfg(true, false, false));
    let job = doc.start_find_count().unwrap();
    doc.edit(EditCommand::InsertText { text: "ab ".into() });
    assert_ne!(
        job.revision(),
        doc.revision(),
        "the UI can tell the result is stale"
    );
    assert_eq!(
        job.run().unwrap().total,
        2,
        "counts the text it was created from"
    );
    assert_eq!(doc.find_status().total, 3);
    let job = doc.start_find_count().unwrap();
    job.cancel();
    assert!(job.run().is_none());
}

#[test]
fn big_documents_count_in_a_background_job() {
    let filler = "x".repeat(40);
    let lines = SYNC_FIND_COUNT_BYTES as usize / 40 + 100;
    let text: String = (0..lines)
        .map(|i| {
            if i % 100 == 0 {
                format!("ab{filler}\n")
            } else {
                format!("{filler}\n")
            }
        })
        .collect();
    let expected = lines.div_ceil(100) as u64;
    let doc = doc_with(&text);
    let st = doc.set_find("ab".into(), cfg(true, false, false));
    assert!(!st.counted, "too big to count on the caller's thread");
    assert!(doc.find_nearest());
    assert_eq!(primary(&doc), (0, 2), "finding still works right away");
    let job = doc.start_find_count().unwrap();
    assert_eq!(job.revision(), doc.revision());
    let counted = std::thread::spawn(move || job.run())
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            counted.total,
            counted.current,
            counted.counted,
            counted.truncated
        ),
        (expected, Some(1), true, false)
    );
    assert!(
        doc_with("ab")
            .set_find("ab".into(), cfg(true, false, false))
            .counted
    );
    doc.clear_find();
    assert!(doc.start_find_count().is_none());
}

#[test]
fn undoing_back_to_the_saved_text_is_not_modified() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "hi").unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    doc.move_cursors(CursorMotion::DocEnd, false);
    doc.edit(EditCommand::InsertText { text: "!".into() });
    doc.edit(EditCommand::Backspace);
    assert!(doc.is_modified(), "same text by editing is still an edit");
    while doc.undo() {}
    assert!(!doc.is_modified(), "undo back to the loaded text is clean");
    doc.redo();
    assert!(doc.is_modified());
    doc.undo();
    assert!(!doc.is_modified());
    doc.set_marked_text(
        "k".into(),
        Utf16Range {
            location: 1,
            length: 0,
        },
        None,
    );
    assert!(doc.is_modified());
    doc.unmark_text();
    assert!(doc.is_modified(), "committed text is an edit");
    doc.undo();
    assert!(!doc.is_modified());
}

#[test]
fn syntax_is_detected_and_arrives_without_blocking_viewport() {
    let dir = tempfile::tempdir().unwrap();
    let rust = dir.path().join("main.rs");
    std::fs::write(&rust, "// note\nfn main() { let name = \"Galaxy\"; }\n").unwrap();
    let doc = Core::new()
        .open_document(rust.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(doc.syntax_language(), Some("Rust".into()));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snap = doc.viewport(0, 2);
        if !snap.styles.is_empty() {
            assert!(snap.styles.iter().any(|run| run.scope == "comment"));
            assert!(snap.styles.iter().any(|run| run.scope == "keyword"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "syntax worker did not return styles"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    doc.edit(EditCommand::InsertText { text: "x".into() });
    assert!(!doc.viewport(0, 2).styles.is_empty());
    assert!(doc.undo());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !doc.poll_syntax() {
        assert!(Instant::now() < deadline, "undo was not reparsed");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        doc.viewport(0, 2)
            .styles
            .iter()
            .any(|run| run.scope == "keyword")
    );
    let plain = dir.path().join("notes.txt");
    std::fs::write(&plain, "plain text").unwrap();
    let plain = Core::new()
        .open_document(plain.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(plain.syntax_language(), None);
    assert!(plain.viewport(0, 1).styles.is_empty());
}

#[test]
fn outline_uses_the_completed_current_syntax_tree() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outline.rs");
    std::fs::write(&path, "struct Item;\nfn make() -> Item { Item }\n").unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let outline = doc.outline();
        if !outline.is_empty() {
            assert_eq!(
                outline
                    .iter()
                    .map(|symbol| symbol.name.as_str())
                    .collect::<Vec<_>>(),
                ["Item", "make"]
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "syntax worker did not return an outline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn diagnostics_are_clipped_to_visible_utf16_lines() {
    let doc = Core::new().new_document();
    doc.edit(EditCommand::InsertText {
        text: "a😀\nxyz".into(),
    });
    doc.set_diagnostics(vec![DiagnosticInfo {
        start_line: 0,
        start_character: 1,
        end_line: 1,
        end_character: 2,
        severity: 1,
        message: "bad".into(),
        unnecessary: false,
    }]);
    let snap = doc.viewport(0, 2);
    assert_eq!(snap.lines[0].diagnostics[0].start, 1);
    assert_eq!(snap.lines[0].diagnostics[0].end, 3);
    assert_eq!(snap.lines[0].diagnostics[0].message, "bad");
    assert_eq!(snap.lines[1].diagnostics[0].start, 0);
    assert_eq!(snap.lines[1].diagnostics[0].end, 2);
}

fn folding_doc() -> Arc<DocumentHandle> {
    let doc = Core::new().new_document();
    doc.edit(EditCommand::InsertText {
        text: "fn a() {\n    one;\n    two;\n}\nfn b() {\n    three;\n}".into(),
    });
    doc
}

#[test]
fn sticky_scopes_name_the_headers_around_a_line() {
    let doc = folding_doc();
    assert_eq!(doc.sticky_scopes(1, 3), [0]);
    assert_eq!(doc.sticky_scopes(5, 3), [4]);
    assert!(doc.sticky_scopes(3, 3).is_empty());
    assert!(doc.sticky_scopes(99, 3).is_empty());
}

#[test]
fn folding_moves_a_hidden_caret_to_the_header() {
    let doc = folding_doc();
    doc.set_caret(TextPos {
        line: 2,
        col_utf16: 2,
    });
    doc.toggle_fold(0);
    assert_eq!(doc.primary_cursor().line, 0);
}

#[test]
fn arrows_step_over_folded_lines() {
    let doc = folding_doc();
    doc.toggle_fold(0);
    doc.set_caret(TextPos {
        line: 0,
        col_utf16: 0,
    });
    doc.move_cursors(CursorMotion::Down, false);
    assert_eq!(doc.primary_cursor().line, 3);
    doc.move_cursors(CursorMotion::Up, false);
    assert_eq!(doc.primary_cursor().line, 0);
}

#[test]
fn typing_in_a_folded_range_opens_it() {
    let doc = folding_doc();
    doc.toggle_fold(4);
    assert!(doc.has_folds());
    doc.set_caret(TextPos {
        line: 4,
        col_utf16: 0,
    });
    doc.edit(EditCommand::InsertText { text: "// ".into() });
    assert!(doc.has_folds(), "typing on the header keeps its fold");
    doc.set_caret(TextPos {
        line: 0,
        col_utf16: 0,
    });
    doc.edit(EditCommand::InsertText { text: "x\n".into() });
    assert!(doc.has_folds());
    assert_eq!(
        doc.fold_state(5),
        FoldState::Folded,
        "the fold moved down a line"
    );
}

#[test]
fn unnecessary_flag_reaches_the_viewport() {
    let doc = Core::new().new_document();
    doc.edit(EditCommand::InsertText {
        text: "use Foo;".into(),
    });
    doc.set_diagnostics(vec![DiagnosticInfo {
        start_line: 0,
        start_character: 0,
        end_line: 0,
        end_character: 8,
        severity: 4,
        message: "unused".into(),
        unnecessary: true,
    }]);
    let span = doc.viewport(0, 1).lines[0].diagnostics[0].clone();
    assert!(span.unnecessary);
    assert_eq!(span.severity, 4);
}

#[test]
fn go_to_line_and_history() {
    let doc = Core::new().new_document();
    doc.edit(EditCommand::InsertText {
        text: "one\n    two\nthree".into(),
    });
    assert_eq!(
        parse_line_target("2:3".into(), doc.line_count()),
        Some(LineTarget {
            line: 1,
            col: Some(2)
        })
    );
    doc.go_to_line(1, None);
    let here = doc.caret_location("k".into());
    assert_eq!((here.line, here.col), (1, 4), "first non-blank char");
    doc.go_to_line(99, Some(99));
    let end = doc.caret_location("k".into());
    assert_eq!((end.line, end.col), (2, 5), "clamped");
    let h = NavigationHistory::new();
    h.record(here.clone());
    assert!(h.can_go_back());
    assert_eq!(
        h.back(Some(NavLocation {
            key: "j".into(),
            ..end
        })),
        Some(here)
    );
    assert!(h.can_go_forward());
}

#[test]
fn open_document_reads_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rs");
    std::fs::write(&path, "fn main() {\n}\n").unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(doc.line_count(), 3);
    assert_eq!(doc.lines(0, 2), vec!["fn main() {", "}"]);
}

#[test]
fn save_preserves_crlf_and_utf16() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.txt");
    let mut src = vec![0xFF, 0xFE];
    for u in "a\r\nb\r\n".encode_utf16() {
        src.extend(u.to_le_bytes());
    }
    std::fs::write(&path, &src).unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(doc.encoding(), TextEncoding::Utf16Le);
    assert_eq!(doc.line_ending(), LineEndingKind::CrLf);
    assert!(doc.has_bom());
    doc.save().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), src);
    let other = dir.path().join("out.txt");
    doc.set_encoding(TextEncoding::Utf8);
    doc.set_line_ending(LineEndingKind::Lf);
    doc.save_as(other.to_string_lossy().into_owned()).unwrap();
    assert_eq!(std::fs::read(&other).unwrap(), b"\xEF\xBB\xBFa\nb\n");
    assert_eq!(doc.path(), other.to_string_lossy());
}

#[test]
fn new_document_is_empty_editable_and_needs_save_as() {
    let doc = Core::new().new_document();
    assert_eq!(doc.line_count(), 1);
    assert_eq!(doc.path(), "");
    doc.edit(EditCommand::InsertText { text: "hi".into() });
    assert_eq!(doc.lines(0, 1), vec!["hi"]);
    assert!(matches!(doc.save(), Err(CoreError::Io { .. })));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.txt");
    doc.save_as(path.to_string_lossy().into_owned()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hi");
    doc.save().unwrap();
}
