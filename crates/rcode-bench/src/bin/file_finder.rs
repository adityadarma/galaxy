//! Go to File per-keystroke latency (M2-07). PRD target: < 30 ms per
//! keystroke on 100k files.
//!
//! Usage: `cargo run -p rcode-bench --release --bin file-finder -- [PATH] [--files N]`
//!
//! Without PATH, paths are generated in memory with the same shape as
//! `gen-fixtures --large` (`src/mod_0_3/mod_1_7/buffer_123.rs`), so the
//! benchmark needs no 100k files on disk. With PATH, the workspace is
//! indexed for real.

use std::time::Instant;

use rcode_bench::Rng;
use rcode_core::file_finder::FileFinder;
use rcode_core::workspace::{IndexDelta, Workspace};

const IDENTS: &[&str] = &[
    "buffer",
    "cursor",
    "index",
    "range",
    "token",
    "parser",
    "node",
    "value",
    "result",
    "config",
    "workspace",
    "document",
    "selection",
    "viewport",
    "handle",
];

fn synthetic(count: usize) -> Vec<String> {
    let mut rng = Rng::new(4);
    let exts = ["rs", "ts", "py", "go", "json", "md"];
    (0..count)
        .map(|i| {
            let mut path = String::from("src");
            for d in 0..1 + rng.below(4) {
                path.push_str(&format!("/mod_{d}_{}", rng.below(12)));
            }
            let name = IDENTS[rng.below(IDENTS.len() as u64) as usize];
            let ext = exts[rng.below(exts.len() as u64) as usize];
            format!("{path}/{name}_{i}.{ext}")
        })
        .collect()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() -> std::io::Result<()> {
    let mut files = 100_000usize;
    let mut path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--files" => files = args.next().and_then(|n| n.parse().ok()).unwrap_or(files),
            _ => path = Some(arg),
        }
    }

    let start = Instant::now();
    let mut finder = match &path {
        Some(p) => FileFinder::from_index(&Workspace::open(p)?.build_file_index()),
        None => FileFinder::from_paths(synthetic(files)),
    };
    let build_ms = start.elapsed().as_secs_f64() * 1e3;
    let recent: Vec<String> = (0..20)
        .filter_map(|i| finder.path(i * 997).map(str::to_owned))
        .collect();
    finder.set_context(finder.path(12_345).map(str::to_owned).as_deref(), &recent);

    // Typed one character at a time, as a user would, plus a backspace.
    let queries = [
        "buffer",
        "src mod_2 cursor",
        "viewport_9",
        "docmnt json",
        "zzzz",
        "parser_12345.rs",
    ];
    let mut times = Vec::new();
    let mut first_key = Vec::new();
    for query in queries {
        for end in 1..=query.len() {
            let t = Instant::now();
            let results = finder.search(&query[..end], 50);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            std::hint::black_box(results);
            times.push(ms);
            if end == 1 {
                first_key.push(ms);
            }
        }
        let t = Instant::now();
        std::hint::black_box(finder.search(&query[..query.len() - 1], 50));
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    first_key.sort_by(f64::total_cmp);

    // A watcher batch (M2-02): ten files removed, ten added.
    let delta = IndexDelta {
        removed: (0..10)
            .filter_map(|i| finder.path(i * 7_919).map(str::to_owned))
            .collect(),
        added: (0..10).map(|i| format!("src/added/new_{i}.rs")).collect(),
    };
    let t = Instant::now();
    finder.apply_delta(&delta);
    let delta_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "finder_files={} finder_build_ms={build_ms:.1} finder_mb={:.2} keystrokes={} \
         key_p50_ms={:.2} key_p95_ms={:.2} key_max_ms={:.2} first_key_max_ms={:.2} delta_ms={delta_ms:.1}",
        finder.len(),
        finder.estimated_memory_bytes() as f64 / (1024.0 * 1024.0),
        times.len(),
        percentile(&times, 0.5),
        percentile(&times, 0.95),
        times[times.len() - 1],
        first_key[first_key.len() - 1],
    );
    Ok(())
}
