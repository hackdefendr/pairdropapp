import AppKit
import Foundation
import Network
import Observation
import PairDropKit

/// Glue between stored preferences and the live PairDrop client, plus the small
/// amount of app-level state the panel needs.
@MainActor
@Observable
final class AppModel {

    let settings: AppSettings
    let client: PairDropClient

    /// Set when the user is composing a message rather than dropping a file.
    var composingTextFor: PairDropPeer?
    var isShowingPairingSheet = false
    var pairKeyEntry = ""

    @ObservationIgnored private let pathMonitor = NWPathMonitor()
    @ObservationIgnored private var lastPathSignature: String?

    init() {
        let settings = AppSettings()
        self.settings = settings
        self.client = PairDropClient(serverAddress: settings.serverAddress,
                                     downloadDirectory: settings.downloadDirectory,
                                     displayName: settings.effectiveDisplayName,
                                     allowUntrustedTLS: settings.allowUntrustedTLS)
        startPathMonitor()
    }

    // MARK: - Connection

    func startIfConfigured() {
        guard settings.isConfigured else { return }
        client.start()
    }

    /// Pushes edited preferences into the client. Changing the address or TLS policy
    /// reconnects; the rest take effect immediately.
    func applySettings() {
        client.downloadDirectory = settings.downloadDirectory
        client.displayName = settings.effectiveDisplayName
        client.allowUntrustedTLS = settings.allowUntrustedTLS
        client.serverAddress = settings.serverAddress

        if settings.isConfigured, case .idle = client.connectionState {
            client.start()
        }
    }

    private func startPathMonitor() {
        pathMonitor.pathUpdateHandler = { [weak self] path in
            // The server groups peers by public IP, so moving between networks puts us in
            // a different room. Reconnect whenever the usable interfaces actually change.
            let signature = "\(path.status)|" + path.availableInterfaces
                .map(\.name)
                .sorted()
                .joined(separator: ",")

            Task { @MainActor in
                guard let self else { return }
                let previous = self.lastPathSignature
                self.lastPathSignature = signature
                guard previous != nil, previous != signature else { return }
                guard path.status == .satisfied, self.settings.isConfigured else { return }
                self.client.networkChanged()
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "app.pairdrop.path"))
    }

    // MARK: - Sending

    func send(urls: [URL], to peer: PairDropPeer) {
        client.send(urls: urls, to: peer)
    }

    func send(text: String, to peer: PairDropPeer) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        client.send(text: trimmed, to: peer)
    }

    // MARK: - Received content

    func reveal(_ files: [ReceivedFile]) {
        NSWorkspace.shared.activateFileViewerSelecting(files.map(\.url))
    }

    func open(_ file: ReceivedFile) {
        NSWorkspace.shared.open(file.url)
    }

    func copyToPasteboard(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }
}
