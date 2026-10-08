//! Rope-backed text document (M1-01).
//!
//! Text is stored in the rope with LF line breaks only. The original line
//! ending and encoding are detected on load and re-applied on save, so the
//! editing code never has to reason about `\r\n` or UTF-16.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ropey::Rope;

/// Line ending style detected on load and applied on save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
    /// Classic Mac OS. Rare, but supported so such files round-trip.
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }
}

/// On-disk text encoding detected on load and applied on save. Whether a
/// byte order mark is written is tracked separately (`Document::has_bom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    pub fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
        }
    }
}

/// Documents above this many bytes open in large-file mode.
pub const LARGE_FILE_BYTES: usize = 20 * 1024 * 1024;

/// A text document backed by a rope.
#[derive(Debug, Clone)]
pub struct Document {
    text: Rope,
    line_ending: LineEnding,
    encoding: Encoding,
    has_bom: bool,
    /// Bumped on every text mutation. Snapshots, syntax trees, and LSP
    /// versions use it to detect stale results.
    revision: u64,
}

impl Document {
    /// Builds a document from an in-memory string. Line breaks are
    /// normalized to LF; the dominant style is remembered for saving.
    pub fn from_text(text: &str) -> Self {
        // Single CR scan decides both detection and normalization; the
        // common LF-only file pays for one memchr pass and nothing else.
        let (line_ending, text) = if text.as_bytes().contains(&b'\r') {
            (
                detect_line_ending(text.as_bytes()),
                Rope::from_str(&normalize_line_breaks(text)),
            )
        } else {
            (LineEnding::Lf, Rope::from_str(text))
        };
        Self {
            text,
            line_ending,
            encoding: Encoding::Utf8,
            has_bom: false,
            revision: 0,
        }
    }

    /// Decodes raw file bytes, detecting encoding (BOM or UTF-16 heuristic)
    /// and line ending. Binary and non-UTF-8/16 input is rejected with
    /// `InvalidData` so it can never be silently corrupted on save.
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let (text, encoding, has_bom) = decode(bytes)?;
        let mut doc = Self::from_text(&text);
        doc.encoding = encoding;
        doc.has_bom = has_bom;
        Ok(doc)
    }

    /// Loads a file from disk.
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::from_bytes(&fs::read(path)?)
    }

    /// Serializes the document with its line ending and encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len_bytes() + 4);
        self.write_to(&mut out).expect("writing to Vec cannot fail");
        out
    }

    /// Streams the encoded document to `out` chunk by chunk, without
    /// materializing the whole text as one string.
    pub fn write_to<W: Write>(&self, out: &mut W) -> io::Result<()> {
        if self.has_bom {
            out.write_all(self.encoding.bom())?;
        }
        let eol = self.line_ending.as_str();
        let mut buf = Vec::new();
        for chunk in self.text.chunks() {
            buf.clear();
            for (i, part) in chunk.split('\n').enumerate() {
                if i > 0 {
                    encode_into(&mut buf, eol, self.encoding);
                }
                encode_into(&mut buf, part, self.encoding);
            }
            out.write_all(&buf)?;
        }
        Ok(())
    }

    /// Saves atomically: write to a temp file in the target's directory,
    /// fsync, then rename over the target so a crash never leaves a
    /// truncated file. Symlinks are followed (the link itself is kept) and
    /// the existing file's permissions are preserved.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let target = resolve_symlink(path)?;
        let dir = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if target.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no file name",
            ));
        }

        // Dropping `tmp` on any error path deletes the temp file.
        let mut tmp = tempfile::Builder::new()
            .prefix(".rcode-")
            .suffix(".tmp")
            .tempfile_in(&dir)?;
        {
            let mut writer = io::BufWriter::new(tmp.as_file_mut());
            self.write_to(&mut writer)?;
            writer.flush()?;
        }
        match fs::metadata(&target) {
            Ok(meta) => tmp.as_file().set_permissions(meta.permissions())?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // tempfile creates 0600; new files get the usual 0644.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    tmp.as_file()
                        .set_permissions(fs::Permissions::from_mode(0o644))?;
                }
            }
            Err(e) => return Err(e),
        }
        tmp.as_file().sync_all()?;
        tmp.persist(&target).map_err(|e| e.error)?;
        // Make the rename itself durable.
        fs::File::open(&dir)?.sync_all()?;
        Ok(())
    }

    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.text.len_bytes()
    }

    /// Large-file mode (PRD 7.1): heavy features stay off above this size.
    /// Measured on the in-memory UTF-8 text, which is close to the file size.
    pub fn is_large(&self) -> bool {
        self.len_bytes() > LARGE_FILE_BYTES
    }

    pub fn line_count(&self) -> usize {
        self.text.len_lines()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Changes the line ending used on the next save.
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Changes the encoding used on the next save. UTF-16 always gets a BOM
    /// so the file stays unambiguous; UTF-8 keeps the current BOM setting.
    pub fn set_encoding(&mut self, encoding: Encoding) {
        self.encoding = encoding;
        if encoding != Encoding::Utf8 {
            self.has_bom = true;
        }
    }

    pub fn has_bom(&self) -> bool {
        self.has_bom
    }

    /// Controls whether a byte order mark is written on save.
    pub fn set_has_bom(&mut self, has_bom: bool) {
        self.has_bom = has_bom;
    }

    /// Returns `count` lines starting at `first` (clamped to the document),
    /// without trailing line breaks. Mirrors the future viewport API.
    pub fn lines(&self, first: usize, count: usize) -> Vec<String> {
        let total = self.line_count();
        let start = first.min(total);
        let end = start.saturating_add(count).min(total);
        (start..end)
            .map(|i| {
                let mut s = self.text.line(i).to_string();
                if s.ends_with('\n') {
                    s.pop();
                }
                s
            })
            .collect()
    }

    /// Inserts text at a char index (clamped). Line breaks in `text` are
    /// normalized to LF to keep the rope invariant.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.revision += 1;
        let idx = char_idx.min(self.text.len_chars());
        if text.contains('\r') {
            self.text.insert(idx, &normalize_line_breaks(text));
        } else {
            self.text.insert(idx, text);
        }
    }

    /// Removes a char range (clamped).
    pub fn remove(&mut self, start: usize, end: usize) {
        let len = self.text.len_chars();
        let (start, end) = (start.min(len), end.min(len));
        if start < end {
            self.revision += 1;
            self.text.remove(start..end);
        }
    }

    /// The in-memory text (LF line breaks).
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    /// Monotonic text revision; changes whenever the text may have changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Read access to the rope for sibling modules (positions, selections).
    pub(crate) fn rope(&self) -> &Rope {
        &self.text
    }

    /// Write access for `change::apply`. Callers must keep the LF-only
    /// invariant (inserted text goes through `normalize_line_breaks`).
    pub(crate) fn rope_mut(&mut self) -> &mut Rope {
        self.revision += 1;
        &mut self.text
    }
}

/// Picks the most frequent line ending. Ties and files without line breaks
/// default to LF.
fn detect_line_ending(bytes: &[u8]) -> LineEnding {
    // Byte counts below auto-vectorize; only CR positions are inspected.
    let lf_total = bytes.iter().filter(|&&b| b == b'\n').count();
    let cr_total = bytes.iter().filter(|&&b| b == b'\r').count();
    let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
    let lf = lf_total - crlf;
    let cr = cr_total - crlf;
    if crlf > lf && crlf >= cr {
        LineEnding::CrLf
    } else if cr > lf && cr > crlf {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    }
}

/// Converts `\r\n` and lone `\r` to `\n`.
pub(crate) fn normalize_line_breaks(text: &str) -> String {
    // Copies the runs between CRs in bulk instead of char by char.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('\r') {
        out.push_str(&rest[..i]);
        out.push('\n');
        rest = &rest[i + 1..];
        if let Some(stripped) = rest.strip_prefix('\n') {
            rest = stripped;
        }
    }
    out.push_str(rest);
    out
}

fn invalid_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Bytes inspected by the binary and BOM-less UTF-16 heuristics.
const SNIFF_LEN: usize = 8 * 1024;

/// Returns `(text, encoding, has_bom)`.
fn decode(bytes: &[u8]) -> io::Result<(String, Encoding, bool)> {
    if let Some(rest) = bytes.strip_prefix(Encoding::Utf8.bom()) {
        return decode_utf8(rest).map(|s| (s, Encoding::Utf8, true));
    }
    if let Some(rest) = bytes.strip_prefix(Encoding::Utf16Le.bom()) {
        return decode_utf16(rest, false).map(|s| (s, Encoding::Utf16Le, true));
    }
    if let Some(rest) = bytes.strip_prefix(Encoding::Utf16Be.bom()) {
        return decode_utf16(rest, true).map(|s| (s, Encoding::Utf16Be, true));
    }
    if let Some(big_endian) = sniff_utf16(bytes) {
        let enc = if big_endian {
            Encoding::Utf16Be
        } else {
            Encoding::Utf16Le
        };
        if let Ok(s) = decode_utf16(bytes, big_endian) {
            return Ok((s, enc, false));
        }
    }
    decode_utf8(bytes).map(|s| (s, Encoding::Utf8, false))
}

fn decode_utf8(bytes: &[u8]) -> io::Result<String> {
    if bytes[..bytes.len().min(SNIFF_LEN)].contains(&0) {
        return Err(invalid_data("binary file"));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| invalid_data("unsupported encoding (expected UTF-8 or UTF-16)"))
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> io::Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(invalid_data("truncated UTF-16 data"));
    }
    let units = bytes.chunks_exact(2).map(|p| {
        if big_endian {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        }
    });
    let text: String = char::decode_utf16(units)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid_data("invalid UTF-16 data"))?;
    if text.contains('\0') {
        return Err(invalid_data("binary file"));
    }
    Ok(text)
}

/// Detects BOM-less UTF-16 by the zero-byte pattern of ASCII-heavy text.
/// Returns `Some(true)` for big-endian, `Some(false)` for little-endian.
fn sniff_utf16(bytes: &[u8]) -> Option<bool> {
    let sample = &bytes[..bytes.len().min(SNIFF_LEN) & !1];
    let pairs = sample.len() / 2;
    if pairs < 2 {
        return None;
    }
    let (mut even_zero, mut odd_zero) = (0usize, 0usize);
    for p in sample.chunks_exact(2) {
        even_zero += usize::from(p[0] == 0);
        odd_zero += usize::from(p[1] == 0);
    }
    // At least 40% of code units look like ASCII (and at least one zero
    // byte exists, so plain UTF-8 never matches), and the other byte
    // position is (almost) never zero.
    let threshold = (pairs * 2 / 5).max(1);
    if odd_zero >= threshold && even_zero * 20 <= pairs {
        Some(false)
    } else if even_zero >= threshold && odd_zero * 20 <= pairs {
        Some(true)
    } else {
        None
    }
}

fn encode_into(buf: &mut Vec<u8>, s: &str, encoding: Encoding) {
    match encoding {
        Encoding::Utf8 => buf.extend_from_slice(s.as_bytes()),
        Encoding::Utf16Le => s.encode_utf16().for_each(|u| buf.extend(u.to_le_bytes())),
        Encoding::Utf16Be => s.encode_utf16().for_each(|u| buf.extend(u.to_be_bytes())),
    }
}

/// Follows a chain of symlinks so saving replaces the link target, not the
/// link. A missing final target is fine (new file).
fn resolve_symlink(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&current)?;
                current = match current.parent() {
                    Some(parent) if link.is_relative() => parent.join(link),
                    _ => link,
                };
            }
            Ok(_) => return Ok(current),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str, big_endian: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend_from_slice(if big_endian {
                &[0xFE, 0xFF]
            } else {
                &[0xFF, 0xFE]
            });
        }
        for u in s.encode_utf16() {
            out.extend(if big_endian {
                u.to_be_bytes()
            } else {
                u.to_le_bytes()
            });
        }
        out
    }

    #[test]
    fn detects_dominant_line_ending() {
        assert_eq!(Document::from_text("a\nb").line_ending(), LineEnding::Lf);
        assert_eq!(
            Document::from_text("a\r\nb").line_ending(),
            LineEnding::CrLf
        );
        assert_eq!(Document::from_text("a\rb\rc").line_ending(), LineEnding::Cr);
        assert_eq!(Document::from_text("").line_ending(), LineEnding::Lf);
        assert_eq!(
            Document::from_text("a\r\nb\r\nc\nd").line_ending(),
            LineEnding::CrLf
        );
        // Tie defaults to LF.
        assert_eq!(
            Document::from_text("a\r\nb\nc").line_ending(),
            LineEnding::Lf
        );
    }

    #[test]
    fn line_breaks_are_normalized_in_memory() {
        let doc = Document::from_text("one\r\ntwo\rthree\nfour");
        assert_eq!(doc.text(), "one\ntwo\nthree\nfour");
        assert_eq!(doc.line_count(), 4);
        assert_eq!(doc.lines(0, 4), vec!["one", "two", "three", "four"]);
    }

    #[test]
    fn lines_are_clamped() {
        let doc = Document::from_text("one\r\ntwo\nthree");
        assert_eq!(doc.lines(1, 10), vec!["two", "three"]);
        assert!(doc.lines(10, 5).is_empty());
    }

    #[test]
    fn insert_and_remove_handle_unicode() {
        let mut doc = Document::from_text("héllo");
        doc.insert(5, " 世界");
        assert_eq!(doc.text(), "héllo 世界");
        doc.remove(0, 2);
        assert_eq!(doc.text(), "llo 世界");
        doc.remove(100, 200); // out of range is a no-op
        assert_eq!(doc.len_chars(), 6);
    }

    #[test]
    fn insert_normalizes_pasted_crlf() {
        let mut doc = Document::from_text("ab");
        doc.insert(1, "x\r\ny");
        assert_eq!(doc.text(), "ax\nyb");
    }

    #[test]
    fn crlf_round_trips_byte_exact() {
        let src = b"fn main() {\r\n    println!(\"hi\");\r\n}\r\n";
        let doc = Document::from_bytes(src).unwrap();
        assert_eq!(doc.line_ending(), LineEnding::CrLf);
        assert_eq!(doc.to_bytes(), src);
    }

    #[test]
    fn utf8_bom_round_trips() {
        let src = b"\xEF\xBB\xBFhello\n";
        let doc = Document::from_bytes(src).unwrap();
        assert_eq!(doc.encoding(), Encoding::Utf8);
        assert!(doc.has_bom());
        assert_eq!(doc.text(), "hello\n");
        assert_eq!(doc.to_bytes(), src);
    }

    #[test]
    fn utf16_with_bom_round_trips() {
        for big_endian in [false, true] {
            let src = utf16("héllo 世界 🦀\r\nline2", big_endian, true);
            let doc = Document::from_bytes(&src).unwrap();
            let expected = if big_endian {
                Encoding::Utf16Be
            } else {
                Encoding::Utf16Le
            };
            assert_eq!(doc.encoding(), expected);
            assert_eq!(doc.line_ending(), LineEnding::CrLf);
            assert_eq!(doc.text(), "héllo 世界 🦀\nline2");
            assert_eq!(doc.to_bytes(), src);
        }
    }

    #[test]
    fn utf16_without_bom_is_sniffed() {
        for big_endian in [false, true] {
            let src = utf16("plain ascii text\nsecond line\n", big_endian, false);
            let doc = Document::from_bytes(&src).unwrap();
            let expected = if big_endian {
                Encoding::Utf16Be
            } else {
                Encoding::Utf16Le
            };
            assert_eq!(doc.encoding(), expected);
            assert_eq!(doc.line_count(), 3);
            assert_eq!(doc.to_bytes(), src);
        }
    }

    #[test]
    fn rejects_binary_and_unknown_encodings() {
        let binary = [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00, 0x00, 0x00];
        let err = Document::from_bytes(&binary).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // Latin-1 "café" is not valid UTF-8.
        let err = Document::from_bytes(b"caf\xe9").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // Unpaired surrogate.
        let err = Document::from_bytes(&[0xFF, 0xFE, 0x00, 0xD8]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_file_is_utf8_lf() {
        let doc = Document::from_bytes(b"").unwrap();
        assert_eq!(doc.encoding(), Encoding::Utf8);
        assert_eq!(doc.line_ending(), LineEnding::Lf);
        assert_eq!(doc.line_count(), 1);
    }

    #[test]
    fn changing_line_ending_and_encoding_applies_on_save() {
        let mut doc = Document::from_text("a\nb\n");
        doc.set_line_ending(LineEnding::CrLf);
        doc.set_encoding(Encoding::Utf16Le);
        assert_eq!(doc.to_bytes(), utf16("a\r\nb\r\n", false, true));
    }

    #[test]
    fn write_to_handles_rope_chunk_boundaries() {
        // Large enough to span many rope chunks.
        let src: String = (0..20_000).map(|i| format!("line {i} ü\r\n")).collect();
        let doc = Document::from_text(&src);
        assert_eq!(doc.to_bytes(), src.as_bytes());
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "old").unwrap();

        let doc = Document::from_text("new content\r\n");
        doc.save(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new content\r\n");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "file.txt")
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        assert_eq!(Document::load(&path).unwrap().line_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        Document::from_text("#!/bin/sh\necho hi\n")
            .save(&path)
            .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);

        let new_path = dir.path().join("new.txt");
        Document::from_text("x").save(&new_path).unwrap();
        let mode = fs::metadata(&new_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn save_through_symlink_keeps_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.txt", &link).unwrap();

        Document::from_text("new").save(&link).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn save_to_missing_directory_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope/file.txt");
        let err = Document::from_text("x").save(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn load_missing_file_errors() {
        let err = Document::load(Path::new("/nonexistent/file")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
