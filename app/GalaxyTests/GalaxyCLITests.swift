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
        XCTAssertTrue(CommandLineTool.isInstalled(helper: helper, link: link))
        try CommandLineTool.install(helper: helper, link: link)
        XCTAssertEqual(try files.destinationOfSymbolicLink(atPath: link.path), helper.path, "reinstall replaces the link")

        let other = dir.appendingPathComponent("another-helper")
        try files.createSymbolicLink(at: other, withDestinationURL: helper)
        XCTAssertTrue(CommandLineTool.isInstalled(helper: helper, link: other), "a link to the helper is installed")
        try files.removeItem(at: other)
        try files.createSymbolicLink(at: other, withDestinationURL: URL(fileURLWithPath: "/usr/bin/true"))
        XCTAssertFalse(CommandLineTool.isInstalled(helper: helper, link: other), "a link to a different executable can be updated")
        try files.removeItem(at: other)
        try files.createSymbolicLink(atPath: other.path, withDestinationPath: "bin/galaxy")
        XCTAssertTrue(CommandLineTool.isInstalled(helper: helper, link: other), "relative links resolve from their directory")

        try files.removeItem(at: link)
        try "mine".write(to: link, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try CommandLineTool.install(helper: helper, link: link))
        XCTAssertFalse(CommandLineTool.isInstalled(helper: helper, link: link), "a regular file is not installed")
        XCTAssertEqual(try String(contentsOf: link, encoding: .utf8), "mine", "a real file is never replaced")
        XCTAssertThrowsError(try CommandLineTool.install(helper: dir.appendingPathComponent("missing"),
                                                         link: dir.appendingPathComponent("other")))
    }

    @MainActor
    func testInstallMenuIsHiddenOnlyForCurrentAppHelper() throws {
        let delegate = AppDelegate()
        let menu = MainMenu.build().item(at: 0)!.submenu!
        let item = try XCTUnwrap(menu.items.first { $0.action == #selector(AppDelegate.installCommandLineTool(_:)) })
        // The actual user link is never changed by this test.
        let original = CommandLineTool.defaultLinkURL
        XCTAssertEqual(CommandLineTool.isInstalled(), CommandLineTool.isInstalled(link: original))
        XCTAssertEqual(item.title, "Install 'galaxy' Command in PATH")
        delegate.menuWillOpen(menu)
        XCTAssertEqual(item.isHidden, CommandLineTool.isInstalled())
    }
}
