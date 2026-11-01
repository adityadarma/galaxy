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
    private let title = NSTextField(labelWithString: "References")
    private let count = NSTextField(labelWithString: "0 references")
    private let previewTitle = NSTextField(labelWithString: "")
    private let preview = ReferencesPreviewView()
    private let previewRuler = ReferencesLineNumberRuler()
    private let previewScroll = NSScrollView()
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

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 28
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(clicked(_:))
        table.doubleAction = #selector(doubleClicked(_:))
        table.onEscape = { [weak self] in self?.dismiss() }
        table.onOpen = { [weak self] in
            guard let self, let location = self.selectedLocation() else { return }
            self.onSelect?(location)
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
        split.addArrangedSubview(previewScroll)
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
            previewScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var acceptsFirstResponder: Bool { true }
    var titleText: String { title.stringValue }
    var statusText: String { count.stringValue }
    var statusDetail: String { preview.plainText }
    var previewHorizontalOffset: CGFloat { previewScroll.contentView.bounds.origin.x }
    var previewGutterWidth: CGFloat { previewRuler.ruleThickness }
    var previewContentView: NSView { preview }
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
        itemName: String = "reference"
    ) {
        guard locations != shownLocations || panelTitle != shownTitle else { return }
        shownLocations = locations
        shownTitle = panelTitle
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
        if let first = locations.first {
            select(first)
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
        count.stringValue = status
        previewTitle.stringValue = ""
        preview.leadingInset = previewRuler.ruleThickness
        preview.showStatus(detail)
        table.reloadData()
    }

    func showPreview(
        path: String,
        text: String,
        location: DefinitionLocation,
        styles: [StyleRun] = [],
        gutterWidth: CGFloat? = nil
    ) {
        previewTitle.stringValue = "\((path as NSString).lastPathComponent):\(location.line + 1):\(location.column + 1)"
        let (attributed, lineRanges) = attributedPreview(text: text, styles: styles)
        previewRuler.update(
            lineRanges: lineRanges,
            font: Theme.editorFont,
            width: gutterWidth
        )
        preview.leadingInset = previewRuler.ruleThickness
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
        onPreview?(location)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { return onClose?() ?? () }
        if event.keyCode == 36, let location = selectedLocation() { return onSelect?(location) ?? () }
        if event.keyCode == 125 { return moveSelection(by: 1) }
        if event.keyCode == 126 { return moveSelection(by: -1) }
        super.keyDown(with: event)
    }

    @objc private func clicked(_ sender: Any?) {
        guard rows.indices.contains(table.clickedRow) else { return }
        switch rows[table.clickedRow] {
        case let .file(group):
            if collapsed.contains(group.uri) { collapsed.remove(group.uri) } else { collapsed.insert(group.uri) }
            rebuildRows()
        case let .reference(location): onPreview?(location)
        }
    }

    @objc private func doubleClicked(_ sender: Any?) {
        if let location = selectedLocation() { onSelect?(location) }
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
    private let inset = NSSize(width: 10, height: 8)
    /// The clip view extends under the floating line-number ruler, so text
    /// starts after the gutter like in the editor.
    var leadingInset: CGFloat = 0
    private var lineHeight: CGFloat { ceil(NSLayoutManager().defaultLineHeight(for: Theme.editorFont)) }

    override var isFlipped: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = Theme.editorBackground.cgColor
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
            width: max(viewport.width, ceil(leadingInset + widest + inset.width * 2)),
            height: max(viewport.height, ceil(CGFloat(max(1, lineRanges.count)) * lineHeight + inset.height * 2))
        )
        updateVisibleLines()
    }

    func updateVisibleLines() {
        var viewport = visibleRect
        // A view outside a window can report an infinite or null visible rect.
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
            let field = NSTextField(labelWithString: "")
            field.isSelectable = false
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
            field.frame = NSRect(
                x: leadingInset + inset.width,
                y: y(forLine: line),
                width: max(0, frame.width - leadingInset - inset.width * 2),
                height: lineHeight
            )
        }
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
        let visible = preview.visibleRect
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
