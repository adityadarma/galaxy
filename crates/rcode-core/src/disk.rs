//! What a document last saw on disk (M2-03).
//!
//! A [`DiskStamp`] is taken when a file is loaded or saved: size,
//! modification time, and a hash of the bytes. [`DiskStamp::check`] compares
//! the file now with it. Size and time are checked first, so an unchanged
//! file costs one `stat`; when they differ the bytes are hashed, so a
//! `touch` or a checkout that writes the same content is not a change.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStamp {
    len: u64,
    modified: Option<SystemTime>,
    hash: u64,
}

/// The file compared with its stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskCheck {
    Unchanged,
    /// Different content; the bytes read, so the caller need not read again.
    Changed(Vec<u8>),
    Deleted,
}

impl DiskStamp {
    /// Stamp for `bytes`, just read from or written to `path`.
    pub fn of(path: &Path, bytes: &[u8]) -> io::Result<Self> {
        let mut h = DefaultHasher::new();
        h.write(bytes);
        Self::with_hash(path, bytes.len() as u64, h.finish())
    }

    fn with_hash(path: &Path, len: u64, hash: u64) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self {
            len,
            modified: meta.modified().ok(),
            hash,
        })
    }

    /// Reads `path` and stamps it.
    pub fn read(path: &Path) -> io::Result<(Self, Vec<u8>)> {
        let bytes = fs::read(path)?;
        Ok((Self::of(path, &bytes)?, bytes))
    }

    /// Compares the file now with this stamp. When only the time changed,
    /// the stamp takes the new time so the next check is a `stat` again.
    pub fn check(&mut self, path: &Path) -> io::Result<DiskCheck> {
        let meta = match fs::metadata(path) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Ok(DiskCheck::Deleted),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DiskCheck::Deleted),
            Err(e) => return Err(e),
        };
        let modified = meta.modified().ok();
        if meta.len() == self.len && modified == self.modified {
            return Ok(DiskCheck::Unchanged);
        }
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DiskCheck::Deleted),
            Err(e) => return Err(e),
        };
        let now = Self::of(path, &bytes)?;
        if now.hash == self.hash && now.len == self.len {
            self.modified = now.modified;
            return Ok(DiskCheck::Unchanged);
        }
        Ok(DiskCheck::Changed(bytes))
    }
}

/// Counts and hashes what passes through, to stamp a save without reading
/// the file back.
pub(crate) struct HashingWriter<W> {
    inner: W,
    hasher: DefaultHasher,
    len: u64,
}

impl<W: Write> HashingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: DefaultHasher::new(),
            len: 0,
        }
    }

    /// `(len, hash)` of everything written.
    pub(crate) fn finish(self) -> (W, u64, u64) {
        (self.inner, self.len, self.hasher.finish())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.write(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl DiskStamp {
    pub(crate) fn from_written(path: &Path, len: u64, hash: u64) -> io::Result<Self> {
        Self::with_hash(path, len, hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_with_a_new_time_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, "one").unwrap();
        let (mut stamp, _) = DiskStamp::read(&p).unwrap();
        assert_eq!(stamp.check(&p).unwrap(), DiskCheck::Unchanged);

        // Rewritten with the same content (touch, checkout): not a change.
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        fs::write(&p, "one").unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(stamp.check(&p).unwrap(), DiskCheck::Unchanged);
        assert_eq!(stamp.modified, Some(later), "time taken over");

        fs::write(&p, "two").unwrap();
        assert_eq!(
            stamp.check(&p).unwrap(),
            DiskCheck::Changed(b"two".to_vec())
        );
        fs::remove_file(&p).unwrap();
        assert_eq!(stamp.check(&p).unwrap(), DiskCheck::Deleted);
    }

    #[test]
    fn hashing_writer_matches_the_stamp_of_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        let mut w = HashingWriter::new(Vec::new());
        w.write_all(b"hello ").unwrap();
        w.write_all(b"world").unwrap();
        let (bytes, len, hash) = w.finish();
        fs::write(&p, &bytes).unwrap();
        assert_eq!(
            DiskStamp::from_written(&p, len, hash).unwrap(),
            DiskStamp::of(&p, b"hello world").unwrap()
        );
    }
}
