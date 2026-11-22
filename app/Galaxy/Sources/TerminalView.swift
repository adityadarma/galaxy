import AppKit

/// Draws a PTY session's screen and sends keys to it.
///
/// The Rust core keeps the grid; this view polls it at 60 Hz but only
/// copies it when `version()` changed. Each line arrives as styled runs
/// positioned by grid column, so colors, bold, wide characters and
/// full-screen programs (vim, htop) line up with the shell's idea of the
/// screen.
@MainActor
final class TerminalView: NSView {
    var onOpenLink: ((String, UInt64, UInt64) -> Void)?
    /// The shell exited; the controller drops the session.
    var onExit: (() -> Void)?
    /// Right-click menu (Move Terminal into Editor Area, …).
    var contextMenu: (() -> NSMenu?)?
    /// The view took keyboard focus (split terminals track the active one).
    var onFocus: (() -> Void)?
    /// A rule on the left edge, between split terminals.
    var showsLeadingDivider = false { didSet { needsDisplay = true } }

    override func menu(for event: NSEvent) -> NSMenu? {
        contextMenu?() ?? super.menu(for: event)
    }

    /// Pasteboard for copy and paste; tests swap in a private one.
    var pasteboard = NSPasteboard.general

    private var handle: TerminalHandle?
    private var timer: Timer?
    private(set) var screen: TerminalScreenInfo?
    private var lastSize: (cols: UInt16, rows: UInt16) = (0, 0)
    private var cursorVisible = true
    private var blinkTick = 0
    private var scrollRemainder: CGFloat = 0
    /// Button held while a mouse-tracking program has the mouse, for drag reports.
    private var heldMouseButton: Int?
    private var lastMouseCell: (column: Int, row: Int)?

    /// A grid position on the visible screen, 0-based.
    struct GridPoint: Equatable {
        var row: Int
        var column: Int
    }
    /// Text selected by dragging (programs without mouse tracking, or with
    /// Shift held). Cleared on typing, scrolling and a new click.
    private var selection: (anchor: GridPoint, head: GridPoint)? {
        didSet { needsDisplay = true }
    }
    private var isSelecting = false

    private var settings = TerminalSettings.current
    private var padding: NSEdgeInsets { settings.padding }

    private var font: NSFont {
        let size = max(10, min(24, UserDefaults.standard.object(forKey: "TerminalFontSize") as? CGFloat ?? 12))
        if let name = UserDefaults.standard.string(forKey: "TerminalFontName"), let font = NSFont(name: name, size: size) {
            return font
        }
        return Theme.editorFont.withSize(size)
    }
    private lazy var fonts = FontSet(font, lineHeight: settings.lineHeight)
    /// Core Text shaping is expensive for TUI screens with many repeated
    /// non-ASCII cells. Reuse shaped glyphs across frames, with a hard cap.
    private struct GlyphKey: Hashable {
        var text: String
        var font: NSFont
        var color: NSColor
        var strokeWidth: CGFloat?
    }
    private var glyphLines: [GlyphKey: CTLine] = [:]
    var cachedGlyphCount: Int { glyphLines.count }
    private static let glyphCacheLimit = 2048
    private var cellWidth: CGFloat { fonts.cellWidth }
    private var lineHeight: CGFloat { fonts.lineHeight }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        // macOS 14+ no longer clips by default: `draw` may get a dirty rect
        // beyond the bounds, and the background fill would paint over the
        // tab bar above an editor-area terminal.
        clipsToBounds = true
        setAccessibilityElement(true)
        setAccessibilityRole(.textArea)
        setAccessibilityIdentifier("terminal")
        setAccessibilityLabel("Terminal")
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    /// Last palette given to the core, so a redraw only sends a changed one.
    private var sentPalette: (ansi: [UInt32], foreground: UInt32, background: UInt32, cursor: UInt32)?

    /// Programs such as opencode ask the terminal for its colors (OSC 10/11)
    /// to choose a light or dark look. The core answers with these.
    private func sendPalette(_ palette: TerminalPalette) {
        guard let handle else { return }
        let values = palette.hexValues
        if let sent = sentPalette, sent.ansi == values.ansi, sent.foreground == values.foreground,
           sent.background == values.background, sent.cursor == values.cursor { return }
        handle.setColors(ansi: values.ansi, foreground: values.foreground,
                         background: values.background, cursor: values.cursor)
        sentPalette = values
    }

    func start(handle: TerminalHandle) {
        let changed = self.handle !== handle
        self.handle = handle
        if changed {
            screen = nil
            lastSize = (0, 0)
            sentPalette = nil
        }
        // Before the shell's first prompt, so its first color query is right.
        sendPalette(TerminalPalette(appearance: effectiveAppearance))
        timer?.invalidate()
        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            onAppKitMainThread { self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        resizeTerminal()
        poll()
        needsDisplay = true
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        handle = nil
        screen = nil
        lastSize = (0, 0)
        selection = nil
        needsDisplay = true
    }

    /// Follows the editor font settings when they change.
    func reloadFont() {
        settings = TerminalSettings.current
        cursorVisible = true
        blinkTick = 0
        fonts = FontSet(font, lineHeight: settings.lineHeight)
        glyphLines.removeAll()
        lastSize = (0, 0)
        resizeTerminal()
        needsDisplay = true
    }

    private func tick() {
        poll()
        // Blink at ~1.9 Hz while focused, like VS Code's default.
        guard settings.cursorBlink else { return }
        blinkTick += 1
        if blinkTick >= 32 {
            blinkTick = 0
            if isFocused {
                cursorVisible.toggle()
                setNeedsDisplayForCursor()
            }
        }
    }

    private func poll() {
        guard let handle else { return }
        _ = handle.pollOutput()
        // OSC 52: programs such as opencode, tmux and vim copy this way.
        if let copied = handle.takeClipboard(), !copied.isEmpty {
            pasteboard.clearContents()
            pasteboard.setString(copied, forType: .string)
        }
        if screen?.version != handle.version() {
            let next = handle.screen()
            if next.displayOffset != screen?.displayOffset { selection = nil }
            if next != screen {
                screen = next
                cursorVisible = true
                blinkTick = 0
                needsDisplay = true
            }
        }
        if !handle.isRunning() {
            timer?.invalidate()
            timer = nil
            onExit?()
        }
    }

    private var isFocused: Bool { window?.firstResponder === self && window?.isKeyWindow == true }

    private func setNeedsDisplayForCursor() {
        guard let cursor = screen?.cursor else { return }
        setNeedsDisplay(cellRect(row: Int(cursor.row), column: Int(cursor.column), width: 2).insetBy(dx: -2, dy: -2))
    }

    // MARK: Geometry

    private func cellRect(row: Int, column: Int, width: Int = 1) -> NSRect {
        NSRect(x: padding.left + CGFloat(column) * cellWidth,
               y: padding.top + CGFloat(row) * lineHeight,
               width: CGFloat(width) * cellWidth, height: lineHeight)
    }

    private func resizeTerminal() {
        guard let handle, bounds.width > 0, bounds.height > 0 else { return }
        let usableWidth = bounds.width - padding.left - padding.right
        let usableHeight = bounds.height - padding.top - padding.bottom
        let cols = UInt16(clamping: max(2, Int(usableWidth / cellWidth)))
        let rows = UInt16(clamping: max(1, Int(usableHeight / lineHeight)))
        guard (cols, rows) != lastSize else { return }
        lastSize = (cols, rows)
        try? handle.resize(cols: cols, rows: rows)
        poll()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        resizeTerminal()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        glyphLines.removeAll()
        needsDisplay = true
    }

    // MARK: Focus

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func becomeFirstResponder() -> Bool {
        cursorVisible = true
        needsDisplay = true
        onFocus?()
        return true
    }

    override func resignFirstResponder() -> Bool {
        needsDisplay = true
        return true
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .iBeam)
    }

    // MARK: Mouse

    /// Programs that enable mouse tracking (opencode, claude, vim, htop) get
    /// clicks, drags and the wheel as escape sequences. Shift or Cmd keeps
    /// the terminal's own handling (links, scrollback). Right-click always
    /// opens the context menu.
    private func forwardsMouse(_ event: NSEvent) -> Bool {
        guard let screen, screen.mouseClick || screen.mouseDrag || screen.mouseMotion else { return false }
        return event.modifierFlags.isDisjoint(with: [.shift, .command])
    }

    /// 1-based grid cell under the event, clamped to the screen.
    private func gridCell(for event: NSEvent) -> (column: Int, row: Int) {
        let point = convert(event.locationInWindow, from: nil)
        let columns = Int(screen?.columns ?? 1)
        let rows = screen?.lines.count ?? 1
        let column = Int(((point.x - padding.left) / cellWidth).rounded(.down))
        let row = Int(((point.y - padding.top) / lineHeight).rounded(.down))
        return (min(max(column, 0), columns - 1) + 1, min(max(row, 0), rows - 1) + 1)
    }

    /// Mouse report for xterm modes 1000-1006, or nil when it can't be encoded.
    /// `button` is 0 left, 1 middle, 2 right, 3 none (motion only), 64/65 wheel.
    static func mouseReport(button: Int, column: Int, row: Int, release: Bool = false,
                            motion: Bool = false, modifiers: NSEvent.ModifierFlags = [],
                            sgr: Bool) -> String? {
        var code = button
        if modifiers.contains(.shift) { code |= 4 }
        if modifiers.contains(.option) { code |= 8 }
        if modifiers.contains(.control) { code |= 16 }
        if motion { code |= 32 }
        if sgr {
            return "\u{1b}[<\(code);\(column);\(row)\(release ? "m" : "M")"
        }
        // Legacy X10 bytes must stay ASCII because the PTY input is UTF-8.
        let legacy = release ? (code & ~3) | 3 : code
        guard legacy + 32 < 128, column + 32 < 128, row + 32 < 128,
              let b = Unicode.Scalar(legacy + 32), let x = Unicode.Scalar(column + 32),
              let y = Unicode.Scalar(row + 32) else { return nil }
        return "\u{1b}[M\(b)\(x)\(y)"
    }

    private func sendMouse(button: Int, event: NSEvent, release: Bool = false, motion: Bool = false) {
        guard let handle, let screen else { return }
        let cell = gridCell(for: event)
        if motion, let last = lastMouseCell, last == cell { return }
        lastMouseCell = cell
        guard let report = Self.mouseReport(button: button, column: cell.column, row: cell.row,
                                            release: release, motion: motion,
                                            modifiers: event.modifierFlags, sgr: screen.sgrMouse) else { return }
        try? handle.write(input: report)
    }

    private func mouseButton(for event: NSEvent) -> Int {
        switch event.type {
        case .otherMouseDown, .otherMouseUp, .otherMouseDragged: return 1
        default: return 0
        }
    }

    private func reportPress(_ event: NSEvent) -> Bool {
        window?.makeFirstResponder(self)
        guard forwardsMouse(event) else { return false }
        let button = mouseButton(for: event)
        heldMouseButton = button
        lastMouseCell = nil
        sendMouse(button: button, event: event)
        return true
    }

    private func reportRelease(_ event: NSEvent) -> Bool {
        guard heldMouseButton != nil else { return false }
        heldMouseButton = nil
        guard forwardsMouse(event) else { return true }
        sendMouse(button: mouseButton(for: event), event: event, release: true)
        return true
    }

    private func reportDrag(_ event: NSEvent) {
        guard let held = heldMouseButton, forwardsMouse(event),
              screen?.mouseDrag == true || screen?.mouseMotion == true else { return }
        sendMouse(button: held, event: event, motion: true)
    }

    override func mouseDown(with event: NSEvent) {
        if reportPress(event) {
            // Track the same drag for Command-C without taking mouse input away
            // from full-screen programs such as opencode.
            let cell = selectionPoint(for: event)
            selection = (cell, cell)
            return
        }
        guard let screen else { return }
        let point = convert(event.locationInWindow, from: nil)
        let wantsLink = event.clickCount == 2 || event.modifierFlags.contains(.command)
        let row = Int((point.y - padding.top) / lineHeight)
        if wantsLink, screen.lines.indices.contains(row),
           let link = Self.fileLink(in: screen.lines[row].text) {
            onOpenLink?(link.path, link.line, link.column)
            return
        }
        let cell = selectionPoint(for: event)
        selection = (cell, cell)
        isSelecting = true
    }

    override func mouseUp(with event: NSEvent) {
        _ = reportRelease(event)
        isSelecting = false
        guard let selection else { return }
        if selection.anchor == selection.head {
            self.selection = nil
        } else {
            copy(nil)
            self.selection = nil
        }
    }

    override func mouseDragged(with event: NSEvent) {
        if isSelecting, let anchor = selection?.anchor {
            selection = (anchor, selectionPoint(for: event))
            return
        }
        if heldMouseButton != nil, forwardsMouse(event), let anchor = selection?.anchor {
            selection = (anchor, selectionPoint(for: event))
        }
        reportDrag(event)
    }

    private func selectionPoint(for event: NSEvent) -> GridPoint {
        let cell = gridCell(for: event)
        return GridPoint(row: cell.row - 1, column: cell.column - 1)
    }

    override func otherMouseDown(with event: NSEvent) {
        if !reportPress(event) { super.otherMouseDown(with: event) }
    }
    override func otherMouseUp(with event: NSEvent) {
        if !reportRelease(event) { super.otherMouseUp(with: event) }
    }
    override func otherMouseDragged(with event: NSEvent) { reportDrag(event) }

    override func mouseMoved(with event: NSEvent) {
        guard screen?.mouseMotion == true, forwardsMouse(event) else { return }
        sendMouse(button: 3, event: event, motion: true)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect],
                                       owner: self))
    }

    /// `path:line[:col]` in a line of output, e.g. compiler errors.
    static func fileLink(in text: String) -> (path: String, line: UInt64, column: UInt64)? {
        let pattern = #"(?:^|[\s(\[])((?:\.{0,2}/)?[\w.\-@]+(?:/[\w.\-@]+)*\.\w+):(\d+)(?::(\d+))?"#
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) else { return nil }
        let ns = text as NSString
        let path = ns.substring(with: match.range(at: 1))
        let line = UInt64(ns.substring(with: match.range(at: 2))) ?? 1
        let column = match.range(at: 3).location == NSNotFound ? 1 : UInt64(ns.substring(with: match.range(at: 3))) ?? 1
        return (path, line, column)
    }

    override func scrollWheel(with event: NSEvent) {
        guard let handle, let screen else { return }
        let delta = event.hasPreciseScrollingDeltas ? event.scrollingDeltaY / lineHeight : event.scrollingDeltaY * 3
        scrollRemainder += delta
        let lines = Int(scrollRemainder)
        guard lines != 0 else { return }
        scrollRemainder -= CGFloat(lines)
        let count = min(abs(lines), 20)
        let up = lines > 0

        if forwardsMouse(event) {
            // Wheel is button 64 (up) / 65 (down) in the mouse protocol.
            let cell = gridCell(for: event)
            guard let report = Self.mouseReport(button: up ? 64 : 65, column: cell.column, row: cell.row,
                                                modifiers: event.modifierFlags, sgr: screen.sgrMouse) else { return }
            try? handle.write(input: String(repeating: report, count: count))
            return
        }
        if screen.altScreen {
            // No mouse tracking: full-screen programs scroll by arrow keys
            // (xterm alternate scroll, mode 1007).
            guard screen.alternateScroll else { return }
            let key = up ? (screen.appCursor ? "\u{1b}OA" : "\u{1b}[A") : (screen.appCursor ? "\u{1b}OB" : "\u{1b}[B")
            try? handle.write(input: String(repeating: key, count: count))
            return
        }
        handle.scroll(lines: Int32(lines))
        poll()
    }

    // MARK: Edit menu

    /// Copies the dragged selection, including drags forwarded to full-screen
    /// programs, or the visible output if nothing was selected.
    @objc func copy(_ sender: Any?) {
        guard let screen else { return }
        let text = Self.textToCopy(in: screen.lines, selection: selection)
        guard !text.isEmpty else { return }
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    static func textToCopy(in lines: [TerminalLineInfo], selection: (anchor: GridPoint, head: GridPoint)?) -> String {
        if let selection {
            return selectedText(in: lines, from: selection.anchor, to: selection.head)
        }
        var visible = lines.map(\.text)
        while visible.last?.isEmpty == true { visible.removeLast() }
        return visible.joined(separator: "\n")
    }

    /// Text between two grid points (either order), cell by cell, so wide
    /// characters and gaps between runs keep their columns. Trailing blanks
    /// on each line are dropped.
    static func selectedText(in lines: [TerminalLineInfo], from a: GridPoint, to b: GridPoint) -> String {
        let (start, end) = (a.row, a.column) <= (b.row, b.column) ? (a, b) : (b, a)
        var rows: [String] = []
        for row in max(0, start.row)...max(0, min(end.row, lines.count - 1)) where lines.indices.contains(row) {
            let low = row == start.row ? start.column : 0
            let high = row == end.row ? end.column : Int.max
            var text = ""
            var column = 0
            func pad(to target: Int) {
                while column < target {
                    if column >= low && column <= high { text.append(" ") }
                    column += 1
                }
            }
            for run in lines[row].runs {
                pad(to: Int(run.column))
                for character in run.text {
                    if column >= low && column <= high { text.append(character) }
                    column += character.unicodeScalars.first.map(cellCount) ?? 1
                }
            }
            while text.last == " " { text.removeLast() }
            rows.append(text)
        }
        while rows.last?.isEmpty == true { rows.removeLast() }
        return rows.joined(separator: "\n")
    }

    @objc func paste(_ sender: Any?) {
        guard let handle, var text = Self.pasteText(from: pasteboard) else { return }
        text = text.replacingOccurrences(of: "\r\n", with: "\r").replacingOccurrences(of: "\n", with: "\r")
        if screen?.bracketedPaste == true { text = "\u{1b}[200~\(text)\u{1b}[201~" }
        try? handle.write(input: text)
    }

    /// Where pasted pictures are saved; the system clears its temporary folder.
    static let pastedImageDirectory = FileManager.default.temporaryDirectory
        .appendingPathComponent("GalaxyPastedImages", isDirectory: true)

    /// What a paste types: the paths of files copied in Finder, else the
    /// text, else a copied picture (a screenshot) saved as a PNG file, by
    /// path. Shell programs, AI command lines among them, take a file path
    /// where a terminal cannot send the picture itself.
    static func pasteText(from pasteboard: NSPasteboard, imageDirectory: URL = pastedImageDirectory) -> String? {
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self],
                                             options: [.urlReadingFileURLsOnly: true]) as? [URL], !urls.isEmpty {
            return urls.map { shellEscaped($0.path) }.joined(separator: " ")
        }
        if let text = pasteboard.string(forType: .string) { return text }
        let png: Data
        if let data = pasteboard.data(forType: .png) {
            png = data
        } else if let tiff = pasteboard.data(forType: .tiff), let rep = NSBitmapImageRep(data: tiff),
                  let data = rep.representation(using: .png, properties: [:]) {
            png = data
        } else {
            return nil
        }
        do {
            try FileManager.default.createDirectory(at: imageDirectory, withIntermediateDirectories: true)
            let url = imageDirectory.appendingPathComponent("pasted-\(Int(Date().timeIntervalSince1970 * 1000)).png")
            try png.write(to: url, options: .atomic)
            return shellEscaped(url.path)
        } catch {
            return nil
        }
    }

    /// Backslash-escapes a path the way Terminal.app pastes and drops files.
    static func shellEscaped(_ path: String) -> String {
        let plain = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "/._-+,:@%"))
        var out = ""
        for scalar in path.unicodeScalars {
            if !plain.contains(scalar) { out.append("\\") }
            out.unicodeScalars.append(scalar)
        }
        return out
    }

    @objc override func selectAll(_ sender: Any?) {
        guard let screen, !screen.lines.isEmpty else { return }
        selection = (GridPoint(row: 0, column: 0),
                     GridPoint(row: screen.lines.count - 1, column: Int(screen.columns) - 1))
    }

    /// Clears the screen and scrollback (⌘K in Terminal.app and VS Code).
    @objc func clearTerminal(_ sender: Any?) {
        try? handle?.write(input: "\u{0c}")
    }

    // MARK: Keys

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self else { return false }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if flags == .command, event.charactersIgnoringModifiers == "k" {
            clearTerminal(nil)
            return true
        }
        // ⌃ letters must reach the shell (⌃C, ⌃D…), not the menu.
        if flags.contains(.control), !flags.contains(.command) {
            keyDown(with: event)
            return true
        }
        return false
    }

    override func keyDown(with event: NSEvent) {
        guard let handle else { return }
        let bytes = Self.bytes(for: event, appCursor: screen?.appCursor ?? false)
        guard !bytes.isEmpty else { return }
        cursorVisible = true
        blinkTick = 0
        selection = nil
        try? handle.write(input: bytes)
        poll()
    }

    /// What a key sends, xterm style.
    static func bytes(for event: NSEvent, appCursor: Bool) -> String {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let option = flags.contains(.option)
        let control = flags.contains(.control)
        let shift = flags.contains(.shift)
        // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
        let modifier = 1 + (shift ? 1 : 0) + (option ? 2 : 0) + (control ? 4 : 0)
        func cursorKey(_ letter: String) -> String {
            if modifier > 1 { return "\u{1b}[1;\(modifier)\(letter)" }
            return (appCursor ? "\u{1b}O" : "\u{1b}[") + letter
        }
        func tildeKey(_ code: Int) -> String {
            modifier > 1 ? "\u{1b}[\(code);\(modifier)~" : "\u{1b}[\(code)~"
        }
        switch event.keyCode {
        case 36, 76:
            // Modern terminal UIs use CSI-u to distinguish a requested newline
            // from Enter, which submits the current prompt.
            if shift { return "\u{1b}[13;2u" }
            return option ? "\u{1b}\r" : "\r"
        case 48: return shift ? "\u{1b}[Z" : "\t"
        case 51: return option ? "\u{1b}\u{7f}" : (control ? "\u{08}" : "\u{7f}")
        case 53: return "\u{1b}"
        case 117: return tildeKey(3)
        case 123: return option && !shift && !control ? "\u{1b}b" : cursorKey("D")
        case 124: return option && !shift && !control ? "\u{1b}f" : cursorKey("C")
        case 125: return cursorKey("B")
        case 126: return cursorKey("A")
        case 115: return cursorKey("H")
        case 119: return cursorKey("F")
        case 116: return tildeKey(5)
        case 121: return tildeKey(6)
        case 122: return "\u{1b}OP"
        case 120: return "\u{1b}OQ"
        case 99: return "\u{1b}OR"
        case 118: return "\u{1b}OS"
        case 96: return tildeKey(15)
        case 97: return tildeKey(17)
        case 98: return tildeKey(18)
        case 100: return tildeKey(19)
        case 101: return tildeKey(20)
        case 109: return tildeKey(21)
        case 103: return tildeKey(23)
        case 111: return tildeKey(24)
        default: break
        }
        if control, let plain = event.charactersIgnoringModifiers?.lowercased().unicodeScalars.first {
            switch plain {
            case "a"..."z": return String(UnicodeScalar(plain.value - 96)!)
            case "@", " ", "2": return "\u{0}"
            case "[", "3": return "\u{1b}"
            case "\\", "4": return "\u{1c}"
            case "]", "5": return "\u{1d}"
            case "^", "6": return "\u{1e}"
            case "_", "-", "7": return "\u{1f}"
            default: break
            }
        }
        // Option sends the character macOS composes (∆ for ⌥J), like
        // Terminal.app without "Use Option as Meta key".
        return event.characters ?? ""
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        let palette = TerminalPalette(appearance: effectiveAppearance)
        sendPalette(palette)
        palette.background.setFill()
        dirtyRect.intersection(bounds).fill()
        if showsLeadingDivider {
            NSColor.separatorColor.setFill()
            NSRect(x: 0, y: 0, width: 1, height: bounds.height).intersection(dirtyRect).fill()
        }
        guard let screen, let context = NSGraphicsContext.current?.cgContext else { return }

        let firstRow = max(0, Int((dirtyRect.minY - padding.top) / lineHeight))
        let lastRow = min(screen.lines.count - 1, Int((dirtyRect.maxY - padding.top) / lineHeight))
        guard firstRow <= lastRow else { return drawScrollIndicator(screen, palette) }

        for row in firstRow...lastRow {
            let line = screen.lines[row]
            let y = padding.top + CGFloat(row) * lineHeight
            for run in line.runs {
                let colors = palette.colors(for: run)
                // Block and box characters are UI artwork, not text. Keep
                // their requested color instead of boosting faint dividers
                // (such as opencode's lower-half-block separator) to 4.5:1.
                let glyphColor = palette.colors(for: run, adjustContrast: false).foreground
                let width = CGFloat(run.text.unicodeScalars.reduce(0) { $0 + Self.cellCount($1) })
                let rect = NSRect(x: padding.left + CGFloat(run.column) * cellWidth, y: y,
                                  width: width * cellWidth, height: lineHeight)
                if let background = colors.background {
                    background.setFill()
                    rect.fill()
                }
                drawRun(run, in: rect, color: colors.foreground, glyphColor: glyphColor, context: context)
            }
        }
        drawSelection(screen)
        drawCursor(screen, palette)
        drawScrollIndicator(screen, palette)
    }

    private func drawSelection(_ screen: TerminalScreenInfo) {
        guard let selection else { return }
        let (start, end) = (selection.anchor.row, selection.anchor.column) <= (selection.head.row, selection.head.column)
            ? (selection.anchor, selection.head) : (selection.head, selection.anchor)
        NSColor.selectedTextBackgroundColor.withAlphaComponent(0.45).setFill()
        let last = Int(screen.columns) - 1
        for row in max(0, start.row)...min(end.row, screen.lines.count - 1) {
            let low = row == start.row ? start.column : 0
            let high = row == end.row ? end.column : last
            guard high >= low else { continue }
            cellRect(row: row, column: low, width: high - low + 1).fill()
        }
    }

    private func drawRun(_ run: TerminalRunInfo, in rect: NSRect, color: NSColor,
                         glyphColor: NSColor, context: CGContext) {
        guard !run.text.allSatisfy({ $0 == " " }) || run.underline || run.strikeout else { return }
        var attributes: [NSAttributedString.Key: Any] = [
            .font: fonts.font(bold: run.bold, italic: run.italic),
            .foregroundColor: color,
            .ligature: 0,
        ]
        if let stroke = fonts.syntheticBoldStrokeWidth(bold: run.bold, italic: run.italic) {
            attributes[.strokeWidth] = stroke
            attributes[.strokeColor] = color
        }
        // Place each cell at its grid column, so the shell's layout holds
        // even where the font's glyphs are not exactly one cell wide.
        let baseline = rect.minY + fonts.baselineOffset
        if run.text.unicodeScalars.allSatisfy({ $0.isASCII }) {
            let kern = cellWidth - fonts.asciiAdvance
            if abs(kern) > 0.01 { attributes[.kern] = kern }
            Self.drawText(run.text, attributes: attributes, x: rect.minX, baseline: baseline, context: context)
        } else {
            let runFont = fonts.font(bold: run.bold, italic: run.italic)
            let strokeWidth = fonts.syntheticBoldStrokeWidth(bold: run.bold, italic: run.italic)
            var column = 0
            for character in run.text {
                let cells = character.unicodeScalars.first.map(Self.cellCount) ?? 1
                let x = rect.minX + CGFloat(column) * cellWidth
                let cell = NSRect(x: x, y: rect.minY, width: CGFloat(cells) * cellWidth, height: lineHeight)
                if let scalar = character.unicodeScalars.first, character.unicodeScalars.count == 1,
                   let parts = Self.customGlyph(scalar, cell: cell) {
                    // Block and line characters fill the whole cell, so
                    // TUIs (opencode, htop) draw seamless bars and boxes.
                    for part in parts {
                        glyphColor.withAlphaComponent(glyphColor.alphaComponent * part.alpha).setFill()
                        part.rect.fill()
                    }
                } else if character != " " {
                    let line = glyphLine(for: String(character), attributes: attributes,
                                         font: runFont, color: color, strokeWidth: strokeWidth)
                    Self.drawLine(line, x: x, baseline: baseline, context: context)
                }
                column += cells
            }
        }
        // Drawn by hand: Core Text does not draw underlines.
        if run.underline {
            color.setFill()
            NSRect(x: rect.minX, y: baseline + max(1, round(-fonts.underlinePosition)), width: rect.width, height: 1).fill()
        }
        if run.strikeout {
            color.setFill()
            NSRect(x: rect.minX, y: round(baseline - fonts.xHeight / 2), width: rect.width, height: 1).fill()
        }
    }

    /// Rectangles that draw block elements (U+2580–U+259F) and the common
    /// box-drawing lines (U+2500–U+257F) edge to edge in `cell`, as VS
    /// Code's terminal does. Font glyphs for these stop short of the cell
    /// because lines are taller than the font, which left gaps between
    /// rows. `nil` for every other character. `cell` is in flipped
    /// coordinates (y grows down).
    static func customGlyph(_ scalar: Unicode.Scalar, cell: NSRect) -> [(rect: NSRect, alpha: CGFloat)]? {
        let v = scalar.value
        let x = cell.minX, y = cell.minY, w = cell.width, h = cell.height
        func r(_ fx: CGFloat, _ fy: CGFloat, _ fw: CGFloat, _ fh: CGFloat, _ alpha: CGFloat = 1) -> (rect: NSRect, alpha: CGFloat) {
            (NSRect(x: x + fx * w, y: y + fy * h, width: fw * w, height: fh * h), alpha)
        }
        switch v {
        case 0x2580: return [r(0, 0, 1, 0.5)]
        case 0x2581...0x2587:
            let f = CGFloat(v - 0x2580) / 8
            return [r(0, 1 - f, 1, f)]
        case 0x2588: return [r(0, 0, 1, 1)]
        case 0x2589...0x258F:
            let f = CGFloat(0x2590 - v) / 8
            return [r(0, 0, f, 1)]
        case 0x2590: return [r(0.5, 0, 0.5, 1)]
        case 0x2591: return [r(0, 0, 1, 1, 0.25)]
        case 0x2592: return [r(0, 0, 1, 1, 0.5)]
        case 0x2593: return [r(0, 0, 1, 1, 0.75)]
        case 0x2594: return [r(0, 0, 1, 1.0 / 8)]
        case 0x2595: return [r(7.0 / 8, 0, 1.0 / 8, 1)]
        case 0x2596...0x259F:
            // Quadrants: upper-left, upper-right, lower-left, lower-right.
            let masks: [UInt8] = [0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111]
            let mask = masks[Int(v - 0x2596)]
            var parts: [(rect: NSRect, alpha: CGFloat)] = []
            if mask & 0b1000 != 0 { parts.append(r(0, 0, 0.5, 0.5)) }
            if mask & 0b0100 != 0 { parts.append(r(0.5, 0, 0.5, 0.5)) }
            if mask & 0b0010 != 0 { parts.append(r(0, 0.5, 0.5, 0.5)) }
            if mask & 0b0001 != 0 { parts.append(r(0.5, 0.5, 0.5, 0.5)) }
            return parts
        default:
            break
        }
        // Box drawing: weight per arm (up, down, left, right); 1 light, 2 heavy.
        let arms: (UInt8, UInt8, UInt8, UInt8)
        switch v {
        case 0x2500: arms = (0, 0, 1, 1)
        case 0x2501: arms = (0, 0, 2, 2)
        case 0x2502: arms = (1, 1, 0, 0)
        case 0x2503: arms = (2, 2, 0, 0)
        case 0x250C, 0x256D: arms = (0, 1, 0, 1)
        case 0x250F: arms = (0, 2, 0, 2)
        case 0x2510, 0x256E: arms = (0, 1, 1, 0)
        case 0x2513: arms = (0, 2, 2, 0)
        case 0x2514, 0x2570: arms = (1, 0, 0, 1)
        case 0x2517: arms = (2, 0, 0, 2)
        case 0x2518, 0x256F: arms = (1, 0, 1, 0)
        case 0x251B: arms = (2, 0, 2, 0)
        case 0x251C: arms = (1, 1, 0, 1)
        case 0x2523: arms = (2, 2, 0, 2)
        case 0x2524: arms = (1, 1, 1, 0)
        case 0x252B: arms = (2, 2, 2, 0)
        case 0x252C: arms = (0, 1, 1, 1)
        case 0x2533: arms = (0, 2, 2, 2)
        case 0x2534: arms = (1, 0, 1, 1)
        case 0x253B: arms = (2, 0, 2, 2)
        case 0x253C: arms = (1, 1, 1, 1)
        case 0x254B: arms = (2, 2, 2, 2)
        case 0x2574: arms = (0, 0, 1, 0)
        case 0x2575: arms = (1, 0, 0, 0)
        case 0x2576: arms = (0, 0, 0, 1)
        case 0x2577: arms = (0, 1, 0, 0)
        case 0x2578: arms = (0, 0, 2, 0)
        case 0x2579: arms = (2, 0, 0, 0)
        case 0x257A: arms = (0, 0, 0, 2)
        case 0x257B: arms = (0, 2, 0, 0)
        default: return nil
        }
        let light = max(1, (w / 8).rounded())
        let heavy = max(2, (w / 4).rounded())
        func thickness(_ weight: UInt8) -> CGFloat { weight == 2 ? heavy : light }
        let cx = (cell.midX * 2).rounded() / 2
        let cy = (cell.midY * 2).rounded() / 2
        // Arms meet in the middle, overlapping by the crossing line's width.
        let vertical = thickness(max(arms.0, arms.1))
        let horizontal = thickness(max(arms.2, arms.3))
        var parts: [(rect: NSRect, alpha: CGFloat)] = []
        if arms.0 > 0 {
            let t = thickness(arms.0)
            parts.append((NSRect(x: cx - t / 2, y: y, width: t, height: cy - y + horizontal / 2), 1))
        }
        if arms.1 > 0 {
            let t = thickness(arms.1)
            parts.append((NSRect(x: cx - t / 2, y: cy - horizontal / 2, width: t, height: cell.maxY - cy + horizontal / 2), 1))
        }
        if arms.2 > 0 {
            let t = thickness(arms.2)
            parts.append((NSRect(x: x, y: cy - t / 2, width: cx - x + vertical / 2, height: t), 1))
        }
        if arms.3 > 0 {
            let t = thickness(arms.3)
            parts.append((NSRect(x: cx - vertical / 2, y: cy - t / 2, width: cell.maxX - cx + vertical / 2, height: t), 1))
        }
        return parts
    }

    /// Draws `text` with its baseline exactly at `baseline`. AppKit's
    /// string drawing adds the font's own line spacing on top, which put
    /// the glyphs low in their cells, under the prompt's colored blocks.
    static func drawText(_ text: String, attributes: [NSAttributedString.Key: Any], x: CGFloat, baseline: CGFloat,
                          context: CGContext) {
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
        drawLine(line, x: x, baseline: baseline, context: context)
    }

    func glyphLine(for text: String, attributes: [NSAttributedString.Key: Any],
                   font: NSFont, color: NSColor, strokeWidth: CGFloat?) -> CTLine {
        let key = GlyphKey(text: text, font: font, color: color, strokeWidth: strokeWidth)
        if let line = glyphLines[key] { return line }
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
        if glyphLines.count >= Self.glyphCacheLimit { glyphLines.removeAll(keepingCapacity: true) }
        glyphLines[key] = line
        return line
    }

    private static func drawLine(_ line: CTLine, x: CGFloat, baseline: CGFloat, context: CGContext) {
        context.saveGState()
        // The view is flipped; Core Text draws with y up.
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        context.textPosition = CGPoint(x: x, y: baseline)
        CTLineDraw(line, context)
        context.restoreGState()
    }

    private func drawCursor(_ screen: TerminalScreenInfo, _ palette: TerminalPalette) {
        guard let cursor = screen.cursor, screen.lines.indices.contains(Int(cursor.row)) else { return }
        let rect = cellRect(row: Int(cursor.row), column: Int(cursor.column))
        palette.cursor.set()
        if isFocused {
            guard cursorVisible || !settings.cursorBlink else { return }
            switch settings.cursorStyle {
            case .bar:
                NSRect(x: rect.minX, y: rect.minY, width: 2, height: rect.height).fill()
                return
            case .underline:
                NSRect(x: rect.minX, y: rect.maxY - 2, width: rect.width, height: 2).fill()
                return
            case .block:
                rect.fill()
            }
            // The character under a block cursor, in the background color.
            if let character = character(at: cursor, in: screen), character != " ",
               let context = NSGraphicsContext.current?.cgContext {
                Self.drawText(String(character), attributes: [
                    .font: fonts.font(bold: false, italic: false),
                    .foregroundColor: palette.background,
                ], x: rect.minX, baseline: rect.minY + fonts.baselineOffset, context: context)
            }
        } else {
            let outline = NSBezierPath(rect: rect.insetBy(dx: 0.5, dy: 0.5))
            outline.lineWidth = 1
            outline.stroke()
        }
    }

    private func character(at cursor: TerminalCursorInfo, in screen: TerminalScreenInfo) -> Character? {
        for run in screen.lines[Int(cursor.row)].runs {
            var column = Int(run.column)
            for character in run.text {
                let cells = character.unicodeScalars.first.map(Self.cellCount) ?? 1
                if (column..<(column + cells)).contains(Int(cursor.column)) { return character }
                column += cells
            }
        }
        return nil
    }

    /// A thin bar on the right while scrolled back, like VS Code's scrollbar.
    private func drawScrollIndicator(_ screen: TerminalScreenInfo, _ palette: TerminalPalette) {
        guard screen.displayOffset > 0, screen.historySize > 0 else { return }
        let total = CGFloat(screen.historySize) + CGFloat(screen.lines.count)
        let height = max(20, bounds.height * CGFloat(screen.lines.count) / total)
        let fromBottom = CGFloat(screen.displayOffset) / CGFloat(screen.historySize)
        let y = (bounds.height - height) * (1 - fromBottom)
        NSColor.secondaryLabelColor.withAlphaComponent(0.4).setFill()
        NSBezierPath(roundedRect: NSRect(x: bounds.width - 8, y: y, width: 5, height: height), xRadius: 2.5, yRadius: 2.5).fill()
    }

    /// Columns a character takes in the grid: 2 for East Asian wide and
    /// most emoji, as the core's grid lays them out.
    static func cellCount(_ scalar: Unicode.Scalar) -> Int {
        let v = scalar.value
        if v < 0x1100 { return 1 }
        if (0x1100...0x115F).contains(v) || (0x2E80...0xA4CF).contains(v) || (0xAC00...0xD7A3).contains(v)
            || (0xF900...0xFAFF).contains(v) || (0xFE30...0xFE4F).contains(v) || (0xFF00...0xFF60).contains(v)
            || (0xFFE0...0xFFE6).contains(v) || (0x1F300...0x1F64F).contains(v) || (0x1F900...0x1F9FF).contains(v)
            || (0x20000...0x3FFFD).contains(v) {
            return 2
        }
        return 1
    }
}

/// Terminal look settings from UserDefaults (the Settings window's Terminal tab).
struct TerminalSettings: Equatable {
    enum CursorStyle: String, CaseIterable {
        case block, bar, underline
    }

    static let lineHeightKey = "TerminalLineHeight"
    static let cursorStyleKey = "TerminalCursorStyle"
    static let cursorBlinkKey = "TerminalCursorBlink"
    static let paddingHorizontalKey = "TerminalPaddingHorizontal"
    static let paddingVerticalKey = "TerminalPaddingVertical"

    static let lineHeightRange: ClosedRange<Double> = 0.5...1.5
    static let paddingRange: ClosedRange<Double> = 0...40
    static let defaultPaddingHorizontal: Double = 20
    static let defaultPaddingVertical: Double = 4

    var lineHeight: CGFloat
    var cursorStyle: CursorStyle
    var cursorBlink: Bool
    var paddingHorizontal: CGFloat
    var paddingVertical: CGFloat

    /// Left gets the horizontal value and right 8 pt less, which keeps the
    /// old 20 / 12 default; the scroll indicator sits in the right margin.
    var padding: NSEdgeInsets {
        NSEdgeInsets(top: paddingVertical, left: paddingHorizontal,
                     bottom: paddingVertical, right: max(0, paddingHorizontal - 8))
    }

    static var current: TerminalSettings { load(from: .standard) }

    static func load(from defaults: UserDefaults) -> TerminalSettings {
        func clamp(_ key: String, _ range: ClosedRange<Double>, _ fallback: Double) -> CGFloat {
            CGFloat(min(max(defaults.object(forKey: key) as? Double ?? fallback, range.lowerBound), range.upperBound))
        }
        return TerminalSettings(
            lineHeight: clamp(lineHeightKey, lineHeightRange, 1.0),
            cursorStyle: defaults.string(forKey: cursorStyleKey).flatMap(CursorStyle.init) ?? .block,
            cursorBlink: defaults.object(forKey: cursorBlinkKey) as? Bool ?? true,
            paddingHorizontal: clamp(paddingHorizontalKey, paddingRange, defaultPaddingHorizontal),
            paddingVertical: clamp(paddingVerticalKey, paddingRange, defaultPaddingVertical)
        )
    }
}

/// The terminal font in its four styles, with the grid metrics.
struct FontSet {
    let regular: NSFont
    let bold: NSFont
    let italic: NSFont
    let boldItalic: NSFont
    let cellWidth: CGFloat
    let asciiAdvance: CGFloat
    let lineHeight: CGFloat
    let ascent: CGFloat
    let baselineOffset: CGFloat
    let underlinePosition: CGFloat
    let xHeight: CGFloat

    init(_ font: NSFont, lineHeight multiplier: CGFloat = 1) {
        underlinePosition = font.underlinePosition
        xHeight = font.xHeight
        let manager = NSFontManager.shared
        regular = font
        bold = manager.convert(font, toHaveTrait: .boldFontMask)
        italic = manager.convert(font, toHaveTrait: .italicFontMask)
        boldItalic = manager.convert(bold, toHaveTrait: .italicFontMask)
        asciiAdvance = ("M" as NSString).size(withAttributes: [.font: font]).width
        cellWidth = ceil(asciiAdvance * 2) / 2
        // Like VS Code's `terminal.integrated.lineHeight`: 1 makes rows as
        // tall as the font, so TUIs stack their rows without gaps.
        lineHeight = ceil((font.ascender - font.descender + font.leading) * multiplier)
        ascent = font.ascender
        // Center the glyphs (ascender to descender) in the taller line.
        let natural = font.ascender - font.descender
        baselineOffset = round((lineHeight - natural) / 2 + font.ascender)
    }

    func font(bold isBold: Bool, italic isItalic: Bool) -> NSFont {
        switch (isBold, isItalic) {
        case (true, true): boldItalic
        case (true, false): bold
        case (false, true): italic
        case (false, false): regular
        }
    }

    /// Core Text does not synthesize a bold face for fonts such as Monaco:
    /// AppKit returns the regular face for `convert(toHaveTrait:)`. A small
    /// negative stroke width fills and outlines bold cells without changing
    /// their point size, cell width, or baseline.
    func syntheticBoldStrokeWidth(bold isBold: Bool, italic isItalic: Bool) -> CGFloat? {
        guard isBold, !font(bold: isBold, italic: isItalic).fontDescriptor.symbolicTraits.contains(.bold) else {
            return nil
        }
        return -3
    }
}

/// ANSI colors close to VS Code's Dark Modern and Light Modern terminal
/// themes, so prompts look as they do there.
struct TerminalPalette {
    let background: NSColor
    let foreground: NSColor
    let cursor: NSColor
    let ansi: [NSColor]

    /// Background and default text follow the editor theme, so the terminal
    /// and the editor read as one surface.
    init(appearance: NSAppearance) {
        let dark = Theme.definition(for: appearance).isDark
        background = Theme.editorBackground.resolved(for: appearance)
        foreground = Theme.editorForeground.resolved(for: appearance)
        cursor = Theme.accent.resolved(for: appearance)
        if dark {
            ansi = ["#000000", "#CD3131", "#0DBC79", "#E5E510", "#2472C8", "#BC3FBC", "#11A8CD", "#E5E5E5",
                    "#666666", "#F14C4C", "#23D18B", "#F5F543", "#3B8EEA", "#D670D6", "#29B8DB", "#E5E5E5"]
                .map { NSColor(hex: $0) ?? .textColor }
        } else {
            // Tuned for the warm off-white editor. Prompts such as
            // powerlevel10k fill segments with these under black or white
            // text, so yellow and blue are segment colors first; as text,
            // `minimumContrast` darkens them. White (7) stays light for the
            // clock segment.
            ansi = ["#000000", "#B42318", "#3D9C41", "#DBB90F", "#2E6A9F", "#8E3FA8", "#0E7C86", "#D3D0CB",
                    "#6E6A64", "#C9252D", "#2E8B3A", "#B07800", "#3B74D1", "#9C4BBB", "#13848F", "#EAE8E4"]
                .map { NSColor(hex: $0) ?? .textColor }
        }
        minimumContrast = dark ? nil : 4.5
    }

    /// Text below this contrast with its cell is darkened or lightened until
    /// it reaches it, like VS Code's `terminal.integrated.minimumContrastRatio`.
    /// Prompts use bright 256-color greens meant for dark terminals.
    let minimumContrast: CGFloat?

    static func luminance(_ color: NSColor) -> CGFloat {
        let c = color.usingColorSpace(.sRGB) ?? color
        func linear(_ v: CGFloat) -> CGFloat { v <= 0.03928 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
        return 0.2126 * linear(c.redComponent) + 0.7152 * linear(c.greenComponent) + 0.0722 * linear(c.blueComponent)
    }

    static func contrast(_ a: NSColor, _ b: NSColor) -> CGFloat {
        let (x, y) = (luminance(a), luminance(b))
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }

    /// `color`, moved toward black on a light cell or white on a dark one in
    /// 10% steps until it has `ratio` contrast with `background`.
    static func readable(_ color: NSColor, on background: NSColor, ratio: CGFloat) -> NSColor {
        guard contrast(color, background) < ratio else { return color }
        let fg = color.usingColorSpace(.sRGB) ?? color
        let target: CGFloat = luminance(background) > 0.5 ? 0 : 1
        var adjusted = fg
        for step in 1...10 {
            let t = CGFloat(step) / 10
            func mix(_ v: CGFloat) -> CGFloat { v + (target - v) * t }
            adjusted = NSColor(srgbRed: mix(fg.redComponent), green: mix(fg.greenComponent),
                               blue: mix(fg.blueComponent), alpha: fg.alphaComponent)
            if contrast(adjusted, background) >= ratio { break }
        }
        return adjusted
    }

    /// `0xRRGGBB` of each color, the form the core answers color queries with.
    var hexValues: (ansi: [UInt32], foreground: UInt32, background: UInt32, cursor: UInt32) {
        func hex(_ color: NSColor) -> UInt32 {
            let c = color.usingColorSpace(.sRGB) ?? color
            func byte(_ v: CGFloat) -> UInt32 { UInt32((max(0, min(1, v)) * 255).rounded()) }
            return byte(c.redComponent) << 16 | byte(c.greenComponent) << 8 | byte(c.blueComponent)
        }
        return (ansi.map(hex), hex(foreground), hex(background), hex(cursor))
    }

    func color(_ color: TerminalColorInfo, isForeground: Bool) -> NSColor? {
        switch color {
        case .default: return isForeground ? foreground : nil
        case .indexed(let index): return indexed(index)
        case .rgb(let r, let g, let b):
            return NSColor(srgbRed: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: 1)
        }
    }

    func colors(for run: TerminalRunInfo, adjustContrast: Bool = true) -> (foreground: NSColor, background: NSColor?) {
        var fg = color(run.foreground, isForeground: true) ?? foreground
        var bg = color(run.background, isForeground: false)
        // Bold ANSI 0–7 use the bright variant, as xterm does.
        if run.bold, case .indexed(let index) = run.foreground, index < 8 { fg = ansi[Int(index) + 8] }
        if run.inverse {
            let swapped = bg ?? background
            bg = fg
            fg = swapped
        }
        if adjustContrast, let minimumContrast { fg = Self.readable(fg, on: bg ?? background, ratio: minimumContrast) }
        if run.dim { fg = fg.withAlphaComponent(0.6) }
        return (fg, bg)
    }

    private func indexed(_ index: UInt8) -> NSColor {
        let i = Int(index)
        if i < 16 { return ansi[i] }
        if i < 232 {
            let n = i - 16
            let levels: [CGFloat] = [0, 95, 135, 175, 215, 255]
            return NSColor(srgbRed: levels[n / 36] / 255, green: levels[(n / 6) % 6] / 255, blue: levels[n % 6] / 255, alpha: 1)
        }
        let gray = CGFloat(8 + (i - 232) * 10) / 255
        return NSColor(srgbRed: gray, green: gray, blue: gray, alpha: 1)
    }
}
