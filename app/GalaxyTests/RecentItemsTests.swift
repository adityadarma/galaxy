import XCTest
@testable import Galaxy

@MainActor
final class RecentItemsTests: XCTestCase {
    func testMenuShowsMoreAndClearAndLimitsPreviewToTen() throws {
        let defaults = UserDefaults.standard
        let folders = defaults.object(forKey: "RecentProjectPaths")
        let files = defaults.object(forKey: "RecentFilePaths")
        defer {
            defaults.set(folders, forKey: "RecentProjectPaths")
            defaults.set(files, forKey: "RecentFilePaths")
        }
        defaults.set((0..<12).map { "/somewhere/folder\($0)" }, forKey: "RecentProjectPaths")
        defaults.set(["/somewhere/file.txt"], forKey: "RecentFilePaths")

        let fileMenu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "File" })
        let recent = try XCTUnwrap(fileMenu.items.first { $0.title == "Open Recent" }?.submenu)
        XCTAssertEqual(recent.items.filter { $0.action == #selector(MainWindowController.openRecentProject(_:)) }.count, 10)
        XCTAssertEqual(recent.items.filter { $0.action == #selector(MainWindowController.openRecentFile(_:)) }.count, 1)
        XCTAssertNotNil(recent.items.first { $0.title == "More…" })
        XCTAssertNotNil(recent.items.first { $0.title == "Clear Recently Opened…" })
        XCTAssertEqual(MainWindowController.recentProjectPaths().count, 12, "More retains the full history")

        MainWindowController.clearRecentlyOpened()
        XCTAssertTrue(MainWindowController.recentProjectPaths().isEmpty)
        XCTAssertTrue(MainWindowController.recentFilePaths().isEmpty)
        XCTAssertFalse(try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "File" }?
            .items.first { $0.title == "Open Recent" }?.submenu?
            .items.first { $0.title == "Clear Recently Opened…" }).isEnabled)
    }

    func testPickerSearchRemoveAndOpen() throws {
        let picker = RecentItemsPicker()
        let folder = RecentItemsPicker.Item(path: "/somewhere/galaxy", isFolder: true)
        let file = RecentItemsPicker.Item(path: "/somewhere/rust.rs", isFolder: false)
        picker.show([folder, file])
        XCTAssertEqual(picker.results, [folder, file])
        picker.field.stringValue = "rust"
        picker.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        XCTAssertEqual(picker.results, [file])
        var opened: RecentItemsPicker.Item?
        picker.onOpen = { opened = $0 }
        let editor = NSTextView()
        XCTAssertTrue(picker.control(picker.field, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        XCTAssertEqual(opened, file)
        picker.show([folder, file])
        XCTAssertEqual(picker.results.count, 2)
        picker.field.stringValue = "rust"
        picker.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        picker.show([folder], resetQuery: false)
        XCTAssertTrue(picker.results.isEmpty, "removing a result preserves the current search")
    }

    func testMoreOpensSearchableOverlayAndRemovePreservesTheOtherHistory() throws {
        let defaults = UserDefaults.standard
        let folders = defaults.object(forKey: "RecentProjectPaths")
        let files = defaults.object(forKey: "RecentFilePaths")
        defer {
            defaults.set(folders, forKey: "RecentProjectPaths")
            defaults.set(files, forKey: "RecentFilePaths")
            MainMenu.refreshRecentProjects()
        }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("recent-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("hello.txt")
        try "hello".write(to: file, atomically: true, encoding: .utf8)
        defaults.set([folder.path], forKey: "RecentProjectPaths")
        defaults.set([file.path], forKey: "RecentFilePaths")

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.showRecentItems(nil)
        let picker = try XCTUnwrap(controller.loadedRecentItemsPicker)
        XCTAssertTrue(controller.isRecentItemsVisible)
        XCTAssertEqual(picker.results.count, 2)
        picker.onRemove?(RecentItemsPicker.Item(path: folder.path, isFolder: true))
        XCTAssertTrue(MainWindowController.recentProjectPaths().isEmpty)
        XCTAssertEqual(MainWindowController.recentFilePaths(), [file.path])
        XCTAssertEqual(picker.results, [RecentItemsPicker.Item(path: file.path, isFolder: false)])
        picker.onOpen?(picker.results[0])
        XCTAssertFalse(controller.isRecentItemsVisible)
        XCTAssertEqual(controller.activeTab?.title, "hello.txt")
    }
}
