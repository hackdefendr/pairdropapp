import Foundation
import Observation
import PairDropKit

/// User preferences, backed by UserDefaults.
///
/// The download folder is stored as a security-scoped bookmark so the choice survives
/// both relaunches and, later, App Sandbox.
@MainActor
@Observable
final class AppSettings {

    private enum Key {
        static let serverAddress = "serverAddress"
        static let displayName = "displayName"
        static let allowUntrustedTLS = "allowUntrustedTLS"
        static let downloadBookmark = "downloadDirectoryBookmark"
        static let revealInFinder = "revealInFinder"
    }

    private let defaults: UserDefaults

    var serverAddress: String {
        didSet { defaults.set(serverAddress, forKey: Key.serverAddress) }
    }

    /// Empty means "use the machine name".
    var displayName: String {
        didSet { defaults.set(displayName, forKey: Key.displayName) }
    }

    var allowUntrustedTLS: Bool {
        didSet { defaults.set(allowUntrustedTLS, forKey: Key.allowUntrustedTLS) }
    }

    var revealInFinder: Bool {
        didSet { defaults.set(revealInFinder, forKey: Key.revealInFinder) }
    }

    private(set) var downloadDirectory: URL

    var effectiveDisplayName: String {
        let trimmed = displayName.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? DeviceIdentity.machineName() : trimmed
    }

    var isConfigured: Bool {
        ServerEndpoint(address: serverAddress) != nil
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.serverAddress = defaults.string(forKey: Key.serverAddress) ?? ""
        self.displayName = defaults.string(forKey: Key.displayName) ?? ""
        self.allowUntrustedTLS = defaults.bool(forKey: Key.allowUntrustedTLS)
        self.revealInFinder = defaults.object(forKey: Key.revealInFinder) as? Bool ?? true
        self.downloadDirectory = AppSettings.resolveDownloadDirectory(defaults: defaults)
    }

    func setDownloadDirectory(_ url: URL) {
        downloadDirectory = url
        if let bookmark = try? url.bookmarkData(options: .withSecurityScope,
                                                includingResourceValuesForKeys: nil,
                                                relativeTo: nil) {
            defaults.set(bookmark, forKey: Key.downloadBookmark)
        }
    }

    private static func resolveDownloadDirectory(defaults: UserDefaults) -> URL {
        if let bookmark = defaults.data(forKey: Key.downloadBookmark) {
            var stale = false
            if let url = try? URL(resolvingBookmarkData: bookmark,
                                  options: .withSecurityScope,
                                  relativeTo: nil,
                                  bookmarkDataIsStale: &stale), !stale {
                return url
            }
        }
        let downloads = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
        return downloads ?? FileManager.default.homeDirectoryForCurrentUser
    }
}
