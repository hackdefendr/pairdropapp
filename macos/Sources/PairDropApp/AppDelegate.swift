import AppKit
import Observation
import PairDropKit
import UserNotifications

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {

    private var model: AppModel!
    private var statusController: StatusItemController!
    private var notifier: Notifier!
    private var seenEventIds = Set<UUID>()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Menu bar only: no Dock icon, no main menu window.
        NSApp.setActivationPolicy(.accessory)

        model = AppModel()
        notifier = Notifier()
        statusController = StatusItemController(model: model)

        model.startIfConfigured()
        observeEvents()

        if !model.settings.isConfigured {
            SettingsWindowController.shared.show(model: model)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        model?.client.stop()
    }

    /// Mirrors newly posted client events into system notifications, and reveals
    /// received files if the user asked for that.
    private func observeEvents() {
        withObservationTracking {
            _ = model.client.events.count
        } onChange: {
            Task { @MainActor [weak self] in
                self?.handleNewEvents()
                self?.observeEvents()
            }
        }
    }

    private func handleNewEvents() {
        for event in model.client.events where !seenEventIds.contains(event.id) {
            seenEventIds.insert(event.id)

            switch event.kind {
            case .incomingFiles(let files):
                notifier.post(title: event.peerName ?? "PairDrop", body: event.message)
                if model.settings.revealInFinder { model.reveal(files) }
            case .incomingText(let text):
                model.copyToPasteboard(text)
                notifier.post(title: event.peerName ?? "PairDrop", body: "Copied to clipboard: \(text.prefix(80))")
            case .failure:
                notifier.post(title: "PairDrop", body: event.message)
            case .success, .info:
                break
            }
        }

        // Keep the seen set from growing without bound.
        let live = Set(model.client.events.map(\.id))
        seenEventIds.formIntersection(live)
    }
}

/// Best-effort system notifications.
///
/// `UNUserNotificationCenter.current()` traps when the process isn't a real, signed
/// app bundle, so it is only touched once we've confirmed we are one. When it isn't
/// available the in-panel activity list is the surface.
@MainActor
final class Notifier {

    private let isAvailable: Bool

    init() {
        isAvailable = Bundle.main.bundleIdentifier != nil
            && Bundle.main.bundleURL.pathExtension == "app"

        guard isAvailable else { return }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    func post(title: String, body: String) {
        guard isAvailable else { return }

        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body

        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: nil)
    }
}
