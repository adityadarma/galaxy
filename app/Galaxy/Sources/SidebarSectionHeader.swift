import AppKit
import os

private extension NSLayoutConstraint {
    func withPriority(_ priority: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}

/// Sidebar header, as in VSCode with one folder open: the folder name in
/// caps (EXPLORER before a folder is open) and, on the right, New File,
/// New Folder, Refresh, Collapse Folders and a "…" menu. Same height as the
/// tab strip, with a rule underneath.
@MainActor
final class SidebarSectionHeader: NSView {
    enum Mode {
        case explorer
        case sourceControl
        case search
    }

    static var height: CGFloat { Theme.tabHeight }

    var mode: Mode = .explorer {
        didSet { updateMode() }
    }

    let label = NSTextField(labelWithString: "EXPLORER")

    // Explorer tools
    let newFileButton = SidebarSectionHeader.tool("doc.badge.plus", "New File…")
    let newFolderButton = SidebarSectionHeader.tool("folder.badge.plus", "New Folder…")
    let refreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh Explorer")
    let collapseButton = SidebarSectionHeader.tool("rectangle.stack.badge.minus", "Collapse Folders in Explorer")
    let moreButton = SidebarSectionHeader.tool("ellipsis", "More Actions…")
    var tools: [NSButton] { [newFileButton, newFolderButton, refreshButton, collapseButton, moreButton] }

    // Source Control tools
    let scmPullButton = SidebarSectionHeader.tool("arrow.down.to.line", "Pull")
    let scmBranchButton = SidebarSectionHeader.tool("arrow.triangle.branch", "Branches")
    let scmCommitButton = SidebarSectionHeader.tool("checkmark", "Commit")
    let scmRefreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh")
    let scmMoreButton = SidebarSectionHeader.tool("ellipsis", "More Actions…")
    var scmTools: [NSButton] {
        [scmPullButton, scmBranchButton, scmCommitButton, scmRefreshButton, scmMoreButton]
    }

    let searchRefreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh Search")
    let searchClearButton = SidebarSectionHeader.tool("line.3.horizontal.decrease.circle", "Clear Search")
    let searchReplaceButton = SidebarSectionHeader.tool("doc.badge.plus", "Toggle Replace")
    let searchFilterButton = SidebarSectionHeader.tool("line.3.horizontal.decrease", "Search Filters")
    let searchMoreButton = SidebarSectionHeader.tool("ellipsis", "More Search Actions…")
    var searchTools: [NSButton] { [searchRefreshButton, searchClearButton, searchReplaceButton, searchFilterButton, searchMoreButton] }

    var onNewFile: (() -> Void)?
    var onNewFolder: (() -> Void)?
    var onRefresh: (() -> Void)?
    var onCollapseAll: (() -> Void)?
    var moreMenu: (() -> NSMenu)?

    var onScmPull: (() -> Void)?
    var onScmBranch: (() -> Void)?
    var onScmCommit: (() -> Void)?
    var onScmRefresh: (() -> Void)?
    var scmMoreMenu: (() -> NSMenu)?
    var onSearchRefresh: (() -> Void)?
    var onSearchClear: (() -> Void)?
    var onSearchToggleReplace: (() -> Void)?
    var onSearchToggleDetails: (() -> Void)?
    var searchMoreMenu: (() -> NSMenu)?

    /// Folder name; empty shows EXPLORER.
    var title = "" {
        didSet {
            if mode == .explorer {
                label.stringValue = title.isEmpty ? "EXPLORER" : title.uppercased()
            }
            setAccessibilityLabel(title.isEmpty ? "Explorer" : title)
        }
    }

    /// The buttons only make sense with a folder open.
    var showsTools = true {
        didSet {
            if mode == .explorer {
                tools.forEach { $0.isHidden = !showsTools }
            }
        }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Explorer")
        setAccessibilityIdentifier("folder-title")

        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .labelColor
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        label.setContentHuggingPriority(.init(1), for: .horizontal)

        for (button, action) in [(newFileButton, #selector(newFile(_:))), (newFolderButton, #selector(newFolder(_:))),
                                 (refreshButton, #selector(refresh(_:))), (collapseButton, #selector(collapseAll(_:))),
                                 (moreButton, #selector(more(_:)))] {
            button.target = self
            button.action = action
        }

        for (button, action) in [
            (scmPullButton, #selector(scmPullAction(_:))),
            (scmBranchButton, #selector(scmBranchAction(_:))),
            (scmCommitButton, #selector(scmCommitAction(_:))),
            (scmRefreshButton, #selector(scmRefreshAction(_:))),
            (scmMoreButton, #selector(scmMoreAction(_:))),
        ] {
            button.target = self
            button.action = action
            button.isHidden = true
        }

        for (button, action) in [
            (searchRefreshButton, #selector(searchRefreshAction(_:))),
            (searchClearButton, #selector(searchClearAction(_:))),
            (searchReplaceButton, #selector(searchReplaceAction(_:))),
            (searchFilterButton, #selector(searchFilterAction(_:))),
            (searchMoreButton, #selector(searchMoreAction(_:))),
        ] {
            button.target = self
            button.action = action
            button.isHidden = true
        }

        let row = NSStackView(views: [label] + tools + scmTools + searchTools)
        row.spacing = 3
        row.setCustomSpacing(6, after: label)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Self.height),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14).withPriority(.defaultHigh),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8).withPriority(.defaultHigh),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func updateMode() {
        switch mode {
        case .explorer:
            scmTools.forEach { $0.isHidden = true }
            searchTools.forEach { $0.isHidden = true }
            tools.forEach { $0.isHidden = !showsTools }
            label.stringValue = title.isEmpty ? "EXPLORER" : title.uppercased()
        case .sourceControl:
            tools.forEach { $0.isHidden = true }
            scmTools.forEach { $0.isHidden = false }
            searchTools.forEach { $0.isHidden = true }
            label.stringValue = "SOURCE CONTROL"
        case .search:
            tools.forEach { $0.isHidden = true }
            scmTools.forEach { $0.isHidden = true }
            searchTools.forEach { $0.isHidden = false }
            label.stringValue = "SEARCH"
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).intersection(dirtyRect).fill()
    }

    private static func tool(_ symbol: String, _ title: String) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        b.widthAnchor.constraint(equalToConstant: 20).isActive = true
        b.heightAnchor.constraint(equalToConstant: 20).isActive = true
        return b
    }

    @objc private func newFile(_ sender: Any?) { onNewFile?() }
    @objc private func newFolder(_ sender: Any?) { onNewFolder?() }
    @objc private func refresh(_ sender: Any?) { onRefresh?() }
    @objc private func collapseAll(_ sender: Any?) { onCollapseAll?() }

    @objc private func more(_ sender: Any?) {
        guard let menu = moreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: moreButton.bounds.maxY + 4), in: moreButton)
    }

    @objc private func scmPullAction(_ sender: Any?) { onScmPull?() }
    @objc private func scmBranchAction(_ sender: Any?) { onScmBranch?() }
    @objc private func scmCommitAction(_ sender: Any?) { onScmCommit?() }
    @objc private func scmRefreshAction(_ sender: Any?) { onScmRefresh?() }

    @objc private func scmMoreAction(_ sender: Any?) {
        guard let menu = scmMoreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: scmMoreButton.bounds.maxY + 4), in: scmMoreButton)
    }
    @objc private func searchRefreshAction(_ sender: Any?) { onSearchRefresh?() }
    @objc private func searchClearAction(_ sender: Any?) { onSearchClear?() }
    @objc private func searchReplaceAction(_ sender: Any?) { onSearchToggleReplace?() }
    @objc private func searchFilterAction(_ sender: Any?) { onSearchToggleDetails?() }
    @objc private func searchMoreAction(_ sender: Any?) {
        guard let menu = searchMoreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: searchMoreButton.bounds.maxY + 4), in: searchMoreButton)
    }
}
