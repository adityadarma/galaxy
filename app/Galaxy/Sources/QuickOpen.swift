import AppKit

/// Go to File (⌘P, M2-07): a VSCode-style quick-open panel floating over
/// the top of the window. Type to fuzzy-search the workspace's files;
/// ↑/↓ choose, Return opens, Esc closes. Matched characters are drawn in
/// the accent color.
///
/// With an empty field it also lists the other modes (`>` commands, `@`
/// symbols, `:` line, `%` text). Typing one of those characters first, or
/// clicking its row, hands over to that panel through `onMode`.
///
/// The panel only shows results. `MainWindowController` owns the
/// `FileFinder` and opens the chosen file.
@MainActor
final class QuickOpen: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let width: CGFloat = 620
    static let rowHeight: CGFloat = 26
    static let maxVisibleRows = 12
    static let resultLimit: UInt32 = 200
    /// Above this many files each keystroke searches off the main thread,
    /// so typing in the field never waits for the matcher.
    static let backgroundThreshold: UInt64 = 20_000

    let field = NSTextField()
    let table = NSTableView()
    let message = NSTextField(labelWithString: "")
    private let scroll = NSScrollView()
    private var heightConstraint: NSLayoutConstraint!

    private(set) var results: [FileMatch] = []
    var onOpen: ((FileMatch) -> Void)?
    var onClose: (() -> Void)?
    /// A mode prefix (`>`, `@`, `:`, `%`) was typed or clicked; the text
    /// is the whole field, prefix included.
    var onMode: ((String) -> Void)?
    static let modePrefixes: Set<Character> = [">", "@", ":", "%"]

    /// Shortcut text per command id, shown on the mode rows.
    var hintShortcuts: [String: String] = [:] {
        didSet { hints.shortcuts = hintShortcuts }
    }
    private let hints = QuickOpenHints()
    private var hintsHeight: NSLayoutConstraint!

    /// Set once the workspace has been indexed. A newer index (files
    /// added since) may replace it while the panel is open; the selected
    /// file stays selected.
    var finder: FileFinder? {
        didSet { if finder !== oldValue { runSearch(keepSelection: true) } }
    }
    /// Shown instead of results while there is no finder.
    var placeholderMessage = "Indexing files…" {
        didSet { if finder == nil { show([], keeping: nil) } }
    }

    /// Only the newest background search may update the list.
    private var generation = 0
    /// Called after results are shown (tests wait on it).
    var onResults: (() -> Void)?

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
        setAccessibilityLabel("Go to File")
        setAccessibilityIdentifier("quick-open")

        field.placeholderString = "Search files by name (append : to go to line or @ to go to symbol)"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("quick-open-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("file"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowSizeStyle = .custom
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .regular
        // The field keeps focus; clicks still select and open.
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("quick-open-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.setAccessibilityIdentifier("quick-open-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        hints.translatesAutoresizingMaskIntoConstraints = false
        hints.onPick = { [weak self] prefix in
            guard let self else { return }
            if prefix.isEmpty { self.focus() } else { self.onMode?(prefix) }
        }
        hintsHeight = hints.heightAnchor.constraint(equalToConstant: QuickOpenHints.collapsedHeight)

        for view in [field, hints, scroll, message] { addSubview(view) }
        heightConstraint = heightAnchor.constraint(equalToConstant: 48)
        NSLayoutConstraint.activate([
            heightConstraint,
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            hints.topAnchor.constraint(equalTo: field.bottomAnchor),
            hints.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            hints.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            hintsHeight,
            scroll.topAnchor.constraint(equalTo: hints.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
            message.topAnchor.constraint(equalTo: hints.bottomAnchor, constant: 2),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            message.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -14),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    var query: String { field.stringValue }
    var selectedIndex: Int? { table.selectedRow >= 0 ? table.selectedRow : nil }

    /// Clears the field and lists recent files.
    func reset() {
        field.stringValue = ""
        runSearch()
    }

    func focus() {
        window?.makeFirstResponder(field)
    }

    /// Types `text` as if the user did (tests).
    func setQuery(_ text: String) {
        field.stringValue = text
        runSearch()
    }

    // MARK: Searching

    /// Searches for the field's text. Typing moves the selection to the
    /// best match; `keepSelection` keeps the selected file if it is still
    /// listed.
    func runSearch(keepSelection: Bool = false) {
        generation += 1
        let keep = keepSelection ? selectedIndex.map { results[$0].relPath } : nil
        guard let finder else { return show([], keeping: nil) }
        let query = field.stringValue
        if finder.fileCount() < Self.backgroundThreshold {
            return show(finder.search(query: query, limit: Self.resultLimit), keeping: keep)
        }
        let token = generation
        let limit = Self.resultLimit
        Task.detached(priority: .userInitiated) { [weak self] in
            let found = finder.search(query: query, limit: limit)
            await MainActor.run {
                guard let self, self.generation == token else { return }
                self.show(found, keeping: keep)
            }
        }
    }

    private func show(_ found: [FileMatch], keeping keep: String?) {
        results = found
        table.reloadData()
        if !found.isEmpty {
            let row = keep.flatMap { path in found.firstIndex { $0.relPath == path } } ?? 0
            table.selectRowIndexes([row], byExtendingSelection: false)
            table.scrollRowToVisible(row)
        }
        if finder == nil {
            message.stringValue = placeholderMessage
        } else if found.isEmpty {
            message.stringValue = query.trimmingCharacters(in: .whitespaces).isEmpty
                ? "No files in this folder" : "No matching files"
        }
        message.isHidden = !found.isEmpty
        scroll.isHidden = found.isEmpty
        let showHints = query.isEmpty
        hints.isHidden = !showHints
        hintsHeight.constant = showHints ? QuickOpenHints.expandedHeight : QuickOpenHints.collapsedHeight
        let rows = min(found.count, Self.maxVisibleRows)
        heightConstraint.constant = found.isEmpty
            ? 70 + hintsHeight.constant
            : 8 + 28 + hintsHeight.constant + CGFloat(rows) * Self.rowHeight + 4
        setAccessibilityValue(found.isEmpty ? message.stringValue : "\(found.count) files")
        onResults?()
    }

    // MARK: Keys

    func controlTextDidChange(_ obj: Notification) {
        if let first = field.stringValue.first, Self.modePrefixes.contains(first) {
            return onMode?(field.stringValue) ?? runSearch()
        }
        runSearch()
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        // Focus moved elsewhere (a click outside, the window lost key).
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
        case #selector(NSResponder.insertNewline(_:)): openSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    /// Moves the selection, wrapping at both ends as VSCode does.
    func move(by delta: Int) {
        guard !results.isEmpty else { return }
        let current = selectedIndex ?? 0
        var next = current + delta
        if abs(delta) == 1 {
            next = (next + results.count) % results.count
        } else {
            next = min(max(next, 0), results.count - 1)
        }
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func openSelected() {
        guard let i = selectedIndex, results.indices.contains(i) else { return }
        onOpen?(results[i])
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0 else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        openSelected()
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: QuickOpenCell.reuseID, owner: self) as? QuickOpenCell
            ?? QuickOpenCell()
        cell.show(results[row])
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }
}

/// The other modes, listed under an empty field. Rows are clickable; the
/// keyboard selection stays on the file list.
@MainActor
final class QuickOpenHints: NSView {
    static let rowHeight: CGFloat = 24
    static let modes: [(title: String, prefix: String, command: String)] = [
        ("Go to File", "", "workbench.action.quickOpen"),
        ("Show and Run Commands", ">", "workbench.action.showCommands"),
        ("Search for Text", "%", "workbench.action.findInFiles"),
        ("Go to Symbol in Editor", "@", "workbench.action.gotoSymbol"),
        ("Go to Line", ":", "workbench.action.gotoLine"),
    ]
    static let collapsedHeight: CGFloat = 6
    static let expandedHeight: CGFloat = 6 + CGFloat(modes.count) * rowHeight + 6

    var onPick: ((String) -> Void)?
    var shortcuts: [String: String] = [:] { didSet { rebuild() } }
    private let stack = NSStackView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        stack.orientation = .vertical
        stack.spacing = 0
        stack.alignment = .leading
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
        setAccessibilityIdentifier("quick-open-modes")
        rebuild()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    private func rebuild() {
        for view in stack.arrangedSubviews { stack.removeArrangedSubview(view); view.removeFromSuperview() }
        for mode in Self.modes {
            let row = QuickOpenHintRow(title: mode.title, prefix: mode.prefix, shortcut: shortcuts[mode.command])
            row.onPick = { [weak self] in self?.onPick?(mode.prefix) }
            row.translatesAutoresizingMaskIntoConstraints = false
            stack.addArrangedSubview(row)
            row.leadingAnchor.constraint(equalTo: stack.leadingAnchor).isActive = true
            row.trailingAnchor.constraint(equalTo: stack.trailingAnchor).isActive = true
            row.heightAnchor.constraint(equalToConstant: Self.rowHeight).isActive = true
        }
    }
}

@MainActor
final class QuickOpenHintRow: NSView {
    var onPick: (() -> Void)?
    private var hovering = false { didSet { needsDisplay = true } }

    init(title: String, prefix: String, shortcut: String?) {
        super.init(frame: .zero)
        let name = NSTextField(labelWithString: title)
        name.font = .systemFont(ofSize: 13)
        let tag = NSTextField(labelWithString: prefix)
        tag.font = .systemFont(ofSize: 12)
        tag.textColor = .secondaryLabelColor
        tag.isHidden = prefix.isEmpty
        let keys = NSTextField(labelWithString: shortcut ?? "")
        keys.font = .systemFont(ofSize: 12)
        keys.textColor = .secondaryLabelColor
        keys.isHidden = shortcut == nil
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [name, tag, spacer, keys])
        row.spacing = 8
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(title)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
    override func mouseDown(with event: NSEvent) { onPick?() }

    override func draw(_ dirtyRect: NSRect) {
        guard hovering else { return }
        Theme.accent.withAlphaComponent(0.12).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 2, dy: 0), xRadius: 5, yRadius: 5).fill()
    }
}

/// File-type glyph, file name, then its folder, dimmed. Matched characters
/// are accent-colored in both.
@MainActor
final class QuickOpenCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("quick-open-cell")

    let icon = NSImageView()
    let name = NSTextField(labelWithString: "")
    let folder = NSTextField(labelWithString: "")
    let recentBadge = NSTextField(labelWithString: "recently opened")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseID
        icon.imageScaling = .scaleNone
        icon.setAccessibilityElement(false)
        name.lineBreakMode = .byTruncatingTail
        folder.lineBreakMode = .byTruncatingHead
        folder.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        recentBadge.font = .systemFont(ofSize: 11)
        recentBadge.textColor = .tertiaryLabelColor
        recentBadge.setContentCompressionResistancePriority(.required, for: .horizontal)
        textField = name

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [icon, name, folder, spacer, recentBadge])
        row.spacing = 8
        row.setCustomSpacing(6, after: icon)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 16),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ match: FileMatch) {
        let path = match.relPath as NSString
        let split = Int(match.nameStart)
        let fileName = path.substring(from: split)
        // Folder without the trailing slash.
        let dir = split > 0 ? path.substring(to: split - 1) : ""
        name.attributedStringValue = Self.highlighted(
            fileName, offset: split, match.highlights, font: .systemFont(ofSize: 13), color: .labelColor)
        folder.attributedStringValue = Self.highlighted(
            dir, offset: 0, match.highlights, font: .systemFont(ofSize: 11.5), color: .secondaryLabelColor)
        folder.isHidden = dir.isEmpty
        recentBadge.isHidden = !match.isRecent
        let glyph = FileGlyph.for(name: fileName)
        icon.image = glyph.image
        icon.contentTintColor = glyph.color
        setAccessibilityLabel(dir.isEmpty ? fileName : "\(fileName), \(dir)")
    }

    /// `text` is the slice of the path starting at UTF-16 `offset`;
    /// `highlights` are in whole-path coordinates.
    static func highlighted(_ text: String, offset: Int, _ highlights: [Utf16Range],
                            font: NSFont, color: NSColor) -> NSAttributedString {
        let out = NSMutableAttributedString(string: text, attributes: [.font: font, .foregroundColor: color])
        let bold = NSFontManager.shared.convert(font, toHaveTrait: .boldFontMask)
        let length = (text as NSString).length
        for h in highlights {
            let start = Int(h.location) - offset
            let range = NSIntersectionRange(NSRange(location: start, length: Int(h.length)),
                                            NSRange(location: 0, length: length))
            guard range.length > 0 else { continue }
            out.addAttributes([.foregroundColor: Theme.accent, .font: bold], range: range)
        }
        return out
    }
}

/// Selection always looks focused: the field, not the table, has focus.
@MainActor
final class QuickOpenRowView: NSTableRowView {
    static let reuseID = NSUserInterfaceItemIdentifier("quick-open-row")

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseID
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func drawSelection(in dirtyRect: NSRect) {
        Theme.accent.withAlphaComponent(0.18).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 2, dy: 1), xRadius: 5, yRadius: 5).fill()
    }
}
