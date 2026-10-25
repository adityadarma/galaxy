import AppKit

@MainActor
final class StatusBarGitButton: NSButton {
    override var stringValue: String {
        get { title }
        set { title = newValue }
    }
}

/// Bottom status bar: a message on the left, document facts (cursor,
/// indentation, line ending, encoding) on the right.
@MainActor
final class StatusBar: NSView {
    let message = NSTextField(labelWithString: "")
    let largeFileBadge = NSTextField(labelWithString: "Large file")
    let git = StatusBarGitButton(title: "", target: nil, action: nil)
    var onGitClick: (() -> Void)?
    /// Opens the Git Graph tab of the bottom panel; shown beside the branch.
    let gitGraph = NSButton()
    var onGitGraphClick: (() -> Void)?
    let workspaceScan = StatusBar.label("workspace-scan")
    private var scanRoot: String?
    let cursor = StatusBar.label("status-cursor")
    let language = StatusBar.label("status-language")
    let indent = StatusBar.label("status-indent")
    let lineEnding = StatusBar.label("status-eol")
    let encoding = StatusBar.label("status-encoding")

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Status bar")
        setAccessibilityIdentifier("status-bar")

        message.font = Theme.statusFont
        message.textColor = .secondaryLabelColor
        message.lineBreakMode = .byTruncatingTail
        message.setAccessibilityIdentifier("core-status")
        message.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        message.setContentHuggingPriority(.defaultLow, for: .horizontal)

        largeFileBadge.font = Theme.statusFont
        largeFileBadge.textColor = Theme.accent
        largeFileBadge.toolTip = "Over 20 MB: lines over 10,000 characters are shown cut off."
        largeFileBadge.setAccessibilityIdentifier("large-file-banner")
        largeFileBadge.isHidden = true

        git.isBordered = false
        git.bezelStyle = .inline
        git.font = Theme.statusFont
        git.contentTintColor = .secondaryLabelColor
        git.alignment = .left
        git.setAccessibilityIdentifier("status-git")
        git.target = self
        git.action = #selector(gitClicked(_:))

        gitGraph.image = NSImage(systemSymbolName: "point.3.connected.trianglepath.dotted",
                                 accessibilityDescription: "Git Graph")?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        gitGraph.title = "Git Graph"
        gitGraph.font = Theme.statusFont
        gitGraph.imagePosition = .imageLeading
        gitGraph.imageHugsTitle = true
        gitGraph.alignment = .left
        gitGraph.isBordered = false
        gitGraph.contentTintColor = .secondaryLabelColor
        gitGraph.toolTip = "Git Graph"
        gitGraph.setAccessibilityLabel("Git Graph")
        gitGraph.setAccessibilityIdentifier("status-git-graph")
        gitGraph.target = self
        gitGraph.action = #selector(gitGraphClicked(_:))
        gitGraph.isHidden = true

        workspaceScan.setAccessibilityLabel("Workspace scan status")
        workspaceScan.isHidden = true

        let left = NSStackView(views: [git, gitGraph, workspaceScan, message, largeFileBadge])
        left.spacing = 8
        let right = NSStackView(views: [cursor, language, indent, lineEnding, encoding])
        right.spacing = 14
        // The spacer takes the slack, so the facts sit on the right edge.
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        spacer.setContentCompressionResistancePriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [left, spacer, right])
        row.distribution = .fill
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        right.setContentCompressionResistancePriority(.required, for: .horizontal)
        addSubview(row)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Theme.statusHeight),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    // Own background, so the bar never shows through to whatever is behind
    // it (a window snapshot, or a transparent title bar later on).
    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        // Views do not clip to bounds by default since macOS 14, so the
        // dirty rect can reach past the bar and over the editor.
        dirtyRect.intersection(bounds).fill()
    }

    /// Shows the facts of `doc`, or clears them without a document.
    func show(document doc: DocumentHandle?, path: String? = nil) {
        guard let doc else {
            for label in [cursor, language, indent, lineEnding, encoding] { label.stringValue = "" }
            largeFileBadge.isHidden = true
            return
        }
        let settings = doc.indentSettings()
        language.stringValue = doc.syntaxLanguage() ?? Self.language(for: path)
        indent.stringValue = settings.useTabs ? "Tab Size: \(settings.tabSize)" : "Spaces: \(settings.tabSize)"
        lineEnding.stringValue = switch doc.lineEnding() {
        case .lf: "LF"
        case .crLf: "CRLF"
        case .cr: "CR"
        }
        let name = switch doc.encoding() {
        case .utf8: "UTF-8"
        case .utf16Le: "UTF-16 LE"
        case .utf16Be: "UTF-16 BE"
        }
        encoding.stringValue = doc.hasBom() ? "\(name) with BOM" : name
        largeFileBadge.isHidden = !doc.isLarge()
        showCursor(of: doc)
    }

    func showGit(branch: String?, changedCount: Int) {
        guard let branch else {
            git.title = ""
            git.stringValue = ""
            git.image = nil
            gitGraph.isHidden = true
            return
        }
        gitGraph.isHidden = false
        git.image = NSImage(systemSymbolName: "arrow.triangle.branch", accessibilityDescription: "Git branch")?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        git.imagePosition = .imageLeading
        git.imageHugsTitle = true
        git.imageScaling = .scaleNone
        git.contentTintColor = .secondaryLabelColor
        let text = branch
        git.title = text
        git.stringValue = text
        git.toolTip = branch
    }

    func resetWorkspaceScan(root: String) {
        scanRoot = root
        workspaceScan.stringValue = ""
        workspaceScan.isHidden = true
    }

    func workspaceIndexStarted(root: String, total: UInt64) {
        guard root == scanRoot else { return }
        workspaceScan.isHidden = false
        workspaceScan.stringValue = total == 0
            ? "Indexing files..."
            : "Indexing 0 of \(total) files"
    }

    func workspaceIndexProgress(root: String, completed: UInt64, total: UInt64) {
        guard root == scanRoot else { return }
        workspaceScan.isHidden = false
        workspaceScan.stringValue = "Indexing \(completed) of \(total) files"
    }

    func workspaceIndexFinished(root: String, total: UInt64, changed: UInt64) {
        guard root == scanRoot else { return }
        _ = changed
        workspaceScan.stringValue = ""
        workspaceScan.isHidden = true
    }

    @objc private func gitClicked(_ sender: Any?) { onGitClick?() }
    @objc private func gitGraphClicked(_ sender: Any?) { onGitGraphClick?() }

    /// Brighter while the Git Graph tab is showing.
    func setGitGraphVisible(_ visible: Bool) {
        gitGraph.contentTintColor = visible ? .labelColor : .secondaryLabelColor
    }

    /// Lightweight path detection for the status bar. Syntax parsers replace
    /// this in M3, but the label is useful before highlighting exists.
    static func language(for path: String?) -> String {
        guard let path, !path.isEmpty else { return "Plain Text" }
        let name = URL(fileURLWithPath: path).lastPathComponent.lowercased()
        let extensionMap = [
            "rs": "Rust", "swift": "Swift", "c": "C", "h": "C/C++ Header", "cc": "C++", "cpp": "C++", "cxx": "C++",
            "m": "Objective-C", "mm": "Objective-C++", "go": "Go", "py": "Python", "rb": "Ruby", "java": "Java",
            "kt": "Kotlin", "kts": "Kotlin", "cs": "C#", "php": "PHP", "sh": "Shell Script", "bash": "Shell Script",
            "zsh": "Shell Script", "fish": "Fish", "js": "JavaScript", "mjs": "JavaScript", "cjs": "JavaScript",
            "ts": "TypeScript", "tsx": "TypeScript React", "jsx": "JavaScript React", "html": "HTML", "htm": "HTML", "vue": "Vue", "svelte": "Svelte",
            "css": "CSS", "scss": "SCSS", "sass": "Sass", "less": "Less", "json": "JSON", "jsonc": "JSON with Comments",
            "toml": "TOML", "yaml": "YAML", "yml": "YAML", "xml": "XML", "md": "Markdown", "markdown": "Markdown",
            "txt": "Plain Text", "csv": "CSV", "sql": "SQL", "graphql": "GraphQL", "gql": "GraphQL", "lua": "Lua",
            "r": "R", "dart": "Dart", "ex": "Elixir", "exs": "Elixir", "erl": "Erlang", "hrl": "Erlang",
            "dockerfile": "Dockerfile", "makefile": "Makefile",
        ]
        if let language = extensionMap[(name as NSString).pathExtension] { return language }
        switch name {
        case "dockerfile": return "Dockerfile"
        case "makefile", "gnumakefile": return "Makefile"
        case "cargo.toml": return "TOML"
        case "package.json": return "JSON"
        case ".gitignore", ".gitattributes": return "Git Config"
        default: return "Plain Text"
        }
    }

    func showCursor(of doc: DocumentHandle) {
        let snap = doc.statusSnapshot()
        showCursor(primary: snap.primary, count: snap.selectionCount)
    }

    /// "Ln 3, Col 7", plus the cursor count when there are several.
    func showCursor(primary: CursorInfo, count: UInt64) {
        var text = "Ln \(primary.line + 1), Col \(primary.col + 1)"
        if count > 1 { text += " (\(count) cursors)" }
        cursor.stringValue = text
    }

    private static func label(_ id: String) -> NSTextField {
        let label = NSTextField(labelWithString: "")
        label.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        label.textColor = .secondaryLabelColor
        label.setAccessibilityIdentifier(id)
        return label
    }
}
