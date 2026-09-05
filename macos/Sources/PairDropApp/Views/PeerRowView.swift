import PairDropKit
import SwiftUI

/// One nearby device. The whole row is a drop target.
struct PeerRowView: View {

    let model: AppModel
    let peer: PairDropPeer

    @State private var isTargeted = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                icon

                VStack(alignment: .leading, spacing: 1) {
                    Text(peer.displayName)
                        .font(.system(size: 13, weight: .medium))
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }

                Spacer(minLength: 4)

                trailing
            }

            if let progress = peer.activity.progress {
                ProgressView(value: progress)
                    .progressViewStyle(.linear)
                    .controlSize(.small)
            }

            if let request = peer.pendingRequest {
                incomingRequest(request)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(background)
        .contentShape(RoundedRectangle(cornerRadius: 8))
        .pairDropTarget(isTargeted: $isTargeted) { payload in
            switch payload {
            case .files(let urls): model.send(urls: urls, to: peer)
            case .text(let text): model.send(text: text, to: peer)
            }
        }
        .contextMenu {
            Button("Send Files…") { chooseFiles() }
            Button("Send Message…") { model.composingTextFor = peer }
            if let hash = peer.connectionHash {
                Divider()
                Text("Connection \(formatted(hash))")
            }
        }
        .animation(.easeOut(duration: 0.15), value: isTargeted)
    }

    // MARK: Pieces

    private var icon: some View {
        Image(systemName: peer.isMobile ? "iphone" : "desktopcomputer")
            .font(.system(size: 17))
            .foregroundStyle(isReady ? Color.accentColor : Color.secondary)
            .frame(width: 24)
    }

    @ViewBuilder
    private var trailing: some View {
        switch peer.activity {
        case .preparing, .awaitingResponse:
            ProgressView().controlSize(.small).scaleEffect(0.7)
        case .sending(let p), .receiving(let p):
            Text("\(Int(p * 100))%")
                .font(.system(size: 11, weight: .medium).monospacedDigit())
                .foregroundStyle(.secondary)
        case .incomingRequest:
            EmptyView()
        case .idle:
            if peer.isPaired {
                Image(systemName: "link")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .help("Paired device")
            }
        }
    }

    @ViewBuilder
    private var background: some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(isTargeted ? Color.accentColor.opacity(0.18) : Color.primary.opacity(0.04))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(Color.accentColor, lineWidth: isTargeted ? 1.5 : 0)
            }
    }

    private func incomingRequest(_ request: TransferRequest) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(requestDescription(request))
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 6) {
                Button("Accept") { peer.respondToRequest(accepted: true) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                Button("Decline") { peer.respondToRequest(accepted: false) }
                    .controlSize(.small)
            }
        }
        .padding(.top, 2)
    }

    // MARK: Text

    private var isReady: Bool {
        peer.connectionState == .connected
    }

    private var subtitle: String {
        switch peer.activity {
        case .preparing: return "Preparing…"
        case .awaitingResponse: return "Waiting for them to accept…"
        case .sending: return "Sending…"
        case .receiving: return "Receiving…"
        case .incomingRequest: return "Wants to send you files"
        case .idle: break
        }

        switch peer.connectionState {
        case .connecting: return "Connecting…"
        case .disconnected: return "Disconnected"
        case .failed(let reason): return reason
        case .connected: return peer.deviceName.isEmpty ? "Ready" : peer.deviceName
        }
    }

    private func requestDescription(_ request: TransferRequest) -> String {
        let size = ByteCountFormatter.string(fromByteCount: request.totalSize, countStyle: .file)
        if request.header.count == 1 {
            return "\(request.header[0].name) · \(size)"
        }
        return "\(request.header.count) files · \(size)"
    }

    /// PairDrop shows the verification hash in groups of four.
    private func formatted(_ hash: String) -> String {
        stride(from: 0, to: hash.count, by: 4).map { offset in
            let start = hash.index(hash.startIndex, offsetBy: offset)
            let end = hash.index(start, offsetBy: min(4, hash.count - offset))
            return String(hash[start..<end])
        }.joined(separator: " ")
    }

    private func chooseFiles() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.prompt = "Send"
        panel.message = "Send to \(peer.displayName)"
        guard panel.runModal() == .OK, !panel.urls.isEmpty else { return }
        model.send(urls: panel.urls, to: peer)
    }
}
