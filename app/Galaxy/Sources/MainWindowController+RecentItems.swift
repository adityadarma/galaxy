import AppKit

extension MainWindowController {
    var isRecentItemsVisible: Bool { loadedRecentItemsPicker.map { !$0.isHidden } ?? false }

    private var recentItemsPicker: RecentItemsPicker {
        if let picker = loadedRecentItemsPicker { return picker }
        let picker = RecentItemsPicker()
        picker.isHidden = true
        picker.translatesAutoresizingMaskIntoConstraints = false
        picker.onClose = { [weak self] in self?.closeRecentItems() }
        picker.onOpen = { [weak self] item in
            self?.closeRecentItems()
            self?.openRecentPath(item.path)
        }
        picker.onRemove = { [weak self] item in
            Self.forgetRecentPath(item.path, isFolder: item.isFolder)
            self?.reloadRecentItems(resetQuery: false)
        }
        window?.contentView?.addSubview(picker)
        let preferred = picker.widthAnchor.constraint(equalToConstant: RecentItemsPicker.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            picker.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            picker.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            picker.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
            preferred,
        ])
        loadedRecentItemsPicker = picker
        return picker
    }

    @objc func showRecentItems(_ sender: Any?) {
        closeOverlays()
        let picker = recentItemsPicker
        picker.isHidden = false
        reloadRecentItems()
        picker.focus()
    }

    func reloadRecentItems(resetQuery: Bool = true) {
        let folders = Self.recentProjectPaths().map { RecentItemsPicker.Item(path: $0, isFolder: true) }
        let files = Self.recentFilePaths().map { RecentItemsPicker.Item(path: $0, isFolder: false) }
        recentItemsPicker.show(folders + files, resetQuery: resetQuery)
    }

    func closeRecentItems() {
        guard isRecentItemsVisible else { return }
        recentItemsPicker.isHidden = true
        window?.makeFirstResponder(activeTab != nil ? editorView : nil)
    }
}
