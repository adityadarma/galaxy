import Sparkle

/// Owns Sparkle for the application lifetime. Development builds omit the
/// public key, which deliberately leaves update checks unavailable.
@MainActor
final class AppUpdater {
    private let controller: SPUStandardUpdaterController?

    init() {
        let publicKey = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String
        guard let publicKey, !publicKey.isEmpty, !publicKey.hasPrefix("$(") else {
            controller = nil
            return
        }
        controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }

    var isAvailable: Bool { controller != nil }

    func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }
}
