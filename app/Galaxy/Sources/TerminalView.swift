import AppKit
import CoreText

@MainActor
final class TerminalView: NSView {
    var onNewTab: (() -> Void)?
    var onSelectTab: ((Int) -> Void)?
    var onOpenLink: ((String, UInt64, UInt64) -> Void)?
    private var handle: TerminalHandle?
    private var timer: Timer?
    private var lines: [TerminalLineInfo] = []
    private let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    private let tabControl = NSSegmentedControl()
    private let newTabButton = NSButton(title: "+", target: nil, action: nil)
    private var cellWidth: CGFloat { ("0" as NSString).size(withAttributes: [.font: font]).width }
    private var lineHeight: CGFloat { ceil(font.pointSize * 1.35) }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        setAccessibilityElement(true)
        setAccessibilityRole(.textArea)
        setAccessibilityIdentifier("terminal")
        setAccessibilityLabel("Terminal")
        tabControl.segmentStyle = .texturedRounded
        tabControl.target = self
        tabControl.action = #selector(selectTab(_:))
        tabControl.translatesAutoresizingMaskIntoConstraints = false
        newTabButton.bezelStyle = .texturedRounded
        newTabButton.target = self
        newTabButton.action = #selector(addTab(_:))
        newTabButton.translatesAutoresizingMaskIntoConstraints = false
        addSubview(tabControl)
        addSubview(newTabButton)
        NSLayoutConstraint.activate([
            tabControl.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            tabControl.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            newTabButton.leadingAnchor.constraint(equalTo: tabControl.trailingAnchor, constant: 4),
            newTabButton.centerYAnchor.constraint(equalTo: tabControl.centerYAnchor),
            newTabButton.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -6),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func start(handle: TerminalHandle) {
        self.handle = handle
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.poll() }
        }
        resizeTerminal()
        poll()
        window?.makeFirstResponder(self)
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        handle = nil
        lines = []
        needsDisplay = true
    }

    func setTabs(count: Int, selected: Int) {
        tabControl.segmentCount = count
        for index in 0..<count { tabControl.setLabel("Terminal \(index + 1)", forSegment: index) }
        tabControl.selectedSegment = selected
    }

    @objc private func selectTab(_ sender: NSSegmentedControl) { onSelectTab?(sender.selectedSegment) }
    @objc private func addTab(_ sender: Any?) { onNewTab?() }

    private func poll() {
        guard let handle else { return }
        _ = handle.pollOutput()
        let next = handle.snapshot()
        if next != lines {
            lines = next
            needsDisplay = true
        }
    }

    private func resizeTerminal() {
        guard let handle else { return }
        let cols = UInt16(max(2, Int(bounds.width / cellWidth)))
        let rows = UInt16(max(1, Int(bounds.height / lineHeight)))
        try? handle.resize(cols: cols, rows: rows)
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        resizeTerminal()
    }

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        guard event.clickCount == 2 else { return }
        let point = convert(event.locationInWindow, from: nil)
        let row = Int((point.y - 4) / lineHeight)
        guard lines.indices.contains(row) else { return }
        let text = lines[row].text
        let pattern = #"(?:^|\s)([^\s:]+(?:/[^\s:]+)*):(\d+)(?::(\d+))?"#
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) else { return }
        let path = (text as NSString).substring(with: match.range(at: 1))
        let line = UInt64((text as NSString).substring(with: match.range(at: 2))) ?? 1
        let col = match.range(at: 3).location == NSNotFound
            ? 1
            : UInt64((text as NSString).substring(with: match.range(at: 3))) ?? 1
        onOpenLink?(path, line, col)
    }

    @objc func copy(_ sender: Any?) {
        let text = lines.map(\.text).joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    @objc func paste(_ sender: Any?) {
        guard let handle, let text = NSPasteboard.general.string(forType: .string) else { return }
        try? handle.write(input: text)
    }

    @objc override func selectAll(_ sender: Any?) { copy(sender) }

    override func keyDown(with event: NSEvent) {
        guard let handle else { return }
        let bytes: String
        switch event.keyCode {
        case 36: bytes = "\r"
        case 48: bytes = "\t"
        case 51: bytes = "\u{7f}"
        case 123: bytes = "\u{1b}[D"
        case 124: bytes = "\u{1b}[C"
        case 125: bytes = "\u{1b}[B"
        case 126: bytes = "\u{1b}[A"
        default: bytes = event.characters ?? ""
        }
        if !bytes.isEmpty { try? handle.write(input: bytes) }
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        NSColor.textBackgroundColor.setFill()
        dirtyRect.fill()
        guard !lines.isEmpty else { return }
        let context = NSGraphicsContext.current?.cgContext
        context?.saveGState()
        defer { context?.restoreGState() }
        for (row, line) in lines.enumerated() {
            let top = CGFloat(row) * lineHeight
            guard top <= dirtyRect.maxY, top + lineHeight >= dirtyRect.minY else { continue }
            let attributes: [NSAttributedString.Key: Any] = [
                .font: line.bold ? NSFont.monospacedSystemFont(ofSize: 12, weight: .bold) : font,
                .foregroundColor: terminalColor(line.foreground),
                .underlineStyle: line.underline ? NSUnderlineStyle.single.rawValue : 0,
            ]
            NSAttributedString(string: line.text, attributes: attributes)
                .draw(at: NSPoint(x: 6, y: top + 1))
        }
    }

    private func terminalColor(_ index: UInt8) -> NSColor {
        let palette: [NSColor] = [.black, .systemRed, .systemGreen, .systemYellow,
                                  .systemBlue, .systemPurple, .systemTeal, .white]
        return palette[Int(index) % palette.count]
    }
}
