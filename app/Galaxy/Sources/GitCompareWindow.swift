import AppKit

/// Colors the lines of a unified diff: additions green, removals red, hunk
/// headers in the accent color. Pure so it can be tested.
enum GitDiffText {
    static func attributed(_ diff: String) -> NSAttributedString {
        let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        let result = NSMutableAttributedString()
        for line in diff.split(separator: "\n", omittingEmptySubsequences: false) {
            var attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.labelColor]
            if line.hasPrefix("+++") || line.hasPrefix("---") {
                attributes[.foregroundColor] = NSColor.secondaryLabelColor
            } else if line.hasPrefix("+") {
                attributes[.backgroundColor] = Theme.diffAdded
            } else if line.hasPrefix("-") {
                attributes[.backgroundColor] = Theme.diffRemoved
            } else if line.hasPrefix("@@") {
                attributes[.foregroundColor] = Theme.accent
            } else if line.hasPrefix("diff ") || line.hasPrefix("index ") || line.hasPrefix("rename ")
                        || line.hasPrefix("new file") || line.hasPrefix("deleted file") {
                attributes[.foregroundColor] = NSColor.secondaryLabelColor
            }
            result.append(NSAttributedString(string: String(line) + "\n", attributes: attributes))
        }
        return result
    }

    static func color(forStatus status: String) -> NSColor {
        switch status {
        case "A": .systemGreen
        case "D": .systemRed
        case "R", "C": .systemBlue
        default: .systemOrange
        }
    }
}

/// "Select for Compare": the files that differ between two commits on the
/// left, the selected file's diff on the right.
@MainActor
final class GitCompareWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let workspace: Workspace
    let from: String
    let to: String
    private let table = NSTableView()
    private let textView = NSTextView()
    private let summary = NSTextField(labelWithString: "Loading…")
    private(set) var files: [GitFileDiffInfo] = []
    private var diffRequest = 0

    init(workspace: Workspace, from: String, to: String) {
        self.workspace = workspace
        self.from = from
        self.to = to
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 980, height: 620),
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered, defer: false)
        window.title = "Compare \(from.prefix(8)) \u{2192} \(to.prefix(8))"
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier("git-compare-window")
        super.init(window: window)
        buildContent(in: window)
        load()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func buildContent(in window: NSWindow) {
        let status = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("status"))
        status.width = 24
        status.minWidth = 24
        status.maxWidth = 24
        let path = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("path"))
        path.resizingMask = .autoresizingMask
        table.addTableColumn(status)
        table.addTableColumn(path)
        table.headerView = nil
        table.rowHeight = 22
        table.intercellSpacing = .zero
        table.dataSource = self
        table.delegate = self
        table.setAccessibilityIdentifier("git-compare-files")

        let list = NSScrollView()
        list.documentView = table
        list.hasVerticalScroller = true
        list.translatesAutoresizingMaskIntoConstraints = false

        summary.textColor = .secondaryLabelColor
        summary.font = .systemFont(ofSize: 11)
        summary.translatesAutoresizingMaskIntoConstraints = false
        let left = NSView()
        left.addSubview(summary)
        left.addSubview(list)
        NSLayoutConstraint.activate([
            summary.topAnchor.constraint(equalTo: left.topAnchor, constant: 6),
            summary.leadingAnchor.constraint(equalTo: left.leadingAnchor, constant: 8),
            summary.trailingAnchor.constraint(equalTo: left.trailingAnchor, constant: -8),
            list.topAnchor.constraint(equalTo: summary.bottomAnchor, constant: 6),
            list.leadingAnchor.constraint(equalTo: left.leadingAnchor),
            list.trailingAnchor.constraint(equalTo: left.trailingAnchor),
            list.bottomAnchor.constraint(equalTo: left.bottomAnchor),
            left.widthAnchor.constraint(greaterThanOrEqualToConstant: 220),
        ])

        textView.isEditable = false
        textView.isRichText = true
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.isHorizontallyResizable = true
        textView.textContainer?.widthTracksTextView = false
        textView.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                                                       height: CGFloat.greatestFiniteMagnitude)
        textView.setAccessibilityLabel("File diff")
        textView.setAccessibilityIdentifier("git-compare-diff")
        let diff = NSScrollView()
        diff.documentView = textView
        diff.hasVerticalScroller = true
        diff.hasHorizontalScroller = true

        let split = NSSplitView()
        split.isVertical = true
        split.dividerStyle = .thin
        split.addArrangedSubview(left)
        split.addArrangedSubview(diff)
        split.setHoldingPriority(.defaultHigh, forSubviewAt: 0)
        window.contentView = split
        split.setPosition(280, ofDividerAt: 0)
    }

    private func load() {
        let (workspace, from, to) = (workspace, from, to)
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result: Result<[GitFileDiffInfo], Error> = Result { try workspace.gitCompareFiles(from: from, to: to) }
            Task { @MainActor [weak self] in self?.loaded(result) }
        }
    }

    private func loaded(_ result: Result<[GitFileDiffInfo], Error>) {
        switch result {
        case .success(let files):
            self.files = files
            summary.stringValue = files.isEmpty ? "No differences" : "\(files.count) changed file\(files.count == 1 ? "" : "s")"
            table.reloadData()
            if !files.isEmpty { table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false) }
        case .failure(let error):
            summary.stringValue = "Could not compare"
            textView.string = Self.describe(error)
        }
    }

    private static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return "\(error)"
    }

    func numberOfRows(in tableView: NSTableView) -> Int { files.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard files.indices.contains(row) else { return nil }
        let file = files[row]
        let cell = NSTextField(labelWithString: "")
        cell.lineBreakMode = .byTruncatingMiddle
        if tableColumn?.identifier.rawValue == "status" {
            cell.stringValue = file.status
            cell.alignment = .center
            cell.font = .monospacedSystemFont(ofSize: 11, weight: .semibold)
            cell.textColor = GitDiffText.color(forStatus: file.status)
        } else {
            cell.stringValue = file.oldPath.map { "\($0) \u{2192} \(file.path)" } ?? file.path
            cell.font = .systemFont(ofSize: 12)
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = table.selectedRow
        guard files.indices.contains(row) else { return }
        showDiff(of: files[row])
    }

    private func showDiff(of file: GitFileDiffInfo) {
        diffRequest += 1
        let request = diffRequest
        textView.string = "Loading…"
        let (workspace, from, to) = (workspace, from, to)
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result: Result<String, Error> = Result {
                try workspace.gitCompareDiff(from: from, to: to, path: file.path, oldPath: file.oldPath)
            }
            Task { @MainActor [weak self] in
                guard let self, request == self.diffRequest else { return }
                switch result {
                case .success(let text):
                    self.textView.textStorage?.setAttributedString(
                        text.isEmpty ? NSAttributedString(string: "No textual changes (binary or mode change).")
                                     : GitDiffText.attributed(text))
                case .failure(let error):
                    self.textView.string = Self.describe(error)
                }
            }
        }
    }
}
