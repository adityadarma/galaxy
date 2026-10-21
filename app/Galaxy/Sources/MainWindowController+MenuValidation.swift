import AppKit
import os

extension MainWindowController: NSMenuItemValidation {
    /// Disables menu items (and palette commands) that cannot do anything
    /// right now.
    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        let hasDocument = editorView.document != nil && activeTab?.isDocument != false
        switch item.action {
        case #selector(saveDocument(_:)), #selector(saveDocumentAs(_:)),
             #selector(showFind(_:)), #selector(showFindAndReplace(_:)),
             #selector(findNext(_:)), #selector(findPrevious(_:)), #selector(useSelectionForFind(_:)):
            return hasDocument
        case #selector(refreshExplorer(_:)), #selector(collapseExplorerFolders(_:)),
             #selector(explorerNewFile(_:)), #selector(explorerNewFolder(_:)),
             #selector(revealInFinder(_:)), #selector(copyExplorerPath(_:)),
             #selector(copyExplorerRelativePath(_:)):
            return workspace != nil
        // From a key or the palette (a tab menu sets its own state).
        case #selector(closeOtherTabs(_:)):
            return tabs.contains { $0 !== activeTab && !$0.isPinned }
        case #selector(closeTabsToTheRight(_:)):
            return activeIndex.map { i in tabs[(i + 1)...].contains { !$0.isPinned } } ?? false
        case #selector(closeAllTabs(_:)):
            return tabs.contains { !$0.isPinned }
        case #selector(closeSavedTabs(_:)):
            return !tabs.isEmpty
        case #selector(copyTabPath(_:)), #selector(revealTabInFinder(_:)):
            return activeTab?.isDocument == true && activeTab?.fileKey != nil
        case #selector(copyTabRelativePath(_:)), #selector(revealTabInExplorer(_:)):
            return activeTab?.isDocument == true && activeTab?.relPath != nil
        case #selector(renameExplorerItem(_:)), #selector(deleteExplorerItem(_:)):
            return workspace != nil && explorer.selectedEntry != nil
        case #selector(closeTab(_:)):
            return true
        case #selector(closeEditorSplit(_:)):
            return groups.count > 1
        case #selector(focusFirstEditorGroup(_:)):
            return groups.count > 1
        case #selector(focusSecondEditorGroup(_:)):
            return groups.count > 1
        case #selector(focusThirdEditorGroup(_:)):
            return groups.count > 2
        case #selector(moveEditorToNextGroup(_:)), #selector(moveEditorToPreviousGroup(_:)):
            return activeTab != nil
        case #selector(splitEditorRight(_:)), #selector(splitEditorDown(_:)):
            return activeTab?.isDocument == true
        case #selector(moveTerminalToEditor(_:)), #selector(createTerminalEditor(_:)), #selector(createTerminal(_:)),
             #selector(runBuildTask(_:)), #selector(runTestTask(_:)):
            return workspace != nil
        case #selector(splitTerminal(_:)):
            return workspace != nil
        case #selector(moveTerminalToNewWindow(_:)), #selector(renameTerminal(_:)), #selector(changeTerminalColor(_:)),
             #selector(changeTerminalIcon(_:)), #selector(toggleTerminalListSize(_:)):
            return (item.representedObject as? PanelTerminal ?? activeTerminal) != nil
        case #selector(killTerminal(_:)), #selector(clearActiveTerminal(_:)):
            return activeTerminalHandle != nil
        case #selector(runSelectedTextInTerminal(_:)), #selector(runActiveFileInTerminal(_:)):
            return workspace != nil && activeTab?.isDocument == true
        case #selector(moveTerminalToPanel(_:)):
            return ((item.representedObject as? EditorTab) ?? activeTab)?.terminal != nil
        case #selector(showNextTab(_:)), #selector(showPreviousTab(_:)), #selector(selectLastUsedTab(_:)):
            return tabs.count > 1
        case #selector(toggleGitBlame(_:)):
            item.state = gitBlameEnabled ? .on : .off
            return true
        case #selector(startDebugging(_:)):
            item.title = debugger.state == .paused ? "Continue" : "Start Debugging"
            return workspace != nil && (debugger.state == .idle || debugger.state == .paused)
        case #selector(stopDebugging(_:)):
            return debugger.isActive
        case #selector(debugStepOver(_:)), #selector(debugStepInto(_:)), #selector(debugStepOut(_:)):
            return debugger.state == .paused
        case #selector(toggleBreakpoint(_:)):
            return activeTab?.isDocument == true && activeTab?.fileKey != nil
        case #selector(removeAllBreakpoints(_:)):
            return !debugger.breakpoints.isEmpty
        case #selector(toggleMinimap(_:)):
            item.state = minimapEnabled ? .on : .off
            return true
        case #selector(toggleWordWrap(_:)):
            item.state = wordWrapEnabled ? .on : .off
            return true
        case #selector(toggleBreadcrumbs(_:)):
            item.state = breadcrumbBar.isEnabled ? .on : .off
            return true
        case #selector(toggleStickyScroll(_:)):
            item.state = stickyScopeView.isEnabled ? .on : .off
            return true
        case #selector(toggleMarkdownPreview(_:)): return isMarkdownPreviewOn || canPreviewMarkdown
        case #selector(navigateBack(_:)): return navHistory.canGoBack()
        case #selector(navigateForward(_:)): return navHistory.canGoForward()
        default: return true
        }
    }
}
