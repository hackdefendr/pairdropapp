import Foundation
import ServiceManagement
import os

/// "Open at Login", via the modern `SMAppService` registration.
///
/// The system only honours this for an app in a stable, signed location — typically
/// `/Applications`. Running the binary straight out of `.build` will fail to register,
/// which `isAvailable` reports so the UI can explain rather than silently do nothing.
@MainActor
enum LoginItem {

    private static let log = Logger(subsystem: "app.pairdrop.mac", category: "loginitem")

    /// False when running from a build directory rather than an installed copy.
    static var isAvailable: Bool {
        let path = Bundle.main.bundleURL.path
        return Bundle.main.bundleURL.pathExtension == "app" && !path.contains("/.build/")
    }

    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    /// - Returns: nil on success, or a message explaining why it didn't take.
    @discardableResult
    static func setEnabled(_ enabled: Bool) -> String? {
        guard isAvailable else {
            return "Move PairDrop to your Applications folder first — macOS won't open a login item from a build folder."
        }

        do {
            if enabled {
                // Re-registering an already-registered service throws, so clear it first.
                if SMAppService.mainApp.status == .enabled {
                    try? SMAppService.mainApp.unregister()
                }
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            return nil
        } catch {
            log.error("login item \(enabled ? "register" : "unregister") failed: \(error.localizedDescription)")

            if SMAppService.mainApp.status == .requiresApproval {
                return "Approve PairDrop in System Settings → General → Login Items."
            }
            return error.localizedDescription
        }
    }
}
