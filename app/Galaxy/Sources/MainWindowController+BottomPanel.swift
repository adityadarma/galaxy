import AppKit
import os

extension MainWindowController {
    @objc func toggleTerminal(_ sender: Any?) {
        setBottomPanelOpen(!isBottomPanelVisible)
        if isBottomPanelVisible { showBottomPanel(tab: bottomPanelTab) }
    }

    /// Opens or closes the panel by resizing it. Panels stay in the view
    /// hierarchy: adding and removing views while AppKit updates tracking
    /// areas crashed (`_NSViewSubviewMutationSafeApply`).
    func setBottomPanelOpen(_ open: Bool) {
        if !open {
            isBottomPanelMaximized = false
            bottomPanelHeader.setMaximized(false)
            // Typing goes back to the editor, as in VS Code.
            if terminalPanel.owns(window?.firstResponder) { window?.makeFirstResponder(editorView) }
            outputPanel.stop()
            portsPanel.stop()
        }
        bottomPanelHeight.constant = open ? currentBottomPanelHeight() : 0
        bottomPanelHeaderHeight.constant = open ? BottomPanelHeader.height : 0
        bottomPanelHeader.isHidden = !open
        bottomPanelContent.isHidden = !open
        // A card of its own, apart from the editor card by the card gap.
        bottomPanelCard.isHidden = !open
        bottomPanelGap.constant = open ? -Theme.cardGap : 0
        titleBar.setPanelVisible(open)
        detailView.needsLayout = true
    }

    private func currentBottomPanelHeight() -> CGFloat {
        guard isBottomPanelMaximized else { return bottomPanelOpenHeight }
        // Leave the tab strip and a sliver of editor, like VS Code.
        let available = detailView.bounds.height - Theme.tabHeight - BottomPanelHeader.height - Theme.cardGap - 40
        return max(bottomPanelOpenHeight, available)
    }

    var bottomPanelHeaderForTesting: BottomPanelHeader { bottomPanelHeader }
    var bottomPanelContentHeight: CGFloat { bottomPanelHeight.constant }

    /// Drag on the panel's top edge. The size is kept for next time.
    func resizeBottomPanel(began: Bool, deltaY: CGFloat) {
        if began { resizeStartHeight = bottomPanelHeight.constant; return }
        setBottomPanelHeight(resizeStartHeight + deltaY)
    }

    /// Sets the open panel's content height, leaving the editor some room.
    func setBottomPanelHeight(_ height: CGFloat) {
        let maximum = max(Self.minBottomPanelHeight,
                          detailView.bounds.height - Theme.tabHeight - BottomPanelHeader.height - Theme.cardGap - 40)
        let clamped = min(max(height, Self.minBottomPanelHeight), maximum)
        isBottomPanelMaximized = false
        bottomPanelHeader.setMaximized(false)
        bottomPanelOpenHeight = clamped
        UserDefaults.standard.set(Double(clamped), forKey: "BottomPanelHeight")
        if isBottomPanelVisible { bottomPanelHeight.constant = clamped }
        detailView.needsLayout = true
    }

    /// Panel children are laid out at least this tall; a shorter panel
    /// would cut off the terminal's bottom rows.
    static let minBottomPanelHeight: CGFloat = BottomPanelContentView.minimumChildHeight

    func toggleBottomPanelMaximized() {
        isBottomPanelMaximized.toggle()
        bottomPanelHeader.setMaximized(isBottomPanelMaximized)
        bottomPanelHeight.constant = currentBottomPanelHeight()
        detailView.needsLayout = true
    }

    func showBottomPanel(tab: BottomPanelHeader.Tab) {
        bottomPanelTab = tab
        bottomPanelHeader.select(tab)
        terminalPanel.isHidden = tab != .terminal
        problemsPanel.isHidden = tab != .problems
        outputPanel.isHidden = tab != .output
        portsPanel.isHidden = tab != .ports
        debugConsole.isHidden = tab != .debugConsole
        if tab == .terminal {
            ensureTerminal()
            window?.makeFirstResponder(terminalPanel.activePane)
        }
        if tab == .output { outputPanel.start() } else { outputPanel.stop() }
        if tab == .ports { portsPanel.start() } else { portsPanel.stop() }
    }
}
