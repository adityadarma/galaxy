//! Transaction-based undo/redo (M1-04).
//!
//! Each history entry is a group of inverse change sets plus the
//! selections before and after the group. Undo applies the inverses (newest
//! first) and turns their results into a redo entry, so redo is the exact
//! mirror of undo and no forward copies are stored.
//!
//! Grouping: consecutive edits of the same kind (typing, deleting) merge
//! into one undo step while the user keeps going. A group ends when
//! - the pause since the previous edit exceeds `group_timeout` (500 ms),
//! - the edit kind changes (typing after backspace),
//! - the selections moved between edits (click or arrow keys), or
//! - the caller calls `break_group` (e.g. on save or focus loss).
//!
//! Limits: at most `max_steps` entries and roughly `max_bytes` of stored
//! text per document; the oldest entries are dropped first. The newest
//! entry is always kept so a single large paste stays undoable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::change::ChangeSet;
use crate::composition::Composition;
use crate::document::Document;
use crate::edit::Transaction;
use crate::selection::SelectionSet;

/// How an edit groups with its neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typing characters. Merges with adjacent typing.
    Insert,
    /// Backspace / delete. Merges with adjacent deletes.
    Delete,
    /// Paste, newline, indent, and other commands. Never merges.
    Other,
}

impl EditKind {
    fn merges(self) -> bool {
        self != EditKind::Other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryConfig {
    pub max_steps: usize,
    pub max_bytes: usize,
    pub group_timeout: Duration,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_steps: 1_000,
            max_bytes: 8 * 1024 * 1024,
            group_timeout: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    /// Identifies the text state this entry leads to (see `state_id`).
    id: u64,
    /// Change sets in recording order; reverting applies them in reverse.
    steps: Vec<ChangeSet>,
    sel_before: SelectionSet,
    sel_after: SelectionSet,
    kind: EditKind,
    last_edit: Instant,
    bytes: usize,
}

impl Entry {
    fn new(
        id: u64,
        step: ChangeSet,
        before: SelectionSet,
        after: SelectionSet,
        kind: EditKind,
        now: Instant,
    ) -> Self {
        let bytes = step.heap_size() + sel_size(&before) + sel_size(&after);
        Self {
            id,
            steps: vec![step],
            sel_before: before,
            sel_after: after,
            kind,
            last_edit: now,
            bytes,
        }
    }

    /// Applies the inverse steps to `doc` and returns the mirror entry.
    fn revert(self, doc: &mut Document) -> Entry {
        let mirror: Vec<ChangeSet> = self.steps.iter().rev().map(|s| doc.apply(s)).collect();
        let bytes = mirror.iter().map(ChangeSet::heap_size).sum::<usize>()
            + sel_size(&self.sel_before)
            + sel_size(&self.sel_after);
        // The mirror keeps the id: redo returns to the same state.
        Entry {
            id: self.id,
            steps: mirror,
            sel_before: self.sel_after,
            sel_after: self.sel_before,
            kind: self.kind,
            last_edit: self.last_edit,
            bytes,
        }
    }
}

fn sel_size(sel: &SelectionSet) -> usize {
    std::mem::size_of_val(sel.as_slice())
}

#[derive(Debug, Clone, Default)]
pub struct History {
    config: HistoryConfig,
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    undo_bytes: usize,
    /// Set by `break_group`; the next record starts a new entry.
    sealed: bool,
    /// Last id handed out; 0 is the initial text.
    last_id: u64,
    /// State with an empty undo stack: the initial text, or the state left
    /// behind by the oldest dropped entry.
    base_id: u64,
}

impl History {
    pub fn new(config: HistoryConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Approximate bytes held by the undo stack.
    pub fn undo_bytes(&self) -> usize {
        self.undo_bytes
    }

    /// Identifies the current text state. Undoing back to a state returns
    /// its id again, so comparing with the id saved at load/save tells
    /// whether the text differs from disk. Any new edit gets a fresh id.
    pub fn state_id(&self) -> u64 {
        self.undo.back().map_or(self.base_id, |e| e.id)
    }

    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// Ends the current group so the next edit starts a new undo step.
    pub fn break_group(&mut self) {
        self.sealed = true;
    }

    /// Records an applied edit. `inverse` is what `Document::apply`
    /// returned; empty sets are ignored. Clears the redo stack.
    pub fn record(
        &mut self,
        inverse: ChangeSet,
        sel_before: SelectionSet,
        sel_after: SelectionSet,
        kind: EditKind,
        now: Instant,
    ) {
        if inverse.is_empty() {
            return;
        }
        self.redo.clear();

        let sealed = std::mem::take(&mut self.sealed);
        if let Some(last) = self.undo.back_mut() {
            let mergeable = !sealed
                && kind.merges()
                && last.kind == kind
                && now.saturating_duration_since(last.last_edit) <= self.config.group_timeout
                && last.sel_after == sel_before;
            if mergeable {
                let old_bytes = last.bytes;
                last.bytes = old_bytes - sel_size(&last.sel_after)
                    + inverse.heap_size()
                    + sel_size(&sel_after);
                last.steps.push(inverse);
                last.sel_after = sel_after;
                last.last_edit = now;
                // Merged text is a new state.
                self.last_id += 1;
                last.id = self.last_id;
                self.undo_bytes = self.undo_bytes - old_bytes + last.bytes;
                self.enforce_limits();
                return;
            }
        }

        let id = self.next_id();
        let entry = Entry::new(id, inverse, sel_before, sel_after, kind, now);
        self.undo_bytes += entry.bytes;
        self.undo.push_back(entry);
        self.enforce_limits();
    }

    /// Reverts the newest group. Returns the selections from before it.
    pub fn undo(&mut self, doc: &mut Document) -> Option<SelectionSet> {
        let entry = self.undo.pop_back()?;
        self.undo_bytes -= entry.bytes;
        // `revert` swaps before/after, so the mirror's `sel_after` is the
        // state we are returning to.
        let mirror = entry.revert(doc);
        let sel = mirror.sel_after.clone();
        self.redo.push(mirror);
        self.sealed = true;
        Some(sel)
    }

    /// Re-applies the newest undone group. Returns the selections from
    /// after it.
    pub fn redo(&mut self, doc: &mut Document) -> Option<SelectionSet> {
        let entry = self.redo.pop()?;
        let mirror = entry.revert(doc);
        let sel = mirror.sel_after.clone();
        self.undo_bytes += mirror.bytes;
        self.undo.push_back(mirror);
        self.sealed = true;
        self.enforce_limits();
        Some(sel)
    }

    /// Records several already-applied change sets as ONE undo step, e.g. a
    /// whole IME composition. `steps` are inverses in application order.
    pub fn record_group(
        &mut self,
        steps: Vec<ChangeSet>,
        sel_before: SelectionSet,
        sel_after: SelectionSet,
    ) {
        let steps: Vec<ChangeSet> = steps.into_iter().filter(|s| !s.is_empty()).collect();
        if steps.is_empty() {
            return;
        }
        self.redo.clear();
        let bytes = steps.iter().map(ChangeSet::heap_size).sum::<usize>()
            + sel_size(&sel_before)
            + sel_size(&sel_after);
        let id = self.next_id();
        self.undo.push_back(Entry {
            id,
            steps,
            sel_before,
            sel_after,
            kind: EditKind::Other,
            last_edit: Instant::now(),
            bytes,
        });
        self.undo_bytes += bytes;
        self.enforce_limits();
    }

    /// Drops all history (e.g. after reloading the file from disk).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.undo_bytes = 0;
        self.sealed = false;
        // A fresh state: callers re-read `state_id` (e.g. after a reload).
        self.base_id = self.next_id();
    }

    fn enforce_limits(&mut self) {
        while self.undo.len() > 1
            && (self.undo.len() > self.config.max_steps || self.undo_bytes > self.config.max_bytes)
        {
            if let Some(old) = self.undo.pop_front() {
                self.undo_bytes -= old.bytes;
                self.base_id = old.id;
            }
        }
    }
}

/// A document with its selections and undo history: the editing state of
/// one buffer. The FFI layer wraps this.
#[derive(Debug, Clone)]
pub struct Editor {
    pub doc: Document,
    pub sel: SelectionSet,
    pub history: History,
    /// Active IME composition (marked text), if any. See `composition.rs`.
    pub(crate) composition: Option<Composition>,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        Self::with_history(doc, HistoryConfig::default())
    }

    pub fn with_history(doc: Document, config: HistoryConfig) -> Self {
        Self {
            doc,
            sel: SelectionSet::default(),
            history: History::new(config),
            composition: None,
        }
    }

    /// Applies an edit transaction and records it for undo.
    pub fn apply(&mut self, tx: Transaction, kind: EditKind, now: Instant) {
        // Commits marked text as-is; the document text is unchanged, so `tx`
        // (built from the current state) stays valid.
        self.unmark_text();
        if tx.is_noop() {
            return;
        }
        let before = self.sel.clone();
        let inverse = tx.apply(&mut self.doc, &mut self.sel);
        self.history
            .record(inverse, before, self.sel.clone(), kind, now);
    }

    /// Replaces the selections (cursor movement). Not an undo step, but it
    /// naturally ends the current typing group.
    pub fn set_selections(&mut self, sel: SelectionSet) {
        self.unmark_text();
        self.sel = sel;
    }

    /// Replaces the text with `new` (a file reloaded from disk, M2-03) as
    /// one undo step, changing only the part that differs, and takes its
    /// encoding, BOM and line ending. Returns whether the text changed.
    pub fn reload(&mut self, new: &Document, now: Instant) -> bool {
        self.unmark_text();
        self.history.break_group();
        let tx = crate::edit::replace_by_diff(&self.doc, &self.sel, &new.text());
        let changed = !tx.is_noop();
        self.apply(tx, EditKind::Other, now);
        self.history.break_group();
        self.doc.set_encoding(new.encoding());
        self.doc.set_has_bom(new.has_bom());
        self.doc.set_line_ending(new.line_ending());
        changed
    }

    /// Returns `false` when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        self.unmark_text();
        match self.history.undo(&mut self.doc) {
            Some(sel) => {
                self.sel = sel;
                true
            }
            None => false,
        }
    }

    /// Returns `false` when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        self.unmark_text();
        match self.history.redo(&mut self.doc) {
            Some(sel) => {
                self.sel = sel;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{self, IndentConfig};
    use crate::selection::Selection;

    const CFG: IndentConfig = IndentConfig {
        use_tabs: false,
        tab_size: 4,
    };

    struct Clock(Instant);

    impl Clock {
        fn tick(&mut self, ms: u64) -> Instant {
            self.0 += Duration::from_millis(ms);
            self.0
        }
    }

    fn editor(text: &str) -> (Editor, Clock) {
        let mut ed = Editor::new(Document::from_text(text));
        let end = ed.doc.len_chars();
        ed.sel = SelectionSet::single(Selection::caret(end));
        (ed, Clock(Instant::now()))
    }

    fn type_str(ed: &mut Editor, clock: &mut Clock, s: &str) {
        for ch in s.chars() {
            let tx = edit::insert_text(&ed.doc, &ed.sel, &ch.to_string());
            ed.apply(tx, EditKind::Insert, clock.tick(100));
        }
    }

    fn backspace(ed: &mut Editor, clock: &mut Clock) {
        let tx = edit::backspace(&ed.doc, &ed.sel, &CFG);
        ed.apply(tx, EditKind::Delete, clock.tick(100));
    }

    #[test]
    fn reload_changes_only_the_differing_part_as_one_undo_step() {
        let (mut ed, mut clock) = editor("one\ntwo\nthree\n");
        // Caret in "three", after the changed line.
        ed.sel = SelectionSet::single(Selection::caret(10));
        let before = ed.history.state_id();
        let mut disk = Document::from_text("one\r\nTWO!\r\nthree\r\n");
        disk.set_has_bom(true);
        assert!(ed.reload(&disk, clock.tick(10)));
        assert_eq!(ed.doc.text(), "one\nTWO!\nthree\n");
        assert_eq!(
            ed.sel.primary().head,
            11,
            "caret moved with the text after the change"
        );
        assert_eq!(ed.doc.line_ending(), crate::document::LineEnding::CrLf);
        assert!(ed.doc.has_bom());

        // Typing right after is a separate step.
        type_str(&mut ed, &mut clock, "x");
        assert!(ed.undo());
        assert!(ed.undo(), "the reload is undoable");
        assert_eq!(ed.doc.text(), "one\ntwo\nthree\n");
        assert_eq!(ed.history.state_id(), before);

        let same = Document::from_text("one\ntwo\nthree\n");
        assert!(
            !ed.reload(&same, clock.tick(10)),
            "same text: nothing recorded"
        );
    }

    #[test]
    fn reload_by_diff_respects_multibyte_boundaries() {
        let (mut ed, mut clock) = editor("aé😀b");
        assert!(ed.reload(&Document::from_text("aè😀b"), clock.tick(10)));
        assert_eq!(ed.doc.text(), "aè😀b");
        assert!(ed.reload(&Document::from_text(""), clock.tick(10)));
        assert_eq!(ed.doc.text(), "");
        assert!(ed.reload(&Document::from_text("😀😀"), clock.tick(10)));
        assert_eq!(ed.doc.text(), "😀😀");
    }

    #[test]
    fn state_id_returns_when_undoing_and_redoing() {
        let (mut ed, mut clock) = editor("");
        let start = ed.history.state_id();
        type_str(&mut ed, &mut clock, "ab");
        let ab = ed.history.state_id();
        assert_ne!(ab, start);
        clock.tick(600);
        type_str(&mut ed, &mut clock, "c");
        assert_ne!(ed.history.state_id(), ab);
        ed.undo();
        assert_eq!(ed.history.state_id(), ab);
        ed.undo();
        assert_eq!(ed.history.state_id(), start);
        ed.redo();
        assert_eq!(ed.history.state_id(), ab);
        // A different edit from the same text is a new state.
        type_str(&mut ed, &mut clock, "x");
        let abx = ed.history.state_id();
        ed.undo();
        assert_eq!(ed.history.state_id(), ab);
        assert_ne!(abx, ab);
    }

    #[test]
    fn merged_typing_changes_the_state_id() {
        let (mut ed, mut clock) = editor("");
        type_str(&mut ed, &mut clock, "a");
        let a = ed.history.state_id();
        type_str(&mut ed, &mut clock, "b");
        assert_eq!(ed.history.undo_len(), 1);
        assert_ne!(ed.history.state_id(), a, "merged edit is new text");
    }

    #[test]
    fn dropped_entries_keep_the_base_state() {
        let mut ed = Editor::with_history(
            Document::from_text(""),
            HistoryConfig {
                max_steps: 2,
                ..HistoryConfig::default()
            },
        );
        let mut clock = Clock(Instant::now());
        let mut ids = vec![ed.history.state_id()];
        for ch in ["a", "b", "c"] {
            clock.tick(600);
            type_str(&mut ed, &mut clock, ch);
            ids.push(ed.history.state_id());
        }
        assert_eq!(ed.history.undo_len(), 2);
        while ed.undo() {}
        assert_eq!(ed.doc.text(), "a");
        assert_eq!(ed.history.state_id(), ids[1]);
    }

    #[test]
    fn continuous_typing_is_one_undo_step() {
        let (mut ed, mut clock) = editor("");
        type_str(&mut ed, &mut clock, "hello");
        assert_eq!(ed.history.undo_len(), 1);
        assert!(ed.undo());
        assert_eq!(ed.doc.text(), "");
        assert_eq!(ed.sel.primary(), Selection::caret(0));
        assert!(!ed.undo());
    }

    #[test]
    fn pause_longer_than_timeout_splits_groups() {
        let (mut ed, mut clock) = editor("");
        type_str(&mut ed, &mut clock, "ab");
        clock.tick(600);
        type_str(&mut ed, &mut clock, "cd");
        assert_eq!(ed.history.undo_len(), 2);
        ed.undo();
        assert_eq!(ed.doc.text(), "ab");
        ed.undo();
        assert_eq!(ed.doc.text(), "");
    }

    #[test]
    fn kind_change_and_cursor_move_split_groups() {
        let (mut ed, mut clock) = editor("");
        type_str(&mut ed, &mut clock, "abc");
        backspace(&mut ed, &mut clock);
        backspace(&mut ed, &mut clock);
        assert_eq!(ed.history.undo_len(), 2);

        // Moving the caret ends the typing group even without a pause.
        ed.set_selections(SelectionSet::single(Selection::caret(0)));
        type_str(&mut ed, &mut clock, "X");
        assert_eq!(ed.history.undo_len(), 3);
        assert_eq!(ed.doc.text(), "Xa");

        ed.undo();
        assert_eq!(ed.doc.text(), "a");
        ed.undo();
        assert_eq!(ed.doc.text(), "abc");
        assert_eq!(ed.sel.primary(), Selection::caret(3));
    }

    #[test]
    fn other_edits_never_merge() {
        let (mut ed, mut clock) = editor("a");
        for _ in 0..3 {
            let tx = edit::paste(&ed.doc, &ed.sel, "x");
            ed.apply(tx, EditKind::Other, clock.tick(10));
        }
        assert_eq!(ed.history.undo_len(), 3);
    }

    #[test]
    fn break_group_splits_typing() {
        let (mut ed, mut clock) = editor("");
        type_str(&mut ed, &mut clock, "ab");
        ed.history.break_group();
        type_str(&mut ed, &mut clock, "cd");
        assert_eq!(ed.history.undo_len(), 2);
    }

    #[test]
    fn redo_mirrors_undo_and_new_edit_clears_redo() {
        let (mut ed, mut clock) = editor("x");
        type_str(&mut ed, &mut clock, "yz");
        ed.undo();
        assert_eq!(ed.doc.text(), "x");
        assert!(ed.history.can_redo());
        assert!(ed.redo());
        assert_eq!(ed.doc.text(), "xyz");
        assert_eq!(ed.sel.primary(), Selection::caret(3));
        assert!(!ed.redo());

        ed.undo();
        type_str(&mut ed, &mut clock, "!");
        assert!(!ed.history.can_redo());
        assert_eq!(ed.doc.text(), "x!");
    }

    #[test]
    fn multi_cursor_undo_restores_all_selections() {
        let doc = Document::from_text("a\nb\nc");
        let mut ed = Editor::new(doc);
        ed.sel = SelectionSet::new(
            vec![
                Selection::caret(1),
                Selection::caret(3),
                Selection::caret(5),
            ],
            1,
            &ed.doc,
        );
        let before = ed.sel.clone();
        let mut clock = Clock(Instant::now());
        type_str(&mut ed, &mut clock, "!!");
        assert_eq!(ed.doc.text(), "a!!\nb!!\nc!!");
        ed.undo();
        assert_eq!(ed.doc.text(), "a\nb\nc");
        assert_eq!(ed.sel, before);
    }

    #[test]
    fn step_limit_drops_oldest() {
        let config = HistoryConfig {
            max_steps: 3,
            ..HistoryConfig::default()
        };
        let mut ed = Editor::with_history(Document::from_text(""), config);
        let mut clock = Clock(Instant::now());
        for c in ["a", "b", "c", "d", "e"] {
            let tx = edit::paste(&ed.doc, &ed.sel, c);
            ed.apply(tx, EditKind::Other, clock.tick(10));
        }
        assert_eq!(ed.history.undo_len(), 3);
        while ed.undo() {}
        assert_eq!(ed.doc.text(), "ab");
    }

    #[test]
    fn byte_limit_drops_oldest_but_keeps_newest() {
        let config = HistoryConfig {
            max_bytes: 4 * 1024,
            ..HistoryConfig::default()
        };
        let mut ed = Editor::with_history(Document::from_text(""), config);
        let mut clock = Clock(Instant::now());
        let big = "x".repeat(3 * 1024);
        for _ in 0..3 {
            // Deleting stores the removed text in the inverse.
            let tx = edit::paste(&ed.doc, &ed.sel, &big);
            ed.apply(tx, EditKind::Other, clock.tick(10));
            ed.sel = SelectionSet::single(Selection::new(0, ed.doc.len_chars()));
            let tx = edit::backspace(&ed.doc, &ed.sel, &CFG);
            ed.apply(tx, EditKind::Other, clock.tick(10));
        }
        assert!(ed.history.undo_bytes() <= 4 * 1024 || ed.history.undo_len() == 1);
        assert!(ed.history.can_undo());

        // A single edit larger than the limit is still undoable.
        let huge = "y".repeat(16 * 1024);
        let tx = edit::paste(&ed.doc, &ed.sel, &huge);
        ed.apply(tx, EditKind::Other, clock.tick(10));
        ed.sel = SelectionSet::single(Selection::new(0, ed.doc.len_chars()));
        let tx = edit::backspace(&ed.doc, &ed.sel, &CFG);
        ed.apply(tx, EditKind::Other, clock.tick(10));
        assert_eq!(ed.history.undo_len(), 1);
        assert!(ed.undo());
        assert_eq!(ed.doc.len_chars(), huge.len());
    }

    #[test]
    fn noop_edits_are_not_recorded() {
        let (mut ed, mut clock) = editor("");
        backspace(&mut ed, &mut clock); // nothing to delete
        assert!(!ed.history.can_undo());
    }
}
