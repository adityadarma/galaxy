import AppKit

@MainActor
final class CompletionPopup: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onSelect: ((CompletionInfo) -> Void)?
    var onClose: (() -> Void)?

    static let width: CGFloat = 480
    private static let rowHeight: CGFloat = 26
    private static let verticalPadding: CGFloat = 4
    private static let maxRows = 9
    private static let maxResults = 300

    private var all: [CompletionInfo] = []
    private var results: [(item: CompletionInfo, matched: [Int])] = []
    private let table = NoFocusTableView()
    private var shownQuery: String?
    private let scroll = NSScrollView()
    private let clip = NSView()
    private var heightConstraint: NSLayoutConstraint?

    /// Size that fits the rows shown now: the list grows up to nine rows
    /// instead of leaving empty space under a short list.
    var preferredSize: NSSize {
        let rows = max(1, min(results.count, Self.maxRows))
        return NSSize(width: Self.width, height: CGFloat(rows) * Self.rowHeight + Self.verticalPadding * 2)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        // The shadow lives on this layer, the rounded clip on `clip`: one
        // layer cannot both clip its rows to the corners and cast a shadow.
        layer?.shadowOpacity = 0.35
        layer?.shadowRadius = 14
        layer?.shadowOffset = CGSize(width: 0, height: -4)
        layer?.shadowColor = NSColor.black.cgColor
        clip.wantsLayer = true
        clip.layer?.cornerRadius = 9
        clip.layer?.masksToBounds = true
        clip.layer?.borderWidth = 1
        clip.translatesAutoresizingMaskIntoConstraints = false
        addSubview(clip)

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("completion"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.gridStyleMask = []
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(clicked(_:))
        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsets(top: Self.verticalPadding, left: 0, bottom: Self.verticalPadding, right: 0)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        clip.addSubview(scroll)

        let height = heightAnchor.constraint(equalToConstant: preferredSize.height)
        heightConstraint = height
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: Self.width), height,
            clip.leadingAnchor.constraint(equalTo: leadingAnchor), clip.trailingAnchor.constraint(equalTo: trailingAnchor),
            clip.topAnchor.constraint(equalTo: topAnchor), clip.bottomAnchor.constraint(equalTo: bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: clip.leadingAnchor), scroll.trailingAnchor.constraint(equalTo: clip.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: clip.topAnchor), scroll.bottomAnchor.constraint(equalTo: clip.bottomAnchor),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.list)
        setAccessibilityIdentifier("completion-popup")
        applyColors()
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }
    override func updateLayer() { applyColors() }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColors()
        table.reloadData()
    }

    private func applyColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            clip.layer?.backgroundColor = Theme.sidebar.cgColor
            clip.layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    func update(_ items: [CompletionInfo], query: String) {
        // The window timer refreshes every 0.1 s; resetting selection and
        // scroll each time made the list impossible to scroll or pick from.
        if !isHidden, shownQuery == query, Self.same(all, items) { return }
        shownQuery = query
        all = items
        results = Self.rank(items, query: query)
        heightConstraint?.constant = preferredSize.height
        table.reloadData()
        if !results.isEmpty { table.selectRowIndexes([0], byExtendingSelection: false); table.scrollRowToVisible(0) }
        isHidden = results.isEmpty
    }

    private static func same(_ a: [CompletionInfo], _ b: [CompletionInfo]) -> Bool {
        a.count == b.count && zip(a, b).allSatisfy {
            $0.label == $1.label && $0.detail == $1.detail && $0.insertText == $1.insertText && $0.kind == $1.kind
        }
    }

    // MARK: Ranking

    /// Items that fit `query`, best first. The first typed letter has to
    /// start a word of the label (`isset`, `getUser`, `app.name`, `$order`),
    /// or the query has to appear as one run inside it. Later letters may
    /// then be spread out, but score lower the further apart they are. A
    /// label whose words never start with the first letter and does not
    /// contain the query is dropped. Equal scores keep the order they came in.
    static func rank(_ items: [CompletionInfo], query: String) -> [(item: CompletionInfo, matched: [Int])] {
        let q = Array(query.lowercased())
        guard !q.isEmpty else {
            return items.prefix(maxResults).map { ($0, []) }
        }
        var scored: [(index: Int, score: Int, matched: [Int])] = []
        for (index, item) in items.enumerated() {
            if let hit = match(label: item.label, query: q) {
                scored.append((index, hit.score, hit.matched))
            }
        }
        scored.sort { $0.score != $1.score ? $0.score > $1.score : $0.index < $1.index }
        return scored.prefix(maxResults).map { (items[$0.index], $0.matched) }
    }

    static func match(label: String, query q: [Character]) -> (score: Int, matched: [Int])? {
        let chars = Array(label)
        let lower = Array(label.lowercased())
        // Lowercasing can change the length (`İ`); skip such labels' highlights.
        guard lower.count == chars.count, !q.isEmpty, q.count <= chars.count else { return nil }
        let boundary = chars.indices.map { i -> Bool in
            if i == 0 { return true }
            let previous = chars[i - 1], current = chars[i]
            if !previous.isLetter && !previous.isNumber { return true }
            if current.isUppercase && previous.isLowercase { return true }
            return current.isNumber && !previous.isNumber
        }
        var best: (score: Int, matched: [Int])?
        var tried = 0
        for start in chars.indices where boundary[start] && lower[start] == q[0] {
            tried += 1
            if tried > 8 { break }
            var matched = [start]
            var score = 0
            var position = start
            var failed = false
            for character in q.dropFirst() {
                var next: Int?
                if position + 1 < chars.count, lower[position + 1] == character {
                    next = position + 1
                } else {
                    next = ((position + 1)..<chars.count).first { boundary[$0] && lower[$0] == character }
                        ?? ((position + 1)..<chars.count).first { lower[$0] == character }
                }
                guard let found = next else { failed = true; break }
                score -= found - position - 1
                matched.append(found)
                position = found
            }
            if failed { continue }
            for (offset, index) in matched.enumerated() {
                score += 10
                if boundary[index] { score += 12 }
                if offset > 0, matched[offset - 1] + 1 == index { score += 15 }
            }
            // A match at the very start (past a `$`) beats one further in.
            let lead = chars.first == "$" ? 1 : 0
            if start == 0 || start == lead { score += 60 }
            if chars.count == q.count { score += 120 }
            score -= (chars.count - q.count) / 3
            if best == nil || score > best!.score { best = (score, matched) }
        }
        if best != nil { return best }
        // The query as one run inside the label.
        if let range = lower.firstRange(of: q) {
            let matched = Array(range)
            return (4 * q.count - range.lowerBound - (chars.count - q.count) / 3, matched)
        }
        return nil
    }

    // MARK: Selection and keys

    func move(_ delta: Int) {
        guard !results.isEmpty else { return }
        let current = table.selectedRow < 0 ? 0 : table.selectedRow
        let next = (current + delta + results.count) % results.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func accept() {
        guard table.selectedRow >= 0, results.indices.contains(table.selectedRow) else { return }
        onSelect?(results[table.selectedRow].item)
    }

    func cancel() { onClose?() }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        let id = NSUserInterfaceItemIdentifier("completion-row")
        return (tableView.makeView(withIdentifier: id, owner: self) as? CompletionRowView) ?? {
            let view = CompletionRowView()
            view.identifier = id
            return view
        }()
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("completion-cell")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? CompletionCell) ?? {
            let view = CompletionCell()
            view.identifier = id
            return view
        }()
        guard results.indices.contains(row) else { return cell }
        let entry = results[row]
        cell.configure(item: entry.item, matched: entry.matched, appearance: effectiveAppearance)
        return cell
    }

    @objc private func clicked(_ sender: Any?) { accept() }
}

/// Keeps keyboard focus in the editor when a row is clicked.
private final class NoFocusTableView: NSTableView {
    override var acceptsFirstResponder: Bool { false }
}

/// A rounded highlight inset from the edges, in the accent colour.
private final class CompletionRowView: NSTableRowView {
    override func drawSelection(in dirtyRect: NSRect) {
        guard selectionHighlightStyle != .none else { return }
        let rect = bounds.insetBy(dx: 5, dy: 1)
        Theme.accent.resolved(for: effectiveAppearance).withAlphaComponent(0.26).setFill()
        NSBezierPath(roundedRect: rect, xRadius: 6, yRadius: 6).fill()
    }
}

/// One suggestion: kind badge, name with the typed letters picked out, and
/// what it is (a namespace, a type, a value) quietly on the right.
private final class CompletionCell: NSView {
    private let badge = KindBadge()
    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")

    private static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    private static let boldFont = NSFont.monospacedSystemFont(ofSize: 12, weight: .bold)

    override init(frame: NSRect) {
        super.init(frame: frame)
        title.lineBreakMode = .byTruncatingTail
        title.cell?.usesSingleLineMode = true
        detail.font = .systemFont(ofSize: 11)
        detail.textColor = .tertiaryLabelColor
        detail.alignment = .right
        detail.cell?.usesSingleLineMode = true
        for view in [badge, title, detail] { addSubview(view) }
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(item: CompletionInfo, matched: [Int], appearance: NSAppearance) {
        badge.configure(kind: item.kind, appearance: appearance)
        let accent = Theme.accent.resolved(for: appearance)
        let text = NSMutableAttributedString()
        let hits = Set(matched)
        for (index, character) in item.label.enumerated() {
            let isHit = hits.contains(index)
            text.append(NSAttributedString(string: String(character), attributes: [
                .font: isHit ? Self.boldFont : Self.font,
                .foregroundColor: isHit ? accent : NSColor.labelColor,
            ]))
        }
        title.attributedStringValue = text
        let summary = (item.detail ?? "").replacingOccurrences(of: "\n", with: " ")
        detail.stringValue = summary
        // Namespaces say most at their end; everything else at its start.
        detail.lineBreakMode = summary.contains("\\") ? .byTruncatingHead : .byTruncatingTail
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let height = bounds.height
        let badgeSize: CGFloat = 16
        badge.frame = NSRect(x: 12, y: (height - badgeSize) / 2, width: badgeSize, height: badgeSize)
        let rightEdge = bounds.width - 14
        let wanted = ceil(detail.attributedStringValue.size().width) + 4
        let detailWidth = detail.stringValue.isEmpty ? 0 : min(wanted, bounds.width * 0.42)
        let detailHeight = ceil(detail.intrinsicContentSize.height)
        detail.frame = NSRect(x: rightEdge - detailWidth, y: (height - detailHeight) / 2, width: detailWidth, height: detailHeight)
        let titleX = badge.frame.maxX + 10
        let titleHeight = ceil(title.intrinsicContentSize.height)
        let titleRight = detailWidth > 0 ? detail.frame.minX - 10 : rightEdge
        title.frame = NSRect(x: titleX, y: (height - titleHeight) / 2, width: max(0, titleRight - titleX), height: titleHeight)
    }
}

/// A small rounded square with a letter for the kind of suggestion.
private final class KindBadge: NSView {
    private var glyph = "·"
    private var color = NSColor.secondaryLabelColor

    func configure(kind: UInt64?, appearance: NSAppearance) {
        let (glyph, scope) = Self.style(for: kind)
        self.glyph = glyph
        color = scope.map { Theme.syntax($0, appearance: appearance) } ?? .secondaryLabelColor
        needsDisplay = true
    }

    /// Letter and theme scope for an LSP `CompletionItemKind`.
    private static func style(for kind: UInt64?) -> (String, String?) {
        switch kind {
        case 2, 4: ("m", "function")
        case 3: ("ƒ", "function")
        case 5, 10: ("p", "property")
        case 6: ("v", "variable")
        case 7: ("C", "type")
        case 8: ("I", "type")
        case 9: ("M", "type")
        case 13: ("E", "type")
        case 14: ("k", "keyword")
        case 15: ("s", "string")
        case 20, 21: ("c", "constant")
        case 22: ("S", "type")
        default: ("·", nil)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        let rect = bounds.insetBy(dx: 0.5, dy: 0.5)
        color.withAlphaComponent(0.18).setFill()
        NSBezierPath(roundedRect: rect, xRadius: 4, yRadius: 4).fill()
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10, weight: .semibold),
            .foregroundColor: color,
        ]
        let size = glyph.size(withAttributes: attributes)
        glyph.draw(at: NSPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2), withAttributes: attributes)
    }
}
