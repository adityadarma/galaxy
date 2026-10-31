import AppKit
import os

/// One row in the explorer. Children are `nil` until the folder is first
/// expanded, so a huge tree costs nothing until the user opens it.
@MainActor
final class ExplorerNode {
    let entry: FileEntry
    var children: [ExplorerNode]?
    /// Matched by a `.gitignore` rule itself or sits inside a folder that is.
    /// The core flags only the entry; the folder's flag is carried down here.
    let isIgnored: Bool

    init(entry: FileEntry, parentIgnored: Bool = false) {
        self.entry = entry
        self.isIgnored = parentIgnored || entry.isIgnored
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
            let inherited = node?.isIgnored ?? false
            list = try workspace.listDir(relPath: rel).map { ExplorerNode(entry: $0, parentIgnored: inherited) }
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
        cell.isIgnored = node.isIgnored
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

    /// The pointer is over the explorer. Indent guides are drawn only then.
    private(set) var isPointerInside = false {
        didSet {
            guard isPointerInside != oldValue else { return }
            enumerateAvailableRowViews { row, _ in row.needsDisplay = true }
        }
    }

    private func syncPointer() {
        guard let window else { isPointerInside = false; return }
        let point = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        isPointerInside = NSMouseInRect(point, visibleRect, isFlipped)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.filter { $0.owner === self }.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self, userInfo: nil))
        syncPointer()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        syncPointer()
    }

    override func mouseEntered(with event: NSEvent) { isPointerInside = true }
    override func mouseExited(with event: NSEvent) { isPointerInside = false }

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

    /// Ignored by git: the whole row is dimmed, like VS Code does.
    var isIgnored = false {
        didSet { alphaValue = isIgnored ? Self.ignoredAlpha : 1 }
    }
    static let ignoredAlpha: CGFloat = 0.5

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
        icon.imageScaling = .scaleProportionallyDown
        icon.setAccessibilityElement(false)
        label.font = .systemFont(ofSize: 13)
        label.lineBreakMode = .byTruncatingTail
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
        row.setCustomSpacing(6, after: icon)
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
        label.backgroundColor = editing ? Theme.editorBackground : nil
        label.isBordered = editing
        label.lineBreakMode = editing ? .byClipping : .byTruncatingTail
        label.cell?.isScrollable = editing
    }

    func show(_ entry: FileEntry, expanded: Bool) {
        setEditing(false)
        label.stringValue = entry.name
        isFolder = entry.isDir
        isExpanded = expanded
        if entry.isDir {
            let folder = ExplorerIcon.folder(named: entry.name)
            icon.isHidden = folder.image == nil || !ExplorerIcon.showsFolderIcons
            icon.image = folder.image
            icon.contentTintColor = nil
            updateChevron()
        } else {
            icon.isHidden = false
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
    private(set) var isHovered = false { didSet { if isHovered != oldValue { needsDisplay = true } } }

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseID
    }

    /// Row views are recycled while the pointer stays still (restoring the
    /// session expands folders and scrolls). A view that was hovered then
    /// moved away never gets `mouseExited`, so it kept its hover tint on
    /// whatever row it was reused for.
    override func prepareForReuse() {
        super.prepareForReuse()
        isHovered = false
    }

    /// Hover follows where the pointer really is, not the last event seen.
    private func syncHoverWithPointer() {
        guard let window else { isHovered = false; return }
        let point = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        isHovered = window.isKeyWindow && NSMouseInRect(point, bounds, isFlipped)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        syncHoverWithPointer()
    }

    override func setFrameOrigin(_ newOrigin: NSPoint) {
        super.setFrameOrigin(newOrigin)
        // A row that moved under (or away from) a still pointer gets no event.
        if isHovered || window != nil { syncHoverWithPointer() }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func drawBackground(in dirtyRect: NSRect) {
        if isHovered, !isSelected {
            NSColor.labelColor.withAlphaComponent(0.05).setFill()
            bounds.fill()
        }
        // Guides show only while the pointer is over the explorer.
        guard (superview as? ExplorerOutlineView)?.isPointerInside == true else { return }
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
        syncHoverWithPointer()
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }
}

/// Material Icon Theme assets are kept as SVG so the Explorer stays sharp at
/// every macOS scale. Unsupported types retain the existing SF Symbol fallback.
@MainActor
enum ExplorerIcon {
    struct Icon {
        let image: NSImage?
        let isMaterial: Bool
    }

    private static var cache: [String: NSImage?] = [:]

    /// UserDefaults key of the Settings > Explorer "Folder icons" switch.
    static let showFolderIconsKey = "ExplorerShowFolderIcons"
    static var showsFolderIcons: Bool {
        UserDefaults.standard.object(forKey: showFolderIconsKey) as? Bool ?? true
    }

    static func folder(named name: String) -> Icon {
        let asset = folderAsset(named: name) ?? "folder-base"
        return Icon(image: image(named: asset), isMaterial: true)
    }

    static func folderAsset(named name: String) -> String? {
        switch name.lowercased() {
        case "src", "sources", "source": "folder-src"
        case "app": "folder-src"
        case ".docker", "docker": "folder-docker"
        case ".git": "folder-git"
        case ".github", ".gitea": "folder-github"
        case "config", "configs", "configuration", "settings", "bootstrap", "cache", "framework": "folder-config"
        case "images", "image", "img", "icons", "media": "folder-images"
        case "public", "static", "storage": "folder-public"
        case "docs", "doc", "documentation": "folder-docs"
        case "test", "tests", "spec", "specs", "__tests__", "feature", "unit": "folder-test"
        case "components", "component", "models", "providers", "services", "views", "css", "js": "folder-components"
        case "routes", "route", "http": "folder-routes"
        case "database", "db": "folder-database"
        case "migrations", "migration", "factories", "seeders": "folder-migrations"
        case "resources": "folder-src"
        case "scripts", "script", "bin": "folder-scripts"
        case "node_modules": "folder-node"
        case "packages", "package", "vendor": "folder-packages"
        default: nil
        }
    }

    static func image(named name: String) -> NSImage? {
        if let hit = cache[name] { return hit }
        // Xcode copies resource folders into the bundle's flat Resources root.
        let url = Bundle.main.url(forResource: name, withExtension: "svg")
            ?? Bundle.main.url(forResource: name, withExtension: "svg", subdirectory: "MaterialIcons")
        let image = url.flatMap(NSImage.init(contentsOf:))
        // The SVGs only carry a viewBox (24 or 32 units), which NSImage takes
        // as point size. Pin them to the 16pt glyph size used everywhere;
        // vector content still renders sharp at that size.
        image?.size = NSSize(width: 16, height: 16)
        cache[name] = image
        return image
    }
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
        let lowercased = name.lowercased()
        let ext = (lowercased as NSString).pathExtension
        let key = namedFileKey(lowercased) ?? (lowercased.hasSuffix(".blade.php") ? "blade.php" : (lowercased.hasPrefix(".git") ? ".git" : ext))
        if let hit = cache[key] { return hit }
        let (asset, symbol, color) = style(for: key)
        let image = asset.flatMap(ExplorerIcon.image(named:)) ?? NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .medium))
        let glyph = Glyph(image: image, color: color)
        cache[key] = glyph
        return glyph
    }

    static func namedFileKey(_ name: String) -> String? {
        if name.hasPrefix("dockerfile.") { return "dockerfile" }
        if name.hasPrefix(".env.") { return ".env" }
        if name.contains("logrotate") { return "logrotate" }
        if name.hasPrefix("supervisord.") { return "supervisor" }
        if name.hasPrefix("firebase") { return "firebase" }
        if name.hasPrefix("supabase") { return "supabase" }
        if name.hasPrefix("vercel") { return "vercel" }
        if name.hasPrefix("netlify") { return "netlify" }
        if name.hasPrefix("crontab") { return "crontab" }
        return switch name {
        case "dockerfile", "docker-compose.yml", "docker-compose.yaml",
            "package.json", "package-lock.json", "npm-shrinkwrap.json",
            "pnpm-lock.yaml", "yarn.lock", "bun.lockb", "bun.lock",
            "composer.lock", "cargo.lock", "gemfile.lock", "tsconfig.json", "jsconfig.json",
            ".eslintrc", ".eslintrc.json", ".eslintrc.js", "eslint.config.js", "eslint.config.mjs",
            ".prettierrc", ".prettierrc.json", ".prettierrc.js", "prettier.config.js",
            "readme.md", "readme", "license", "license.md", "copying",
            ".dockerignore", ".editorconfig", ".env", ".env.example", ".npmignore", ".prettierignore",
            ".gitignore", ".gitattributes", ".gitmodules", ".gitlab-ci.yml",
            ".gitlab-ci_gitlab.yml", ".gitlab-ci_release.yml", "artisan", "crontab", "nginx.conf", "www.conf",
            "firebase.json", "supabase.toml", "vercel.json", "netlify.toml", ".npmrc", "composer.json":
            name
        default:
            nil
        }
    }

    static func style(for key: String) -> (String?, String, NSColor) {
        switch key {
        case "dockerfile", "docker-compose.yml", "docker-compose.yaml": ("docker", "shippingbox", .labelColor)
        case "package.json": ("npm", "shippingbox", .labelColor)
        case "package-lock.json", "npm-shrinkwrap.json": ("lock", "lock", .labelColor)
        case "pnpm-lock.yaml": ("pnpm", "lock", .labelColor)
        case "yarn.lock": ("yarn", "lock", .labelColor)
        case "bun.lockb", "bun.lock": ("bun", "lock", .labelColor)
        case "composer.lock", "cargo.lock", "gemfile.lock": ("lock", "lock", .labelColor)
        case "tsconfig.json", "jsconfig.json": ("tsconfig", "gearshape", .labelColor)
        case ".eslintrc", ".eslintrc.json", ".eslintrc.js", "eslint.config.js", "eslint.config.mjs": ("eslint", "checkmark.seal", .labelColor)
        case ".prettierrc", ".prettierrc.json", ".prettierrc.js", "prettier.config.js": ("prettier", "paintbrush", .labelColor)
        case "readme.md", "readme": ("readme", "book", .labelColor)
        case "license", "license.md", "copying": ("license", "doc.text", .labelColor)
        case ".dockerignore": ("docker", "shippingbox", .labelColor)
        case ".editorconfig": ("settings", "slider.horizontal.3", .labelColor)
        case ".env", ".env.example": ("lock", "lock", .labelColor)
        case ".npmignore": ("npm", "shippingbox", .labelColor)
        case ".npmrc": ("npm", "shippingbox", .labelColor)
        case "composer.json": ("json", "curlybraces", .labelColor)
        case ".prettierignore": ("prettier", "paintbrush", .labelColor)
        case ".gitignore", ".gitattributes", ".gitmodules": ("git", "arrow.triangle.branch", .labelColor)
        case ".gitlab-ci.yml", ".gitlab-ci_gitlab.yml", ".gitlab-ci_release.yml": ("gitlab", "arrow.triangle.branch", .labelColor)
        case "nginx.conf": ("nginx", "network", .labelColor)
        case "logrotate": (nil, "arrow.triangle.2.circlepath", .systemOrange)
        case "www.conf": ("php", "doc", .labelColor)
        case "supervisor", "crontab": ("settings", "gearshape", .labelColor)
        case "artisan": ("laravel", "hammer", .labelColor)
        case "firebase": ("firebase", "flame", .labelColor)
        case "supabase": ("supabase", "bolt", .labelColor)
        case "vercel": ("vercel", "triangle", .labelColor)
        case "netlify": ("netlify", "network", .labelColor)
        case "tf", "tfvars": ("terraform", "cube", .labelColor)
        case "php", "phtml": ("php", "doc", .labelColor)
        case "blade.php": ("laravel", "doc", .labelColor)
        case "rs": ("rust", "gearshape.fill", .labelColor)
        case "dart": ("dart", "doc", .labelColor)
        case "swift": ("swift", "swift", .labelColor)
        case "md", "markdown": ("markdown", "info.circle.fill", .labelColor)
        case "json": ("json", "curlybraces", .labelColor)
        case "toml": ("toml", "slider.horizontal.3", .labelColor)
        case "yml", "yaml": ("yaml", "slider.horizontal.3", .labelColor)
        case "ini", "cfg", "plist", "xcconfig": ("settings", "slider.horizontal.3", .labelColor)
        case "sh", "zsh", "bash", "fish": (nil, "terminal", .systemGreen)
        case "jsx": ("react", "r.square.fill", .labelColor)
        case "tsx": ("react_ts", "r.square.fill", .labelColor)
        case "js", "mjs", "cjs": ("javascript", "j.square.fill", .labelColor)
        case "ts", "mts", "cts": ("typescript", "t.square.fill", .labelColor)
        case "html", "htm": ("html", "chevron.left.forwardslash.chevron.right", .labelColor)
        case "xml": ("xml", "chevron.left.forwardslash.chevron.right", .labelColor)
        case "css": ("css", "number", .labelColor)
        case "scss", "sass", "less": ("sass", "number", .labelColor)
        case "py": ("python", "p.square.fill", .labelColor)
        case "go": ("go", "doc", .labelColor)
        case "vue": ("vue", "v.square.fill", .labelColor)
        case "svelte": ("svelte", "s.square.fill", .labelColor)
        case "prisma": ("prisma", "cylinder", .labelColor)
        case "c", "h", "cpp", "hpp", "m", "mm": (nil, "c.square.fill", .systemIndigo)
        case "png", "jpg", "jpeg", "gif", "svg", "webp", "icns": ("image", "photo", .labelColor)
        case ".git": (nil, "arrow.triangle.branch", .systemRed)
        case "txt", "log": (nil, "doc.text", .secondaryLabelColor)
        default: (nil, "doc", .secondaryLabelColor)
        }
    }
}
