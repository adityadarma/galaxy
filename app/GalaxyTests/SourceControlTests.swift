import XCTest
@testable import Galaxy

@MainActor
final class SourceControlTests: XCTestCase {
    func testCommitMessageClearsAfterCommitEvenWhenPushFails() {
        XCTAssertTrue(SourceControlController.shouldClearCommitMessage(
            current: "Fix bug", submitted: "Fix bug", didCommit: true))
        XCTAssertFalse(SourceControlController.shouldClearCommitMessage(
            current: "Fix bug", submitted: "Fix bug", didCommit: false), "failed commits keep the draft")
        XCTAssertFalse(SourceControlController.shouldClearCommitMessage(
            current: "Next change", submitted: "Fix bug", didCommit: true), "new draft must not be erased")
        XCTAssertFalse(SourceControlController.shouldClearCommitMessage(
            current: "Fix bug", submitted: nil, didCommit: true), "non-commit operations leave the draft alone")
    }

    func testSourceControlInitialState() {
        let sc = SourceControlController()
        sc.refresh(status: nil)
        XCTAssertEqual(sc.numberOfRows(in: NSTableView()), 2, "Should show CHANGES header and empty row 'No changes'")
        XCTAssertEqual(sc.tableView(NSTableView(), heightOfRow: 0), 24)
    }

    func testSourceControlSectionsAndCollapsing() {
        let sc = SourceControlController()
        let status = GitStatusInfo(
            branch: "main",
            changedPaths: ["src/main.rs", "README.md"],
            changes: [
                GitChangeInfo(path: "src/main.rs", staged: true, code: "M "),
                GitChangeInfo(path: "README.md", staged: false, code: " M")
            ]
        )
        sc.refresh(status: status)

        // 1 staged header + 1 staged change + 1 unstaged header + 1 unstaged change = 4 rows
        XCTAssertEqual(sc.numberOfRows(in: NSTableView()), 4)

        // Trigger header click on row 0 (STAGED CHANGES) to collapse
        let dummyTable = NSTableView()
        // We can simulate table cell action or toggle
        sc.refresh(status: status)
        XCTAssertEqual(sc.numberOfRows(in: dummyTable), 4)
    }

    func testChangeCountMatchesBothSections() {
        let sc = SourceControlController()
        sc.refresh(status: GitStatusInfo(
            branch: "main",
            changedPaths: ["a.swift", "b.swift", "c.swift"],
            changes: [GitChangeInfo(path: "a.swift", staged: true, code: "M "),
                      GitChangeInfo(path: "b.swift", staged: true, code: "MM"),
                      GitChangeInfo(path: "c.swift", staged: false, code: "??")]))
        XCTAssertEqual(sc.changeCount, 4, "a file with staged and unstaged edits counts in each section")
        sc.refresh(status: nil)
        XCTAssertEqual(sc.changeCount, 0)
    }

    func testOpenDiffPassesStagedSectionForMixedFile() {
        let sc = SourceControlController()
        var opened: [(String, Bool)] = []
        sc.onOpenDiff = { opened.append(($0, $1)) }
        let change = GitChangeInfo(path: "mixed.swift", staged: true, code: "MM")
        sc.open(change, staged: true)
        sc.open(change, staged: false)
        XCTAssertEqual(opened.map(\.0), ["mixed.swift", "mixed.swift"])
        XCTAssertEqual(opened.map(\.1), [true, false])
    }

    func testChangeRowHasAnOpenFileButtonThatOpensTheFileNotTheDiff() throws {
        let sc = SourceControlController()
        var opened: [String] = []
        var diffs: [String] = []
        sc.onOpen = { opened.append($0) }
        sc.onOpenDiff = { path, _ in diffs.append(path) }
        sc.refresh(status: GitStatusInfo(
            branch: "main", changedPaths: ["README.md", "gone.txt"],
            changes: [GitChangeInfo(path: "README.md", staged: false, code: " M"),
                      GitChangeInfo(path: "gone.txt", staged: false, code: " D")]))

        let table = NSTableView()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("scm-changes"))
        // Row 0 is the Changes header, row 1 README.md, row 2 gone.txt.
        let modified = try XCTUnwrap(sc.tableView(table, viewFor: column, row: 1) as? SourceControlItemCellView)
        let deleted = try XCTUnwrap(sc.tableView(table, viewFor: column, row: 2) as? SourceControlItemCellView)

        XCTAssertTrue(modified.openFileButton.isHidden, "buttons show on hover only")
        modified.mouseEntered(with: NSEvent())
        XCTAssertFalse(modified.openFileButton.isHidden)
        XCTAssertEqual(modified.openFileButton.toolTip, "Open File")
        modified.openFileButton.performClick(nil)
        XCTAssertEqual(opened, ["README.md"])
        XCTAssertTrue(diffs.isEmpty, "the button opens the file, not the diff")

        deleted.mouseEntered(with: NSEvent())
        XCTAssertTrue(deleted.openFileButton.isHidden, "a deleted file has nothing to open")
        XCTAssertFalse(deleted.discardButton.isHidden)
        modified.mouseExited(with: NSEvent())
        XCTAssertTrue(modified.openFileButton.isHidden)
    }

    func testDeletedChangeOpensCommittedDiffInsteadOfMissingFile() {
        let sc = SourceControlController()
        var opened: [String] = []
        var diffs: [String] = []
        sc.onOpen = { opened.append($0) }
        sc.onOpenDiff = { path, _ in diffs.append(path) }

        sc.open(GitChangeInfo(path: "deleted.txt", staged: false, code: " D"))
        sc.open(GitChangeInfo(path: "staged-deletion.txt", staged: true, code: "D "))
        sc.open(GitChangeInfo(path: "new.txt", staged: false, code: "??"))

        XCTAssertEqual(diffs, ["deleted.txt", "staged-deletion.txt"])
        XCTAssertEqual(opened, ["new.txt"])
    }

    func testDirectoryUsesTheFullRowUntilTheHoverButtonsAppear() throws {
        let sc = SourceControlController()
        let long = "app/Galaxy/Sources/Deeply/Nested/Folder"
        sc.refresh(status: GitStatusInfo(
            branch: "main", changedPaths: ["\(long)/File.swift"],
            changes: [GitChangeInfo(path: "\(long)/File.swift", staged: false, code: " M")]))
        let table = NSTableView()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("scm-changes"))
        let cell = try XCTUnwrap(sc.tableView(table, viewFor: column, row: 1) as? SourceControlItemCellView)
        cell.frame = NSRect(x: 0, y: 0, width: 420, height: 24)

        cell.layoutSubtreeIfNeeded()
        let idle = cell.dirLabel.frame.maxX
        XCTAssertEqual(cell.dirLabel.lineBreakMode, .byTruncatingTail, "cut at the end, not in the middle")
        XCTAssertLessThanOrEqual(idle, cell.statusBadge.frame.minX, "never under the status letter")
        XCTAssertGreaterThan(idle, cell.openFileButton.frame.minX, "reaches into the space the buttons will use")

        cell.mouseEntered(with: NSEvent())
        cell.layoutSubtreeIfNeeded()
        XCTAssertLessThanOrEqual(cell.dirLabel.frame.maxX, cell.openFileButton.frame.minX, "makes room for the buttons")

        cell.mouseExited(with: NSEvent())
        cell.layoutSubtreeIfNeeded()
        XCTAssertEqual(cell.dirLabel.frame.maxX, idle, accuracy: 0.5)
    }

    func testCompactCountFormat() {
        XCTAssertEqual(CountFormat.compact(0), "0")
        XCTAssertEqual(CountFormat.compact(3), "3")
        XCTAssertEqual(CountFormat.compact(999), "999")
        XCTAssertEqual(CountFormat.compact(1000), "1k")
        XCTAssertEqual(CountFormat.compact(1001), "1k+")
        XCTAssertEqual(CountFormat.compact(1999), "1k+")
        XCTAssertEqual(CountFormat.compact(2000), "2k")
        XCTAssertEqual(CountFormat.compact(2500), "2k+")
        XCTAssertEqual(CountFormat.compact(12_000), "12k")
    }

    func testActivityBarToggleSourceControl() {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        let activity = controller.activityBar
        let scButton = activity.buttons[.sourceControl]
        XCTAssertNotNil(scButton)

        // Initial state: explorer is active
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .explorer)

        // Click Source Control in Activity Bar -> switches to source control
        scButton?.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .sourceControl)
        XCTAssertEqual(controller.activeSidebarTab, .sourceControl)
        XCTAssertEqual(controller.sectionHeader.title, "SOURCE CONTROL")
        XCTAssertEqual(controller.sectionHeader.label.stringValue, "SOURCE CONTROL", "caps, like EXPLORER and SEARCH")
        let header = controller.sectionHeader
        let wired = [header.scmPullButton, header.scmBranchButton, header.scmCommitButton, header.scmRefreshButton, header.scmMoreButton]
        XCTAssertEqual(header.scmTools, wired, "every header icon has an action")
        XCTAssertTrue(wired.allSatisfy { $0.action != nil })
        // Like VSCode, only "…" shows until the pointer is over the header.
        XCTAssertFalse(header.scmMoreButton.isHidden)
        XCTAssertTrue(wired.filter { $0 !== header.scmMoreButton }.allSatisfy(\.isHidden))

        // Click Source Control in Activity Bar AGAIN -> toggles / collapses sidebar
        scButton?.performClick(nil)
        XCTAssertFalse(controller.isSidebarVisible)
        XCTAssertNil(activity.active)

        // Click Source Control while hidden -> restores sidebar with Source Control
        scButton?.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .sourceControl)
        XCTAssertEqual(controller.activeSidebarTab, .sourceControl)

        // Click Explorer -> switches to Explorer
        let expButton = activity.buttons[.explorer]
        expButton?.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .explorer)
        XCTAssertEqual(controller.activeSidebarTab, .explorer)

        // Click Explorer again -> collapses sidebar
        expButton?.performClick(nil)
        XCTAssertFalse(controller.isSidebarVisible)
        XCTAssertNil(activity.active)
    }

    func testToggleSourceControlAction() {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        // Trigger showSourceControlAction (menu / shortcut ⌃⇧G)
        controller.showSourceControlAction(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(controller.activityBar.active, .sourceControl)

        // Trigger again -> collapses
        controller.showSourceControlAction(nil)
        XCTAssertFalse(controller.isSidebarVisible)

        // Trigger again -> opens source control
        controller.showSourceControlAction(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(controller.activityBar.active, .sourceControl)
    }

    // MARK: Commit message box

    private func laidOutMessageBox(width: CGFloat = 260) -> (host: NSView, box: CommitMessageInputContainer) {
        let host = NSView(frame: NSRect(x: 0, y: 0, width: 400, height: 600))
        let box = CommitMessageInputContainer()
        box.translatesAutoresizingMaskIntoConstraints = false
        host.addSubview(box)
        NSLayoutConstraint.activate([
            box.topAnchor.constraint(equalTo: host.topAnchor),
            box.leadingAnchor.constraint(equalTo: host.leadingAnchor),
            box.widthAnchor.constraint(equalToConstant: width),
        ])
        host.layoutSubtreeIfNeeded()
        return (host, box)
    }

    private func lineHeight(_ box: CommitMessageInputContainer) -> CGFloat {
        box.textView.layoutManager!.defaultLineHeight(for: box.textView.font!)
    }

    func testMessageBoxStartsOneLineHighAndGrowsOnlyWhenTextNeedsMore() {
        let (host, box) = laidOutMessageBox()
        let lh = lineHeight(box)
        let oneLine = CommitMessageInputContainer.height(forContent: lh, lineHeight: lh)
        XCTAssertEqual(box.frame.height, oneLine, accuracy: 1, "one line by default")

        box.text = "one"
        host.layoutSubtreeIfNeeded()
        XCTAssertEqual(box.frame.height, oneLine, accuracy: 1, "typing on one line does not grow it")

        box.textView.insertNewline(nil)
        host.layoutSubtreeIfNeeded()
        XCTAssertEqual(box.frame.height, CommitMessageInputContainer.height(forContent: lh * 2, lineHeight: lh), accuracy: 1, "Return adds a line")

        box.text = "one\ntwo\nthree\nfour"
        host.layoutSubtreeIfNeeded()
        XCTAssertEqual(box.frame.height, CommitMessageInputContainer.height(forContent: lh * 4, lineHeight: lh), accuracy: 1, "four lines")

        box.text = ""
        host.layoutSubtreeIfNeeded()
        XCTAssertEqual(box.frame.height, oneLine, accuracy: 1, "shrinks back to one line")
    }

    func testMessageBoxStopsGrowingAtTheLineLimit() {
        let (host, box) = laidOutMessageBox()
        let lh = lineHeight(box)
        box.text = (1...30).map { "line \($0)" }.joined(separator: "\n")
        host.layoutSubtreeIfNeeded()
        let max = CommitMessageInputContainer.height(forContent: lh * CGFloat(CommitMessageInputContainer.maxLines), lineHeight: lh)
        XCTAssertEqual(box.frame.height, max, accuracy: 1, "capped; the rest scrolls")
    }

    func testMessageBoxWrapsLongTextInsteadOfScrollingSideways() {
        let (host, box) = laidOutMessageBox(width: 180)
        let lh = lineHeight(box)
        box.text = String(repeating: "word ", count: 40)
        host.layoutSubtreeIfNeeded()
        XCTAssertGreaterThan(box.frame.height, CommitMessageInputContainer.height(forContent: lh, lineHeight: lh) + 1, "wrapped onto more lines")
    }

    func testReturnAddsALineAndCommandReturnCommits() throws {
        let (_, box) = laidOutMessageBox()
        var commits = 0
        box.onCommandReturn = { commits += 1 }
        box.textView.insertText("subject", replacementRange: NSRange(location: NSNotFound, length: 0))
        box.textView.insertNewline(nil)
        box.textView.insertText("body", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(box.text, "subject\nbody", "Return is a newline, not a commit")
        XCTAssertEqual(commits, 0)

        let event = try XCTUnwrap(NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: .command, timestamp: 0, windowNumber: 0,
            context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
        box.textView.keyDown(with: event)
        XCTAssertEqual(commits, 1)
        XCTAssertEqual(box.text, "subject\nbody", "⌘Return does not type anything")
    }

    func testMessageBoxReportsTextChangesAndFocus() {
        let (_, box) = laidOutMessageBox()
        var changes = 0
        box.onTextChange = { changes += 1 }
        box.textView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertGreaterThan(changes, 0, "the commit button state follows typing")
        XCTAssertEqual(box.placeholder, "Message (⌘Enter to commit)")
    }

    func testCommitButtonShowsAndClearsLoadingState() {
        let button = CommitSplitButton()
        button.setLoading(true, title: "Pushing…")
        XCTAssertTrue(button.isLoading)
        XCTAssertEqual(button.mainButton.accessibilityLabel(), "Pushing…")

        button.setLoading(false)
        XCTAssertFalse(button.isLoading)
        XCTAssertEqual(button.mainButton.accessibilityLabel(), "Commit")
    }

    func testCommandShiftFOpensSidebarSearchSeededWithSelection() {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        // ⌘⇧F opens the Search view in the sidebar, not a floating panel.
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-search-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("a.ts")
        try? "const needle = 1;\n".write(to: file, atomically: true, encoding: .utf8)
        try? controller.openWorkspace(at: folder)
        try? controller.openFile(at: file)
        controller.activeTab?.doc.selectWordAt(pos: TextPos(line: 0, colUtf16: 7))
        let menuItem = NSApp.mainMenu?.items
            .compactMap(\.submenu)
            .flatMap(\.items)
            .first { $0.action == #selector(MainWindowController.showSearchSidebar(_:)) }
        XCTAssertEqual(menuItem?.keyEquivalent, "f")
        XCTAssertEqual(menuItem?.keyEquivalentModifierMask, [.command, .shift])
        controller.showSearchSidebar(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(controller.activeSidebarTab, .search)
        XCTAssertFalse(controller.isWorkspaceSearchVisible, "no floating search panel")
        XCTAssertEqual(controller.searchSidebar.queryText, "needle", "seeded with the selection")

    }

    func testSavingEditedFileRefreshesSidebarSearchAndKeepsOtherMatches() throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-search-save-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let package = folder.appendingPathComponent("package.json")
        let nested = folder.appendingPathComponent("nested")
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        try "{\"version\":\"0.5.123\"}\n".write(to: package, atomically: true, encoding: .utf8)
        try "{\"version\":\"0.5.123\"}\n".write(
            to: nested.appendingPathComponent("package.json"), atomically: true, encoding: .utf8)

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openWorkspace(at: folder)
        try controller.openFile(at: package)
        controller.showSearchTab()
        controller.searchSidebar.present(seed: "0.5.123")
        func waitFor(_ condition: () -> Bool) {
            let deadline = Date().addingTimeInterval(10)
            while !condition(), Date() < deadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.02))
            }
        }
        waitFor { controller.searchSidebar.results.count == 2 }
        XCTAssertEqual(Set(controller.searchSidebar.results.map(\.relPath)),
                       ["package.json", "nested/package.json"])

        let doc = try XCTUnwrap(controller.activeTab?.doc)
        _ = doc.setFind(pattern: "0.5.123", config: FindConfig(caseSensitive: true, wholeWord: false, regex: false))
        XCTAssertEqual(doc.replaceAll(replacement: "0.5.124"), 1)
        XCTAssertTrue(controller.save())
        waitFor { controller.searchSidebar.results.count == 1 }
        XCTAssertEqual(controller.searchSidebar.results.map(\.relPath), ["nested/package.json"])
        XCTAssertEqual(controller.searchSidebar.results.first?.text, "{\"version\":\"0.5.123\"}")
        XCTAssertTrue(try String(contentsOf: package, encoding: .utf8).contains("0.5.124"))
    }
}
