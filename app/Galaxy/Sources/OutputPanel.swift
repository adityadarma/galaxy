import AppKit

@MainActor
final class OutputPanel: NSView {
    private let textView = NSTextView()
    private var timer: Timer?
    private let maxCharacters = 200_000

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = Theme.sidebar.cgColor
        setAccessibilityRole(.textArea)
        setAccessibilityLabel("Output")
        setAccessibilityIdentifier("output-panel")

        textView.isEditable = false
        textView.isSelectable = true
        textView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        textView.textColor = .secondaryLabelColor
        textView.backgroundColor = .clear
        textView.textContainerInset = NSSize(width: 10, height: 8)
        textView.autoresizingMask = [.width]

        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.documentView = textView
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func start() {
        timer?.invalidate()
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.refresh() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    private func refresh() {
        guard let url = newestLogURL(), let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .utf8) else { return }
        let suffix = String(text.suffix(maxCharacters))
        guard suffix != textView.string else { return }
        textView.string = suffix
        textView.scrollToEndOfDocument(nil)
    }

    private func newestLogURL() -> URL? {
        let directory = Log.directory
        let files = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        )) ?? []
        return files
            .filter { $0.lastPathComponent.hasPrefix("core.log") }
            .max { lhs, rhs in
                let left = (try? lhs.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                let right = (try? rhs.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                return left < right
            }
    }
}
