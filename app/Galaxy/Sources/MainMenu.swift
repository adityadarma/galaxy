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
        appMenu.addItem(withTitle: "Check for Updates…", action: #selector(AppDelegate.checkForUpdates(_:)), keyEquivalent: "")
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
        selectionMenu.addItem(.separator())
        selectionMenu.addItem(withTitle: "Copy Line Up", action: #selector(EditorView.copyLinesUp(_:)), keyEquivalent: "")
        selectionMenu.addItem(withTitle: "Copy Line Down", action: #selector(EditorView.copyLinesDown(_:)), keyEquivalent: "")
        selectionMenu.addItem(withTitle: "Move Line Up", action: #selector(EditorView.moveLinesUp(_:)), keyEquivalent: "")
        selectionMenu.addItem(withTitle: "Move Line Down", action: #selector(EditorView.moveLinesDown(_:)), keyEquivalent: "")
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
        goMenu.addItem(withTitle: "Find References", action: #selector(MainWindowController.findReferences(_:)), keyEquivalent: "")
        goMenu.addItem(withTitle: "Rename Symbol…", action: #selector(MainWindowController.renameSymbol(_:)), keyEquivalent: "")
        let codeActions = goMenu.addItem(withTitle: "Code Actions…", action: #selector(MainWindowController.showCodeActions(_:)), keyEquivalent: ".")
        codeActions.keyEquivalentModifierMask = [.command]
        goMenu.addItem(withTitle: "Go to File…", action: #selector(MainWindowController.showGoToFile(_:)), keyEquivalent: "p")
        let symbol = goMenu.addItem(withTitle: "Go to Symbol…", action: #selector(MainWindowController.showGoToSymbol(_:)), keyEquivalent: "o")
        symbol.keyEquivalentModifierMask = [.command, .shift]
        let line = goMenu.addItem(withTitle: "Go to Line…", action: #selector(MainWindowController.showGoToLine(_:)), keyEquivalent: "g")
        line.keyEquivalentModifierMask = [.control]
        goMenu.addItem(.separator())
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
        viewMenu.addItem(withTitle: "Split Editor Right", action: #selector(MainWindowController.splitEditorRight(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Split Editor Down", action: #selector(MainWindowController.splitEditorDown(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Toggle Git Blame", action: #selector(MainWindowController.toggleGitBlame(_:)), keyEquivalent: "")
        let mdPreview = viewMenu.addItem(withTitle: "Toggle Markdown Preview", action: #selector(MainWindowController.toggleMarkdownPreview(_:)), keyEquivalent: "v")
        mdPreview.keyEquivalentModifierMask = [.command, .shift]
        viewMenu.addItem(withTitle: "Close Editor Split", action: #selector(MainWindowController.closeEditorSplit(_:)), keyEquivalent: "")
        let search = viewMenu.addItem(withTitle: "Search in Workspace…", action: #selector(MainWindowController.showSearchSidebar(_:)), keyEquivalent: "f")
        search.keyEquivalentModifierMask = [.command, .shift]
        let explorer = viewMenu.addItem(withTitle: "Show Explorer", action: #selector(MainWindowController.showExplorer(_:)), keyEquivalent: "e")
        explorer.keyEquivalentModifierMask = [.command, .shift]
        let scm = viewMenu.addItem(withTitle: "Show Source Control", action: #selector(MainWindowController.showSourceControlAction(_:)), keyEquivalent: "g")
        scm.keyEquivalentModifierMask = [.control, .shift]
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "Show Problems", action: #selector(MainWindowController.showProblemsPanel(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Show Debug Console", action: #selector(MainWindowController.showDebugConsole(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Show Output", action: #selector(MainWindowController.showOutputPanel(_:)), keyEquivalent: "")
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "Toggle Full Screen", action: #selector(NSWindow.toggleFullScreen(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Zoom In", action: #selector(MainWindowController.zoomEditorIn(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Zoom Out", action: #selector(MainWindowController.zoomEditorOut(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Reset Zoom", action: #selector(MainWindowController.zoomEditorReset(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Toggle Minimap", action: #selector(MainWindowController.toggleMinimap(_:)), keyEquivalent: "")
        let wordWrap = viewMenu.addItem(withTitle: "Toggle Word Wrap", action: #selector(MainWindowController.toggleWordWrap(_:)), keyEquivalent: "z")
        wordWrap.keyEquivalentModifierMask = [.option]
        viewMenu.addItem(withTitle: "Toggle Breadcrumbs", action: #selector(MainWindowController.toggleBreadcrumbs(_:)), keyEquivalent: "")
        viewMenu.addItem(withTitle: "Toggle Sticky Scroll", action: #selector(MainWindowController.toggleStickyScroll(_:)), keyEquivalent: "")
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

        let runItem = NSMenuItem()
        let runMenu = NSMenu(title: "Run")
        runMenu.addItem(withTitle: "Start Debugging", action: #selector(MainWindowController.startDebugging(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Stop Debugging", action: #selector(MainWindowController.stopDebugging(_:)), keyEquivalent: "")
        runMenu.addItem(.separator())
        runMenu.addItem(withTitle: "Step Over", action: #selector(MainWindowController.debugStepOver(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Step Into", action: #selector(MainWindowController.debugStepInto(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Step Out", action: #selector(MainWindowController.debugStepOut(_:)), keyEquivalent: "")
        runMenu.addItem(.separator())
        runMenu.addItem(withTitle: "Toggle Breakpoint", action: #selector(MainWindowController.toggleBreakpoint(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Remove All Breakpoints", action: #selector(MainWindowController.removeAllBreakpoints(_:)), keyEquivalent: "")
        runMenu.addItem(.separator())
        runMenu.addItem(withTitle: "Run Build Task", action: #selector(MainWindowController.runBuildTask(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Run Test Task", action: #selector(MainWindowController.runTestTask(_:)), keyEquivalent: "")
        runMenu.addItem(withTitle: "Download Go Modules", action: #selector(MainWindowController.downloadGoModules(_:)), keyEquivalent: "")
        runItem.submenu = runMenu

        let terminalItem = NSMenuItem()
        let terminalMenu = NSMenu(title: "Terminal")
        terminalMenu.addItem(withTitle: "New Terminal", action: #selector(MainWindowController.createTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "New Terminal in Editor Area", action: #selector(MainWindowController.createTerminalEditor(_:)), keyEquivalent: "")
        terminalMenu.addItem(.separator())
        terminalMenu.addItem(withTitle: "Run Selected Text", action: #selector(MainWindowController.runSelectedTextInTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Run Active File", action: #selector(MainWindowController.runActiveFileInTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(.separator())
        terminalMenu.addItem(withTitle: "Move Terminal into Editor Area", action: #selector(MainWindowController.moveTerminalToEditor(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Move Terminal into Panel", action: #selector(MainWindowController.moveTerminalToPanel(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Move Terminal into New Window", action: #selector(MainWindowController.moveTerminalToNewWindow(_:)), keyEquivalent: "")
        terminalMenu.addItem(.separator())
        terminalMenu.addItem(withTitle: "Split Terminal", action: #selector(MainWindowController.splitTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Rename Terminal\u{2026}", action: #selector(MainWindowController.renameTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Change Terminal Color\u{2026}", action: #selector(MainWindowController.changeTerminalColor(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Change Terminal Icon\u{2026}", action: #selector(MainWindowController.changeTerminalIcon(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Toggle Terminal List Size to Content Width", action: #selector(MainWindowController.toggleTerminalListSize(_:)), keyEquivalent: "")
        terminalMenu.addItem(.separator())
        terminalMenu.addItem(withTitle: "Clear Terminal", action: #selector(MainWindowController.clearActiveTerminal(_:)), keyEquivalent: "")
        terminalMenu.addItem(withTitle: "Kill Terminal", action: #selector(MainWindowController.killTerminal(_:)), keyEquivalent: "")
        terminalItem.submenu = terminalMenu

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
        let toggleComment = editMenu.addItem(withTitle: "Toggle Line Comment", action: #selector(EditorView.toggleComment(_:)), keyEquivalent: "/")
        toggleComment.keyEquivalentModifierMask = .command
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Delete Line", action: #selector(EditorView.deleteLines(_:)), keyEquivalent: "")
        editMenu.addItem(withTitle: "Join Lines", action: #selector(EditorView.joinLines(_:)), keyEquivalent: "")
        editMenu.addItem(withTitle: "Sort Lines Ascending", action: #selector(EditorView.sortLinesAscending(_:)), keyEquivalent: "")
        editMenu.addItem(withTitle: "Sort Lines Descending", action: #selector(EditorView.sortLinesDescending(_:)), keyEquivalent: "")
        editMenu.addItem(withTitle: "Trim Trailing Whitespace", action: #selector(EditorView.trimTrailingWhitespace(_:)), keyEquivalent: "")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Transform to Uppercase", action: #selector(EditorView.transformToUppercase(_:)), keyEquivalent: "")
        editMenu.addItem(withTitle: "Transform to Lowercase", action: #selector(EditorView.transformToLowercase(_:)), keyEquivalent: "")
        editItem.submenu = editMenu
        main.addItem(editItem)
        main.addItem(selectionItem)
        main.addItem(findItem)
        main.addItem(viewItem)
        main.addItem(goItem)
        main.addItem(runItem)
        main.addItem(terminalItem)

        // AppKit fills the Window menu with the open windows, and adds the
        // menu search field to Help.
        let windowItem = NSMenuItem()
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        windowMenu.addItem(.separator())
        windowMenu.addItem(withTitle: "Bring All to Front", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: "")
        windowItem.submenu = windowMenu
        main.addItem(windowItem)
        NSApp?.windowsMenu = windowMenu

        let helpItem = NSMenuItem()
        let helpMenu = NSMenu(title: "Help")
        helpItem.submenu = helpMenu
        main.addItem(helpItem)
        NSApp?.helpMenu = helpMenu

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
