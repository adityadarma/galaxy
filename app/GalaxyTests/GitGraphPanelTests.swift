import XCTest
@testable import Galaxy

@MainActor
final class GitGraphPanelTests: XCTestCase {
    private func commit(_ hash: String, lane: UInt32 = 0, incoming: [UInt32] = [], outgoing: [UInt32] = [0],
                        passing: [UInt32] = []) -> GitCommitInfo {
        GitCommitInfo(hash: hash, author: "A", timestamp: 0, subject: hash, refs: [],
                      lane: lane, passing: passing, incoming: incoming, outgoing: outgoing)
    }

    func testUncommittedRowComesFirstOnTheNewestCommitsLane() {
        let rows = GitGraphRows.make(commits: [commit("a", lane: 2), commit("b")], uncommitted: 3)
        XCTAssertEqual(rows.count, 3)
        XCTAssertNil(rows[0].commit)
        XCTAssertEqual(rows[0].uncommittedCount, 3)
        XCTAssertEqual(rows[0].lane, 2)
        XCTAssertEqual(rows[0].outgoing, [2])
        XCTAssertEqual(rows[1].commit?.hash, "a")
    }

    func testNoUncommittedRowWhenTreeIsClean() {
        let rows = GitGraphRows.make(commits: [commit("a")], uncommitted: 0)
        XCTAssertEqual(rows.count, 1)
        XCTAssertNotNil(rows[0].commit)
    }

    func testLaneCountCoversTheWidestRow() {
        let rows = GitGraphRows.make(
            commits: [commit("a", outgoing: [0, 1]), commit("b", passing: [3])], uncommitted: 0)
        XCTAssertEqual(GitGraphRows.laneCount(rows), 4)
        XCTAssertEqual(GitGraphRows.laneCount([]), 1)
    }

    func testTableColumnsMatchTheRows() {
        let panel = GitGraphPanel()
        let rows = GitGraphRows.make(commits: [GitCommitInfo(
            hash: "0123456789abcdef", author: "Aditya", timestamp: 0, subject: "feat: x", refs: [],
            lane: 0, passing: [], incoming: [], outgoing: [])], uncommitted: 1)
        panel.apply(rows)
        XCTAssertEqual(panel.rows.count, 2)
        XCTAssertEqual(GitGraphPanel.text(for: "Commit", item: rows[1]), "01234567")
        XCTAssertEqual(GitGraphPanel.text(for: "Author", item: rows[1]), "Aditya")
        XCTAssertEqual(GitGraphPanel.text(for: "Author", item: rows[0]), "*")
    }

    func testToggleOpensTheGitGraphAsAnEditorTabAndClosesIt() {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        let before = c.tabs.count
        c.toggleGitGraph(nil)
        XCTAssertEqual(c.tabs.count, before + 1)
        XCTAssertTrue(c.isGitGraphTabShown)
        XCTAssertEqual(c.activeTab?.title, "Git Graph")
        XCTAssertFalse(c.isBottomPanelVisible, "not in the bottom panel")
        XCTAssertEqual(c.statusLabel.stringValue, "", "the status bar button already names the tab")
        XCTAssertEqual(c.statusBar.gitGraph.contentTintColor, .labelColor)
        c.toggleGitGraph(nil)
        XCTAssertEqual(c.tabs.count, before)
        XCTAssertFalse(c.isGitGraphTabShown)
        XCTAssertEqual(c.statusBar.gitGraph.contentTintColor, .secondaryLabelColor)
    }

    func testToggleSwitchesBackToTheOpenGraphTabInsteadOfOpeningASecond() {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.toggleGitGraph(nil)
        let graphIndex = c.activeIndex
        c.newDocument(nil)
        XCTAssertFalse(c.isGitGraphTabShown)
        let count = c.tabs.count
        c.toggleGitGraph(nil)
        XCTAssertTrue(c.isGitGraphTabShown)
        XCTAssertEqual(c.activeIndex, graphIndex)
        XCTAssertEqual(c.tabs.count, count, "one graph tab at a time")
    }

    func testPanelIsOpaqueSoTheBreadcrumbBehindItDoesNotShowThrough() {
        // The table header is translucent; without its own background the
        // path bar under the tab ("Git Graph") showed blurred through it.
        XCTAssertTrue(GitGraphPanel().isOpaque)
    }

    func testBottomPanelHasNoGitGraphTab() {
        XCTAssertEqual(BottomPanelHeader.Tab.allCases.map(\.title),
                       ["PROBLEMS", "OUTPUT", "DEBUG CONSOLE", "TERMINAL", "PORTS"])
    }

    func testContextMenuOffersCommitActionsAndBranchCheckout() {
        let panel = GitGraphPanel()
        let tagged = GitCommitInfo(
            hash: "0123456789abcdef", author: "A", timestamp: 0, subject: "s",
            refs: [GitRefInfo(name: "feature", kind: .branch), GitRefInfo(name: "origin/x", kind: .remote)],
            lane: 0, passing: [], incoming: [], outgoing: [])
        panel.apply(GitGraphRows.make(commits: [tagged], uncommitted: 1))
        XCTAssertTrue(panel.contextItems(forRow: 0).isEmpty, "uncommitted row is not a commit")
        let titles = panel.contextItems(forRow: 1).map(\.title)
        XCTAssertEqual(titles.filter { !$0.isEmpty }, [
            "Add Tag…", "Create Branch…",
            "Checkout Branch \"feature\"", "Checkout Commit (Detached)",
            "Cherry Pick…", "Create Fixup Commit", "Create Squash Commit", "Revert…", "Drop…",
            "Merge into current branch…", "Rebase current branch on this Commit…",
            "Reset current branch to this Commit…",
            "Copy Commit Hash to Clipboard", "Copy Commit Subject to Clipboard",
            "Select for Compare", "Pin Commit",
        ])
        XCTAssertTrue(panel.contextItems(forRow: 9).isEmpty)
    }

    func testCopyHashPutsTheFullHashOnThePasteboard() throws {
        let panel = GitGraphPanel()
        panel.apply(GitGraphRows.make(commits: [commit("0123456789abcdef")], uncommitted: 0))
        let copy = try XCTUnwrap(panel.contextItems(forRow: 0).first { $0.title == "Copy Commit Hash to Clipboard" })
        _ = copy.target?.perform(copy.action, with: copy)
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "0123456789abcdef")
    }

    func testTagMenuOffersDeleteAndOnlyPushesTagsMissingFromOrigin() {
        let tagged = GitCommitInfo(
            hash: "0123456789abcdef", author: "A", timestamp: 0, subject: "s",
            refs: [GitRefInfo(name: "v1", kind: .tag), GitRefInfo(name: "v2", kind: .tag)],
            lane: 0, passing: [], incoming: [], outgoing: [])
        let panel = GitGraphPanel()
        // Origin unknown: only the local delete, nothing guessed.
        panel.apply(GitGraphRows.make(commits: [tagged], uncommitted: 0))
        let unknown = panel.contextItems(forRow: 0).map(\.title)
        XCTAssertTrue(unknown.contains("Delete Tag \"v1\" (Local)"))
        XCTAssertFalse(unknown.contains { $0.contains("origin") })

        // The rows carry origin's tags, which the description pills use.
        let rows = GitGraphRows.make(commits: [tagged], uncommitted: 0, originTags: ["v1"])
        XCTAssertEqual(rows[0].originTags, ["v1"])
    }

    func testDeletingATagAsksFirstAndCancelDoesNothing() throws {
        let tagged = GitCommitInfo(
            hash: "0123456789abcdef", author: "A", timestamp: 0, subject: "s",
            refs: [GitRefInfo(name: "v1", kind: .tag)],
            lane: 0, passing: [], incoming: [], outgoing: [])
        let panel = GitGraphPanel()
        var asked: [String] = []
        panel.confirm = { title, _, _ in asked.append(title); return false }
        var messages: [String] = []
        panel.onMessage = { messages.append($0) }
        panel.apply(GitGraphRows.make(commits: [tagged], uncommitted: 0))
        let delete = try XCTUnwrap(panel.contextItems(forRow: 0).first { $0.title.hasPrefix("Delete Tag") })
        _ = delete.target?.perform(delete.action, with: delete)
        XCTAssertEqual(asked, ["Delete tag \"v1\"?"])
        XCTAssertTrue(messages.isEmpty, "declined: nothing ran")
    }

    // MARK: Menu rules

    private func titles(_ panel: GitGraphPanel, row: Int) -> [String] {
        panel.contextItems(forRow: row).map(\.title).filter { !$0.isEmpty }
    }

    func testPinTogglesAndShowsOnTheRow() throws {
        let panel = GitGraphPanel()
        let rows = GitGraphRows.make(commits: [commit("aaaaaaaa1111")], uncommitted: 0, pinned: ["aaaaaaaa1111"])
        XCTAssertTrue(rows[0].pinned)
        XCTAssertFalse(GitGraphRows.make(commits: [commit("aaaaaaaa1111")], uncommitted: 0)[0].pinned)

        panel.apply(GitGraphRows.make(commits: [commit("aaaaaaaa1111")], uncommitted: 0))
        let pin = try XCTUnwrap(panel.contextItems(forRow: 0).first { $0.title == "Pin Commit" })
        _ = pin.target?.perform(pin.action, with: pin)
        XCTAssertEqual(panel.pinned, ["aaaaaaaa1111"])
        panel.togglePin("aaaaaaaa1111")
        XCTAssertTrue(panel.pinned.isEmpty)
    }

    func testSelectForCompareThenOffersCompareOnAnotherCommit() throws {
        let panel = GitGraphPanel()
        var messages: [String] = []
        panel.onMessage = { messages.append($0) }
        panel.apply(GitGraphRows.make(commits: [commit("aaaaaaaa1111"), commit("bbbbbbbb2222")], uncommitted: 0))
        XCTAssertFalse(titles(panel, row: 1).contains { $0.hasPrefix("Compare with") })

        let select = try XCTUnwrap(panel.contextItems(forRow: 0).first { $0.title == "Select for Compare" })
        _ = select.target?.perform(select.action, with: select)
        XCTAssertEqual(panel.compareBase, "aaaaaaaa1111")
        XCTAssertEqual(messages.count, 1)

        XCTAssertTrue(titles(panel, row: 1).contains("Compare with Selected (aaaaaaaa)"))
        XCTAssertTrue(titles(panel, row: 0).contains("Unselect for Compare"))
        XCTAssertFalse(titles(panel, row: 0).contains { $0.hasPrefix("Compare with") }, "not against itself")
    }

    func testDiffTextColorsAddedRemovedAndHunkLines() {
        let text = GitDiffText.attributed("@@ -1 +1 @@\n-old\n+new\n same\n")
        let line = { (offset: Int, key: NSAttributedString.Key) in text.attribute(key, at: offset, effectiveRange: nil) }
        XCTAssertNotNil(line(12, .backgroundColor), "removed line")
        XCTAssertNotNil(line(17, .backgroundColor), "added line")
        XCTAssertNil(line(22, .backgroundColor), "context line")
        XCTAssertEqual(GitDiffText.color(forStatus: "A"), .systemGreen)
        XCTAssertEqual(GitDiffText.color(forStatus: "D"), .systemRed)
    }

    // MARK: Real repository

    private func git(_ dir: URL, _ args: String...) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", dir.path] + args
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0, "git \(args.joined(separator: " "))")
    }

    private func makeRepo() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("git-graph-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try git(dir, "init", "-b", "main")
        try git(dir, "config", "user.name", "T")
        try git(dir, "config", "user.email", "t@example.com")
        return dir
    }

    private func commitFile(_ dir: URL, _ name: String, _ text: String, _ message: String) throws {
        try text.write(to: dir.appendingPathComponent(name), atomically: true, encoding: .utf8)
        try git(dir, "add", "-A")
        try git(dir, "commit", "-m", message)
    }

    private func waitUntil(_ timeout: TimeInterval = 10, _ condition: () -> Bool) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > end { return false }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        return true
    }

    private func panel(for dir: URL) throws -> GitGraphPanel {
        let panel = GitGraphPanel()
        panel.workspace = try CoreBridge().core.openWorkspace(path: dir.path)
        panel.refresh()
        XCTAssertTrue(waitUntil { !panel.rows.isEmpty })
        return panel
    }

    private func run(_ panel: GitGraphPanel, row: Int, _ title: String,
                     file: StaticString = #filePath, line: UInt = #line) throws -> String {
        let item = try XCTUnwrap(panel.contextItems(forRow: row).first { $0.title == title },
                                 "missing \(title)", file: file, line: line)
        var message: String?
        panel.onMessage = { message = $0 }
        _ = item.target?.perform(item.action, with: item)
        XCTAssertTrue(waitUntil { message != nil }, file: file, line: line)
        return message ?? ""
    }

    func testHistoryBeyondOnePageIsLoadedWhenScrolledToTheEnd() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        for i in 1...7 { try commitFile(dir, "f.txt", "\(i)\n", "commit \(i)") }
        let panel = GitGraphPanel()
        panel.pageSize = 3
        panel.workspace = try CoreBridge().core.openWorkspace(path: dir.path)
        panel.refresh()
        XCTAssertTrue(waitUntil { !panel.rows.isEmpty })
        // Nothing lays the panel out here, so every page counts as "near the
        // end" and loading continues until the history runs out.
        XCTAssertTrue(waitUntil { panel.rows.count == 7 }, "all 7 commits, not just the first page of 3")
        XCTAssertTrue(waitUntil { !panel.hasMore })
        XCTAssertEqual(panel.rows.last?.commit?.subject, "commit 1", "the oldest commit is reached")
    }

    func testMenuActionsRunAgainstARealRepository() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "a\n", "first")
        try git(dir, "switch", "-c", "feature")
        try commitFile(dir, "f.txt", "f\n", "on feature")
        try git(dir, "switch", "main")
        try commitFile(dir, "m.txt", "m\n", "on main")

        let panel = try panel(for: dir)
        panel.confirm = { _, _, _ in true }
        let rows = panel.rows.compactMap(\.commit)
        let featureRow = try XCTUnwrap(panel.rows.firstIndex { $0.commit?.subject == "on feature" })
        let headRow = try XCTUnwrap(panel.rows.firstIndex { $0.commit?.subject == "on main" })
        XCTAssertEqual(rows.count, 3)

        // HEAD offers Reset Last Commit; others do not. Merge/Rebase only off HEAD.
        XCTAssertTrue(titles(panel, row: headRow).contains("Reset Last Commit (Soft)…"))
        XCTAssertFalse(titles(panel, row: headRow).contains("Merge into current branch…"))
        XCTAssertFalse(titles(panel, row: featureRow).contains("Reset Last Commit (Soft)…"))
        XCTAssertTrue(titles(panel, row: featureRow).contains("Merge into current branch…"))

        XCTAssertEqual(try run(panel, row: featureRow, "Cherry Pick…").prefix(6), "Cherry")
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir.appendingPathComponent("f.txt").path))
    }

    func testDeclinedConfirmationRunsNothing() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "a\n", "first")
        try commitFile(dir, "b.txt", "b\n", "second")
        let panel = try panel(for: dir)
        var asked: [String] = []
        panel.confirm = { title, _, _ in asked.append(title); return false }
        var messages: [String] = []
        panel.onMessage = { messages.append($0) }
        for title in ["Revert…", "Drop…", "Reset Last Commit (Soft)…"] {
            let item = try XCTUnwrap(panel.contextItems(forRow: 0).first { $0.title == title })
            _ = item.target?.perform(item.action, with: item)
        }
        XCTAssertEqual(asked.count, 3)
        XCTAssertTrue(messages.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir.appendingPathComponent("b.txt").path))
    }

    func testConflictingCherryPickReportsAbortAndKeepsTheTree() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "base\n", "base")
        try git(dir, "switch", "-c", "feature")
        try commitFile(dir, "a.txt", "theirs\n", "theirs")
        try git(dir, "switch", "main")
        try commitFile(dir, "a.txt", "ours\n", "ours")

        let panel = try panel(for: dir)
        panel.confirm = { _, _, _ in true }
        let row = try XCTUnwrap(panel.rows.firstIndex { $0.commit?.subject == "theirs" })
        let message = try run(panel, row: row, "Cherry Pick…")
        XCTAssertTrue(message.contains("aborted"), message)
        XCTAssertEqual(try String(contentsOf: dir.appendingPathComponent("a.txt"), encoding: .utf8), "ours\n")
    }

    func testHardResetNeedsASecondConfirmationAndCancelKeepsChanges() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "one\n", "one")
        try commitFile(dir, "a.txt", "two\n", "two")
        try "dirty\n".write(to: dir.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)

        let panel = try panel(for: dir)
        XCTAssertTrue(waitUntil { panel.rows.first?.commit == nil })
        let target = try XCTUnwrap(panel.rows.firstIndex { $0.commit?.subject == "one" })
        panel.chooseResetMode = { _ in .hard }
        var asked: [(String, String)] = []
        panel.confirm = { title, detail, _ in asked.append((title, detail)); return false }
        let item = try XCTUnwrap(panel.contextItems(forRow: target).first { $0.title.hasPrefix("Reset current branch") })
        _ = item.target?.perform(item.action, with: item)
        XCTAssertEqual(asked.count, 1)
        XCTAssertTrue(asked[0].1.contains("1 uncommitted change"), asked[0].1)
        XCTAssertTrue(asked[0].1.contains("cannot be undone"))
        XCTAssertEqual(try String(contentsOf: dir.appendingPathComponent("a.txt"), encoding: .utf8), "dirty\n")
    }

    func testCompareWindowListsAndDiffsChangedFiles() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "one\n", "one")
        try commitFile(dir, "a.txt", "two\n", "two")
        let ws = try CoreBridge().core.openWorkspace(path: dir.path)
        let log = try ws.gitLogGraph(limit: 10)
        let window = GitCompareWindowController(workspace: ws, from: log[1].hash, to: log[0].hash)
        XCTAssertTrue(waitUntil { !window.files.isEmpty })
        XCTAssertEqual(window.files.map(\.path), ["a.txt"])
        XCTAssertEqual(window.files.first?.status, "M")
    }

    // MARK: Toolbar filters

    private func subjects(_ panel: GitGraphPanel) -> [String] { panel.rows.compactMap { $0.commit?.subject } }

    func testAuthorFilterKeepsOnlyThatAuthorsCommits() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "1", "by T")
        try git(dir, "config", "user.name", "Zed")
        try commitFile(dir, "b.txt", "2", "by Zed")
        let panel = try panel(for: dir)
        XCTAssertEqual(subjects(panel).count, 2)

        panel.setAuthorFilter("Zed")
        XCTAssertTrue(waitUntil { self.subjects(panel) == ["by Zed"] }, "\(subjects(panel))")
        XCTAssertEqual(panel.authorFilter, "Zed")

        panel.setAuthorFilter(nil)
        XCTAssertTrue(waitUntil { self.subjects(panel).count == 2 })
    }

    func testBranchFilterStartsFromThatBranch() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "1", "base")
        try git(dir, "switch", "-c", "feature/x")
        try commitFile(dir, "b.txt", "2", "on feature")
        try git(dir, "switch", "main")
        let panel = try panel(for: dir)
        XCTAssertEqual(subjects(panel).count, 2)

        panel.setBranchFilter("main")
        XCTAssertTrue(waitUntil { self.subjects(panel) == ["base"] }, "\(subjects(panel))")

        panel.setBranchFilter("feature/x")
        XCTAssertTrue(waitUntil { self.subjects(panel) == ["on feature", "base"] }, "\(subjects(panel))")
    }

    func testRemoteCheckboxHidesRemoteBranches() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "1", "base")
        try git(dir, "update-ref", "refs/remotes/origin/main", "main")
        let panel = try panel(for: dir)
        let hasRemote = { panel.rows.contains { $0.commit?.refs.contains { $0.kind == .remote } == true } }
        XCTAssertTrue(waitUntil { hasRemote() })

        panel.setShowRemotes(false)
        XCTAssertTrue(waitUntil { !hasRemote() })
        XCTAssertFalse(panel.showRemotes)

        panel.setShowRemotes(true)
        XCTAssertTrue(waitUntil { hasRemote() })
    }

    func testWorkingTreeRowHiddenWhenFilteringByAuthor() throws {
        let dir = try makeRepo()
        defer { try? FileManager.default.removeItem(at: dir) }
        try commitFile(dir, "a.txt", "1", "one")
        try "dirty".write(to: dir.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        let panel = try panel(for: dir)
        XCTAssertTrue(waitUntil { panel.rows.first?.commit == nil })

        panel.setAuthorFilter("T")
        XCTAssertTrue(waitUntil { panel.rows.first?.commit != nil })
    }

    // MARK: Find and HEAD

    func testFindNextWrapsAndMatchesSubjectAuthorHashAndRef() throws {
        let panel = GitGraphPanel()
        var tagged = commit("abcdef0123456789")
        tagged.refs = [GitRefInfo(name: "v2.0", kind: .tag)]
        panel.apply(GitGraphRows.make(commits: [commit("1111111100"), tagged, commit("2222222200")], uncommitted: 0))

        XCTAssertTrue(panel.findNext("v2"))
        XCTAssertTrue(panel.findNext("abcdef"), "hash prefix")
        XCTAssertTrue(panel.findNext("1111"), "wraps to the first row")
        XCTAssertTrue(panel.findNext("A"), "author is 'A'")
        XCTAssertFalse(panel.findNext("nothing like this"))
        XCTAssertFalse(panel.findNext("   "), "blank query never matches")
    }

    func testScrollToHeadReportsWhenHeadIsNotLoaded() throws {
        let panel = GitGraphPanel()
        var messages: [String] = []
        panel.onMessage = { messages.append($0) }
        panel.apply(GitGraphRows.make(commits: [commit("aaaaaaaa1111")], uncommitted: 0))
        panel.scrollToHead()
        XCTAssertEqual(messages.count, 1)

        var head = commit("bbbbbbbb2222")
        head.refs = [GitRefInfo(name: "main", kind: .head)]
        panel.apply(GitGraphRows.make(commits: [commit("aaaaaaaa1111"), head], uncommitted: 0))
        panel.scrollToHead()
        XCTAssertEqual(messages.count, 1, "found: no message")
    }
}
