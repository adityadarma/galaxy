import AppKit
import WebKit

/// Rendered Markdown pane (VS Code "Open Preview to the Side").
///
/// The shell page loads once; later updates swap the content `div` through
/// `evaluateJavaScript`, so scroll position survives edits. Page JavaScript is
/// off: HTML from the core is already escaped, and this keeps it that way even
/// if that ever regresses.
/// Reports wheel and trackpad scrolling, so the editor can follow the preview.
final class PreviewWebView: WKWebView {
    var onScrollWheel: (() -> Void)?

    override func scrollWheel(with event: NSEvent) {
        super.scrollWheel(with: event)
        onScrollWheel?()
    }
}

@MainActor
final class MarkdownPreviewView: NSView, WKNavigationDelegate {
    let webView: PreviewWebView
    private var isShellLoaded = false
    private var pendingHTML: String?
    private var lastHTML: String?
    /// Source line (fractional) at the top of the editor; applied after every
    /// content update, which resets nothing but changes the page height.
    private var topLine: Double = 0
    /// Called with the source line at the top of the preview after the user
    /// scrolls it.
    var onUserScroll: ((Double) -> Void)?

    /// File extensions treated as Markdown.
    static let extensions: Set<String> = ["md", "markdown", "mdown", "mkd", "mdx"]

    static func isMarkdown(path: String) -> Bool {
        extensions.contains((path as NSString).pathExtension.lowercased())
    }

    override init(frame: NSRect) {
        let config = WKWebViewConfiguration()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        webView = PreviewWebView(frame: .zero, configuration: config)
        super.init(frame: frame)
        webView.onScrollWheel = { [weak self] in self?.reportUserScroll() }
        webView.navigationDelegate = self
        webView.translatesAutoresizingMaskIntoConstraints = false
        webView.setValue(false, forKey: "drawsBackground")
        addSubview(webView)
        NSLayoutConstraint.activate([
            webView.topAnchor.constraint(equalTo: topAnchor),
            webView.bottomAnchor.constraint(equalTo: bottomAnchor),
            webView.leadingAnchor.constraint(equalTo: leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
        setAccessibilityLabel("Markdown Preview")
        webView.loadHTMLString(Self.shell, baseURL: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Renders `markdown`. Skips work when the HTML is unchanged.
    func show(markdown: String) {
        let html = markdownToHtml(source: markdown)
        guard html != lastHTML else { return }
        lastHTML = html
        guard isShellLoaded else {
            pendingHTML = html
            return
        }
        apply(html)
    }

    private func apply(_ html: String) {
        guard let data = try? JSONEncoder().encode(html),
              let literal = String(data: data, encoding: .utf8) else { return }
        webView.evaluateJavaScript("document.getElementById('c').innerHTML = \(literal);") { [weak self] _, _ in
            guard let self else { return }
            self.scroll(toSourceLine: self.topLine)
        }
    }

    // MARK: Scroll sync

    /// Scrolls so that `line` (0-based, fractional) of the source is at the top.
    /// Position between two blocks is interpolated from their markers.
    func scroll(toSourceLine line: Double) {
        topLine = line
        guard isShellLoaded else { return }
        webView.evaluateJavaScript("""
        (function(l){var a=document.querySelectorAll('.src');if(!a.length)return;
        var py=0,pl=0,ny=null,nl=0;
        for(var i=0;i<a.length;i++){var d=+a[i].getAttribute('data-line');
        var y=a[i].getBoundingClientRect().top+window.scrollY;
        if(d<=l){py=y;pl=d;}else{ny=y;nl=d;break;}}
        var t=(ny===null||nl<=pl)?py:py+(ny-py)*(l-pl)/(nl-pl);
        window.scrollTo(0,t);})(\(line))
        """)
    }

    /// The source line at the top of the preview, or `nil` before it has content.
    func sourceLineAtTop(_ completion: @escaping (Double?) -> Void) {
        guard isShellLoaded else { return completion(nil) }
        webView.evaluateJavaScript("""
        (function(){var a=document.querySelectorAll('.src');if(!a.length)return null;
        var s=window.scrollY,py=0,pl=0,ny=null,nl=0;
        for(var i=0;i<a.length;i++){var d=+a[i].getAttribute('data-line');
        var y=a[i].getBoundingClientRect().top+window.scrollY;
        if(y<=s){py=y;pl=d;}else{ny=y;nl=d;break;}}
        if(ny===null||ny<=py)return pl;
        return pl+(nl-pl)*(s-py)/(ny-py);})()
        """) { result, _ in
            completion((result as? NSNumber)?.doubleValue)
        }
    }

    private func reportUserScroll() {
        sourceLineAtTop { [weak self] line in
            guard let self, let line else { return }
            self.topLine = line
            self.onUserScroll?(line)
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        isShellLoaded = true
        if let html = pendingHTML {
            pendingHTML = nil
            apply(html)
        }
    }

    /// Links open in the default browser; the pane itself never navigates.
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        guard action.navigationType == .linkActivated else {
            decisionHandler(.allow)
            return
        }
        if let url = action.request.url, ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") {
            NSWorkspace.shared.open(url)
        }
        decisionHandler(.cancel)
    }

    private static let shell = """
    <!doctype html>
    <html><head><meta charset="utf-8">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src https: data:">
    <style>
    :root { color-scheme: light dark; }
    body { font: 14px/1.6 -apple-system, system-ui, sans-serif; color: CanvasText;
           margin: 0; padding: 16px 28px 48px; max-width: 880px; word-wrap: break-word; }
    h1, h2 { border-bottom: 1px solid rgba(128,128,128,.35); padding-bottom: .3em; }
    h1, h2, h3, h4, h5, h6 { margin: 1.4em 0 .6em; line-height: 1.25; }
    .src { display: block; height: 0; margin: 0; padding: 0; }
    .src:first-child + * { margin-top: 0; }
    a { color: LinkText; }
    code, pre { font: 12.5px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
    code { background: rgba(128,128,128,.18); padding: .15em .35em; border-radius: 4px; }
    pre { background: rgba(128,128,128,.14); padding: 12px 14px; border-radius: 6px; overflow-x: auto; }
    pre code { background: none; padding: 0; }
    blockquote { margin: 0; padding: 0 1em; border-left: 4px solid rgba(128,128,128,.45); opacity: .85; }
    table { border-collapse: collapse; }
    th, td { border: 1px solid rgba(128,128,128,.4); padding: 6px 12px; }
    img { max-width: 100%; }
    hr { border: 0; border-top: 1px solid rgba(128,128,128,.35); }
    </style></head>
    <body><div id="c"></div></body></html>
    """
}
