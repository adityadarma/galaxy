//! Core document benchmarks (M0-07).
//!
//! Budgets from docs/PRD.md section 6 are noted next to each benchmark.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use galaxy_core::document::Document;
use galaxy_core::edit;
use galaxy_core::find::{FindOptions, Finder};
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::viewport;

fn load(c: &mut Criterion) {
    let text = galaxy_bench::source_string(1 << 20, 1);
    let mut g = c.benchmark_group("document_load");
    g.throughput(Throughput::Bytes(text.len() as u64));
    // Budget: open 1 MB file < 30 ms (this measures the in-memory part only).
    g.bench_function("from_text_1mb", |b| {
        b.iter(|| Document::from_text(black_box(&text)))
    });
    g.finish();
}

fn viewport(c: &mut Criterion) {
    let doc = Document::from_text(&galaxy_bench::source_lines(50_000, 2));
    // Budget: viewport snapshot of 100 lines < 0.5 ms (M1-05).
    c.bench_function("viewport_100_lines_mid_50k", |b| {
        b.iter(|| doc.lines(black_box(25_000), black_box(100)))
    });
}

fn edit(c: &mut Criterion) {
    let doc = Document::from_text(&galaxy_bench::source_lines(50_000, 2));
    let mid = doc.len_chars() / 2;
    // Budget: keypress-to-render < 8 ms; a single insert must be microseconds.
    c.bench_function("insert_char_mid_50k", |b| {
        b.iter_batched_ref(
            || doc.clone(),
            |d| d.insert(black_box(mid), "x"),
            BatchSize::SmallInput,
        )
    });
}

fn multi_cursor(c: &mut Criterion) {
    let doc = Document::from_text(&galaxy_bench::source_lines(50_000, 2));
    // 100 carets spread over the file, like a large Cmd+D selection.
    let carets: Vec<Selection> = (0..100)
        .map(|i| Selection::caret(doc.line_to_char(i * 500)))
        .collect();
    let sel = SelectionSet::new(carets, 0, &doc);
    // Budget: keypress-to-render < 8 ms including render; core must be far below.
    c.bench_function("type_char_100_cursors_50k", |b| {
        b.iter_batched(
            || (doc.clone(), sel.clone()),
            |(mut d, mut s)| edit::insert_text(&d, &s, "x").apply(&mut d, &mut s),
            BatchSize::SmallInput,
        )
    });
}

fn snapshot(c: &mut Criterion) {
    let doc = Document::from_text(&galaxy_bench::source_lines(50_000, 2));
    // A selection spanning the viewport plus a caret on many visible lines.
    let first = doc.line_to_char(25_000);
    let mut sels = vec![Selection::new(first + 3, doc.line_to_char(25_040) + 5)];
    sels.extend((25_050..25_100).map(|l| Selection::caret(doc.line_to_char(l) + 4)));
    let sel = SelectionSet::new(sels, 0, &doc);
    // Budget (M1-05): snapshot of 100 lines < 0.5 ms.
    c.bench_function("viewport_snapshot_100_lines_50k", |b| {
        b.iter(|| viewport::snapshot(&doc, &sel, black_box(25_000), black_box(100)))
    });
}

/// Large-file scenarios (M1-14). Few samples: each iteration is big.
fn large(c: &mut Criterion) {
    let mut g = c.benchmark_group("large");
    g.sample_size(10);

    // Budget (PRD 6): open a 100 MB file < 500 ms. In-memory part only.
    let bytes = galaxy_bench::source_string(100 << 20, 3).into_bytes();
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("from_bytes_100mb", |b| {
        b.iter(|| Document::from_bytes(black_box(&bytes)).unwrap())
    });
    g.throughput(Throughput::Elements(1));

    // A minified file: one 10 MB line. The viewport must not copy it all.
    let one_line = Document::from_text(&"x = 1; ".repeat((10 << 20) / 7));
    let sel = SelectionSet::single(Selection::caret(5));
    g.bench_function("snapshot_one_10mb_line", |b| {
        b.iter(|| viewport::snapshot(&one_line, &sel, 0, black_box(60)))
    });

    // Find count over 20 MB, as the find bar does after each keystroke.
    let big = Document::from_bytes(&bytes[..20 << 20]).unwrap();
    let finder = Finder::new("usize", FindOptions::default()).unwrap();
    let caret = SelectionSet::single(Selection::caret(0));
    g.bench_function("find_summary_20mb", |b| {
        b.iter(|| finder.summary(black_box(&big), &caret))
    });
    g.bench_function("find_next_20mb", |b| {
        b.iter(|| finder.find_next(black_box(&big), big.len_chars() / 2, true))
    });
    g.finish();
}

criterion_group!(benches, load, viewport, edit, multi_cursor, snapshot, large);
criterion_main!(benches);
