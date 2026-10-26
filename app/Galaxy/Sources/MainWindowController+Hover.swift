import AppKit
import os

extension MainWindowController {
    /// Cancels a pending hover as well as hiding its card. Without clearing
    /// the request position, the polling loop can immediately reopen it.
    func dismissHover() {
        hoverPopover.isHidden = true
        hoverDocument = nil
        hoverRequestPosition = nil
        hoverShownRange = nil
        hoverShownContents = nil
        hoverShownPosition = nil
    }

    func beginHover(at position: TextPos, point: NSPoint) {
        if handleBlameHover(point: point) { return }
        guard position.line != UInt64.max, let document = activeTab?.doc else {
            dismissHover()
            return
        }
        if let diagnostic = document.diagnosticAt(line: position.line, character: position.colUtf16) {
            showDiagnosticHover(diagnostic, at: position)
            return
        }
        // Same spot: skip unless the last answer was empty (the server may
        // still be indexing), then retry at most once a second.
        if hoverDocument === document, hoverRequestPosition?.line == position.line,
           hoverRequestPosition?.colUtf16 == position.colUtf16,
           !hoverPopover.isHidden || Date().timeIntervalSince(hoverRequestTime) < 1 { return }
        // Leaving the hovered word hides the card; moving within it keeps it.
        if !hoverPopover.isHidden, !(hoverShownRange.map { range in
            position.line == range.line && position.colUtf16 >= range.start && position.colUtf16 <= range.end
        } ?? false) {
            hoverPopover.isHidden = true
        }
        hoverRequestTime = Date()
        hoverDocument = document
        hoverPosition = position
        hoverPoint = point
        hoverRequestPosition = position
        // Only remember the position once a request actually went out, so
        // hovering the same spot again retries after the server is ready.
        if !bridge.requestHover(document: document, line: position.line, character: position.colUtf16) {
            hoverRequestPosition = nil
        }
    }

    private func showDiagnosticHover(_ diagnostic: DiagnosticInfo, at position: TextPos) {
        let severity = switch diagnostic.severity {
        case 1: "Error"
        case 2: "Warning"
        case 3: "Information"
        case 4: "Hint"
        default: "Diagnostic"
        }
        let contents = "**\(severity)**\n\n\(diagnostic.message)"
        if hoverShownContents != contents || hoverPopover.isHidden {
            hoverPopover.show(contents)
            hoverPopover.frame = hoverFrame(for: HoverInfo(
                contents: contents,
                startLine: diagnostic.startLine,
                startCharacter: diagnostic.startCharacter,
                endLine: diagnostic.endLine,
                endCharacter: diagnostic.endCharacter
            ), at: position)
            hoverPopover.isHidden = false
            hoverPopover.needsLayout = true
        }
        hoverDocument = nil
        hoverRequestPosition = nil
        hoverShownContents = contents
        hoverShownPosition = position
        hoverShownRange = (diagnostic.startLine, diagnostic.startCharacter, diagnostic.endCharacter)
    }

    func refreshHover() {
        guard let document = hoverDocument, let position = hoverRequestPosition,
              let hover = bridge.hover(document: document),
              hover.startLine == nil || (position.line >= (hover.startLine ?? 0) && position.line <= (hover.endLine ?? position.line)) else { return }
        // The poll timer calls this every 100 ms; only re-layout on new content.
        if !hoverPopover.isHidden, hoverShownContents == hover.contents,
           hoverShownPosition?.line == position.line, hoverShownPosition?.colUtf16 == position.colUtf16 { return }
        hoverShownContents = hover.contents
        hoverShownPosition = position
        if let line = hover.startLine, hover.endLine == line,
           let start = hover.startCharacter, let end = hover.endCharacter {
            hoverShownRange = (line, start, end)
        } else {
            hoverShownRange = (position.line, position.colUtf16, position.colUtf16)
        }
        hoverPopover.show(hover.contents)
        hoverPopover.frame = hoverFrame(for: hover, at: position)
        hoverPopover.isHidden = false
        hoverPopover.needsLayout = true
    }

    /// Places the card above the hovered word (VS Code default), or below,
    /// or on whichever side of the line has more room, clamped to the
    /// visible editor so it never spills off the window edges.
    private func hoverFrame(for hover: HoverInfo, at position: TextPos) -> NSRect {
        let editor = editorView
        let line = position.line
        let text = activeTab?.doc.displayLine(line: line) ?? ""
        let column = hover.startLine == line ? (hover.startCharacter ?? position.colUtf16) : position.colUtf16
        _ = text
        let wordRect = editor.rect(forLine: line, column: column)
        let anchor = detailView.convert(wordRect, from: editor)
        let visible = detailView.convert(editor.visibleRect, from: editor).insetBy(dx: 8, dy: 4)
        let size = hoverPopover.fittingSize(maxHeight: min(HoverPopover.maxHeight, max(60, visible.height / 2)))
        return HoverPopover.placement(size: size, anchor: anchor, in: visible)
    }
}
