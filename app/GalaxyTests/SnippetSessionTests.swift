import XCTest
@testable import Galaxy

@MainActor
final class SnippetSessionTests: XCTestCase {
    func testDocumentGrowthExpandsCurrentAndShiftsLaterStops() {
        var session = SnippetSession(
            stops: [[
                .init(index: 1, location: 10, length: 2),
                .init(index: 1, location: 20, length: 2),
            ], [
                .init(index: 2, location: 30, length: 1),
            ]],
            documentLength: 40
        )
        session.noteDocumentChange(length: 43)
        XCTAssertEqual(session.stops[0][0].length, 5)
        XCTAssertEqual(session.stops[0][1].length, 5)
        XCTAssertEqual(session.stops[1][0].location, 33)
    }

    func testNavigationMovesByTabstopGroup() {
        var session = SnippetSession(stops: [
            [.init(index: 1, location: 1, length: 2)],
            [.init(index: 2, location: 4, length: 1)],
            [.init(index: 0, location: 6, length: 0)],
        ], documentLength: 7)
        XCTAssertEqual(session.currentStop?.first?.index, 1)
        XCTAssertEqual(session.move(1)?.first?.index, 2)
        XCTAssertEqual(session.move(1)?.first?.index, 0)
        XCTAssertNil(session.move(1))
        XCTAssertTrue(session.isFinished)
    }

    func testSpanCoversAllMirrorsAndFinalStop() {
        let session = SnippetSession(stops: [
            [.init(index: 1, location: 10, length: 3),
             .init(index: 1, location: 30, length: 3)],
            [.init(index: 0, location: 40, length: 0)],
        ], documentLength: 41)
        XCTAssertEqual(session.span?.start, 10)
        XCTAssertEqual(session.span?.end, 40)
    }
}
