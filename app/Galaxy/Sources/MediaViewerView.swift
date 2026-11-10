import AppKit
import PDFKit

/// Files the editor shows as a picture instead of text. SVG is not here: it is
/// XML, and developers want to edit it, so it opens as text.
enum MediaKind: Equatable {
    case image
    case pdf

    static let imageExtensions: Set<String> = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "ico", "icns",
    ]

    static func kind(for url: URL) -> MediaKind? {
        let ext = url.pathExtension.lowercased()
        if ext == "pdf" { return .pdf }
        return imageExtensions.contains(ext) ? .image : nil
    }
}

/// Keeps a document smaller than the viewport in the middle of it.
@MainActor
private final class CenteringClipView: NSClipView {
    override func constrainBoundsRect(_ proposedBounds: NSRect) -> NSRect {
        var rect = super.constrainBoundsRect(proposedBounds)
        guard let documentView else { return rect }
        if rect.width > documentView.frame.width {
            rect.origin.x = (documentView.frame.width - rect.width) / 2
        }
        if rect.height > documentView.frame.height {
            rect.origin.y = (documentView.frame.height - rect.height) / 2
        }
        return rect
    }
}

/// Shows an image or a PDF in the editor area, for the media tabs. Images
/// zoom with pinch or ⌘-scroll and start fitted to the view; PDFs scroll by
/// page and fit the width.
@MainActor
final class MediaViewerView: NSView {
    private let imageScroll = NSScrollView()
    private let imageView = NSImageView()
    private let pdfView = PDFView()
    private let message = NSTextField(labelWithString: "")
    private var needsFit = false
    private var loadedStamp: Date?

    private(set) var url: URL?
    private(set) var kind: MediaKind?
    /// One line for the status bar: kind, size in pixels or pages, file size.
    private(set) var summary = ""

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true

        imageScroll.contentView = CenteringClipView()
        imageScroll.documentView = imageView
        imageScroll.hasVerticalScroller = true
        imageScroll.hasHorizontalScroller = true
        imageScroll.autohidesScrollers = true
        imageScroll.drawsBackground = false
        imageScroll.allowsMagnification = true
        imageScroll.minMagnification = 0.05
        imageScroll.maxMagnification = 32
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.isEditable = false

        pdfView.autoScales = true
        pdfView.displayMode = .singlePageContinuous
        pdfView.displaysPageBreaks = true

        message.font = .systemFont(ofSize: 13)
        message.textColor = .secondaryLabelColor
        message.alignment = .center
        message.lineBreakMode = .byTruncatingMiddle

        for view in [imageScroll, pdfView, message] as [NSView] {
            view.isHidden = true
            addSubview(view)
        }
        setAccessibilityIdentifier("media-viewer")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Read from disk each time: `URL.resourceValues` caches per URL instance
    /// and would hide a later change.
    private static func attributes(_ url: URL) -> [FileAttributeKey: Any] {
        (try? FileManager.default.attributesOfItem(atPath: url.path)) ?? [:]
    }

    private static func modified(_ url: URL) -> Date? {
        attributes(url)[.modificationDate] as? Date
    }

    /// The file changed on disk since it was loaded.
    func isStale(for url: URL) -> Bool {
        self.url != url || loadedStamp != Self.modified(url)
    }

    /// Loads `url`, replacing what was shown. A file that cannot be read shows
    /// a message in place of the picture.
    func show(url: URL) {
        self.url = url
        kind = MediaKind.kind(for: url)
        loadedStamp = Self.modified(url)
        imageScroll.isHidden = true
        pdfView.isHidden = true
        message.isHidden = true
        pdfView.document = nil
        imageView.image = nil
        needsFit = false
        let size = (Self.attributes(url)[.size] as? NSNumber).map {
            ByteCountFormatter.string(fromByteCount: $0.int64Value, countStyle: .file)
        }
        let suffix = size.map { ", \($0)" } ?? ""
        pdfView.backgroundColor = Theme.editorBackground.resolved(for: effectiveAppearance)

        switch kind {
        case .pdf?:
            if let document = PDFDocument(url: url) {
                pdfView.document = document
                pdfView.isHidden = false
                let pages = document.pageCount
                summary = "PDF, \(pages) \(pages == 1 ? "page" : "pages")\(suffix)"
            } else {
                fail(url)
            }
        case .image?:
            if let image = NSImage(contentsOf: url) {
                let pixels = image.representations
                    .map { NSSize(width: $0.pixelsWide, height: $0.pixelsHigh) }
                    .max { $0.width < $1.width }
                let shown = (pixels?.width ?? 0) > 0 ? pixels! : image.size
                imageView.image = image
                imageView.frame = NSRect(origin: .zero, size: image.size)
                imageScroll.magnification = 1
                imageScroll.isHidden = false
                needsFit = true
                summary = "\(url.pathExtension.uppercased()) image, \(Int(shown.width)) × \(Int(shown.height)) px\(suffix)"
            } else {
                fail(url)
            }
        case nil:
            fail(url)
        }
        needsLayout = true
    }

    private func fail(_ url: URL) {
        message.stringValue = "Cannot display \(url.lastPathComponent)"
        message.isHidden = false
        summary = message.stringValue
    }

    /// Moves keyboard focus to the picture, so arrows and paging keys scroll it.
    func focus() {
        window?.makeFirstResponder(kind == .pdf ? pdfView : imageScroll)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        pdfView.backgroundColor = Theme.editorBackground.resolved(for: effectiveAppearance)
        needsDisplay = true
    }

    override func layout() {
        super.layout()
        imageScroll.frame = bounds
        pdfView.frame = bounds
        message.frame = NSRect(x: 20, y: bounds.midY - 12, width: max(0, bounds.width - 40), height: 24)
        if needsFit, bounds.width > 0, bounds.height > 0, !imageScroll.isHidden { fitImage() }
    }

    /// A picture bigger than the view starts shrunk to fit; a smaller one
    /// stays at its own size.
    private func fitImage() {
        needsFit = false
        let size = imageView.frame.size
        let available = imageScroll.contentSize
        guard size.width > 0, size.height > 0, available.width > 0, available.height > 0 else { return }
        imageScroll.magnification = min(1, available.width / size.width, available.height / size.height)
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

/// The window's content view: takes files dropped from outside the app.
@MainActor
final class FileDropView: NSView {
    /// Called with the dropped files and folders, after the drop has ended.
    var onDrop: (([URL]) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        registerForDraggedTypes([.fileURL])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func fileURLs(in sender: NSDraggingInfo) -> [URL] {
        let options: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        return (sender.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: options) as? [URL]) ?? []
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        fileURLs(in: sender).isEmpty ? [] : .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        fileURLs(in: sender).isEmpty ? [] : .copy
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let urls = fileURLs(in: sender)
        guard !urls.isEmpty else { return false }
        // Let the drag session finish first: opening may show an alert.
        DispatchQueue.main.async { [weak self] in self?.onDrop?(urls) }
        return true
    }
}
