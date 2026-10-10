//! Background Tree-sitter parsing (M3-01).
//!
//! A [`SyntaxService`] belongs to one document and owns one parser on one
//! worker thread. Editing never waits for parsing: it clones Ropey's cheap
//! rope handle, tags it with `Document::revision`, and polls completed trees
//! on the UI side. A tree for an older revision is discarded.
//!
//! M3-02 supplies `InputEdit` and the previous tree for incremental parsing;
//! M3-03 supplies the bundled grammars. This layer intentionally knows
//! neither language names nor highlight queries.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};

use ropey::Rope;
use tree_sitter::{Language, Parser, Tree};

use crate::document::Document;

struct Request {
    revision: u64,
    text: Rope,
}

/// A parsed concrete syntax tree, tied to the document revision it parsed.
#[derive(Debug)]
pub struct SyntaxSnapshot {
    revision: u64,
    tree: Tree,
}

impl SyntaxSnapshot {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
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
        self.latest_requested.store(revision, Ordering::Release);
        // The receiver only disappears while Drop is joining the worker.
        let _ = self
            .requests
            .as_ref()
            .expect("syntax service is alive")
            .send(Request { revision, text });
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
    while let Ok(request) = requests.recv() {
        let text = request.text;
        let tree = parser.parse_with_options(
            &mut |byte, _| {
                if byte >= text.len_bytes() {
                    return "";
                }
                let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
                &chunk[byte - chunk_start..]
            },
            None,
            None,
        );
        // A newer document was submitted while parsing. Do not make an old
        // tree observable; the next request will replace parser state later.
        if request.revision != latest.load(Ordering::Acquire) {
            continue;
        }
        if let Some(tree) = tree {
            let _ = results.send(SyntaxSnapshot {
                revision: request.revision,
                tree,
            });
        }
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
}
