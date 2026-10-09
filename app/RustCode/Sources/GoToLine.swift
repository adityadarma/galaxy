import AppKit

/// Go to Line (⌃G, M2-08): a small field over the top of the editor, styled
/// like Go to File. Accepts `42`, `42:7`, `42,7` and `-1` (last line);
/// Return jumps, Esc or a click elsewhere closes.
///
/// The panel only parses and reports. `MainWindowController` moves the
/// caret and records the jump in the back/forward history.
@MainActor
final class GoToLine: NSView, NSTextFieldDelegate {
    static let width: CGFloat = 420

    let field = NSTextField()
    let message = NSTextField(labelWithString: "")
    var onGo: ((LineTarget) -> Void)?
    var onClose: (() -> Void)?

    /// Lines in the document the panel was opened for.
    private(set) var lineCount: UInt64 = 1
    /// Parsed input, `nil` while it is not a line number.
    private(set) var target: LineTarget?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        shadow = NSShadow()
        layer?.cornerRadius = 8
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.28
        layer?.shadowRadius = 14
        layer?.shadowOffset = NSSize(width: 0, height: -4)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Go to Line")
        setAccessibilityIdentifier("go-to-line")

        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("go-to-line-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.lineBreakMode = .byTruncatingTail
        message.setAccessibilityIdentifier("go-to-line-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        addSubview(field)
        addSubview(message)
        NSLayoutConstraint.activate([
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            message.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 7),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            message.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -14),
            message.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -9),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    /// Clears the field for a document of `lineCount` lines; `current` is
    /// the caret's zero-based line, shown as a hint.
    func reset(lineCount: UInt64, current: UInt64) {
        self.lineCount = max(1, lineCount)
        field.stringValue = ""
        field.placeholderString = "Line \(current + 1) of \(self.lineCount). Type a line number, or line:column."
        update()
    }

    func focus() {
        window?.makeFirstResponder(field)
    }

    /// Types `text` as if the user did (tests).
    func setInput(_ text: String) {
        field.stringValue = text
        update()
    }

    private func update() {
        let text = field.stringValue
        target = parseLineTarget(input: text, lineCount: lineCount)
        if let t = target {
            message.stringValue = t.col.map { "Go to line \(t.line + 1), column \($0 + 1)" }
                ?? "Go to line \(t.line + 1)"
        } else if text.trimmingCharacters(in: .whitespaces).isEmpty {
            message.stringValue = "Lines 1 to \(lineCount). Negative numbers count from the end."
        } else {
            message.stringValue = "Not a line number. Type a number from 1 to \(lineCount)."
        }
        setAccessibilityValue(message.stringValue)
    }

    func go() {
        guard let target else { return NSSound.beep() }
        onGo?(target)
    }

    // MARK: Keys

    func controlTextDidChange(_ obj: Notification) { update() }

    func controlTextDidEndEditing(_ obj: Notification) {
        if window?.firstResponder !== field.currentEditor() { onClose?() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)): go()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }
}
