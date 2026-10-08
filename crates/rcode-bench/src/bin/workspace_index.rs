//! Measures the full, gitignore-aware workspace index (M2-01).
//!
//! Usage: `cargo run -p rcode-bench --release --bin workspace-index -- PATH`

use std::path::PathBuf;
use std::time::Instant;

use rcode_core::workspace::Workspace;

fn main() -> std::io::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: workspace-index PATH",
            )
        })?;
    let workspace = Workspace::open(path)?;
    let start = Instant::now();
    let index = workspace.build_file_index();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
    let memory_mb = index.estimated_memory_bytes() as f64 / (1024.0 * 1024.0);
    println!(
        "workspace_files={} workspace_index_ms={elapsed_ms:.2} workspace_index_estimated_mb={memory_mb:.2}",
        index.len()
    );
    Ok(())
}
