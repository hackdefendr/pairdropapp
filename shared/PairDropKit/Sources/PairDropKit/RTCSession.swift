import Foundation
@preconcurrency import WebRTC
import os

/// Signals this session needs to hand to the signaling server.
public struct OutboundSignal: Sendable {
    public let sdp: SessionDescriptionPayload?
    public let ice: IceCandidatePayload?
}

@MainActor
public protocol RTCSessionDelegate: AnyObject {
    func rtcSession(_ session: RTCSession, needsToSend signal: OutboundSignal)
    func rtcSessionDidOpenChannel(_ session: RTCSession)
    func rtcSessionDidCloseChannel(_ session: RTCSession)
    func rtcSession(_ session: RTCSession, didReceiveText data: Data)
    func rtcSession(_ session: RTCSession, didReceiveBinary data: Data)
    func rtcSession(_ session: RTCSession, didFailWith reason: String)
    /// ICE gave up. The session is unusable and has to be rebuilt to try again.
    func rtcSessionConnectionDidFail(_ session: RTCSession)
}

/// One WebRTC peer connection plus the single ordered data channel PairDrop uses.
///
/// The caller creates the channel named `data-channel` and sends the offer; the callee
/// waits for the in-band-negotiated channel to arrive. That asymmetry mirrors `RTCPeer`
/// in public/scripts/network.js and is what makes us interoperable with the web client.
@MainActor
public final class RTCSession: NSObject {

    /// Shared because building a factory is expensive and libwebrtc expects one per process.
    private static let factory: RTCPeerConnectionFactory = {
        RTCInitializeSSL()
        // Data channels only — no audio or video codecs needed.
        return RTCPeerConnectionFactory(encoderFactory: nil, decoderFactory: nil)
    }()

    public static let channelLabel = "data-channel"

    public let peerId: String
    public let isCaller: Bool
    public weak var delegate: RTCSessionDelegate?

    public private(set) var isChannelOpen = false

    private var connection: RTCPeerConnection?
    private var channel: RTCDataChannel?
    private let rtcConfig: RTCConfigPayload
    private let log = Logger(subsystem: "app.pairdrop.kit", category: "rtc")

    /// ICE that arrives before the remote description is set must be held back.
    private var pendingRemoteCandidates: [RTCIceCandidate] = []
    private var hasRemoteDescription = false

    public init(peerId: String, isCaller: Bool, rtcConfig: RTCConfigPayload) {
        self.peerId = peerId
        self.isCaller = isCaller
        self.rtcConfig = rtcConfig
        super.init()
    }

    // MARK: - Connection setup

    /// Starts the connection. Only the caller sends an offer; the callee stays passive
    /// until a remote offer arrives.
    public func start() {
        guard isCaller else { return }
        openConnectionIfNeeded()
        openChannel()
    }

    private func openConnectionIfNeeded() {
        if let connection, connection.signalingState != .closed { return }

        let configuration = RTCConfiguration()
        configuration.iceServers = rtcConfig.iceServers.map {
            if let username = $0.username, let credential = $0.credential {
                return RTCIceServer(urlStrings: $0.urls, username: username, credential: credential)
            }
            return RTCIceServer(urlStrings: $0.urls)
        }
        configuration.sdpSemantics = .unifiedPlan
        configuration.continualGatheringPolicy = .gatherContinually
        // Trickle ICE: candidates are relayed as they are found, same as the browser.

        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let connection = RTCSession.factory.peerConnection(with: configuration,
                                                                 constraints: constraints,
                                                                 delegate: self) else {
            delegate?.rtcSession(self, didFailWith: "Could not create a WebRTC peer connection.")
            return
        }
        self.connection = connection
    }

    private func openChannel() {
        guard let connection else { return }

        let config = RTCDataChannelConfiguration()
        config.isOrdered = true
        guard let channel = connection.dataChannel(forLabel: RTCSession.channelLabel, configuration: config) else {
            delegate?.rtcSession(self, didFailWith: "Could not open the data channel.")
            return
        }
        channel.delegate = self
        self.channel = channel

        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        connection.offer(for: constraints) { [weak self] description, error in
            Task { @MainActor in
                guard let self else { return }
                guard let description else {
                    self.delegate?.rtcSession(self, didFailWith: error?.localizedDescription ?? "Offer failed.")
                    return
                }
                self.apply(local: description)
            }
        }
    }

    private func apply(local description: RTCSessionDescription) {
        guard let connection else { return }
        connection.setLocalDescription(description) { [weak self] error in
            Task { @MainActor in
                guard let self else { return }
                if let error {
                    self.delegate?.rtcSession(self, didFailWith: error.localizedDescription)
                    return
                }
                let payload = SessionDescriptionPayload(type: RTCSession.string(for: description.type),
                                                        sdp: description.sdp)
                self.delegate?.rtcSession(self, needsToSend: OutboundSignal(sdp: payload, ice: nil))
            }
        }
    }

    // MARK: - Inbound signalling

    public func handle(sdp payload: SessionDescriptionPayload) {
        openConnectionIfNeeded()
        guard let connection else { return }

        let type = RTCSession.sdpType(for: payload.type)
        let description = RTCSessionDescription(type: type, sdp: payload.sdp)

        connection.setRemoteDescription(description) { [weak self] error in
            Task { @MainActor in
                guard let self else { return }
                if let error {
                    self.delegate?.rtcSession(self, didFailWith: error.localizedDescription)
                    return
                }
                self.hasRemoteDescription = true
                self.flushPendingCandidates()

                guard type == .offer else { return }
                let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
                connection.answer(for: constraints) { answer, answerError in
                    Task { @MainActor in
                        guard let answer else {
                            self.delegate?.rtcSession(self, didFailWith: answerError?.localizedDescription ?? "Answer failed.")
                            return
                        }
                        self.apply(local: answer)
                    }
                }
            }
        }
    }

    public func handle(ice payload: IceCandidatePayload) {
        openConnectionIfNeeded()
        let candidate = RTCIceCandidate(sdp: payload.candidate,
                                        sdpMLineIndex: payload.sdpMLineIndex ?? 0,
                                        sdpMid: payload.sdpMid)
        guard hasRemoteDescription else {
            pendingRemoteCandidates.append(candidate)
            return
        }
        add(candidate)
    }

    private func flushPendingCandidates() {
        let queued = pendingRemoteCandidates
        pendingRemoteCandidates.removeAll()
        queued.forEach(add)
    }

    private func add(_ candidate: RTCIceCandidate) {
        connection?.add(candidate) { [weak self] error in
            guard let error else { return }
            Task { @MainActor in self?.log.notice("addIceCandidate: \(error.localizedDescription)") }
        }
    }

    // MARK: - Sending

    /// Bytes queued in the data channel but not yet handed to the network.
    public var bufferedAmount: UInt64 { channel?.bufferedAmount ?? 0 }

    @discardableResult
    public func send(json: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return false }
        return send(data: data, isBinary: false)
    }

    @discardableResult
    public func send(_ message: TransferMessage) -> Bool {
        send(json: message.json)
    }

    @discardableResult
    public func send(data: Data, isBinary: Bool) -> Bool {
        guard let channel, channel.readyState == .open else { return false }
        return channel.sendData(RTCDataBuffer(data: data, isBinary: isBinary))
    }

    // MARK: - Teardown

    public func close() {
        channel?.delegate = nil
        channel?.close()
        channel = nil
        connection?.delegate = nil
        connection?.close()
        connection = nil
        isChannelOpen = false
        hasRemoteDescription = false
        pendingRemoteCandidates.removeAll()
    }

    /// 16-digit verification code derived from both DTLS fingerprints, matching
    /// `RTCPeer.getConnectionHash()` in the web client.
    public func connectionHash() -> String? {
        guard let local = connection?.localDescription?.sdp,
              let remote = connection?.remoteDescription?.sdp,
              let localFingerprint = RTCSession.fingerprint(in: local),
              let remoteFingerprint = RTCSession.fingerprint(in: remote) else { return nil }

        let combined = isCaller
            ? localFingerprint + remoteFingerprint
            : remoteFingerprint + localFingerprint
        return Cyrb53.connectionHash(combined)
    }

    private static func fingerprint(in sdp: String) -> String? {
        for line in sdp.components(separatedBy: "\r\n") where line.hasPrefix("a=fingerprint:") {
            return String(line.dropFirst("a=fingerprint:".count))
        }
        return nil
    }

    private static func string(for type: RTCSdpType) -> String {
        switch type {
        case .offer: return "offer"
        case .prAnswer: return "pranswer"
        case .answer: return "answer"
        case .rollback: return "rollback"
        @unknown default: return "offer"
        }
    }

    private static func sdpType(for string: String) -> RTCSdpType {
        switch string {
        case "answer": return .answer
        case "pranswer": return .prAnswer
        case "rollback": return .rollback
        default: return .offer
        }
    }
}

// MARK: - RTCPeerConnectionDelegate

extension RTCSession: RTCPeerConnectionDelegate {

    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        let payload = IceCandidatePayload(candidate: candidate.sdp,
                                          sdpMid: candidate.sdpMid,
                                          sdpMLineIndex: candidate.sdpMLineIndex)
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.delegate?.rtcSession(self, needsToSend: OutboundSignal(sdp: nil, ice: payload))
        }
    }

    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            // The callee's channel arrives here, negotiated in-band by the caller.
            dataChannel.delegate = self
            self.channel = dataChannel
            if dataChannel.readyState == .open { self.markChannelOpen() }
        }
    }

    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            switch newState {
            case .disconnected:
                // Often transient — ICE frequently recovers on its own, so don't tear
                // anything down yet.
                self.delegate?.rtcSession(self, didFailWith: "Connection lost.")
            case .failed:
                self.delegate?.rtcSessionConnectionDidFail(self)
            default:
                break
            }
        }
    }

    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    nonisolated public func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    nonisolated public func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
}

// MARK: - RTCDataChannelDelegate

extension RTCSession: RTCDataChannelDelegate {

    nonisolated public func dataChannelDidChangeState(_ dataChannel: RTCDataChannel) {
        let state = dataChannel.readyState
        Task { @MainActor [weak self] in
            guard let self else { return }
            switch state {
            case .open:
                self.markChannelOpen()
            case .closed, .closing:
                if self.isChannelOpen {
                    self.isChannelOpen = false
                    self.delegate?.rtcSessionDidCloseChannel(self)
                }
            default:
                break
            }
        }
    }

    nonisolated public func dataChannel(_ dataChannel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        let data = buffer.data
        let isBinary = buffer.isBinary
        Task { @MainActor [weak self] in
            guard let self else { return }
            if isBinary {
                self.delegate?.rtcSession(self, didReceiveBinary: data)
            } else {
                self.delegate?.rtcSession(self, didReceiveText: data)
            }
        }
    }

    private func markChannelOpen() {
        guard !isChannelOpen else { return }
        isChannelOpen = true
        log.notice("data channel open with \(self.peerId, privacy: .public)")
        delegate?.rtcSessionDidOpenChannel(self)
    }
}
