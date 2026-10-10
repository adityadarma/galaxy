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
        fileMenu.addItem(withTitle: "New", action: #selector(MainWindowController.newDocument(_:)), keyEquivalent: "n")
        fileMenu.addItem(withTitle: "Open File or Folder…", action: #selector(MainWindowController.openDocument(_:)), keyEquivalent: "o")
        let openFolder = fileMenu.addItem(withTitle: "Open Folder…", action: #selector(MainWindowController.openFolder(_:)), keyEquivalent: "o")
        openFolder.keyEquivalentModifierMask = [.command, .shift]
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
        goMenu.addItem(withTitle: "Go to File…", action: #selector(MainWindowController.showGoToFile(_:)), keyEquivalent: "p")
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
        let explorer = viewMenu.addItem(withTitle: "Show Explorer", action: #selector(MainWindowController.showExplorer(_:)), keyEquivalent: "e")
        explorer.keyEquivalentModifierMask = [.command, .shift]
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
}
