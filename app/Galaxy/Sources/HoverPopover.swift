import AppKit

/// Hover card. Renders the engine's Markdown (code fences as monospaced
/// blocks, inline bold/italic/code), sizes itself to the content and scrolls
/// when the content is taller than the space it is given.
@MainActor
final class HoverPopover: NSView {
    static let maxWidth: CGFloat = 560
    static let maxHeight: CGFloat = 360
    static let padding = NSSize(width: 12, height: 8)

    private let scrollView = NSScrollView()
    /// TextKit 1 on purpose: a default `NSTextView()` uses TextKit 2, where
    /// the `textStorage`/`layoutManager` path used here silently stays empty.
    private let textView = NSTextView(usingTextLayoutManager: false)

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 6
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.shadowOpacity = 0.25
        layer?.shadowRadius = 8

        textView.isEditable = false
        textView.isSelectable = true
        textView.drawsBackground = false
        textView.textContainerInset = Self.padding
        textView.textContainer?.lineFragmentPadding = 0
        textView.textContainer?.widthTracksTextView = true
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]

        scrollView.documentView = textView
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.autoresizingMask = [.width, .height]
        addSubview(scrollView)

        setAccessibilityElement(true)
        setAccessibilityRole(.helpTag)
        setAccessibilityIdentifier("hover-popover")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Plain text currently shown (tests, accessibility).
    var renderedText: String { textView.string }
    var debugTextView: NSTextView? { textView }

    private var rendered = NSAttributedString()

    func show(_ markdown: String) {
        rendered = Self.render(markdown)
        textView.textStorage?.setAttributedString(rendered)
        textView.scroll(.zero)
        setAccessibilityValue(rendered.string)
    }

    /// Size that fits the content within `maxWidth` x `maxHeight`. Measured
    /// from the attributed string itself, so it works before the view has
    /// ever been laid out.
    func fittingSize(maxHeight: CGFloat = HoverPopover.maxHeight) -> NSSize {
        let options: NSString.DrawingOptions = [.usesLineFragmentOrigin, .usesFontLeading]
        let unbounded = NSSize(width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        let natural = ceil(rendered.boundingRect(with: unbounded, options: options).width)
        let textWidth = min(Self.maxWidth - Self.padding.width * 2, max(96, natural + 2))
        let textHeight = ceil(rendered.boundingRect(
            with: NSSize(width: textWidth, height: .greatestFiniteMagnitude), options: options).height)
        let width = textWidth + Self.padding.width * 2
        let height = textHeight + Self.padding.height * 2 + 2
        return NSSize(width: width, height: min(max(24, height), maxHeight))
    }

    override func layout() {
        super.layout()
        scrollView.frame = bounds
        let width = scrollView.contentSize.width
        textView.textContainer?.containerSize = NSSize(width: width - Self.padding.width * 2,
                                                       height: .greatestFiniteMagnitude)
        textView.frame = NSRect(x: 0, y: 0, width: width, height: max(textView.frame.height, scrollView.contentSize.height))
        textView.sizeToFit()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        needsLayout = true
    }

    override func updateLayer() { layer?.backgroundColor = Theme.sidebar.cgColor }

    // MARK: Markdown

    /// Converts hover Markdown into an attributed string. Fenced blocks
    /// become monospaced paragraphs without their language tag; prose goes
    /// through Foundation's inline Markdown parser.
    static func render(_ markdown: String) -> NSAttributedString {
        let body = NSFont.systemFont(ofSize: 12)
        let code = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        let out = NSMutableAttributedString()
        var inFence = false
        var prose: [String] = []
        var codeLines: [String] = []

        func appendSeparator() {
            if out.length > 0 { out.append(NSAttributedString(string: "\n\n", attributes: [.font: body])) }
        }
        func flushProse() {
            let text = prose.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            prose = []
            guard !text.isEmpty else { return }
            appendSeparator()
            out.append(inlineMarkdown(text, body: body, code: code))
        }
        func flushCode() {
            // Declarations may be prefixed with `<?php`; it adds nothing.
            let lines = codeLines.filter { $0.trimmingCharacters(in: .whitespaces) != "<?php" }
            codeLines = []
            let text = lines.joined(separator: "\n").trimmingCharacters(in: .newlines)
            guard !text.isEmpty else { return }
            appendSeparator()
            out.append(NSAttributedString(string: text, attributes: [.font: code, .foregroundColor: NSColor.labelColor]))
        }

        for line in markdown.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                if inFence { flushCode() } else { flushProse() }
                inFence.toggle()
            } else if inFence {
                codeLines.append(line)
            } else if line.trimmingCharacters(in: .whitespaces).allSatisfy({ $0 == "-" || $0 == "_" || $0 == "*" }),
                      line.trimmingCharacters(in: .whitespaces).count >= 3 {
                flushProse() // horizontal rule: just a paragraph break
            } else {
                prose.append(line)
            }
        }
        if inFence { flushCode() } else { flushProse() }
        return out
    }

    private static func inlineMarkdown(_ text: String, body: NSFont, code: NSFont) -> NSAttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        guard let parsed = try? AttributedString(markdown: text, options: options) else {
            return NSAttributedString(string: text, attributes: [.font: body, .foregroundColor: NSColor.labelColor])
        }
        let result = NSMutableAttributedString()
        for run in parsed.runs {
            let piece = String(parsed[run.range].characters)
            var font = body
            var color = NSColor.labelColor
            if let intent = run.inlinePresentationIntent {
                if intent.contains(.code) { font = code }
                var traits: NSFontDescriptor.SymbolicTraits = []
                if intent.contains(.stronglyEmphasized) { traits.insert(.bold) }
                if intent.contains(.emphasized) { traits.insert(.italic) }
                if !traits.isEmpty {
                    let descriptor = font.fontDescriptor.withSymbolicTraits(font.fontDescriptor.symbolicTraits.union(traits))
                    font = NSFont(descriptor: descriptor, size: font.pointSize) ?? font
                }
            }
            if run.link != nil { color = .linkColor }
            result.append(NSAttributedString(string: piece, attributes: [.font: font, .foregroundColor: color]))
        }
        return result
    }

    // MARK: Placement

    /// Frame for a card of `size` next to `anchor` (the hovered word), inside
    /// `bounds`. Both rects use non-flipped coordinates. Prefers above the
    /// word like VS Code, falls back to below, then to whichever side has more
    /// room (height shrinks to fit and the content scrolls).
    static func placement(size: NSSize, anchor: NSRect, in bounds: NSRect, gap: CGFloat = 4) -> NSRect {
        let above = bounds.maxY - (anchor.maxY + gap)
        let below = (anchor.minY - gap) - bounds.minY
        var height = size.height
        let y: CGFloat
        if above >= height {
            y = anchor.maxY + gap
        } else if below >= height {
            y = anchor.minY - gap - height
        } else if above >= below {
            height = max(24, above)
            y = anchor.maxY + gap
        } else {
            height = max(24, below)
            y = anchor.minY - gap - height
        }
        let width = min(size.width, bounds.width)
        let x = min(max(anchor.minX, bounds.minX), bounds.maxX - width)
        return NSRect(x: x, y: y, width: width, height: height)
    }
}
