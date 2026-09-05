import Foundation
import os

public enum SignalingState: Equatable, Sendable {
    case idle
    case connecting
    case connected
    case waitingToRetry(seconds: Int)
    case failed(String)
}

@MainActor
public protocol SignalingClientDelegate: AnyObject {
    func signalingClient(_ client: SignalingClient, didReceive message: ServerMessage)
    func signalingClient(_ client: SignalingClient, didChangeState state: SignalingState)
}

/// Maintains the WebSocket connection to a PairDrop signaling server.
///
/// The server pings every second and drops peers that go 5s without a pong, so the
/// receive loop must stay hot; everything here is cheap and main-actor isolated.
@MainActor
public final class SignalingClient: NSObject {

    public private(set) var state: SignalingState = .idle {
        didSet {
            guard state != oldValue else { return }
            delegate?.signalingClient(self, didChangeState: state)
        }
    }

    public weak var delegate: SignalingClientDelegate?

    /// Identity assigned by the server. Reused across reconnects so peers see one device.
    public private(set) var peerId: String?
    public private(set) var peerIdHash: String?
    public private(set) var assignedDisplayName: String?
    public private(set) var assignedDeviceName: String?
    public private(set) var rtcConfig: RTCConfigPayload = .fallback
    public private(set) var wsFallbackAvailable = false

    private let endpoint: ServerEndpoint
    private let userAgent: String
    private let allowUntrustedTLS: Bool
    private let log = Logger(subsystem: "app.pairdrop.kit", category: "signaling")

    private var session: URLSession!
    private var task: URLSessionWebSocketTask?
    private var reconnectAttempt = 0
    private var reconnectWorkItem: DispatchWorkItem?
    private var intentionallyClosed = false
    /// Guards against acting on callbacks from a socket we already replaced.
    private var generation = 0

    public init(endpoint: ServerEndpoint, userAgent: String, allowUntrustedTLS: Bool = false) {
        self.endpoint = endpoint
        self.userAgent = userAgent
        self.allowUntrustedTLS = allowUntrustedTLS
        super.init()

        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = 15
        session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
    }

    // MARK: - Lifecycle

    public func connect() {
        guard task == nil else { return }
        intentionallyClosed = false
        reconnectWorkItem?.cancel()
        state = .connecting

        generation += 1
        let generation = self.generation

        Task { [weak self] in
            guard let self else { return }
            let config = await self.fetchInstanceConfig()
            guard generation == self.generation, !self.intentionallyClosed else { return }
            self.openSocket(signalingServer: config?.signalingServer, generation: generation)
        }
    }

    /// Sends the polite `disconnect` frame so the server tears our rooms down immediately,
    /// then closes without scheduling a retry.
    public func disconnect() {
        intentionallyClosed = true
        reconnectWorkItem?.cancel()
        generation += 1
        if task != nil {
            send(.disconnect)
        }
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        state = .idle
    }

    /// Drops the socket and reconnects immediately — used when the network path changes.
    public func reconnectNow() {
        reconnectAttempt = 0
        generation += 1
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        connect()
    }

    // MARK: - Sending

    public func send(_ message: ClientMessage) {
        send(json: message.json)
    }

    public func send(json: [String: Any]) {
        guard let task,
              let data = try? JSONSerialization.data(withJSONObject: json),
              let text = String(data: data, encoding: .utf8) else { return }
        task.send(.string(text)) { [weak self] error in
            guard let error else { return }
            Task { @MainActor in self?.log.error("send failed: \(error.localizedDescription)") }
        }
    }

    // MARK: - Connecting

    private func fetchInstanceConfig() async -> InstanceConfig? {
        var request = URLRequest(url: endpoint.configURL)
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        request.timeoutInterval = 10
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                return nil
            }
            return try? JSONDecoder().decode(InstanceConfig.self, from: data)
        } catch {
            // Not fatal: an unreachable /config just means we fall back to this host for
            // signalling. The WebSocket attempt below surfaces a real outage.
            log.notice("config fetch failed: \(error.localizedDescription)")
            return nil
        }
    }

    private func openSocket(signalingServer: String?, generation: Int) {
        guard let url = endpoint.webSocketURL(signalingServer: signalingServer,
                                              peerId: peerId,
                                              peerIdHash: peerIdHash) else {
            state = .failed("Could not build a WebSocket URL for this server address.")
            return
        }

        var request = URLRequest(url: url)
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")

        let task = session.webSocketTask(with: request)
        self.task = task
        log.notice("connecting to \(url.absoluteString, privacy: .public)")
        task.resume()
        receive(on: task, generation: generation)
    }

    private func receive(on task: URLSessionWebSocketTask, generation: Int) {
        task.receive { [weak self] result in
            Task { @MainActor in
                guard let self, generation == self.generation else { return }
                switch result {
                case .success(let message):
                    if self.state != .connected {
                        self.reconnectAttempt = 0
                        self.state = .connected
                    }
                    self.handle(message)
                    self.receive(on: task, generation: generation)
                case .failure(let error):
                    self.handleDrop(error: error)
                }
            }
        }
    }

    private func handle(_ message: URLSessionWebSocketTask.Message) {
        let data: Data?
        switch message {
        case .string(let text): data = text.data(using: .utf8)
        case .data(let raw): data = raw
        @unknown default: data = nil
        }
        guard let data, let parsed = ServerMessage.parse(data) else { return }

        switch parsed {
        case .ping:
            send(.pong)
            return  // never surfaced upstream; it is pure keepalive
        case .wsConfig(let config):
            if let rtc = config.rtcConfig, !rtc.iceServers.isEmpty { rtcConfig = rtc }
            wsFallbackAvailable = config.wsFallback ?? false
        case .displayName(let id, let hash, let display, let device):
            peerId = id
            peerIdHash = hash
            assignedDisplayName = display
            assignedDeviceName = device
        default:
            break
        }

        delegate?.signalingClient(self, didReceive: parsed)
    }

    private func handleDrop(error: Error) {
        task = nil
        guard !intentionallyClosed else { return }

        log.notice("socket dropped: \(error.localizedDescription)")
        scheduleReconnect()
    }

    private func scheduleReconnect() {
        reconnectAttempt += 1
        let delay = min(pow(2.0, Double(min(reconnectAttempt, 5))), 30.0)
        state = .waitingToRetry(seconds: Int(delay))

        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor in
                guard let self, !self.intentionallyClosed else { return }
                self.connect()
            }
        }
        reconnectWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }
}

extension SignalingClient: URLSessionDelegate {
    nonisolated public func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        // Opt-in escape hatch for self-hosted instances behind a self-signed certificate.
        guard allowUntrustedTLS,
              challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        completionHandler(.useCredential, URLCredential(trust: trust))
    }
}
