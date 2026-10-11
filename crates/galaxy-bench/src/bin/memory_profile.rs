//! Heap profile workload for `dhat` (M7-02).
//!
//! Run with `cargo run -p galaxy-bench --release --bin memory-profile -- 100000`.
//! The resulting `dhat-heap.json` can be opened in dhat's web viewer.

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use galaxy_core::document::Document;
use galaxy_core::file_finder::FileFinder;
use galaxy_core::workspace::Workspace;
use std::env;
use std::path::PathBuf;

fn main() {
    let _profiler = dhat::Profiler::new_heap();
    let lines = env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let source = (0..lines)
        .map(|line| format!("    let value_{line} = compute(input, {line});\n"))
        .collect::<String>();
    let document = Document::from_text(&source);
    let _snapshot = document.lines(lines / 2, 100);

    let root = tempfile::tempdir().expect("temporary profile workspace");
    for index in 0..lines {
        let path = root.path().join(format!("src/module_{index}.rs"));
        let parent = path.parent().expect("module parent");
        std::fs::create_dir_all(parent).expect("module directory");
        std::fs::write(path, "fn main() {}\n").expect("module file");
    }
    let workspace = Workspace::open(PathBuf::from(root.path())).expect("profile workspace");
    let entries = workspace.build_file_index();
    let _finder = FileFinder::from_index(&entries);
    eprintln!("profiled {lines} lines and files");
}
