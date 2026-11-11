import AppKit

extension MainWindowController {
    @objc func refreshRustGeneratedSources(_ sender: Any?) {
        guard let workspace else {
            statusLabel.stringValue = "Open a workspace to refresh Rust generated sources."
            return
        }
        let root = workspace.root()
        if rustGeneratedSourceRefresh?.root == root || rustCompilerRefreshWorkItem != nil {
            rustCompilerRefreshGeneration += 1
            rustCompilerRefreshWorkItem?.cancel()
            rustCompilerRefreshWorkItem = nil
            statusLabel.stringValue = bridge.cancelGeneratedRustSourceRefresh(root: root)
                ? "Canceling Rust generated source refresh..."
                : "Rust generated source refresh is no longer running."
            return
        }
        let settingsURL = GalaxySettings.workspaceURL(root: workspace.root())
        let enabled = (try? GalaxySettings.load(from: settingsURL).rustCompilerRefreshEnabled) ?? false
        if !enabled {
            let alert = NSAlert()
            alert.messageText = "Enable Rust Generated Source Refresh?"
            alert.informativeText = "This runs Cargo in this workspace. Cargo build scripts and procedural macros may execute arbitrary workspace or dependency code."
            alert.addButton(withTitle: "Enable & Refresh")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            do {
                try GalaxySettings.write(true, key: "compilerRefreshEnabled", section: "rust", to: settingsURL)
            } catch {
                report(error, while: "enable Rust generated source refresh")
                return
            }
        }
        let requestID = UUID()
        rustGeneratedSourceRefresh = (root, requestID, false)
        statusLabel.stringValue = "Refreshing Rust generated sources... Run Refresh Generated Rust Sources again to cancel."
        bridge.refreshGeneratedRustSources(root: root) { [weak self, weak workspace] result in
            guard let self, let workspace, self.workspace === workspace,
                  self.rustGeneratedSourceRefresh?.id == requestID else { return }
            self.rustGeneratedSourceRefresh = nil
            self.statusLabel.stringValue = result.message
            self.scheduleParameterHints(after: 0)
        }
    }

    func scheduleRustCompilerRefresh(afterSaving doc: DocumentHandle) {
        guard let workspace, URL(fileURLWithPath: doc.path()).pathExtension == "rs",
              (try? GalaxySettings.load(from: GalaxySettings.workspaceURL(root: workspace.root())).rustCompilerRefreshEnabled) == true,
              rustGeneratedSourceRefresh?.automatic != false else { return }
        rustCompilerRefreshGeneration += 1
        let generation = rustCompilerRefreshGeneration
        let root = workspace.root()
        rustCompilerRefreshWorkItem?.cancel()
        if rustGeneratedSourceRefresh?.automatic == true {
            _ = bridge.cancelGeneratedRustSourceRefresh(root: root)
        }
        let item = DispatchWorkItem { [weak self, weak workspace] in
            guard let self, let workspace, self.workspace === workspace,
                  self.rustCompilerRefreshGeneration == generation,
                  self.rustGeneratedSourceRefresh == nil else { return }
            self.rustCompilerRefreshWorkItem = nil
            let requestID = UUID()
            self.rustGeneratedSourceRefresh = (root, requestID, true)
            self.statusLabel.stringValue = "Rust: checking after save..."
            self.bridge.refreshGeneratedRustSources(root: root) { [weak self, weak workspace] result in
                guard let self, let workspace, self.workspace === workspace,
                      self.rustCompilerRefreshGeneration == generation,
                      self.rustGeneratedSourceRefresh?.id == requestID else { return }
                self.rustGeneratedSourceRefresh = nil
                self.statusLabel.stringValue = result.message
                self.scheduleParameterHints(after: 0)
            }
        }
        rustCompilerRefreshWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5, execute: item)
    }
}
