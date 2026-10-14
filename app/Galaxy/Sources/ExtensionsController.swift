import AppKit

/// Sidebar list of language server plugins: built-in and installed ones,
/// plus what the configured registries offer. All work happens in the
/// window controller; this view only reports what the user asked for.
@MainActor
final class ExtensionsController: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    enum Row: Equatable {
        case header(String)
        case plugin(PluginInfo)
        case available(RegistryEntryInfo)
        case message(String)
    }

    let view = NSView()
    private let urlField = NSTextField()
    private let installButton = NSButton(title: "Install", target: nil, action: nil)
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let statusLabel = NSTextField(labelWithString: "")

    private(set) var rows: [Row] = []
    private var plugins: [PluginInfo] = []
    private var available: [RegistryEntryInfo] = []
    private var registryErrors: [String] = []
    private var hasRegistries = false
    /// Plugin id -> newest upstream commit.
    private var updates: [String: String] = [:]

    /// `URL`, `URL#REF` or `URL#REF:SUBDIR`.
    var onInstallSpec: ((_ spec: String, _ sha256: String?) -> Void)?
    var onUninstall: ((String) -> Void)?
    var onSetEnabled: ((_ id: String, _ enabled: Bool) -> Void)?
    var onUpdate: ((PluginInfo) -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false
        view.setAccessibilityIdentifier("extensions-sidebar")

        urlField.placeholderString = "Git URL, e.g. https://host/repo.git#v1.0:plugins/php"
        urlField.font = .systemFont(ofSize: 12)
        urlField.lineBreakMode = .byTruncatingMiddle
        urlField.delegate = self
        urlField.setAccessibilityIdentifier("extensions-url")
        urlField.translatesAutoresizingMaskIntoConstraints = false

        installButton.bezelStyle = .rounded
        installButton.controlSize = .small
        installButton.target = self
        installButton.action = #selector(installFromField(_:))
        installButton.setAccessibilityIdentifier("extensions-install")
        installButton.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("extensions"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .none
        table.dataSource = self
        table.delegate = self
        table.setAccessibilityIdentifier("extensions-list")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false

        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(urlField)
        view.addSubview(installButton)
        view.addSubview(statusLabel)
        view.addSubview(scroll)
        NSLayoutConstraint.activate([
            urlField.topAnchor.constraint(equalTo: view.topAnchor, constant: 10),
            urlField.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            installButton.leadingAnchor.constraint(equalTo: urlField.trailingAnchor, constant: 6),
            installButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10),
            installButton.centerYAnchor.constraint(equalTo: urlField.centerYAnchor),
            statusLabel.topAnchor.constraint(equalTo: urlField.bottomAnchor, constant: 6),
            statusLabel.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            statusLabel.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10),
            scroll.topAnchor.constraint(equalTo: statusLabel.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        rebuild()
    }

    // MARK: Data

    func show(plugins: [PluginInfo], available: [RegistryEntryInfo]? = nil, registryErrors: [String]? = nil,
              hasRegistries: Bool? = nil, updates: [String: String]? = nil) {
        self.plugins = plugins
        if let available { self.available = available }
        if let registryErrors { self.registryErrors = registryErrors }
        if let hasRegistries { self.hasRegistries = hasRegistries }
        if let updates { self.updates = updates }
        rebuild()
    }

    func setStatus(_ text: String) {
        statusLabel.stringValue = text
    }

    var statusText: String { statusLabel.stringValue }

    func clearField() {
        urlField.stringValue = ""
    }

    private func rebuild() {
        var rows: [Row] = []
        let installed = plugins.filter { !$0.builtin }
        let builtin = plugins.filter(\.builtin)
        rows.append(.header("INSTALLED"))
        if installed.isEmpty {
            rows.append(.message("Nothing installed. Paste a git URL above."))
        } else {
            rows += installed.map(Row.plugin)
        }
        rows.append(.header("BUILT-IN"))
        rows += builtin.map(Row.plugin)
        let installedIDs = Set(plugins.map(\.id))
        let offered = available.filter { !installedIDs.contains($0.id) || updates[$0.id] != nil }
        rows.append(.header("AVAILABLE"))
        if !hasRegistries {
            rows.append(.message("No registry configured. Use the + button to add one."))
        } else if offered.isEmpty && registryErrors.isEmpty {
            rows.append(.message("Nothing new in your registries."))
        }
        rows += offered.filter { !installedIDs.contains($0.id) }.map(Row.available)
        rows += registryErrors.map(Row.message)
        self.rows = rows
        table.reloadData()
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        switch rows[row] {
        case .header: 26
        case .plugin, .available: 46
        case .message: 30
        }
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { false }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch rows[row] {
        case .header(let title):
            let label = Self.label(title, size: 10.5, weight: .semibold, color: .secondaryLabelColor)
            return Self.cell([label], leading: 12, trailing: 10, centered: true)
        case .message(let text):
            let label = Self.label(text, size: 11.5, weight: .regular, color: .secondaryLabelColor)
            label.lineBreakMode = .byTruncatingTail
            return Self.cell([label], leading: 12, trailing: 10, centered: true)
        case .plugin(let plugin):
            return pluginCell(plugin, row: row)
        case .available(let entry):
            return availableCell(entry, row: row)
        }
    }

    private func pluginCell(_ plugin: PluginInfo, row: Int) -> NSView {
        let name = Self.label(plugin.name, size: 12, weight: .semibold, color: plugin.enabled ? .labelColor : .tertiaryLabelColor)
        let version = Self.label("v" + plugin.version, size: 10.5, weight: .regular, color: .secondaryLabelColor)
        var state = plugin.languages.joined(separator: ", ")
        if !plugin.enabled {
            state += " · disabled"
        } else if !plugin.available {
            state += plugin.installable ? " · not installed" : " · server not found"
        } else if plugin.builtin {
            state += " · using PATH"
        }
        if updates[plugin.id] != nil { state += " · update available" }
        let detail = Self.label(state, size: 11, weight: .regular, color: plugin.available || !plugin.enabled ? .secondaryLabelColor : .systemOrange)
        detail.lineBreakMode = .byTruncatingTail
        detail.toolTip = plugin.source ?? plugin.description
        let titleRow = NSStackView(views: [name, version])
        titleRow.spacing = 6
        let text = NSStackView(views: [titleRow, detail])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        text.setContentHuggingPriority(.init(1), for: .horizontal)
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var buttons: [NSButton] = []
        if updates[plugin.id] != nil {
            buttons.append(button("arrow.down.circle", "Update", row, #selector(updateClicked(_:)), id: "update"))
        }
        if plugin.installable {
            buttons.append(button("arrow.down.to.line", "Install locally", row, #selector(installBuiltinClicked(_:)), id: "install-builtin"))
        }
        buttons.append(button(plugin.enabled ? "checkmark.circle.fill" : "circle",
                              plugin.enabled ? "Disable" : "Enable", row, #selector(toggleClicked(_:)), id: "toggle"))
        if !plugin.builtin {
            buttons.append(button("trash", "Uninstall", row, #selector(uninstallClicked(_:)), id: "uninstall"))
        }
        return Self.cell([text] + buttons, leading: 12, trailing: 8, centered: true)
    }

    private func availableCell(_ entry: RegistryEntryInfo, row: Int) -> NSView {
        let name = Self.label(entry.name, size: 12, weight: .semibold, color: .labelColor)
        let version = Self.label(entry.version.isEmpty ? "" : "v" + entry.version, size: 10.5, weight: .regular, color: .secondaryLabelColor)
        let detail = Self.label(entry.description.isEmpty ? entry.languages.joined(separator: ", ") : entry.description,
                                size: 11, weight: .regular, color: .secondaryLabelColor)
        detail.lineBreakMode = .byTruncatingTail
        detail.toolTip = entry.source
        let titleRow = NSStackView(views: [name, version])
        titleRow.spacing = 6
        let text = NSStackView(views: [titleRow, detail])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        text.setContentHuggingPriority(.init(1), for: .horizontal)
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let install = button("arrow.down.to.line", "Install", row, #selector(installAvailableClicked(_:)), id: "install")
        return Self.cell([text, install], leading: 12, trailing: 8, centered: true)
    }

    // MARK: Actions

    @objc private func installFromField(_ sender: Any?) {
        let spec = urlField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !spec.isEmpty else { NSSound.beep(); return }
        onInstallSpec?(spec, nil)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        installFromField(nil)
        return true
    }

    @objc private func installAvailableClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), case .available(let entry) = rows[sender.tag] else { return }
        onInstallSpec?(entry.source, entry.sha256)
    }

    @objc private func installBuiltinClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), case .plugin(let plugin) = rows[sender.tag] else { return }
        onInstallSpec?("builtin:" + plugin.id, nil)
    }

    @objc private func updateClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), case .plugin(let plugin) = rows[sender.tag] else { return }
        onUpdate?(plugin)
    }

    @objc private func toggleClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), case .plugin(let plugin) = rows[sender.tag] else { return }
        onSetEnabled?(plugin.id, !plugin.enabled)
    }

    @objc private func uninstallClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), case .plugin(let plugin) = rows[sender.tag] else { return }
        onUninstall?(plugin.id)
    }

    // MARK: Building blocks

    private func button(_ symbol: String, _ title: String, _ row: Int, _ action: Selector, id: String) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 13, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        b.setAccessibilityIdentifier("extensions-\(id)-\(row)")
        b.tag = row
        b.target = self
        b.action = action
        b.widthAnchor.constraint(equalToConstant: 22).isActive = true
        b.heightAnchor.constraint(equalToConstant: 22).isActive = true
        b.setContentHuggingPriority(.required, for: .horizontal)
        return b
    }

    private static func label(_ text: String, size: CGFloat, weight: NSFont.Weight, color: NSColor) -> NSTextField {
        let field = NSTextField(labelWithString: text)
        field.font = .systemFont(ofSize: size, weight: weight)
        field.textColor = color
        field.lineBreakMode = .byTruncatingTail
        return field
    }

    private static func cell(_ views: [NSView], leading: CGFloat, trailing: CGFloat, centered: Bool) -> NSView {
        let cell = NSTableCellView()
        let stack = NSStackView(views: views)
        stack.spacing = 4
        stack.alignment = .centerY
        stack.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: leading),
            stack.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -trailing),
            stack.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}
