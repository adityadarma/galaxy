import XCTest
@testable import Galaxy

@MainActor
final class NavigationTests: XCTestCase {
    private func makeController(text: String = "") throws -> MainWindowController {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        controller.newDocument(nil)
        let view = controller.editorView
        view.pasteboard = NSPasteboard(name: NSPasteboard.Name("galaxy-test-\(UUID().uuidString)"))
        if !text.isEmpty { view.document?.edit(command: .insertText(text: text)) }
        view.document?.moveCursors(motion: .docStart, extend: false)
        view.didChangeDocument()
        view.flushPendingUpdates()
        return controller
    }

    private func selection(_ view: EditorView) -> (Int, Int) {
        let r = view.selectedRange()
        return (r.location, r.length)
    }

    private func key(_ view: EditorView, _ selector: Selector) {
        view.doCommand(by: selector)
    }

    private func click(_ view: EditorView, line: Int, utf16Col: Int, count: Int = 1,
                       flags: NSEvent.ModifierFlags = [], type: NSEvent.EventType = .leftMouseDown) {
        let text = view.document?.lines(first: UInt64(line), count: 1).first ?? ""
        let x = view.textOriginX + view.xOffset(in: text, column: UInt64(utf16Col)) + 0.5
        let y = CGFloat(line) * view.metrics.lineHeight + view.metrics.lineHeight / 2
        let point = view.convert(NSPoint(x: x, y: y), to: nil)
        let event = NSEvent.mouseEvent(
            with: type, location: point, modifierFlags: flags, timestamp: 0,
            windowNumber: view.window?.windowNumber ?? 0, context: nil,
            eventNumber: 0, clickCount: count, pressure: 1)!
        if type == .leftMouseDragged { view.mouseDragged(with: event) } else { view.mouseDown(with: event) }
    }

    func testArrowKeysAndShiftSelection() throws {
        let c = try makeController(text: "hello world\nfoo")
        let v = c.editorView
        key(v, #selector(NSResponder.moveRight(_:)))
        key(v, #selector(NSResponder.moveRight(_:)))
        XCTAssertEqual(selection(v).0, 2)
        key(v, #selector(NSResponder.moveRightAndModifySelection(_:)))
        XCTAssertEqual(selection(v).0, 2)
        XCTAssertEqual(selection(v).1, 1)
        key(v, #selector(NSResponder.moveWordRightAndModifySelection(_:)))
        XCTAssertEqual(selection(v).1, 3, "extends to the end of 'hello'")
        key(v, #selector(NSResponder.moveLeft(_:)))
        XCTAssertEqual(selection(v).0, 2, "Left collapses to the selection start")
        XCTAssertEqual(selection(v).1, 0)
        key(v, #selector(NSResponder.moveDown(_:)))
        key(v, #selector(NSResponder.moveToEndOfLine(_:)))
        XCTAssertEqual(selection(v).0, 15)
        key(v, #selector(NSResponder.moveToBeginningOfDocumentAndModifySelection(_:)))
        XCTAssertEqual(selection(v).0, 0)
        XCTAssertEqual(selection(v).1, 15)
    }

    func testHomeEndPageKeysMoveTheCaret() throws {
        let lines = (0..<200).map { "line \($0)" }.joined(separator: "\n")
        let c = try makeController(text: lines)
        let v = c.editorView
        key(v, #selector(NSResponder.scrollToEndOfDocument(_:)))
        XCTAssertEqual(selection(v).0, 6, "End moves to the end of \"line 0\"")
        key(v, #selector(NSResponder.scrollPageDown(_:)))
        XCTAssertGreaterThan(selection(v).0, 20, "Page Down moves the caret down")
        key(v, #selector(NSResponder.moveToEndOfDocument(_:)))
        XCTAssertEqual(selection(v).0, (v.document?.lenUtf16()).map(Int.init))
        key(v, #selector(NSResponder.pageUp(_:)))
        XCTAssertLessThan(selection(v).0, Int(v.document!.lenUtf16()) - 20)
    }

    func testTypingAfterMovingInsertsAtTheCaret() throws {
        let c = try makeController(text: "abc")
        let v = c.editorView
        key(v, #selector(NSResponder.moveRight(_:)))
        v.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), ["aXbc"])
        XCTAssertEqual(selection(v).0, 2)
    }

    func testMouseClickDragWordLineAndMultiCaret() throws {
        let c = try makeController(text: "hello world\nsecond line")
        let v = c.editorView
        click(v, line: 0, utf16Col: 6)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 0)

        click(v, line: 1, utf16Col: 3, type: .leftMouseDragged)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 12 + 3 - 6)

        click(v, line: 0, utf16Col: 8, count: 2)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 5, "double-click selects 'world'")

        click(v, line: 1, utf16Col: 2, count: 3)
        XCTAssertEqual(selection(v).0, 12)
        XCTAssertEqual(selection(v).1, 11, "triple-click selects the last line")

        click(v, line: 0, utf16Col: 1)
        click(v, line: 1, utf16Col: 1, flags: .shift)
        XCTAssertEqual(selection(v).0, 1)
        XCTAssertEqual(selection(v).1, 12, "shift-click extends")

        click(v, line: 0, utf16Col: 0)
        click(v, line: 1, utf16Col: 0, flags: .option)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 2).selectionCount, 2)
        v.insertText("#", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 2), ["#hello world", "#second line"])
    }

    func testIdentifierRangeForCommandHoverLink() {
        let text = "use App\\Providers\\MacroBuilderServiceProvider; $user->name(42);"
        let start = (text as NSString).range(of: "MacroBuilder").location
        XCTAssertEqual(EditorView.identifierRange(in: text, at: start + 3),
                       start..<(start + "MacroBuilderServiceProvider".utf16.count))
        let variable = (text as NSString).range(of: "$user").location
        XCTAssertEqual(EditorView.identifierRange(in: text, at: variable + 2), variable..<(variable + 5))
        let number = (text as NSString).range(of: "42").location
        XCTAssertNil(EditorView.identifierRange(in: text, at: number))
        XCTAssertNil(EditorView.identifierRange(in: "a ;; b", at: 3))
    }

    func testQualifiedNameRangeCoversTheWholeNamespacePath() {
        let text = "use App\\Models\\OutletProduct; new \\Carbon\\Carbon($x);"
        let ns = text as NSString
        let full = ns.range(of: "App\\Models\\OutletProduct")
        // Any segment widens to the whole name.
        for word in ["App", "Models", "OutletProduct"] {
            let inner = ns.range(of: word)
            let range = EditorView.qualifiedNameRange(in: text, around: inner.location..<(inner.location + inner.length))
            XCTAssertEqual(range, full.location..<(full.location + full.length), word)
        }
        // A leading backslash (global namespace) is part of the name.
        let carbon = ns.range(of: "\\Carbon\\Carbon")
        let first = ns.range(of: "Carbon")
        XCTAssertEqual(EditorView.qualifiedNameRange(in: text, around: first.location..<(first.location + first.length)),
                       carbon.location..<(carbon.location + carbon.length))
        // A name without a backslash, and a variable, stay as they are.
        let variable = ns.range(of: "$x")
        XCTAssertEqual(EditorView.qualifiedNameRange(in: text, around: variable.location..<(variable.location + variable.length)),
                       variable.location..<(variable.location + variable.length))
        XCTAssertEqual(EditorView.qualifiedNameRange(in: "new Foo();", around: 4..<7), 4..<7)
    }

    func testLastSegmentStartIsTheClassName() {
        let text = "use App\\Models\\OutletProduct;"
        let ns = text as NSString
        let full = ns.range(of: "App\\Models\\OutletProduct")
        let range = full.location..<(full.location + full.length)
        XCTAssertEqual(EditorView.lastSegmentStart(in: text, range: range), ns.range(of: "OutletProduct").location)
        XCTAssertEqual(EditorView.lastSegmentStart(in: "Foo", range: 0..<3), 0)
    }

    func testCommandHoverUnderlinesWordAndClearsOnRelease() throws {
        let c = try makeController(text: "use Foo\\BarService;\nsecond")
        let v = c.editorView
        v.onDefinitionClick = { _ in true }
        let text = "use Foo\\BarService;"
        let column = (text as NSString).range(of: "BarService").location + 2
        let x = v.textOriginX + v.xOffset(in: text, column: UInt64(column)) + 0.5
        v.lastMousePoint = NSPoint(x: x, y: v.metrics.lineHeight / 2)

        v.updateDefinitionLink(commandHeld: true)
        XCTAssertEqual(v.definitionLink?.line, 0)
        XCTAssertEqual(v.definitionLink?.start, 8)
        XCTAssertEqual(v.definitionLink?.end, 18)

        v.updateDefinitionLink(commandHeld: false)
        XCTAssertNil(v.definitionLink, "releasing Cmd removes the underline")

        // Past the end of the line there is nothing to open.
        v.lastMousePoint = NSPoint(x: 10_000, y: v.metrics.lineHeight / 2)
        v.updateDefinitionLink(commandHeld: true)
        XCTAssertNil(v.definitionLink)
    }

    func testImportSpecifierRangeCoversOnlyModulePaths() {
        func range(_ text: String, _ literal: String) -> String? {
            let ns = text as NSString
            let r = ns.range(of: literal)
            guard let found = EditorView.importSpecifierRange(in: text, stringStart: r.location, stringEnd: r.location + r.length)
            else { return nil }
            return ns.substring(with: NSRange(location: found.lowerBound, length: found.count))
        }
        XCTAssertEqual(range("import type { A } from '../config/env'", "'../config/env'"), "../config/env")
        XCTAssertEqual(range("import fs from 'node:fs/promises'", "'node:fs/promises'"), "node:fs/promises")
        XCTAssertEqual(range("export * from \"./x\"", "\"./x\""), "./x")
        XCTAssertEqual(range("import './polyfill'", "'./polyfill'"), "./polyfill")
        XCTAssertEqual(range("const a = require('lodash')", "'lodash'"), "lodash")
        XCTAssertEqual(range("const a = await import('./lazy')", "'./lazy'"), "./lazy")
        XCTAssertNil(range("const s = 'hello'", "'hello'"))
        XCTAssertNil(range("console.log('from')", "'from'"))
        XCTAssertNil(range("import x from ''", "''"))
    }

    func testRelativeImportsResolveToFilesOnDisk() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-imp-\(UUID().uuidString)")
        let manager = FileManager.default
        try manager.createDirectory(at: dir.appendingPathComponent("config"), withIntermediateDirectories: true)
        try manager.createDirectory(at: dir.appendingPathComponent("drivers"), withIntermediateDirectories: true)
        try manager.createDirectory(at: dir.appendingPathComponent("src"), withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: dir) }
        for name in ["config/env.ts", "drivers/index.ts", "src/util.ts", "src/data.json", "src/main.ts"] {
            try "x".write(to: dir.appendingPathComponent(name), atomically: true, encoding: .utf8)
        }
        let main = dir.appendingPathComponent("src/main.ts").path
        func resolve(_ spec: String) -> String? {
            EditorView.resolveImport(spec, from: main).map { $0.replacingOccurrences(of: dir.standardizedFileURL.path + "/", with: "") }
        }
        XCTAssertEqual(resolve("../config/env"), "config/env.ts", "extension is added")
        XCTAssertEqual(resolve("../drivers"), "drivers/index.ts", "a folder resolves to its index")
        XCTAssertEqual(resolve("./util.js"), "src/util.ts", "ESM style .js names the .ts source")
        XCTAssertEqual(resolve("./data.json"), "src/data.json")
        XCTAssertNil(resolve("./missing"))
        XCTAssertNil(resolve("node:fs/promises"), "packages are left to the language server")
        XCTAssertNil(resolve("react"))
    }

    func testCommandHoverLinksImportPaths() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-impl-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.ts")
        let lines = ["import type { AgentEnv } from '../config/env'", "import fs from 'node:fs/promises'", "const s = 'hello'"]
        try (lines.joined(separator: "\n") + "\n").write(to: file, atomically: true, encoding: .utf8)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file)
        let v = c.editorView
        v.onDefinitionClick = { _ in true }
        let deadline = Date().addingTimeInterval(5)
        while v.document?.viewport(firstLine: 0, count: 3).styles.isEmpty ?? true, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        func link(line: Int, at needle: String) -> (UInt64, UInt64)? {
            let text = lines[line]
            let column = (text as NSString).range(of: needle).location
            v.lastMousePoint = NSPoint(x: v.textOriginX + v.xOffset(in: text, column: UInt64(column)) + 0.5,
                                       y: CGFloat(line) * v.metrics.lineHeight + v.metrics.lineHeight / 2)
            v.updateDefinitionLink(commandHeld: true)
            return v.definitionLink.map { ($0.start, $0.end) }
        }
        let first = (lines[0] as NSString).range(of: "../config/env")
        XCTAssertEqual(link(line: 0, at: "env")?.0, UInt64(first.location), "the whole path is one link")
        XCTAssertEqual(link(line: 0, at: "env")?.1, UInt64(first.location + first.length))
        XCTAssertNotNil(link(line: 0, at: "/config"), "slashes are part of the link")
        XCTAssertNotNil(link(line: 1, at: "node:fs"), "package paths are links too")
        XCTAssertNil(link(line: 2, at: "hello"), "other strings are not links")
    }

    func testCommandHoverSkipsPrimitiveTypesOutsidePHPToo() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-prim-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.ts")
        let line = "function f(a: string, b: Foo | null, c: number) {}"
        try (line + "\n").write(to: file, atomically: true, encoding: .utf8)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file)
        let v = c.editorView
        v.onDefinitionClick = { _ in true }
        func link(_ word: String) -> Bool {
            let column = (line as NSString).range(of: word).location + 1
            v.lastMousePoint = NSPoint(x: v.textOriginX + v.xOffset(in: line, column: UInt64(column)) + 0.5,
                                       y: v.metrics.lineHeight / 2)
            v.updateDefinitionLink(commandHeld: true)
            return v.definitionLink != nil
        }
        XCTAssertFalse(link("string"))
        XCTAssertFalse(link("null"))
        XCTAssertFalse(link("number"))
        XCTAssertTrue(link("Foo"), "a user type stays a link")
        XCTAssertTrue(link("f("), "the function name stays a link")
    }

    func testCommandHoverSkipsKeywordsButKeepsNames() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-link-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.php")
        let line = "    public function boot(?string $accessKey = null): void { Log::info('Queue job'); return Foo::bar(config('app.name'), true); }"
        try "<?php\nclass A {\n\(line)\n}\n".write(to: file, atomically: true, encoding: .utf8)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        try c.openFile(at: file)
        let v = c.editorView
        v.onDefinitionClick = { _ in true }

        // Syntax parsing is asynchronous; wait until the line has styles.
        let deadline = Date().addingTimeInterval(5)
        while v.document?.viewport(firstLine: 2, count: 1).styles.isEmpty ?? true, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        func link(_ word: String) -> (UInt64, UInt64)? {
            let column = (line as NSString).range(of: word).location + 1
            v.lastMousePoint = NSPoint(x: v.textOriginX + v.xOffset(in: line, column: UInt64(column)) + 0.5,
                                       y: 2 * v.metrics.lineHeight + v.metrics.lineHeight / 2)
            v.updateDefinitionLink(commandHeld: true)
            return v.definitionLink.map { ($0.start, $0.end) }
        }
        XCTAssertNil(link("public"), "keywords are not links")
        XCTAssertNil(link("function"))
        XCTAssertNil(link("return"))
        XCTAssertNotNil(link("boot"), "method names stay links")
        XCTAssertNotNil(link("Foo"), "class names stay links")
        XCTAssertNotNil(link("bar"))
        XCTAssertNil(link("Queue"), "plain strings are not links")
        XCTAssertNil(link("string"), "built-in types are not links")
        XCTAssertNil(link("null"), "literals are not links")
        XCTAssertNil(link("void"))
        XCTAssertNil(link("true"))
        XCTAssertNotNil(link("$accessKey"), "variables stay links")
        let app = (line as NSString).range(of: "app.name").location
        let configLink = link("app.name")
        XCTAssertEqual(configLink?.0, UInt64(app), "config() keys link as a whole")
        XCTAssertEqual(configLink?.1, UInt64(app + "app.name".utf16.count))
    }

    func testCommandHoverSkipsWordsBetweenJSXTagsButKeepsCode() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-jsx-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.tsx")
        let line = "const view = <p className=\"cls\">Recorded admin {total}</p>;"
        try "const total = 1;\n\(line)\n".write(to: file, atomically: true, encoding: .utf8)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file)
        let v = c.editorView
        v.onDefinitionClick = { _ in true }

        // Syntax parsing is asynchronous; wait until the line has styles.
        let deadline = Date().addingTimeInterval(5)
        while v.document?.viewport(firstLine: 1, count: 1).styles.isEmpty ?? true, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        func link(_ word: String) -> (UInt64, UInt64)? {
            let column = (line as NSString).range(of: word).location + 1
            v.lastMousePoint = NSPoint(x: v.textOriginX + v.xOffset(in: line, column: UInt64(column)) + 0.5,
                                       y: 1 * v.metrics.lineHeight + v.metrics.lineHeight / 2)
            v.updateDefinitionLink(commandHeld: true)
            return v.definitionLink.map { ($0.start, $0.end) }
        }
        XCTAssertNil(link("Recorded"), "text between tags is not a name")
        XCTAssertNil(link("admin"))
        XCTAssertNotNil(link("total"), "an expression inside {} is code")
        XCTAssertNotNil(link("view"))
    }

    func testCommandClickOnLogChannelOpensLoggingConfig() throws {
        // The Laravel app sits below the opened file's folder tree, as in a
        // workspace whose root is not the project root.
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-log-\(UUID().uuidString)")
        let root = dir.appendingPathComponent("backend")
        let providers = root.appendingPathComponent("app/Providers")
        try FileManager.default.createDirectory(at: providers, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("config"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try "#!/usr/bin/env php\n".write(to: root.appendingPathComponent("artisan"), atomically: true, encoding: .utf8)
        let logging = root.appendingPathComponent("config/logging.php")
        try "<?php\n\nreturn [\n    'channels' => [\n        'stack' => ['driver' => 'stack'],\n        'queue' => ['driver' => 'daily'],\n    ],\n];\n"
            .write(to: logging, atomically: true, encoding: .utf8)
        let file = providers.appendingPathComponent("AppServiceProvider.php")
        let line = "        Log::channel('queue')->info('Queue job processing', [])"
        try "<?php\nclass A {\n    function boot() {\n\(line);\n    }\n}\n".write(to: file, atomically: true, encoding: .utf8)

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        try c.openFile(at: file)
        let v = c.editorView
        let deadline = Date().addingTimeInterval(5)
        while v.document?.viewport(firstLine: 3, count: 1).styles.isEmpty ?? true, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        let column = (line as NSString).range(of: "queue").location + 2
        XCTAssertEqual(v.logChannelName(at: TextPos(line: 3, colUtf16: UInt64(column))), "queue")
        XCTAssertNil(v.logChannelName(at: TextPos(line: 3, colUtf16: UInt64((line as NSString).range(of: "Queue job").location + 2))))

        click(v, line: 3, utf16Col: column, flags: .command)
        let opened = c.activeTab.map { URL(fileURLWithPath: $0.doc.path()).resolvingSymlinksInPath().path }
        XCTAssertEqual(opened, logging.resolvingSymlinksInPath().path, "Cmd+click opens config/logging.php")
        let caret = c.activeTab?.doc.primaryCursor()
        XCTAssertEqual(caret?.line, 5, "caret on the 'queue' key")
        XCTAssertEqual(caret?.colUtf16, 8)
    }

    func testOnlyFrameworkHelperStringsAreLinks() {
        func range(_ text: String, _ literal: String) -> Range<Int>? {
            let r = (text as NSString).range(of: literal)
            return EditorView.frameworkStringRange(in: text, stringStart: r.location, stringEnd: r.location + r.length)
        }
        let config = "$url = config('services.sipay.url');"
        let start = (config as NSString).range(of: "services").location
        XCTAssertEqual(range(config, "'services.sipay.url'"), start..<(start + "services.sipay.url".utf16.count))
        XCTAssertNotNil(range("return view( \"home\" );", "\"home\""))
        XCTAssertNil(range("Log::channel('queue')->info('Queue job processing', [", "'Queue job processing'"))
        XCTAssertNil(range("'connection' => $event->connectionName,", "'connection'"))
        XCTAssertNil(range("Log::info('message')", "'message'"))
        XCTAssertNil(range("$broadcast->channel('orders')", "'orders'"), "channel() is only a link on Log")
    }

    func testFacadeAndMethodStringsAreLinks() {
        func range(_ text: String, _ literal: String) -> Range<Int>? {
            let r = (text as NSString).range(of: literal)
            return EditorView.frameworkStringRange(in: text, stringStart: r.location, stringEnd: r.location + r.length)
        }
        XCTAssertNotNil(range("Storage::disk('s3')->put($p, $c);", "'s3'"))
        XCTAssertNotNil(range("Config::get('app.name')", "'app.name'"))
        XCTAssertNotNil(range("\\Illuminate\\Support\\Facades\\Log::channel('queue')", "'queue'"))
        XCTAssertNotNil(range("$path = storage_path('logs/x.log');", "'logs/x.log'"))
        XCTAssertNotNil(range("$this->lang->get('auth.failed')", "'auth.failed'"))
    }

    func testCodeIgniterAndSymfonyStringsAreLinks() {
        func range(_ text: String, _ literal: String) -> Range<Int>? {
            let r = (text as NSString).range(of: literal)
            return EditorView.frameworkStringRange(in: text, stringStart: r.location, stringEnd: r.location + r.length)
        }
        XCTAssertNotNil(range("return route_to('user_profile', 1);", "'user_profile'"))
        XCTAssertNotNil(range("$text = lang('App.welcome');", "'App.welcome'"))
        XCTAssertNotNil(range("$m = model('UserModel');", "'UserModel'"))
        XCTAssertNotNil(range("return $this->render('product/index.html.twig');", "'product/index.html.twig'"))
        XCTAssertNotNil(range("return $this->redirectToRoute('app_home');", "'app_home'"))
        XCTAssertNotNil(range("$p = $this->getParameter('kernel.project_dir');", "'kernel.project_dir'"))
        XCTAssertNil(range("$this->logger->info('app_home');", "'app_home'"))
    }

    func testLogChannelNameIsExtracted() {
        func name(_ text: String, _ literal: String) -> String? {
            let r = (text as NSString).range(of: literal)
            return EditorView.logChannelName(in: text, stringStart: r.location, stringEnd: r.location + r.length)
        }
        XCTAssertEqual(name("Log::channel('queue')->info('x');", "'queue'"), "queue")
        XCTAssertEqual(name("Log :: channel( \"daily\" )", "\"daily\""), "daily")
        XCTAssertNil(name("Storage::disk('s3')", "'s3'"))
        XCTAssertNil(name("Log::channel('queue')->info('x');", "'x'"))
    }

    func testClickingPastTheEndsClampsToValidPositions() throws {
        let c = try makeController(text: "ab\ncd")
        let v = c.editorView
        let below = v.textPosition(at: NSPoint(x: 10_000, y: 10_000))
        XCTAssertEqual(below.line, 1)
        XCTAssertEqual(below.colUtf16, 2)
        let above = v.textPosition(at: NSPoint(x: -50, y: -50))
        XCTAssertEqual(above.line, 0)
        XCTAssertEqual(above.colUtf16, 0)
        let nan = v.textPosition(at: NSPoint(x: 0, y: CGFloat.nan))
        XCTAssertEqual(nan.line, 0)
    }

    func testSelectAllCopyCutPasteAndLinewise() throws {
        let c = try makeController(text: "one\ntwo\nthree")
        let v = c.editorView
        let pb = v.pasteboard

        v.selectAll(nil)
        XCTAssertEqual(selection(v).1, 13)
        v.copy(nil)
        XCTAssertEqual(pb.string(forType: .string), "one\ntwo\nthree")
        XCTAssertNil(pb.string(forType: EditorView.linewiseType))

        // No selection: copy takes the whole line and flags it.
        click(v, line: 1, utf16Col: 1)
        v.copy(nil)
        XCTAssertEqual(pb.string(forType: .string), "two\n")
        XCTAssertEqual(pb.string(forType: EditorView.linewiseType), "two\n")
        v.paste(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 5), ["one", "two", "two", "three"])

        v.cut(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 5), ["one", "two", "three"])
        XCTAssertEqual(pb.string(forType: .string), "two\n")

        // The same text copied from elsewhere is a plain paste.
        pb.clearContents()
        pb.setString("two\n", forType: .string)
        click(v, line: 0, utf16Col: 3)
        v.paste(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), ["onetwo"])
    }

    func testMenuItemsAreValidatedAgainstState() throws {
        let c = try makeController(text: "x")
        let v = c.editorView
        func enabled(_ action: Selector) -> Bool {
            v.validateMenuItem(NSMenuItem(title: "", action: action, keyEquivalent: ""))
        }
        XCTAssertTrue(enabled(#selector(EditorView.undoEdit(_:))))
        XCTAssertFalse(enabled(#selector(EditorView.redoEdit(_:))))
        v.undoEdit(nil)
        XCTAssertTrue(enabled(#selector(EditorView.redoEdit(_:))))
        v.pasteboard.clearContents()
        XCTAssertFalse(enabled(#selector(EditorView.paste(_:))))
    }

    // MARK: Saving

    func testSaveWritesTheFileAndClearsTheEditedMark() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-save-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.txt")
        try "hi".write(to: file, atomically: true, encoding: .utf8)

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.open(url: file)
        XCTAssertEqual(c.window?.isDocumentEdited, false)

        let v = c.editorView
        key(v, #selector(NSResponder.moveToEndOfDocument(_:)))
        v.insertText("!", replacementRange: NSRange(location: NSNotFound, length: 0))
        v.flushPendingUpdates()
        XCTAssertEqual(c.window?.isDocumentEdited, true, "typing marks the window as edited")

        c.saveDocument(nil)
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "hi!")
        XCTAssertEqual(c.window?.isDocumentEdited, false)
    }

    func testSavingAnUntitledDocumentAsksForALocation() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-saveas-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let target = dir.appendingPathComponent("new.txt")

        let c = try makeController(text: "draft")
        defer { c.window?.orderOut(nil) }
        var asked = 0
        c.askSaveLocation = { _ in asked += 1; return target }
        XCTAssertEqual(c.window?.isDocumentEdited, true, "the initial text already differs from a clean buffer")
        c.editorView.insertText("!", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        XCTAssertEqual(c.window?.isDocumentEdited, true)

        XCTAssertTrue(c.save())
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "!draft")
        XCTAssertEqual(c.window?.title, "new.txt")
        XCTAssertEqual(c.window?.isDocumentEdited, false)

        c.saveDocument(nil)
        XCTAssertEqual(asked, 1, "a second save reuses the path")

        // Cancelling the panel reports failure and leaves the text dirty.
        let d = try makeController(text: "zzz")
        defer { d.window?.orderOut(nil) }
        d.askSaveLocation = { _ in nil }
        XCTAssertFalse(d.save())
    }

    func testUnsavedChangesPromptBeforeReplacingOrClosing() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-prompt-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.txt")
        let b = dir.appendingPathComponent("b.txt")
        try "A".write(to: a, atomically: true, encoding: .utf8)
        try "B".write(to: b, atomically: true, encoding: .utf8)

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        var asked: [String] = []
        var answer = MainWindowController.SaveChoice.cancel
        c.askSaveChoice = { name in asked.append(name); return answer }

        c.open(url: a)
        XCTAssertTrue(c.confirmDiscardChanges(), "clean documents never prompt")
        XCTAssertTrue(asked.isEmpty)

        // Opening another file adds a tab: nothing to ask yet.
        c.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.open(url: b)
        XCTAssertTrue(asked.isEmpty)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["B"])
        XCTAssertFalse(c.windowShouldClose(c.window!))
        XCTAssertEqual(asked, ["a.txt"], "closing asks about the unsaved tab")
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["xA"], "Cancel shows the tab it asked about")

        answer = .discard
        XCTAssertTrue(c.closeTab(at: 0))
        XCTAssertEqual(try String(contentsOf: a, encoding: .utf8), "A", "discard leaves the file alone")
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["B"])

        c.editorView.insertText("y", replacementRange: NSRange(location: NSNotFound, length: 0))
        answer = .save
        XCTAssertTrue(c.windowShouldClose(c.window!))
        XCTAssertEqual(try String(contentsOf: b, encoding: .utf8), "yB", "save writes before closing")
    }

    // MARK: Multi-cursor

    func testCmdDAddsNextOccurrencesAndTypingEditsAll() throws {
        let c = try makeController(text: "foo bar\nfoo baz\nfoo")
        let v = c.editorView
        click(v, line: 0, utf16Col: 1)
        v.selectNextOccurrence(nil)
        XCTAssertEqual(selection(v).0, 0)
        XCTAssertEqual(selection(v).1, 3, "first press selects the word")
        v.selectNextOccurrence(nil)
        v.selectNextOccurrence(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["X bar", "X baz", "X"])
        v.undoEdit(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["foo bar", "foo baz", "foo"])
    }

    func testSelectAllOccurrencesAndCursorAboveBelow() throws {
        let c = try makeController(text: "ab\nab\nab")
        let v = c.editorView
        click(v, line: 1, utf16Col: 1)
        v.selectAllOccurrences(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)

        click(v, line: 0, utf16Col: 1)
        v.addCursorBelow(nil)
        v.addCursorBelow(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.addCursorAbove(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3, "no line above the first")
        v.insertText("-", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["a-b", "a-b", "a-b"])
    }

    func testOptionShiftDragMakesAColumnSelection() throws {
        let c = try makeController(text: "abcdef\nabcdef\nab")
        let v = c.editorView
        let flags: NSEvent.ModifierFlags = [.option, .shift]
        click(v, line: 0, utf16Col: 1, flags: flags)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 1)
        click(v, line: 2, utf16Col: 2, flags: flags, type: .leftMouseDragged)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.insertText("|", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["a|cdef", "a|cdef", "a|"])
        v.mouseUp(with: NSEvent.mouseEvent(
            with: .leftMouseUp, location: .zero, modifierFlags: [], timestamp: 0,
            windowNumber: v.window?.windowNumber ?? 0, context: nil, eventNumber: 0,
            clickCount: 1, pressure: 0)!)
        XCTAssertNil(v.columnAnchor)
    }

    func testColumnPositionKeepsColumnsBeyondTheLineEnd() throws {
        let c = try makeController(text: "abcdef\nab")
        let v = c.editorView
        let y = v.metrics.lineHeight * 1.5
        let far = v.columnPosition(at: NSPoint(x: v.textOriginX + v.metrics.spaceWidth * 5, y: y))
        XCTAssertEqual(far.line, 1)
        XCTAssertEqual(far.colUtf16, 5, "five columns in, three past the 2-char line")
        let inside = v.columnPosition(at: NSPoint(x: v.textOriginX + v.metrics.spaceWidth * 1, y: y))
        XCTAssertEqual(inside.colUtf16, 1)
    }

    func testSelectionMenuHasTheExpectedShortcuts() throws {
        let menu = try XCTUnwrap(
            MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Selection" })
        let next = try XCTUnwrap(menu.item(withTitle: "Add Next Occurrence"))
        XCTAssertEqual(next.keyEquivalent, "d")
        XCTAssertEqual(next.keyEquivalentModifierMask, .command)
        let all = try XCTUnwrap(menu.item(withTitle: "Select All Occurrences"))
        XCTAssertEqual(all.keyEquivalent, "l")
        XCTAssertEqual(all.keyEquivalentModifierMask, [.command, .shift])
        let above = try XCTUnwrap(menu.item(withTitle: "Add Cursor Above"))
        XCTAssertEqual(above.keyEquivalentModifierMask, [.command, .option])
        XCTAssertNotNil(menu.item(withTitle: "Add Cursor Below"))
    }

    // MARK: Per-frame coalescing (M1-15)

    func testChangeNotificationsAreCoalescedPerFrame() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let v = c.editorView
        var notified = 0
        v.onChange = { notified += 1 }
        let before = v.frameFlushes
        for _ in 0..<50 {
            v.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        }
        XCTAssertEqual(notified, 0, "nothing runs until the frame")
        XCTAssertTrue(v.hasPendingChange)
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), [String(repeating: "x", count: 50)],
                       "edits themselves are applied immediately")

        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "fifty edits, one notification")
        XCTAssertEqual(v.frameFlushes, before + 1)
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "no extra work without a new change")
    }

    func testTheDisplayLinkDeliversPendingChangesOnItsOwn() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let v = c.editorView
        let delivered = expectation(description: "onChange on the next frame")
        v.onChange = { delivered.fulfill() }
        v.insertText("y", replacementRange: NSRange(location: NSNotFound, length: 0))
        wait(for: [delivered], timeout: 2)
        XCTAssertFalse(v.hasPendingChange)
    }

    func testClosingTheWindowSeesTheLatestChange() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        var asked = 0
        c.askSaveChoice = { _ in asked += 1; return .cancel }
        c.editorView.insertText("z", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertFalse(c.windowShouldClose(c.window!))
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(c.window?.isDocumentEdited, true)
    }
}

@MainActor
final class LatencyProbeTests: XCTestCase {
    private func keyEvent(_ chars: String, keyCode: UInt16, window: NSWindow) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
                         timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                         context: nil, characters: chars, charactersIgnoringModifiers: chars,
                         isARepeat: false, keyCode: keyCode)!
    }

    func testAKeyThatEditsDrawsImmediately() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let window = try XCTUnwrap(c.window)
        let probe = LatencyProbe.shared
        probe.reset()

        c.editorView.keyDown(with: keyEvent("a", keyCode: 0, window: window))
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a"], "went through the real key path")
        XCTAssertEqual(probe.latencies.count, 1, "the edit must not wait for the next display frame")
        XCTAssertEqual(probe.drawTimes.count, 1)
        let sample = try XCTUnwrap(probe.latencies.first)
        XCTAssertGreaterThanOrEqual(sample, probe.drawTimes[0], "includes the draw itself")
        XCTAssertLessThan(sample, 1_000, "sanity bound, not a performance gate")

        // Another draw without a key (caret blink, scroll) is not a sample.
        c.editorView.needsDisplay = true
        c.editorView.displayIfNeeded()
        XCTAssertEqual(probe.latencies.count, 1)
        XCTAssertTrue(probe.report.hasPrefix("latency_samples=1 "), probe.report)
    }

    func testPercentilesAndTheRingBuffer() {
        XCTAssertEqual(LatencyProbe.percentile([1, 2, 3, 4], 0.5), 2)
        XCTAssertEqual(LatencyProbe.percentile([1, 2, 3, 4], 0.95), 4)
        XCTAssertEqual(LatencyProbe.percentile([], 0.5), 0)
        let probe = LatencyProbe.shared
        probe.reset()
        let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                                     windowNumber: 0, context: nil, characters: "x",
                                     charactersIgnoringModifiers: "x", isARepeat: false, keyCode: 7)!
        for _ in 0..<(LatencyProbe.capacity + 10) {
            probe.keyDown(event)
            probe.drawFinished(drawStart: ProcessInfo.processInfo.systemUptime)
        }
        XCTAssertEqual(probe.latencies.count, LatencyProbe.capacity, "keeps the newest samples only")
        probe.keyDown(event)
        probe.cancelPending()
        probe.drawFinished(drawStart: ProcessInfo.processInfo.systemUptime)
        XCTAssertEqual(probe.latencies.count, LatencyProbe.capacity, "a cancelled key is not counted")
        probe.reset()
        XCTAssertNil(probe.summary)
    }

    func testArrowKeysDoNotRunWholeDocumentListeners() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let v = c.editorView
        v.document?.edit(command: .insertText(text: "abc\ndef"))
        v.didChangeDocument()
        v.flushPendingUpdates()
        var notified = 0
        v.onChange = { notified += 1 }
        v.doCommand(by: #selector(NSResponder.moveLeft(_:)))
        v.doCommand(by: #selector(NSResponder.moveUp(_:)))
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 0, "moving the caret is not an edit")
        XCTAssertEqual(v.selectedRange().location, 2, "but the caret did move")
        v.doCommand(by: #selector(NSResponder.deleteBackward(_:)))
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "an edit still notifies")
    }
}
