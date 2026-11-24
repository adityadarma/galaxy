import AppKit

@MainActor
final class PreferencesWindowController: NSWindowController, NSTextFieldDelegate {
    private let onChange: () -> Void
    private let appearance = NSPopUpButton(frame: .zero, pullsDown: false)
    private let font = NSPopUpButton(frame: .zero, pullsDown: false)
    private let fontSize = NSStepper(frame: .zero)
    private let fontSizeLabel = NSTextField(labelWithString: "")
    private let lineSpacing = NSStepper(frame: .zero)
    private let lineSpacingLabel = NSTextField(labelWithString: "")

    // Terminal tab: UserDefaults keys read by `TerminalView`.
    static let terminalFontNameKey = "TerminalFontName"
    static let terminalFontSizeKey = "TerminalFontSize"
    static let terminalSameAsEditor = "Same as Editor"
    private let terminalFont = NSPopUpButton(frame: .zero, pullsDown: false)
    private let terminalFontSize = NSStepper(frame: .zero)
    private let terminalFontSizeLabel = NSTextField(labelWithString: "")
    private let terminalLineHeight = NSPopUpButton(frame: .zero, pullsDown: false)
    private let terminalCursorStyle = NSPopUpButton(frame: .zero, pullsDown: false)
    private let terminalCursorBlink = NSButton(checkboxWithTitle: "Blink cursor", target: nil, action: nil)
    private let terminalPaddingH = NSStepper(frame: .zero)
    private let terminalPaddingHLabel = NSTextField(labelWithString: "")
    private let terminalPaddingV = NSStepper(frame: .zero)
    private let terminalPaddingVLabel = NSTextField(labelWithString: "")
    private static let terminalLineHeights: [Double] = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5]
    private static let terminalCursorStyles: [(title: String, value: TerminalSettings.CursorStyle)] = [
        ("Block", .block), ("Bar", .bar), ("Underline", .underline),
    ]

    // AI settings: a general provider at the top, a model per feature, and
    // optionally a separate provider for code completion.
    private let baseURLField = NSTextField()
    private let apiKeyField = NSSecureTextField()
    private let modelRefreshButton = NSButton()
    private let modelStatus = NSTextField(labelWithString: "")
    private let modelPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let completionEnabledBox = NSButton(checkboxWithTitle: "Show inline suggestions",
                                                target: nil, action: nil)
    private let completionCustomBox = NSButton(checkboxWithTitle: "Use a different provider",
                                               target: nil, action: nil)
    private let completionBaseURLField = NSTextField()
    private let completionApiKeyField = NSSecureTextField()
    private let completionRefreshButton = NSButton()
    private let completionStatus = NSTextField(labelWithString: "")
    private let completionModelPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    /// Per-feature state under each group's rows (e.g. "choose a model").
    private let commitNote = PreferencesWindowController.makeNote()
    private let completionNote = PreferencesWindowController.makeNote()
    /// Base URL, API key and model status rows of the completion provider.
    private var completionProviderRows: [NSView] = []
    /// Model IDs last read from the general provider, so switching the
    /// completion provider back can refill its popup without a request.
    private var generalModels: [String] = []
    private var modelLoadTask: Task<Void, Never>?
    private var modelReloadWork: DispatchWorkItem?
    private var completionLoadTask: Task<Void, Never>?
    private var completionReloadWork: DispatchWorkItem?

    // Editor tab: written to settings.json (a workspace's own file still wins).
    private let settingsURL: URL
    private let tabSizePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let insertSpacesBox = NSButton(checkboxWithTitle: "Insert spaces when pressing Tab", target: nil, action: nil)
    private let whitespacePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let autoClosingTagsBox = NSButton(checkboxWithTitle: "Close tags automatically", target: nil, action: nil)
    private let linkedEditingBox = NSButton(checkboxWithTitle: "Rename the matching tag", target: nil, action: nil)
    private let parameterHintsBox = NSButton(checkboxWithTitle: "Show names before arguments", target: nil, action: nil)
    private let formatOnSaveBox = NSButton(checkboxWithTitle: "Format on save", target: nil, action: nil)
    private let autoSavePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let autoSaveDelay = NSStepper(frame: .zero)
    private let autoSaveDelayLabel = NSTextField(labelWithString: "")
    private let settingsStatus = NSTextField(wrappingLabelWithString: "")
    /// One `files.exclude` row in the Explorer tab: a built-in glob or one
    /// written in settings.json.
    struct ExcludeEntry: Equatable {
        let glob: String
        let hidden: Bool
        let isDefault: Bool
    }
    /// The user's own globs first, then the built-in ones in `defaultFilesExclude` order.
    private var excludeEntries: [ExcludeEntry] = []
    private let excludeTable = NSTableView()
    private let excludeField = NSTextField()
    private let excludeAddButton = NSButton(title: "Add", target: nil, action: nil)
    private let excludeRemoveButton = NSButton(title: "Remove", target: nil, action: nil)
    private var editorControls: [NSControl] {
        [tabSizePopup, insertSpacesBox, whitespacePopup, parameterHintsBox, autoClosingTagsBox, linkedEditingBox,
          formatOnSaveBox, autoSavePopup, autoSaveDelay, gitBlameBox, excludeTable, excludeField]
    }

    // View tab: the same switches as the View menu.
    private let minimapBox = NSButton(checkboxWithTitle: "Minimap", target: nil, action: nil)
    private let wordWrapBox = NSButton(checkboxWithTitle: "Word wrap", target: nil, action: nil)
    private let breadcrumbsBox = NSButton(checkboxWithTitle: "Breadcrumbs", target: nil, action: nil)
    private let stickyScrollBox = NSButton(checkboxWithTitle: "Sticky scroll", target: nil, action: nil)
    private let gitBlameBox = NSButton(checkboxWithTitle: "Show blame on the caret line", target: nil, action: nil)
    private let folderIconsBox = NSButton(checkboxWithTitle: "Show folder icons", target: nil, action: nil)

    private static let tabSizes = [2, 4, 8]
    private static let whitespaceModes: [(title: String, value: WhitespaceRendering)] = [
        ("None", .none), ("Boundary", .boundary), ("All", .all),
    ]
    private static let autoSaveModes: [(title: String, value: AutoSaveMode)] = [
        ("Off", .off), ("After Delay", .afterDelay), ("On Focus Change", .onFocusChange),
    ]
    /// UserDefaults keys of the View toggles, with their defaults.
    private static let viewDefaults: [(key: String, fallback: Bool)] = [
        ("Minimap", true), ("WordWrap", false), ("ShowBreadcrumbs", true), ("StickyScroll", true),
    ]

    init(settingsURL: URL = GalaxySettings.globalURL, onChange: @escaping () -> Void) {
        self.settingsURL = settingsURL
        self.onChange = onChange
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 440, height: 200),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Settings"
        window.isReleasedWhenClosed = false
        window.toolbarStyle = .preference
        super.init(window: window)
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// One toolbar tab per group, the standard macOS Settings layout.
    private func build() {
        let tabs = SettingsTabViewController()
        tabs.tabStyle = .toolbar
        tabs.addTabViewItem(tab("General", symbol: "gearshape", pane: buildGeneralPane()))
        tabs.addTabViewItem(tab("Editor", symbol: "text.cursor", pane: buildEditorPane()))
        tabs.addTabViewItem(tab("Explorer", symbol: "folder", pane: buildExplorerPane()))
        tabs.addTabViewItem(tab("View", symbol: "rectangle.split.3x1", pane: buildViewPane()))
        tabs.addTabViewItem(tab("Terminal", symbol: "terminal", pane: buildTerminalPane()))
        tabs.addTabViewItem(tab("AI", symbol: "sparkles", pane: buildAIPane()))
        window?.contentViewController = tabs
        tabs.fitWindow(to: tabs.tabViewItems.first)
        window?.center()
        window?.delegate = self
        syncControls()
        loadModels()
    }

    // MARK: Editor and View tabs

    private func buildEditorPane() -> NSView {
        let stack = NSStackView()
        tabSizePopup.addItems(withTitles: Self.tabSizes.map(String.init))
        tabSizePopup.setAccessibilityLabel("Tab size")
        tabSizePopup.target = self; tabSizePopup.action = #selector(saveTabSize(_:))
        fixWidth(tabSizePopup)
        insertSpacesBox.target = self; insertSpacesBox.action = #selector(saveInsertSpaces(_:))
        addGroup("Indentation", to: stack, rows: [
            row("Tab size", tabSizePopup),
            row("", insertSpacesBox),
        ])

        whitespacePopup.addItems(withTitles: Self.whitespaceModes.map(\.title))
        whitespacePopup.setAccessibilityLabel("Render whitespace")
        whitespacePopup.target = self; whitespacePopup.action = #selector(saveWhitespace(_:))
        fixWidth(whitespacePopup)
        parameterHintsBox.target = self; parameterHintsBox.action = #selector(saveParameterHints(_:))
        parameterHintsBox.setAccessibilityLabel("Show parameter names before call arguments")
        addGroup("Rendering", to: stack, rows: [
            row("Whitespace", whitespacePopup),
            row("Parameter hints", parameterHintsBox),
        ])

        autoClosingTagsBox.target = self; autoClosingTagsBox.action = #selector(saveAutoClosingTags(_:))
        autoClosingTagsBox.setAccessibilityLabel("Auto close tags")
        linkedEditingBox.target = self; linkedEditingBox.action = #selector(saveLinkedEditing(_:))
        linkedEditingBox.setAccessibilityLabel("Rename matching tag while editing")
        addGroup("Tags", to: stack, rows: [
            row("While typing", autoClosingTagsBox),
            row("", linkedEditingBox),
            footnote("Applies to HTML, PHP and JSX."),
        ])

        formatOnSaveBox.target = self; formatOnSaveBox.action = #selector(saveFormatOnSave(_:))
        autoSavePopup.addItems(withTitles: Self.autoSaveModes.map(\.title))
        autoSavePopup.setAccessibilityLabel("Auto save")
        autoSavePopup.target = self; autoSavePopup.action = #selector(saveAutoSave(_:))
        fixWidth(autoSavePopup)
        autoSaveDelay.minValue = 250; autoSaveDelay.maxValue = 10_000; autoSaveDelay.increment = 250
        autoSaveDelay.setAccessibilityLabel("Auto save delay")
        autoSaveDelay.target = self; autoSaveDelay.action = #selector(saveAutoSaveDelay(_:))
        addGroup("Files", to: stack, rows: [
            row("Auto save", autoSavePopup),
            row("Delay", valueControl(autoSaveDelay, autoSaveDelayLabel)),
            row("", formatOnSaveBox),
            footnote("Format on save uses the project's or PATH's rustfmt, gofmt, dart format, prettier, pint or php-cs-fixer."),
        ])

        settingsStatus.font = .systemFont(ofSize: 11)
        settingsStatus.textColor = .secondaryLabelColor
        settingsStatus.preferredMaxLayoutWidth = Self.groupWidth
        settingsStatus.widthAnchor.constraint(equalToConstant: Self.groupWidth).isActive = true
        settingsStatus.stringValue = "Saved to settings.json. A folder's .galaxy/settings.json overrides these."
        stack.addArrangedSubview(settingsStatus)
        return pane(stack)
    }

    private func buildExplorerPane() -> NSView {
        let stack = NSStackView()
        folderIconsBox.target = self; folderIconsBox.action = #selector(saveFolderIcons(_:))
        addGroup("Appearance", to: stack, rows: [row("Folders", folderIconsBox)])

        for (id, width) in [("hide", 22.0), ("glob", 250.0), ("kind", 60.0)] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
            column.width = width
            column.resizingMask = id == "glob" ? .autoresizingMask : []
            excludeTable.addTableColumn(column)
        }
        excludeTable.headerView = nil
        excludeTable.style = .plain
        excludeTable.rowHeight = 22
        excludeTable.usesAlternatingRowBackgroundColors = true
        excludeTable.allowsMultipleSelection = true
        excludeTable.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        excludeTable.setAccessibilityLabel("Hidden file patterns")
        excludeTable.dataSource = self
        excludeTable.delegate = self
        let scroll = NSScrollView()
        scroll.documentView = excludeTable
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.widthAnchor.constraint(equalToConstant: 370).isActive = true
        scroll.heightAnchor.constraint(equalToConstant: 200).isActive = true

        excludeField.placeholderString = "**/*.log"
        excludeField.font = .monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        excludeField.setAccessibilityLabel("Pattern to hide")
        excludeField.delegate = self
        // Return adds the pattern; leaving the field does not.
        excludeField.cell?.sendsActionOnEndEditing = false
        excludeField.target = self; excludeField.action = #selector(addFilesExclude(_:))
        excludeField.setContentHuggingPriority(.defaultLow, for: .horizontal)
        excludeAddButton.target = self; excludeAddButton.action = #selector(addFilesExclude(_:))
        excludeRemoveButton.target = self; excludeRemoveButton.action = #selector(removeFilesExclude(_:))
        excludeRemoveButton.toolTip = "Remove the selected patterns. Built-in patterns can only be unchecked."
        let editRow = NSStackView(views: [excludeField, excludeAddButton, excludeRemoveButton])
        editRow.spacing = 8
        editRow.translatesAutoresizingMaskIntoConstraints = false
        editRow.widthAnchor.constraint(equalToConstant: 370).isActive = true

        addGroup("Hidden Files", to: stack, rows: [
            scroll,
            editRow,
            footnote("Checked patterns are hidden in the Explorer (files.exclude). Built-in patterns can be unchecked but not removed."),
        ])
        return pane(stack)
    }

    private func buildViewPane() -> NSView {
        let stack = NSStackView()
        let editorBoxes = [minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox]
        for box in editorBoxes {
            box.target = self; box.action = #selector(saveViewToggle(_:))
        }
        addGroup("Editor", to: stack, rows: [
            row("Show", minimapBox),
            row("", breadcrumbsBox),
            row("", stickyScrollBox),
            row("Long lines", wordWrapBox),
            footnote("The View menu toggles the same switches."),
        ])
        gitBlameBox.target = self; gitBlameBox.action = #selector(saveGitBlame(_:))
        gitBlameBox.setAccessibilityLabel("Inline Git blame on the caret line")
        addGroup("Git", to: stack, rows: [row("Blame", gitBlameBox)])
        return pane(stack)
    }

    private func buildTerminalPane() -> NSView {
        let stack = NSStackView()
        let installedFonts = Self.installedFontFamilies()
        terminalFont.addItems(withTitles: [Self.terminalSameAsEditor] + installedFonts)
        let saved = UserDefaults.standard.string(forKey: Self.terminalFontNameKey) ?? Self.terminalSameAsEditor
        terminalFont.selectItem(withTitle: installedFonts.contains(saved) ? saved : Self.terminalSameAsEditor)
        terminalFont.setAccessibilityLabel("Terminal font")
        terminalFont.target = self; terminalFont.action = #selector(saveTerminalFont(_:))
        fixWidth(terminalFont)

        let size = UserDefaults.standard.object(forKey: Self.terminalFontSizeKey) as? Double ?? 12
        terminalFontSize.minValue = 10; terminalFontSize.maxValue = 24; terminalFontSize.increment = 1
        terminalFontSize.doubleValue = min(max(size, 10), 24)
        terminalFontSize.setAccessibilityLabel("Terminal font size")
        terminalFontSize.target = self; terminalFontSize.action = #selector(saveTerminalFontSize(_:))
        updateTerminalFontSizeLabel()

        let current = TerminalSettings.current
        terminalLineHeight.addItems(withTitles: Self.terminalLineHeights.map { String(format: "%.1f×", $0) })
        let heightIndex = Self.terminalLineHeights.enumerated()
            .min { abs($0.element - Double(current.lineHeight)) < abs($1.element - Double(current.lineHeight)) }?.offset ?? 0
        terminalLineHeight.selectItem(at: heightIndex)
        terminalLineHeight.setAccessibilityLabel("Terminal line height")
        terminalLineHeight.target = self; terminalLineHeight.action = #selector(saveTerminalLineHeight(_:))
        fixWidth(terminalLineHeight)
        addGroup("Font", to: stack, rows: [
            row("Font family", terminalFont),
            row("Font size", valueControl(terminalFontSize, terminalFontSizeLabel)),
            row("Line height", terminalLineHeight),
            footnote("\"Same as Editor\" follows the editor font family. Changes apply to open terminals right away."),
        ])

        terminalCursorStyle.addItems(withTitles: Self.terminalCursorStyles.map(\.title))
        terminalCursorStyle.selectItem(at: Self.terminalCursorStyles.firstIndex { $0.value == current.cursorStyle } ?? 0)
        terminalCursorStyle.setAccessibilityLabel("Terminal cursor style")
        terminalCursorStyle.target = self; terminalCursorStyle.action = #selector(saveTerminalCursorStyle(_:))
        fixWidth(terminalCursorStyle)
        terminalCursorBlink.state = current.cursorBlink ? .on : .off
        terminalCursorBlink.target = self; terminalCursorBlink.action = #selector(saveTerminalCursorBlink(_:))
        addGroup("Cursor", to: stack, rows: [
            row("Style", terminalCursorStyle),
            row("", terminalCursorBlink),
        ])

        for (stepper, value, label) in [
            (terminalPaddingH, current.paddingHorizontal, "Horizontal terminal padding"),
            (terminalPaddingV, current.paddingVertical, "Vertical terminal padding"),
        ] {
            stepper.minValue = TerminalSettings.paddingRange.lowerBound
            stepper.maxValue = TerminalSettings.paddingRange.upperBound
            stepper.increment = 2
            stepper.doubleValue = Double(value)
            stepper.setAccessibilityLabel(label)
            stepper.target = self; stepper.action = #selector(saveTerminalPadding(_:))
        }
        updateTerminalPaddingLabels()
        addGroup("Padding", to: stack, rows: [
            row("Horizontal", valueControl(terminalPaddingH, terminalPaddingHLabel)),
            row("Vertical", valueControl(terminalPaddingV, terminalPaddingVLabel)),
        ])
        return pane(stack)
    }

    @objc private func saveTerminalFont(_ sender: Any?) {
        let title = terminalFont.titleOfSelectedItem ?? Self.terminalSameAsEditor
        if title == Self.terminalSameAsEditor {
            UserDefaults.standard.removeObject(forKey: Self.terminalFontNameKey)
        } else {
            UserDefaults.standard.set(title, forKey: Self.terminalFontNameKey)
        }
        onChange()
    }

    @objc private func saveTerminalFontSize(_ sender: Any?) {
        UserDefaults.standard.set(terminalFontSize.doubleValue, forKey: Self.terminalFontSizeKey)
        updateTerminalFontSizeLabel()
        onChange()
    }

    @objc private func saveTerminalLineHeight(_ sender: Any?) {
        let index = max(terminalLineHeight.indexOfSelectedItem, 0)
        UserDefaults.standard.set(Self.terminalLineHeights[index], forKey: TerminalSettings.lineHeightKey)
        onChange()
    }

    @objc private func saveTerminalCursorStyle(_ sender: Any?) {
        let style = Self.terminalCursorStyles[max(terminalCursorStyle.indexOfSelectedItem, 0)].value
        UserDefaults.standard.set(style.rawValue, forKey: TerminalSettings.cursorStyleKey)
        onChange()
    }

    @objc private func saveTerminalCursorBlink(_ sender: Any?) {
        UserDefaults.standard.set(terminalCursorBlink.state == .on, forKey: TerminalSettings.cursorBlinkKey)
        onChange()
    }

    @objc private func saveTerminalPadding(_ sender: Any?) {
        UserDefaults.standard.set(terminalPaddingH.doubleValue, forKey: TerminalSettings.paddingHorizontalKey)
        UserDefaults.standard.set(terminalPaddingV.doubleValue, forKey: TerminalSettings.paddingVerticalKey)
        updateTerminalPaddingLabels()
        onChange()
    }

    private func updateTerminalPaddingLabels() {
        terminalPaddingHLabel.stringValue = "\(Int(terminalPaddingH.doubleValue)) pt"
        terminalPaddingVLabel.stringValue = "\(Int(terminalPaddingV.doubleValue)) pt"
    }

    private func updateTerminalFontSizeLabel() {
        terminalFontSizeLabel.stringValue = "\(Int(terminalFontSize.doubleValue)) pt"
    }

    private func section(_ title: String) -> NSTextField {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 12, weight: .semibold)
        return label
    }

    /// Shows the saved values; run again when the window comes back, since
    /// the View menu and settings.json can change them meanwhile.
    private func syncControls() {
        let defaults = UserDefaults.standard
        for (box, entry) in zip([minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox], Self.viewDefaults) {
            box.state = (defaults.object(forKey: entry.key) as? Bool ?? entry.fallback) ? .on : .off
        }
        folderIconsBox.state = ExplorerIcon.showsFolderIcons ? .on : .off
        let settings: GalaxySettings
        do {
            settings = FileManager.default.fileExists(atPath: settingsURL.path)
                ? try GalaxySettings.load(from: settingsURL) : .empty
        } catch {
            // Leave a broken file for the user to fix rather than overwrite it.
            editorControls.forEach { $0.isEnabled = false }
            showSettingsError("settings.json could not be read, so these are locked. Fix the file to edit them here.")
            excludeTable.reloadData()
            updateExcludeButtons()
            return
        }
        editorControls.forEach { $0.isEnabled = true }
        let size = Int(settings.effectiveTabSize)
        if !Self.tabSizes.contains(size), tabSizePopup.item(withTitle: String(size)) == nil {
            tabSizePopup.addItem(withTitle: String(size))
        }
        tabSizePopup.selectItem(withTitle: String(size))
        insertSpacesBox.state = settings.useTabs ? .off : .on
        whitespacePopup.selectItem(at: Self.whitespaceModes.firstIndex { $0.value == settings.effectiveRenderWhitespace } ?? 2)
        autoClosingTagsBox.state = settings.effectiveAutoClosingTags ? .on : .off
        linkedEditingBox.state = settings.effectiveLinkedEditing ? .on : .off
        formatOnSaveBox.state = settings.effectiveFormatOnSave ? .on : .off
        parameterHintsBox.state = settings.effectiveInlineParameterHints ? .on : .off
        gitBlameBox.state = settings.effectiveGitBlame ? .on : .off
        showExcludes(settings)
        autoSavePopup.selectItem(at: Self.autoSaveModes.firstIndex { $0.value == settings.effectiveAutoSave } ?? 0)
        autoSaveDelay.doubleValue = settings.effectiveAutoSaveDelay * 1000
        updateAutoSaveDelay()
    }

    private func updateAutoSaveDelay() {
        autoSaveDelayLabel.stringValue = "\(Int(autoSaveDelay.doubleValue)) ms"
        let afterDelay = Self.autoSaveModes[max(autoSavePopup.indexOfSelectedItem, 0)].value == .afterDelay
        autoSaveDelay.isEnabled = afterDelay && autoSavePopup.isEnabled
        autoSaveDelayLabel.textColor = autoSaveDelay.isEnabled ? .labelColor : .tertiaryLabelColor
    }

    private func showSettingsError(_ message: String) {
        settingsStatus.textColor = .systemRed
        settingsStatus.stringValue = message
    }

    /// Writes one key to settings.json and lets open windows pick it up.
    private func store(_ value: Any, _ key: String, in section: String = "editor") {
        do {
            try GalaxySettings.write(value, key: key, section: section, to: settingsURL)
            onChange()
        } catch {
            showSettingsError("Could not save \(section).\(key): \(error.localizedDescription)")
            syncControls()
        }
    }

    @objc private func saveTabSize(_ sender: Any?) {
        guard let size = tabSizePopup.titleOfSelectedItem.flatMap(Int.init) else { return }
        store(size, "tabSize")
    }

    @objc private func saveInsertSpaces(_ sender: Any?) { store(insertSpacesBox.state == .on, "insertSpaces") }
    @objc private func saveAutoClosingTags(_ sender: Any?) { store(autoClosingTagsBox.state == .on, "autoClosingTags") }
    @objc private func saveLinkedEditing(_ sender: Any?) { store(linkedEditingBox.state == .on, "linkedEditing") }
    @objc private func saveFormatOnSave(_ sender: Any?) { store(formatOnSaveBox.state == .on, "formatOnSave") }
    @objc private func saveGitBlame(_ sender: Any?) { store(gitBlameBox.state == .on, "gitBlame") }
    @objc private func saveParameterHints(_ sender: Any?) {
        store(parameterHintsBox.state == .on, "inlineParameterHints")
    }

    /// Refills the pattern list, keeping the selected patterns selected.
    private func showExcludes(_ settings: GalaxySettings, selecting globs: Set<String>? = nil) {
        let keep = globs ?? Set(excludeTable.selectedRowIndexes.compactMap {
            excludeEntries.indices.contains($0) ? excludeEntries[$0].glob : nil
        })
        let hidden = Set(settings.effectiveFilesExclude)
        let defaults = GalaxySettings.defaultFilesExclude
        let custom = (settings.filesExclude ?? [:]).keys.filter { !defaults.contains($0) }.sorted()
        excludeEntries = custom.map { ExcludeEntry(glob: $0, hidden: hidden.contains($0), isDefault: false) }
            + defaults.map { ExcludeEntry(glob: $0, hidden: hidden.contains($0), isDefault: true) }
        excludeTable.reloadData()
        let rows = IndexSet(excludeEntries.indices.filter { keep.contains(excludeEntries[$0].glob) })
        excludeTable.selectRowIndexes(rows, byExtendingSelection: false)
        if let first = rows.first { excludeTable.scrollRowToVisible(first) }
        updateExcludeButtons()
    }

    private var selectedCustomExcludes: [String] {
        excludeTable.selectedRowIndexes.compactMap {
            excludeEntries.indices.contains($0) && !excludeEntries[$0].isDefault ? excludeEntries[$0].glob : nil
        }
    }

    private func updateExcludeButtons() {
        let enabled = excludeTable.isEnabled
        excludeAddButton.isEnabled = enabled
            && !excludeField.stringValue.trimmingCharacters(in: .whitespaces).isEmpty
        excludeRemoveButton.isEnabled = enabled && !selectedCustomExcludes.isEmpty
    }

    /// Runs one files.exclude write, then shows the file's new state.
    @discardableResult
    private func saveExcludes(selecting globs: Set<String>? = nil, _ write: () throws -> Void) -> Bool {
        do {
            try write()
            onChange()
        } catch {
            showSettingsError("Could not save files.exclude: \(error.localizedDescription)")
            syncControls()
            return false
        }
        let settings = (try? GalaxySettings.load(from: settingsURL)) ?? .empty
        showExcludes(settings, selecting: globs)
        return true
    }

    @objc private func toggleFilesExclude(_ sender: NSButton) {
        guard excludeEntries.indices.contains(sender.tag) else { return }
        let glob = excludeEntries[sender.tag].glob
        let hide = sender.state == .on
        saveExcludes { try GalaxySettings.writeFilesExclude(glob: glob, enabled: hide, to: settingsURL) }
    }

    @objc private func addFilesExclude(_ sender: Any?) {
        let glob = excludeField.stringValue.trimmingCharacters(in: .whitespaces)
        guard !glob.isEmpty, excludeTable.isEnabled else { return }
        // Already hidden: point at the row instead of writing it again.
        if excludeEntries.contains(where: { $0.glob == glob && $0.hidden }) {
            excludeField.stringValue = ""
            let settings = (try? GalaxySettings.load(from: settingsURL)) ?? .empty
            return showExcludes(settings, selecting: [glob])
        }
        if saveExcludes(selecting: [glob], { try GalaxySettings.writeFilesExclude(glob: glob, enabled: true, to: settingsURL) }) {
            excludeField.stringValue = ""
            updateExcludeButtons()
        }
    }

    @objc private func removeFilesExclude(_ sender: Any?) {
        let globs = selectedCustomExcludes
        guard !globs.isEmpty else { return }
        saveExcludes(selecting: []) { try GalaxySettings.removeFilesExclude(globs: globs, from: settingsURL) }
    }

    @objc private func saveWhitespace(_ sender: Any?) {
        store(Self.whitespaceModes[max(whitespacePopup.indexOfSelectedItem, 0)].value.rawValue, "renderWhitespace")
    }

    @objc private func saveAutoSave(_ sender: Any?) {
        updateAutoSaveDelay()
        store(Self.autoSaveModes[max(autoSavePopup.indexOfSelectedItem, 0)].value.rawValue, "autoSave", in: "files")
    }

    @objc private func saveAutoSaveDelay(_ sender: Any?) {
        updateAutoSaveDelay()
        store(Int(autoSaveDelay.doubleValue), "autoSaveDelay", in: "files")
    }

    @objc private func saveFolderIcons(_ sender: Any?) {
        UserDefaults.standard.set(folderIconsBox.state == .on, forKey: ExplorerIcon.showFolderIconsKey)
        onChange()
    }

    @objc private func saveViewToggle(_ sender: Any?) {
        guard let box = sender as? NSButton,
              let index = [minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox].firstIndex(of: box) else { return }
        UserDefaults.standard.set(box.state == .on, forKey: Self.viewDefaults[index].key)
        onChange()
    }

    private func tab(_ title: String, symbol: String, pane: NSView) -> NSTabViewItem {
        let controller = NSViewController()
        controller.view = pane
        controller.title = title
        let item = NSTabViewItem(viewController: controller)
        item.label = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return item
    }

    /// A pane holding `stack` with the usual Settings margins.
    private func pane(_ stack: NSStackView) -> NSView {
        let view = NSView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -20),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
        ])
        return view
    }

    private func buildGeneralPane() -> NSView {
        let stack = NSStackView()
        appearance.addItems(withTitles: Theme.names)
        appearance.selectItem(withTitle: Theme.names.contains(Theme.selectedName) ? Theme.selectedName : Theme.systemName)
        appearance.target = self; appearance.action = #selector(saveAppearance(_:))
        let installedFonts = Self.installedFontFamilies()
        font.addItems(withTitles: ["System Monospaced"] + installedFonts)
        let savedFont = UserDefaults.standard.string(forKey: "EditorFontName") ?? "System Monospaced"
        font.selectItem(withTitle: installedFonts.contains(savedFont) || savedFont == "System Monospaced" ? savedFont : "System Monospaced")
        font.target = self; font.action = #selector(saveFont(_:))
        let size = UserDefaults.standard.object(forKey: "EditorFontSize") as? Double ?? 13
        fontSize.minValue = 10; fontSize.maxValue = 24; fontSize.increment = 1; fontSize.doubleValue = size
        fontSize.target = self; fontSize.action = #selector(saveFontSize(_:))
        updateFontSizeLabel()
        let spacing = UserDefaults.standard.object(forKey: "EditorLineSpacing") as? Double ?? 0
        lineSpacing.minValue = 0; lineSpacing.maxValue = 12; lineSpacing.increment = 1; lineSpacing.doubleValue = spacing
        lineSpacing.target = self; lineSpacing.action = #selector(saveLineSpacing(_:))
        updateLineSpacingLabel()

        appearance.setAccessibilityLabel("Theme")
        font.setAccessibilityLabel("Editor font")
        fontSize.setAccessibilityLabel("Editor font size")
        lineSpacing.setAccessibilityLabel("Editor line spacing")
        fixWidth(appearance)
        fixWidth(font)

        addGroup("Appearance", to: stack, rows: [row("Theme", appearance)])
        addGroup("Editor Font", to: stack, rows: [
            row("Font family", font),
            row("Font size", valueControl(fontSize, fontSizeLabel)),
            row("Line spacing", valueControl(lineSpacing, lineSpacingLabel)),
            footnote("The terminal can use its own font in the Terminal tab."),
        ])
        return pane(stack)
    }

    private func buildAIPane() -> NSView {
        let stack = NSStackView()

        // General provider: shared by every AI feature.
        configure(baseURLField, key: AIConnection.Keys.baseURL,
                  placeholder: AIProvider.defaultBaseURL, label: "AI base URL")
        configure(apiKeyField, key: AIConnection.Keys.apiKey,
                  placeholder: "sk-… (optional for local servers)", label: "AI API key")
        configureStatus(modelStatus, button: modelRefreshButton, action: #selector(reloadModels(_:)))
        addGroup("General Provider", to: stack, rows: [
            row("Base URL", baseURLField),
            row("API Key", apiKeyField),
            row("Models", statusRow(modelStatus, modelRefreshButton)),
            footnote("OpenAI-compatible <Base URL>/chat/completions: OpenAI, OpenRouter, LM Studio, Ollama (http://localhost:11434/v1), …"),
        ])

        configurePopup(modelPopup, label: "Commit message model", action: #selector(saveModel(_:)))
        addGroup("Commit Message", to: stack, rows: [row("Model", modelPopup), commitNote])

        completionEnabledBox.target = self
        completionEnabledBox.action = #selector(saveCompletionEnabled(_:))
        completionEnabledBox.state = UserDefaults.standard.bool(forKey: AICompletionService.Keys.enabled) ? .on : .off
        completionCustomBox.target = self
        completionCustomBox.action = #selector(saveCompletionCustomProvider(_:))
        completionCustomBox.state = AIConnection.completionUsesCustomProvider ? .on : .off
        configure(completionBaseURLField, key: AIConnection.Keys.completionBaseURL,
                  placeholder: AIProvider.defaultBaseURL, label: "Code completion base URL")
        configure(completionApiKeyField, key: AIConnection.Keys.completionApiKey,
                  placeholder: "sk-… (optional for local servers)", label: "Code completion API key")
        configureStatus(completionStatus, button: completionRefreshButton, action: #selector(reloadCompletionModels(_:)))
        configurePopup(completionModelPopup, label: "Code completion model",
                       action: #selector(saveCompletionModel(_:)))
        // Shown only while "Use a different provider" is on.
        completionProviderRows = [
            row("Base URL", completionBaseURLField),
            row("API Key", completionApiKeyField),
            row("Models", statusRow(completionStatus, completionRefreshButton)),
        ]
        // Provider first, then the model: the list comes from that provider.
        addGroup("Code Completion", to: stack, rows: [
            row("", completionEnabledBox),
            row("", completionCustomBox),
        ] + completionProviderRows + [
            row("Model", completionModelPopup),
            completionNote,
        ])

        showModels([], in: modelPopup, selecting: AICommitService.shared.model)
        showModels([], in: completionModelPopup, selecting: AICompletionService.shared.model)
        updateCompletionProviderControls()
        updateFeatureNotes()
        return pane(stack)
    }

    /// A titled, rounded block of rows, as in System Settings.
    private func addGroup(_ title: String, to stack: NSStackView, rows: [NSView]) {
        let content = NSStackView(views: rows)
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 10
        content.translatesAutoresizingMaskIntoConstraints = false

        let box = NSBox()
        box.boxType = .custom
        box.titlePosition = .noTitle
        box.cornerRadius = 8
        box.borderWidth = 1
        box.borderColor = .separatorColor
        box.fillColor = NSColor.labelColor.withAlphaComponent(0.04)
        box.contentViewMargins = .zero
        box.translatesAutoresizingMaskIntoConstraints = false
        box.contentView?.addSubview(content)
        if let inner = box.contentView {
            NSLayoutConstraint.activate([
                content.topAnchor.constraint(equalTo: inner.topAnchor, constant: 12),
                content.bottomAnchor.constraint(equalTo: inner.bottomAnchor, constant: -12),
                content.leadingAnchor.constraint(equalTo: inner.leadingAnchor, constant: 14),
                content.trailingAnchor.constraint(lessThanOrEqualTo: inner.trailingAnchor, constant: -14),
            ])
        }

        let heading = section(title)
        stack.addArrangedSubview(heading)
        stack.setCustomSpacing(6, after: heading)
        stack.addArrangedSubview(box)
        stack.setCustomSpacing(18, after: box)
        box.widthAnchor.constraint(equalToConstant: Self.groupWidth).isActive = true
    }

    /// Width of every group box; rows inside are label (120) + control (238).
    private static let groupWidth: CGFloat = 398

    /// Gives long popups (fonts, themes) the same width as the AI fields.
    private func fixWidth(_ control: NSView) {
        control.translatesAutoresizingMaskIntoConstraints = false
        control.widthAnchor.constraint(equalToConstant: 238).isActive = true
    }

    /// A stepper with its current value beside it.
    private func valueControl(_ stepper: NSStepper, _ value: NSTextField) -> NSStackView {
        value.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        let controls = NSStackView(views: [stepper, value])
        controls.spacing = 8
        return controls
    }

    private static func makeNote() -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: "")
        label.font = .systemFont(ofSize: 11)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = 370
        label.translatesAutoresizingMaskIntoConstraints = false
        label.widthAnchor.constraint(equalToConstant: 370).isActive = true
        return label
    }

    /// Small grey text under a group's rows.
    private func footnote(_ text: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: 11)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = 370
        label.translatesAutoresizingMaskIntoConstraints = false
        label.widthAnchor.constraint(equalToConstant: 370).isActive = true
        return label
    }

    // MARK: AI model pickers

    /// First popup item while no model is chosen. It saves nothing.
    private static let noModelTitle = "Select a model…"

    /// Tells, per feature, whether it can run and what is missing.
    private func updateFeatureNotes() {
        if AICommitService.shared.model == nil {
            setNote(commitNote, "Choose a model to generate commit messages with AI. Until then the button stays off.", warning: true)
        } else {
            setNote(commitNote, "The sparkle button in Source Control writes the message from the diff.", warning: false)
        }
        if AICompletionService.shared.model == nil {
            setNote(completionNote, "Choose a model to show inline suggestions.", warning: true)
        } else {
            setNote(completionNote, "Sends the code around the caret to the provider; a small, fast model works best. Tab accepts a suggestion, Esc dismisses it.", warning: false)
        }
    }

    private func setNote(_ label: NSTextField, _ text: String, warning: Bool) {
        label.stringValue = text
        label.textColor = warning ? .systemOrange : .secondaryLabelColor
    }

    private func configurePopup(_ popup: NSPopUpButton, label: String, action: Selector) {
        popup.target = self
        popup.action = action
        popup.setAccessibilityLabel(label)
        popup.translatesAutoresizingMaskIntoConstraints = false
        popup.widthAnchor.constraint(equalToConstant: 238).isActive = true
    }

    /// Fills a popup. The saved model is always present, even when the
    /// server does not list it, so a choice is never silently replaced.
    private func showModels(_ models: [String], in popup: NSPopUpButton, selecting selected: String?) {
        popup.removeAllItems()
        var titles = models
        if let selected, !titles.contains(selected) { titles.insert(selected, at: 0) }
        if selected == nil { titles.insert(Self.noModelTitle, at: 0) }
        popup.addItems(withTitles: titles)
        if let selected, !models.isEmpty, !models.contains(selected) {
            popup.item(withTitle: selected)?.toolTip = "Not offered by this server"
        }
        popup.selectItem(withTitle: selected ?? Self.noModelTitle)
    }

    private func configureStatus(_ status: NSTextField, button: NSButton, action: Selector) {
        button.bezelStyle = .texturedRounded
        button.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Reload models")
        button.imagePosition = .imageOnly
        button.toolTip = "Reload models from the server"
        button.target = self
        button.action = action
        button.widthAnchor.constraint(equalToConstant: 26).isActive = true
        status.font = .systemFont(ofSize: 11)
        status.textColor = .secondaryLabelColor
        status.lineBreakMode = .byTruncatingTail
        status.translatesAutoresizingMaskIntoConstraints = false
        status.widthAnchor.constraint(equalToConstant: 208).isActive = true
    }

    private func statusRow(_ status: NSTextField, _ button: NSButton) -> NSStackView {
        let row = NSStackView(views: [status, button])
        row.spacing = 4
        return row
    }

    /// The completion provider fields only apply while their checkbox is on.
    private func updateCompletionProviderControls() {
        let custom = completionCustomBox.state == .on
        for row in completionProviderRows { row.isHidden = !custom }
        // With its own provider, the model list waits for that provider's /models.
        if !custom { completionModelPopup.isEnabled = true }
        // The pane changed height: resize the window to it.
        if let tabs = window?.contentViewController as? SettingsTabViewController,
           let item = tabs.tabViewItems.first(where: { $0.label == "AI" }),
           tabs.tabView.selectedTabViewItem === item {
            tabs.fitWindow(to: item, animate: window?.isVisible ?? false)
        }
    }

    /// Reads `/models` and reports in `status`. `apply` gets the list, or
    /// an empty one on failure, so the saved choices stay in the popups.
    private func fetchModels(from provider: AIProvider, status: NSTextField, button: NSButton,
                             selected: @escaping () -> [String],
                             apply: @escaping ([String]) -> Void) -> Task<Void, Never> {
        button.isEnabled = false
        status.textColor = .secondaryLabelColor
        status.stringValue = "Loading models…"
        return Task { [weak status, weak button] in
            let result: Result<[String], Error>
            do { result = .success(try await provider.fetchModels()) } catch { result = .failure(error) }
            guard !Task.isCancelled, let status else { return }
            button?.isEnabled = true
            switch result {
            case .success(let models):
                apply(models)
                if models.isEmpty {
                    status.stringValue = "Server returned no models."
                } else if let missing = selected().first(where: { !models.contains($0) }) {
                    status.textColor = .systemOrange
                    status.stringValue = "'\(missing)' is not on this server."
                } else {
                    status.stringValue = "\(models.count) models available."
                }
            case .failure(let error):
                apply([])
                status.textColor = .systemRed
                status.stringValue = "Could not load models: \(error)"
            }
        }
    }

    /// Automatic loads wait until the provider has both a base URL and an
    /// API key, so a half-filled form does not report "API key rejected".
    /// The reload button always asks.
    private func hasCredentials(_ provider: AIProvider) -> Bool {
        !AIConnection.trimmed(UserDefaults.standard.string(forKey: provider.baseURLKey)).isEmpty
            && provider.apiKey != nil
    }

    private func showWaitingForCredentials(_ status: NSTextField, button: NSButton) {
        button.isEnabled = true
        status.textColor = .secondaryLabelColor
        status.stringValue = "Enter Base URL and API key to load models."
    }

    private func loadModels(force: Bool = false) {
        modelLoadTask?.cancel()
        let custom = AIConnection.completionUsesCustomProvider
        if custom { loadCompletionModels(force: force) }
        guard force || hasCredentials(AIConnection.general) else {
            return showWaitingForCredentials(modelStatus, button: modelRefreshButton)
        }
        modelLoadTask = fetchModels(
            from: AIConnection.general, status: modelStatus, button: modelRefreshButton,
            selected: { (custom ? [AICommitService.shared.model]
                : [AICommitService.shared.model, AICompletionService.shared.model]).compactMap { $0 } },
            apply: { [weak self] models in
                guard let self else { return }
                self.generalModels = models
                self.showModels(models, in: self.modelPopup, selecting: AICommitService.shared.model)
                if !AIConnection.completionUsesCustomProvider {
                    self.showModels(models, in: self.completionModelPopup, selecting: AICompletionService.shared.model)
                }
            })
    }

    private func loadCompletionModels(force: Bool = false) {
        completionLoadTask?.cancel()
        guard AIConnection.completionUsesCustomProvider else { return }
        completionModelPopup.isEnabled = false
        guard force || hasCredentials(AIConnection.completionOverride) else {
            return showWaitingForCredentials(completionStatus, button: completionRefreshButton)
        }
        completionLoadTask = fetchModels(
            from: AIConnection.completionOverride, status: completionStatus, button: completionRefreshButton,
            selected: { [AICompletionService.shared.model].compactMap { $0 } },
            apply: { [weak self] models in
                guard let self else { return }
                self.showModels(models, in: self.completionModelPopup, selecting: AICompletionService.shared.model)
                // Pick a model only from a provider that answered.
                self.completionModelPopup.isEnabled = !models.isEmpty
            })
    }

    @objc private func reloadModels(_ sender: Any?) { loadModels(force: true) }
    @objc private func reloadCompletionModels(_ sender: Any?) { loadCompletionModels(force: true) }

    @objc private func saveModel(_ sender: Any?) {
        saveChoice(of: modelPopup, key: AICommitService.Keys.model)
    }

    @objc private func saveCompletionModel(_ sender: Any?) {
        saveChoice(of: completionModelPopup, key: AICompletionService.Keys.model)
    }

    /// Saves the popup's model. Once one is chosen the placeholder goes away.
    private func saveChoice(of popup: NSPopUpButton, key: String) {
        guard let model = popup.titleOfSelectedItem, !model.isEmpty, model != Self.noModelTitle else { return }
        UserDefaults.standard.set(model, forKey: key)
        if let placeholder = popup.item(withTitle: Self.noModelTitle) { popup.removeItem(at: popup.index(of: placeholder)) }
        popup.selectItem(withTitle: model)
        updateFeatureNotes()
    }

    @objc private func saveCompletionEnabled(_ sender: Any?) {
        UserDefaults.standard.set(completionEnabledBox.state == .on, forKey: AICompletionService.Keys.enabled)
    }

    @objc private func saveCompletionCustomProvider(_ sender: Any?) {
        let custom = completionCustomBox.state == .on
        UserDefaults.standard.set(custom, forKey: AIConnection.Keys.completionCustomProvider)
        updateCompletionProviderControls()
        if custom {
            loadCompletionModels()
        } else {
            completionLoadTask?.cancel()
            showModels(generalModels, in: completionModelPopup, selecting: AICompletionService.shared.model)
        }
    }

    private func row(_ title: String, _ control: NSView) -> NSStackView {
        let label = NSTextField(labelWithString: title)
        label.translatesAutoresizingMaskIntoConstraints = false
        label.widthAnchor.constraint(equalToConstant: 120).isActive = true
        let row = NSStackView(views: [label, control])
        row.spacing = 12
        return row
    }

    @objc private func saveAppearance(_ sender: Any?) {
        Theme.select(appearance.titleOfSelectedItem ?? Theme.systemName)
        onChange()
    }

    @objc private func saveFont(_ sender: Any?) {
        UserDefaults.standard.set(font.titleOfSelectedItem, forKey: "EditorFontName")
        onChange()
    }

    @objc private func saveFontSize(_ sender: Any?) {
        UserDefaults.standard.set(fontSize.doubleValue, forKey: "EditorFontSize")
        updateFontSizeLabel()
        onChange()
    }

    @objc private func saveLineSpacing(_ sender: Any?) {
        UserDefaults.standard.set(lineSpacing.doubleValue, forKey: "EditorLineSpacing")
        updateLineSpacingLabel()
        onChange()
    }

    /// Keys of fields whose value lives in the Keychain, not UserDefaults.
    private static let secretKeys: Set<String> = [AIConnection.Keys.apiKey, AIConnection.Keys.completionApiKey]

    private func configure(_ field: NSTextField, key: String, placeholder: String, label: String) {
        field.placeholderString = placeholder
        field.stringValue = (Self.secretKeys.contains(key) ? AIKeychain.read(key) : UserDefaults.standard.string(forKey: key)) ?? ""
        field.identifier = NSUserInterfaceItemIdentifier(key)
        field.delegate = self
        field.setAccessibilityLabel(label)
        field.translatesAutoresizingMaskIntoConstraints = false
        field.widthAnchor.constraint(equalToConstant: 238).isActive = true
    }

    /// Saves each AI field as it is typed; an empty field falls back to the default.
    func controlTextDidChange(_ obj: Notification) {
        if obj.object as? NSTextField === excludeField { return updateExcludeButtons() }
        let generalKeys = [AIConnection.Keys.baseURL, AIConnection.Keys.apiKey]
        let completionKeys = [AIConnection.Keys.completionBaseURL, AIConnection.Keys.completionApiKey]
        guard let field = obj.object as? NSTextField, let key = field.identifier?.rawValue,
              (generalKeys + completionKeys).contains(key)
        else { return }
        let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if Self.secretKeys.contains(key) {
            // API keys go to the Keychain only, never to the plain-text plist.
            AIKeychain.write(value, account: key)
        } else if value.isEmpty {
            UserDefaults.standard.removeObject(forKey: key)
        } else {
            UserDefaults.standard.set(value, forKey: key)
        }
        // Base URL or key changed: reload that provider's list once typing pauses.
        if completionKeys.contains(key) {
            completionReloadWork?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.loadCompletionModels() }
            completionReloadWork = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.8, execute: work)
        } else {
            modelReloadWork?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.loadModels() }
            modelReloadWork = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.8, execute: work)
        }
    }

    private func updateFontSizeLabel() { fontSizeLabel.stringValue = "\(Int(fontSize.doubleValue)) pt" }
    private func updateLineSpacingLabel() { lineSpacingLabel.stringValue = "\(Int(lineSpacing.doubleValue)) pt" }

    private static func installedFontFamilies() -> [String] {
        NSFontManager.shared.availableFontFamilies
            .filter { $0 != ".AppleSystemUIFont" && $0 != ".SFNSDisplay" && NSFont(name: $0, size: 13) != nil }
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    }
}

extension PreferencesWindowController: NSWindowDelegate {
    func windowDidBecomeKey(_ notification: Notification) { syncControls() }
}

extension PreferencesWindowController: NSTableViewDataSource, NSTableViewDelegate {
    func numberOfRows(in tableView: NSTableView) -> Int { excludeEntries.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let id = tableColumn?.identifier.rawValue, excludeEntries.indices.contains(row) else { return nil }
        let entry = excludeEntries[row]
        switch id {
        case "hide":
            let box = NSButton(checkboxWithTitle: "", target: self, action: #selector(toggleFilesExclude(_:)))
            box.state = entry.hidden ? .on : .off
            box.tag = row
            box.isEnabled = tableView.isEnabled
            box.setAccessibilityLabel("Hide \(entry.glob)")
            return centered(box)
        case "glob":
            let label = NSTextField(labelWithString: entry.glob)
            label.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
            label.lineBreakMode = .byTruncatingMiddle
            label.toolTip = entry.glob
            label.textColor = entry.hidden ? .labelColor : .secondaryLabelColor
            return centered(label)
        default:
            let label = NSTextField(labelWithString: entry.isDefault ? "Built-in" : "Custom")
            label.font = .systemFont(ofSize: 11)
            label.textColor = .tertiaryLabelColor
            label.alignment = .right
            return centered(label)
        }
    }

    /// Wraps a cell's control so it sits in the middle of the row.
    private func centered(_ view: NSView) -> NSView {
        let cell = NSTableCellView()
        view.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(view)
        NSLayoutConstraint.activate([
            view.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            view.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
            view.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor, constant: -2),
        ])
        if view is NSTextField, (view as? NSTextField)?.alignment == .right {
            view.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4).isActive = true
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) { updateExcludeButtons() }
}

/// Toolbar tabs whose window grows or shrinks to the selected pane, keeping
/// the title bar in place, like the system Settings windows.
@MainActor
final class SettingsTabViewController: NSTabViewController {
    override func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        super.tabView(tabView, didSelect: tabViewItem)
        fitWindow(to: tabViewItem, animate: view.window?.isVisible ?? false)
    }

    func fitWindow(to item: NSTabViewItem?, animate: Bool = false) {
        guard let window = view.window, let pane = item?.viewController?.view else { return }
        let size = pane.fittingSize
        let contentRect = window.contentRect(forFrameRect: window.frame)
        // The toolbar sits outside the content view; keep the top edge fixed.
        let chrome = window.frame.height - contentRect.height
        var frame = window.frame
        frame.size = NSSize(width: max(size.width, 440), height: size.height + chrome)
        frame.origin.y = window.frame.maxY - frame.height
        window.setFrame(frame, display: true, animate: animate)
    }
}
