//! Deterministic fixture generation shared by benches and the CLI.
//!
//! Content is generated from a fixed-seed PRNG so benchmark inputs are
//! identical across machines and runs.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Small xorshift PRNG; good enough for synthetic source text.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

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

/// Writes Rust-like source text of roughly `target_bytes` to `out`.
pub fn write_source<W: Write>(out: &mut W, target_bytes: usize, seed: u64) -> io::Result<usize> {
    let mut rng = Rng::new(seed);
    let mut written = 0usize;
    let mut fn_id = 0u64;
    while written < target_bytes {
        let name = IDENTS[rng.below(IDENTS.len() as u64) as usize];
        let mut block = format!("pub fn {name}_{fn_id}(input: &str) -> usize {{\n");
        for _ in 0..(3 + rng.below(8)) {
            let a = IDENTS[rng.below(IDENTS.len() as u64) as usize];
            let b = IDENTS[rng.below(IDENTS.len() as u64) as usize];
            let n = rng.below(10_000);
            block.push_str(&format!("    let {a} = {b}.len() + {n}; // compute {a}\n"));
        }
        block.push_str("    input.len()\n}\n\n");
        out.write_all(block.as_bytes())?;
        written += block.len();
        fn_id += 1;
    }
    Ok(written)
}

/// Generates source text in memory.
pub fn source_string(target_bytes: usize, seed: u64) -> String {
    let mut buf = Vec::with_capacity(target_bytes + 256);
    write_source(&mut buf, target_bytes, seed).expect("writing to Vec cannot fail");
    String::from_utf8(buf).expect("generator emits ASCII")
}

/// Generates exactly `lines` lines of source text.
pub fn source_lines(lines: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut s = String::with_capacity(lines * 48);
    for i in 0..lines {
        let a = IDENTS[rng.below(IDENTS.len() as u64) as usize];
        s.push_str(&format!("    let {a}_{i} = {};\n", rng.below(1_000_000)));
    }
    s
}

/// Creates a synthetic repository with `file_count` files spread over nested
/// directories, plus a `.gitignore` and ignored `target/` and `node_modules/`.
pub fn write_repo(root: &Path, file_count: usize, seed: u64) -> io::Result<()> {
    let mut rng = Rng::new(seed);
    fs::create_dir_all(root)?;
    fs::write(root.join(".gitignore"), "target/\nnode_modules/\n")?;

    let exts = ["rs", "ts", "py", "go", "json", "md"];
    for i in 0..file_count {
        let depth = 1 + rng.below(4) as usize;
        let mut dir = root.join("src");
        for d in 0..depth {
            dir = dir.join(format!("mod_{}_{}", d, rng.below(12)));
        }
        fs::create_dir_all(&dir)?;
        let ext = exts[rng.below(exts.len() as u64) as usize];
        let name = IDENTS[rng.below(IDENTS.len() as u64) as usize];
        let path = dir.join(format!("{name}_{i}.{ext}"));
        let mut f = BufWriter::new(fs::File::create(path)?);
        write_source(&mut f, 512 + rng.below(4096) as usize, rng.next_u64())?;
        f.flush()?;
    }

    // Ignored noise that the indexer must skip.
    for ignored in ["target/debug", "node_modules/pkg"] {
        let dir = root.join(ignored);
        fs::create_dir_all(&dir)?;
        for i in 0..(file_count / 10).max(1) {
            fs::write(dir.join(format!("junk_{i}.txt")), "ignored\n")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        assert_eq!(source_string(4096, 7), source_string(4096, 7));
        assert_ne!(source_string(4096, 7), source_string(4096, 8));
    }

    #[test]
    fn source_lines_has_exact_line_count() {
        assert_eq!(source_lines(123, 1).lines().count(), 123);
    }

    #[test]
    fn size_reaches_target() {
        assert!(source_string(10_000, 1).len() >= 10_000);
    }
}
