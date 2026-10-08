//! Generates benchmark fixtures (M0-08).
//!
//! Usage: `cargo run -p rcode-bench --release --bin gen-fixtures -- [out_dir] [--large]`
//!
//! Default output: `fixtures/`. `--large` also creates the 100 MB file and
//! the 100k-file repository, which take noticeably longer and more disk.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> std::io::Result<()> {
    let mut out = PathBuf::from("fixtures");
    let mut large = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--large" => large = true,
            "-h" | "--help" => {
                println!("gen-fixtures [out_dir] [--large]");
                return Ok(());
            }
            other => out = PathBuf::from(other),
        }
    }
    fs::create_dir_all(&out)?;

    step("file_1mb.rs", || {
        write_file(&out.join("file_1mb.rs"), 1 << 20)
    })?;
    step("lines_50k.rs", || {
        fs::write(
            out.join("lines_50k.rs"),
            rcode_bench::source_lines(50_000, 2),
        )
    })?;
    step("repo_10k/", || {
        rcode_bench::write_repo(&out.join("repo_10k"), 10_000, 3)
    })?;

    if large {
        step("file_100mb.rs", || {
            write_file(&out.join("file_100mb.rs"), 100 << 20)
        })?;
        step("repo_100k/", || {
            rcode_bench::write_repo(&out.join("repo_100k"), 100_000, 4)
        })?;
    }

    println!("fixtures written to {}", out.display());
    Ok(())
}

fn write_file(path: &std::path::Path, bytes: usize) -> std::io::Result<()> {
    let mut f = BufWriter::new(fs::File::create(path)?);
    rcode_bench::write_source(&mut f, bytes, 1)?;
    f.flush()
}

fn step(name: &str, f: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<()> {
    let t = Instant::now();
    f()?;
    println!("{name:<16} {:>8.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}
