import Foundation
#if canImport(IOKit)
import IOKit
#endif
#if canImport(UIKit)
import UIKit
#endif

/// How this device introduces itself.
///
/// PairDrop's server derives the *device name* (the subtitle in peer lists) by running
/// our `User-Agent` through ua-parser-js, and the *display name* from a hash of our peer
/// id. We can't change either from the client — but once a data channel is open, peers
/// honour a `display-name-changed` frame, which is how we surface the real machine name.
public struct DeviceIdentity: Sendable {

    public let userAgent: String
    public let displayName: String

    public init(userAgent: String, displayName: String) {
        self.userAgent = userAgent
        self.displayName = displayName
    }

    public static func current(appVersion: String = "1.0") -> DeviceIdentity {
        DeviceIdentity(userAgent: defaultUserAgent(appVersion: appVersion),
                       displayName: machineName())
    }

    /// ua-parser-js has no generic browser fallback, so the best we can do is have it
    /// recognise the OS and nothing else: the server then labels us plain "Mac".
    /// (Including `Macintosh` makes it report the device model as "Macintosh", giving
    /// the much worse "Mac Macintosh"; verified against the server's own ua-parser-js.)
    public static func defaultUserAgent(appVersion: String = "1.0") -> String {
        let os = ProcessInfo.processInfo.operatingSystemVersion
        #if os(macOS)
        let version = "\(os.majorVersion)_\(os.minorVersion)_\(os.patchVersion)"
        return "PairDrop/\(appVersion) (Mac OS X \(version))"
        #else
        let version = "\(os.majorVersion)_\(os.minorVersion)"
        return "PairDrop/\(appVersion) (iPhone; CPU iPhone OS \(version) like Mac OS X)"
        #endif
    }

    /// The name the user recognises — "Josh's MacBook Pro" — not the Bonjour hostname.
    public static func machineName() -> String {
        #if os(macOS)
        if let name = Host.current().localizedName, !name.isEmpty { return name }
        var hostname = ProcessInfo.processInfo.hostName
        if hostname.hasSuffix(".local") { hostname.removeLast(6) }
        return hostname.isEmpty ? "Mac" : hostname
        #elseif canImport(UIKit)
        return UIDevice.current.name
        #else
        return ProcessInfo.processInfo.hostName
        #endif
    }
}
