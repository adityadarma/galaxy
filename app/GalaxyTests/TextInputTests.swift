import XCTest
@testable import Galaxy

/// Drives `EditorView` through the `NSTextInputClient` entry points the
/// same way an input method does. Real IME behaviour (Kotoeri, Pinyin,
/// Korean, the emoji picker) still needs manual testing.
@MainActor
final class TextInputTests: XCTestCase {
    private func makeView(text: String, caretAt offset: UInt64 = 0) throws -> (EditorView, DocumentHandle, URL) {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        let doc = try CoreBridge().openDocument(path: url.path)
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 200))
        view.document = doc
        let p = doc.positionAt(offset: offset)
        doc.setSelections(selections: [TextSelection(anchor: p, head: p)], primary: 0)
        return (view, doc, url)
    }

    private let none = NSRange(location: NSNotFound, length: 0)

    func testJapaneseCompositionIsOneUndoStep() throws {
        let (view, doc, url) = try makeView(text: "")
        defer { try? FileManager.default.removeItem(at: url) }

        for preedit in ["k", "ka", "か", "かん"] {
            view.setMarkedText(preedit, selectedRange: NSRange(location: preedit.utf16.count, length: 0),
                               replacementRange: none)
            XCTAssertTrue(view.hasMarkedText())
            XCTAssertEqual(view.markedRange(), NSRange(location: 0, length: preedit.utf16.count))
        }
        view.insertText("漢字", replacementRange: none)
        XCTAssertFalse(view.hasMarkedText())
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["漢字"])
        XCTAssertEqual(view.selectedRange(), NSRange(location: 2, length: 0))

        view.undoEdit(nil)
        XCTAssertEqual(doc.lines(first: 0, count: 1), [""], "whole composition undoes at once")
        view.redoEdit(nil)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["漢字"])
    }

    func testMarkedTextIsReportedForUnderline() throws {
        let (view, doc, url) = try makeView(text: "ab")
        defer { try? FileManager.default.removeItem(at: url) }
        doc.setSelections(selections: [TextSelection(
            anchor: TextPos(line: 0, colUtf16: 2), head: TextPos(line: 0, colUtf16: 2))], primary: 0)
        view.setMarkedText("あい", selectedRange: NSRange(location: 2, length: 0), replacementRange: none)
        let span = try XCTUnwrap(doc.viewport(firstLine: 0, count: 1).lines[0].marked)
        XCTAssertEqual(span.start, 2)
        XCTAssertEqual(span.end, 4)
        view.unmarkText()
        XCTAssertNil(doc.viewport(firstLine: 0, count: 1).lines[0].marked)
    }

    func testDeadKeyAccentComposesToOneCharacter() throws {
        let (view, doc, url) = try makeView(text: "caf", caretAt: 3)
        defer { try? FileManager.default.removeItem(at: url) }
        view.setMarkedText("´", selectedRange: NSRange(location: 1, length: 0), replacementRange: none)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["caf´"])
        view.insertText("é", replacementRange: none)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["café"])
        view.undoEdit(nil)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["caf"])
    }

    func testEmojiPickerAndAccentMenuReplaceARange() throws {
        let (view, doc, url) = try makeView(text: "ab", caretAt: 2)
        defer { try? FileManager.default.removeItem(at: url) }
        // Press-and-hold "a" then choose "á": replaces the character before the caret.
        view.insertText("á", replacementRange: NSRange(location: 1, length: 1))
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["aá"])
        // Emoji picker inserts at the caret.
        view.insertText("😀", replacementRange: none)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["aá😀"])
        XCTAssertEqual(view.selectedRange(), NSRange(location: 4, length: 0), "emoji is two UTF-16 units")
    }

    func testPlainTypingGroupsIntoOneUndoStep() throws {
        let (view, doc, url) = try makeView(text: "")
        defer { try? FileManager.default.removeItem(at: url) }
        for ch in "hello" { view.insertText(String(ch), replacementRange: none) }
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["hello"])
        view.undoEdit(nil)
        XCTAssertEqual(doc.lines(first: 0, count: 1), [""])
    }

    func testTypingPairsDelimitersAndSkipsTheClosingCharacter() throws {
        let (view, doc, url) = try makeView(text: "")
        defer { try? FileManager.default.removeItem(at: url) }
        view.insertText("(", replacementRange: none)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["()"])
        XCTAssertEqual(view.selectedRange(), NSRange(location: 1, length: 0))
        view.insertText(")", replacementRange: none)
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["()"])
        XCTAssertEqual(view.selectedRange(), NSRange(location: 2, length: 0))

        // Replacement text from an input method is not auto-paired.
        view.insertText("\"", replacementRange: NSRange(location: 0, length: 1))
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["\")"])
    }

    func testAttributedSubstringAndRangesAreDocumentWide() throws {
        let (view, _, url) = try makeView(text: "😀\nabc")
        defer { try? FileManager.default.removeItem(at: url) }
        var actual = NSRange()
        let s = view.attributedSubstring(forProposedRange: NSRange(location: 0, length: 99), actualRange: &actual)
        XCTAssertEqual(s?.string, "😀\nabc")
        XCTAssertEqual(actual, NSRange(location: 0, length: 6))
        XCTAssertEqual(view.attributedSubstring(forProposedRange: NSRange(location: 3, length: 2), actualRange: nil)?.string, "ab")
        XCTAssertNil(view.attributedSubstring(forProposedRange: none, actualRange: nil))
    }

    func testCandidateWindowRectFollowsTheMarkedText() throws {
        let (view, _, url) = try makeView(text: "abc\ndef")
        defer { try? FileManager.default.removeItem(at: url) }
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 600, height: 200),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = view

        let onLine0 = view.firstRect(forCharacterRange: NSRange(location: 1, length: 0), actualRange: nil)
        let onLine1 = view.firstRect(forCharacterRange: NSRange(location: 5, length: 0), actualRange: nil)
        XCTAssertEqual(onLine0.height, view.metrics.lineHeight)
        // Screen coordinates grow upward, so the lower line has a smaller y.
        XCTAssertEqual(onLine0.minY - onLine1.minY, view.metrics.lineHeight, accuracy: 0.5)
        XCTAssertGreaterThan(onLine0.minX, window.frame.minX + view.textOriginX)
    }

    func testCharacterIndexInvertsFirstRect() throws {
        let (view, _, url) = try makeView(text: "hello\nworld")
        defer { try? FileManager.default.removeItem(at: url) }
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 600, height: 200),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = view
        for location in [0, 2, 5, 6, 9, 11] {
            let rect = view.firstRect(forCharacterRange: NSRange(location: location, length: 0), actualRange: nil)
            let probe = NSPoint(x: rect.minX + 0.5, y: rect.midY)
            XCTAssertEqual(view.characterIndex(for: probe), location, "offset \(location)")
        }
    }

    func testEditingCommandsAndViewSizeStayInSync() throws {
        let (view, doc, url) = try makeView(text: "ab", caretAt: 2)
        defer { try? FileManager.default.removeItem(at: url) }
        view.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        XCTAssertEqual(doc.lineCount(), 2)
        XCTAssertEqual(view.lineCount, 2, "view resizes after the document changes")
        view.doCommand(by: #selector(NSResponder.deleteBackward(_:)))
        XCTAssertEqual(doc.lineCount(), 1)
        XCTAssertEqual(view.lineCount, 1)
        view.doCommand(by: #selector(NSResponder.insertTab(_:)))
        XCTAssertEqual(doc.lines(first: 0, count: 1), ["ab  "])
    }
}
