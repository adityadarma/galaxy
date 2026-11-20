import XCTest
@testable import Galaxy

/// M2-12: the helper is part of the app bundle. Its process exit codes are
/// verified from the shell because XCTest treats a child tool's nonzero exit
/// as a failure of its hosted app.
final class GalaxyCLITests: XCTestCase {
    func testHelperIsBundledInTheAppsExecutableDirectory() {
        let app = Bundle(for: AppDelegate.self).bundleURL
        let helper = app.appendingPathComponent("Contents/Resources/galaxy")
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: helper.path), helper.path)
    }

    @MainActor
    func testInstallLinksTheHelperAndKeepsRegularFiles() throws {
        let files = FileManager.default
        let dir = files.temporaryDirectory.appendingPathComponent("galaxy-cli-\(UUID().uuidString)")
        defer { try? files.removeItem(at: dir) }
        let helper = Bundle(for: AppDelegate.self).bundleURL.appendingPathComponent("Contents/Resources/galaxy")
        let link = dir.appendingPathComponent("bin/galaxy")

        try CommandLineTool.install(helper: helper, link: link)
        XCTAssertEqual(try files.destinationOfSymbolicLink(atPath: link.path), helper.path)
        try CommandLineTool.install(helper: helper, link: link)
        XCTAssertEqual(try files.destinationOfSymbolicLink(atPath: link.path), helper.path, "reinstall replaces the link")

        try files.removeItem(at: link)
        try "mine".write(to: link, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try CommandLineTool.install(helper: helper, link: link))
        XCTAssertEqual(try String(contentsOf: link, encoding: .utf8), "mine", "a real file is never replaced")
        XCTAssertThrowsError(try CommandLineTool.install(helper: dir.appendingPathComponent("missing"),
                                                         link: dir.appendingPathComponent("other")))
    }
}
