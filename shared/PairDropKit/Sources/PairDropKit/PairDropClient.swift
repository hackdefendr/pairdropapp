import Foundation
import os

/// Something the UI should tell the user about.
public struct PairDropEvent: Identifiable, Sendable {
    public enum Kind: Sendable {
        case info
        case success
        case failure
        case incomingText(String)
        case incomingFiles([ReceivedFile])
    }

    public let id = UUID()
    public let kind: Kind
    public let message: String
    public let peerName: String?
    public let date = Date()
}

/// Pairing flow state for the "connect a device on another network" sheet.
public struct PairingSession: Equatable, Sendable {
    public let pairKey: String
    public let roomSecret: String
}

/// Owns the signaling connection and the set of nearby peers.
///
/// This is the object the UI observes; it is the native counterpart of `PeersManager`
/// in public/scripts/network.js.
@MainActor
@Observable
public final class PairDropClient {

    // MARK: Observable state

    public private(set) var connectionState: SignalingState = .idle
    public private(set) var peers: [PairDropPeer] = []
    public private(set) var events: [PairDropEvent] = []
    public private(set) var pairing: PairingSession?
    public private(set) var publicRoomId: String?

    /// Server-assigned identity, shown in settings so the user can confirm they are online.
    public private(set) var assignedDisplayName: String?

    public var serverAddress: String {
        didSet {
            guard serverAddress != oldValue else { return }
            restart()
        }
    }

    /// Where received files are written.
    public var downloadDirectory: URL

    /// Trust self-signed certificates. Off by default; on for LAN instances that use one.
    public var allowUntrustedTLS: Bool {
        didSet {
            guard allowUntrustedTLS != oldValue else { return }
            restart()
        }
    }

    /// Accept every incoming transfer without asking. Off by default; used by the
    /// headless probe and available to anyone who wants a trusted-network mode.
    public var autoAcceptEverything = false {
        didSet { peers.forEach(applyAutoAccept) }
    }

    /// The name peers see. Defaults to the machine name.
    public var displayName: String {
        didSet {
            guard displayName != oldValue else { return }
            for peer in peers { peer.announce(displayName: displayName) }
        }
    }

    // MARK: Internals

    @ObservationIgnored private var signaling: SignalingClient?
    @ObservationIgnored private var peersById: [String: PairDropPeer] = [:]
    @ObservationIgnored private let identity: DeviceIdentity
    @ObservationIgnored private let secrets = RoomSecretStore()
    @ObservationIgnored private let log = Logger(subsystem: "app.pairdrop.kit", category: "client")

    public init(serverAddress: String,
                downloadDirectory: URL,
                displayName: String? = nil,
                allowUntrustedTLS: Bool = false) {
        self.identity = DeviceIdentity.current()
        self.serverAddress = serverAddress
        self.downloadDirectory = downloadDirectory
        self.displayName = displayName ?? identity.displayName
        self.allowUntrustedTLS = allowUntrustedTLS
    }

    // MARK: - Lifecycle

    public func start() {
        guard signaling == nil else { return }
        guard let endpoint = ServerEndpoint(address: serverAddress) else {
            connectionState = .failed("\"\(serverAddress)\" isn't a valid server address.")
            return
        }

        let client = SignalingClient(endpoint: endpoint,
                                     userAgent: identity.userAgent,
                                     allowUntrustedTLS: allowUntrustedTLS)
        client.delegate = self
        signaling = client
        client.connect()
    }

    public func stop() {
        for peer in peers { peer.close() }
        peers = []
        peersById = [:]
        signaling?.disconnect()
        signaling = nil
        connectionState = .idle
    }

    public func restart() {
        stop()
        start()
    }

    /// Call when the network path changes so we re-enter the right IP room.
    public func networkChanged() {
        signaling?.reconnectNow()
    }

    // MARK: - Actions

    public func send(urls: [URL], to peer: PairDropPeer) {
        peer.send(urls: urls)
    }

    public func send(text: String, to peer: PairDropPeer) {
        peer.sendText(text)
    }

    public func peer(withId id: String) -> PairDropPeer? { peersById[id] }

    public func dismissEvent(_ event: PairDropEvent) {
        events.removeAll { $0.id == event.id }
    }

    public func clearEvents() {
        events.removeAll()
    }

    // MARK: - Pairing

    public func beginPairing() {
        signaling?.send(.pairDeviceInitiate)
    }

    public func cancelPairing() {
        signaling?.send(.pairDeviceCancel)
        pairing = nil
    }

    public func joinPairing(key: String) {
        let digits = key.filter(\.isNumber)
        guard digits.count == 6 else {
            post(.failure, "A pairing key is six digits.")
            return
        }
        signaling?.send(.pairDeviceJoin(pairKey: digits))
    }

    public var pairedDevices: [RoomSecretStore.Entry] { secrets.load() }

    public func unpair(secret: String) {
        let remaining = secrets.remove(secret: secret)
        signaling?.send(.roomSecretsDeleted([secret]))
        signaling?.send(.roomSecrets(remaining.map(\.secret)))
    }

    public func setAutoAccept(_ autoAccept: Bool, forSecret secret: String) {
        secrets.update(secret: secret, autoAccept: autoAccept)
        for peer in peers where peer.rooms.contains(where: { $0.type == .secret && $0.id == secret }) {
            peer.autoAccept = autoAccept
        }
    }

    // MARK: - Peer bookkeeping

    private func createOrRefreshPeer(_ info: PeerInfo, isCaller: Bool, room: RoomRef) {
        if let existing = peersById[info.id] {
            existing.update(info: info)
            existing.join(room: room)
            applyAutoAccept(to: existing)
            return
        }

        guard info.rtcSupported else {
            log.notice("peer \(info.id, privacy: .public) has no WebRTC; the WebSocket fallback is not implemented yet")
            return
        }

        let rtcConfig = signaling?.rtcConfig ?? .fallback
        let peer = PairDropPeer(info: info, isCaller: isCaller, room: room, rtcConfig: rtcConfig)
        peer.delegate = self
        peer.localDisplayName = displayName
        applyAutoAccept(to: peer)

        peersById[info.id] = peer
        sortPeers()
        peer.start()
    }

    private func applyAutoAccept(to peer: PairDropPeer) {
        if autoAcceptEverything {
            peer.autoAccept = true
            return
        }
        let entries = secrets.load()
        peer.autoAccept = peer.rooms.contains { room in
            room.type == .secret && entries.contains { $0.secret == room.id && $0.autoAccept }
        }
    }

    private func removePeer(id: String, room: RoomRef, disconnected: Bool) {
        guard let peer = peersById[id] else { return }

        peer.leave(room: room)
        // Only tear the connection down when we no longer share any room with them.
        guard disconnected || peer.rooms.isEmpty else { return }

        peer.close()
        peersById.removeValue(forKey: id)
        sortPeers()
    }

    private func sortPeers() {
        peers = peersById.values.sorted {
            if $0.isPaired != $1.isPaired { return $0.isPaired }
            return $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending
        }
    }

    private func post(_ kind: PairDropEvent.Kind, _ message: String, peerName: String? = nil) {
        events.append(PairDropEvent(kind: kind, message: message, peerName: peerName))
        if events.count > 50 { events.removeFirst(events.count - 50) }
    }
}

// MARK: - SignalingClientDelegate

extension PairDropClient: SignalingClientDelegate {

    public func signalingClient(_ client: SignalingClient, didChangeState state: SignalingState) {
        connectionState = state
        if case .connected = state { return }
        // Peers only exist while we share a signalling session with them.
        if case .waitingToRetry = state {
            for peer in peers { peer.close() }
            peersById = [:]
            peers = []
        }
    }

    public func signalingClient(_ client: SignalingClient, didReceive message: ServerMessage) {
        switch message {
        case .displayName(_, _, let display, _):
            assignedDisplayName = display
            // Join the LAN room and re-announce every device we have paired with.
            client.send(.joinIpRoom)
            let stored = secrets.load().map(\.secret)
            if !stored.isEmpty { client.send(.roomSecrets(stored)) }

        case .peers(let list, let room):
            // We were already in the room, so we place the calls.
            for info in list { createOrRefreshPeer(info, isCaller: true, room: room) }

        case .peerJoined(let info, let room):
            // They arrived after us, so they place the call.
            createOrRefreshPeer(info, isCaller: false, room: room)

        case .peerLeft(let peerId, let room, let disconnect):
            removePeer(id: peerId, room: room, disconnected: disconnect)

        case .signal(let sender, _, let sdp, let ice, _):
            guard let peer = peersById[sender.id] else { return }
            if let sdp { peer.handle(sdp: sdp) }
            if let ice { peer.handle(ice: ice) }

        case .pairDeviceInitiated(let roomSecret, let pairKey):
            pairing = PairingSession(pairKey: pairKey, roomSecret: roomSecret)

        case .pairDeviceJoined(let roomSecret, let peerId):
            let name = peersById[peerId]?.displayName ?? "the other device"
            secrets.add(RoomSecretStore.Entry(secret: roomSecret, displayName: name, autoAccept: false))
            client.send(.roomSecrets([roomSecret]))
            pairing = nil
            post(.success, "Paired with \(name).")

        case .pairDeviceJoinKeyInvalid:
            post(.failure, "That pairing key isn't valid.")

        case .pairDeviceCanceled:
            pairing = nil

        case .joinKeyRateLimit:
            post(.failure, "Too many attempts. Wait a few seconds and try again.")

        case .secretRoomDeleted(let roomSecret):
            secrets.remove(secret: roomSecret)
            post(.info, "A paired device removed this pairing.")

        case .roomSecretRegenerated(let old, let new):
            let remaining = secrets.replace(secret: old, with: new)
            client.send(.roomSecrets(remaining.map(\.secret)))

        case .publicRoomCreated(let roomId):
            publicRoomId = roomId

        case .publicRoomLeft:
            publicRoomId = nil

        case .publicRoomIdInvalid:
            post(.failure, "That room code isn't valid.")

        case .relayed(let type, _, _):
            // Only reachable when the instance runs with --include-ws-fallback and a peer
            // has no WebRTC. Not supported yet; drop rather than half-handle it.
            log.notice("ignoring WebSocket-fallback frame: \(type, privacy: .public)")

        case .wsConfig, .ping, .unknown:
            break
        }
    }
}

// MARK: - PairDropPeerDelegate

extension PairDropClient: PairDropPeerDelegate {

    public func peer(_ peer: PairDropPeer, send signal: OutboundSignal) {
        guard let signaling else { return }
        let room = peer.signalingRoom
        if let sdp = signal.sdp {
            signaling.send(.signalSDP(to: peer.id, room: room, sdp: sdp))
        }
        if let ice = signal.ice {
            signaling.send(.signalICE(to: peer.id, room: room, ice: ice))
        }
    }

    public func peerDidChange(_ peer: PairDropPeer) {
        sortPeers()
    }

    public func peer(_ peer: PairDropPeer, didReceiveTransferRequest request: TransferRequest) {
        // The UI observes `peer.pendingRequest`; this hook exists for alerting.
        let count = request.header.count
        let what = count == 1 ? request.header[0].name : "\(count) files"
        post(.info, "\(peer.displayName) wants to send \(what).", peerName: peer.displayName)
    }

    public func peer(_ peer: PairDropPeer, didReceiveFiles files: [ReceivedFile]) {
        post(.incomingFiles(files),
             files.count == 1 ? "Received \(files[0].name)" : "Received \(files.count) files",
             peerName: peer.displayName)
    }

    public func peer(_ peer: PairDropPeer, didReceiveText text: String) {
        post(.incomingText(text), "Received a message", peerName: peer.displayName)
    }

    public func peer(_ peer: PairDropPeer, didFinishSending count: Int) {
        post(.success, count == 1 ? "Sent 1 file" : "Sent \(count) files", peerName: peer.displayName)
    }

    public func peer(_ peer: PairDropPeer, didFailWith message: String) {
        post(.failure, message, peerName: peer.displayName)
    }

    public func downloadDirectory(for peer: PairDropPeer) -> URL {
        downloadDirectory
    }
}
