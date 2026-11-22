import Foundation

/// Line correspondence between the committed text (left) and the working text
/// (right) of a diff tab. Lines are 0-based. Built from the changed regions, so
/// both panes can scroll together and tint exactly the lines that differ.
struct DiffLineMap: Equatable {
    struct TextRange: Equatable {
        var start: Int
        var end: Int
    }

    struct InlineChanges: Equatable {
        var old: [UInt64: TextRange] = [:]
        var new: [UInt64: TextRange] = [:]
    }

    /// UTF-16 columns for the portion that differs on each paired line.
    /// Unpaired lines are entirely new or removed. Run off the UI thread.
    func inlineChanges(oldText: String, newText: String) -> InlineChanges {
        let oldLines = oldText.split(separator: "\n", omittingEmptySubsequences: false)
        let newLines = newText.split(separator: "\n", omittingEmptySubsequences: false)
        var result = InlineChanges()
        for hunk in hunks {
            for offset in 0..<max(hunk.oldLen, hunk.newLen) {
                let oldIndex = hunk.oldStart + offset
                let newIndex = hunk.newStart + offset
                let hasOld = offset < hunk.oldLen && oldLines.indices.contains(oldIndex)
                let hasNew = offset < hunk.newLen && newLines.indices.contains(newIndex)
                let old = hasOld ? String(oldLines[oldIndex]) : ""
                let new = hasNew ? String(newLines[newIndex]) : ""
                let oldChars = Array(old)
                let newChars = Array(new)
                var prefix = 0
                while prefix < min(oldChars.count, newChars.count), oldChars[prefix] == newChars[prefix] {
                    prefix += 1
                }
                var suffix = 0
                while suffix < min(oldChars.count, newChars.count) - prefix,
                      oldChars[oldChars.count - 1 - suffix] == newChars[newChars.count - 1 - suffix] {
                    suffix += 1
                }
                let start = oldChars.prefix(prefix).reduce(0) { $0 + $1.utf16.count }
                if hasOld {
                    let end = old.utf16.count - oldChars.suffix(suffix).reduce(0) { $0 + $1.utf16.count }
                    if end > start { result.old[UInt64(oldIndex)] = TextRange(start: start, end: end) }
                }
                if hasNew {
                    let end = new.utf16.count - newChars.suffix(suffix).reduce(0) { $0 + $1.utf16.count }
                    if end > start { result.new[UInt64(newIndex)] = TextRange(start: start, end: end) }
                }
            }
        }
        return result
    }

    struct Spacer: Equatable {
        /// Insert virtual rows immediately before this document line.
        var beforeLine: Int
        var rows: Int
    }

    struct Hunk: Equatable {
        var oldStart: Int, oldLen: Int, newStart: Int, newLen: Int
    }

    let hunks: [Hunk]

    init(hunks: [Hunk] = []) { self.hunks = hunks }

    init(_ infos: [GitDiffHunkInfo]) {
        hunks = infos.map {
            Hunk(oldStart: Int($0.oldStart), oldLen: Int($0.oldLen), newStart: Int($0.newStart), newLen: Int($0.newLen))
        }
    }

    /// Committed line that sits beside working line `line`. Inside a changed
    /// region that is the region's first committed line.
    func oldLine(forNew line: Int) -> Int {
        var shift = 0
        for hunk in hunks {
            if line < hunk.newStart { break }
            if line < hunk.newStart + hunk.newLen { return hunk.oldStart }
            shift += hunk.oldLen - hunk.newLen
        }
        return max(0, line + shift)
    }

    /// Working line that sits beside committed line `line`.
    func newLine(forOld line: Int) -> Int {
        var shift = 0
        for hunk in hunks {
            if line < hunk.oldStart { break }
            if line < hunk.oldStart + hunk.oldLen { return hunk.newStart }
            shift += hunk.newLen - hunk.oldLen
        }
        return max(0, line + shift)
    }

    /// Committed lines that were removed or replaced.
    var oldChangedLines: [UInt64] {
        hunks.flatMap { hunk in (hunk.oldStart..<hunk.oldStart + hunk.oldLen).map(UInt64.init) }
    }

    /// Working lines that were added or replaced.
    var newChangedLines: [UInt64] {
        hunks.flatMap { hunk in (hunk.newStart..<hunk.newStart + hunk.newLen).map(UInt64.init) }
    }

    /// Virtual rows that align each changed region with the longer side.
    var oldSpacers: [Spacer] {
        hunks.compactMap { hunk in
            let rows = hunk.newLen - hunk.oldLen
            return rows > 0 ? Spacer(beforeLine: hunk.oldStart + hunk.oldLen, rows: rows) : nil
        }
    }

    var newSpacers: [Spacer] {
        hunks.compactMap { hunk in
            let rows = hunk.oldLen - hunk.newLen
            return rows > 0 ? Spacer(beforeLine: hunk.newStart + hunk.newLen, rows: rows) : nil
        }
    }

    func visualRow(forOld line: Int) -> Int {
        line + oldSpacers.filter { $0.beforeLine <= line }.reduce(0) { $0 + $1.rows }
    }

    func visualRow(forNew line: Int) -> Int {
        line + newSpacers.filter { $0.beforeLine <= line }.reduce(0) { $0 + $1.rows }
    }
}
