import AppKit

@MainActor
enum MainMenu {
    static func build() -> NSMenu {
        let main = NSMenu()

        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About Galaxy",
                        action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                        keyEquivalent: "")
        let preferences = appMenu.addItem(withTitle: "Settings…", action: #selector(AppDelegate.showPreferences(_:)), keyEquivalent: ",")
        preferences.keyEquivalentModifierMask = [.command]
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Hide Galaxy", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        let hideOthers = appMenu.addItem(withTitle: "Hide Others",
                                         action: #selector(NSApplication.hideOtherApplications(_:)),
                                         keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit Galaxy", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        main.addItem(appItem)

        let fileItem = NSMenuItem()
        let fileMenu = NSMenu(title: "File")
        let newWindow = fileMenu.addItem(withTitle: "New Window", action: #selector(AppDelegate.newWindow(_:)), keyEquivalent: "n")
        newWindow.keyEquivalentModifierMask = [.command, .shift]
        fileMenu.addItem(withTitle: "New", action: #selector(MainWindowController.newDocument(_:)), keyEquivalent: "n")
        fileMenu.addItem(withTitle: "Open File or Folder…", action: #selector(MainWindowController.openDocument(_:)), keyEquivalent: "o")
        let openFolder = fileMenu.addItem(withTitle: "Open Folder…", action: #selector(MainWindowController.openFolder(_:)), keyEquivalent: "o")
        openFolder.keyEquivalentModifierMask = [.command, .shift]
        let recent = NSMenuItem(title: "Recent Projects", action: nil, keyEquivalent: "")
        let recentMenu = NSMenu(title: "Recent Projects")
        recentMenu.identifier = NSUserInterfaceItemIdentifier("recent-projects")
        populateRecentProjects(recentMenu)
        recent.submenu = recentMenu
        fileMenu.addItem(recent)
        fileMenu.addItem(.separator())
        fileMenu.addItem(withTitle: "Save", action: #selector(MainWindowController.saveDocument(_:)), keyEquivalent: "s")
        let saveAs = fileMenu.addItem(withTitle: "Save As…", action: #selector(MainWindowController.saveDocumentAs(_:)), keyEquivalent: "s")
        saveAs.keyEquivalentModifierMask = [.command, .shift]
        fileMenu.addItem(.separator())
        fileMenu.addItem(withTitle: "Close Tab", action: #selector(MainWindowController.closeTab(_:)), keyEquivalent: "w")
        let closeWindow = fileMenu.addItem(withTitle: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        closeWindow.keyEquivalentModifierMask = [.command, .shift]
        fileItem.submenu = fileMenu
        main.addItem(fileItem)

        let selectionItem = NSMenuItem()
        let selectionMenu = NSMenu(title: "Selection")
        selectionMenu.addItem(withTitle: "Add Next Occurrence", action: #selector(EditorView.selectNextOccurrence(_:)), keyEquivalent: "d")
        let allOccurrences = selectionMenu.addItem(withTitle: "Select All Occurrences", action: #selector(EditorView.selectAllOccurrences(_:)), keyEquivalent: "l")
        allOccurrences.keyEquivalentModifierMask = [.command, .shift]
        let above = selectionMenu.addItem(withTitle: "Add Cursor Above", action: #selector(EditorView.addCursorAbove(_:)), keyEquivalent: "\u{F700}")
        above.keyEquivalentModifierMask = [.command, .option]
        let below = selectionMenu.addItem(withTitle: "Add Cursor Below", action: #selector(EditorView.addCursorBelow(_:)), keyEquivalent: "\u{F701}")
        below.keyEquivalentModifierMask = [.command, .option]
        selectionItem.submenu = selectionMenu

        let findItem = NSMenuItem()
        let findMenu = NSMenu(title: "Find")
        findMenu.addItem(withTitle: "Find…", action: #selector(MainWindowController.showFind(_:)), keyEquivalent: "f")
        let replace = findMenu.addItem(withTitle: "Find and Replace…", action: #selector(MainWindowController.showFindAndReplace(_:)), keyEquivalent: "f")
        replace.keyEquivalentModifierMask = [.command, .option]
        findMenu.addItem(.separator())
        findMenu.addItem(withTitle: "Find Next", action: #selector(MainWindowController.findNext(_:)), keyEquivalent: "g")
        let previous = findMenu.addItem(withTitle: "Find Previous", action: #selector(MainWindowController.findPrevious(_:)), keyEquivalent: "g")
        previous.keyEquivalentModifierMask = [.command, .shift]
        findMenu.addItem(withTitle: "Use Selection for Find", action: #selector(MainWindowController.useSelectionForFind(_:)), keyEquivalent: "e")
        findItem.submenu = findMenu

        let goItem = NSMenuItem()
        let goMenu = NSMenu(title: "Go")
        goMenu.addItem(withTitle: "Go to Definition", action: #selector(MainWindowController.goToDefinition(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Go to Type Definition", action: #selector(MainWindowController.goToTypeDefinition(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Go to Implementation", action: #selector(MainWindowController.goToImplementation(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Find References", action: #selector(MainWindowController.findReferences(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Rename Symbol…", action: #selector(MainWindowController.renameSymbol(_:)), keyEquivalent: "")
        let codeActions = goMenu.addItem(withTitle: "Code Actions…", action: #selector(MainWindowController.showCodeActions(_:)), keyEquivalent: ".")
        codeActions.keyEquivalentModifierMask = [.command]
        let format = goMenu.addItem(withTitle: "Format Document", action: #selector(MainWindowController.formatDocument(_:)), keyEquivalent: "f")
        format.keyEquivalentModifierMask = [.command, .option, .shift]
        goMenu.addItem(withTitle: "Go to File…", action: #selector(MainWindowController.showGoToFile(_:)), keyEquivalent: "p")
        let symbol = goMenu.addItem(withTitle: "Go to Symbol…", action: #selector(MainWindowController.showGoToSymbol(_:)), keyEquivalent: "o")
        symbol.keyEquivalentModifierMask = [.command, .shift]
        let workspaceSymbol = goMenu.addItem(withTitle: "Go to Workspace Symbol…", action: #selector(MainWindowController.showGoToWorkspaceSymbol(_:)), keyEquivalent: "t")
        workspaceSymbol.keyEquivalentModifierMask = [.command]
        let line = goMenu.addItem(withTitle: "Go to Line…", action: #selector(MainWindowController.showGoToLine(_:)), keyEquivalent: "g")
        line.keyEquivalentModifierMask = [.control]
        goMenu.addItem(.separator())
        goMenu.addItem(withTitle: "Restart Language Server", action: #selector(MainWindowController.restartLanguageServer(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Stop Language Server", action: #selector(MainWindowController.stopLanguageServer(_:)), keyEquivalent: "")
        // The window's key monitor handles both keys; these show them.
        let back = goMenu.addItem(withTitle: "Back", action: #selector(MainWindowController.navigateBack(_:)), keyEquivalent: "-")
        back.keyEquivalentModifierMask = [.control]
        let forward = goMenu.addItem(withTitle: "Forward", action: #selector(MainWindowController.navigateForward(_:)), keyEquivalent: "-")
        forward.keyEquivalentModifierMask = [.control, .shift]
        goItem.submenu = goMenu

        let viewItem = NSMenuItem()
        let viewMenu = NSMenu(title: "View")
        let palette = viewMenu.addItem(withTitle: "Command Palette…", action: #selector(MainWindowController.showCommandPalette(_:)), keyEquivalent: "p")
        palette.keyEquivalentModifierMask = [.command, .shift]
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "Toggle Sidebar", action: #selector(MainWindowController.toggleSidebarVisibility(_:)), keyEquivalent: "b")
        let terminal = viewMenu.addItem(withTitle: "Toggle Terminal", action: #selector(MainWindowController.toggleTerminal(_:)), keyEquivalent: "j")
        terminal.keyEquivalentModifierMask = [.command]
        viewMenu.addItem(withTitle: "Toggle Git Graph", action: #selector(MainWindowController.toggleGitGraph(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Move Terminal into Editor Area", action: #selector(MainWindowController.moveTerminalToEditor(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Move Terminal into Panel", action: #selector(MainWindowController.moveTerminalToPanel(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "New Terminal in Editor Area", action: #selector(MainWindowController.createTerminalEditor(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Split Editor Right", action: #selector(MainWindowController.splitEditorRight(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Split Editor Down", action: #selector(MainWindowController.splitEditorDown(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Toggle Git Blame", action: #selector(MainWindowController.toggleGitBlame(_:)), keyEquivalent: "")
        let mdPreview = viewMenu.addItem(withTitle: "Toggle Markdown Preview", action: #selector(MainWindowController.toggleMarkdownPreview(_:)), keyEquivalent: "v")
        mdPreview.keyEquivalentModifierMask = [.command, .shift]
        viewMenu.addItem(withTitle: "Close Editor Split", action: #selector(MainWindowController.closeEditorSplit(_:)), keyEquivalent: "")
        let search = viewMenu.addItem(withTitle: "Search in Workspace…", action: #selector(MainWindowController.showWorkspaceSearch(_:)), keyEquivalent: "f")
        search.keyEquivalentModifierMask = [.command, .shift]
        let explorer = viewMenu.addItem(withTitle: "Show Explorer", action: #selector(MainWindowController.showExplorer(_:)), keyEquivalent: "e")
        explorer.keyEquivalentModifierMask = [.command, .shift]
        let scm = viewMenu.addItem(withTitle: "Show Source Control", action: #selector(MainWindowController.showSourceControlAction(_:)), keyEquivalent: "g")
        scm.keyEquivalentModifierMask = [.control, .shift]
        let extensionsItem = viewMenu.addItem(withTitle: "Show Extensions", action: #selector(MainWindowController.showExtensionsAction(_:)), keyEquivalent: "x")
        extensionsItem.keyEquivalentModifierMask = [.command, .shift]
        viewMenu.addItem(.separator())
        let nextTab = viewMenu.addItem(withTitle: "Show Next Tab", action: #selector(MainWindowController.showNextTab(_:)), keyEquivalent: "]")
        nextTab.keyEquivalentModifierMask = [.command, .shift]
        let previousTab = viewMenu.addItem(withTitle: "Show Previous Tab", action: #selector(MainWindowController.showPreviousTab(_:)), keyEquivalent: "[")
        previousTab.keyEquivalentModifierMask = [.command, .shift]
        // The window's key monitor handles ⌃Tab; the item shows it.
        let lastTab = viewMenu.addItem(withTitle: "Switch to Last Tab", action: #selector(MainWindowController.selectLastUsedTab(_:)), keyEquivalent: "\t")
        lastTab.keyEquivalentModifierMask = [.control]
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "Show Typing Latency", action: #selector(MainWindowController.showTypingLatency(_:)), keyEquivalent: "")
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "Keyboard Shortcuts…", action: #selector(MainWindowController.openKeybindingsFile(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Open User Settings (JSON)…", action: #selector(MainWindowController.openUserSettingsJson(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Open Workspace Settings (JSON)…", action: #selector(MainWindowController.openWorkspaceSettingsJson(_:)), keyEquivalent: "")
        viewItem.submenu = viewMenu

        let editItem = NSMenuItem()
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(withTitle: "Undo", action: #selector(EditorView.undoEdit(_:)), keyEquivalent: "z")
        let redo = editMenu.addItem(withTitle: "Redo", action: #selector(EditorView.redoEdit(_:)), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: #selector(EditorView.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(EditorView.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: #selector(EditorView.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Select All", action: #selector(EditorView.selectAll(_:)), keyEquivalent: "a")
        editMenu.addItem(withTitle: "Toggle Line Comment", action: #selector(EditorView.toggleComment(_:)), keyEquivalent: "/")
        editItem.submenu = editMenu
        main.addItem(editItem)
        main.addItem(selectionItem)
        main.addItem(findItem)
        main.addItem(viewItem)
        main.addItem(goItem)

        return main
    }

    static func refreshRecentProjects() {
        guard let menu = findRecentProjectsMenu(in: NSApp.mainMenu) else { return }
        populateRecentProjects(menu)
    }

    private static func populateRecentProjects(_ menu: NSMenu) {
        menu.removeAllItems()
        for path in MainWindowController.recentProjectPaths() {
            let item = NSMenuItem(title: URL(fileURLWithPath: path).lastPathComponent,
                                  action: #selector(MainWindowController.openRecentProject(_:)),
                                  keyEquivalent: "")
            item.representedObject = path
            item.toolTip = path
            menu.addItem(item)
        }
        if menu.items.isEmpty {
            let empty = NSMenuItem(title: "No Recent Projects", action: nil, keyEquivalent: "")
            empty.isEnabled = false
            menu.addItem(empty)
        }
    }

    private static func findRecentProjectsMenu(in menu: NSMenu?) -> NSMenu? {
        guard let menu else { return nil }
        for item in menu.items {
            if item.submenu?.identifier == NSUserInterfaceItemIdentifier("recent-projects") {
                return item.submenu
            }
            if let found = findRecentProjectsMenu(in: item.submenu) { return found }
        }
        return nil
    }
}
