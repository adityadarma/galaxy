import XCTest
@testable import Galaxy

@MainActor
final class ReferencesPanelTests: XCTestCase {
    private func location(_ path: String, line: UInt64, column: UInt64) -> DefinitionLocation {
        DefinitionLocation(uri: URL(fileURLWithPath: path).absoluteString, line: line, column: column)
    }

    private func waitUntil(
        timeout: TimeInterval = 2,
        _ condition: () -> Bool
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
    }

    func testGroupsReferencesByFileAndSelectsTheFirstLocation() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let first = location("/workspace/src/a.ts", line: 2, column: 4)
        let second = location("/workspace/src/a.ts", line: 8, column: 1)
        let third = location("/workspace/tests/a.test.ts", line: 3, column: 7)
        var previewed: DefinitionLocation?
        panel.onPreview = { previewed = $0 }

        panel.update([second, third, first], workspaceRoot: "/workspace")

        XCTAssertEqual(panel.numberOfRows(in: NSTableView()), 5, "two file headers and three references")
        XCTAssertEqual(previewed, second, "the first engine result is previewed immediately")
    }

    func testPreferredReferenceIsPreviewedInsteadOfFirstEngineResult() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let local = location("/workspace/local.tsx", line: 11, column: 2)
        let declaration = location("/workspace/model-preference.ts", line: 102, column: 16)
        var previewed: DefinitionLocation?
        panel.onPreview = { previewed = $0 }

        panel.update(
            [local, declaration],
            workspaceRoot: "/workspace",
            preferredLocation: declaration
        )

        XCTAssertEqual(previewed, declaration)
        XCTAssertEqual(panel.resultsTableView.selectedRow, 3)
    }

    func testSelectingAnotherReferenceImmediatelyUpdatesPreview() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let first = location("/workspace/src/a.ts", line: 17, column: 9)
        let second = location("/workspace/src/a.ts", line: 169, column: 17)
        var previewed: DefinitionLocation?
        panel.onPreview = { previewed = $0 }
        panel.update([first, second], workspaceRoot: "/workspace")
        XCTAssertEqual(previewed, first)

        previewed = nil
        panel.resultsTableView.selectRowIndexes(IndexSet(integer: 2), byExtendingSelection: false)

        XCTAssertEqual(previewed, second)
    }

    func testOpeningReferenceUsesItsRowAfterFileGroupsAreSorted() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let zulu = location("/workspace/zulu.ts", line: 2, column: 3)
        let alpha = location("/workspace/alpha.ts", line: 7, column: 11)
        var opened: DefinitionLocation?
        panel.onSelect = { opened = $0 }

        panel.update([zulu, alpha], workspaceRoot: "/workspace")
        panel.handleClick(row: 3, clickCount: 2)

        XCTAssertEqual(opened, zulu, "the row must retain its URI after file groups are sorted")
    }

    func testSingleClickDoesNotReplaceSelectionPreviewWithClickedRowCallback() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let local = location("/workspace/local.tsx", line: 11, column: 2)
        let model = location("/workspace/model-preference.ts", line: 102, column: 16)
        var previews: [DefinitionLocation] = []
        panel.onPreview = { previews.append($0) }
        panel.update([local, model], workspaceRoot: "/workspace")
        previews.removeAll()

        panel.resultsTableView.selectRowIndexes(IndexSet(integer: 3), byExtendingSelection: false)
        panel.handleClick(row: 3, clickCount: 1)

        XCTAssertEqual(previews, [model], "a single click must issue exactly one preview for its selected row")
    }

    func testSingleClickPreviewsEventRowWhenTableSelectionIsStillOnFirstResult() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let local = location("/workspace/local.tsx", line: 11, column: 2)
        let model = location("/workspace/model-preference.ts", line: 102, column: 16)
        var previewed: DefinitionLocation?
        panel.onPreview = { previewed = $0 }
        panel.update([local, model], workspaceRoot: "/workspace")
        XCTAssertEqual(panel.resultsTableView.selectedRow, 1)

        panel.handleClick(row: 3, clickCount: 1)

        XCTAssertEqual(previewed, model, "the mouse event row must win over stale table selection")
    }

    func testRefreshingUnchangedReferencesKeepsTheUserSelection() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let first = location("/workspace/src/a.ts", line: 17, column: 9)
        let second = location("/workspace/src/a.ts", line: 169, column: 17)
        var previewed: DefinitionLocation?
        panel.onPreview = { previewed = $0 }
        panel.update([first, second], workspaceRoot: "/workspace")
        panel.resultsTableView.selectRowIndexes(IndexSet(integer: 2), byExtendingSelection: false)
        XCTAssertEqual(previewed, second)

        previewed = nil
        panel.update([first, second], workspaceRoot: "/workspace")

        XCTAssertNil(previewed, "an unchanged background refresh must not restore the first result")
        XCTAssertEqual(panel.resultsTableView.selectedRow, 2)
    }

    func testResultsPaneStartsAtOneThirdOfPeekWidth() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        panel.layoutSubtreeIfNeeded()

        XCTAssertEqual(panel.resultsPaneFraction, 1.0 / 3.0, accuracy: 0.03)
    }

    func testLineRangeUsesUtf16AndIncludesTheLineBreak() {
        let text = "first\nemoji 😀 value\nlast"
        let range = ReferencesPanel.lineRange(in: text, line: 1)
        XCTAssertEqual((text as NSString).substring(with: range), "emoji 😀 value\n")
        XCTAssertEqual(range.location, 6)
        XCTAssertEqual(range.length, 15, "the emoji occupies two UTF-16 units")
        XCTAssertEqual(ReferencesPanel.lineRange(in: text, line: 9).length, 0)
    }

    func testPreviewIncludesLineNumberGutterImmediately() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let target = location("/workspace/src/a.ts", line: 99, column: 0)
        panel.update([target], workspaceRoot: "/workspace")

        panel.showPreview(path: "/workspace/src/a.ts", text: "const first = 1;\nconst second = 2;\n", location: target)

        XCTAssertTrue(panel.statusDetail.contains("const first = 1;"))
        XCTAssertTrue(panel.statusDetail.contains("const second = 2;"))
        XCTAssertFalse(panel.statusDetail.contains("1  const first"), "line numbers stay in the fixed ruler")
    }

    func testPreviewKeepsEditorGutterWidthAndStartsAtLeftEdge() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let target = location("/workspace/src/a.ts", line: 1, column: 400)
        let longLine = String(repeating: "const value = something; ", count: 80)
        panel.update([target], workspaceRoot: "/workspace")

        panel.showPreview(
            path: "/workspace/src/a.ts",
            text: "first\n\(longLine)\n",
            location: target,
            gutterWidth: 76
        )

        XCTAssertEqual(panel.previewGutterWidth, 76)
        XCTAssertEqual(panel.previewHorizontalOffset, 0)
    }

    func testPreviewHeaderShowsFileLocationAndReferenceCount() {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let first = location("/workspace/app/Services/ToastAlertService.php", line: 6, column: 12)
        let second = location("/workspace/app/Http/PermissionController.php", line: 3, column: 8)
        panel.update([first, second], workspaceRoot: "/workspace")

        panel.showPreview(
            path: "/workspace/app/Services/ToastAlertService.php",
            text: "<?php\n",
            location: first
        )

        XCTAssertEqual(
            panel.titleText,
            "ToastAlertService.php  app/Services - References"
        )
        XCTAssertEqual(panel.statusText, "(2)")
    }

    func testPreviewDrawsCodePixels() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let target = location("/workspace/src/a.ts", line: 1, column: 0)
        panel.update([target], workspaceRoot: "/workspace")
        panel.showPreview(
            path: "/workspace/src/a.ts",
            text: "const visibleText = 1;\nconst selected = 2;\n",
            location: target,
            gutterWidth: 76
        )
        panel.layoutSubtreeIfNeeded()

        let preview = panel.previewContentView
        let renderedLines = preview.subviews.compactMap { ($0 as? NSTextField)?.stringValue }
        XCTAssertTrue(renderedLines.contains("const visibleText = 1;"))
        XCTAssertTrue(renderedLines.contains("const selected = 2;"))
        let rep = try XCTUnwrap(preview.bitmapImageRepForCachingDisplay(in: preview.bounds))
        preview.cacheDisplay(in: preview.bounds, to: rep)
        let scale = CGFloat(rep.pixelsWide) / preview.bounds.width
        let background = try XCTUnwrap(
            rep.colorAt(x: 2, y: 2)?
                .usingColorSpace(.sRGB)
        )
        var differentPixels = 0
        for x in stride(from: 8.0, to: min(preview.bounds.width, 260), by: 2) {
            for y in stride(from: 0, to: rep.pixelsHigh, by: 2) {
                guard let color = rep.colorAt(x: Int(x * scale), y: y)?
                    .usingColorSpace(.sRGB) else { continue }
                if abs(color.redComponent - background.redComponent) > 0.08
                    || abs(color.greenComponent - background.greenComponent) > 0.08
                    || abs(color.blueComponent - background.blueComponent) > 0.08 {
                    differentPixels += 1
                }
            }
        }
        XCTAssertGreaterThan(differentPixels, 5, "preview code must produce visible glyph pixels")
    }

    func testPreviewDrawsAfterScrollingToReference() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let window = NSWindow(
            contentRect: panel.frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = panel
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }

        let target = location("/workspace/src/index.ts", line: 24, column: 7)
        let text = (0..<50).map { "const visibleLine\($0) = \($0);" }.joined(separator: "\n")
        panel.update([target], workspaceRoot: "/workspace")
        panel.showPreview(path: "/workspace/src/index.ts", text: text, location: target, gutterWidth: 76)
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))

        let preview = panel.previewContentView
        let visible = preview.visibleRect
        XCTAssertGreaterThan(visible.minY, 0)
        let rep = try XCTUnwrap(preview.bitmapImageRepForCachingDisplay(in: visible))
        preview.cacheDisplay(in: visible, to: rep)
        let background = try XCTUnwrap(rep.colorAt(x: 2, y: 2)?.usingColorSpace(.sRGB))
        var hasInk = false
        for x in stride(from: 8, to: min(rep.pixelsWide, 260), by: 2) {
            for y in stride(from: 0, to: rep.pixelsHigh, by: 2) {
                guard let color = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if abs(color.redComponent - background.redComponent) > 0.08
                    || abs(color.greenComponent - background.greenComponent) > 0.08
                    || abs(color.blueComponent - background.blueComponent) > 0.08 {
                    hasInk = true
                    break
                }
            }
            if hasInk { break }
        }
        XCTAssertTrue(hasInk, "scrolled preview must redraw source glyphs in its new visible rect")
    }

    func testPreloadedSnippetAppearsWithoutSelectingReference() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let target = location("/workspace/src/a.ts", line: 3, column: 2)
        panel.update([target], workspaceRoot: "/workspace")

        panel.setSnippets([(target, "  const answer = value;\n")])

        let table = NSTableView()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference"))
        table.addTableColumn(column)
        let cell = try XCTUnwrap(panel.tableView(table, viewFor: column, row: 1) as? ReferencesResultCell)
        XCTAssertEqual(cell.sourceLabel.stringValue, "const answer = value;")
    }

    func testReferenceRowsUseFixedLocationColumnAndTailTruncation() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let short = location("/workspace/src/a.ts", line: 6, column: 9)
        let long = location("/workspace/src/a.ts", line: 128, column: 16)
        panel.update([short, long], workspaceRoot: "/workspace")
        panel.setSnippets([
            (short, "import { createMiniHost } from './mini-host'"),
            (long, "export function createMiniHost(input: Input)"),
        ])
        let table = NSTableView(frame: NSRect(x: 0, y: 0, width: 280, height: 100))
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference"))
        column.width = 280
        table.addTableColumn(column)

        let first = try XCTUnwrap(panel.tableView(table, viewFor: column, row: 1) as? ReferencesResultCell)
        let second = try XCTUnwrap(panel.tableView(table, viewFor: column, row: 2) as? ReferencesResultCell)
        first.frame = NSRect(x: 0, y: 0, width: 280, height: 28)
        second.frame = first.frame
        first.layoutSubtreeIfNeeded()
        second.layoutSubtreeIfNeeded()

        XCTAssertEqual(first.sourceLabel.frame.minX, second.sourceLabel.frame.minX)
        XCTAssertEqual(first.sourceLabel.lineBreakMode, .byTruncatingTail)
        XCTAssertEqual(first.sourceLabel.stringValue, "import { createMiniHost } from './mini-host'")
        XCTAssertEqual(second.locationLabel.stringValue, "129:17")
    }

    func testReferenceRowsHaveBalancedVerticalPaddingAndReadableDisclosure() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        panel.update([location("/workspace/src/a.ts", line: 6, column: 9)], workspaceRoot: "/workspace")
        let table = NSTableView(frame: NSRect(x: 0, y: 0, width: 280, height: 100))
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference"))
        column.width = 280
        table.addTableColumn(column)

        XCTAssertEqual(panel.tableView(table, heightOfRow: 0), 28)
        XCTAssertEqual(panel.tableView(table, heightOfRow: 1), 26)

        let header = try XCTUnwrap(panel.tableView(table, viewFor: column, row: 0) as? ReferencesFileCell)
        header.frame = NSRect(x: 0, y: 0, width: 280, height: 28)
        header.layoutSubtreeIfNeeded()
        XCTAssertEqual(header.disclosureLabel.font?.pointSize, 12)
        XCTAssertEqual(header.nameLabel.lineBreakMode, .byTruncatingMiddle)
        XCTAssertEqual(header.disclosureLabel.frame.minY, header.bounds.height - header.disclosureLabel.frame.maxY)

        let result = try XCTUnwrap(panel.tableView(table, viewFor: column, row: 1) as? ReferencesResultCell)
        result.frame = NSRect(x: 0, y: 0, width: 280, height: 26)
        result.layoutSubtreeIfNeeded()
        XCTAssertEqual(result.sourceLabel.frame.minY, result.bounds.height - result.sourceLabel.frame.maxY)
    }

    func testCloseButtonCallbackCanDismissThePeek() {
        let panel = ReferencesPanel(frame: .zero)
        var closed = false
        panel.onClose = { closed = true }
        panel.dismiss()
        XCTAssertTrue(closed)
    }

    func testFindReferencesShowsPeekInTheActiveEditor() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "const value = 1;\nconsole.log(value);\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.window?.setContentSize(NSSize(width: 900, height: 600))
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.activeTab?.doc.goToLine(line: 1, col: 14)

        controller.findReferences(nil)
        controller.refreshReferences()

        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        XCTAssertTrue(panel.superview === controller.groupChrome)
        XCTAssertFalse(panel.isHidden)
        XCTAssertGreaterThan(panel.frame.width, 300)
        XCTAssertGreaterThan(panel.frame.height, 100)
    }

    func testFindReferencesShowsVisibleFeedbackWhenCursorHasNoSymbol() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "const value = 1;\n\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.window?.setContentSize(NSSize(width: 900, height: 600))
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.activeTab?.doc.goToLine(line: 1, col: 0)

        controller.findReferences(nil)

        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        XCTAssertTrue(panel.superview === controller.groupChrome)
        XCTAssertFalse(panel.isHidden)
    }

    func testPendingReferencesShowsWorkspaceIndexProgress() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).rs")
        try "fn main() {}\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.workspaceIndexState = (url.deletingLastPathComponent().path, 12, 80)

        controller.findReferences(nil)
        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        XCTAssertEqual(panel.statusText, "Finding references...")
        XCTAssertTrue(panel.statusDetail.contains("Indexing 12 of 80 files"))
    }

    func testInFlightReferencesDoNotTimeOutWhileLazyIndexBuilds() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "export function value() {}\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        let document = try XCTUnwrap(controller.activeTab?.doc)
        controller.findReferences(nil)
        let cursor = document.primaryCursor()
        controller.referencesRequest = (
            document,
            TextPos(line: cursor.line, colUtf16: cursor.colUtf16),
            false,
            true,
            Date.distantPast,
            Date.distantPast
        )

        controller.refreshReferences()

        XCTAssertNotNil(controller.referencesRequest)
        XCTAssertEqual(controller.loadedReferencesPanel?.statusText, "Finding references...")
    }

    func testPartialReferencesStayPendingUntilIndexingFinishes() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("commands.ts")
        try "export const Commands = {};\nCommands;\n"
            .write(to: url, atomically: true, encoding: .utf8)

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.activeTab?.doc.goToLine(line: 0, col: 14)
        controller.workspaceIndexState = (directory.path, 1, 10)

        controller.findReferences(nil)

        XCTAssertNotNil(controller.referencesRequest)
        waitUntil { controller.loadedReferencesPanel?.statusText == "(2)" }
        XCTAssertEqual(controller.loadedReferencesPanel?.statusText, "(2)")

        controller.workspaceIndexState = nil
        controller.referencesRequest?.lastAttempt = .distantPast
        controller.refreshReferences()
        waitUntil { controller.referencesRequest == nil }
        XCTAssertNil(controller.referencesRequest)
    }

    func testCommandClickOnDeclarationShowsReferences() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "const value = 1;\nconsole.log(value);\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.window?.setContentSize(NSSize(width: 900, height: 600))
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        let click = TextPos(line: 0, colUtf16: 8)

        controller.showDefinitionLocations([location(url.path, line: 0, column: 6)], at: click)
        controller.refreshReferences()

        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        XCTAssertTrue(panel.superview === controller.groupChrome)
        XCTAssertEqual(panel.titleText, "References")
        XCTAssertEqual(controller.activeTab?.doc.primaryCursor().colUtf16, 8)
    }

    func testCommandClickOnUsageStillJumps() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "const value = 1;\nconsole.log(value);\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.activeTab?.doc.goToLine(line: 1, col: 14)

        controller.showDefinitionLocations(
            [location(url.path, line: 0, column: 6)],
            at: TextPos(line: 1, colUtf16: 14)
        )

        XCTAssertNil(controller.referencesRequest)
        XCTAssertNil(controller.loadedReferencesPanel?.superview)
        XCTAssertEqual(controller.activeTab?.doc.primaryCursor().line, 0)
    }

    func testFindReferencesPreviewShowsSourceOfOpenFile() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-references-\(UUID().uuidString).ts")
        try "// header\nexport const password = 1;\nconsole.log(password);\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.window?.setContentSize(NSSize(width: 1000, height: 700))
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        controller.activeTab?.doc.goToLine(line: 1, col: 15)

        controller.findReferences(nil)
        waitUntil { controller.loadedReferencesPanel?.statusText.hasPrefix("(") == true }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))

        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        let rendered = panel.previewContentView.subviews
            .compactMap { $0 as? NSTextField }
            .filter { !$0.isHidden }
            .map(\.stringValue)
        XCTAssertTrue(rendered.contains("export const password = 1;"), "rendered: \(rendered)")
        XCTAssertTrue(rendered.contains("// header"), "rendered: \(rendered)")
    }

    func testReferencePreviewHighlightsFileThatIsNotOpen() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-reference-preview-\(UUID().uuidString).ts")
        try "export const highlighted = 1;\nconsole.log(highlighted);\n"
            .write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.newDocument(nil)
        let target = location(url.path, line: 0, column: 13)
        controller.showDefinitionLocations(
            [target, location("/workspace/other.ts", line: 0, column: 0)],
            at: TextPos(line: 0, colUtf16: 0)
        )

        controller.previewReference(target)
        waitUntil { controller.loadedReferencesPanel?.previewHasSyntaxHighlighting == true }

        XCTAssertEqual(controller.loadedReferencesPanel?.previewHasSyntaxHighlighting, true)
    }

    func testReferencePreviewUsesTheEditorWhitespaceMarkers() {
        XCTAssertEqual(EditorView.whitespaceColumns(in: "    let value = 1", mode: .all), [0, 1, 2, 3, 7, 13, 15])
    }

    func testPreviewSourceIsVisibleThroughScrollViewBesideRuler() throws {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let window = NSWindow(
            contentRect: panel.frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = panel
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        let target = location("/workspace/src/env.ts", line: 9, column: 13)
        let text = (0..<30).map { "export const visibleLine\($0) = Config.value(\($0));" }
            .joined(separator: "\n")
        panel.update([target], workspaceRoot: "/workspace")
        panel.showPreview(path: "/workspace/src/env.ts", text: text, location: target, gutterWidth: 60)
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))

        // Render the scroll view, not the document view: the ruler is a
        // sibling above the clip view and is what hid the source before.
        let scroll = try XCTUnwrap(panel.previewContentView.enclosingScrollView)
        let rep = try XCTUnwrap(scroll.bitmapImageRepForCachingDisplay(in: scroll.bounds))
        scroll.cacheDisplay(in: scroll.bounds, to: rep)
        let scale = CGFloat(rep.pixelsWide) / scroll.bounds.width
        let background = try XCTUnwrap(
            rep.colorAt(x: Int(200 * scale), y: rep.pixelsHigh - 2)?.usingColorSpace(.sRGB)
        )
        var ink = 0
        for x in stride(from: 70.0, to: 400, by: 2) {
            for y in stride(from: 0, to: rep.pixelsHigh, by: 2) {
                guard let color = rep.colorAt(x: Int(x * scale), y: y)?.usingColorSpace(.sRGB) else { continue }
                if abs(color.redComponent - background.redComponent) > 0.15
                    || abs(color.greenComponent - background.greenComponent) > 0.15
                    || abs(color.blueComponent - background.blueComponent) > 0.15 {
                    ink += 1
                }
            }
        }
        XCTAssertGreaterThan(ink, 50, "source text must be visible right of the line-number gutter")
        let first = try XCTUnwrap(panel.previewContentView.subviews
            .compactMap { $0 as? NSTextField }.first { !$0.isHidden })
        XCTAssertEqual(first.frame.minX, 0, "code starts at the ruler edge, with no extra left space")
    }

    private func makeScrolledPanel(scopes: @escaping @Sendable (Int) -> [Int]) throws -> (ReferencesPanel, NSWindow) {
        let panel = ReferencesPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        let window = NSWindow(contentRect: panel.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = panel
        window.orderFrontRegardless()
        let target = location("/workspace/src/scopes.ts", line: 40, column: 2)
        let text = (0..<80).map { "line \($0)" }.joined(separator: "\n")
        panel.update([target], workspaceRoot: "/workspace")
        panel.showPreview(
            path: "/workspace/src/scopes.ts",
            text: text,
            location: target,
            gutterWidth: 60,
            stickyScopes: scopes
        )
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        return (panel, window)
    }

    func testPreviewPinsEnclosingScopeHeaderWhileScrolled() throws {
        let (panel, window) = try makeScrolledPanel { line in line >= 1 ? [0] : [] }
        defer { window.orderOut(nil) }
        XCTAssertEqual(panel.previewStickyLines, [0], "the block header stays pinned over the scrolled preview")
    }

    func testPreviewPinsNothingAtTheTop() throws {
        let (panel, window) = try makeScrolledPanel { line in line >= 1 ? [0] : [] }
        defer { window.orderOut(nil) }
        let scroll = try XCTUnwrap(panel.previewContentView.enclosingScrollView)
        scroll.contentView.scroll(to: .zero)
        scroll.reflectScrolledClipView(scroll.contentView)
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        XCTAssertTrue(panel.previewStickyLines.isEmpty)
    }

    func testPreviewWithoutScopesPinsNothing() throws {
        let (panel, window) = try makeScrolledPanel { _ in [] }
        defer { window.orderOut(nil) }
        XCTAssertTrue(panel.previewStickyLines.isEmpty)
    }

    func testMultipleDefinitionTargetsOpenPeek() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.window?.setContentSize(NSSize(width: 900, height: 600))
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.newDocument(nil)
        let first = location("/workspace/src/first.ts", line: 1, column: 2)
        let second = location("/workspace/src/second.ts", line: 3, column: 4)

        controller.showDefinitionLocations(
            [first, second],
            at: TextPos(line: 0, colUtf16: 0)
        )

        let panel = try XCTUnwrap(controller.loadedReferencesPanel)
        XCTAssertTrue(panel.superview === controller.groupChrome)
        XCTAssertEqual(panel.titleText, "Definitions")
        XCTAssertEqual(panel.statusText, "(2)")
    }
}
