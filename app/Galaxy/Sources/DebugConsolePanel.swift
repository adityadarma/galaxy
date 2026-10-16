import AppKit

/// Bottom-panel tab for the debugger: a transcript with an input line for
/// evaluating expressions, and a pane with the call stack and locals.
@MainActor
final class DebugConsolePanel: NSView, NSTextFieldDelegate {
    private let transcript = NSTextView()
    private let state = NSTextView()
    private let input = NSTextField()
    private let maxCharacters = 200_000
    var onEvaluate: ((String) -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = Theme.sidebar.cgColor
        setAccessibilityRole(.group)
        setAccessibilityLabel("Debug Console")
        setAccessibilityIdentifier("debug-console-panel")

        let font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        for view in [transcript, state] {
            view.isEditable = false
            view.isSelectable = true
            view.font = font
            view.textColor = .secondaryLabelColor
            view.backgroundColor = .clear
            view.textContainerInset = NSSize(width: 10, height: 8)
            view.autoresizingMask = [.width]
        }
        transcript.setAccessibilityIdentifier("debug-console-transcript")
        state.setAccessibilityIdentifier("debug-console-state")

        func scroller(_ view: NSTextView) -> NSScrollView {
            let scroll = NSScrollView()
            scroll.drawsBackground = false
            scroll.hasVerticalScroller = true
            scroll.scrollerStyle = .overlay
            scroll.documentView = view
            scroll.translatesAutoresizingMaskIntoConstraints = false
            return scroll
        }
        let left = scroller(transcript), right = scroller(state)
        input.placeholderString = "Evaluate expression (Enter)"
        input.font = font
        input.isBordered = false
        input.drawsBackground = false
        input.focusRingType = .none
        input.delegate = self
        input.setAccessibilityIdentifier("debug-console-input")
        input.translatesAutoresizingMaskIntoConstraints = false
        let divider = NSBox()
        divider.boxType = .separator
        divider.translatesAutoresizingMaskIntoConstraints = false
        let vertical = NSBox()
        vertical.boxType = .separator
        vertical.translatesAutoresizingMaskIntoConstraints = false
        for view in [left, right, input, divider, vertical] as [NSView] { addSubview(view) }
        NSLayoutConstraint.activate([
            left.leadingAnchor.constraint(equalTo: leadingAnchor),
            left.topAnchor.constraint(equalTo: topAnchor),
            left.bottomAnchor.constraint(equalTo: divider.topAnchor),
            vertical.leadingAnchor.constraint(equalTo: left.trailingAnchor),
            vertical.topAnchor.constraint(equalTo: topAnchor),
            vertical.bottomAnchor.constraint(equalTo: bottomAnchor),
            right.leadingAnchor.constraint(equalTo: vertical.trailingAnchor),
            right.trailingAnchor.constraint(equalTo: trailingAnchor),
            right.topAnchor.constraint(equalTo: topAnchor),
            right.bottomAnchor.constraint(equalTo: bottomAnchor),
            right.widthAnchor.constraint(equalTo: widthAnchor, multiplier: 0.35),
            divider.leadingAnchor.constraint(equalTo: leadingAnchor),
            divider.trailingAnchor.constraint(equalTo: left.trailingAnchor),
            divider.bottomAnchor.constraint(equalTo: input.topAnchor, constant: -2),
            input.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            input.trailingAnchor.constraint(equalTo: left.trailingAnchor, constant: -10),
            input.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -6),
        ])
        showState(frames: [], variables: [], reason: nil)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    var transcriptText: String { transcript.string }
    var stateText: String { state.string }

    func append(_ text: String, category: String) {
        guard !text.isEmpty, let storage = transcript.textStorage else { return }
        let color: NSColor = category == "stderr" ? .systemRed : (category == "console" ? .tertiaryLabelColor : .labelColor)
        let atEnd = transcript.visibleRect.maxY >= transcript.bounds.maxY - 4
        storage.append(NSAttributedString(string: text, attributes: [.font: transcript.font as Any, .foregroundColor: color]))
        if storage.length > maxCharacters {
            storage.deleteCharacters(in: NSRange(location: 0, length: storage.length - maxCharacters))
        }
        if atEnd { transcript.scrollToEndOfDocument(nil) }
    }

    func clear() { transcript.string = "" }

    func showState(frames: [DebugFrameInfo], variables: [DebugVariableInfo], reason: String?) {
        var lines: [String] = []
        if let reason { lines.append("PAUSED: \(reason)") }
        lines.append("CALL STACK")
        if frames.isEmpty { lines.append("  (not paused)") }
        for frame in frames.prefix(20) {
            let file = frame.path.map { ($0 as NSString).lastPathComponent } ?? "?"
            lines.append("  \(frame.name)  \(file):\(frame.line)")
        }
        lines.append("")
        lines.append("VARIABLES")
        if variables.isEmpty { lines.append("  (none)") }
        for variable in variables.prefix(200) {
            lines.append("  \(variable.name) = \(variable.value)")
        }
        state.string = lines.joined(separator: "\n")
    }

    func submit() {
        let text = input.stringValue.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return }
        input.stringValue = ""
        append("> \(text)\n", category: "console")
        onEvaluate?(text)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        submit()
        return true
    }

    func focusInput() { window?.makeFirstResponder(input) }
}
