//! Background Tree-sitter parsing (M3-01).
//!
//! A [`SyntaxService`] belongs to one document and owns one parser on one
//! worker thread. Editing never waits for parsing: it clones Ropey's cheap
//! rope handle, tags it with `Document::revision`, and polls completed trees
//! on the UI side. A tree for an older revision is discarded.
//!
//! M3-02 adds `InputEdit` and the previous tree for incremental parsing;
//! M3-03 supplies the bundled grammars. This layer intentionally knows
//! neither language names nor highlight queries.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};

use ropey::Rope;
use tree_sitter::{InputEdit, Language, Parser, Point, Tree};

use crate::document::Document;

struct Request {
    revision: u64,
    text: Rope,
    /// The revision that `edit` transforms, when this is an edit request.
    base_revision: Option<u64>,
    edit: Option<SyntaxEdit>,
}

/// One normalized text mutation expressed in Tree-sitter byte coordinates.
///
/// `from_byte_ranges` computes the row/byte-column points from Rope chunks.
/// The byte positions must be UTF-8 character boundaries; editor changes
/// already satisfy that invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: Point,
    pub old_end_position: Point,
    pub new_end_position: Point,
}

impl SyntaxEdit {
    pub fn from_byte_ranges(
        old: &Rope,
        new: &Rope,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
    ) -> Self {
        Self {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position: point_at_byte(old, start_byte),
            old_end_position: point_at_byte(old, old_end_byte),
            new_end_position: point_at_byte(new, new_end_byte),
        }
    }

    fn input_edit(self) -> InputEdit {
        InputEdit {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: self.start_position,
            old_end_position: self.old_end_position,
            new_end_position: self.new_end_position,
        }
    }
}

fn point_at_byte(text: &Rope, byte: usize) -> Point {
    let byte = byte.min(text.len_bytes());
    let char_idx = text.byte_to_char(byte);
    let row = text.char_to_line(char_idx);
    Point {
        row,
        column: byte - text.line_to_byte(row),
    }
}

/// A parsed concrete syntax tree, tied to the document revision it parsed.
#[derive(Debug)]
pub struct SyntaxSnapshot {
    revision: u64,
    tree: Tree,
    incremental: bool,
}

impl SyntaxSnapshot {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Whether this tree reused an immediately previous parsed tree.
    pub fn is_incremental(&self) -> bool {
        self.incremental
    }
}

/// One background parser for one document.
///
/// `Language` is injected so this module remains independent of the grammar
/// bundle. Send a document after loading and after each text change, then
/// poll from the UI event loop without blocking.
pub struct SyntaxService {
    requests: Option<Sender<Request>>,
    results: Receiver<SyntaxSnapshot>,
    latest_requested: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl SyntaxService {
    pub fn new(language: Language) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<SyntaxSnapshot>();
        let latest_requested = Arc::new(AtomicU64::new(0));
        let latest_for_worker = Arc::clone(&latest_requested);
        let worker = thread::Builder::new()
            .name("galaxy-syntax".into())
            .spawn(move || run(language, request_rx, result_tx, latest_for_worker))
            .expect("creating syntax worker thread");
        Self {
            requests: Some(request_tx),
            results: result_rx,
            latest_requested,
            worker: Some(worker),
        }
    }

    /// Queues the document's current rope. Cloning a Rope is O(1): text is
    /// still read by Tree-sitter through rope chunks on the worker.
    pub fn submit_document(&self, doc: &Document) {
        self.submit(doc.revision(), doc.rope().clone());
    }

    pub fn submit(&self, revision: u64, text: Rope) {
        self.send(Request {
            revision,
            text,
            base_revision: None,
            edit: None,
        });
    }

    /// Queues an edit from `base_revision` to `revision`. The worker uses the
    /// old tree only when it has parsed exactly `base_revision`; otherwise it
    /// parses the submitted rope in full, preserving correctness under load.
    pub fn submit_edit(&self, base_revision: u64, revision: u64, text: Rope, edit: SyntaxEdit) {
        self.send(Request {
            revision,
            text,
            base_revision: Some(base_revision),
            edit: Some(edit),
        });
    }

    fn send(&self, request: Request) {
        let revision = request.revision;
        self.latest_requested.store(revision, Ordering::Release);
        // The receiver only disappears while Drop is joining the worker.
        let _ = self
            .requests
            .as_ref()
            .expect("syntax service is alive")
            .send(request);
    }

    /// Returns the newest completed tree, if any. Older results are ignored
    /// even if a request raced with worker completion.
    pub fn poll(&self) -> Option<SyntaxSnapshot> {
        let mut newest = None;
        loop {
            match self.results.try_recv() {
                Ok(snapshot)
                    if snapshot.revision == self.latest_requested.load(Ordering::Acquire) =>
                {
                    newest = Some(snapshot);
                }
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return newest,
            }
        }
    }
}

impl Drop for SyntaxService {
    fn drop(&mut self) {
        // Closing the sender lets the worker finish its current parse then
        // exit. Joining avoids a parser thread outliving its document.
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(
    language: Language,
    requests: Receiver<Request>,
    results: Sender<SyntaxSnapshot>,
    latest: Arc<AtomicU64>,
) {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("valid Tree-sitter language");
    let mut previous = None::<(u64, Tree)>;
    while let Ok(request) = requests.recv() {
        let text = request.text;
        let incremental = matches!(
            (request.base_revision, request.edit, previous.as_ref()),
            (Some(base), Some(_), Some((revision, _))) if base == *revision
        );
        let old_tree = if incremental {
            let (_, tree) = previous.as_mut().expect("matched above");
            tree.edit(&request.edit.expect("matched above").input_edit());
            Some(tree.clone())
        } else {
            None
        };
        let tree = parser.parse_with_options(
            &mut |byte, _| {
                if byte >= text.len_bytes() {
                    return "";
                }
                let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
                &chunk[byte - chunk_start..]
            },
            old_tree.as_ref(),
            None,
        );
        let Some(tree) = tree else {
            previous = None;
            continue;
        };
        previous = Some((request.revision, tree.clone()));
        // A newer document was submitted while parsing. Do not make an old
        // tree observable; the next request will replace parser state later.
        if request.revision != latest.load(Ordering::Acquire) {
            continue;
        }
        let _ = results.send(SyntaxSnapshot {
            revision: request.revision,
            tree,
            incremental,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn wait_for(service: &SyntaxService) -> SyntaxSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(snapshot) = service.poll() {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "syntax worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_parses_rope_chunks_without_blocking_submitter() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let doc = Document::from_text("{\"answer\": 42}");
        service.submit_document(&doc);

        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), doc.revision());
        assert_eq!(snapshot.tree().root_node().kind(), "document");
        assert!(!snapshot.tree().root_node().has_error());
    }

    #[test]
    fn a_stale_tree_is_not_observable() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        // A large first request makes it very likely the second arrives while
        // the worker is still parsing; correctness does not depend on timing.
        service.submit(1, Rope::from_str(&format!("[{}]", "0,".repeat(100_000))));
        service.submit(2, Rope::from_str("[true]"));

        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), 2);
        assert_eq!(snapshot.tree().root_node().kind(), "document");
    }

    #[test]
    fn parses_an_edit_incrementally_with_utf8_and_newlines() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let before = Rope::from_str("{\"name\": \"world\"}");
        service.submit(10, before.clone());
        let first = wait_for(&service);
        assert!(!first.is_incremental());

        let after = Rope::from_str("{\n  \"name\": \"世界\"\n}");
        let edit = SyntaxEdit::from_byte_ranges(
            &before,
            &after,
            1,
            before.len_bytes() - 1,
            after.len_bytes() - 1,
        );
        assert_eq!(edit.start_position, Point { row: 0, column: 1 });
        assert_eq!(edit.new_end_position, Point { row: 2, column: 0 });
        service.submit_edit(10, 11, after, edit);

        let second = wait_for(&service);
        assert_eq!(second.revision(), 11);
        assert!(second.is_incremental());
        assert!(!second.tree().root_node().has_error());
    }

    #[test]
    fn non_contiguous_edit_falls_back_to_full_parse() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let first = Rope::from_str("[1]");
        service.submit(1, first.clone());
        let _ = wait_for(&service);

        let second = Rope::from_str("[2]");
        let edit = SyntaxEdit::from_byte_ranges(&first, &second, 1, 2, 2);
        // Revision 7 was never parsed, so using its edit would be invalid.
        service.submit_edit(7, 8, second, edit);
        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), 8);
        assert!(!snapshot.is_incremental());
        assert!(!snapshot.tree().root_node().has_error());
    }
}
