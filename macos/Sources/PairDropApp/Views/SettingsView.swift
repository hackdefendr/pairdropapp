import AppKit
import PairDropKit
import SwiftUI

enum SettingsTab: Hashable {
    case general
    case devices
}

/// Which tab is showing. Held outside the view so reopening Settings can switch tabs —
/// `@State` would keep the value from the first time the view was built.
@MainActor
@Observable
final class SettingsSelection {
    var tab: SettingsTab = .general
}

struct SettingsView: View {

    let model: AppModel
    @Bindable var selection: SettingsSelection

    var body: some View {
        TabView(selection: $selection.tab) {
            GeneralSettingsView(model: model)
                .tabItem { Label("General", systemImage: "gearshape") }
                .tag(SettingsTab.general)

            PairedDevicesView(model: model)
                .tabItem { Label("Devices", systemImage: "link") }
                .tag(SettingsTab.devices)
        }
        // A Form reports almost no ideal height, and the window sizes itself to that —
        // without an explicit height the tab bar shows with nothing underneath it.
        .frame(width: 480, height: 520)
        .padding(.top, 8)
    }
}

// MARK: - General

struct GeneralSettingsView: View {

    let model: AppModel

    @State private var address = ""
    @State private var name = ""
    @State private var openAtLogin = false
    @State private var loginItemProblem: String?

    private var settings: AppSettings { model.settings }

    var body: some View {
        Form {
            Section {
                TextField("Server", text: $address, prompt: Text("https://drop.example.com"))
                    .onSubmit(applyServer)
                Text("Your self-hosted PairDrop instance. The same address you open in a browser.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Toggle("Trust self-signed certificates", isOn: Binding(
                    get: { settings.allowUntrustedTLS },
                    set: { settings.allowUntrustedTLS = $0; model.applySettings() }
                ))
                .help("Only enable this for an instance on your own network.")

                HStack {
                    Button("Connect") { applyServer() }
                        .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
                    statusLabel
                }
            }

            Section {
                TextField("Device name", text: $name, prompt: Text(DeviceIdentity.machineName()))
                    .onSubmit(applyName)
                Text("What other devices call this Mac.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                LabeledContent("Save files to") {
                    HStack {
                        Text(settings.downloadDirectory.path)
                            .font(.caption)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Button("Choose…") { chooseFolder() }
                    }
                }
                Toggle("Reveal received files in Finder", isOn: Binding(
                    get: { settings.revealInFinder },
                    set: { settings.revealInFinder = $0 }
                ))
            }

            Section {
                Toggle("Open PairDrop at login", isOn: Binding(
                    get: { openAtLogin },
                    set: { newValue in
                        loginItemProblem = LoginItem.setEnabled(newValue)
                        openAtLogin = LoginItem.isEnabled
                    }
                ))
                .disabled(!LoginItem.isAvailable)

                if let loginItemProblem {
                    Text(loginItemProblem)
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                } else if !LoginItem.isAvailable {
                    Text("Available once PairDrop is in your Applications folder.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                LabeledContent("Version") {
                    Text(Self.versionString)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            address = settings.serverAddress
            name = settings.displayName
            openAtLogin = LoginItem.isEnabled
        }
    }

    private static var versionString: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "—"
        let build = info?["CFBundleVersion"] as? String ?? "—"
        return "\(short) (\(build))"
    }

    @ViewBuilder
    private var statusLabel: some View {
        switch model.client.connectionState {
        case .connected:
            Label("Connected", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.caption)
        case .connecting:
            Text("Connecting…").font(.caption).foregroundStyle(.secondary)
        case .waitingToRetry(let seconds):
            Text("Retrying in \(seconds)s").font(.caption).foregroundStyle(.orange)
        case .failed(let message):
            Text(message).font(.caption).foregroundStyle(.red).lineLimit(2)
        case .idle:
            Text("Not connected").font(.caption).foregroundStyle(.secondary)
        }
    }

    private func applyServer() {
        settings.serverAddress = address.trimmingCharacters(in: .whitespaces)
        model.applySettings()
    }

    private func applyName() {
        settings.displayName = name.trimmingCharacters(in: .whitespaces)
        model.applySettings()
    }

    private func chooseFolder() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = settings.downloadDirectory
        panel.prompt = "Choose"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        settings.setDownloadDirectory(url)
        model.applySettings()
    }
}

// MARK: - Paired devices

struct PairedDevicesView: View {

    let model: AppModel

    @State private var devices: [RoomSecretStore.Entry] = []
    @State private var key = ""

    var body: some View {
        Form {
            Section("Pair a device") {
                Text("Pairing lets two devices find each other even on different networks.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let pairing = model.client.pairing {
                    HStack {
                        Text(pairing.pairKey)
                            .font(.system(size: 28, weight: .semibold, design: .monospaced))
                            .textSelection(.enabled)
                        Spacer()
                        Button("Cancel") { model.client.cancelPairing() }
                    }
                    Text("Enter this key on the other device.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    HStack {
                        Button("Create pairing key") { model.client.beginPairing() }
                            .disabled(model.client.connectionState != .connected)
                        Spacer()
                    }
                }

                LabeledContent("Their key") {
                    HStack(spacing: 8) {
                        TextField("", text: $key, prompt: Text("123456"))
                            .labelsHidden()
                            .frame(width: 90)
                            .onSubmit(join)
                        Button("Join", action: join)
                            .disabled(key.filter(\.isNumber).count != 6)
                    }
                }
            }

            Section("Paired devices") {
                if devices.isEmpty {
                    Text("No paired devices yet.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(devices, id: \.secret) { device in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(device.displayName)
                                Toggle("Accept files automatically", isOn: Binding(
                                    get: { device.autoAccept },
                                    set: { newValue in
                                        model.client.setAutoAccept(newValue, forSecret: device.secret)
                                        reload()
                                    }
                                ))
                                .font(.caption)
                                .toggleStyle(.checkbox)
                            }
                            Spacer()
                            Button("Unpair") {
                                model.client.unpair(secret: device.secret)
                                reload()
                            }
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear(perform: reload)
        .onChange(of: model.client.pairing) { _, _ in reload() }
    }

    private func join() {
        guard key.filter(\.isNumber).count == 6 else { return }
        model.client.joinPairing(key: key)
        key = ""
    }

    private func reload() {
        devices = model.client.pairedDevices
    }
}

// MARK: - Window host

/// Settings live in a normal window; the app is otherwise menu-bar only.
@MainActor
final class SettingsWindowController {

    static let shared = SettingsWindowController()

    private var window: NSWindow?
    private let selection = SettingsSelection()

    func show(model: AppModel, tab: SettingsTab = .general) {
        selection.tab = tab

        if window == nil {
            let hosting = NSHostingController(rootView: SettingsView(model: model, selection: selection))
            let window = NSWindow(contentViewController: hosting)
            window.title = "PairDrop Settings"
            window.styleMask = [.titled, .closable, .miniaturizable]
            window.isReleasedWhenClosed = false
            // The hosting controller's ideal size is unreliable for Forms, so state the
            // size we want rather than inheriting a collapsed one.
            window.setContentSize(NSSize(width: 480, height: 520))
            window.center()
            self.window = window
        }

        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
    }
}
