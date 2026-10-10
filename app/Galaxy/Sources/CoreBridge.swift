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

    func openDocument(path: String) throws -> DocumentHandle {
        try core.openDocument(path: path)
    }

    @discardableResult
    func startLSP(document: DocumentHandle, language: String, root: String) -> Bool {
        (try? core.startLsp(document: document, language: language, root: root)) ?? false
    }

    func pollLSP() {
        _ = core.pollLsp()
    }

    func syncLSP(document: DocumentHandle) {
        core.syncLspDocument(document: document)
    }

    func saveLSP(document: DocumentHandle) {
        core.saveLspDocument(document: document)
    }

    func requestHover(document: DocumentHandle, line: UInt64, character: UInt64) {
        _ = core.requestHover(document: document, line: line, character: character)
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
