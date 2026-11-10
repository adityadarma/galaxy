import AppKit

/// The bottom panel's Test Results tab: the output of the selected run on
/// the left, and on the right the runs, grouped by test tool with the
/// newest first.
@MainActor
final class TestResultsPanel: NSView, NSTableViewDataSource, NSTableViewDelegate {
    enum Row: Equatable {
        case group(String)
        case run(Int)
    }

    /// Width of the run list.
    static let listWidth: CGFloat = 280

    var onStop: ((TestRunRecord) -> Void)?
    var onClear: (() -> Void)?

    private weak var runner: TestRunner?
    private(set) var rows: [Row] = []
    private(set) var selectedRecord: TestRunRecord?

    private let outputView = NSTextView()
    private let outputScroll = NSScrollView()
    private let table = NSTableView()
    private let listScroll = NSScrollView()
    private let empty = NSTextField(labelWithString: "No tests have been run yet.\nUse the run icon next to a test.")
    private let stopButton = NSButton(title: "Stop", target: nil, action: nil)
    private let clearButton = NSButton(title: "Clear", target: nil, action: nil)
    private let separator = NSBox()
    private var isReloading = false

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Test results")
        setAccessibilityIdentifier("test-results-panel")

        outputView.isEditable = false
        outputView.isSelectable = true
        outputView.isRichText = true
        outputView.drawsBackground = false
        outputView.textContainerInset = NSSize(width: 12, height: 8)
        outputView.isAutomaticQuoteSubstitutionEnabled = false
        outputView.isVerticallyResizable = true
        outputView.isHorizontallyResizable = true
        outputView.autoresizingMask = [.width]
        outputView.textContainer?.widthTracksTextView = false
        outputView.textContainer?.containerSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        outputView.setAccessibilityIdentifier("test-results-output")
        outputScroll.documentView = outputView
        outputScroll.hasVerticalScroller = true
        outputScroll.hasHorizontalScroller = true
        outputScroll.scrollerStyle = .overlay
        outputScroll.drawsBackground = false
        outputScroll.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("run"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 26
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.style = .plain
        table.dataSource = self
        table.delegate = self
        table.setAccessibilityIdentifier("test-results-list")
        listScroll.documentView = table
        listScroll.hasVerticalScroller = true
        listScroll.scrollerStyle = .overlay
        listScroll.drawsBackground = false
        listScroll.translatesAutoresizingMaskIntoConstraints = false

        for (button, action) in [(stopButton, #selector(stopClicked(_:))), (clearButton, #selector(clearClicked(_:)))] {
            button.bezelStyle = .texturedRounded
            button.controlSize = .small
            button.target = self
            button.action = action
            button.translatesAutoresizingMaskIntoConstraints = false
        }
        stopButton.setAccessibilityIdentifier("test-results-stop")
        clearButton.setAccessibilityIdentifier("test-results-clear")

        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false

        empty.textColor = .secondaryLabelColor
        empty.alignment = .center
        empty.maximumNumberOfLines = 2
        empty.translatesAutoresizingMaskIntoConstraints = false

        for view in [outputScroll, separator, stopButton, clearButton, listScroll, empty] as [NSView] {
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            outputScroll.topAnchor.constraint(equalTo: topAnchor),
            outputScroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            outputScroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            outputScroll.trailingAnchor.constraint(equalTo: separator.leadingAnchor),
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.widthAnchor.constraint(equalToConstant: 1),
            separator.trailingAnchor.constraint(equalTo: listScroll.leadingAnchor),
            listScroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            listScroll.widthAnchor.constraint(equalToConstant: Self.listWidth),
            listScroll.topAnchor.constraint(equalTo: stopButton.bottomAnchor, constant: 6),
            listScroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            clearButton.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            clearButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            stopButton.centerYAnchor.constraint(equalTo: clearButton.centerYAnchor),
            stopButton.trailingAnchor.constraint(equalTo: clearButton.leadingAnchor, constant: -6),
            empty.centerXAnchor.constraint(equalTo: centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        reload()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.fill()
    }

    /// The output text shown on the left.
    var outputText: String { outputView.string }

    func bind(to runner: TestRunner) { self.runner = runner }

    /// Rebuilds the run list. `selecting` shows that run's output; without
    /// it the current run stays selected, if it is still there.
    func reload(selecting record: TestRunRecord? = nil) {
        let records = runner?.records ?? []
        var order: [String] = []
        var groups: [String: [TestRunRecord]] = [:]
        // Groups ordered by their newest run.
        for record in records.reversed() {
            if groups[record.test.runner] == nil { order.append(record.test.runner) }
            groups[record.test.runner, default: []].append(record)
        }
        rows = order.flatMap { name in
            [Row.group(name)] + (groups[name] ?? []).map { Row.run($0.id) }
        }
        let target = record ?? selectedRecord.flatMap { current in records.first { $0 === current } }
        isReloading = true
        table.reloadData()
        isReloading = false
        stopButton.isEnabled = runner?.hasRunning ?? false
        clearButton.isEnabled = records.contains { !$0.isRunning }
        empty.isHidden = !records.isEmpty
        listScroll.isHidden = records.isEmpty
        show(target ?? records.last)
    }

    private func show(_ record: TestRunRecord?) {
        let changed = record !== selectedRecord
        selectedRecord = record
        if let record, let index = rows.firstIndex(of: .run(record.id)) {
            isReloading = true
            table.selectRowIndexes(IndexSet(integer: index), byExtendingSelection: false)
            isReloading = false
        } else {
            table.deselectAll(nil)
        }
        if changed || record == nil {
            outputView.textStorage?.setAttributedString(record?.output ?? NSAttributedString())
            outputView.scrollToEndOfDocument(nil)
        }
        outputScroll.isHidden = record == nil
    }

    /// Adds text to the output if it belongs to the shown run. Stays at the
    /// bottom only when it already was there, so scrolling up to read is kept.
    func append(_ fragment: NSAttributedString, to record: TestRunRecord) {
        guard record === selectedRecord, let storage = outputView.textStorage else { return }
        let visible = outputScroll.contentView.bounds
        let atBottom = visible.maxY >= outputView.frame.height - 24
        storage.append(fragment)
        if atBottom { outputView.scrollToEndOfDocument(nil) }
    }

    private func record(for row: Row) -> TestRunRecord? {
        guard case .run(let id) = row else { return nil }
        return runner?.records.first { $0.id == id }
    }

    @objc private func stopClicked(_ sender: Any?) {
        guard let runner else { return }
        // A selected running test stops first; otherwise all that run.
        if let selectedRecord, selectedRecord.isRunning { onStop?(selectedRecord); return }
        for record in runner.records where record.isRunning { onStop?(record) }
    }

    @objc private func clearClicked(_ sender: Any?) { onClear?() }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        rows.indices.contains(row) && { if case .group = rows[row] { true } else { false } }()
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        rows.indices.contains(row) && record(for: rows[row]) != nil
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        let cell = (tableView.makeView(withIdentifier: TestRunCell.identifier, owner: self) as? TestRunCell) ?? TestRunCell()
        cell.identifier = TestRunCell.identifier
        switch rows[row] {
        case .group(let name):
            cell.configure(group: name)
        case .run(let id):
            guard let record = runner?.records.first(where: { $0.id == id }) else { return cell }
            cell.configure(record: record)
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading, table.selectedRow >= 0, rows.indices.contains(table.selectedRow),
              let record = record(for: rows[table.selectedRow]) else { return }
        show(record)
    }
}

/// One row of the run list: a group title, or a run with its state icon.
@MainActor
final class TestRunCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("test-run-cell")

    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")
    private var leading: NSLayoutConstraint?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        icon.translatesAutoresizingMaskIntoConstraints = false
        title.translatesAutoresizingMaskIntoConstraints = false
        title.lineBreakMode = .byTruncatingTail
        detail.translatesAutoresizingMaskIntoConstraints = false
        detail.textColor = .secondaryLabelColor
        detail.font = .systemFont(ofSize: 11)
        detail.setContentHuggingPriority(.required, for: .horizontal)
        detail.setContentCompressionResistancePriority(.required, for: .horizontal)
        addSubview(icon)
        addSubview(title)
        addSubview(detail)
        let leading = icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10)
        self.leading = leading
        NSLayoutConstraint.activate([
            leading,
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 14),
            icon.heightAnchor.constraint(equalToConstant: 14),
            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            title.centerYAnchor.constraint(equalTo: centerYAnchor),
            title.trailingAnchor.constraint(lessThanOrEqualTo: detail.leadingAnchor, constant: -8),
            detail.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            detail.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(group name: String) {
        icon.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 9, weight: .semibold))
        icon.contentTintColor = .secondaryLabelColor
        title.stringValue = name
        title.font = .systemFont(ofSize: 11, weight: .semibold)
        title.textColor = .secondaryLabelColor
        detail.stringValue = ""
        setAccessibilityLabel("\(name) test results")
    }

    func configure(record: TestRunRecord) {
        let (symbol, tint, label): (String, NSColor, String) = switch record.state {
        case .running: ("arrow.triangle.2.circlepath", .systemBlue, "running")
        case .passed: ("checkmark.circle.fill", .systemGreen, "passed")
        case .failed: ("xmark.circle.fill", .systemRed, "failed")
        case .ready: ("stop.circle", .secondaryLabelColor, "stopped")
        }
        icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        icon.contentTintColor = tint
        title.stringValue = "Test run #\(record.id + 1): \(record.test.name)"
        title.font = .systemFont(ofSize: 12)
        title.textColor = .labelColor
        detail.stringValue = record.isRunning ? "running" : record.durationText
        setAccessibilityLabel("Test run \(record.id + 1), \(record.test.name), \(label)")
    }
}
