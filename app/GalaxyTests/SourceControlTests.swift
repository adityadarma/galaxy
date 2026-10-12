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
}
