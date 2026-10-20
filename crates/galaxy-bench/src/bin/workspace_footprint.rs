//! Core-side footprint workload: 10k-file workspace with 5 open tabs.
//!
//! Usage:
//! `cargo run -p galaxy-bench --release --bin workspace-footprint -- [repo] [tabs] [extra files...]`
//!
//! Defaults to `fixtures/repo_10k` (create it with `gen-fixtures`) and 5
//! tabs. It mirrors what the app does per tab: open, background parse,
//! viewport draws with syntax, typing bursts. RSS is
//! sampled with `ps` after each phase and printed as `phase=<name> rss_mb=`
//! lines plus a final `peak_rss_mb=`. The numbers cover Galaxy's own process
//! only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use galaxy_core::document::Document;
use galaxy_core::edit;
use galaxy_core::file_finder::FileFinder;
use galaxy_core::history::{EditKind, Editor};
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::syntax::{SyntaxLanguage, SyntaxService, SyntaxSnapshot};
use galaxy_core::workspace::Workspace;

/// Counts live and peak heap bytes so phases can separate memory still in
/// use (live) from allocator high-water marks that only show up in RSS.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator unchanged; only adds counters.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            let live = LIVE.fetch_add(new_size, Ordering::Relaxed) + new_size;
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        new
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Keystrokes typed into each tab.
const KEYSTROKES: usize = 200;
/// Lines drawn per frame, as on a tall editor window.
const VISIBLE_LINES: usize = 60;

struct Tab {
    editor: Editor,
    language: SyntaxLanguage,
    syntax: SyntaxService,
    snapshot: Option<SyntaxSnapshot>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(args.next().unwrap_or_else(|| "fixtures/repo_10k".into()));
    let tab_count: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(5);
    if !repo.is_dir() {
        eprintln!(
            "{} not found; run `cargo run -p galaxy-bench --release --bin gen-fixtures`",
            repo.display()
        );
        std::process::exit(2);
    }

    let mut peak = 0.0f64;
    let mut sample = |phase: &str| {
        let mb = rss_mb();
        peak = peak.max(mb);
        let live = LIVE.load(Ordering::Relaxed) as f64 / 1048576.0;
        // Peak since the previous phase, then reset for the next one.
        let phase_peak =
            PEAK.swap(LIVE.load(Ordering::Relaxed), Ordering::Relaxed) as f64 / 1048576.0;
        let (malloc_used, malloc_reserved) = malloc_mb();
        println!(
            "phase={phase} rss_mb={mb:.1} rust_heap_mb={live:.1} rust_phase_peak_mb={phase_peak:.1} \
             malloc_used_mb={malloc_used:.1} malloc_reserved_mb={malloc_reserved:.1}"
        );
    };
    sample("start");

    let workspace = Workspace::open(repo.clone()).expect("open workspace");
    let index = workspace.build_file_index();
    let finder = FileFinder::from_index(&index);
    std::hint::black_box(&finder);
    println!("files={}", index.len());
    sample("index");

    // Extra paths after the tab count open as additional tabs, e.g. a 1 MB
    // file to show per-draw cost on a large document.
    let mut paths = pick_files(&repo, &index, tab_count);
    paths.extend(args.map(PathBuf::from));
    let mut tabs: Vec<Tab> = paths.iter().filter_map(|path| open_tab(path)).collect();
    println!("tabs={}", tabs.len());
    for tab in &mut tabs {
        wait_for_parse(tab);
    }
    sample("tabs_parsed");

    let now = Instant::now();
    for tab in &mut tabs {
        for key in 0..KEYSTROKES {
            let tx = edit::insert_text(
                &tab.editor.doc,
                &tab.editor.sel,
                if key % 40 == 39 { "\n" } else { "x" },
            );
            tab.editor.apply(
                tx,
                EditKind::Insert,
                now + Duration::from_millis(key as u64 * 30),
            );
            tab.syntax.submit_document(&tab.editor.doc);
            // The draw that follows every keystroke in the app.
            draw(tab, 0);
        }
        wait_for_parse(tab);
    }
    sample("typing");

    for tab in &mut tabs {
        let lines = tab.editor.doc.line_count();
        let mut first = 0;
        while first < lines {
            draw(tab, first);
            first += VISIBLE_LINES;
        }
    }
    sample("scroll");

    // A second pass separates leaks (keeps growing) from allocator
    // high-water marks (flat).
    for tab in &mut tabs {
        let lines = tab.editor.doc.line_count();
        let mut first = 0;
        while first < lines {
            draw(tab, first);
            first += VISIBLE_LINES;
        }
    }
    sample("scroll_again");
    println!(
        "undo_bytes={}",
        tabs.iter()
            .map(|t| t.editor.history.undo_bytes())
            .sum::<usize>()
    );

    drop(index);
    println!("peak_rss_mb={peak:.1}");
}

fn pick_files(
    repo: &Path,
    index: &galaxy_core::workspace::WorkspaceIndex,
    count: usize,
) -> Vec<PathBuf> {
    // Prefer files with a grammar so every tab exercises syntax.
    let mut picked = Vec::new();
    if count == 0 {
        return picked;
    }
    for i in 0..index.len() {
        let Some(rel) = index.path(i) else { continue };
        if SyntaxLanguage::detect(&rel, "").is_some() {
            picked.push(repo.join(rel));
            if picked.len() == count {
                break;
            }
        }
    }
    picked
}

fn open_tab(path: &Path) -> Option<Tab> {
    let doc = Document::load(path).ok()?;
    let language = SyntaxLanguage::detect(&path.to_string_lossy(), "")?;
    let syntax = SyntaxService::new(language.grammar());
    syntax.submit_document(&doc);
    let mut editor = Editor::new(doc);
    let end = editor.doc.len_chars();
    editor.set_selections(SelectionSet::single(Selection::caret(end / 2)));
    Some(Tab {
        editor,
        language,
        syntax,
        snapshot: None,
    })
}

fn wait_for_parse(tab: &mut Tab) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let target = tab.editor.doc.revision();
    while Instant::now() < deadline {
        while let Some(snapshot) = tab.syntax.poll() {
            tab.snapshot = Some(snapshot);
        }
        if tab
            .snapshot
            .as_ref()
            .is_some_and(|s| s.revision() >= target)
        {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn draw(tab: &mut Tab, first: usize) {
    while let Some(snapshot) = tab.syntax.poll() {
        tab.snapshot = Some(snapshot);
    }
    let snap = match &tab.snapshot {
        Some(s) => tab
            .editor
            .viewport_with_syntax(first, VISIBLE_LINES, s.tree(), tab.language),
        None => tab.editor.viewport(first, VISIBLE_LINES),
    };
    std::hint::black_box(snap);
}

/// `malloc_statistics_t` from `<malloc/malloc.h>`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct MallocStatistics {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut MallocStatistics);
}

/// All-zone malloc bytes in use and reserved, in MB. Covers C allocations
/// (Tree-sitter) that the Rust counter cannot see.
#[cfg(target_os = "macos")]
fn malloc_mb() -> (f64, f64) {
    let mut stats = MallocStatistics::default();
    // SAFETY: a null zone asks for all zones; `stats` is a valid out-pointer.
    #[allow(unsafe_code)]
    unsafe {
        malloc_zone_statistics(std::ptr::null_mut(), &mut stats)
    };
    (
        stats.size_in_use as f64 / 1048576.0,
        stats.size_allocated as f64 / 1048576.0,
    )
}

#[cfg(not(target_os = "macos"))]
fn malloc_mb() -> (f64, f64) {
    (0.0, 0.0)
}

/// Resident set size of this process via `ps` (KiB on macOS and Linux).
fn rss_mb() -> f64 {
    Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map_or(0.0, |kib| kib / 1024.0)
}
