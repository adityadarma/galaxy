import XCTest
@testable import Galaxy

@MainActor
final class EmptyEditorViewTests: XCTestCase {
    func testSplitsModifiersAndKeyIntoCaps() {
        XCTAssertEqual(EmptyEditorView.keys(from: "⇧⌘P"), ["⇧", "⌘", "P"])
        XCTAssertEqual(EmptyEditorView.keys(from: "⌘,"), ["⌘", ","])
        XCTAssertEqual(EmptyEditorView.keys(from: "⌃⌥⇧⌘T"), ["⌃", "⌥", "⇧", "⌘", "T"])
    }

    func testKeepsNamedKeysWhole() {
        XCTAssertEqual(EmptyEditorView.keys(from: "⌘Space"), ["⌘", "Space"])
        XCTAssertEqual(EmptyEditorView.keys(from: "⌥⌘↑"), ["⌥", "⌘", "↑"])
    }

    func testShortcutWithoutModifierIsOneCap() {
        XCTAssertEqual(EmptyEditorView.keys(from: "F5"), ["F5"])
        XCTAssertEqual(EmptyEditorView.keys(from: ""), [])
    }

    func testReloadDoesNotStackRows() {
        func labels(in view: NSView) -> Int {
            view.subviews.reduce(view is NSTextField ? 1 : 0) { $0 + labels(in: $1) }
        }
        let view = EmptyEditorView(frame: .zero)
        let first = labels(in: view)
        view.reload()
        view.reload()
        XCTAssertEqual(labels(in: view), first)
    }

    func testStartsHiddenUntilShown() {
        XCTAssertTrue(EmptyEditorView(frame: .zero).isHidden)
    }
}
