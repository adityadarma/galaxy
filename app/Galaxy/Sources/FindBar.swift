import AppKit

/// In-file find and replace bar (M1-13), shown above the editor.
///
/// The bar only collects input and shows results; `MainWindowController`
/// owns the search itself and wires the closures below to the document.
@MainActor
final class FindBar: NSView, NSTextFieldDelegate {
    static let findOnlyHeight: CGFloat = 36
    static let withReplaceHeight: CGFloat = 68

    let findField = NSTextField()
    let replaceField = NSTextField()
    let countLabel = NSTextField(labelWithString: "")
    let caseButton = FindBar.toggle("Aa", tip: "Match Case", id: "find-case")
    let wordButton = FindBar.toggle("ab", tip: "Match Whole Word", id: "find-word")
    let regexButton = FindBar.toggle(".*", tip: "Use Regular Expression", id: "find-regex")
    let previousButton = FindBar.icon("chevron.up", tip: "Previous Match (⇧⌘G)", id: "find-previous")
    let nextButton = FindBar.icon("chevron.down", tip: "Next Match (⌘G)", id: "find-next")
    let closeButton = FindBar.icon("xmark", tip: "Close (Esc)", id: "find-close")
    let replaceButton = NSButton(title: "Replace", target: nil, action: nil)
    let replaceAllButton = NSButton(title: "Replace All", target: nil, action: nil)
    private let replaceRow = NSStackView()

    var onPatternChange: (() -> Void)?
    var onNext: (() -> Void)?
    var onPrevious: (() -> Void)?
    var onSelectAll: (() -> Void)?
    var onReplace: (() -> Void)?
    var onReplaceAll: (() -> Void)?
    var onClose: (() -> Void)?

    var pattern: String { findField.stringValue }
    var replacement: String { replaceField.stringValue }
    var isReplaceVisible: Bool { !replaceRow.isHidden }

    var config: FindConfig {
        FindConfig(
            caseSensitive: caseButton.state == .on,
            wholeWord: wordButton.state == .on,
            regex: regexButton.state == .on
        )
    }

    /// Height the bar needs, so the window can reserve exactly that.
    var preferredHeight: CGFloat {
        isReplaceVisible ? Self.withReplaceHeight : Self.findOnlyHeight
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        setAccessibilityRole(.group)
        setAccessibilityLabel("Find")
        setAccessibilityIdentifier("find-bar")

        configure(findField, placeholder: "Find", id: "find-field")
        configure(replaceField, placeholder: "Replace", id: "replace-field")
        countLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        countLabel.textColor = .secondaryLabelColor
        countLabel.alignment = .right
        countLabel.setAccessibilityIdentifier("find-count")
        countLabel.setContentHuggingPriority(.required, for: .horizontal)
        countLabel.setContentCompressionResistancePriority(.required, for: .horizontal)
        countLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 70).isActive = true

        for button in [caseButton, wordButton, regexButton] {
            button.target = self
            button.action = #selector(optionToggled(_:))
        }
        previousButton.target = self
        previousButton.action = #selector(previousClicked(_:))
        nextButton.target = self
        nextButton.action = #selector(nextClicked(_:))
        closeButton.target = self
        closeButton.action = #selector(closeClicked(_:))
        for (button, id, action) in [
            (replaceButton, "replace-one", #selector(replaceClicked(_:))),
            (replaceAllButton, "replace-all", #selector(replaceAllClicked(_:))),
        ] {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.setAccessibilityIdentifier(id)
        }

        let findRow = NSStackView(views: [
            findField, caseButton, wordButton, regexButton, countLabel,
            previousButton, nextButton, closeButton,
        ])
        findRow.spacing = 4
        replaceRow.setViews([replaceField, replaceButton, replaceAllButton], in: .leading)
        replaceRow.spacing = 4
        replaceRow.isHidden = true

        let rows = NSStackView(views: [findRow, replaceRow])
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 4
        rows.translatesAutoresizingMaskIntoConstraints = false
        addSubview(rows)
        NSLayoutConstraint.activate([
            rows.topAnchor.constraint(equalTo: topAnchor, constant: 5),
            rows.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            rows.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            findRow.trailingAnchor.constraint(equalTo: rows.trailingAnchor),
            // Both fields line up, and the replace buttons sit under the toggles.
            replaceField.widthAnchor.constraint(equalTo: findField.widthAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.borderWidth = 0
    }

    // MARK: Showing and hiding

    func setReplaceVisible(_ visible: Bool) {
        replaceRow.isHidden = !visible
    }

    func focusFindField(selectAll: Bool = true) {
        window?.makeFirstResponder(findField)
        if selectAll { findField.selectText(nil) }
    }

    /// Sets the pattern as if the user typed it.
    func setPattern(_ text: String) {
        findField.stringValue = text
        onPatternChange?()
    }

    /// Shows the result of the last search next to the field.
    func show(status: FindStatus?) {
        guard let status, !pattern.isEmpty else {
            countLabel.stringValue = ""
            countLabel.textColor = .secondaryLabelColor
            findField.textColor = .textColor
            return
        }
        if let error = status.error {
            countLabel.stringValue = error
            countLabel.textColor = .systemRed
            findField.textColor = .systemRed
            return
        }
        findField.textColor = .textColor
        if !status.counted {
            // Big document: the count is running on a background thread.
            countLabel.stringValue = "Counting…"
            countLabel.textColor = .secondaryLabelColor
            return
        }
        countLabel.textColor = status.total == 0 ? .systemRed : .secondaryLabelColor
        let more = status.truncated ? "+" : ""
        if status.total == 0 {
            countLabel.stringValue = "No results"
        } else if let current = status.current {
            countLabel.stringValue = "\(current) of \(status.total)\(more)"
        } else {
            countLabel.stringValue = "\(status.total)\(more) found"
        }
    }

    // MARK: Actions

    @objc private func optionToggled(_ sender: Any?) { onPatternChange?() }
    @objc private func previousClicked(_ sender: Any?) { onPrevious?() }
    @objc private func nextClicked(_ sender: Any?) { onNext?() }
    @objc private func closeClicked(_ sender: Any?) { onClose?() }
    @objc private func replaceClicked(_ sender: Any?) { onReplace?() }
    @objc private func replaceAllClicked(_ sender: Any?) { onReplaceAll?() }

    // MARK: NSTextFieldDelegate

    func controlTextDidChange(_ notification: Notification) {
        if (notification.object as? NSTextField) === findField { onPatternChange?() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.cancelOperation(_:)):
            onClose?()
            return true
        case #selector(NSResponder.insertNewline(_:)):
            let flags = NSApp.currentEvent?.modifierFlags ?? []
            if control === replaceField {
                if flags.contains(.option) { onReplaceAll?() } else { onReplace?() }
            } else if flags.contains(.option) {
                onSelectAll?()
            } else if flags.contains(.shift) {
                onPrevious?()
            } else {
                onNext?()
            }
            return true
        default:
            return false
        }
    }

    // MARK: Building blocks

    private func configure(_ field: NSTextField, placeholder: String, id: String) {
        field.placeholderString = placeholder
        field.controlSize = .small
        field.font = .systemFont(ofSize: 12)
        field.bezelStyle = .roundedBezel
        field.usesSingleLineMode = true
        field.lineBreakMode = .byClipping
        field.delegate = self
        field.setAccessibilityIdentifier(id)
        field.setAccessibilityLabel(placeholder)
        field.widthAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true
    }

    private static func toggle(_ title: String, tip: String, id: String) -> NSButton {
        let button = NSButton(title: title, target: nil, action: nil)
        button.setButtonType(.pushOnPushOff)
        button.bezelStyle = .smallSquare
        button.controlSize = .small
        button.font = .systemFont(ofSize: 11, weight: .medium)
        button.toolTip = tip
        button.setAccessibilityLabel(tip)
        button.setAccessibilityIdentifier(id)
        button.widthAnchor.constraint(equalToConstant: 26).isActive = true
        return button
    }

    private static func icon(_ symbol: String, tip: String, id: String) -> NSButton {
        let button = NSButton(title: "", target: nil, action: nil)
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tip)
        button.isBordered = false
        button.imagePosition = .imageOnly
        button.contentTintColor = .secondaryLabelColor
        button.toolTip = tip
        button.setAccessibilityLabel(tip)
        button.setAccessibilityIdentifier(id)
        button.widthAnchor.constraint(equalToConstant: 22).isActive = true
        return button
    }
}
