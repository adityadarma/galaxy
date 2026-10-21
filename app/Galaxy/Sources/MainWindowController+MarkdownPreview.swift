import AppKit
import os

extension MainWindowController {
    /// Shows or hides the rendered Markdown beside the editor (VS Code
    /// "Open Preview to the Side"). Only Markdown files can be previewed.
    @objc func toggleMarkdownPreview(_ sender: Any?) {
        guard isMarkdownPreviewOn || canPreviewMarkdown else { return NSSound.beep() }
        isMarkdownPreviewOn.toggle()
        updateMarkdownPreview()
    }

    var canPreviewMarkdown: Bool {
        guard let tab = activeTab, tab.isDocument else { return false }
        return MarkdownPreviewView.isMarkdown(path: tab.doc.path())
    }

    /// Brings the preview pane in line with the active tab: shown for a
    /// Markdown file while the preview is on, hidden otherwise. Takes the
    /// secondary pane's place, and gives it back when it closes.
    func updateMarkdownPreview() {
        defer { tabBar.setPreviewButton(visible: canPreviewMarkdown, active: isMarkdownPreviewShown) }
        let wanted = isMarkdownPreviewOn && !isDiffPaneShown && canPreviewMarkdown
        if wanted, let tab = activeTab {
            if !isMarkdownPreviewShown {
                secondaryScrollView.removeFromSuperview()
                editorSplitView.isVertical = true
                editorSplitView.addSubview(markdownPreview)
                editorSplitView.layoutSubtreeIfNeeded()
                if markdownPreview.frame.width < 1 {
                    editorSplitView.setPosition(editorSplitView.bounds.width / 2, ofDividerAt: 0)
                }
                editorSplitView.adjustSubviews()
            }
            markdownPreview.show(markdown: tab.doc.text())
            syncPreviewToEditor()
        } else if isMarkdownPreviewShown {
            markdownPreview.removeFromSuperview()
            if isDiffPaneShown {
                editorSplitView.isVertical = true
            } else {
                editorSplitView.isVertical = true
            }
            editorSplitView.adjustSubviews()
        }
    }

    /// Moves the Markdown preview to the source line at the top of the editor.
    func syncPreviewToEditor() {
        guard isMarkdownPreviewShown, !isSyncingPreviewScroll else { return }
        guard editorView.metrics.lineHeight > 0 else { return }
        let y = max(0, scrollView.contentView.bounds.origin.y + scrollView.contentInsets.top)
        markdownPreview.scroll(toSourceLine: editorView.fractionalLine(atY: y))
    }

    /// Moves the editor to `line`, the source line at the top of the preview.
    func syncEditorToPreview(line: Double) {
        let lineHeight = editorView.metrics.lineHeight
        guard isMarkdownPreviewShown, lineHeight > 0 else { return }
        let clip = scrollView.contentView
        let wanted = NSPoint(x: clip.bounds.origin.x, y: editorView.y(forFractionalLine: line) - scrollView.contentInsets.top)
        let target = clip.constrainBoundsRect(NSRect(origin: wanted, size: clip.bounds.size)).origin
        isSyncingPreviewScroll = true
        defer { isSyncingPreviewScroll = false }
        clip.scroll(to: target)
        scrollView.reflectScrolledClipView(clip)
    }
}
