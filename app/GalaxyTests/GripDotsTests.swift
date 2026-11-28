import XCTest
@testable import Galaxy

/// The dots drawn in the draggable gaps between the sidebar, the editor
/// and the bottom panel.
@MainActor
final class GripDotsTests: XCTestCase {
    func testDotsAreCentredAlongTheGap() {
        let sideBySide = NSRect(x: 100, y: 0, width: 6, height: 400)
        let stacked = GripDots.rects(centeredIn: sideBySide, vertical: true)
        XCTAssertEqual(stacked.count, 3)
        XCTAssertEqual(Set(stacked.map(\.midX)), [103])
        XCTAssertEqual(stacked[1].midY, 200, accuracy: 0.001)
        XCTAssertLessThan(stacked[0].maxY, stacked[1].minY)

        let above = NSRect(x: 0, y: 50, width: 800, height: 6)
        let row = GripDots.rects(centeredIn: above, vertical: false)
        XCTAssertEqual(Set(row.map(\.midY)), [53])
        XCTAssertEqual(row[1].midX, 400, accuracy: 0.001)
        XCTAssertLessThan(row[0].maxX, row[1].minX)
        for dot in stacked + row {
            XCTAssertEqual(dot.width, GripDots.diameter)
        }
    }

    func testTheBottomGapShowsDotsOnlyWhileThePanelIsOpen() {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        XCTAssertTrue(c.bottomPanelResizeHandle.isHidden)
        c.setBottomPanelOpen(true)
        c.window?.layoutIfNeeded()
        XCTAssertFalse(c.bottomPanelResizeHandle.isHidden)
        XCTAssertEqual(c.bottomPanelResizeHandle.frame.height, Theme.cardGap)
    }
}
