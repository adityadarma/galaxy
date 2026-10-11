import XCTest
@testable import Galaxy

@MainActor
final class OutlinePickerTests: XCTestCase {
    private func symbols() -> [OutlineSymbol] {
        [
            OutlineSymbol(uri: nil, name: "Account", kind: "class", line: 2, column: 0),
            OutlineSymbol(uri: nil, name: "deposit", kind: "method", line: 4, column: 4),
            OutlineSymbol(uri: nil, name: "main", kind: "function", line: 10, column: 0),
        ]
    }

    func testPickerFiltersAndReportsSelectedSymbol() {
        let picker = OutlinePicker()
        picker.reset(symbols: symbols())
        XCTAssertEqual(picker.results.map(\.name), ["Account", "deposit", "main"])
        picker.setQuery("pos")
        XCTAssertEqual(picker.results.map(\.name), ["deposit"])
        var chosen: OutlineSymbol?
        picker.onGo = { chosen = $0 }
        picker.goSelected()
        XCTAssertEqual(chosen?.name, "deposit")
        XCTAssertEqual(chosen?.line, 4)
    }
}
