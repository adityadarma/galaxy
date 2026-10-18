import AppKit
import XCTest
@testable import Galaxy

@MainActor
final class ExtensionsTests: XCTestCase {
    private var pluginsDir: URL!

    override func setUp() {
        super.setUp()
        pluginsDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-plugins-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: pluginsDir, withIntermediateDirectories: true)
        setenv("GALAXY_PLUGINS_DIR", pluginsDir.path, 1)
    }

    override func tearDown() {
        unsetenv("GALAXY_PLUGINS_DIR")
        try? FileManager.default.removeItem(at: pluginsDir)
        super.tearDown()
    }

    private func plugin(_ id: String, builtin: Bool, enabled: Bool = true,
                        available: Bool = true, installable: Bool = false) -> PluginInfo {
        PluginInfo(id: id, name: id, version: "1.0.0", description: "", languages: ["php"],
                   builtin: builtin, enabled: enabled, available: available, installable: installable,
                   source: builtin ? nil : "https://x/y.git", commit: nil)
    }

    private func identifiers(of view: NSView?) -> [String] {
        guard let view else { return [] }
        return ([view] + view.subviews.flatMap { [$0] + $0.subviews }).map { $0.accessibilityIdentifier() }
    }

    func testNotInstalledPluginHasInstallButNoToggle() {
        let controller = ExtensionsController()
        controller.show(plugins: [plugin("missing", builtin: true, available: false, installable: true),
                                  plugin("disabled-missing", builtin: true, enabled: false, available: false, installable: true),
                                  plugin("ready", builtin: true)],
                        available: [], registryErrors: [], hasRegistries: false, updates: [:])
        func ids(_ id: String) -> [String] {
            let row = controller.rows.firstIndex { if case .plugin(let p) = $0 { p.id == id } else { false } }!
            return identifiers(of: controller.tableView(NSTableView(), viewFor: nil, row: row))
        }
        let missing = ids("missing")
        XCTAssertTrue(missing.contains { $0.hasPrefix("extensions-install-builtin-") })
        XCTAssertFalse(missing.contains { $0.hasPrefix("extensions-toggle-") }, "nothing to disable before install")
        XCTAssertTrue(ids("disabled-missing").contains { $0.hasPrefix("extensions-toggle-") }, "already disabled stays re-enable-able")
        XCTAssertTrue(ids("ready").contains { $0.hasPrefix("extensions-toggle-") })
    }

    func testListGroupsInstalledBuiltInAndAvailable() {
        let controller = ExtensionsController()
        let offered = RegistryEntryInfo(id: "remote", name: "Remote", description: "d", version: "2", languages: [],
                                        source: "https://x/r.git", sha256: nil, registry: "reg")
        controller.show(plugins: [plugin("mine", builtin: false), plugin("gopls", builtin: true)],
                        available: [offered], registryErrors: [], hasRegistries: true, updates: [:])
        XCTAssertEqual(controller.rows.first, .header("INSTALLED"))
        XCTAssertTrue(controller.rows.contains(.header("BUILT-IN")))
        XCTAssertTrue(controller.rows.contains(.available(offered)))
        XCTAssertEqual(controller.numberOfRows(in: NSTableView()), controller.rows.count)
    }

    func testEmptyStatesExplainWhatToDo() {
        let controller = ExtensionsController()
        controller.show(plugins: [plugin("gopls", builtin: true)], available: [], registryErrors: [], hasRegistries: false)
        let messages = controller.rows.compactMap { row -> String? in
            if case .message(let text) = row { text } else { nil }
        }
        XCTAssertTrue(messages.contains { $0.contains("Nothing installed") })
        XCTAssertFalse(messages.contains { $0.lowercased().contains("paste a git url") }, "there is no URL field to paste into")
        XCTAssertTrue(messages.contains { $0.contains("No registry") })
    }

    func testAlreadyInstalledPluginsAreNotOfferedAgain() {
        let controller = ExtensionsController()
        let entry = RegistryEntryInfo(id: "mine", name: "Mine", description: "", version: "1", languages: [],
                                      source: "https://x/m.git", sha256: nil, registry: "reg")
        controller.show(plugins: [plugin("mine", builtin: false)], available: [entry], registryErrors: [],
                        hasRegistries: true, updates: [:])
        XCTAssertFalse(controller.rows.contains(.available(entry)))
    }

    func testActivityBarOpensExtensionsSidebar() {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.activityBar.buttons[.extensions]?.performClick(nil)
        XCTAssertEqual(controller.activeSidebarTab, .extensions)
        XCTAssertEqual(controller.sectionHeader.title, "EXTENSIONS")
        XCTAssertFalse(controller.extensions.view.isHidden)
        controller.activityBar.buttons[.explorer]?.performClick(nil)
        XCTAssertTrue(controller.extensions.view.isHidden)
    }

    func testBuiltInPluginsAreListedAndCanBeDisabled() throws {
        let before = pluginList()
        XCTAssertTrue(before.contains { $0.id == "gopls" && $0.builtin && $0.enabled })
        try pluginSetEnabled(id: "gopls", enabled: false)
        XCTAssertTrue(pluginList().contains { $0.id == "gopls" && !$0.enabled })
        try pluginSetEnabled(id: "gopls", enabled: true)
        XCTAssertTrue(pluginList().contains { $0.id == "gopls" && $0.enabled })
    }

    func testBuiltInWithInstallStepsOffersInstallButNothingRunsUntilConfirmed() throws {
        // Which built-ins are installable depends on this machine's PATH, but
        // the rule does not: only a missing server is offered an install.
        for plugin in pluginList() where plugin.installable {
            XCTAssertTrue(plugin.builtin, plugin.id)
            XCTAssertFalse(plugin.available, "\(plugin.id) is on PATH, no install needed")
        }
        XCTAssertFalse(pluginList().contains { $0.id == "rust-analyzer" && $0.installable }, "PATH-only built-ins have no install")

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        var shown: PluginPreview?
        controller.askConfirmPluginInstall = { preview in shown = preview; return false }
        controller.installPlugin(spec: "builtin:phpactor", sha256: nil)
        try waitUntil("preview shown") { shown != nil }
        XCTAssertEqual(shown?.id, "phpactor")
        XCTAssertTrue(shown?.commands.contains { $0.contains("composer require") } ?? false)
        XCTAssertFalse(FileManager.default.fileExists(atPath: pluginsDir.appendingPathComponent("phpactor").path),
                       "declined install leaves no files")
        XCTAssertTrue(pluginList().first { $0.id == "phpactor" }?.builtin ?? false)
    }

    func testInstallFromGitAsksFirstThenInstallsAndUninstalls() throws {
        let repo = try makeRepo()
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        var shown: PluginPreview?
        controller.askConfirmPluginInstall = { preview in shown = preview; return false }
        controller.installPlugin(spec: "file://\(repo.path)#main:plugins/demo", sha256: nil)
        try waitUntil("declined install is reported") { shown != nil }
        XCTAssertEqual(shown?.id, "demo")
        XCTAssertTrue(shown?.commands.contains { $0.hasPrefix("touch ") } ?? false, "commands are shown before running")
        XCTAssertFalse(pluginList().contains { $0.id == "demo" }, "nothing installed after Cancel")

        controller.askConfirmPluginInstall = { _ in true }
        controller.installPlugin(spec: "file://\(repo.path)#main:plugins/demo", sha256: nil)
        try waitUntil("plugin installed") { pluginList().contains { $0.id == "demo" } }
        let installed = try XCTUnwrap(pluginList().first { $0.id == "demo" })
        XCTAssertFalse(installed.builtin)
        XCTAssertNotNil(installed.commit)
        XCTAssertTrue(FileManager.default.fileExists(atPath: pluginsDir.appendingPathComponent("demo/built").path))

        controller.askConfirmPluginUninstall = { _ in true }
        controller.uninstallPlugin(id: "demo")
        XCTAssertFalse(pluginList().contains { $0.id == "demo" })
    }

    func testFailedFetchLeavesAMessageAndInstallsNothing() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.askConfirmPluginInstall = { _ in XCTFail("must not ask"); return false }
        controller.installPlugin(spec: "http://insecure.example/x.git", sha256: nil)
        try waitUntil("error shown") { controller.extensions.statusText.hasPrefix("Could not fetch") }
        XCTAssertEqual(pluginList().filter { !$0.builtin }.count, 0)
    }

    // MARK: Helpers

    private func waitUntil(_ what: String, timeout: TimeInterval = 20, _ condition: () -> Bool) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline { throw XCTSkip("timed out waiting for: \(what)") }
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
    }

    private func makeRepo() throws -> URL {
        let repo = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-repo-\(UUID().uuidString)")
        let plugin = repo.appendingPathComponent("plugins/demo")
        try FileManager.default.createDirectory(at: plugin.appendingPathComponent("bin"), withIntermediateDirectories: true)
        try """
        {"id":"demo","name":"Demo","version":"1.0.0","languages":["zig"],
         "server":{"command":["${pluginDir}/bin/demo-ls"]},
         "install":{"steps":[["touch","${pluginDir}/built"]]}}
        """.write(to: plugin.appendingPathComponent("galaxy-plugin.json"), atomically: true, encoding: .utf8)
        try "#!/bin/sh\n".write(to: plugin.appendingPathComponent("bin/demo-ls"), atomically: true, encoding: .utf8)
        for args in [["init", "-q", "-b", "main"], ["add", "."],
                     ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "one"]] {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = ["git"] + args
            process.currentDirectoryURL = repo
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice
            try process.run()
            process.waitUntilExit()
            XCTAssertEqual(process.terminationStatus, 0, "git \(args)")
        }
        addTeardownBlock { try? FileManager.default.removeItem(at: repo) }
        return repo
    }

    func testSidebarHasNoGitURLFieldOrInstallButton() {
        let controller = ExtensionsController()
        func allViews(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(allViews) }
        let views = allViews(controller.view)
        XCTAssertFalse(views.contains { $0.accessibilityIdentifier() == "extensions-url" })
        XCTAssertFalse(views.contains { $0.accessibilityIdentifier() == "extensions-install" })
        XCTAssertFalse(views.contains { ($0 as? NSTextField)?.isEditable == true }, "nothing to type into here")
    }
}
