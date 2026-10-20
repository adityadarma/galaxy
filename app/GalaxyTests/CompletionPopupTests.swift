import XCTest
@testable import Galaxy

@MainActor
final class CompletionPopupTests: XCTestCase {
    private func item(_ label: String, detail: String? = nil, kind: UInt64? = nil) -> CompletionInfo {
        CompletionInfo(label: label, detail: detail, insertText: label, isSnippet: false, kind: kind)
    }

    private func labels(_ items: [CompletionInfo], _ query: String) -> [String] {
        CompletionPopup.rank(items, query: query).map(\.item.label)
    }

    func testExactAndPrefixMatchesComeBeforeScatteredLetters() {
        let items = [
            item("AccessPermissionSetting", detail: "Google\\Service"),
            item("CommissionChargeTo", detail: "App\\Enums"),
            item("isset"),
            item("issetValue"),
        ]
        let ranked = labels(items, "isset")
        XCTAssertEqual(ranked.first, "isset")
        XCTAssertEqual(ranked, ["isset", "issetValue"])
    }

    func testCamelCaseInitialsMatch() {
        let items = [item("getUserName"), item("gutter"), item("unrelated")]
        XCTAssertEqual(labels(items, "gun"), ["getUserName"])
    }

    func testVariablesMatchPastTheDollar() {
        let items = [item("$order"), item("$other"), item("border")]
        let ranked = labels(items, "or")
        XCTAssertEqual(ranked.first, "$order")
    }

    func testDottedKeysMatchAtWordStarts() {
        let items = [item("app.name"), item("app.debug"), item("database.default")]
        XCTAssertEqual(labels(items, "app.na"), ["app.name"])
    }

    func testEmptyQueryKeepsTheOrderGiven() {
        let items = [item("b"), item("a"), item("c")]
        XCTAssertEqual(labels(items, ""), ["b", "a", "c"])
    }

    func testMatchedIndexesPointAtTheTypedLetters() throws {
        let hit = try XCTUnwrap(CompletionPopup.match(label: "getUserName", query: Array("gun")))
        XCTAssertEqual(hit.matched, [0, 3, 7])
    }

    func testHeightFollowsTheNumberOfRows() {
        let popup = CompletionPopup()
        popup.update([item("alpha"), item("alphabet")], query: "alp")
        let short = popup.preferredSize.height
        popup.update((0..<40).map { item("alpha\($0)") }, query: "alp")
        XCTAssertGreaterThan(popup.preferredSize.height, short)
        popup.update((0..<400).map { item("alpha\($0)") }, query: "alp")
        XCTAssertLessThan(popup.preferredSize.height, 300)
    }
}
