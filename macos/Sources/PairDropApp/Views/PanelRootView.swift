import AppKit
import PairDropKit
import SwiftUI

/// Everything inside the menu bar panel.
struct PanelRootView: View {

    let model: AppModel

    private var client: PairDropClient { model.client }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            // A borderless panel is a poor host for a real sheet, so composing takes
            // over the body instead of floating above it.
            if let peer = model.composingTextFor {
                SendTextComposer(model: model, peer: peer)
            } else if !model.settings.isConfigured {
                setupPrompt
            } else {
                peerList
            }

            if model.composingTextFor == nil, !client.events.isEmpty {
                Divider()
                activityFeed
            }

            Divider()
            footer
        }
        .frame(width: 320)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 8) {
            Text("PairDrop")
                .font(.system(size: 13, weight: .semibold))

            Spacer()

            HStack(spacing: 5) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 7, height: 7)
                Text(statusText)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
    }

    private var statusColor: Color {
        switch client.connectionState {
        case .connected: return .green
        case .connecting: return .orange
        case .waitingToRetry: return .orange
        case .failed: return .red
        case .idle: return .secondary
        }
    }

    private var statusText: String {
        switch client.connectionState {
        case .connected: return client.assignedDisplayName.map { "You are \($0)" } ?? "Connected"
        case .connecting: return "Connecting…"
        case .waitingToRetry(let seconds): return "Retrying in \(seconds)s"
        case .failed(let message): return message
        case .idle: return "Not connected"
        }
    }

    // MARK: Body states

    private var setupPrompt: some View {
        VStack(spacing: 8) {
            Image(systemName: "server.rack")
                .font(.system(size: 22))
                .foregroundStyle(.secondary)
            Text("Set your PairDrop server")
                .font(.system(size: 12, weight: .medium))
            Text("Point the app at your self-hosted instance to see nearby devices.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Open Settings…") { SettingsWindowController.shared.show(model: model) }
                .controlSize(.small)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 22)
    }

    @ViewBuilder
    private var peerList: some View {
        if client.peers.isEmpty {
            emptyState
        } else {
            ScrollView {
                VStack(spacing: 4) {
                    ForEach(client.peers) { peer in
                        PeerRowView(model: model, peer: peer)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
            }
            // A ScrollView's ideal height is zero, and the panel sizes itself to the
            // content's fitting size — without a floor the list collapses away.
            .frame(minHeight: 76, maxHeight: 320)
        }
    }

    private var emptyState: some View {
        VStack(spacing: 6) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 20))
                .foregroundStyle(.secondary)
            Text("No devices nearby")
                .font(.system(size: 12, weight: .medium))
            Text("Open PairDrop on another device on this network.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 24)
    }

    // MARK: Activity

    private var activityFeed: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Recent")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Clear") { client.clearEvents() }
                    .buttonStyle(.plain)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .padding(.bottom, 4)

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(client.events.reversed().prefix(4)) { event in
                        EventRowView(model: model, event: event)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
            }
            .frame(minHeight: 34, maxHeight: 140)
        }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 12) {
            Button {
                model.isShowingPairingSheet = true
                SettingsWindowController.shared.show(model: model, tab: .devices)
            } label: {
                Label("Pair", systemImage: "link.badge.plus")
                    .labelStyle(.iconOnly)
            }
            .buttonStyle(.plain)
            .help("Pair with a device on another network")

            Button {
                SettingsWindowController.shared.show(model: model)
            } label: {
                Label("Settings", systemImage: "gearshape")
                    .labelStyle(.iconOnly)
            }
            .buttonStyle(.plain)
            .help("Settings")

            Spacer()

            Button("Quit") { NSApp.terminate(nil) }
                .buttonStyle(.plain)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}

// MARK: - Event row

struct EventRowView: View {

    let model: AppModel
    let event: PairDropEvent

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 11))
                .foregroundStyle(tint)
                .frame(width: 14)

            VStack(alignment: .leading, spacing: 0) {
                Text(event.message)
                    .font(.system(size: 11))
                    .lineLimit(1)
                if let peerName = event.peerName {
                    Text(peerName)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
            }

            Spacer(minLength: 4)

            action
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 3)
    }

    @ViewBuilder
    private var action: some View {
        switch event.kind {
        case .incomingFiles(let files):
            Button("Show") { model.reveal(files) }
                .buttonStyle(.plain)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Color.accentColor)
        case .incomingText(let text):
            Button("Copy") { model.copyToPasteboard(text) }
                .buttonStyle(.plain)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Color.accentColor)
        default:
            EmptyView()
        }
    }

    private var symbol: String {
        switch event.kind {
        case .success: return "checkmark.circle.fill"
        case .failure: return "exclamationmark.triangle.fill"
        case .info: return "info.circle"
        case .incomingFiles: return "arrow.down.circle.fill"
        case .incomingText: return "text.bubble.fill"
        }
    }

    private var tint: Color {
        switch event.kind {
        case .success, .incomingFiles, .incomingText: return .green
        case .failure: return .orange
        case .info: return .secondary
        }
    }
}

// MARK: - Text composer

struct SendTextComposer: View {

    let model: AppModel
    let peer: PairDropPeer

    @State private var text = ""
    @FocusState private var isFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Message to \(peer.displayName)")
                .font(.system(size: 12, weight: .medium))

            TextEditor(text: $text)
                .font(.system(size: 12))
                .scrollContentBackground(.hidden)
                .padding(4)
                .frame(height: 88)
                .background(Color.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 5))
                .focused($isFocused)

            HStack {
                Spacer()
                Button("Cancel") { model.composingTextFor = nil }
                    .controlSize(.small)
                Button("Send") { send() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(12)
        .onAppear { isFocused = true }
    }

    private func send() {
        model.send(text: text, to: peer)
        model.composingTextFor = nil
    }
}
