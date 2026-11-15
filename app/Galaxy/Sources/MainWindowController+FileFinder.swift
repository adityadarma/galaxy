import AppKit
import os

extension MainWindowController {
    /// Walks the workspace off the main thread and installs the result if
    /// the workspace is still the same.
    func rebuildFileFinder() {
        guard let workspace else { return }
        finderGeneration += 1
        let token = finderGeneration
        Task.detached(priority: .userInitiated) { [weak self] in
            let finder = workspace.buildFileFinder()
            await MainActor.run {
                guard let self, self.finderGeneration == token else { return }
                finder.setContext(current: self.activeTab.flatMap { self.relativePath($0.doc.path()) },
                                  recent: self.recentFiles)
                self.fileFinder = finder
                self.loadedQuickOpen?.placeholderMessage = "Indexing files…"
                self.loadedQuickOpen?.finder = finder
                self.onFileFinderReady?()
            }
        }
    }

    /// Updates the Go to File index for changed paths, off the main thread.
    func updateFileFinder(changed paths: [String], then done: (@MainActor @Sendable () -> Void)? = nil) {
        guard let workspace else { done?(); return }
        // Still being built: the walk will see the change, or a restart will.
        guard let finder = fileFinder else {
            rebuildFileFinder()
            done?()
            return
        }
        let token = finderGeneration
        Task.detached(priority: .utility) { [weak self] in
            let updated = finder.applyChanges(workspace: workspace, changed: paths)
            await MainActor.run {
                defer { done?() }
                guard let self, self.finderGeneration == token, updated, self.isQuickOpenVisible else { return }
                self.quickOpen.runSearch(keepSelection: true)
            }
        }
    }

    /// Starts watching `workspace`; batches come back on the main actor.
    func startWatching(_ workspace: Workspace) {
        workspaceWatch?.stop()
        workspaceWatch = nil
        watchGeneration += 1
        let sink = WatchSink(controller: self, generation: watchGeneration)
        do {
            workspaceWatch = try workspace.watch(listener: sink)
        } catch {
            // Not fatal: the explorer and ⌘P still work, refreshed by hand.
            Log.ui.error("watch failed: \(String(describing: error), privacy: .public)")
        }
    }

    /// A watcher batch (M2-02): update the explorer and the file index.
    fileprivate func workspaceChanged(_ batch: WatchBatch, generation: Int) {
        guard generation == watchGeneration, workspace != nil else { return }
        if batch.gitChanged && !batch.rescan && batch.paths.isEmpty {
            refreshGitStatus()
            onWorkspaceChangesApplied?(batch)
            return
        }
        if let workspace {
            bridge.applyWorkspaceChanges(root: workspace.root(), paths: batch.paths, rescan: batch.rescan)
        }
        if batch.rescan {
            explorer.refresh()
            checkOpenFiles()
            rebuildFileFinder()
            refreshGitStatus()
            onWorkspaceChangesApplied?(batch)
            return
        }
        explorer.refresh(changed: batch.paths)
        refreshGitStatus()
        checkOpenFiles(changed: batch.paths)
        updateFileFinder(changed: batch.paths) { [weak self] in
            self?.onWorkspaceChangesApplied?(batch)
        }
    }

    func noteRecent(_ rel: String) {
        recentFiles.removeAll { $0 == rel }
        recentFiles.insert(rel, at: 0)
        if recentFiles.count > 50 { recentFiles.removeLast(recentFiles.count - 50) }
    }

    /// `path` relative to the workspace root, if it is inside it.
    func relativePath(_ path: String) -> String? {
        guard let workspace, let file = Self.fileKey(path), let root = Self.fileKey(workspace.root()) else {
            return nil
        }
        let prefix = root.hasSuffix("/") ? root : root + "/"
        return file.hasPrefix(prefix) ? String(file.dropFirst(prefix.count)) : nil
    }
}

/// Receives watcher batches on the watcher thread and hands them to the
/// window on the main actor. Holds the window weakly.
/// `@unchecked Sendable`: the state is set once in `init`; the weak
/// reference is only dereferenced on the main actor.
private final class WatchSink: WorkspaceListener, @unchecked Sendable {
    private weak var controller: MainWindowController?
    private let generation: Int

    init(controller: MainWindowController, generation: Int) {
        self.controller = controller
        self.generation = generation
    }

    func onChanges(batch: WatchBatch) {
        let generation = generation
        Task { @MainActor [weak controller] in
            controller?.workspaceChanged(batch, generation: generation)
        }
    }
}
