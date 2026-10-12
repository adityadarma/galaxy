import AppKit
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

/// File explorer sidebar (M2-04), drawn like VSCode's: a chevron for
/// folders (no folder icon), small colored file-type glyphs, indent guides,
/// and a full-width selection. Lists one directory at a time through the
/// Rust `Workspace`.
@MainActor
final class ExplorerController: NSObject {
    /// Row height in points.
    static let rowHeight: CGFloat = 22

    let scrollView = NSScrollView()
    let outlineView = ExplorerOutlineView()

    private(set) var workspace: Workspace?
    /// Number of `listDir` calls so far. Tests use it to prove laziness.
    private(set) var listCalls = 0
    /// Called when the user selects a file row (opens a preview tab).
    var onOpenFile: ((Workspace, FileEntry) -> Void)?
    /// Called when the user double-clicks a file row (keeps it open).
    var onPinFile: ((Workspace, FileEntry) -> Void)?

    private var roots: [ExplorerNode]?
    /// Relative paths of files with unsaved changes (M2-04). Folders that
    /// contain one are marked too.
    var dirtyPaths: Set<String> = [] {
        didSet { if dirtyPaths != oldValue { refreshDirtyMarks() } }
    }
    var gitChangedPaths: Set<String> = [] {
        didSet { if gitChangedPaths != oldValue { refreshGitMarks() } }
    }
    var gitStatusMap: [String: GitFileStatus] = [:] {
        didSet {
            gitChangedPaths = Set(gitStatusMap.keys)
            refreshGitMarks()
        }
    }
    /// Set while a folder click selects its row, so the selection change
    /// does not toggle the folder a second time.
    private var selectingFromClick = false
    /// Set while `reveal` selects a row: no opening, no toggling.
    private var quietSelection = false
    /// Row being renamed in place (M2-05), and its field.
    private(set) var renamingEntry: FileEntry?
    private weak var renameField: NSTextField?
    private weak var renameCell: ExplorerCell?
    /// Called with the new name when an in-place rename is confirmed.
    var onRename: ((FileEntry, String) -> Void)?
    /// Builds the right-click menu, after the clicked row was selected.
    var contextMenu: (() -> NSMenu?)?

    override init() {
        super.init()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        outlineView.addTableColumn(column)
        outlineView.outlineTableColumn = column
        outlineView.headerView = nil
        outlineView.style = .fullWidth
        outlineView.rowSizeStyle = .custom
        outlineView.rowHeight = Self.rowHeight
        outlineView.intercellSpacing = .zero
        outlineView.backgroundColor = .clear
        outlineView.autoresizesOutlineColumn = false
        outlineView.allowsEmptySelection = true
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.target = self
        outlineView.doubleAction = #selector(doubleClicked(_:))
        outlineView.onFolderClick = { [weak self] row in self?.folderClicked(row: row) ?? false }
        outlineView.onContextMenu = { [weak self] row in self?.menu(forRow: row) }
        outlineView.setAccessibilityLabel("File explorer")
        outlineView.setAccessibilityIdentifier("explorer")

        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.scrollerStyle = .overlay
        scrollView.autohidesScrollers = true
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

    /// Relists after the watcher reported `paths` (M2-02), but only when
    /// one of them is in a folder that has been listed. Returns whether it
    /// relisted.
    @discardableResult
    func refresh(changed paths: [String]) -> Bool {
        guard workspace != nil, roots != nil else { return false }
        var listed: Set<String> = [""]
        var stack = roots ?? []
        while let node = stack.popLast() {
            if let children = node.children {
                listed.insert(node.entry.relPath)
                stack.append(contentsOf: children)
            }
        }
        let touched = paths.contains { listed.contains(($0 as NSString).deletingLastPathComponent) }
            || paths.contains { listed.contains($0) }
        guard touched else { return false }
        refresh()
        return true
    }

    /// Lists everything again, keeping open folders open and the selected
    /// row selected (without opening it again).
    func refresh() {
        guard workspace != nil else { return }
        var open = Set<String>()
        for row in 0..<outlineView.numberOfRows {
            if let node = outlineView.item(atRow: row) as? ExplorerNode, outlineView.isItemExpanded(node) {
                open.insert(node.entry.relPath)
            }
        }
        let selected = selectedEntry?.relPath
        let scroll = scrollView.contentView.bounds.origin
        roots = nil
        quietSelection = true
        defer { quietSelection = false }
        outlineView.reloadData()
        // Expanding adds rows below, so walk forward as the list grows.
        var row = 0
        while row < outlineView.numberOfRows {
            if let node = outlineView.item(atRow: row) as? ExplorerNode, open.contains(node.entry.relPath) {
                outlineView.expandItem(node)
            }
            row += 1
        }
        if let selected, let row = self.row(forRelPath: selected) {
            outlineView.selectRowIndexes([row], byExtendingSelection: false)
        }
        scrollView.contentView.scroll(to: scroll)
        scrollView.reflectScrolledClipView(scrollView.contentView)
    }

    func isDirty(_ entry: FileEntry) -> Bool {
        guard !dirtyPaths.isEmpty else { return false }
        if !entry.isDir { return dirtyPaths.contains(entry.relPath) }
        let prefix = entry.relPath + "/"
        return dirtyPaths.contains { $0.hasPrefix(prefix) }
    }

    func isGitChanged(_ entry: FileEntry) -> Bool {
        if !entry.isDir { return gitChangedPaths.contains(entry.relPath) || (gitStatusMap[entry.relPath] != nil) }
        let prefix = entry.relPath.isEmpty ? "" : entry.relPath + "/"
        return gitChangedPaths.contains { $0.hasPrefix(prefix) } || gitStatusMap.keys.contains { $0.hasPrefix(prefix) }
    }

    func gitStatus(for entry: FileEntry) -> GitFileStatus {
        if !entry.isDir {
            return gitStatusMap[entry.relPath] ?? (gitChangedPaths.contains(entry.relPath) ? .modified : .none)
        }
        let prefix = entry.relPath.isEmpty ? "" : entry.relPath + "/"
        var hasAdded = false
        var hasModified = false
        var hasDeleted = false
        for (path, status) in gitStatusMap where path.hasPrefix(prefix) {
            switch status {
            case .modified: hasModified = true
            case .added, .untracked, .renamed: hasAdded = true
            case .deleted: hasDeleted = true
            case .none: break
            }
        }
        if hasModified { return .modified }
        if hasAdded { return .added }
        if hasDeleted { return .deleted }
        if gitChangedPaths.contains(where: { $0.hasPrefix(prefix) }) { return .modified }
        return .none
    }

    /// Updates the rows on screen; others pick it up when they are made.
    private func refreshDirtyMarks() {
        let rows = outlineView.rows(in: outlineView.visibleRect)
        guard rows.length > 0 else { return }
        for row in rows.location..<(rows.location + rows.length) {
            guard let node = outlineView.item(atRow: row) as? ExplorerNode,
                  let cell = outlineView.view(atColumn: 0, row: row, makeIfNecessary: false) as? ExplorerCell
            else { continue }
            cell.isDirty = isDirty(node.entry)
        }
    }

    private func refreshGitMarks() {
        let rows = outlineView.rows(in: outlineView.visibleRect)
        guard rows.length > 0 else { return }
        for row in rows.location..<(rows.location + rows.length) {
            guard let node = outlineView.item(atRow: row) as? ExplorerNode,
                  let cell = outlineView.view(atColumn: 0, row: row, makeIfNecessary: false) as? ExplorerCell
            else { continue }
            let status = gitStatus(for: node.entry)
            cell.gitStatus = status
            cell.isGitChanged = status != .none
        }
    }

    /// The selected row's entry, if any.
    var selectedEntry: FileEntry? {
        let row = outlineView.selectedRow
        return row >= 0 ? (outlineView.item(atRow: row) as? ExplorerNode)?.entry : nil
    }

    /// Where New File / New Folder go: the selected folder, the selected
    /// file's folder, or the root (`""`).
    var targetFolder: String {
        guard let entry = selectedEntry else { return "" }
        if entry.isDir { return entry.relPath }
        return (entry.relPath as NSString).deletingLastPathComponent
    }

    /// Expands the folders down to `relPath` and selects it, without
    /// opening or toggling anything.
    func reveal(relPath: String) {
        guard workspace != nil, !relPath.isEmpty else { return }
        if roots == nil {
            _ = children(of: nil)
            outlineView.reloadData()
        }
        let parts = relPath.split(separator: "/").map(String.init)
        var prefix = ""
        for (i, part) in parts.enumerated() {
            prefix = prefix.isEmpty ? part : prefix + "/" + part
            guard let row = row(forRelPath: prefix) else { return }
            if i < parts.count - 1, let node = outlineView.item(atRow: row) {
                outlineView.expandItem(node)
            } else {
                quietSelection = true
                outlineView.selectRowIndexes([row], byExtendingSelection: false)
                quietSelection = false
                outlineView.scrollRowToVisible(row)
            }
        }
    }

    private func row(forRelPath rel: String) -> Int? {
        (0..<outlineView.numberOfRows).first {
            (outlineView.item(atRow: $0) as? ExplorerNode)?.entry.relPath == rel
        }
    }

    /// Right click: selects the row under the pointer without opening it
    /// (nothing when below the rows, meaning the root), then the menu.
    func menu(forRow row: Int) -> NSMenu? {
        if renamingEntry != nil { finishRename(commit: true) }
        quietSelection = true
        if row >= 0 {
            outlineView.selectRowIndexes([row], byExtendingSelection: false)
        } else {
            outlineView.deselectAll(nil)
        }
        quietSelection = false
        return contextMenu?()
    }

    // MARK: Rename in place (M2-05)

    /// Turns the row's name into a text field with the name selected up to
    /// the extension, as in VSCode. Return confirms, Escape cancels, and
    /// moving focus elsewhere confirms.
    @discardableResult
    func beginRename(relPath: String) -> Bool {
        if renamingEntry != nil { finishRename(commit: true) }
        guard let row = row(forRelPath: relPath), let node = outlineView.item(atRow: row) as? ExplorerNode else {
            return false
        }
        outlineView.scrollRowToVisible(row)
        guard let cell = outlineView.view(atColumn: 0, row: row, makeIfNecessary: true) as? ExplorerCell else {
            return false
        }
        renamingEntry = node.entry
        renameCell = cell
        renameField = cell.label
        cell.setEditing(true)
        cell.label.delegate = self
        outlineView.window?.makeFirstResponder(cell.label)
        if let editor = cell.label.currentEditor() {
            let name = node.entry.name as NSString
            let stem = node.entry.isDir ? name : name.deletingPathExtension as NSString
            // ".gitignore" has no stem: select it all.
            let length = stem.length == 0 ? name.length : stem.length
            editor.selectedRange = NSRange(location: 0, length: length)
        }
        return true
    }

    /// Ends an in-place rename; `commit` passes the new name on.
    func finishRename(commit: Bool) {
        guard let entry = renamingEntry else { return }
        renamingEntry = nil
        let name = renameField?.stringValue ?? entry.name
        renameField?.delegate = nil
        renameField?.stringValue = entry.name
        renameCell?.setEditing(false)
        renameField = nil
        renameCell = nil
        outlineView.window?.makeFirstResponder(outlineView)
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        if commit, !trimmed.isEmpty, trimmed != entry.name { onRename?(entry, name) }
    }

    /// The field while a rename is in progress (tests type into it).
    var renameFieldForTesting: NSTextField? { renameField }

    func collapseAll() {
        outlineView.collapseItem(nil, collapseChildren: true)
    }

    private func toggle(_ node: ExplorerNode) {
        if outlineView.isItemExpanded(node) {
            outlineView.collapseItem(node)
        } else {
            outlineView.expandItem(node)
        }
    }

    /// Mouse down on a row. Every click on a folder opens or closes it,
    /// however fast the clicks come (a quick second click is not swallowed
    /// as a double-click), as in VSCode. Returns `false` for files, which
    /// keep the normal table click handling.
    func folderClicked(row: Int) -> Bool {
        guard let node = outlineView.item(atRow: row) as? ExplorerNode, node.entry.isDir else { return false }
        outlineView.window?.makeFirstResponder(outlineView)
        selectingFromClick = true
        outlineView.selectRowIndexes([row], byExtendingSelection: false)
        selectingFromClick = false
        toggle(node)
        return true
    }

    @objc private func doubleClicked(_ sender: Any?) {
        let row = outlineView.clickedRow
        guard row >= 0, let node = outlineView.item(atRow: row) as? ExplorerNode,
              !node.entry.isDir, let workspace else { return }
        onPinFile?(workspace, node.entry)
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

    private func updateChevron(of notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? ExplorerNode else { return }
        let row = outlineView.row(forItem: node)
        guard row >= 0 else { return }
        (outlineView.view(atColumn: 0, row: row, makeIfNecessary: false) as? ExplorerCell)?
            .isExpanded = outlineView.isItemExpanded(node)
    }
}

extension ExplorerController: NSTextFieldDelegate {
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.cancelOperation(_:)):
            finishRename(commit: false)
            return true
        case #selector(NSResponder.insertNewline(_:)):
            finishRename(commit: true)
            return true
        default:
            return false
        }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        // Focus moved elsewhere: keep the typed name, as VSCode does.
        finishRename(commit: true)
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
        let cell = outlineView.makeView(withIdentifier: ExplorerCell.reuseID, owner: self) as? ExplorerCell
            ?? ExplorerCell()
        cell.show(node.entry, expanded: outlineView.isItemExpanded(node))
        cell.isDirty = isDirty(node.entry)
        let status = gitStatus(for: node.entry)
        cell.gitStatus = status
        cell.isGitChanged = status != .none
        return cell
    }

    func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
        let row = outlineView.makeView(withIdentifier: ExplorerRowView.reuseID, owner: self) as? ExplorerRowView
            ?? ExplorerRowView()
        row.level = outlineView.level(forItem: item)
        return row
    }

    func outlineViewItemDidExpand(_ notification: Notification) { updateChevron(of: notification) }
    func outlineViewItemDidCollapse(_ notification: Notification) { updateChevron(of: notification) }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        let row = outlineView.selectedRow
        guard row >= 0, let node = outlineView.item(atRow: row) as? ExplorerNode, !quietSelection else { return }
        let event = NSApp.currentEvent?.type
        if node.entry.isDir {
            // Arrow keys only move, as in VSCode (← and → open and close).
            // Mouse clicks toggle in `folderClicked`.
            guard event != .keyDown, !selectingFromClick else { return }
            toggle(node)
        } else if let workspace {
            onOpenFile?(workspace, node.entry)
        }
    }
}

// MARK: - Views

/// Places cells by depth itself and hides the system disclosure triangle;
/// cells draw their own chevron.
@MainActor
final class ExplorerOutlineView: NSOutlineView {
    static let baseInset: CGFloat = 8
    static let indent: CGFloat = 12

    /// Handles a left click on `row`; returns `true` when it did.
    var onFolderClick: ((Int) -> Bool)?
    /// Right click on `row` (-1 below the rows).
    var onContextMenu: ((Int) -> NSMenu?)?
    /// Keys handled before the table's own (Return renames, ⌘⌫ deletes).
    var onKey: ((NSEvent) -> Bool)?

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = row(at: convert(event.locationInWindow, from: nil))
        return onContextMenu?(row) ?? super.menu(for: event)
    }

    override func keyDown(with event: NSEvent) {
        if onKey?(event) == true { return }
        super.keyDown(with: event)
    }

    override func mouseDown(with event: NSEvent) {
        let row = row(at: convert(event.locationInWindow, from: nil))
        let plain = event.modifierFlags.intersection([.command, .shift, .control, .option]).isEmpty
        if row >= 0, plain, onFolderClick?(row) == true { return }
        super.mouseDown(with: event)
    }

    override func frameOfOutlineCell(atRow row: Int) -> NSRect { .zero }

    override func frameOfCell(atColumn column: Int, row: Int) -> NSRect {
        let frame = super.frameOfCell(atColumn: column, row: row)
        let x = Self.baseInset + CGFloat(level(forRow: row)) * Self.indent
        return NSRect(x: x, y: frame.minY, width: max(0, bounds.width - x), height: frame.height)
    }

    /// x of the indent guide for depth `level`: under that level's chevron.
    static func guideX(level: Int) -> CGFloat {
        baseInset + CGFloat(level) * indent + ExplorerCell.chevronSize / 2
    }
}

@MainActor
final class ExplorerCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("explorer-cell")
    static let chevronSize: CGFloat = 16

    let chevron = NSImageView()
    let icon = NSImageView()
    let label = NSTextField(labelWithString: "")
    /// Copper dot on the right for unsaved changes, as on the tab.
    let dirtyDot = NSTextField(labelWithString: "●")
    let gitDot = NSTextField(labelWithString: "●")
    private(set) var isFolder = false

    var isDirty = false {
        didSet {
            dirtyDot.isHidden = !isDirty
            label.setAccessibilityLabel(isDirty ? "\(label.stringValue), unsaved changes" : nil)
        }
    }

    var gitStatus: GitFileStatus = .none {
        didSet {
            if let color = gitStatus.color {
                label.textColor = color
            } else {
                label.textColor = .labelColor
            }
        }
    }

    var isGitChanged = false {
        didSet {
            gitDot.isHidden = true
            if !isGitChanged {
                gitStatus = .none
            }
        }
    }

    var isExpanded = false {
        didSet { if isFolder, isExpanded != oldValue { updateChevron() } }
    }

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseID
        chevron.imageScaling = .scaleNone
        chevron.contentTintColor = .secondaryLabelColor
        chevron.setAccessibilityElement(false)
        icon.imageScaling = .scaleNone
        icon.setAccessibilityElement(false)
        label.font = .systemFont(ofSize: 13)
        label.lineBreakMode = .byTruncatingMiddle
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        imageView = icon
        textField = label
        dirtyDot.font = .systemFont(ofSize: 9)
        dirtyDot.textColor = Theme.accent
        dirtyDot.isHidden = true
        dirtyDot.setAccessibilityElement(false)
        dirtyDot.setContentCompressionResistancePriority(.required, for: .horizontal)
        gitDot.font = .systemFont(ofSize: 9)
        gitDot.textColor = .systemYellow
        gitDot.isHidden = true
        gitDot.setContentCompressionResistancePriority(.required, for: .horizontal)

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [chevron, icon, label, spacer, gitDot, dirtyDot])
        row.orientation = .horizontal
        row.spacing = 4
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            chevron.widthAnchor.constraint(equalToConstant: Self.chevronSize),
            icon.widthAnchor.constraint(equalToConstant: 16),
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Folders: chevron and name. Files: an empty chevron slot (so names
    /// line up as in VSCode), then the file-type glyph and name.
    /// The name becomes an editable field for a rename in place.
    func setEditing(_ editing: Bool) {
        label.isEditable = editing
        label.isSelectable = editing
        label.drawsBackground = editing
        label.backgroundColor = editing ? .textBackgroundColor : nil
        label.isBordered = editing
        label.lineBreakMode = editing ? .byClipping : .byTruncatingMiddle
        label.cell?.isScrollable = editing
    }

    func show(_ entry: FileEntry, expanded: Bool) {
        setEditing(false)
        label.stringValue = entry.name
        isFolder = entry.isDir
        isExpanded = expanded
        icon.isHidden = entry.isDir
        if entry.isDir {
            updateChevron()
        } else {
            chevron.image = nil
            let glyph = FileGlyph.for(name: entry.name)
            icon.image = glyph.image
            icon.contentTintColor = glyph.color
        }
    }

    private func updateChevron() {
        chevron.image = NSImage(systemSymbolName: isExpanded ? "chevron.down" : "chevron.right",
                                accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
    }
}

/// Full-width selection and hover, plus indent guides.
@MainActor
final class ExplorerRowView: NSTableRowView {
    static let reuseID = NSUserInterfaceItemIdentifier("explorer-row")
    var level = 0 { didSet { if level != oldValue { needsDisplay = true } } }
    private var isHovered = false { didSet { if isHovered != oldValue { needsDisplay = true } } }

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseID
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func drawBackground(in dirtyRect: NSRect) {
        if isHovered, !isSelected {
            NSColor.labelColor.withAlphaComponent(0.05).setFill()
            bounds.fill()
        }
        NSColor.labelColor.withAlphaComponent(0.12).setFill()
        for l in 0..<level {
            NSRect(x: ExplorerOutlineView.guideX(level: l), y: 0, width: 1, height: bounds.height).fill()
        }
    }

    override func drawSelection(in dirtyRect: NSRect) {
        if isEmphasized {
            Theme.accent.withAlphaComponent(0.22).setFill()
            bounds.fill()
            // Focused row outline, as VSCode marks keyboard focus.
            Theme.accent.withAlphaComponent(0.7).setStroke()
            let outline = NSBezierPath(rect: bounds.insetBy(dx: 0.5, dy: 0.5))
            outline.lineWidth = 1
            outline.stroke()
        } else {
            NSColor.labelColor.withAlphaComponent(0.09).setFill()
            bounds.fill()
        }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }
}

/// Small colored glyph per file type, in the spirit of VSCode's Seti icons.
@MainActor
enum FileGlyph {
    struct Glyph {
        let image: NSImage?
        let color: NSColor
    }

    private static var cache: [String: Glyph] = [:]

    static func `for`(name: String) -> Glyph {
        let ext = (name as NSString).pathExtension.lowercased()
        let key = name.hasPrefix(".git") ? ".git" : ext
        if let hit = cache[key] { return hit }
        let (symbol, color) = style(for: key)
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .medium))
        let glyph = Glyph(image: image, color: color)
        cache[key] = glyph
        return glyph
    }

    private static func style(for key: String) -> (String, NSColor) {
        switch key {
        case "rs": ("gearshape.fill", Theme.accent)
        case "swift": ("swift", .systemOrange)
        case "md", "markdown": ("info.circle.fill", .systemBlue)
        case "json": ("curlybraces", .systemYellow)
        case "toml", "lock", "yml", "yaml", "ini", "cfg", "plist", "xcconfig":
            ("slider.horizontal.3", .secondaryLabelColor)
        case "sh", "zsh", "bash", "fish": ("terminal", .systemGreen)
        case "js", "mjs", "cjs", "jsx": ("j.square.fill", .systemYellow)
        case "ts", "tsx": ("t.square.fill", .systemBlue)
        case "html", "htm", "xml": ("chevron.left.forwardslash.chevron.right", .systemOrange)
        case "css", "scss": ("number", .systemBlue)
        case "py": ("p.square.fill", .systemTeal)
        case "c", "h", "cpp", "hpp", "m", "mm": ("c.square.fill", .systemIndigo)
        case "png", "jpg", "jpeg", "gif", "svg", "webp", "icns": ("photo", .systemPurple)
        case ".git": ("arrow.triangle.branch", .systemRed)
        case "txt", "log": ("doc.text", .secondaryLabelColor)
        default: ("doc", .secondaryLabelColor)
        }
    }
}
