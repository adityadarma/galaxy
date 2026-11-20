//! Go to File (M2-07): fuzzy search over the workspace's files.
//!
//! Paths are stored back to back in one `String` with end offsets, about
//! 60 bytes per file and no per-path allocation. Each keystroke scores the
//! candidates with `nucleo-matcher` (the matcher used by Helix), in
//! parallel for large workspaces, then adds two bonuses:
//!
//! - **recency**: files opened recently rank higher;
//! - **proximity**: files near the current file rank higher.
//!
//! A match inside the file name also beats the same match spread over
//! folder names. Typing more characters only narrows the result, so a
//! query that extends the previous one rescans just the previous matches.

use std::collections::{HashMap, HashSet};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;

use crate::workspace::{IndexDelta, WorkspaceIndex};

/// Below this many candidates one thread is faster than spawning several.
const PARALLEL_THRESHOLD: usize = 8_192;
/// Most threads used per search.
const MAX_THREADS: usize = 2;
/// Recent files remembered for ranking.
pub const MAX_RECENT: usize = 50;

/// One result, ready for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    /// Relative, `/`-separated path.
    pub path: String,
    /// UTF-16 offset where the file name starts in `path`.
    pub name_start_utf16: usize,
    /// Matched characters as `(start, length)` UTF-16 ranges in `path`,
    /// sorted and merged.
    pub highlights: Vec<(usize, usize)>,
    /// Ranking score, higher first. Only meaningful within one search.
    pub score: u32,
    pub is_recent: bool,
}

pub struct FileFinder {
    text: String,
    /// End offset (exclusive) of each path in `text`.
    ends: Vec<u32>,
    /// `0..len`, kept to avoid rebuilding it for every full scan.
    all: Vec<u32>,
    /// One per thread; each preallocates its scoring matrix.
    matchers: Vec<Matcher>,
    /// Path index to recency rank (0 = most recent).
    recent: HashMap<u32, u32>,
    /// Folders of the current file, e.g. `["src", "core"]`.
    current_dir: Vec<String>,
    /// The previous query and every path it matched, for narrowing.
    last: Option<(String, Vec<u32>)>,
}

impl FileFinder {
    pub fn from_index(index: &WorkspaceIndex) -> Self {
        Self::from_paths((0..index.len()).filter_map(|i| index.path(i)))
    }

    pub fn from_paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut text = String::new();
        let mut ends = Vec::new();
        for path in paths {
            let path = path.as_ref();
            // Offsets are u32; a 4 GB path list is not a real workspace.
            if text.len() + path.len() > u32::MAX as usize {
                break;
            }
            text.push_str(path);
            ends.push(text.len() as u32);
        }
        text.shrink_to_fit();
        ends.shrink_to_fit();
        let all: Vec<u32> = (0..ends.len() as u32).collect();
        let threads = if ends.len() < PARALLEL_THRESHOLD {
            1
        } else {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .clamp(1, MAX_THREADS)
        };
        let config = Config::DEFAULT.match_paths();
        Self {
            text,
            ends,
            all,
            matchers: (0..threads).map(|_| Matcher::new(config.clone())).collect(),
            recent: HashMap::new(),
            current_dir: Vec::new(),
            last: None,
        }
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Applies a watcher delta (M2-02): drops `removed` paths and
    /// everything under them, then adds `added`. Ranking context (current
    /// file, recent files) is kept. Returns `false` when nothing changed.
    /// Rebuilds the packed path list: about 20 ms for 100k files, so call
    /// it off the main thread.
    pub fn apply_delta(&mut self, delta: &IndexDelta) -> bool {
        let removed: HashSet<&str> = delta.removed.iter().map(String::as_str).collect();
        let is_removed = |path: &str| {
            removed.contains(path)
                || path
                    .match_indices('/')
                    .any(|(i, _)| removed.contains(&path[..i]))
        };
        let mut kept: Vec<&str> = Vec::with_capacity(self.ends.len() + delta.added.len());
        let mut dropped = 0usize;
        for i in 0..self.ends.len() as u32 {
            let path = path_at(&self.text, &self.ends, i);
            if is_removed(path) {
                dropped += 1;
            } else {
                kept.push(path);
            }
        }
        let present: HashSet<&str> = kept.iter().copied().collect();
        let new: Vec<&str> = delta
            .added
            .iter()
            .map(String::as_str)
            .filter(|p| !present.contains(p))
            .collect();
        if dropped == 0 && new.is_empty() {
            return false;
        }

        // Recent ranks are by index; carry them over by path.
        let mut recent: Vec<(u32, String)> = self
            .recent
            .iter()
            .map(|(&i, &rank)| (rank, path_at(&self.text, &self.ends, i).to_owned()))
            .collect();
        recent.sort_unstable();
        let current_dir = std::mem::take(&mut self.current_dir);

        let mut next = Self::from_paths(kept.into_iter().chain(new));
        next.current_dir = current_dir;
        let wanted: HashMap<&str, u32> = recent.iter().map(|(r, p)| (p.as_str(), *r)).collect();
        if !wanted.is_empty() {
            for i in 0..next.ends.len() as u32 {
                if let Some(&rank) = wanted.get(path_at(&next.text, &next.ends, i)) {
                    next.recent.insert(i, rank);
                }
            }
        }
        *self = next;
        true
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn path(&self, index: usize) -> Option<&str> {
        (index < self.ends.len()).then(|| path_at(&self.text, &self.ends, index as u32))
    }

    /// Bytes held by the finder itself (paths, offsets, matrices excluded).
    pub fn estimated_memory_bytes(&self) -> usize {
        self.text.capacity() + (self.ends.capacity() + self.all.capacity()) * 4
    }

    /// Sets what ranking favors: `current` is the open file, `recent` the
    /// recently opened files, most recent first. Paths not in the
    /// workspace are ignored.
    pub fn set_context(&mut self, current: Option<&str>, recent: &[String]) {
        self.current_dir = current
            .and_then(|p| p.rsplit_once('/'))
            .map(|(dir, _)| dir.split('/').map(str::to_owned).collect())
            .unwrap_or_default();
        let wanted: HashMap<&str, u32> = recent
            .iter()
            .take(MAX_RECENT)
            .enumerate()
            .rev() // the first occurrence wins
            .map(|(rank, p)| (p.as_str(), rank as u32))
            .collect();
        self.recent.clear();
        if wanted.is_empty() {
            return;
        }
        for i in 0..self.ends.len() as u32 {
            if let Some(&rank) = wanted.get(path_at(&self.text, &self.ends, i)) {
                self.recent.insert(i, rank);
            }
        }
    }

    /// Best `limit` files for `query`. An empty query lists recent files.
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<FileMatch> {
        let query = query.trim();
        if query.is_empty() {
            self.last = None;
            return self.recent_list(limit);
        }
        let pattern = Pattern::new(
            query,
            CaseMatching::Smart,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        if pattern.atoms.is_empty() {
            self.last = None;
            return self.recent_list(limit);
        }

        // Extending the query can only drop matches (all atoms are fuzzy
        // and must all match). A backslash can turn an escaped space into
        // part of an atom, which breaks that, so it always rescans.
        let narrowed = match &self.last {
            Some((previous, matched))
                if query.starts_with(previous.as_str()) && !query.contains('\\') =>
            {
                Some(matched.clone())
            }
            _ => None,
        };
        let scored = match &narrowed {
            Some(candidates) => self.score(&pattern, candidates),
            None => {
                let all = std::mem::take(&mut self.all);
                let scored = self.score(&pattern, &all);
                self.all = all;
                scored
            }
        };
        self.last = Some((query.to_owned(), scored.iter().map(|&(i, _)| i).collect()));

        let mut ranked: Vec<(u32, u32)> = scored
            .into_iter()
            .map(|(i, base)| (i, self.rank(&pattern, i, base)))
            .collect();
        let order = |a: &(u32, u32), b: &(u32, u32)| {
            b.1.cmp(&a.1).then_with(|| {
                let pa = path_at(&self.text, &self.ends, a.0);
                let pb = path_at(&self.text, &self.ends, b.0);
                pa.len().cmp(&pb.len()).then_with(|| pa.cmp(pb))
            })
        };
        if ranked.len() > limit && limit > 0 {
            ranked.select_nth_unstable_by(limit - 1, order);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(order);
        ranked
            .into_iter()
            .map(|(i, score)| self.describe(i, score, Some(&pattern)))
            .collect()
    }

    fn recent_list(&mut self, limit: usize) -> Vec<FileMatch> {
        let mut recent: Vec<(u32, u32)> = self.recent.iter().map(|(&i, &r)| (i, r)).collect();
        recent.sort_unstable_by_key(|&(_, rank)| rank);
        let mut out: Vec<FileMatch> = recent
            .into_iter()
            .take(limit)
            .map(|(i, _)| self.describe(i, 0, None))
            .collect();
        // Nothing opened yet: something to arrow through.
        if out.is_empty() {
            out = (0..self.ends.len().min(limit) as u32)
                .map(|i| self.describe(i, 0, None))
                .collect();
        }
        out
    }

    /// Base fuzzy score of every candidate that matches.
    fn score(&mut self, pattern: &Pattern, candidates: &[u32]) -> Vec<(u32, u32)> {
        let text = self.text.as_str();
        let ends = self.ends.as_slice();
        let run = |part: &[u32], matcher: &mut Matcher| -> Vec<(u32, u32)> {
            let mut buf = Vec::new();
            part.iter()
                .filter_map(|&i| {
                    let path = path_at(text, ends, i);
                    pattern
                        .score(Utf32Str::new(path, &mut buf), matcher)
                        .map(|s| (i, s))
                })
                .collect()
        };
        if candidates.len() < PARALLEL_THRESHOLD || self.matchers.len() == 1 {
            return run(candidates, &mut self.matchers[0]);
        }
        let chunk = candidates.len().div_ceil(self.matchers.len());
        std::thread::scope(|s| {
            let handles: Vec<_> = candidates
                .chunks(chunk)
                .zip(self.matchers.iter_mut())
                .map(|(part, matcher)| s.spawn(move || run(part, matcher)))
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("file finder thread panicked"))
                .collect()
        })
    }

    /// Final score: fuzzy score, plus a file-name match, recency and
    /// closeness to the current file.
    fn rank(&mut self, pattern: &Pattern, i: u32, base: u32) -> u32 {
        let path = path_at(&self.text, &self.ends, i);
        let name = &path[name_start(path)..];
        let mut buf = Vec::new();
        let name_bonus = pattern
            .score(Utf32Str::new(name, &mut buf), &mut self.matchers[0])
            .map_or(0, |s| s / 2);
        let recency_bonus = self
            .recent
            .get(&i)
            .map_or(0, |&rank| 40u32.saturating_sub(rank));
        let shared = path
            .split('/')
            .zip(&self.current_dir)
            .take_while(|(a, b)| a == b)
            .count() as u32;
        let proximity_bonus = (shared * 4).min(16);
        base + name_bonus + recency_bonus + proximity_bonus
    }

    fn describe(&mut self, i: u32, score: u32, pattern: Option<&Pattern>) -> FileMatch {
        let path = path_at(&self.text, &self.ends, i).to_owned();
        let name_start_utf16 = utf16_len(&path[..name_start(&path)]);
        let highlights = pattern
            .map(|p| highlights(&path, p, &mut self.matchers[0]))
            .unwrap_or_default();
        FileMatch {
            path,
            name_start_utf16,
            highlights,
            score,
            is_recent: self.recent.contains_key(&i),
        }
    }
}

fn path_at<'a>(text: &'a str, ends: &[u32], i: u32) -> &'a str {
    let i = i as usize;
    let start = if i == 0 { 0 } else { ends[i - 1] as usize };
    &text[start..ends[i] as usize]
}

/// Byte offset of the file name.
fn name_start(path: &str) -> usize {
    path.rfind('/').map_or(0, |slash| slash + 1)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Matched characters of `path` as merged UTF-16 ranges.
pub(crate) fn highlights(
    path: &str,
    pattern: &Pattern,
    matcher: &mut Matcher,
) -> Vec<(usize, usize)> {
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let haystack = Utf32Str::new(path, &mut buf);
    let ascii_indices = matches!(haystack, Utf32Str::Ascii(_));
    if pattern.indices(haystack, matcher, &mut indices).is_none() {
        return Vec::new();
    }
    indices.sort_unstable();
    indices.dedup();

    // nucleo counts bytes for ASCII haystacks and graphemes otherwise.
    let spans: Vec<(usize, usize)> = if path.is_ascii() {
        indices.iter().map(|&i| (i as usize, 1)).collect()
    } else if ascii_indices {
        // Non-ASCII text whose graphemes all start with an ASCII char
        // (combining marks): indices are byte offsets.
        indices
            .iter()
            .filter(|&&b| path.is_char_boundary(b as usize))
            .filter_map(|&b| {
                let b = b as usize;
                let c = path[b..].chars().next()?;
                Some((utf16_len(&path[..b]), c.len_utf16()))
            })
            .collect()
    } else {
        let wanted: HashSet<u32> = indices.iter().copied().collect();
        let mut out = Vec::new();
        let mut offset = 0;
        for (g, grapheme) in path.graphemes(true).enumerate() {
            let len = utf16_len(grapheme);
            if wanted.contains(&(g as u32)) {
                out.push((offset, len));
            }
            offset += len;
        }
        out
    };

    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, len) in spans {
        match merged.last_mut() {
            Some((s, l)) if *s + *l == start => *l += len,
            _ => merged.push((start, len)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finder(paths: &[&str]) -> FileFinder {
        FileFinder::from_paths(paths.iter().copied())
    }

    fn paths(results: &[FileMatch]) -> Vec<&str> {
        results.iter().map(|m| m.path.as_str()).collect()
    }

    #[test]
    fn apply_delta_adds_and_removes_and_keeps_recent_files() {
        let mut f = finder(&[
            "src/main.rs",
            "src/net/tcp.rs",
            "src/net/udp.rs",
            "README.md",
        ]);
        f.set_context(Some("src/main.rs"), &["README.md".into()]);
        let changed = f.apply_delta(&IndexDelta {
            removed: vec!["src/net".into(), "gone.txt".into()],
            added: vec!["src/lib.rs".into(), "README.md".into()],
        });
        assert!(changed);
        assert_eq!(
            f.len(),
            3,
            "folder removed with its files; README not duplicated"
        );
        assert_eq!(paths(&f.search("lib", 10)), ["src/lib.rs"]);
        assert!(f.search("tcp", 10).is_empty());
        let empty = f.search("", 10);
        assert_eq!(paths(&empty), ["README.md"], "still the recent file");
        assert!(empty[0].is_recent);

        // A prefix that is not a folder boundary does not remove anything.
        assert!(!f.apply_delta(&IndexDelta {
            removed: vec!["src/ma".into()],
            added: vec![],
        }));
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn apply_delta_resets_the_narrowing_cache() {
        let mut f = finder(&["alpha.rs"]);
        assert_eq!(f.search("al", 10).len(), 1);
        f.apply_delta(&IndexDelta {
            removed: vec![],
            added: vec!["alpine.rs".into()],
        });
        assert_eq!(
            f.search("alp", 10).len(),
            2,
            "new file found when typing on"
        );
    }

    #[test]
    fn finds_fuzzy_matches_and_skips_the_rest() {
        let mut f = finder(&["src/main.rs", "src/lib.rs", "README.md", "docs/manual.md"]);
        let found = f.search("mnrs", 10);
        assert_eq!(paths(&found), ["src/main.rs"]);
        assert!(f.search("zzz", 10).is_empty());
    }

    #[test]
    fn file_name_matches_beat_matches_spread_over_folders() {
        let mut f = finder(&["view/port/x.rs", "src/viewport.rs"]);
        assert_eq!(f.search("viewport", 10)[0].path, "src/viewport.rs");
    }

    #[test]
    fn recent_files_rank_higher_and_fill_the_empty_query() {
        let mut f = finder(&["a/config.rs", "b/config.rs", "c/config.rs"]);
        f.set_context(None, &["c/config.rs".into(), "b/config.rs".into()]);
        let found = f.search("config", 10);
        assert_eq!(paths(&found), ["c/config.rs", "b/config.rs", "a/config.rs"]);
        assert!(found[0].is_recent && !found[2].is_recent);
        assert_eq!(paths(&f.search("", 10)), ["c/config.rs", "b/config.rs"]);
        assert_eq!(paths(&f.search("  ", 10)), ["c/config.rs", "b/config.rs"]);
    }

    #[test]
    fn files_near_the_current_one_rank_higher() {
        let mut f = finder(&["a/x/mod.rs", "b/y/mod.rs", "c/z/mod.rs"]);
        f.set_context(Some("b/y/other.rs"), &[]);
        assert_eq!(f.search("mod", 10)[0].path, "b/y/mod.rs");
        f.set_context(Some("c/z/other.rs"), &[]);
        assert_eq!(f.search("mod", 10)[0].path, "c/z/mod.rs");
    }

    #[test]
    fn ties_prefer_shorter_paths_then_alphabetical() {
        let mut f = finder(&["deep/er/main.rs", "b/main.rs", "a/main.rs"]);
        assert_eq!(
            paths(&f.search("main", 10)),
            ["a/main.rs", "b/main.rs", "deep/er/main.rs"]
        );
    }

    #[test]
    fn limit_keeps_the_best() {
        let names: Vec<String> = (0..100).map(|i| format!("f/file_{i:03}.rs")).collect();
        let mut f = FileFinder::from_paths(&names);
        let found = f.search("file", 5);
        assert_eq!(found.len(), 5);
        assert!(found.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn several_words_must_all_match_in_any_order() {
        let mut f = finder(&[
            "src/view/render.rs",
            "src/view/layout.rs",
            "src/model/render.rs",
        ]);
        assert_eq!(paths(&f.search("render view", 10)), ["src/view/render.rs"]);
    }

    #[test]
    fn smart_case() {
        let mut f = finder(&["src/Main.rs", "src/main.rs"]);
        assert_eq!(f.search("main", 10).len(), 2, "lowercase ignores case");
        assert_eq!(
            paths(&f.search("Main", 10)),
            ["src/Main.rs"],
            "uppercase respects it"
        );
    }

    #[test]
    fn narrowing_gives_the_same_results_as_a_fresh_search() {
        let names: Vec<String> = (0..2_000)
            .map(|i| {
                format!(
                    "src/mod_{}/{}_{i}.rs",
                    i % 17,
                    ["buffer", "cursor", "parser"][i % 3]
                )
            })
            .collect();
        let mut typed = FileFinder::from_paths(&names);
        let query = "src mod bufr 12";
        for end in 1..=query.len() {
            let incremental = typed.search(&query[..end], 50);
            let fresh = FileFinder::from_paths(&names).search(&query[..end], 50);
            assert_eq!(incremental, fresh, "query {:?}", &query[..end]);
        }
        // Deleting a character is not an extension: full rescan.
        assert_eq!(
            typed.search("src mod buf", 50),
            FileFinder::from_paths(&names).search("src mod buf", 50)
        );
    }

    #[test]
    fn highlights_are_merged_utf16_ranges() {
        let mut f = finder(&["src/main.rs"]);
        let m = &f.search("main", 10)[0];
        assert_eq!(m.highlights, [(4, 4)]);
        assert_eq!(m.name_start_utf16, 4);

        let mut f = finder(&["docs/résumé/naïve.md"]);
        let m = &f.search("naive", 10)[0];
        let text: Vec<u16> = m.path.encode_utf16().collect();
        let lit: String = m
            .highlights
            .iter()
            .map(|&(s, l)| String::from_utf16(&text[s..s + l]).unwrap())
            .collect();
        assert_eq!(lit, "naïve");
        assert_eq!(m.name_start_utf16, "docs/résumé/".encode_utf16().count());

        // Astral characters take two UTF-16 units.
        let mut f = finder(&["😀/abc.rs"]);
        let m = &f.search("abc", 10)[0];
        assert_eq!(m.highlights, [(3, 3)]);
    }

    #[test]
    fn large_lists_use_threads_and_agree_with_one_thread() {
        let names: Vec<String> = (0..20_000)
            .map(|i| {
                format!(
                    "src/m{}/n{}/{}_{i}.rs",
                    i % 13,
                    i % 7,
                    ["token", "value", "node"][i % 3]
                )
            })
            .collect();
        let mut many = FileFinder::from_paths(&names);
        let mut one = FileFinder::from_paths(&names);
        one.matchers.truncate(1);
        assert_eq!(many.search("m3 tok", 30), one.search("m3 tok", 30));
        assert_eq!(many.search("value 99", 30), one.search("value 99", 30));
    }

    #[test]
    fn builds_from_a_workspace_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let ws = crate::workspace::Workspace::open(dir.path()).unwrap();
        let mut f = FileFinder::from_index(&ws.build_file_index());
        assert_eq!(f.len(), 2);
        assert_eq!(paths(&f.search("main", 10)), ["src/main.rs"]);
        assert!(f.estimated_memory_bytes() >= "src/main.rsREADME.md".len());
    }
}
