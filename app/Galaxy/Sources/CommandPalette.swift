import AppKit

/// Command palette (⇧⌘P, M2-09): every registered command, fuzzy searched
/// by the Rust `CommandPalette`. Styled like Go to File; each row shows
/// `Category: Title`, matched characters in the accent color, "recently
/// used", and the menu shortcut if the command has one.
///
/// Commands are run through the responder chain, starting from whatever
/// had focus before the palette opened, the same way a menu item would.
@MainActor
final class CommandPaletteView: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let width: CGFloat = 620
    static let rowHeight: CGFloat = 26
    static let maxVisibleRows = 12

    /// Command id to the action it sends. Ids without an entry are not
    /// listed (`testEveryCommandHasAnAction` keeps the two in step).
    static let actions: [String: Selector] = [
        "workbench.action.files.newUntitledFile": #selector(MainWindowController.newDocument(_:)),
        "workbench.action.files.openFileFolder": #selector(MainWindowController.openDocument(_:)),
        "workbench.action.files.openFolder": #selector(MainWindowController.openFolder(_:)),
        "workbench.action.files.save": #selector(MainWindowController.saveDocument(_:)),
        "workbench.action.files.saveAs": #selector(MainWindowController.saveDocumentAs(_:)),
        "workbench.action.closeActiveEditor": #selector(MainWindowController.closeTab(_:)),
        "workbench.action.closeWindow": #selector(NSWindow.performClose(_:)),
        "undo": #selector(EditorView.undoEdit(_:)),
        "redo": #selector(EditorView.redoEdit(_:)),
        "editor.action.clipboardCutAction": #selector(EditorView.cut(_:)),
        "editor.action.clipboardCopyAction": #selector(EditorView.copy(_:)),
        "editor.action.clipboardPasteAction": #selector(EditorView.paste(_:)),
        "editor.action.selectAll": #selector(EditorView.selectAll(_:)),
        "editor.action.commentLine": #selector(EditorView.toggleComment(_:)),
        "editor.action.addSelectionToNextFindMatch": #selector(EditorView.selectNextOccurrence(_:)),
        "editor.action.selectHighlights": #selector(EditorView.selectAllOccurrences(_:)),
        "editor.action.insertCursorAbove": #selector(EditorView.addCursorAbove(_:)),
        "editor.action.insertCursorBelow": #selector(EditorView.addCursorBelow(_:)),
        "actions.find": #selector(MainWindowController.showFind(_:)),
        "editor.action.startFindReplaceAction": #selector(MainWindowController.showFindAndReplace(_:)),
        "editor.action.nextMatchFindAction": #selector(MainWindowController.findNext(_:)),
        "editor.action.previousMatchFindAction": #selector(MainWindowController.findPrevious(_:)),
        "actions.findWithSelection": #selector(MainWindowController.useSelectionForFind(_:)),
        "workbench.action.quickOpen": #selector(MainWindowController.showGoToFile(_:)),
        "workbench.action.findInFiles": #selector(MainWindowController.showWorkspaceSearch(_:)),
        "workbench.action.gotoSymbol": #selector(MainWindowController.showGoToSymbol(_:)),
        "workbench.action.gotoWorkspaceSymbol": #selector(MainWindowController.showGoToWorkspaceSymbol(_:)),
        "workbench.action.openUserSettingsJson": #selector(MainWindowController.openUserSettingsJson(_:)),
        "workbench.action.openWorkspaceSettingsJson": #selector(MainWindowController.openWorkspaceSettingsJson(_:)),
        "workbench.action.gotoLine": #selector(MainWindowController.showGoToLine(_:)),
        "workbench.action.navigateBack": #selector(MainWindowController.navigateBack(_:)),
        "workbench.action.navigateForward": #selector(MainWindowController.navigateForward(_:)),
        "workbench.action.showCommands": #selector(MainWindowController.showCommandPalette(_:)),
        "workbench.action.openGlobalKeybindingsFile": #selector(MainWindowController.openKeybindingsFile(_:)),
        "workbench.action.toggleSidebarVisibility": #selector(MainWindowController.toggleSidebarVisibility(_:)),
         "workbench.action.togglePanel": #selector(MainWindowController.toggleTerminal(_:)),
         "workbench.action.splitEditorRight": #selector(MainWindowController.splitEditorRight(_:)),
         "workbench.action.splitEditorDown": #selector(MainWindowController.splitEditorDown(_:)),
         "workbench.action.closeEditorSplit": #selector(MainWindowController.closeEditorSplit(_:)),
        "workbench.view.explorer": #selector(MainWindowController.showExplorer(_:)),
        "workbench.action.nextEditor": #selector(MainWindowController.showNextTab(_:)),
        "workbench.action.previousEditor": #selector(MainWindowController.showPreviousTab(_:)),
        "workbench.action.openPreviousRecentlyUsedEditor": #selector(MainWindowController.selectLastUsedTab(_:)),
        "explorer.newFile": #selector(MainWindowController.explorerNewFile(_:)),
        "explorer.newFolder": #selector(MainWindowController.explorerNewFolder(_:)),
        "revealFileInOS": #selector(MainWindowController.revealInFinder(_:)),
        "copyFilePath": #selector(MainWindowController.copyExplorerPath(_:)),
        "copyRelativeFilePath": #selector(MainWindowController.copyExplorerRelativePath(_:)),
        "renameFile": #selector(MainWindowController.renameExplorerItem(_:)),
        "deleteFile": #selector(MainWindowController.deleteExplorerItem(_:)),
        "workbench.files.action.refreshFilesExplorer": #selector(MainWindowController.refreshExplorer(_:)),
        "workbench.files.action.collapseExplorerFolders": #selector(MainWindowController.collapseExplorerFolders(_:)),
        "galaxy.showTypingLatency": #selector(MainWindowController.showTypingLatency(_:)),
        "galaxy.about": #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
        "workbench.action.quit": #selector(NSApplication.terminate(_:)),
    ]

    let field = NSTextField()
    let table = NSTableView()
    let message = NSTextField(labelWithString: "No matching commands")
    private let scroll = NSScrollView()
    private var heightConstraint: NSLayoutConstraint!

    /// The Rust registry; set by the window controller.
    var palette: CommandPalette?
    /// Ids that can run right now, decided when the palette opens.
    private(set) var available: [String] = []
    /// Menu shortcut per command id, e.g. "⇧⌘S".
    private(set) var shortcuts: [String: String] = [:]
    private(set) var results: [CommandMatch] = []
    var onRun: ((CommandMatch) -> Void)?
    var onClose: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        shadow = NSShadow()
        layer?.cornerRadius = 8
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.28
        layer?.shadowRadius = 14
        layer?.shadowOffset = NSSize(width: 0, height: -4)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Command Palette")
        setAccessibilityIdentifier("command-palette")

        field.placeholderString = "Type the name of a command"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("command-palette-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("command"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowSizeStyle = .custom
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("command-palette-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.isHidden = true
        message.setAccessibilityIdentifier("command-palette-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        for view in [field, scroll, message] { addSubview(view) }
        heightConstraint = heightAnchor.constraint(equalToConstant: 76)
        NSLayoutConstraint.activate([
            heightConstraint,
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
            message.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 8),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    var selectedIndex: Int? { table.selectedRow >= 0 ? table.selectedRow : nil }

    /// Clears the field and lists the commands in `available`.
    func reset(available: [String], shortcuts: [String: String]) {
        self.available = available
        self.shortcuts = shortcuts
        field.stringValue = ""
        runSearch()
    }

    func focus() { window?.makeFirstResponder(field) }

    /// Types `text` as if the user did (tests).
    func setQuery(_ text: String) {
        field.stringValue = text
        runSearch()
    }

    /// 36 commands: searching on the main thread takes microseconds.
    func runSearch() {
        results = palette?.search(query: field.stringValue, available: available) ?? []
        table.reloadData()
        if !results.isEmpty {
            table.selectRowIndexes([0], byExtendingSelection: false)
            table.scrollRowToVisible(0)
        }
        message.isHidden = !results.isEmpty
        scroll.isHidden = results.isEmpty
        let rows = min(results.count, Self.maxVisibleRows)
        heightConstraint.constant = results.isEmpty ? 76 : 8 + 28 + 6 + CGFloat(rows) * Self.rowHeight + 4
        setAccessibilityValue(results.isEmpty ? message.stringValue : "\(results.count) commands")
    }

    // MARK: Keys

    func controlTextDidChange(_ obj: Notification) { runSearch() }

    func controlTextDidEndEditing(_ obj: Notification) {
        if window?.firstResponder !== field.currentEditor() { onClose?() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.pageUp(_:)), #selector(NSResponder.scrollPageUp(_:)):
            move(by: -Self.maxVisibleRows)
        case #selector(NSResponder.pageDown(_:)), #selector(NSResponder.scrollPageDown(_:)):
            move(by: Self.maxVisibleRows)
        case #selector(NSResponder.insertNewline(_:)): runSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    /// Wraps at both ends for single steps, as in Go to File.
    func move(by delta: Int) {
        guard !results.isEmpty else { return }
        var next = (selectedIndex ?? 0) + delta
        if abs(delta) == 1 {
            next = (next + results.count) % results.count
        } else {
            next = min(max(next, 0), results.count - 1)
        }
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func runSelected() {
        guard let i = selectedIndex, results.indices.contains(i) else { return NSSound.beep() }
        onRun?(results[i])
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0 else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        runSelected()
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: CommandPaletteCell.reuseID, owner: self) as? CommandPaletteCell
            ?? CommandPaletteCell()
        let match = results[row]
        cell.show(match, shortcut: shortcuts[match.id])
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }

    // MARK: Shortcuts

    /// Shortcut text for every menu item, keyed by its action.
    static func shortcuts(in menu: NSMenu?) -> [Selector: String] {
        var out: [Selector: String] = [:]
        func walk(_ menu: NSMenu) {
            for item in menu.items {
                if let sub = item.submenu { walk(sub) }
                guard let action = item.action, !item.keyEquivalent.isEmpty, out[action] == nil else { continue }
                out[action] = describe(key: item.keyEquivalent, modifiers: item.keyEquivalentModifierMask)
            }
        }
        if let menu { walk(menu) }
        return out
    }

    /// "⇧⌘S" style, modifiers in the order macOS menus use.
    static func describe(key: String, modifiers: NSEvent.ModifierFlags) -> String {
        var text = ""
        if modifiers.contains(.control) { text += "⌃" }
        if modifiers.contains(.option) { text += "⌥" }
        if modifiers.contains(.shift) { text += "⇧" }
        if modifiers.contains(.command) { text += "⌘" }
        let names: [String: String] = [
            "\t": "⇥", "\r": "↩", " ": "Space", "\u{1B}": "⎋",
            "\u{F700}": "↑", "\u{F701}": "↓", "\u{F702}": "←", "\u{F703}": "→",
        ]
        return text + (names[key] ?? key.uppercased())
    }
}

@MainActor
final class CommandPaletteCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("command-palette-cell")

    let label = NSTextField(labelWithString: "")
    let recentBadge = NSTextField(labelWithString: "recently used")
    let shortcut = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseID
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        recentBadge.font = .systemFont(ofSize: 11)
        recentBadge.textColor = .tertiaryLabelColor
        shortcut.font = .systemFont(ofSize: 12)
        shortcut.textColor = .secondaryLabelColor
        for view in [recentBadge, shortcut] {
            view.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        textField = label

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [label, spacer, recentBadge, shortcut])
        row.spacing = 10
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ match: CommandMatch, shortcut text: String?) {
        label.attributedStringValue = QuickOpenCell.highlighted(
            match.label, offset: 0, match.highlights, font: .systemFont(ofSize: 13), color: .labelColor)
        recentBadge.isHidden = !match.isRecent
        shortcut.stringValue = text ?? ""
        shortcut.isHidden = text == nil
        setAccessibilityLabel(text.map { "\(match.label), \($0)" } ?? match.label)
    }
}
