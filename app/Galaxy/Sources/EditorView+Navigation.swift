import AppKit
import CoreText

/// Cursor movement, selection, mouse and clipboard (M1-11).
///
/// Keys arrive through `interpretKeyEvents` as standard `NSResponder`
/// selectors (`moveLeft:`, `moveWordRightAndModifySelection:` ...), so the
/// user's system key bindings (including Emacs-style Ctrl keys) just work.
extension EditorView {
    /// Pasteboard type that marks text copied as whole lines, so a later
    /// paste goes above the caret's line (VSCode behavior).
    static let linewiseType = NSPasteboard.PasteboardType("dev.adityadarma.galaxy.linewise-copy")

    // MARK: Key bindings

    /// `(motion, extend)`; page motions get their line count at use.
    private static let bindings: [Selector: (CursorMotion, Bool)] = {
        var table: [Selector: (CursorMotion, Bool)] = [:]
        func add(_ plain: Selector, _ shifted: Selector, _ motion: CursorMotion) {
            table[plain] = (motion, false)
            table[shifted] = (motion, true)
        }
        typealias R = NSResponder
        add(#selector(R.moveLeft(_:)), #selector(R.moveLeftAndModifySelection(_:)), .left)
        add(#selector(R.moveBackward(_:)), #selector(R.moveBackwardAndModifySelection(_:)), .left)
        add(#selector(R.moveRight(_:)), #selector(R.moveRightAndModifySelection(_:)), .right)
        add(#selector(R.moveForward(_:)), #selector(R.moveForwardAndModifySelection(_:)), .right)
        add(#selector(R.moveUp(_:)), #selector(R.moveUpAndModifySelection(_:)), .up)
        add(#selector(R.moveDown(_:)), #selector(R.moveDownAndModifySelection(_:)), .down)
        add(#selector(R.moveWordLeft(_:)), #selector(R.moveWordLeftAndModifySelection(_:)), .wordLeft)
        add(#selector(R.moveWordBackward(_:)), #selector(R.moveWordBackwardAndModifySelection(_:)), .wordLeft)
        add(#selector(R.moveWordRight(_:)), #selector(R.moveWordRightAndModifySelection(_:)), .wordRight)
        add(#selector(R.moveWordForward(_:)), #selector(R.moveWordForwardAndModifySelection(_:)), .wordRight)
        add(#selector(R.moveToBeginningOfLine(_:)), #selector(R.moveToBeginningOfLineAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToLeftEndOfLine(_:)), #selector(R.moveToLeftEndOfLineAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToBeginningOfParagraph(_:)), #selector(R.moveToBeginningOfParagraphAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToEndOfLine(_:)), #selector(R.moveToEndOfLineAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToRightEndOfLine(_:)), #selector(R.moveToRightEndOfLineAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToEndOfParagraph(_:)), #selector(R.moveToEndOfParagraphAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToBeginningOfDocument(_:)), #selector(R.moveToBeginningOfDocumentAndModifySelection(_:)), .docStart)
        add(#selector(R.moveToEndOfDocument(_:)), #selector(R.moveToEndOfDocumentAndModifySelection(_:)), .docEnd)
        add(#selector(R.pageUp(_:)), #selector(R.pageUpAndModifySelection(_:)), .pageUp(lines: 0))
        add(#selector(R.pageDown(_:)), #selector(R.pageDownAndModifySelection(_:)), .pageDown(lines: 0))
        // Home/End and Page keys are "scroll" commands by default on macOS;
        // in an editor they move the caret (Home/End act on the line).
        table[#selector(R.scrollToBeginningOfDocument(_:))] = (.lineStart, false)
        table[#selector(R.scrollToEndOfDocument(_:))] = (.lineEnd, false)
        table[#selector(R.scrollPageUp(_:))] = (.pageUp(lines: 0), false)
        table[#selector(R.scrollPageDown(_:))] = (.pageDown(lines: 0), false)
        return table
    }()

    /// Lines in one page, keeping one line of overlap.
    var pageLines: UInt32 {
        // Counted in rows when wrap is on; the core moves by visual row.
        UInt32(max(1, Int(visibleRect.height / metrics.lineHeight) - 1))
    }

    /// Runs a navigation selector. Returns `false` if `selector` is not one.
    func performNavigation(_ selector: Selector) -> Bool {
        guard let document else { return false }
        if selector == #selector(NSResponder.selectAll(_:)) {
            document.selectAll()
            return true
        }
        guard let (motion, extend) = Self.bindings[selector] else { return false }
        let resolved: CursorMotion = switch motion {
        case .pageUp: .pageUp(lines: pageLines)
        case .pageDown: .pageDown(lines: pageLines)
        default: motion
        }
        document.moveCursors(motion: resolved, extend: extend)
        return true
    }

    override func selectAll(_ sender: Any?) {
        document?.selectAll()
        reloadDocument()
    }

    // MARK: Mouse

    /// Document position under `point` (view coordinates). Points above,
    /// below or beside the text clamp to the nearest valid position.
    func textPosition(at point: NSPoint) -> TextPos {
        guard let document, lineCount > 0 else { return TextPos(line: 0, colUtf16: 0) }
        let (line, row) = self.line(atY: min(max(point.y, 0), CGFloat(max(rowCount, 1)) * metrics.lineHeight - 1))
        let text = document.displayLine(line: line)
        guard !text.isEmpty else { return TextPos(line: line, colUtf16: 0) }
        let rows = lineRows(line)
        // Later rows are drawn shifted left by where they start.
        let shift = rowShift(in: text, rows: rows, row: row)
        let localX = point.x - textOriginX + shift
        var index = max(0, CTLineGetStringIndexForPosition(layout(for: text), CGPoint(x: localX, y: 0)))
        // Core Text puts the caret of a hinted argument halfway through its
        // label's room, so the left half of the argument's first glyph would
        // hit the next slot. Split that glyph at its own middle instead.
        if index > 0, inlineHints[text]?.contains(where: { $0.column == index - 1 }) == true {
            let glyphStart = xOffset(in: text, column: UInt64(index - 1))
            let glyphEnd = xOffset(in: text, column: UInt64(index))
            if localX < (glyphStart + glyphEnd) / 2 { index -= 1 }
        }
        if rows.count > 1 {
            // A point past a wrapped row's end is the row's last slot, not
            // the first column of the next row.
            let upper = row < rows.breaks.count ? rows.end(of: row) - 1 : Int.max
            index = min(max(index, rows.start(of: row)), upper)
        }
        return TextPos(line: line, colUtf16: UInt64(index))
    }

    override func mouseDown(with event: NSEvent) {
        guard let document else { return super.mouseDown(with: event) }
        window?.makeFirstResponder(self)
        if document.hasMarkedText() { inputContext?.discardMarkedText() }
        let local = convert(event.locationInWindow, from: nil)
        // The peek zone swallows clicks; its header buttons act.
        if let hit = inlineHunkHit(at: local) {
            if case .button(let action) = hit { onInlineHunkAction?(action) }
            return
        }
        // An inferred return type is a button: a double-click writes it into the code.
        if let hint = returnTypeHint(at: local) {
            if event.clickCount == 2 { onReturnTypeHintDoubleClick?(hint.line, hint.column) }
            return
        }
        let pos = textPosition(at: local)
        // The "..." after a folded line opens it again.
        if event.clickCount == 1, let hit = foldPillRects.first(where: { $0.value.contains(local) }) {
            toggleFold(line: UInt64(hit.key))
            return
        }
        let barMinX = gutterShift + gitBarX - 2
        if diffSide == nil, local.x >= barMinX, local.x < barMinX + Self.gitBarWidth + 4,
           gitLineChanges[pos.line + 1] != nil, event.clickCount == 1 {
            onGitChangeClick?(pos.line)
            return
        }
        if local.x < gutterWidth + gutterShift, event.clickCount == 1 {
            // A test's run icon, in the left padding of the gutter.
            if local.x - gutterShift < Self.runMarkerWidth + 2, runMarkers[pos.line] != nil, let onRunTest {
                onRunTest(pos.line)
                return
            }
            // The chevron column folds; the line numbers set or clear a breakpoint.
            if local.x - gutterShift >= foldZoneMinX, !wrapSuspended, document.foldState(line: pos.line) != .plain {
                toggleFold(line: pos.line)
                return
            }
            if let onToggleBreakpoint {
                onToggleBreakpoint(pos.line)
                return
            }
        }
        let flags = event.modifierFlags
        // ⌘+click on a keyword or comment is a plain click, not a lookup.
        if flags.contains(.command), event.clickCount == 1, let range = navigableRange(at: pos) {
            // References requests require the caret to remain on their symbol
            // while the asynchronous definition lookup completes. Context-menu
            // navigation already does this; Cmd-click must do the same.
            let lookupPosition = definitionPosition(at: pos, range: range)
            document.setCaret(pos: lookupPosition)
            if onDefinitionClick?(lookupPosition) == true {
                reloadDocument()
                return
            }
        }
        columnAnchor = nil
        if flags.contains(.option), flags.contains(.shift) {
            // Opt+Shift+drag: column (box) selection.
            let anchor = columnPosition(at: local)
            columnAnchor = anchor
            document.columnSelect(anchor: anchor, head: anchor)
            reloadDocument()
            return
        }
        switch event.clickCount {
        case 2: document.selectWordAt(pos: pos)
        case 3...: document.selectLineAt(pos: pos)
        default:
            if flags.contains(.shift) {
                document.extendSelectionTo(pos: pos)
            } else if flags.contains(.option) {
                document.addCaret(pos: pos)
            } else {
                let before = document.primaryCursor()
                document.setCaret(pos: pos)
                if let onClickMove {
                    onClickMove(before, document.primaryCursor())
                }
            }
        }
        reloadDocument()
    }

    /// Right click in the text. As in VSCode, a click outside the selection
    /// moves the caret there first so the menu acts on what was clicked.
    override func menu(for event: NSEvent) -> NSMenu? {
        guard let document, let onContextMenu else { return super.menu(for: event) }
        let local = convert(event.locationInWindow, from: nil)
        guard local.x >= gutterWidth + gutterShift else { return nil }
        window?.makeFirstResponder(self)
        let pos = textPosition(at: local)
        let selection = selectedRange()
        let offset = Int(document.utf16Offset(pos: pos))
        if selection.length == 0 || offset < selection.location || offset > selection.location + selection.length {
            document.setCaret(pos: pos)
            reloadDocument()
        }
        return onContextMenu()
    }

    override func mouseDragged(with event: NSEvent) {
        guard let document else { return }
        autoscroll(with: event)
        let local = convert(event.locationInWindow, from: nil)
        if let anchor = columnAnchor {
            document.columnSelect(anchor: anchor, head: columnPosition(at: local))
        } else {
            document.extendSelectionTo(pos: textPosition(at: local))
        }
        reloadDocument()
    }

    override func mouseUp(with event: NSEvent) {
        columnAnchor = nil
    }

    /// Like `textPosition(at:)`, but a point to the right of a short line
    /// keeps its column (counted in space widths), so a box dragged past
    /// the end of a line still spans the same columns on longer lines.
    func columnPosition(at point: NSPoint) -> TextPos {
        guard let document, lineCount > 0 else { return TextPos(line: 0, colUtf16: 0) }
        var pos = textPosition(at: point)
        let text = document.displayLine(line: pos.line)
        let lineWidth = text.isEmpty ? 0 : CGFloat(CTLineGetTypographicBounds(layout(for: text), nil, nil, nil))
        let beyond = point.x - textOriginX - lineWidth
        if beyond > metrics.spaceWidth / 2, metrics.spaceWidth > 0 {
            pos.colUtf16 = UInt64(text.utf16.count) + UInt64((beyond / metrics.spaceWidth).rounded())
        }
        return pos
    }

    /// Arrow over line numbers, I-beam (or link hand) over text, and a hand
    /// over active peek-header actions.
    override func resetCursorRects() {
        func addCursorRectIfValid(_ rect: NSRect, cursor: NSCursor) {
            // AppKit asks for cursor rects while restoring layout after sleep
            // and can briefly expose an empty or non-finite visible rect.
            // Passing either through addCursorRect raises an Objective-C
            // exception inside NSTrackingArea on macOS 26.
            guard rect.width > 0, rect.height > 0,
                  rect.minX.isFinite, rect.minY.isFinite,
                  rect.maxX.isFinite, rect.maxY.isFinite else { return }
            addCursorRect(rect, cursor: cursor)
        }
        let edge = gutterWidth + gutterShift
        let gutter = NSRect(x: gutterShift, y: visibleRect.minY, width: gutterWidth, height: visibleRect.height)
            .intersection(visibleRect)
        let text = NSRect(x: edge, y: visibleRect.minY,
                           width: max(0, visibleRect.maxX - edge), height: visibleRect.height)
        addCursorRectIfValid(gutter, cursor: .arrow)
        addCursorRectIfValid(text, cursor: definitionLink == nil ? .iBeam : .pointingHand)
        if let hunk = inlineGitHunk {
            let top = inlineHunkTop(hunk)
            for button in inlineHunkButtonRects(top: top)
            where Self.inlineHunkActionIsEnabled(button.action, hunk: hunk) {
                addCursorRectIfValid(button.rect.intersection(visibleRect), cursor: .pointingHand)
            }
        }
    }

    /// Shows or clears the Cmd-hover link for the word under the mouse.
    func updateDefinitionLink(commandHeld: Bool) {
        guard commandHeld, onDefinitionClick != nil, let point = lastMousePoint,
              point.x >= gutterWidth + gutterShift, let document,
              Self.supportsDefinitionNavigation(document), lineCount > 0 else {
            definitionLink = nil
            return
        }
        let row = point.y / metrics.lineHeight
        guard row.isFinite, row >= 0, Int(row) < rowCount else { definitionLink = nil; return }
        let pos = textPosition(at: point)
        let text = document.displayLine(line: pos.line)
        // Hit-testing returns the nearest caret slot; ignore points past the text.
        let rows = lineRows(pos.line)
        let lineRow = rows.row(containing: Int(pos.colUtf16))
        let rowEnd = min(rows.end(of: lineRow), text.utf16.count)
        let shift = rowShift(in: text, rows: rows, row: lineRow)
        guard point.x - textOriginX + shift <= xOffset(in: text, column: UInt64(rowEnd)) else {
            definitionLink = nil
            return
        }
        definitionLink = navigableRange(at: pos, text: text)
            .map { (line: pos.line, start: UInt64($0.lowerBound), end: UInt64($0.upperBound)) }
    }

    /// The position to look up for a ⌘+click on the link `range` at `pos`.
    func definitionPosition(at pos: TextPos, range: Range<Int>) -> TextPos {
        guard let document, Self.isPHP(document) else { return pos }
        let text = document.displayLine(line: pos.line)
        let start = Self.lastSegmentStart(in: text, range: range)
        return start == range.lowerBound ? pos : TextPos(line: pos.line, colUtf16: UInt64(start))
    }

    /// Syntax scopes that never have a definition to jump to. Strings stay
    /// navigable: the PHP engine resolves `config('...')`, `route('...')`, etc.
    static let nonNavigableScopes: Set<String> = ["keyword", "comment", "number", "operator", "punctuation"]

    /// PHP scalar/pseudo types and literal constants: nothing to open.
    /// `self` and `parent` stay links since they resolve to a class.
    static let phpBuiltins: Set<String> = [
        "string", "int", "integer", "float", "double", "bool", "boolean", "array", "object",
        "mixed", "void", "never", "iterable", "callable", "null", "true", "false",
    ]

    /// Primitive types and literal constants of the other languages. They
    /// share the `type`/`constant` scopes with user declarations, but there is
    /// no declaration to open (the lib `.d.ts` interfaces behind them are noise).
    static let typeScriptBuiltins: Set<String> = [
        "string", "number", "boolean", "bigint", "symbol", "object", "any", "unknown", "never", "void",
        "null", "undefined", "true", "false",
    ]
    static let rustBuiltins: Set<String> = [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64", "bool", "char", "str", "true", "false",
    ]
    static let goBuiltins: Set<String> = [
        "string", "bool", "byte", "rune", "int", "int8", "int16", "int32", "int64", "uint", "uint8",
        "uint16", "uint32", "uint64", "uintptr", "float32", "float64", "complex64", "complex128",
        "nil", "true", "false", "iota",
    ]
    static let pythonBuiltins: Set<String> = ["int", "float", "str", "bool", "bytes", "None", "True", "False"]

    /// Names to leave unlinked for `document`'s language, and whether they
    /// compare case-insensitively (PHP only).
    static func builtins(for document: DocumentHandle) -> (names: Set<String>, caseInsensitive: Bool)? {
        if isPHP(document) { return (phpBuiltins, true) }
        switch document.syntaxLanguage() {
        case "TypeScript", "TSX", "JavaScript", "Vue", "Svelte": return (typeScriptBuiltins, false)
        case "Rust": return (rustBuiltins, false)
        case "Go": return (goBuiltins, false)
        case "Python": return (pythonBuiltins, false)
        default: return nil
        }
    }

    static func isJavaScriptFamily(_ document: DocumentHandle) -> Bool {
        ["TypeScript", "TSX", "JavaScript", "Vue", "Svelte"].contains(document.syntaxLanguage() ?? "")
    }

    /// Cmd-hover is for source code only. Configuration and markup grammars
    /// classify words for coloring, but their tokens have no definitions to open.
    static func supportsDefinitionNavigation(_ document: DocumentHandle) -> Bool {
        isPHP(document) || [
            "TypeScript", "TSX", "JavaScript", "Rust", "Go", "Python",
            "C", "C++", "C#", "Java", "Ruby", "Lua", "Swift", "SQL", "PowerShell",
            "Dart", "R", "Vue", "Svelte",
        ].contains(document.syntaxLanguage() ?? "")
    }

    /// Module path inside the quotes of a JavaScript/TypeScript import or a
    /// Dart import/export/part directive;
    /// `nil` for any other string.
    static func importSpecifierRange(in text: String, stringStart: Int, stringEnd: Int) -> Range<Int>? {
        let units = Array(text.utf16)
        guard stringStart >= 0, stringEnd <= units.count, stringEnd - stringStart >= 3 else { return nil }
        let quotes: Set<UInt16> = [UInt16(UInt8(ascii: "'")), UInt16(UInt8(ascii: "\""))]
        guard quotes.contains(units[stringStart]), units[stringStart] == units[stringEnd - 1] else { return nil }
        let before = (text as NSString).substring(to: stringStart)
        guard before.range(of: #"(?:\bfrom|\bimport|\bexport|\bpart|\brequire\s*\(|\bimport\s*\()\s*$"#,
                           options: .regularExpression) != nil else { return nil }
        return (stringStart + 1)..<(stringEnd - 1)
    }

    /// The module path under `pos` when it sits in an import/require string
    /// of a TypeScript, JavaScript, or Dart file, quotes excluded.
    func importSpecifier(at pos: TextPos, text: String? = nil) -> (range: Range<Int>, path: String)? {
        guard let document, Self.isJavaScriptFamily(document) || document.syntaxLanguage() == "Dart" else { return nil }
        let text = text ?? document.displayLine(line: pos.line)
        let column = Int(pos.colUtf16)
        if document.syntaxLanguage() == "Dart" {
            let units = Array(text.utf16)
            let quotes: Set<UInt16> = [UInt16(UInt8(ascii: "'")), UInt16(UInt8(ascii: "\""))]
            guard column < units.count else { return nil }
            guard let stringStart = (0...column).reversed().first(where: { quotes.contains(units[$0]) }),
                  let stringEnd = ((column + 1)..<units.count).first(where: { units[$0] == units[stringStart] }) else {
                return nil
            }
            let range = Self.importSpecifierRange(in: text, stringStart: stringStart, stringEnd: stringEnd + 1)
            return range.map { ($0, (text as NSString).substring(with: NSRange(location: $0.lowerBound, length: $0.count))) }
        }
        // The widest string run is the whole literal, quotes included.
        let run = document.viewport(firstLine: pos.line, count: 1).styles
            .filter { $0.line == pos.line && $0.scope == "string" && Int($0.start) <= column && column < Int($0.end) }
            .max { ($0.end - $0.start) < ($1.end - $1.start) }
        guard let run,
              let range = Self.importSpecifierRange(in: text, stringStart: Int(run.start), stringEnd: Int(run.end)) else { return nil }
        let path = (text as NSString).substring(with: NSRange(location: range.lowerBound, length: range.count))
        return (range, path)
    }

    /// File a relative import points at, trying the extensions and `index`
    /// files the module resolvers try. `nil` for packages (`react`,
    /// `node:fs`), which the built-in TypeScript engine resolves.
    static func resolveImport(_ spec: String, from file: String) -> String? {
        guard spec.hasPrefix("./") || spec.hasPrefix("../") || spec == "." || spec == ".." else { return nil }
        let dir = (file as NSString).deletingLastPathComponent
        let base = ((dir as NSString).appendingPathComponent(spec) as NSString).standardizingPath
        let manager = FileManager.default
        func isFile(_ path: String) -> Bool {
            var directory: ObjCBool = false
            return manager.fileExists(atPath: path, isDirectory: &directory) && !directory.boolValue
        }
        var candidates = ["", ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".json"].map { base + $0 }
        // TypeScript ESM imports name the compiled file (`./x.js` for `x.ts`).
        for (compiled, sources) in [(".js", [".ts", ".tsx", ".d.ts"]), (".jsx", [".tsx"]), (".mjs", [".mts"]), (".cjs", [".cts"])]
        where base.hasSuffix(compiled) {
            candidates += sources.map { String(base.dropLast(compiled.count)) + $0 }
        }
        candidates += ["index.ts", "index.tsx", "index.d.ts", "index.js", "index.jsx"]
            .map { (base as NSString).appendingPathComponent($0) }
        return candidates.first(where: isFile)
    }

    static func isPHP(_ document: DocumentHandle) -> Bool {
        let language = document.syntaxLanguage()?.lowercased() ?? ""
        return language.contains("php") || language.contains("blade")
            || document.path().lowercased().hasSuffix(".php")
    }

    /// Identifier under `pos` unless Tree-sitter marks it as a keyword,
    /// comment, number or operator.
    func navigableRange(at pos: TextPos, text: String? = nil) -> Range<Int>? {
        guard let document, Self.supportsDefinitionNavigation(document) else { return nil }
        let text = text ?? document.displayLine(line: pos.line)
        // Module paths in imports are links as a whole, punctuation included.
        if let specifier = importSpecifier(at: pos, text: text) { return specifier.range }
        guard let range = Self.identifierRange(in: text, at: Int(pos.colUtf16)) else { return nil }
        // Words between tags (`<p>Recorded admin</p>`) are text, not names.
        if document.isMarkupText(line: pos.line, colUtf16: UInt64(range.lowerBound)) { return nil }
        let styles = document.viewport(firstLine: pos.line, count: 1).styles.filter { $0.line == pos.line }
        // Look up the scope at the name itself: PHP grammars may style the
        // `$` of a variable as punctuation.
        let units = Array(text.utf16)
        let nameStart = units[range.lowerBound] == UInt16(UInt8(ascii: "$")) ? range.lowerBound + 1 : range.lowerBound
        // Several runs can cover the name (Vue puts a broad `function` capture
        // around a whole call, with the string inside it). A string literal
        // wins, and its widest run is the whole literal, quotes included.
        let covering = styles.filter { Int($0.start) <= nameStart && nameStart < Int($0.end) }
        let literal = covering.filter { $0.scope == "string" }.max { ($0.end - $0.start) < ($1.end - $1.start) }
        guard let run = literal ?? covering.first else {
            // Without a syntax run, literal text of a template string is not a name.
            if Self.isTemplateText(in: text, at: range.lowerBound) { return nil }
            return qualified(range, in: text, document: document)
        }
        if Self.nonNavigableScopes.contains(run.scope) { return nil }
        // Filter primitives only when syntax marks them as types/constants.
        // `table.string()` is a method, not the TypeScript `string` primitive.
        if ["type", "constant"].contains(run.scope), let builtins = Self.builtins(for: document) {
            let word = (text as NSString).substring(with: NSRange(location: range.lowerBound, length: range.count))
            if builtins.names.contains(builtins.caseInsensitive ? word.lowercased() : word) { return nil }
        }
        if run.scope == "string" {
            // Tree-sitter styles a template literal as one string run, including
            // its `${...}` expression. That expression is code, not string text.
            if Self.isTemplateInterpolation(in: text, at: range.lowerBound) { return qualified(range, in: text, document: document) }
            let stringStart = Int(run.start), stringEnd = Int(run.end)
            // An array element on its own line (`'detailKibD',`) belongs to a
            // call opened on an earlier line, so look at the lines above too.
            var source = text, offset = 0
            if Self.frameworkStringRange(in: text, stringStart: stringStart, stringEnd: stringEnd) == nil,
               Self.isPHP(document), pos.line > 0,
               text.utf16.prefix(stringStart).allSatisfy({ $0 == 0x20 || $0 == 0x09 }) {
                let first = pos.line > 400 ? pos.line - 400 : 0
                let prefix = (first..<pos.line).map { document.displayLine(line: $0) }.joined(separator: "\n") + "\n"
                source = prefix + text
                offset = prefix.utf16.count
            }
            guard let range = Self.frameworkStringRange(
                in: source, stringStart: offset + stringStart, stringEnd: offset + stringEnd
            ) else { return nil }
            let segment = Self.eloquentRelationSegmentRange(
                in: source, stringStart: offset + stringStart, stringEnd: offset + stringEnd,
                at: offset + Int(pos.colUtf16)
            ) ?? range
            return (segment.lowerBound - offset)..<(segment.upperBound - offset)
        }
        return qualified(range, in: text, document: document)
    }

    /// Whether `column` sits inside a `${...}` expression in a template literal.
    static func isTemplateInterpolation(in text: String, at column: Int) -> Bool {
        templateState(in: text, at: column).interpolation
    }

    /// Whether `column` sits in the literal text of a template literal.
    static func isTemplateText(in text: String, at column: Int) -> Bool {
        let state = templateState(in: text, at: column)
        return state.inTemplate && !state.interpolation
    }

    private static func templateState(in text: String, at column: Int) -> (inTemplate: Bool, interpolation: Bool) {
        let units = Array(text.utf16)
        guard column >= 0, column < units.count else { return (false, false) }
        let backtick = UInt16(UInt8(ascii: "`"))
        let dollar = UInt16(UInt8(ascii: "$"))
        let openBrace = UInt16(UInt8(ascii: "{"))
        let closeBrace = UInt16(UInt8(ascii: "}"))
        var inTemplate = false
        var interpolationDepth = 0
        var i = 0
        while i < column {
            if units[i] == backtick, interpolationDepth == 0 {
                inTemplate.toggle()
            } else if inTemplate, units[i] == dollar, i + 1 < units.count, units[i + 1] == openBrace {
                interpolationDepth = 1
                i += 1
            } else if interpolationDepth > 0, units[i] == openBrace {
                interpolationDepth += 1
            } else if interpolationDepth > 0, units[i] == closeBrace {
                interpolationDepth -= 1
            }
            i += 1
        }
        return (inTemplate, interpolationDepth > 0)
    }

    /// In PHP, `App\Models\Product` is one name: the link covers all of it.
    private func qualified(_ range: Range<Int>, in text: String, document: DocumentHandle) -> Range<Int> {
        Self.isPHP(document) ? Self.qualifiedNameRange(in: text, around: range) : range
    }

    /// `range` widened over the `\`-separated segments around it, and a
    /// leading `\`. Variables (`$x`) and names without a `\` stay as they are.
    static func qualifiedNameRange(in text: String, around range: Range<Int>) -> Range<Int> {
        let units = Array(text.utf16)
        let backslash = UInt16(UInt8(ascii: "\\"))
        func isWord(_ i: Int) -> Bool {
            guard i >= 0, i < units.count, let scalar = Unicode.Scalar(units[i]) else { return false }
            return scalar == "_" || CharacterSet.alphanumerics.contains(scalar)
        }
        guard range.lowerBound >= 0, range.upperBound <= units.count, !range.isEmpty,
              units[range.lowerBound] != UInt16(UInt8(ascii: "$")) else { return range }
        var start = range.lowerBound, end = range.upperBound
        while start > 0, units[start - 1] == backslash {
            start -= 1
            // A `\` with no name before it is the global-namespace prefix.
            guard isWord(start - 1) else { break }
            while isWord(start - 1) { start -= 1 }
        }
        while end < units.count, units[end] == backslash, isWord(end + 1) {
            end += 1
            while isWord(end) { end += 1 }
        }
        return start..<end
    }

    /// Where a click on the name in `range` is sent to the language engine:
    /// the start of its last segment, since that is the class or function
    /// the whole name refers to. Clicking `Models` in `App\Models\Product`
    /// opens `Product`, as in VS Code.
    static func lastSegmentStart(in text: String, range: Range<Int>) -> Int {
        let units = Array(text.utf16)
        let backslash = UInt16(UInt8(ascii: "\\"))
        guard range.lowerBound >= 0, range.upperBound <= units.count else { return range.lowerBound }
        var i = range.upperBound - 1
        while i >= range.lowerBound {
            if units[i] == backslash { return i + 1 }
            i -= 1
        }
        return range.lowerBound
    }

    /// Global helpers whose string argument the PHP engine can resolve to a
    /// file: Laravel (`config`, `route`, `view`, ...) and CodeIgniter 4
    /// (`route_to`, `url_to`, `lang`, `helper`, `model`, `service`) names.
    static let frameworkStringHelpers: Set<String> = [
        "config", "auth", "route", "to_route", "view", "env", "trans", "trans_choice", "__", "asset", "mix", "vite",
        "inertia", "base_path", "resource_path", "config_path", "app_path", "database_path", "lang_path",
        "public_path", "storage_path",
        "route_to", "url_to", "lang", "helper", "model", "service", "t",
    ]

    /// Facade and method names whose string argument the PHP engine (or
    /// Galaxy's own resolver, for `Log::channel`) can resolve, e.g.
    /// `Storage::disk('s3')`, `Config::get('app.name')`, `$lang->get('x')`,
    /// and Symfony's `generateUrl('route')`, `render('a.html.twig')`.
    /// Other calls such as `Log::info('message')` stay plain text.
    static let frameworkStringMethods: Set<String> = [
        "get", "getMany", "string", "integer", "boolean", "float", "array", "prepend", "push", "has",
        "hasForLocale", "choice", "disk", "fake", "persistentFake", "forgetDisk", "make", "view", "render",
        "modal", "inertia", "asset", "route", "channel", "middleware", "withoutMiddleware", "withoutmiddleware",
        "with", "load", "loadMissing", "has", "orHas", "doesntHave", "orDoesntHave", "whereHas", "orWhereHas",
        "whereDoesntHave", "withCount", "withSum", "withAvg", "withMin", "withMax", "withExists", "loadCount",
        "without", "withWhereRelation", "whereHasMorph", "whereRelation",
        "generateUrl", "redirectToRoute", "generate", "renderView", "renderForm", "renderBlock",
        "getParameter", "trans", "htmlTemplate", "textTemplate",
    ]

    static let eloquentRelationMethods: Set<String> = [
        "with", "load", "loadMissing", "has", "orHas", "doesntHave", "orDoesntHave", "whereHas", "orWhereHas",
        "whereDoesntHave", "withCount", "withSum", "withAvg", "withMin", "withMax", "withExists", "loadCount",
        "without", "withWhereRelation", "whereHasMorph", "whereRelation",
    ]

    /// The call a string literal is the first argument of: the function or
    /// method name, and for `A::m(` the class `A` (last namespace segment).
    struct StringCall: Equatable {
        var receiver: String?
        var isMethod: Bool
        var name: String
    }

    static func stringCall(in text: String, stringStart: Int) -> StringCall? {
        let units = Array(text.utf16)
        guard stringStart >= 0, stringStart <= units.count else { return nil }
        func isWord(_ i: Int) -> Bool {
            guard i >= 0, i < units.count, let scalar = Unicode.Scalar(units[i]) else { return false }
            return scalar == "_" || CharacterSet.alphanumerics.contains(scalar)
        }
        func word(endingAt end: Int) -> (String, Int) {
            var i = end
            while isWord(i) { i -= 1 }
            return (String(utf16CodeUnits: Array(units[(i + 1)...end]), count: end - i), i)
        }
        // Walk back over `(` and spaces to the callee name.
        var i = stringStart - 1
        while i >= 0, units[i] == 0x20 { i -= 1 }
        guard i >= 0, units[i] == UInt16(UInt8(ascii: "(")) else { return nil }
        i -= 1
        while i >= 0, units[i] == 0x20 { i -= 1 }
        guard isWord(i) else { return nil }
        let (name, before) = word(endingAt: i)
        i = before
        while i >= 0, units[i] == 0x20 { i -= 1 }
        let colon = UInt16(UInt8(ascii: ":")), arrow = UInt16(UInt8(ascii: ">")), dash = UInt16(UInt8(ascii: "-"))
        if i >= 1, units[i] == colon, units[i - 1] == colon {
            i -= 2
            while i >= 0, units[i] == 0x20 { i -= 1 }
            let receiver = isWord(i) ? word(endingAt: i).0 : nil
            return StringCall(receiver: receiver, isMethod: true, name: name)
        }
        if i >= 1, units[i] == arrow, units[i - 1] == dash {
            return StringCall(receiver: nil, isMethod: true, name: name)
        }
        return StringCall(receiver: nil, isMethod: false, name: name)
    }

    /// `true` for `Log::channel('x')`, which Galaxy resolves itself.
    static func isLogChannel(_ call: StringCall?) -> Bool {
        call?.receiver == "Log" && call?.name == "channel"
    }

    /// Channel name when `pos` is inside the string of `Log::channel('x')`.
    func logChannelName(at pos: TextPos) -> String? {
        guard let document, Self.isPHP(document) else { return nil }
        let column = Int(pos.colUtf16)
        // PHP highlights both `string` and its `string_content`; the widest
        // run is the whole literal, quotes included.
        let run = document.viewport(firstLine: pos.line, count: 1).styles
            .filter { $0.line == pos.line && $0.scope == "string" && Int($0.start) <= column && column < Int($0.end) }
            .max { ($0.end - $0.start) < ($1.end - $1.start) }
        guard let run else { return nil }
        return Self.logChannelName(in: document.displayLine(line: pos.line),
                                   stringStart: Int(run.start), stringEnd: Int(run.end))
    }

    static func logChannelName(in text: String, stringStart: Int, stringEnd: Int) -> String? {
        guard isLogChannel(stringCall(in: text, stringStart: stringStart)),
              let inner = frameworkStringRange(in: text, stringStart: stringStart, stringEnd: stringEnd) else { return nil }
        let units = Array(text.utf16)
        return String(utf16CodeUnits: Array(units[inner]), count: inner.count)
    }

    /// Whole string contents (without quotes) when the literal is the first
    /// argument of a Laravel helper such as `config('app.name')` or of a
    /// resolvable method such as `Storage::disk('s3')`. Any other string,
    /// e.g. a log message, is not a link.
    static func frameworkStringRange(in text: String, stringStart: Int, stringEnd: Int) -> Range<Int>? {
        let units = Array(text.utf16)
        guard stringStart >= 0, stringEnd <= units.count, stringEnd - stringStart >= 2,
              let call = stringCall(in: text, stringStart: stringStart) else {
            guard isRouteActionString(in: text, stringStart: stringStart)
                    || isEloquentRelationArrayString(in: text, stringStart: stringStart) else { return nil }
            return stringContentsRange(in: units, start: stringStart, end: stringEnd)
        }
        if call.isMethod {
            guard frameworkStringMethods.contains(call.name) else { return nil }
            // `channel` is only resolvable on the Log facade.
            if call.name == "channel", !isLogChannel(call) { return nil }
        } else {
            guard frameworkStringHelpers.contains(call.name) else { return nil }
        }
        return stringContentsRange(in: units, start: stringStart, end: stringEnd)
    }

    private static func stringContentsRange(in units: [UInt16], start: Int, end: Int) -> Range<Int>? {
        let first = units[start], last = units[end - 1]
        let quotes: Set<UInt16> = [UInt16(UInt8(ascii: "'")), UInt16(UInt8(ascii: "\""))]
        let inner = quotes.contains(first) && first == last ? (start + 1)..<(end - 1) : start..<end
        return inner.isEmpty ? nil : inner
    }

    /// Legacy Laravel route actions are the second argument to `Route::get`,
    /// unlike framework helper strings which are always the first argument.
    private static func isRouteActionString(in text: String, stringStart: Int) -> Bool {
        let units = Array(text.utf16)
        var i = stringStart - 1
        while i >= 0, units[i] == 0x20 { i -= 1 }
        guard i >= 0, units[i] == UInt16(UInt8(ascii: ",")) else { return false }
        i -= 1
        var depth = 0
        while i >= 0 {
            if units[i] == UInt16(UInt8(ascii: ")")) {
                depth += 1
            } else if units[i] == UInt16(UInt8(ascii: "(")) {
                if depth == 0 {
                    let call = stringCall(in: text, stringStart: i + 1)
                    return call?.receiver?.caseInsensitiveCompare("Route") == .orderedSame
                        && ["get", "post", "put", "patch", "delete", "match", "any"].contains(call?.name.lowercased() ?? "")
                }
                depth -= 1
            }
            i -= 1
        }
        return false
    }

    /// Eloquent accepts relation names in an array, e.g. `with(['author.posts'])`.
    private static func isEloquentRelationArrayString(in text: String, stringStart: Int) -> Bool {
        let units = Array(text.utf16)
        var i = stringStart - 1
        var brackets = 0
        while i >= 0 {
            if units[i] == UInt16(UInt8(ascii: "]")) {
                brackets += 1
            } else if units[i] == UInt16(UInt8(ascii: "[")) {
                if brackets == 0 { break }
                brackets -= 1
            }
            i -= 1
        }
        guard i >= 0 else { return false }
        i -= 1
        while i >= 0 {
            if units[i] == UInt16(UInt8(ascii: "(")) {
                guard let call = stringCall(in: text, stringStart: i + 1) else { return false }
                return call.isMethod && eloquentRelationMethods.contains(call.name)
            }
            i -= 1
        }
        return false
    }

    /// The dotted Eloquent relation segment under the pointer. The backend
    /// follows preceding segments to resolve the owner of this method.
    static func eloquentRelationSegmentRange(
        in text: String, stringStart: Int, stringEnd: Int, at column: Int
    ) -> Range<Int>? {
        let call = stringCall(in: text, stringStart: stringStart)
        guard (call?.isMethod == true && eloquentRelationMethods.contains(call!.name))
                || isEloquentRelationArrayString(in: text, stringStart: stringStart),
              let contents = stringContentsRange(in: Array(text.utf16), start: stringStart, end: stringEnd),
              contents.contains(column) else { return nil }
        let units = Array(text.utf16)
        var start = column
        while start > contents.lowerBound, units[start - 1] != UInt16(UInt8(ascii: ".")) { start -= 1 }
        var end = column
        while end < contents.upperBound, units[end] != UInt16(UInt8(ascii: ".")) { end += 1 }
        return start < end ? start..<end : nil
    }

    /// UTF-16 range of the identifier touching `column` (letters, digits,
    /// `_`, and a leading `$` for PHP variables). `nil` on punctuation.
    static func identifierRange(in text: String, at column: Int) -> Range<Int>? {
        let units = Array(text.utf16)
        func isWord(_ i: Int) -> Bool {
            guard i >= 0, i < units.count, let scalar = Unicode.Scalar(units[i]) else { return false }
            return scalar == "_" || CharacterSet.alphanumerics.contains(scalar)
        }
        // A caret slot sits between characters; prefer the one to its right.
        var start = isWord(column) ? column : (isWord(column - 1) ? column - 1 : -1)
        guard start >= 0 else { return nil }
        var end = start
        while isWord(start - 1) { start -= 1 }
        while isWord(end) { end += 1 }
        if start > 0, units[start - 1] == UInt16(UInt8(ascii: "$")) { start -= 1 }
        // Pure numbers are not navigable.
        let word = units[start..<end].compactMap(Unicode.Scalar.init)
        guard word.contains(where: { !CharacterSet.decimalDigits.contains($0) }) else { return nil }
        return start..<end
    }

    // MARK: Multi-cursor

    @objc func selectNextOccurrence(_ sender: Any?) {
        document?.selectNextOccurrence()
        didChangeSelection()
    }

    @objc func selectAllOccurrences(_ sender: Any?) {
        document?.selectAllOccurrences()
        didChangeSelection()
    }

    @objc func addCursorAbove(_ sender: Any?) {
        document?.addCursorVertical(above: true)
        didChangeSelection()
    }

    @objc func addCursorBelow(_ sender: Any?) {
        document?.addCursorVertical(above: false)
        didChangeSelection()
    }

    /// Scrolls `line` to the middle of the view if it is not on screen
    /// (Go to Line, back/forward), as VSCode does. A visible line stays put.
    func revealLine(_ line: Int, centered: Bool = false) {
        guard let scrollView = enclosingScrollView else { return }
        let clip = scrollView.contentView
        let height = metrics.lineHeight
        let top = rowTop(of: UInt64(line))
        let visible = clip.bounds
        if !centered, top >= visible.minY, top + height <= visible.maxY { return }
        let inset = scrollView.contentInsets.top
        let maxY = max(-inset, frame.height - visible.height)
        let y = min(max(top + height / 2 - visible.height / 2, -inset), maxY)
        clip.scroll(to: NSPoint(x: visible.minX, y: y))
        scrollView.reflectScrolledClipView(clip)
    }

    /// Selection-only change: redraw and keep the primary caret in view.
    func didChangeSelection() {
        noteDocumentRefreshFromInput()
        requestDocumentRefresh(scrollCaret: true)
        // VoiceOver reads the new caret line or selection.
        NSAccessibility.post(element: self, notification: .selectedTextChanged)
    }

    // MARK: Clipboard

    @objc func copy(_ sender: Any?) {
        guard let document else { return }
        write(document.copy())
    }

    @objc func cut(_ sender: Any?) {
        guard !isReadOnly, let document else { return }
        write(document.cut())
        onSnippetInvalidated?()
        didChangeDocument()
    }

    @objc func paste(_ sender: Any?) {
        guard !isReadOnly, let document, let text = pasteboard.string(forType: .string) else { return }
        // The flag only counts if the text is still what we copied.
        let linewise = pasteboard.string(forType: Self.linewiseType) == text
        document.pasteClipboard(text: text, linewise: linewise)
        onSnippetInvalidated?()
        didChangeDocument()
    }

    private func write(_ copied: ClipboardText) {
        guard !copied.text.isEmpty else { return }
        pasteboard.clearContents()
        pasteboard.setString(copied.text, forType: .string)
        if copied.linewise { pasteboard.setString(copied.text, forType: Self.linewiseType) }
    }
}

extension EditorView: NSMenuItemValidation {
    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(undoEdit(_:)): document?.canUndo() ?? false
        case #selector(redoEdit(_:)): document?.canRedo() ?? false
        case #selector(paste(_:)): document != nil && pasteboard.string(forType: .string) != nil
        case #selector(copy(_:)), #selector(cut(_:)), #selector(selectAll(_:)),
             #selector(selectNextOccurrence(_:)), #selector(selectAllOccurrences(_:)),
             #selector(addCursorAbove(_:)), #selector(addCursorBelow(_:)): document != nil
        default: true
        }
    }
}

// MARK: Accessibility (the basis of M1-17, and what UI tests read)

extension EditorView {
    /// Text above this size is not exposed, so a huge file cannot stall
    /// an assistive client that asks for the whole value.
    static let accessibilityTextLimit = 1_000_000

    override func accessibilityValue() -> Any? {
        guard let document else { return "" }
        let total = Int(document.lenUtf16())
        let length = min(total, Self.accessibilityTextLimit)
        return document.textInRange(range: Utf16Range(location: 0, length: UInt64(length)))
    }

    /// Matches `accessibilityValue`: clients index into that string, so a
    /// larger count would point past its end on huge files.
    override func accessibilityNumberOfCharacters() -> Int {
        min(Int(document?.lenUtf16() ?? 0), Self.accessibilityTextLimit)
    }

    override func accessibilitySelectedTextRange() -> NSRange {
        selectedRange()
    }

    override func accessibilitySelectedText() -> String? {
        guard let document else { return nil }
        let r = document.selectedRange()
        return document.textInRange(range: r)
    }

    override func accessibilityInsertionPointLineNumber() -> Int {
        guard let document else { return 0 }
        return Int(document.primaryCursor().line)
    }
}
