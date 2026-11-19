//! On-disk cache of workspace Rust diagnostics. A file is checked again only
//! when its size or modification time changed, or when the project's
//! declarations, imports or Cargo metadata changed (the fingerprint).

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use galaxy_core::lang::Problem;

const SCHEMA: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct CachedProblem {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub message: String,
}

impl From<Problem> for CachedProblem {
    fn from(problem: Problem) -> Self {
        Self {
            start_line: problem.start_line,
            start_column: problem.start_column,
            end_line: problem.end_line,
            end_column: problem.end_column,
            message: problem.message,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct CachedFile {
    pub size: u64,
    pub modified_nanos: u128,
    pub problems: Vec<CachedProblem>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedProject {
    schema: u32,
    root: PathBuf,
    fingerprint: u64,
    files: BTreeMap<PathBuf, CachedFile>,
}

/// Cached files of `root`, or none when the cache belongs to another schema,
/// root or fingerprint.
pub(super) fn load(root: &Path, fingerprint: u64) -> HashMap<PathBuf, CachedFile> {
    let Some(path) = cache_path(root) else {
        return HashMap::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    bincode::deserialize::<CachedProject>(&bytes)
        .ok()
        .filter(|cache| {
            cache.schema == SCHEMA
                && cache.root == canonical(root)
                && cache.fingerprint == fingerprint
        })
        .map(|cache| cache.files.into_iter().collect())
        .unwrap_or_default()
}

pub(super) fn save(
    root: &Path,
    fingerprint: u64,
    files: BTreeMap<PathBuf, CachedFile>,
) -> io::Result<()> {
    let Some(path) = cache_path(root) else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let bytes = bincode::serialize(&CachedProject {
        schema: SCHEMA,
        root: canonical(root),
        fingerprint,
        files,
    })
    .map_err(io::Error::other)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn cache_path(root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical(root).hash(&mut hasher);
    Some(
        PathBuf::from(home)
            .join("Library/Caches/Galaxy/workspace-index")
            .join(format!("{:016x}.rust-diagnostics-v1", hasher.finish())),
    )
}
