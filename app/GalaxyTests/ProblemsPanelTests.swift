import XCTest
@testable import Galaxy

@MainActor
final class ProblemsPanelTests: XCTestCase {
    func testProblemDirectoryIsRelativeToWorkspaceRoot() {
        XCTAssertEqual(
            ProblemsPanel.displayDirectory(
                for: "/Users/test/project/src/components/Button.tsx",
                workspaceRoot: "/Users/test/project"
            ),
            "src/components"
        )
        XCTAssertEqual(
            ProblemsPanel.displayDirectory(
                for: "/Users/test/project/main.ts",
                workspaceRoot: "/Users/test/project"
            ),
            "."
        )
    }

    func testProblemDirectoryKeepsExternalPathDistinct() {
        XCTAssertEqual(
            ProblemsPanel.displayDirectory(
                for: "/Users/test/external/tool.ts",
                workspaceRoot: "/Users/test/project"
            ),
            "/Users/test/external"
        )
    }
}
