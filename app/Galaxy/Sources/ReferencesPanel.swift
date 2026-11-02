import AppKit

/// Inline References Peek: source preview on the left and references grouped
/// by file on the right. A single click previews; a double click navigates.
@MainActor
final class ReferencesPanel: CardView, NSTableViewDataSource, NSTableViewDelegate {
    private enum Row {
        case file(FileGroup)
        case reference(DefinitionLocation)
    }

    private struct FileGroup {
        let uri: String
        let name: String
        let directory: String
        let locations: [DefinitionLocation]
    }

    var onPreview: ((DefinitionLocation) -> Void)?
    var onSelect: ((DefinitionLocation) -> Void)?
    var onClose: (() -> Void)?

    private var rows: [Row] = []
    private var groups: [FileGroup] = []
    private var collapsed: Set<String> = []
    private var snippets: [String: String] = [:]
    private var shownLocations: [DefinitionLocation] = []
    private var shownTitle = ""
    private var previewedLocation: DefinitionLocation?
    private let title = NSTextField(labelWithString: "References")
    private let count = NSTextField(labelWithString: "0 references")
    private let previewTitle = NSTextField(labelWithString: "")
    private let preview = ReferencesPreviewView()
    private let previewRuler = ReferencesLineNumberRuler()
    private let previewScroll = NSScrollView()
    /// Holds the preview scroll view with the sticky scope rows above it.
    private let previewContainer = NSView()
    private let previewSticky = ReferencesStickyView()
    private let resultsScroll = NSScrollView()
    private let table = ReferencesTableView()
    private let closeButton = NSButton()
    private let split = NSSplitView()
    private var positionedDivider = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.systemBlue.withAlphaComponent(0.7).cgColor
        layer?.borderWidth = 1
        layer?.cornerRadius = 0
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("References Peek")
        setAccessibilityIdentifier("references-peek")

        title.font = .systemFont(ofSize: 13, weight: .semibold)
        count.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        count.textColor = .secondaryLabelColor
        previewTitle.font = .systemFont(ofSize: 11, weight: .medium)
        previewTitle.textColor = .secondaryLabelColor
        previewTitle.lineBreakMode = .byTruncatingMiddle

        closeButton.image = NSImage(systemSymbolName: "xmark", accessibilityDescription: "Close References Peek")
        closeButton.isBordered = false
        closeButton.target = self
        closeButton.action = #selector(close(_:))

        let header = NSStackView(views: [title, count, NSView(), previewTitle, closeButton])
        header.spacing = 8
        header.translatesAutoresizingMaskIntoConstraints = false

        preview.frame = NSRect(x: 0, y: 0, width: 640, height: 280)
        previewScroll.documentView = preview
        previewScroll.contentView.copiesOnScroll = false
        previewScroll.drawsBackground = false
        previewScroll.hasVerticalScroller = true
        previewScroll.hasHorizontalScroller = true
        previewScroll.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        previewRuler.clientView = preview
        previewScroll.verticalRulerView = previewRuler
        previewScroll.hasVerticalRuler = true
        previewScroll.rulersVisible = true
        previewScroll.translatesAutoresizingMaskIntoConstraints = false
        previewSticky.translatesAutoresizingMaskIntoConstraints = false
        previewSticky.scrollView = previewScroll
        previewSticky.onSelect = { [weak self] line, pinned in self?.scrollPreview(toLine: line, above: pinned) }
        preview.onViewportChange = { [weak self] in self?.refreshSticky() }
        previewContainer.translatesAutoresizingMaskIntoConstraints = false
        previewContainer.addSubview(previewScroll)
        previewContainer.addSubview(previewSticky)
        NSLayoutConstraint.activate([
            previewScroll.topAnchor.constraint(equalTo: previewContainer.topAnchor),
            previewScroll.bottomAnchor.constraint(equalTo: previewContainer.bottomAnchor),
            previewScroll.leadingAnchor.constraint(equalTo: previewContainer.leadingAnchor),
            previewScroll.trailingAnchor.constraint(equalTo: previewContainer.trailingAnchor),
            previewSticky.topAnchor.constraint(equalTo: previewContainer.topAnchor),
            previewSticky.leadingAnchor.constraint(equalTo: previewContainer.leadingAnchor),
            previewSticky.trailingAnchor.constraint(equalTo: previewContainer.trailingAnchor),
        ])

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 28
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.dataSource = self
        table.delegate = self
        table.onEscape = { [weak self] in self?.dismiss() }
        table.onOpen = { [weak self] in
            guard let self, let location = self.selectedLocation() else { return }
            self.onSelect?(location)
        }
        table.onClick = { [weak self] row, clickCount in
            self?.handleClick(row: row, clickCount: clickCount)
        }
        table.setAccessibilityIdentifier("references-results")
        resultsScroll.documentView = table
        resultsScroll.drawsBackground = false
        resultsScroll.hasVerticalScroller = true
        resultsScroll.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        resultsScroll.translatesAutoresizingMaskIntoConstraints = false

        split.isVertical = true
        split.dividerStyle = .thin
        split.translatesAutoresizingMaskIntoConstraints = false
        split.addArrangedSubview(previewContainer)
        split.addArrangedSubview(resultsScroll)

        addSubview(header)
        addSubview(split)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            header.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            header.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            header.heightAnchor.constraint(equalToConstant: 24),
            split.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 4),
            split.leadingAnchor.constraint(equalTo: leadingAnchor),
            split.trailingAnchor.constraint(equalTo: trailingAnchor),
            split.bottomAnchor.constraint(equalTo: bottomAnchor),
            resultsScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 240),
            previewContainer.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var acceptsFirstResponder: Bool { true }
    var titleText: String { title.stringValue }
    var statusText: String { count.stringValue }
    var statusDetail: String { preview.plainText }
    var previewHasSyntaxHighlighting: Bool { preview.hasSyntaxHighlighting }
    var previewHorizontalOffset: CGFloat { previewScroll.contentView.bounds.origin.x }
    var previewGutterWidth: CGFloat { previewRuler.ruleThickness }
    var previewContentView: NSView { preview }
    /// Source lines currently pinned over the top of the preview.
    var previewStickyLines: [Int] { previewSticky.rows.map(\.line) }
    var resultsTableView: NSTableView { table }
    var resultsPaneFraction: CGFloat {
        guard split.bounds.width > 0 else { return 0 }
        return resultsScroll.frame.width / split.bounds.width
    }

    override func layout() {
        super.layout()
        guard !positionedDivider, split.arrangedSubviews.count == 2, split.bounds.width > 0 else { return }
        split.layoutSubtreeIfNeeded()
        split.setPosition(split.bounds.width * (2.0 / 3.0), ofDividerAt: 0)
        split.layoutSubtreeIfNeeded()
        positionedDivider = true
        preview.updateVisibleLines()
    }

    func update(
        _ locations: [DefinitionLocation],
        workspaceRoot: String?,
        title panelTitle: String = "References",
        itemName: String = "reference",
        preferredLocation: DefinitionLocation? = nil
    ) {
        guard locations != shownLocations || panelTitle != shownTitle else { return }
        shownLocations = locations
        shownTitle = panelTitle
        previewedLocation = nil
        title.stringValue = panelTitle
        snippets = [:]
        let grouped = Dictionary(grouping: locations, by: \.uri)
        groups = grouped.map { uri, locations in
            let path = URL(string: uri)?.path ?? uri.replacingOccurrences(of: "file://", with: "")
            let url = URL(fileURLWithPath: path)
            return FileGroup(
                uri: uri,
                name: url.lastPathComponent,
                directory: ProblemsPanel.displayDirectory(for: path, workspaceRoot: workspaceRoot),
                locations: locations.sorted { ($0.line, $0.column) < ($1.line, $1.column) }
            )
        }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        count.stringValue = "(\(locations.count))"
        count.toolTip = "\(locations.count) \(itemName)\(locations.count == 1 ? "" : "s")"
        rebuildRows()
        if let first = preferredLocation ?? locations.first {
            select(first)
            preview(first)
        }
    }

    func showStatus(_ status: String, detail: String) {
        guard count.stringValue != status || preview.plainText != detail else { return }
        shownLocations = []
        shownTitle = "References"
        title.stringValue = "References"
        groups = []
        rows = []
        snippets = [:]
        previewedLocation = nil
        count.stringValue = status
        previewTitle.stringValue = ""
        preview.leadingInset = 0
        preview.scopeProvider = nil
        preview.showStatus(detail)
        table.reloadData()
    }

    func showPreview(
        path: String,
        text: String,
        location: DefinitionLocation,
        styles: [StyleRun] = [],
        gutterWidth: CGFloat? = nil,
        stickyScopes: ((Int) -> [Int])? = nil
    ) {
        previewTitle.stringValue = "\((path as NSString).lastPathComponent):\(location.line + 1):\(location.column + 1)"
        let (attributed, lineRanges) = attributedPreview(text: text, styles: styles)
        previewRuler.update(
            lineRanges: lineRanges,
            font: Theme.editorFont,
            width: gutterWidth
        )
        preview.leadingInset = 0
        preview.scopeProvider = stickyScopes
        let range = lineRanges.indices.contains(Int(location.line))
            ? lineRanges[Int(location.line)]
            : NSRange(location: 0, length: 0)
        if range.length > 0 {
            attributed.addAttribute(.backgroundColor, value: NSColor.selectedTextBackgroundColor.withAlphaComponent(0.22), range: range)
        }
        preview.update(attributed: attributed, lineRanges: lineRanges, selectedLine: Int(location.line))
        let clipView = previewScroll.contentView
        let centeredY = max(0, preview.y(forLine: Int(location.line)) - clipView.bounds.height / 2)
        clipView.scroll(to: NSPoint(x: 0, y: centeredY))
        previewScroll.reflectScrolledClipView(clipView)
        preview.updateVisibleLines()
        if let group = groups.first(where: { URL(string: $0.uri)?.path == path }) {
            for reference in group.locations {
                let line = Self.lineRange(in: text, line: Int(reference.line))
                guard line.length > 0 else { continue }
                let source = (text as NSString).substring(with: line)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                snippets[Self.key(reference)] = source
            }
            table.reloadData()
        }
    }

    func setSnippets(_ values: [(DefinitionLocation, String)]) {
        for (location, source) in values {
            snippets[Self.key(location)] = source.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        table.reloadData()
    }

    private func refreshSticky() {
        let rows = preview.pinnedScopeLines().compactMap { line in
            preview.lineAttributed(line).map { ReferencesStickyView.Row(line: line, text: $0) }
        }
        previewSticky.show(rows, rowHeight: preview.rowHeight, gutterWidth: previewRuler.ruleThickness)
    }

    /// A click on a pinned row: scroll its line to just below the pinned rows
    /// that still enclose it.
    private func scrollPreview(toLine line: Int, above pinned: Int) {
        let clip = previewScroll.contentView
        let y = preview.y(forLine: line) - CGFloat(pinned) * preview.rowHeight
        let target = NSRect(origin: NSPoint(x: clip.bounds.origin.x, y: y), size: clip.bounds.size)
        clip.scroll(to: clip.constrainBoundsRect(target).origin)
        previewScroll.reflectScrolledClipView(clip)
    }

    private func attributedPreview(text: String, styles: [StyleRun]) -> (NSMutableAttributedString, [NSRange]) {
        let source = text as NSString
        let lineCount = max(1, text.reduce(1) { $1 == "\n" ? $0 + 1 : $0 })
        let result = NSMutableAttributedString()
        var ranges: [NSRange] = []
        var sourceStart = 0
        var lineNumber = 0
        let base = Theme.editorForeground.resolved(for: effectiveAppearance)
        while sourceStart < source.length || (source.length == 0 && lineNumber == 0) {
            let sourceRange = source.lineRange(for: NSRange(location: sourceStart, length: 0))
            let lineText = source.substring(with: sourceRange)
            let outputStart = result.length
            result.append(NSAttributedString(string: lineText, attributes: [
                .font: Theme.editorFont,
                .foregroundColor: base,
            ]))
            let textLength = (lineText as NSString).length
            for style in styles where style.line == UInt64(lineNumber) {
                let start = min(Int(style.start), textLength)
                let end = min(Int(style.end), textLength)
                guard start < end else { continue }
                result.addAttribute(
                    .foregroundColor,
                    value: Theme.syntax(style.scope, appearance: effectiveAppearance),
                    range: NSRange(location: outputStart + start, length: end - start)
                )
            }
            ranges.append(NSRange(location: outputStart, length: result.length - outputStart))
            sourceStart = NSMaxRange(sourceRange)
            lineNumber += 1
            if sourceStart >= source.length { break }
        }
        return (result, ranges)
    }

    static func lineRange(in text: String, line target: Int) -> NSRange {
        let string = text as NSString
        var line = 0
        var start = 0
        while line < target, start < string.length {
            let range = string.lineRange(for: NSRange(location: start, length: 0))
            start = NSMaxRange(range)
            line += 1
        }
        guard line == target, start <= string.length else { return NSRange(location: 0, length: 0) }
        return string.lineRange(for: NSRange(location: start, length: 0))
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard rows.indices.contains(row) else { return 28 }
        if case .file = rows[row] { return 30 }
        return 28
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        switch rows[row] {
        case let .file(group):
            let identifier = NSUserInterfaceItemIdentifier("reference-file-cell")
            let cell = tableView.makeView(withIdentifier: identifier, owner: self)
                as? ReferencesFileCell ?? ReferencesFileCell()
            cell.identifier = identifier
            cell.update(
                expanded: !collapsed.contains(group.uri),
                name: group.name,
                directory: group.directory,
                count: group.locations.count
            )
            return cell
        case let .reference(location):
            let identifier = NSUserInterfaceItemIdentifier("reference-result-cell")
            let cell = tableView.makeView(withIdentifier: identifier, owner: self)
                as? ReferencesResultCell ?? ReferencesResultCell()
            cell.identifier = identifier
            cell.update(
                location: "\(location.line + 1):\(location.column + 1)",
                source: snippets[Self.key(location)] ?? ""
            )
            return cell
        }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard notification.object as? NSTableView === table,
              let location = selectedLocation() else { return }
        preview(location)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { return onClose?() ?? () }
        if event.keyCode == 36, let location = selectedLocation() { return onSelect?(location) ?? () }
        if event.keyCode == 125 { return moveSelection(by: 1) }
        if event.keyCode == 126 { return moveSelection(by: -1) }
        super.keyDown(with: event)
    }

    func handleClick(row: Int, clickCount: Int) {
        guard rows.indices.contains(row) else { return }
        switch rows[row] {
        case let .file(group):
            guard clickCount == 1 else { return }
            if collapsed.contains(group.uri) { collapsed.remove(group.uri) } else { collapsed.insert(group.uri) }
            rebuildRows()
        case let .reference(location):
            if clickCount == 1 {
                preview(location)
            } else if clickCount == 2 {
                onSelect?(location)
            }
        }
    }

    private func preview(_ location: DefinitionLocation) {
        guard previewedLocation != location else { return }
        previewedLocation = location
        onPreview?(location)
    }

    @objc private func close(_ sender: Any?) { dismiss() }

    func dismiss() { onClose?() }

    private func selectedLocation() -> DefinitionLocation? {
        guard rows.indices.contains(table.selectedRow), case let .reference(location) = rows[table.selectedRow] else { return nil }
        return location
    }

    private func select(_ location: DefinitionLocation) {
        if let row = rows.firstIndex(where: {
            guard case let .reference(candidate) = $0 else { return false }
            return candidate == location
        }) {
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            table.scrollRowToVisible(row)
        }
    }

    private func moveSelection(by delta: Int) {
        guard !rows.isEmpty else { return }
        var row = min(max(table.selectedRow + delta, 0), rows.count - 1)
        while rows.indices.contains(row) {
            if case .reference = rows[row] {
                table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                table.scrollRowToVisible(row)
                return
            }
            let next = row + delta
            guard rows.indices.contains(next) else { return }
            row = next
        }
    }

    private func rebuildRows() {
        rows = groups.flatMap { group in
            collapsed.contains(group.uri) ? [.file(group)] : [.file(group)] + group.locations.map(Row.reference)
        }
        table.reloadData()
    }

    private static func key(_ location: DefinitionLocation) -> String {
        "\(location.uri)\u{1f}\(location.line)\u{1f}\(location.column)"
    }
}

/// Fixed columns keep snippets aligned even when line numbers have different
/// digit counts. Truncating the tail preserves the symbol near the line start.
@MainActor
final class ReferencesResultCell: NSTableCellView {
    let locationLabel = NSTextField(labelWithString: "")
    let sourceLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        for label in [locationLabel, sourceLabel] {
            label.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            label.lineBreakMode = .byTruncatingTail
            label.maximumNumberOfLines = 1
            addSubview(label)
        }
        locationLabel.alignment = .right
        locationLabel.textColor = .secondaryLabelColor
        sourceLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(location: String, source: String) {
        locationLabel.stringValue = location
        sourceLabel.stringValue = source
        locationLabel.toolTip = location
        sourceLabel.toolTip = source.isEmpty ? nil : source
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let locationWidth: CGFloat = 58
        let sourceX: CGFloat = 84
        locationLabel.frame = NSRect(x: 12, y: 5, width: locationWidth, height: 18)
        sourceLabel.frame = NSRect(x: sourceX, y: 5, width: max(0, bounds.width - sourceX - 10), height: 18)
    }
}

/// Header columns reserve count and disclosure space, then give remaining
/// width to the directory rather than truncating the whole header as one line.
@MainActor
final class ReferencesFileCell: NSTableCellView {
    let disclosureLabel = NSTextField(labelWithString: "")
    let nameLabel = NSTextField(labelWithString: "")
    let directoryLabel = NSTextField(labelWithString: "")
    let countLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        for label in [disclosureLabel, nameLabel, directoryLabel, countLabel] {
            label.maximumNumberOfLines = 1
            addSubview(label)
        }
        disclosureLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        nameLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        nameLabel.lineBreakMode = .byTruncatingMiddle
        directoryLabel.font = .systemFont(ofSize: 12, weight: .medium)
        directoryLabel.textColor = .secondaryLabelColor
        directoryLabel.lineBreakMode = .byTruncatingMiddle
        countLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .semibold)
        countLabel.alignment = .right
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(expanded: Bool, name: String, directory: String, count: Int) {
        disclosureLabel.stringValue = expanded ? "▾" : "▸"
        nameLabel.stringValue = name
        directoryLabel.stringValue = directory
        countLabel.stringValue = String(count)
        nameLabel.toolTip = name
        directoryLabel.toolTip = directory
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let countWidth: CGFloat = 24
        let contentWidth = max(0, bounds.width - 12 - 16 - countWidth - 10)
        let measuredName = ceil(nameLabel.intrinsicContentSize.width)
        let nameWidth = min(measuredName, max(72, contentWidth * 0.55))
        let nameX: CGFloat = 30
        let directoryX = nameX + nameWidth + 10
        let countX = max(directoryX, bounds.width - countWidth - 10)
        disclosureLabel.frame = NSRect(x: 12, y: 5, width: 14, height: 20)
        nameLabel.frame = NSRect(x: nameX, y: 5, width: nameWidth, height: 20)
        directoryLabel.frame = NSRect(
            x: directoryX,
            y: 5,
            width: max(0, countX - directoryX - 8),
            height: 20
        )
        countLabel.frame = NSRect(x: countX, y: 5, width: countWidth, height: 20)
    }
}

@MainActor
private final class ReferencesTableView: NSTableView {
    var onEscape: (() -> Void)?
    var onOpen: (() -> Void)?
    var onClick: ((Int, Int) -> Void)?

    override func mouseDown(with event: NSEvent) {
        let clicked = row(at: convert(event.locationInWindow, from: nil))
        super.mouseDown(with: event)
        onClick?(clicked, event.clickCount)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { return onEscape?() ?? () }
        if event.keyCode == 36 { return onOpen?() ?? () }
        super.keyDown(with: event)
    }
}

@MainActor
private final class ReferencesPreviewView: NSView {
    private(set) var plainText = ""
    private var attributed = NSAttributedString()
    private var lineRanges: [NSRange] = []
    private var selectedLine: Int?
    private var lineFields: [NSTextField] = []
    private let whitespaceMarkers = ReferencesWhitespaceMarkers()
    private let inset = NSSize(width: 10, height: 8)
    /// The scroll view places the document view beside its line-number ruler.
    var leadingInset: CGFloat = 0
    /// Header lines of the blocks around a line, outermost first.
    var scopeProvider: ((Int) -> [Int])?
    /// Called after the visible lines changed: scrolled, resized or reloaded.
    var onViewportChange: (() -> Void)?
    private static let stickyLimit = 3
    var rowHeight: CGFloat { lineHeight }
    private var lineHeight: CGFloat { ceil(NSLayoutManager().defaultLineHeight(for: Theme.editorFont)) }
    var hasSyntaxHighlighting: Bool {
        let base = Theme.editorForeground.resolved(for: effectiveAppearance)
        var highlighted = false
        attributed.enumerateAttribute(
            .foregroundColor,
            in: NSRange(location: 0, length: attributed.length)
        ) { value, _, stop in
            if let color = value as? NSColor, !color.isEqual(base) {
                highlighted = true
                stop.pointee = true
            }
        }
        return highlighted
    }

    override var isFlipped: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = Theme.editorBackground.cgColor
        whitespaceMarkers.translatesAutoresizingMaskIntoConstraints = false
        whitespaceMarkers.isHidden = true
        addSubview(whitespaceMarkers)
        NSLayoutConstraint.activate([
            whitespaceMarkers.leadingAnchor.constraint(equalTo: leadingAnchor),
            whitespaceMarkers.trailingAnchor.constraint(equalTo: trailingAnchor),
            whitespaceMarkers.topAnchor.constraint(equalTo: topAnchor),
            whitespaceMarkers.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        NotificationCenter.default.removeObserver(self)
        guard let clipView = enclosingScrollView?.contentView else { return }
        clipView.postsBoundsChangedNotifications = true
        clipView.postsFrameChangedNotifications = true
        // Scrolling changes the clip bounds; autolayout resizing the peek
        // changes its frame. Both reveal lines that need fields.
        for name in [NSView.boundsDidChangeNotification, NSView.frameDidChangeNotification] {
            NotificationCenter.default.addObserver(
                self,
                selector: #selector(clipBoundsChanged(_:)),
                name: name,
                object: clipView
            )
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateVisibleLines()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        updateVisibleLines()
    }

    @objc private func clipBoundsChanged(_ notification: Notification) {
        updateVisibleLines()
    }

    func showStatus(_ text: String) {
        let attributed = NSAttributedString(string: text, attributes: [
            .font: Theme.editorFont,
            .foregroundColor: Theme.editorForeground.resolved(for: effectiveAppearance),
        ])
        update(
            attributed: attributed,
            lineRanges: [NSRange(location: 0, length: attributed.length)],
            selectedLine: nil
        )
    }

    func update(attributed: NSAttributedString, lineRanges: [NSRange], selectedLine: Int?) {
        self.attributed = attributed
        plainText = attributed.string
        self.lineRanges = lineRanges
        self.selectedLine = selectedLine
        var widest: CGFloat = 0
        for range in lineRanges {
            widest = max(widest, lineValue(range).size().width)
        }
        let viewport = enclosingScrollView?.contentSize ?? .zero
        frame.size = NSSize(
            width: max(viewport.width, ceil(leadingInset + widest + inset.width)),
            height: max(viewport.height, ceil(CGFloat(max(1, lineRanges.count)) * lineHeight + inset.height * 2))
        )
        updateVisibleLines()
    }

    func updateVisibleLines() {
        var viewport = enclosingScrollView?.contentView.bounds ?? visibleRect
        // A view outside a window can report an infinite or null viewport.
        if viewport.isNull || viewport.isInfinite || !viewport.minY.isFinite || !viewport.maxY.isFinite {
            viewport = .zero
        }
        if viewport.height < lineHeight, let clip = enclosingScrollView?.contentView, clip.bounds.height >= lineHeight {
            viewport = clip.bounds
        }
        if viewport.height < lineHeight {
            // Not laid out yet: render a screenful around the scroll origin
            // so the first pass is never blank.
            let origin = enclosingScrollView?.contentView.bounds.origin ?? .zero
            viewport = NSRect(x: origin.x, y: origin.y, width: max(frame.width, 1), height: 60 * lineHeight)
        }
        let first = max(0, Int(floor((viewport.minY - inset.height) / lineHeight)))
        let last = min(lineRanges.count, Int(ceil((viewport.maxY - inset.height) / lineHeight)) + 1)
        let visibleCount = max(0, last - first)
        while lineFields.count < visibleCount {
            let field = ReferencesLineField(frame: .zero)
            field.isEditable = false
            field.isSelectable = false
            field.isBordered = false
            field.isBezeled = false
            field.drawsBackground = false
            field.lineBreakMode = .byClipping
            field.maximumNumberOfLines = 1
            addSubview(field)
            lineFields.append(field)
        }
        for (offset, field) in lineFields.enumerated() {
            guard offset < visibleCount else {
                field.isHidden = true
                continue
            }
            let line = first + offset
            field.attributedStringValue = lineValue(lineRanges[line])
            field.isHidden = false
            field.drawsBackground = selectedLine == line
            field.backgroundColor = selectedLine == line
                ? NSColor.selectedTextBackgroundColor.withAlphaComponent(0.22)
                : .clear
            // Code starts at the ruler edge, as it does in the editor.
            field.frame = NSRect(
                x: leadingInset,
                y: y(forLine: line),
                width: max(0, frame.width - leadingInset),
                height: lineHeight
            )
        }
        let markerLines = first < last ? (first..<last).map { line in
                (lineValue(lineRanges[line]).string, y(forLine: line))
            } : []
        whitespaceMarkers.update(
            lines: markerLines,
            originX: leadingInset,
            lineHeight: lineHeight
        )
        onViewportChange?()
    }

    func lineAttributed(_ line: Int) -> NSAttributedString? {
        lineRanges.indices.contains(line) ? lineValue(lineRanges[line]) : nil
    }

    /// Scope headers to pin over the top row, with the editor's rule: with `n`
    /// rows pinned, the line `n` rows below the top edge sits in at least `n`
    /// blocks and each of the first `n` headers has reached its slot.
    func pinnedScopeLines() -> [Int] {
        guard let scopeProvider, let clip = enclosingScrollView?.contentView,
              lineHeight > 0, !lineRanges.isEmpty else { return [] }
        let topRow = (clip.bounds.origin.y - inset.height) / lineHeight
        guard topRow > 0 else { return [] }
        for count in stride(from: Self.stickyLimit, through: 1, by: -1) {
            let under = min(lineRanges.count - 1, max(0, Int(topRow.rounded(.down)) + count))
            let chain = scopeProvider(under)
            guard chain.count >= count else { continue }
            let headers = Array(chain.prefix(count))
            if headers.enumerated().allSatisfy({ CGFloat($0.element) <= topRow + CGFloat($0.offset) }) {
                return headers
            }
        }
        return []
    }

    func y(forLine line: Int) -> CGFloat {
        inset.height + CGFloat(max(0, line)) * lineHeight
    }

    private func lineValue(_ range: NSRange) -> NSAttributedString {
        let value = NSMutableAttributedString(attributedString: attributed.attributedSubstring(from: range))
        while value.length > 0 {
            let last = (value.string as NSString).character(at: value.length - 1)
            guard last == 10 || last == 13 else { break }
            value.deleteCharacters(in: NSRange(location: value.length - 1, length: 1))
        }
        return value
    }

}

/// A label whose text starts at the frame edge. `NSTextFieldCell` insets its
/// text by about 2 pt, which pushed the preview's code right of the editor's.
private final class ReferencesLineCell: NSTextFieldCell {
    override func drawingRect(forBounds rect: NSRect) -> NSRect { rect }
}

private final class ReferencesLineField: NSTextField {
    override class var cellClass: AnyClass? {
        get { ReferencesLineCell.self }
        set {}
    }
}

/// Sticky scroll for the preview: the header lines of the blocks around the
/// top visible line stay pinned, drawn like the editor draws its own.
@MainActor
private final class ReferencesStickyView: NSView {
    struct Row: Equatable {
        var line: Int
        var text: NSAttributedString
    }

    private(set) var rows: [Row] = []
    weak var scrollView: NSScrollView?
    /// A click on a row: its line, and how many pinned rows sit above it.
    var onSelect: ((Int, Int) -> Void)?
    private var rowHeight: CGFloat = 0
    private var gutterWidth: CGFloat = 0
    private var heightConstraint: NSLayoutConstraint!

    override var isFlipped: Bool { true }
    override var mouseDownCanMoveWindow: Bool { false }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        heightConstraint = heightAnchor.constraint(equalToConstant: 0)
        heightConstraint.isActive = true
        isHidden = true
        setAccessibilityIdentifier("references-sticky-scope")
        setAccessibilityRole(.group)
        layer?.masksToBounds = false
        layer?.shadowColor = NSColor.black.cgColor
        layer?.shadowOpacity = 0.28
        layer?.shadowRadius = 4
        layer?.shadowOffset = CGSize(width: 0, height: -2)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func layout() {
        super.layout()
        layer?.shadowPath = CGPath(rect: bounds, transform: nil)
    }

    func show(_ newRows: [Row], rowHeight: CGFloat, gutterWidth: CGFloat) {
        let height = CGFloat(newRows.count) * rowHeight
        if heightConstraint.constant != height {
            heightConstraint.constant = height
            needsDisplay = true
        }
        isHidden = newRows.isEmpty
        if self.rowHeight != rowHeight || self.gutterWidth != gutterWidth {
            self.rowHeight = rowHeight
            self.gutterWidth = gutterWidth
            needsDisplay = true
        }
        guard newRows != rows else { return }
        rows = newRows
        setAccessibilityLabel("Scope: " + newRows.map { $0.text.string.trimmingCharacters(in: .whitespacesAndNewlines) }
            .joined(separator: ", "))
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        guard rowHeight > 0, point.y >= 0 else { return }
        let index = Int(point.y / rowHeight)
        guard rows.indices.contains(index) else { return }
        onSelect?(rows[index].line, index)
    }

    /// The rows cover the preview; wheel events still belong to its scroll view.
    override func scrollWheel(with event: NSEvent) {
        if let scrollView { scrollView.scrollWheel(with: event) } else { super.scrollWheel(with: event) }
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
        let attributes: [NSAttributedString.Key: Any] = [
            .font: Theme.editorFont,
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
        for (index, row) in rows.enumerated() {
            let top = CGFloat(index) * rowHeight
            let label = "\(row.line + 1)" as NSString
            let size = label.size(withAttributes: attributes)
            label.draw(
                at: NSPoint(x: gutterWidth - EditorView.gutterTrailingPadding - size.width, y: top),
                withAttributes: attributes
            )
            NSGraphicsContext.saveGraphicsState()
            NSRect(x: gutterWidth, y: top, width: max(0, bounds.width - gutterWidth), height: rowHeight).clip()
            row.text.draw(at: NSPoint(x: gutterWidth, y: top))
            NSGraphicsContext.restoreGraphicsState()
        }
        NSColor.separatorColor.setFill()
        NSRect(x: gutterWidth - 1, y: 0, width: 1, height: bounds.height).fill()
    }
}

/// Mirrors the editor's non-destructive whitespace markers without changing
/// the attributed source used for syntax highlighting.
@MainActor
private final class ReferencesWhitespaceMarkers: NSView {
    private var lines: [(String, CGFloat)] = []
    private var originX: CGFloat = 0
    private var lineHeight: CGFloat = 0

    override var isFlipped: Bool { true }

    func update(lines: [(String, CGFloat)], originX: CGFloat, lineHeight: CGFloat) {
        self.lines = lines
        self.originX = originX
        self.lineHeight = lineHeight
        isHidden = lines.isEmpty
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        let font = Theme.editorFont
        let attributes: [NSAttributedString.Key: Any] = [.font: font]
        let radius = max(1, (font.pointSize / 13).rounded(.down)) * 0.9
        Theme.whitespace.setFill()
        for (text, y) in lines {
            let source = text as NSString
            for column in EditorView.whitespaceColumns(in: text, mode: .all) {
                let prefix = source.substring(to: column) as NSString
                let character = source.substring(with: NSRange(location: column, length: 1)) as NSString
                let start = originX + prefix.size(withAttributes: attributes).width
                let width = character.size(withAttributes: attributes).width
                let center = NSPoint(x: start + width / 2, y: y + lineHeight / 2)
                NSBezierPath(ovalIn: NSRect(
                    x: center.x - radius,
                    y: center.y - radius,
                    width: radius * 2,
                    height: radius * 2
                )).fill()
            }
        }
    }
}

@MainActor
private final class ReferencesLineNumberRuler: NSRulerView {
    private var lineRanges: [NSRange] = []
    private var numberFont = Theme.editorFont

    init() {
        super.init(scrollView: nil, orientation: .verticalRuler)
        // Since macOS 14 views do not clip by default. The ruler sits above
        // the clip view, so an unclipped background fill hides the source.
        clipsToBounds = true
        ruleThickness = 48
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(lineRanges: [NSRange], font: NSFont, width: CGFloat?) {
        self.lineRanges = lineRanges
        numberFont = font
        let digits = max(3, String(max(1, lineRanges.count)).count)
        let digitWidth = ("0" as NSString).size(withAttributes: [.font: font]).width
        ruleThickness = width ?? ceil(
            CGFloat(digits) * digitWidth
                + EditorView.gutterPadding
                + EditorView.gutterTrailingPadding
        )
        needsDisplay = true
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        let rect = rect.intersection(bounds)
        Theme.editorBackground.setFill()
        rect.fill()
        guard let preview = clientView as? ReferencesPreviewView else { return }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: numberFont,
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
        let visible = scrollView?.contentView.bounds ?? preview.visibleRect
        for (index, characterRange) in lineRanges.enumerated() {
            _ = characterRange
            let y = preview.y(forLine: index) - visible.minY
            guard y + numberFont.pointSize * 1.5 >= rect.minY, y <= rect.maxY else { continue }
            let label = "\(index + 1)" as NSString
            let size = label.size(withAttributes: attributes)
            label.draw(
                at: NSPoint(x: ruleThickness - EditorView.gutterTrailingPadding - size.width, y: y),
                withAttributes: attributes
            )
        }
        NSColor.separatorColor.setFill()
        NSRect(x: ruleThickness - 1, y: rect.minY, width: 1, height: rect.height).fill()
    }
}
