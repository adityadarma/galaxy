import Foundation
import os

/// Owns the Rust `Core` and converts core events into main-actor updates.
///
/// Core callbacks arrive on background threads. `EventSink` forwards them to
/// the main actor so UI code never runs off the main thread.
@MainActor
final class CoreBridge {
    let core: Core
    private(set) var lastEvent: CoreEvent?
    var onEvent: ((CoreEvent) -> Void)?

    init() {
        core = Core()
    }

    var version: String { coreVersion() }

    func start() {
        do {
            let level = ProcessInfo.processInfo.environment["GALAXY_LOG"] ?? "info"
            _ = try initLogging(dir: Log.directory.path, level: level)
        } catch {
            Log.core.error("core logging init failed: \(String(describing: error), privacy: .public)")
        }
        core.setListener(listener: EventSink(bridge: self))
    }

    func ping(_ message: String) {
        core.ping(message: message)
    }

    func newDocument() -> DocumentHandle {
        core.newDocument()
    }

    func javascriptProject(path: String) -> JavaScriptProjectInfo? {
        core.javascriptProject(path: path)
    }

    func javascriptComponents(path: String) -> [JavaScriptComponentInfo] {
        core.javascriptComponents(path: path)
    }

    func resolveJavaScriptRelativeImport(specifier: String, fromPath: String) -> DefinitionLocation? {
        core.resolveJavascriptRelativeImport(specifier: specifier, fromPath: fromPath)
    }

    func openDocument(path: String) throws -> DocumentHandle {
        try core.openDocument(path: path)
    }

    /// True when a built-in engine handles the file.
    @discardableResult
    func registerDocument(document: DocumentHandle, language: String, root: String) -> Bool {
        core.registerDocument(document: document, language: language, root: root)
    }

    /// Drains finished analysis results; drives diagnostics updates.
    func pollAnalysis() {
        _ = core.pollAnalysis()
    }

    /// False when no request went out (server missing or still initializing).
    @discardableResult
    func requestHover(document: DocumentHandle, line: UInt64, character: UInt64) -> Bool {
        core.requestHover(document: document, line: line, character: character)
    }

    func hover(document: DocumentHandle) -> HoverInfo? {
        core.hover(document: document)
    }

    func requestCompletion(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestCompletion(document: document, line: line, character: character)
    }

    func completions(document: DocumentHandle) -> [CompletionInfo] {
        core.completions(document: document)
    }

    func snippetCompletions(document: DocumentHandle) -> [CompletionInfo] {
        document.snippetCompletions()
    }

    func expandedSnippet(body: String, baseIndent: String, indentUnit: String,
                         filename: String, selectedText: String, currentYear: String) -> SnippetExpansionInfo {
        expandSnippet(body: body, baseIndent: baseIndent, indentUnit: indentUnit,
                      filename: filename, selectedText: selectedText, currentYear: currentYear)
    }


    func requestSignatureHelp(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestSignatureHelp(document: document, line: line, character: character)
    }

    func signatureHelp(document: DocumentHandle) -> SignatureInfo? {
        core.signatureHelp(document: document)
    }

    func requestDefinition(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestDefinitionForNavigation(
            document: document, line: line, character: character, navigationToken: "definition"
        )
    }

    func definitionLocations(document: DocumentHandle) -> [DefinitionLocation] {
        core.definitionLocations(document: document)
    }

    func requestReferences(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestReferences(document: document, line: line, character: character)
    }

    func references(document: DocumentHandle) -> [DefinitionLocation] {
        core.references(document: document)
    }

    func requestRename(document: DocumentHandle, line: UInt64, character: UInt64, newName: String) {
        _ = core.requestRename(document: document, line: line, character: character, newName: newName)
    }

    func renameEdits(document: DocumentHandle) -> [RenameEdit] {
        core.renameEdits(document: document)
    }

    func applyRenameEdits(document: DocumentHandle, edits: [RenameEdit]) -> Bool {
        document.applyRenameEdits(edits: edits)
    }

    func save(document: DocumentHandle) throws {
        try document.save()
    }

    func setTagEditing(document: DocumentHandle, autoClose: Bool, autoRename: Bool) {
        document.setTagEditing(autoClose: autoClose, autoRename: autoRename)
    }

    func setIndent(document: DocumentHandle, useTabs: Bool, tabSize: UInt32) {
        document.setIndent(useTabs: useTabs, tabSize: tabSize)
    }

    func requestCodeActions(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestCodeActions(document: document, line: line, character: character)
    }

    func codeActions(document: DocumentHandle) -> [CodeActionInfo] {
        core.codeActions(document: document)
    }

    func requestDocumentSymbols(document: DocumentHandle) {
        _ = core.requestDocumentSymbols(document: document)
    }

    func documentSymbols(document: DocumentHandle) -> [LspOutlineSymbol] {
        core.documentSymbols(document: document)
    }

    func search(workspace: Workspace, query: String, options: WorkspaceSearchOptions, limit: UInt64) throws -> [WorkspaceSearchMatch] {
        try workspace.search(query: query, options: options, limit: limit)
    }

    func gitStatus(workspace: Workspace) -> GitStatusInfo? {
        try? workspace.gitStatus()
    }

    func gitBranches(workspace: Workspace) -> [GitBranchInfo] {
        (try? workspace.gitBranches()) ?? []
    }

    func gitLogGraph(workspace: Workspace, limit: UInt32 = 500) -> [GitCommitInfo] {
        (try? workspace.gitLogGraph(limit: limit)) ?? []
    }

    func gitCheckout(workspace: Workspace, name: String) -> Bool {
        (try? workspace.gitCheckout(name: name)) != nil
    }

    func gitCreateBranch(workspace: Workspace, name: String) -> Bool {
        (try? workspace.gitCreateBranch(name: name)) != nil
    }

    func gitStage(workspace: Workspace, path: String) -> Bool {
        (try? workspace.gitStage(path: path)) != nil
    }

    func gitStageAll(workspace: Workspace) -> Bool {
        (try? workspace.gitStageAll()) != nil
    }

    func gitUnstage(workspace: Workspace, path: String) -> Bool {
        (try? workspace.gitUnstage(path: path)) != nil
    }

    func gitUnstageAll(workspace: Workspace) -> Bool {
        (try? workspace.gitUnstageAll()) != nil
    }

    func gitCommit(workspace: Workspace, message: String) -> Bool {
        (try? workspace.gitCommit(message: message)) != nil
    }

    func gitDiffLines(workspace: Workspace, path: String, text: String) -> [GitLineChangeInfo] {
        (try? workspace.gitDiffLines(relPath: path, text: text)) ?? []
    }

    func terminal(workspace: Workspace, cols: UInt16, rows: UInt16) -> TerminalHandle? {
        try? workspace.terminal(cols: cols, rows: rows)
    }

    fileprivate func deliver(_ event: CoreEvent) {
        lastEvent = event
        if case let .pong(message) = event {
            Log.core.debug("pong: \(message, privacy: .public)")
        }
        onEvent?(event)
    }
}

/// Listener passed to Rust. Holds the bridge weakly to avoid a retain cycle
/// (bridge -> core -> listener -> bridge).
/// `@unchecked Sendable`: the only state is a weak reference that is set once
/// in `init` and only dereferenced on the main actor.
private final class EventSink: CoreListener, @unchecked Sendable {
    private weak var bridge: CoreBridge?

    init(bridge: CoreBridge) {
        self.bridge = bridge
    }

    func onEvent(event: CoreEvent) {
        Task { @MainActor [weak bridge] in
            bridge?.deliver(event)
        }
    }
}
