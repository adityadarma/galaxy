import AppKit

/// One table row: a commit, or the "Uncommitted Changes" entry on top.
struct GitGraphRow: Equatable {
    var commit: GitCommitInfo?
    var uncommittedCount = 0
    var lane: UInt32
    var passing: [UInt32]
    var incoming: [UInt32]
    var outgoing: [UInt32]
    /// Tags on `origin`; nil when there is no origin or it could not be read.
    var originTags: Set<String>?
    var pinned = false
}

enum GitGraphRows {
    /// Commits in order, with the working-tree entry first when there are
    /// uncommitted changes. That entry hangs off the first (newest) commit.
    static func make(commits: [GitCommitInfo], uncommitted: Int,
                     originTags: Set<String>? = nil, pinned: Set<String> = []) -> [GitGraphRow] {
        var rows = commits.map {
            GitGraphRow(commit: $0, lane: $0.lane, passing: $0.passing,
                        incoming: $0.incoming, outgoing: $0.outgoing, originTags: originTags,
                        pinned: pinned.contains($0.hash))
        }
        if uncommitted > 0 {
            let lane = commits.first?.lane ?? 0
            rows.insert(GitGraphRow(commit: nil, uncommittedCount: uncommitted, lane: lane,
                                    passing: [], incoming: [], outgoing: commits.isEmpty ? [] : [lane],
                                    originTags: originTags),
                        at: 0)
        }
        return rows
    }

    static func laneCount(_ rows: [GitGraphRow]) -> Int {
        var widest: UInt32 = 0
        for row in rows {
            widest = max(widest, row.lane)
            for lane in row.passing { widest = max(widest, lane) }
            for lane in row.incoming { widest = max(widest, lane) }
            for lane in row.outgoing { widest = max(widest, lane) }
        }
        return Int(widest) + 1
    }
}

/// The Git Graph bottom-panel tab: a commit table whose first column draws
/// the branch lanes, like the Git Graph extension in VS Code.
@MainActor
final class GitGraphPanel: NSView, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    static let rowHeight: CGFloat = 24
    static let laneWidth: CGFloat = 14
    static let commitLimit: UInt32 = 500

    var workspace: Workspace? {
        didSet { loadPinned() }
    }
    /// Status-line text for the result of a context-menu action.
    var onMessage: ((String) -> Void)?
    /// Called after an action changed the repository (branch, tag, HEAD).
    var onRepositoryChanged: (() -> Void)?

    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let empty = NSTextField(labelWithString: "No commits")
    private let refreshButton = NSButton()
    private(set) var rows: [GitGraphRow] = []
    private var timer: Timer?
    private var loading = false
    private var loadingOrigin = false
    private var commits: [GitCommitInfo] = []
    private var uncommitted = 0
    /// Tags on origin as of the last successful read; nil when unknown.
    private(set) var originTags: Set<String>?
    /// Commits the user pinned, saved per workspace.
    private(set) var pinned: Set<String> = []
    /// The commit chosen with "Select for Compare".
    private(set) var compareBase: String?
    private var compareController: GitCompareWindowController?
    /// Asks which kind of reset to do; nil cancels. Tests replace it.
    var chooseResetMode: (_ detail: String) -> GitResetModeInfo? = { detail in
        let alert = NSAlert()
        alert.messageText = "Reset current branch to this commit"
        alert.informativeText = detail
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 360, height: 26), pullsDown: false)
        popup.addItems(withTitles: [
            "Soft: keep all changes staged",
            "Mixed: keep all changes, unstaged",
            "Hard: discard all changes",
        ])
        alert.accessoryView = popup
        alert.addButton(withTitle: "Reset")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        return [GitResetModeInfo.soft, .mixed, .hard][popup.indexOfSelectedItem]
    }
    /// Asks before a destructive or history-changing action. Tests replace it.
    var confirm: (_ title: String, _ detail: String, _ button: String) -> Bool = { title, detail, button in
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: button)
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "dd MMM yyyy HH:mm"
        return formatter
    }()

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityRole(.table)
        setAccessibilityLabel("Git graph")
        setAccessibilityIdentifier("git-graph-panel")

        let columns: [(String, String, CGFloat, CGFloat)] = [
            ("Graph", "Graph", 60, 40),
            ("Description", "Description", 520, 160),
            ("Date", "Date", 130, 100),
            ("Author", "Author", 130, 80),
            ("Commit", "Commit", 80, 60),
        ]
        for (identifier, title, width, minWidth) in columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = title
            column.width = width
            column.minWidth = minWidth
            column.resizingMask = identifier == "Description" ? .autoresizingMask : .userResizingMask
            table.addTableColumn(column)
        }
        table.headerView = NSTableHeaderView()
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        table.backgroundColor = .clear
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.setAccessibilityIdentifier("git-graph-results")
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.delegate = self
        table.menu = menu

        refreshButton.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Refresh")?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        refreshButton.isBordered = false
        refreshButton.imagePosition = .imageOnly
        refreshButton.contentTintColor = .secondaryLabelColor
        refreshButton.toolTip = "Refresh"
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked(_:))
        refreshButton.setAccessibilityLabel("Refresh git graph")
        refreshButton.translatesAutoresizingMaskIntoConstraints = false

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.drawsBackground = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        empty.textColor = .secondaryLabelColor
        empty.translatesAutoresizingMaskIntoConstraints = false
        empty.isHidden = true
        addSubview(scroll)
        addSubview(empty)
        addSubview(refreshButton)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            empty.centerXAnchor.constraint(equalTo: centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: centerYAnchor),
            refreshButton.topAnchor.constraint(equalTo: topAnchor, constant: 2),
            refreshButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            refreshButton.widthAnchor.constraint(equalToConstant: 22),
            refreshButton.heightAnchor.constraint(equalToConstant: 20),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Loads now and keeps the graph current while the tab is showing, so a
    /// commit made from the terminal appears without a manual refresh.
    func start() {
        timer?.invalidate()
        refresh(includeOrigin: true)
        timer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }

    func stop() { timer?.invalidate(); timer = nil }

    @objc private func refreshClicked(_ sender: Any?) { refresh(includeOrigin: true) }

    /// Reloads the commits. Reading origin's tags contacts the network, so
    /// the 5-second poll skips it; opening the tab, the refresh button and
    /// tag actions include it.
    func refresh(includeOrigin: Bool = false) {
        guard let workspace else {
            commits = []
            uncommitted = 0
            originTags = nil
            rebuild()
            return
        }
        if includeOrigin { refreshOriginTags(workspace) }
        guard !loading else { return }
        loading = true
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let commits = (try? workspace.gitLogGraph(limit: Self.commitLimit)) ?? []
            let uncommitted = ((try? workspace.gitStatus())??.changes.count) ?? 0
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.loading = false
                self.commits = commits
                self.uncommitted = uncommitted
                self.rebuild()
            }
        }
    }

    private func refreshOriginTags(_ workspace: Workspace) {
        guard !loadingOrigin else { return }
        loadingOrigin = true
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let result: Result<Set<String>?, Error> = Result { try workspace.gitRemoteTags().map(Set.init) }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.loadingOrigin = false
                switch result {
                case .success(let tags):
                    self.originTags = tags
                case .failure(let error):
                    // Keep the last known set rather than guess.
                    self.onMessage?("Git: could not read tags from origin: \(Self.describe(error))")
                }
                self.rebuild()
            }
        }
    }

    private func rebuild() {
        apply(GitGraphRows.make(commits: commits, uncommitted: uncommitted,
                                originTags: originTags, pinned: pinned))
    }

    // MARK: Pinned commits

    private var pinKey: String? { workspace.map { "GitGraphPinned:" + $0.root() } }

    private func loadPinned() {
        compareBase = nil
        guard let key = pinKey else { pinned = []; return }
        pinned = Set(UserDefaults.standard.stringArray(forKey: key) ?? [])
    }

    func togglePin(_ hash: String) {
        if !pinned.insert(hash).inserted { pinned.remove(hash) }
        if let key = pinKey { UserDefaults.standard.set(pinned.sorted(), forKey: key) }
        rebuild()
    }

    // MARK: Current branch facts

    private var headCommit: GitCommitInfo? { commits.first { $0.refs.contains { $0.kind == .head } } }
    private var isDetached: Bool {
        headCommit?.refs.first { $0.kind == .head }?.name == "HEAD"
    }

    /// Extra warning text when rewriting would diverge from a pushed branch.
    private func remoteNote() -> String {
        guard let workspace, let head = headCommit,
              (try? workspace.gitIsOnRemote(hash: head.hash)) == true else { return "" }
        return " The current commit is already on a remote branch, so pushing afterwards will need a force push."
    }

    func apply(_ next: [GitGraphRow]) {
        guard next != rows else { return }
        rows = next
        let lanes = GitGraphRows.laneCount(next)
        table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier("Graph"))?.width =
            min(240, max(40, CGFloat(lanes) * Self.laneWidth + 16))
        table.reloadData()
        empty.isHidden = !next.isEmpty
        scroll.isHidden = next.isEmpty
    }

    // MARK: Context menu

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let row = table.clickedRow
        guard rows.indices.contains(row) else { return }
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        for item in contextItems(forRow: row) { menu.addItem(item) }
    }

    /// Items for one row; empty for the uncommitted-changes row, which is
    /// not a commit.
    func contextItems(forRow row: Int) -> [NSMenuItem] {
        guard rows.indices.contains(row), let commit = rows[row].commit else { return [] }
        func item(_ title: String, enabled: Bool = true, _ action: @escaping () -> Void) -> NSMenuItem {
            let entry = NSMenuItem(title: title, action: #selector(runMenuAction(_:)), keyEquivalent: "")
            entry.target = self
            entry.representedObject = action as AnyObject
            entry.isEnabled = enabled
            return entry
        }
        let short = String(commit.hash.prefix(8))
        let isHead = commit.hash == headCommit?.hash
        let branch = headCommit?.refs.first { $0.kind == .head }?.name ?? "HEAD"
        var items: [NSMenuItem] = []

        items.append(item("Add Tag…") { [weak self] in
            guard let name = Self.promptName("Add Tag", "Tag name") else { return }
            self?.perform("Created tag \(name)") { try $0.gitCreateTag(name: name, hash: commit.hash) }
        })
        items.append(item("Create Branch…") { [weak self] in
            guard let name = Self.promptName("Create Branch", "Branch name") else { return }
            self?.perform("Created branch \(name)") { try $0.gitCreateBranchAt(name: name, hash: commit.hash) }
        })
        items.append(.separator())

        for ref in commit.refs where ref.kind == .branch {
            items.append(item("Checkout Branch \"\(ref.name)\"") { [weak self] in
                self?.perform("Checked out \(ref.name)") { try $0.gitCheckout(name: ref.name) }
            })
        }
        items.append(item("Checkout Commit (Detached)") { [weak self] in
            self?.perform("Checked out \(short)") { try $0.gitCheckoutCommit(hash: commit.hash) }
        })
        items.append(item("Cherry Pick…") { [weak self] in
            guard let self, self.confirm("Cherry pick \(short)?",
                                         "Applies this commit's changes as a new commit on \(branch). If it conflicts, it is aborted and nothing changes.",
                                         "Cherry Pick") else { return }
            self.perform("Cherry picked \(short)") { try $0.gitCherryPick(hash: commit.hash) }
        })
        items.append(item("Create Fixup Commit") { [weak self] in
            self?.perform("Created fixup commit for \(short)") { try $0.gitCommitFixup(hash: commit.hash) }
        })
        items.append(item("Create Squash Commit") { [weak self] in
            self?.perform("Created squash commit for \(short)") { try $0.gitCommitSquash(hash: commit.hash) }
        })
        items.append(item("Revert…") { [weak self] in
            guard let self, self.confirm("Revert \(short)?",
                                         "Adds a new commit on \(branch) that undoes this commit. If it conflicts, it is aborted and nothing changes.",
                                         "Revert") else { return }
            self.perform("Reverted \(short)") { try $0.gitRevert(hash: commit.hash) }
        })
        if isHead {
            items.append(item("Reset Last Commit (Soft)…") { [weak self] in
                guard let self, self.confirm("Reset last commit?",
                                             "Removes this commit from \(branch) and keeps its changes staged." + self.remoteNote(),
                                             "Reset") else { return }
                self.perform("Reset last commit (soft)") { try $0.gitResetLastCommitSoft() }
            })
        }
        items.append(item("Drop…", enabled: !isDetached) { [weak self] in
            guard let self, self.confirm("Drop \(short)?",
                                         "Removes this commit from \(branch) and replays the commits after it. This rewrites history; if it conflicts it is aborted and nothing changes." + self.remoteNote(),
                                         "Drop") else { return }
            self.perform("Dropped \(short)") { try $0.gitDropCommit(hash: commit.hash) }
        })
        items.append(.separator())

        if !isHead {
            items.append(item("Merge into current branch…") { [weak self] in
                guard let self, self.confirm("Merge \(short) into \(branch)?",
                                             "Creates a merge commit on \(branch). If it conflicts, it is aborted and nothing changes.",
                                             "Merge") else { return }
                self.perform("Merged \(short) into \(branch)") { try $0.gitMerge(hash: commit.hash) }
            })
            items.append(item("Rebase current branch on this Commit…", enabled: !isDetached) { [weak self] in
                guard let self, self.confirm("Rebase \(branch) on \(short)?",
                                             "Replays the commits of \(branch) on top of this commit. This rewrites history; if it conflicts it is aborted and nothing changes." + self.remoteNote(),
                                             "Rebase") else { return }
                self.perform("Rebased \(branch) on \(short)") { try $0.gitRebaseOnto(hash: commit.hash) }
            })
        }
        items.append(item("Reset current branch to this Commit…") { [weak self] in
            guard let self else { return }
            let detail = "Moves \(branch) to \(short)." + self.remoteNote()
            guard let mode = self.chooseResetMode(detail) else { return }
            if mode == .hard {
                let lost = self.uncommitted > 0
                    ? "\(self.uncommitted) uncommitted change\(self.uncommitted == 1 ? "" : "s") will be permanently lost."
                    : "Working tree and index will be reset."
                guard self.confirm("Hard reset to \(short)?", lost + " This cannot be undone.", "Hard Reset") else { return }
            }
            self.perform("Reset \(branch) to \(short)") { try $0.gitResetTo(hash: commit.hash, mode: mode) }
        })
        items.append(.separator())

        items.append(item("Copy Commit Hash to Clipboard") { Self.copy(commit.hash) })
        items.append(item("Copy Commit Subject to Clipboard") { Self.copy(commit.subject) })
        items.append(.separator())

        if let base = compareBase, base != commit.hash {
            items.append(item("Compare with Selected (\(base.prefix(8)))") { [weak self] in
                self?.openCompare(from: base, to: commit.hash)
            })
        }
        items.append(item(compareBase == commit.hash ? "Unselect for Compare" : "Select for Compare") { [weak self] in
            guard let self else { return }
            self.compareBase = self.compareBase == commit.hash ? nil : commit.hash
            self.onMessage?(self.compareBase == nil ? "Compare selection cleared"
                                                    : "Selected \(short) for compare; right-click another commit")
        })
        items.append(.separator())
        items.append(item(pinned.contains(commit.hash) ? "Unpin Commit" : "Pin Commit") { [weak self] in
            self?.togglePin(commit.hash)
        })

        let tags = commit.refs.filter { $0.kind == .tag }.map(\.name)
        if !tags.isEmpty { items.append(.separator()) }
        for tag in tags {
            items += tagItems(tag) { title, enabled, action in item(title, enabled: enabled, action) }
        }
        return items
    }

    func openCompare(from: String, to: String) {
        guard let workspace else { return }
        let controller = GitCompareWindowController(workspace: workspace, from: from, to: to)
        compareController = controller
        controller.showWindow(nil)
    }

    /// Delete / push entries for one tag. Origin entries only appear when
    /// origin's tags are known, so "local only" is never guessed.
    private func tagItems(_ tag: String, _ item: (String, Bool, @escaping () -> Void) -> NSMenuItem) -> [NSMenuItem] {
        var items = [item("Delete Tag \"\(tag)\" (Local)", true) { [weak self] in
            guard let self, self.confirm("Delete tag \"\(tag)\"?",
                                         "The tag is removed from this repository only. Origin is not changed.",
                                         "Delete") else { return }
            self.perform("Deleted local tag \(tag)", refreshOrigin: true) { try $0.gitDeleteTag(name: tag) }
        }]
        guard let originTags else { return items }
        if originTags.contains(tag) {
            items.append(item("Delete Tag \"\(tag)\" on origin", true) { [weak self] in
                guard let self, self.confirm("Delete tag \"\(tag)\" on origin?",
                                             "This removes the tag from the shared remote, so it disappears for everyone. The local tag is kept.",
                                             "Delete on origin") else { return }
                self.perform("Deleted tag \(tag) on origin", refreshOrigin: true) { try $0.gitDeleteRemoteTag(name: tag) }
            })
        } else {
            items.append(item("Push Tag \"\(tag)\" to origin", true) { [weak self] in
                self?.perform("Pushed tag \(tag) to origin", refreshOrigin: true) { try $0.gitPushTag(name: tag) }
            })
        }
        return items
    }

    @objc private func runMenuAction(_ sender: NSMenuItem) {
        (sender.representedObject as? (() -> Void))?()
    }

    private static func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private static func promptName(_ title: String, _ placeholder: String) -> String? {
        let alert = NSAlert()
        alert.messageText = title
        let field = NSTextField(string: "")
        field.placeholderString = placeholder
        field.frame = NSRect(x: 0, y: 0, width: 320, height: 24)
        alert.accessoryView = field
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        let name = field.stringValue.trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? nil : name
    }

    /// Runs a git operation off the main thread, then reports and reloads.
    /// `success` is the status-line text when it works.
    private func perform(_ success: String, refreshOrigin: Bool = false, _ operation: @escaping @Sendable (Workspace) throws -> Void) {
        guard let workspace else { return }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let failure: String? = {
                do { try operation(workspace); return nil } catch { return Self.describe(error) }
            }()
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.onMessage?(failure.map { "Git: \($0)" } ?? success)
                self.onRepositoryChanged?()
                self.refresh(includeOrigin: refreshOrigin)
            }
        }
    }

    private static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return "\(error)"
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row), let identifier = tableColumn?.identifier.rawValue else { return nil }
        let item = rows[row]
        switch identifier {
        case "Graph":
            let view = (tableView.makeView(withIdentifier: GitGraphLaneView.reuse, owner: self) as? GitGraphLaneView)
                ?? GitGraphLaneView()
            view.identifier = GitGraphLaneView.reuse
            view.row = item
            return view
        case "Description":
            let view = (tableView.makeView(withIdentifier: GitGraphDescriptionView.reuse, owner: self) as? GitGraphDescriptionView)
                ?? GitGraphDescriptionView()
            view.identifier = GitGraphDescriptionView.reuse
            view.row = item
            return view
        default:
            let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("git-graph-cell"), owner: self) as? NSTextField)
                ?? NSTextField(labelWithString: "")
            cell.identifier = NSUserInterfaceItemIdentifier("git-graph-cell")
            cell.font = .systemFont(ofSize: 12)
            cell.textColor = .secondaryLabelColor
            cell.lineBreakMode = .byTruncatingTail
            cell.stringValue = Self.text(for: identifier, item: item)
            return cell
        }
    }

    static func text(for column: String, item: GitGraphRow) -> String {
        guard let commit = item.commit else {
            switch column {
            case "Date": return dateFormatter.string(from: Date())
            case "Author", "Commit": return "*"
            default: return ""
            }
        }
        switch column {
        case "Date": return dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(commit.timestamp)))
        case "Author": return commit.author
        case "Commit": return String(commit.hash.prefix(8))
        default: return ""
        }
    }
}

/// Colors for lanes, cycled by lane index.
enum GitGraphPalette {
    static let colors: [NSColor] = [
        NSColor(srgbRed: 0.00, green: 0.45, blue: 0.85, alpha: 1),
        NSColor(srgbRed: 0.20, green: 0.70, blue: 0.40, alpha: 1),
        NSColor(srgbRed: 0.90, green: 0.55, blue: 0.10, alpha: 1),
        NSColor(srgbRed: 0.70, green: 0.30, blue: 0.80, alpha: 1),
        NSColor(srgbRed: 0.85, green: 0.25, blue: 0.30, alpha: 1),
        NSColor(srgbRed: 0.10, green: 0.70, blue: 0.75, alpha: 1),
    ]

    static func color(_ lane: UInt32) -> NSColor { colors[Int(lane) % colors.count] }
}

/// The lane drawing for one row.
@MainActor
final class GitGraphLaneView: NSView {
    static let reuse = NSUserInterfaceItemIdentifier("git-graph-lanes")

    var row: GitGraphRow? { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }

    private func x(_ lane: UInt32) -> CGFloat {
        8 + CGFloat(lane) * GitGraphPanel.laneWidth + GitGraphPanel.laneWidth / 2
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let row else { return }
        let mid = bounds.midY
        func stroke(_ lane: UInt32, _ from: NSPoint, _ to: NSPoint) {
            let path = NSBezierPath()
            path.lineWidth = 1.5
            path.move(to: from)
            if from.x == to.x {
                path.line(to: to)
            } else {
                // Bend near the far end so a branch leaves its node
                // straight and joins the lane in the next row.
                let bendY = from.y == mid ? to.y - 6 : from.y + 6
                path.line(to: NSPoint(x: from.x, y: bendY))
                path.line(to: to)
            }
            GitGraphPalette.color(lane).setStroke()
            path.stroke()
        }
        for lane in row.passing {
            stroke(lane, NSPoint(x: x(lane), y: 0), NSPoint(x: x(lane), y: bounds.maxY))
        }
        for lane in row.incoming {
            stroke(lane, NSPoint(x: x(lane), y: 0), NSPoint(x: x(row.lane), y: mid))
        }
        for lane in row.outgoing {
            stroke(lane, NSPoint(x: x(row.lane), y: mid), NSPoint(x: x(lane), y: bounds.maxY))
        }
        let center = NSPoint(x: x(row.lane), y: mid)
        let radius: CGFloat = 4
        let dot = NSBezierPath(ovalIn: NSRect(x: center.x - radius, y: center.y - radius,
                                              width: radius * 2, height: radius * 2))
        if row.commit == nil {
            NSColor.textBackgroundColor.setFill()
            dot.fill()
            NSColor.secondaryLabelColor.setStroke()
            dot.lineWidth = 1.5
            dot.stroke()
        } else {
            GitGraphPalette.color(row.lane).setFill()
            dot.fill()
        }
    }
}

/// Ref pills followed by the commit subject (or the uncommitted-changes
/// label, in bold).
@MainActor
final class GitGraphDescriptionView: NSView {
    static let reuse = NSUserInterfaceItemIdentifier("git-graph-description")

    var row: GitGraphRow? { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }

    private static let pillFont = NSFont.systemFont(ofSize: 11, weight: .semibold)
    private static let textFont = NSFont.systemFont(ofSize: 12)

    override func draw(_ dirtyRect: NSRect) {
        guard let row else { return }
        var x: CGFloat = 4
        if row.pinned, let pin = NSImage(systemSymbolName: "pin.fill", accessibilityDescription: "Pinned")?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .regular)) {
            let tinted = pin.tinted(with: Theme.accent)
            tinted.draw(in: NSRect(x: x, y: (bounds.height - 12) / 2, width: 12, height: 12))
            x += 16
        }
        let textHeight = Self.textFont.boundingRectForFont.height
        let textY = (bounds.height - textHeight) / 2

        guard let commit = row.commit else {
            let title = NSAttributedString(
                string: "Uncommitted Changes (\(row.uncommittedCount))",
                attributes: [.font: NSFont.systemFont(ofSize: 12, weight: .semibold),
                             .foregroundColor: NSColor.labelColor])
            title.draw(at: NSPoint(x: x, y: textY))
            return
        }

        for ref in commit.refs {
            let color = Self.color(for: ref.kind)
            // A tag missing from origin is drawn as an outline.
            let localOnly = ref.kind == .tag && (row.originTags.map { !$0.contains(ref.name) } ?? false)
            let label = NSAttributedString(
                string: ref.name,
                attributes: [.font: Self.pillFont, .foregroundColor: localOnly ? color : NSColor.white])
            let size = label.size()
            let pill = NSRect(x: x, y: (bounds.height - 16) / 2, width: ceil(size.width) + 12, height: 16)
            let shape = NSBezierPath(roundedRect: pill.insetBy(dx: 0.5, dy: 0.5), xRadius: 4, yRadius: 4)
            if localOnly {
                color.setStroke()
                shape.lineWidth = 1
                shape.stroke()
            } else {
                color.setFill()
                shape.fill()
            }
            label.draw(at: NSPoint(x: pill.minX + 6, y: pill.minY + (16 - size.height) / 2))
            x = pill.maxX + 5
        }
        guard x < bounds.width else { return }
        let subject = NSMutableAttributedString(
            string: commit.subject,
            attributes: [.font: Self.textFont, .foregroundColor: NSColor.labelColor])
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        subject.addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: subject.length))
        subject.draw(in: NSRect(x: x, y: textY, width: bounds.width - x - 4, height: textHeight))
    }

    private static func color(for kind: GitRefKindInfo) -> NSColor {
        switch kind {
        case .head: Theme.accent
        case .branch: NSColor(srgbRed: 0.20, green: 0.60, blue: 0.35, alpha: 1)
        case .remote: NSColor(srgbRed: 0.45, green: 0.45, blue: 0.55, alpha: 1)
        case .tag: NSColor(srgbRed: 0.80, green: 0.50, blue: 0.10, alpha: 1)
        }
    }
}

private extension NSImage {
    /// A copy filled with `color`, for symbols drawn outside a control.
    func tinted(with color: NSColor) -> NSImage {
        let copy = NSImage(size: size, flipped: false) { rect in
            self.draw(in: rect)
            color.set()
            rect.fill(using: .sourceAtop)
            return true
        }
        return copy
    }
}
