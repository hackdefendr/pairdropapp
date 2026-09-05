import Foundation
import os

// MARK: - Public state

public enum PeerConnectionState: Equatable, Sendable {
    case connecting
    case connected
    case disconnected
    case failed(String)
}

public enum PeerActivity: Equatable, Sendable {
    case idle
    /// Reading file metadata before asking the peer to accept.
    case preparing
    /// We asked; waiting for them to accept or decline.
    case awaitingResponse
    case sending(progress: Double)
    /// They asked; waiting for us to accept or decline.
    case incomingRequest
    case receiving(progress: Double)

    public var isBusy: Bool {
        if case .idle = self { return false }
        return true
    }

    public var progress: Double? {
        switch self {
        case .sending(let p), .receiving(let p): return p
        default: return nil
        }
    }
}

public struct ReceivedFile: Identifiable, Hashable, Sendable {
    public let id = UUID()
    public let url: URL
    public let mime: String
    public let size: Int64

    public var name: String { url.lastPathComponent }
}

@MainActor
public protocol PairDropPeerDelegate: AnyObject {
    func peer(_ peer: PairDropPeer, send signal: OutboundSignal)
    func peerDidChange(_ peer: PairDropPeer)
    func peer(_ peer: PairDropPeer, didReceiveTransferRequest request: TransferRequest)
    func peer(_ peer: PairDropPeer, didReceiveFiles files: [ReceivedFile])
    func peer(_ peer: PairDropPeer, didReceiveText text: String)
    func peer(_ peer: PairDropPeer, didFinishSending count: Int)
    func peer(_ peer: PairDropPeer, didFailWith message: String)
    /// Where completed downloads should land.
    func downloadDirectory(for peer: PairDropPeer) -> URL
}

// MARK: - Peer

/// One nearby device: its identity, its WebRTC session, and the transfer state machine
/// from public/scripts/network.js (`Peer` / `RTCPeer`).
@MainActor
@Observable
public final class PairDropPeer: Identifiable {

    public let id: String
    public private(set) var info: PeerInfo
    public private(set) var rooms: Set<RoomRef> = []

    public private(set) var connectionState: PeerConnectionState = .connecting
    public private(set) var activity: PeerActivity = .idle
    public private(set) var connectionHash: String?
    public private(set) var pendingRequest: TransferRequest?

    /// Accept incoming transfers without asking. Set for devices the user has paired.
    public var autoAccept = false

    /// Name the peer told us over the data channel, which beats the server's guess.
    public private(set) var announcedDisplayName: String?

    public var displayName: String {
        announcedDisplayName
            ?? info.name.displayName
            ?? info.name.deviceName
            ?? String(id.prefix(8))
    }

    public var deviceName: String {
        info.name.deviceName ?? [info.name.os, info.name.browser].compactMap { $0 }.joined(separator: " ")
    }

    public var isMobile: Bool { info.name.type == "mobile" || info.name.type == "tablet" }

    public var isPaired: Bool { rooms.contains { $0.type == .secret } }

    @ObservationIgnored public weak var delegate: PairDropPeerDelegate?
    @ObservationIgnored private var session: RTCSession
    @ObservationIgnored private let log = Logger(subsystem: "app.pairdrop.kit", category: "peer")
    @ObservationIgnored private let ioQueue: DispatchQueue

    // Rebuilding the session after ICE gives up
    @ObservationIgnored private let isCaller: Bool
    @ObservationIgnored private let rtcConfig: RTCConfigPayload
    @ObservationIgnored private var reconnectAttempts = 0
    @ObservationIgnored private var reconnectTask: Task<Void, Never>?
    /// Enough retries to ride out a peer that is slow to wake, without hammering it.
    @ObservationIgnored private static let maxReconnectAttempts = 4

    // Outgoing state
    @ObservationIgnored private var outgoingQueue: [URL] = []
    @ObservationIgnored private var requestedURLs: [URL] = []
    /// Touched only on `ioQueue` once created.
    @ObservationIgnored private var chunker: FileChunker?
    @ObservationIgnored private var totalOutgoingBytes: Int64 = 0
    @ObservationIgnored private var outgoingBytesSentBeforeCurrentFile: Int64 = 0
    @ObservationIgnored private var outgoingBytesSentInCurrentFile: Int64 = 0
    @ObservationIgnored private var filesSentInBatch = 0
    /// Held open for the file currently being read, for the sandboxed case.
    @ObservationIgnored private var scopedURL: URL?

    // Incoming state
    @ObservationIgnored private var acceptedRequest: TransferRequest?
    @ObservationIgnored private var remainingHeaders: [FileHeader] = []
    @ObservationIgnored private var receiver: FileReceiver?
    @ObservationIgnored private var incomingFiles: [ReceivedFile] = []
    @ObservationIgnored private var totalIncomingBytesReceived: Int64 = 0
    @ObservationIgnored private var bytesInCurrentFile: Int64 = 0
    @ObservationIgnored private var lastReportedProgress: Double = 0
    @ObservationIgnored private var writeBuffer: [Data] = []
    @ObservationIgnored private var writeBufferBytes = 0

    /// Our own name, sent to the peer as soon as the channel opens.
    @ObservationIgnored public var localDisplayName: String?

    public init(info: PeerInfo, isCaller: Bool, room: RoomRef, rtcConfig: RTCConfigPayload) {
        self.id = info.id
        self.info = info
        self.rooms = [room]
        self.isCaller = isCaller
        self.rtcConfig = rtcConfig
        self.session = RTCSession(peerId: info.id, isCaller: isCaller, rtcConfig: rtcConfig)
        self.ioQueue = DispatchQueue(label: "app.pairdrop.transfer.\(info.id)", qos: .userInitiated)
        self.session.delegate = self
    }

    // MARK: Lifecycle

    public func start() {
        session.start()
    }

    public func handle(sdp: SessionDescriptionPayload) {
        session.handle(sdp: sdp)
    }

    public func handle(ice: IceCandidatePayload) {
        session.handle(ice: ice)
    }

    public func update(info: PeerInfo) {
        self.info = info
        delegate?.peerDidChange(self)
    }

    public func join(room: RoomRef) {
        rooms.insert(room)
        delegate?.peerDidChange(self)
    }

    public func leave(room: RoomRef) {
        rooms.remove(room)
        delegate?.peerDidChange(self)
    }

    /// The room to stamp on outbound signalling frames.
    public var signalingRoom: RoomRef {
        rooms.first { $0.type == .secret } ?? rooms.first { $0.type == .ip } ?? rooms.first
            ?? RoomRef(type: .ip, id: "")
    }

    public func close() {
        reconnectTask?.cancel()
        reconnectTask = nil
        cancelTransfers(reason: nil)
        session.close()
        connectionState = .disconnected
        delegate?.peerDidChange(self)
    }

    /// ICE failed. A peer can be slow to wake — a backgrounded browser tab, a phone
    /// that just came back on the network — so the caller rebuilds the session and
    /// offers again a few times before giving up.
    private func scheduleReconnect() {
        guard isCaller,
              reconnectAttempts < PairDropPeer.maxReconnectAttempts,
              reconnectTask == nil else { return }

        reconnectAttempts += 1
        let attempt = reconnectAttempts
        let delay = min(pow(2.0, Double(attempt)), 16)
        log.notice("retrying connection to \(self.id, privacy: .public) in \(delay)s (attempt \(attempt))")

        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            self.reconnectTask = nil
            self.rebuildSession()
        }
    }

    private func rebuildSession() {
        session.close()
        session = RTCSession(peerId: id, isCaller: isCaller, rtcConfig: rtcConfig)
        session.delegate = self
        connectionState = .connecting
        delegate?.peerDidChange(self)
        session.start()
    }

    public func announce(displayName: String) {
        localDisplayName = displayName
        guard session.isChannelOpen else { return }
        session.send(.displayNameChanged(displayName))
    }

    // MARK: - Sending files

    public func send(urls: [URL]) {
        guard !urls.isEmpty else { return }
        guard case .idle = activity else {
            delegate?.peer(self, didFailWith: "\(displayName) is busy with another transfer.")
            return
        }
        guard session.isChannelOpen else {
            delegate?.peer(self, didFailWith: "Not connected to \(displayName) yet.")
            return
        }

        activity = .preparing
        delegate?.peerDidChange(self)

        ioQueue.async { [weak self] in
            var headers: [FileHeader] = []
            var accepted: [URL] = []
            var total: Int64 = 0
            var imagesOnly = true
            var skipped: [String] = []

            for url in urls {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }

                guard let values = try? url.resourceValues(forKeys: [.fileSizeKey, .isDirectoryKey]),
                      values.isDirectory != true,
                      let size = values.fileSize else {
                    skipped.append(url.lastPathComponent)
                    continue
                }

                // A zero-byte file stalls the transfer: the protocol has no chunk to
                // carry and no completion signal, so neither side ever advances. The
                // web client has the same gap, so drop them rather than hang.
                guard size > 0 else {
                    skipped.append(url.lastPathComponent)
                    continue
                }

                let mime = MimeType.of(url)
                if !MimeType.isImage(mime) { imagesOnly = false }
                headers.append(FileHeader(name: url.lastPathComponent, mime: mime, size: Int64(size)))
                accepted.append(url)
                total += Int64(size)
            }

            let request = TransferRequest(header: headers,
                                          totalSize: total,
                                          imagesOnly: imagesOnly && !headers.isEmpty,
                                          thumbnailDataUrl: nil)
            let finalURLs = accepted
            let finalSkipped = skipped
            Task { @MainActor in
                self?.beginRequest(request, urls: finalURLs, skipped: finalSkipped)
            }
        }
    }

    private func beginRequest(_ request: TransferRequest, urls: [URL], skipped: [String]) {
        guard !urls.isEmpty else {
            activity = .idle
            let detail = skipped.isEmpty
                ? "Nothing to send — folders aren't supported yet."
                : "Can't send \(skipped.joined(separator: ", ")) — empty files and folders aren't supported."
            delegate?.peer(self, didFailWith: detail)
            delegate?.peerDidChange(self)
            return
        }

        if !skipped.isEmpty {
            delegate?.peer(self, didFailWith: "Skipped \(skipped.joined(separator: ", ")) — empty files and folders aren't supported.")
        }

        requestedURLs = urls
        totalOutgoingBytes = request.totalSize
        activity = .awaitingResponse
        session.send(.request(request))
        delegate?.peerDidChange(self)
    }

    public func sendText(_ text: String) {
        guard session.isChannelOpen else {
            delegate?.peer(self, didFailWith: "Not connected to \(displayName) yet.")
            return
        }
        session.send(.text(text))
    }

    private func startSendingQueuedFiles() {
        outgoingQueue = requestedURLs
        requestedURLs = []
        outgoingBytesSentBeforeCurrentFile = 0
        filesSentInBatch = 0
        activity = .sending(progress: 0)
        delegate?.peerDidChange(self)
        dequeueNextFile()
    }

    private func dequeueNextFile() {
        guard !outgoingQueue.isEmpty else { return }
        let url = outgoingQueue.removeFirst()
        outgoingBytesSentInCurrentFile = 0

        let scoped = url.startAccessingSecurityScopedResource()
        scopedURL = scoped ? url : nil

        ioQueue.async { [weak self] in
            guard let self else { return }
            do {
                let chunker = try FileChunker(url: url)
                self.chunker = chunker
                let header = FileHeader(name: url.lastPathComponent,
                                        mime: MimeType.of(url),
                                        size: chunker.size)
                Task { @MainActor in
                    self.session.send(.header(header))
                    self.sendNextPartition()
                }
            } catch {
                Task { @MainActor in
                    self.failTransfer("Could not read \(url.lastPathComponent): \(error.localizedDescription)")
                }
            }
        }
    }

    private func releaseCurrentFile() {
        ioQueue.async { [chunker] in chunker?.close() }
        chunker = nil
        scopedURL?.stopAccessingSecurityScopedResource()
        scopedURL = nil
    }

    /// Reads up to one 1 MB partition off disk, then hands the chunks to the data channel
    /// in one hop. The peer acknowledges each partition before we read the next one.
    private func sendNextPartition() {
        ioQueue.async { [weak self] in
            guard let self, let chunker = self.chunker else { return }

            var chunks: [Data] = []
            chunks.reserveCapacity(TransferConstants.maxPartitionSize / TransferConstants.chunkSize)
            do {
                chunker.beginPartition()
                while !chunker.isFileEnd && !chunker.isPartitionEnd {
                    guard let chunk = try chunker.nextChunk() else { break }
                    chunks.append(chunk)
                }
            } catch {
                Task { @MainActor in self.failTransfer("Read failed: \(error.localizedDescription)") }
                return
            }

            let atFileEnd = chunker.isFileEnd
            let offset = chunker.offset
            Task { @MainActor in
                self.deliver(chunks, atFileEnd: atFileEnd, offset: offset)
            }
        }
    }

    private func deliver(_ chunks: [Data], atFileEnd: Bool, offset: Int64) {
        for chunk in chunks {
            guard session.send(data: chunk, isBinary: true) else {
                failTransfer("The connection to \(displayName) dropped mid-transfer.")
                return
            }
            outgoingBytesSentInCurrentFile += Int64(chunk.count)
        }
        updateSendProgress()

        // At end of file we stay quiet: the receiver answers with file-transfer-complete.
        if !atFileEnd {
            session.send(.partition(offset: offset))
        }
    }

    private func updateSendProgress() {
        guard totalOutgoingBytes > 0 else { return }
        let sent = outgoingBytesSentBeforeCurrentFile + outgoingBytesSentInCurrentFile
        let progress = min(1, Double(sent) / Double(totalOutgoingBytes))
        if case .sending = activity {
            activity = .sending(progress: progress)
            delegate?.peerDidChange(self)
        }
    }

    // MARK: - Receiving files

    public func respondToRequest(accepted: Bool) {
        guard let request = pendingRequest else { return }
        pendingRequest = nil

        session.send(.filesTransferResponse(accepted: accepted, reason: nil))

        if accepted {
            acceptedRequest = request
            remainingHeaders = request.header
            incomingFiles = []
            totalIncomingBytesReceived = 0
            lastReportedProgress = 0
            activity = .receiving(progress: 0)
        } else {
            activity = .idle
        }
        delegate?.peerDidChange(self)
    }

    private func handleIncomingRequest(_ request: TransferRequest) {
        guard pendingRequest == nil, acceptedRequest == nil else {
            // One request at a time per peer, same as the web client.
            session.send(.filesTransferResponse(accepted: false, reason: nil))
            return
        }

        pendingRequest = request
        activity = .incomingRequest
        delegate?.peerDidChange(self)

        if autoAccept {
            respondToRequest(accepted: true)
        } else {
            delegate?.peer(self, didReceiveTransferRequest: request)
        }
    }

    private func handleFileHeader(_ header: FileHeader) {
        guard acceptedRequest != nil, let expected = remainingHeaders.first else { return }

        // The peer must deliver exactly what we agreed to accept.
        guard expected.name == header.name, expected.size == header.size else {
            failTransfer("\(displayName) sent a file we didn't agree to receive. Transfer stopped.")
            return
        }

        bytesInCurrentFile = 0
        do {
            receiver = try FileReceiver(header: header)
        } catch {
            failTransfer("Could not stage the incoming file: \(error.localizedDescription)")
            return
        }

        // No chunks will ever arrive for an empty file, so complete it here rather
        // than waiting forever.
        if header.size == 0 {
            flushWriteBuffer(finishing: true)
        }
    }

    private func handleChunk(_ data: Data) {
        guard let receiver, !data.isEmpty else { return }

        bytesInCurrentFile += Int64(data.count)
        writeBuffer.append(data)
        writeBufferBytes += data.count

        let complete = bytesInCurrentFile >= receiver.header.size
        if writeBufferBytes >= 512_000 || complete {
            flushWriteBuffer(finishing: complete)
        }

        reportReceiveProgress()
    }

    private func flushWriteBuffer(finishing: Bool) {
        let pending = writeBuffer
        writeBuffer = []
        writeBufferBytes = 0
        guard let receiver else { return }
        let directory = delegate?.downloadDirectory(for: self) ?? FileManager.default.temporaryDirectory

        ioQueue.async { [weak self] in
            guard let self else { return }
            do {
                for chunk in pending { try receiver.append(chunk) }
                guard finishing else { return }
                let url = try receiver.finish(movingTo: directory)
                let file = ReceivedFile(url: url, mime: receiver.header.mime, size: receiver.header.size)
                Task { @MainActor in self.finishIncomingFile(file) }
            } catch {
                receiver.discard()
                Task { @MainActor in
                    self.failTransfer("Could not save the incoming file: \(error.localizedDescription)")
                }
            }
        }
    }

    private func finishIncomingFile(_ file: ReceivedFile) {
        receiver = nil
        totalIncomingBytesReceived += file.size
        bytesInCurrentFile = 0
        incomingFiles.append(file)

        session.send(.fileTransferComplete)

        if !remainingHeaders.isEmpty { remainingHeaders.removeFirst() }

        guard remainingHeaders.isEmpty else {
            delegate?.peerDidChange(self)
            return
        }

        let files = incomingFiles
        incomingFiles = []
        acceptedRequest = nil
        activity = .idle
        delegate?.peer(self, didReceiveFiles: files)
        delegate?.peerDidChange(self)
    }

    private func reportReceiveProgress() {
        guard let request = acceptedRequest, request.totalSize > 0 else { return }
        let progress = min(1, Double(totalIncomingBytesReceived + bytesInCurrentFile) / Double(request.totalSize))
        activity = .receiving(progress: progress)
        delegate?.peerDidChange(self)

        // Same throttle the web client uses, so we don't flood the channel.
        guard progress - lastReportedProgress >= 0.005 || progress >= 1 else { return }
        lastReportedProgress = progress
        session.send(.progress(progress))
    }

    // MARK: - Failure handling

    private func failTransfer(_ message: String) {
        cancelTransfers(reason: message)
        delegate?.peer(self, didFailWith: message)
    }

    private func cancelTransfers(reason: String?) {
        releaseCurrentFile()
        outgoingQueue = []
        requestedURLs = []
        outgoingBytesSentInCurrentFile = 0
        outgoingBytesSentBeforeCurrentFile = 0
        filesSentInBatch = 0

        receiver?.discard()
        receiver = nil
        acceptedRequest = nil
        pendingRequest = nil
        remainingHeaders = []
        writeBuffer = []
        writeBufferBytes = 0
        incomingFiles = []

        activity = .idle
        delegate?.peerDidChange(self)
    }
}

// MARK: - RTCSessionDelegate

extension PairDropPeer: RTCSessionDelegate {

    public func rtcSession(_ session: RTCSession, needsToSend signal: OutboundSignal) {
        delegate?.peer(self, send: signal)
    }

    public func rtcSessionDidOpenChannel(_ session: RTCSession) {
        reconnectAttempts = 0
        reconnectTask?.cancel()
        reconnectTask = nil
        connectionState = .connected
        connectionHash = session.connectionHash()
        if let localDisplayName {
            session.send(.displayNameChanged(localDisplayName))
        }
        delegate?.peerDidChange(self)
    }

    public func rtcSessionDidCloseChannel(_ session: RTCSession) {
        connectionState = .disconnected
        cancelTransfers(reason: nil)
        scheduleReconnect()
    }

    public func rtcSession(_ session: RTCSession, didFailWith reason: String) {
        // Informational: ICE may still recover, so don't rebuild on this alone.
        guard connectionState != .connected else { return }
        connectionState = .failed(reason)
        delegate?.peerDidChange(self)
    }

    public func rtcSessionConnectionDidFail(_ session: RTCSession) {
        cancelTransfers(reason: nil)

        let exhausted = reconnectAttempts >= PairDropPeer.maxReconnectAttempts || !isCaller
        connectionState = .failed(exhausted ? "Couldn't connect." : "Reconnecting…")
        delegate?.peerDidChange(self)
        scheduleReconnect()
    }

    public func rtcSession(_ session: RTCSession, didReceiveBinary data: Data) {
        handleChunk(data)
    }

    public func rtcSession(_ session: RTCSession, didReceiveText data: Data) {
        guard let message = TransferMessage.parse(data) else { return }
        handle(message)
    }

    /// Shared by the WebRTC channel and, later, the WebSocket fallback relay.
    public func handle(_ message: TransferMessage) {
        switch message {
        case .request(let request):
            handleIncomingRequest(request)

        case .filesTransferResponse(let accepted, let reason):
            guard case .awaitingResponse = activity else { return }
            if accepted {
                startSendingQueuedFiles()
            } else {
                requestedURLs = []
                activity = .idle
                delegate?.peerDidChange(self)
                let detail = reason.map { " (\($0))" } ?? ""
                delegate?.peer(self, didFailWith: "\(displayName) declined the transfer\(detail).")
            }

        case .header(let header):
            handleFileHeader(header)

        case .partition:
            // The sender paused for our acknowledgement.
            session.send(.partitionReceived)

        case .partitionReceived:
            sendNextPartition()

        case .progress(let progress):
            if case .sending = activity {
                activity = .sending(progress: min(1, progress))
                delegate?.peerDidChange(self)
            }

        case .fileTransferComplete:
            outgoingBytesSentBeforeCurrentFile += outgoingBytesSentInCurrentFile
            outgoingBytesSentInCurrentFile = 0
            filesSentInBatch += 1
            releaseCurrentFile()

            if outgoingQueue.isEmpty {
                let count = filesSentInBatch
                filesSentInBatch = 0
                activity = .idle
                delegate?.peerDidChange(self)
                delegate?.peer(self, didFinishSending: count)
            } else {
                dequeueNextFile()
            }

        case .messageTransferComplete:
            break

        case .text(let text):
            delegate?.peer(self, didReceiveText: text)
            session.send(.messageTransferComplete)

        case .displayNameChanged(let name):
            announcedDisplayName = name.isEmpty ? nil : name
            delegate?.peerDidChange(self)
        }
    }
}
