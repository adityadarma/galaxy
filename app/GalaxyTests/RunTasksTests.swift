import XCTest
@testable import Galaxy

final class RunTasksTests: XCTestCase {
    private func folder(_ files: [String: String]) throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-tasks-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        for (name, text) in files { try text.write(to: root.appendingPathComponent(name), atomically: true, encoding: .utf8) }
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        return root
    }

    func testDetectsTheProjectKindFromItsFiles() throws {
        XCTAssertEqual(RunTasks.detect(in: try folder(["Cargo.toml": ""])),
                       .init(build: "cargo build", test: "cargo test"))
        XCTAssertEqual(RunTasks.detect(in: try folder(["Package.swift": ""])),
                       .init(build: "swift build", test: "swift test"))
        XCTAssertEqual(RunTasks.detect(in: try folder(["go.mod": ""])),
                       .init(build: "go build ./...", test: "go test ./..."))
        XCTAssertEqual(RunTasks.detect(in: try folder([:])), .init(build: nil, test: nil))
    }

    func testNodeUsesItsScriptsAndLockfile() throws {
        let scripts = #"{ "scripts": { "build": "vite build", "test": "vitest" } }"#
        XCTAssertEqual(RunTasks.detect(in: try folder(["package.json": scripts])),
                       .init(build: "npm run build", test: "npm test"))
        XCTAssertEqual(RunTasks.detect(in: try folder(["package.json": scripts, "pnpm-lock.yaml": ""])),
                       .init(build: "pnpm run build", test: "pnpm test"))
        XCTAssertEqual(RunTasks.detect(in: try folder(["package.json": #"{ "scripts": { "dev": "x" } }"#])),
                       .init(build: nil, test: nil), "no build or test script")
    }

    func testLaravelTestsRunThroughArtisan() throws {
        let scripts = #"{ "scripts": { "build": "vite build" } }"#
        XCTAssertEqual(RunTasks.detect(in: try folder(["package.json": scripts, "artisan": ""])),
                       .init(build: "npm run build", test: "php artisan test"))
    }

    func testMakefileTestTargetIsOptional() throws {
        XCTAssertEqual(RunTasks.detect(in: try folder(["Makefile": "all:\n\techo\n"])),
                       .init(build: "make", test: nil))
        XCTAssertEqual(RunTasks.detect(in: try folder(["Makefile": "all:\n\techo\n\ntest:\n\techo\n"])),
                       .init(build: "make", test: "make test"))
    }

    @MainActor
    func testRunMenuAndTaskWithoutAWorkspace() throws {
        let menu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Run" })
        XCTAssertNotNil(menu.item(withTitle: "Run Build Task"))
        XCTAssertNotNil(menu.item(withTitle: "Run Test Task"))
        Keybindings.defaults.apply(to: menu)
        let build = try XCTUnwrap(menu.item(withTitle: "Run Build Task"))
        XCTAssertEqual(build.keyEquivalent, "b")
        XCTAssertEqual(build.keyEquivalentModifierMask, [.command, .shift])
        let c = MainWindowController(bridge: CoreBridge())
        XCTAssertFalse(c.validateMenuItem(build), "needs a folder")
    }
}
