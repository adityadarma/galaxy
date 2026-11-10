import AppKit

extension MainWindowController {
    /// Opens a picture or PDF in its own tab, or shows the tab it already has.
    func openMediaFile(at url: URL, preview: Bool = false) {
        if let i = tabIndex(forPath: url.path) {
            if !preview { tabs[i].isPreview = false }
            selectTab(i)
            return
        }
        let name = url.lastPathComponent
        let rel = relativePath(url.path)
        let components = rel.map { $0.split(separator: "/").map(String.init) } ?? [name]
        // `doc` is an empty buffer that is never shown, as for the Git Graph tab.
        let tab = EditorTab(doc: bridge.newDocument(), title: name, pathComponents: components,
                            windowTitle: name, isPreview: preview, status: "", mediaURL: url)
        tab.fileKey = Self.fileKey(url.path)
        tab.relPath = rel
        add(tab, replacing: slotForOpening(preview: preview))
    }

    /// Shows or hides the viewer for the active tab.
    func showEditorMedia(for tab: EditorTab?) {
        if let tab, let url = tab.mediaURL {
            if mediaViewer.isStale(for: url) { mediaViewer.show(url: url) }
            mediaViewer.isHidden = false
            tab.status = mediaViewer.summary
            statusLabel.stringValue = tab.status
        } else if !mediaViewer.isHidden {
            if (window?.firstResponder as? NSView)?.isDescendant(of: mediaViewer) == true {
                window?.makeFirstResponder(editorView)
            }
            mediaViewer.isHidden = true
        }
    }

    /// Files and folders dropped on the window or the Dock icon. A folder
    /// becomes the workspace first, so the files land in its tab bar.
    func open(urls: [URL]) {
        func isFolder(_ url: URL) -> Bool {
            (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true
        }
        if let folder = urls.first(where: isFolder) { open(url: folder) }
        for url in urls where !isFolder(url) { open(url: url) }
    }
}
