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
}
