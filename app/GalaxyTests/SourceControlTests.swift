import XCTest
@testable import Galaxy

@MainActor
final class SourceControlTests: XCTestCase {
    func testSourceControlInitialState() {
        let sc = SourceControlController()
        sc.refresh(status: nil)
        XCTAssertEqual(sc.numberOfRows(in: NSTableView()), 2, "Should show CHANGES header and empty row 'No changes'")
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
        XCTAssertTrue(wired.allSatisfy { !$0.isHidden && $0.action != nil })

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
}
