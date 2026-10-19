import XCTest
@testable import Galaxy

/// Opt-in: run with `TEST_RUNNER_GALAXY_LARAVEL_PATH=/path/to/laravel-app`.
/// Leaving a folder must stop its language servers and their helpers.
@MainActor
final class LspLifecycleEndToEndTests: XCTestCase {
    func testLeavingAFolderStopsItsServers() throws {
        guard let path = ProcessInfo.processInfo.environment["GALAXY_LARAVEL_PATH"] else {
            throw XCTSkip("GALAXY_LARAVEL_PATH not set")
        }
        let root = URL(fileURLWithPath: path)
        let empty = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-empty-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: empty, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: empty) }

        let bridge = CoreBridge()
        let c = MainWindowController(bridge: bridge)
        c.showWindow(nil)
        c.onWorkspaceLeft = { [weak bridge] left in
            bridge?.sweepLSP(open: c.openDocuments, releaseRoot: left, idleSeconds: 600)
        }
        try c.openWorkspace(at: root)
        let started = bridge.lspStatus().processes
        XCTAssertFalse(started.isEmpty, "no server started for the Laravel project")
        let pids = started.map { Int32(truncatingIfNeeded: $0.pid) }
        func alive(_ pid: Int32) -> Bool { kill(pid, 0) == 0 }
        XCTAssertTrue(pids.allSatisfy(alive))

        try c.openWorkspace(at: empty)
        XCTAssertEqual(bridge.lspStatus().servers, 0, "servers of the left folder still run")
        let deadline = Date().addingTimeInterval(5)
        while pids.contains(where: alive), Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        }
        XCTAssertFalse(pids.contains(where: alive), "server processes survived: \(pids.filter(alive))")
        bridge.shutdownLSP()
    }
}
