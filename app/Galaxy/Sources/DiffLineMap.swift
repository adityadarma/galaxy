import Foundation

/// Line correspondence between the committed text (left) and the working text
/// (right) of a diff tab. Lines are 0-based. Built from the changed regions, so
/// both panes can scroll together and tint exactly the lines that differ.
struct DiffLineMap: Equatable {
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
