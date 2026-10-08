import AppKit
import UniformTypeIdentifiers
import os

/// One row in the explorer. Children are `nil` until the folder is first
/// expanded, so a huge tree costs nothing until the user opens it.
@MainActor
final class ExplorerNode {
    let entry: FileEntry
    var children: [ExplorerNode]?

    init(entry: FileEntry) {
        self.entry = entry
    }
}

/// File explorer sidebar (M2-04): an `NSOutlineView` that lists one
/// directory at a time through the Rust `Workspace`.
@MainActor
final class ExplorerController: NSObject {
    /// Row height in points.
    static let rowHeight: CGFloat = 22

    let scrollView = NSScrollView()
    let outlineView = NSOutlineView()

    private(set) var workspace: Workspace?
    /// Number of `listDir` calls so far. Tests use it to prove laziness.
    private(set) var listCalls = 0
    /// Called when the user selects a file row.
    var onOpenFile: ((Workspace, FileEntry) -> Void)?

    private var roots: [ExplorerNode]?
    private var iconCache: [String: NSImage] = [:]

    override init() {
        super.init()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        outlineView.addTableColumn(column)
        outlineView.outlineTableColumn = column
        outlineView.headerView = nil
        // Compact VSCode-like rows; the source-list style would force ~28 pt.
        outlineView.style = .fullWidth
        outlineView.rowSizeStyle = .custom
        outlineView.rowHeight = Self.rowHeight
        outlineView.intercellSpacing = .zero
        outlineView.indentationPerLevel = 12
        outlineView.autoresizesOutlineColumn = false
        outlineView.allowsEmptySelection = true
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.setAccessibilityLabel("File explorer")
        outlineView.setAccessibilityIdentifier("explorer")

        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
    }

    /// Shows `workspace`. Only the root directory is listed, and only when
    /// the outline view first asks for it.
    func setWorkspace(_ workspace: Workspace) {
        self.workspace = workspace
        roots = nil
        listCalls = 0
        outlineView.reloadData()
    }

    private func children(of node: ExplorerNode?) -> [ExplorerNode] {
        if let node, let loaded = node.children { return loaded }
        if node == nil, let roots { return roots }
        guard let workspace else { return [] }

        listCalls += 1
        let rel = node?.entry.relPath ?? ""
        let list: [ExplorerNode]
        do {
            list = try workspace.listDir(relPath: rel).map { ExplorerNode(entry: $0) }
        } catch {
            Log.ui.error("list \(rel, privacy: .public) failed: \(String(describing: error), privacy: .public)")
            list = []
        }
        if let node { node.children = list } else { roots = list }
        return list
    }

    private func icon(for entry: FileEntry) -> NSImage {
        let key = entry.isDir ? "/dir" : (entry.name as NSString).pathExtension.lowercased()
        if let cached = iconCache[key] { return cached }
        let type: UTType = entry.isDir
            ? .folder
            : (UTType(filenameExtension: key) ?? .data)
        let image = NSWorkspace.shared.icon(for: type)
        image.size = NSSize(width: 16, height: 16)
        iconCache[key] = image
        return image
    }
}

extension ExplorerController: NSOutlineViewDataSource {
    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        children(of: item as? ExplorerNode).count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        children(of: item as? ExplorerNode)[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? ExplorerNode)?.entry.isDir ?? false
    }
}

extension ExplorerController: NSOutlineViewDelegate {
    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let node = item as? ExplorerNode else { return nil }
        let id = NSUserInterfaceItemIdentifier("cell")
        let cell: NSTableCellView
        if let reused = outlineView.makeView(withIdentifier: id, owner: self) as? NSTableCellView {
            cell = reused
        } else {
            cell = NSTableCellView()
            cell.identifier = id
            let image = NSImageView()
            let text = NSTextField(labelWithString: "")
            text.lineBreakMode = .byTruncatingMiddle
            text.font = .systemFont(ofSize: 13)
            image.translatesAutoresizingMaskIntoConstraints = false
            text.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(image)
            cell.addSubview(text)
            cell.imageView = image
            cell.textField = text
            NSLayoutConstraint.activate([
                image.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                image.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                image.widthAnchor.constraint(equalToConstant: 16),
                image.heightAnchor.constraint(equalToConstant: 16),
                text.leadingAnchor.constraint(equalTo: image.trailingAnchor, constant: 6),
                text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
                text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
        }
        cell.textField?.stringValue = node.entry.name
        cell.imageView?.image = icon(for: node.entry)
        return cell
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        let row = outlineView.selectedRow
        guard row >= 0, let node = outlineView.item(atRow: row) as? ExplorerNode else { return }
        if node.entry.isDir {
            // Clicking a folder toggles it, like VSCode.
            if outlineView.isItemExpanded(node) {
                outlineView.collapseItem(node)
            } else {
                outlineView.expandItem(node)
            }
        } else if let workspace {
            onOpenFile?(workspace, node.entry)
        }
    }
}
