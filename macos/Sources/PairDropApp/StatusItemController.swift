import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The menu bar presence: an icon you can click, and a drop target that springs the
/// panel open when you drag files onto it.
@MainActor
final class StatusItemController: NSObject {

    private let model: AppModel
    private let statusItem: NSStatusItem
    private let panel: DropPanel
    private var globalClickMonitor: Any?
    private var localClickMonitor: Any?

    init(model: AppModel) {
        self.model = model
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        self.panel = DropPanel(model: model)
        super.init()

        configureButton()
        panel.onRequestClose = { [weak self] in self?.hidePanel() }
    }

    private func configureButton() {
        guard let button = statusItem.button else { return }

        button.image = NSImage(systemSymbolName: "paperplane", accessibilityDescription: "PairDrop")
        button.image?.isTemplate = true
        button.toolTip = "PairDrop — click to show nearby devices, or drag files here"

        // NSStatusBarButton is created for us and can't be subclassed, so both clicks and
        // drags are handled by a transparent overlay that covers it.
        let dropZone = SpringLoadedDropView(frame: button.bounds)
        dropZone.autoresizingMask = [.width, .height]
        dropZone.onDragEnter = { [weak self] in self?.showPanel() }
        dropZone.onClick = { [weak self] in self?.togglePanel() }
        button.addSubview(dropZone)
    }

    // MARK: - Panel

    private func togglePanel() {
        if panel.isVisible {
            hidePanel()
        } else {
            showPanel()
        }
    }

    func showPanel() {
        guard let button = statusItem.button, let buttonWindow = button.window else { return }

        let buttonRect = buttonWindow.convertToScreen(button.convert(button.bounds, to: nil))
        panel.present(below: buttonRect)
        statusItem.button?.highlight(true)
        installClickMonitors()
    }

    func hidePanel() {
        panel.orderOut(nil)
        statusItem.button?.highlight(false)
        removeClickMonitors()
    }

    /// Dismiss when the user clicks anywhere outside the panel.
    private func installClickMonitors() {
        guard globalClickMonitor == nil else { return }

        globalClickMonitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            Task { @MainActor in self?.hidePanel() }
        }
        localClickMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown]) { [weak self] event in
            guard event.keyCode == 53 else { return event }  // Escape
            Task { @MainActor in self?.hidePanel() }
            return nil
        }
    }

    private func removeClickMonitors() {
        if let globalClickMonitor { NSEvent.removeMonitor(globalClickMonitor) }
        if let localClickMonitor { NSEvent.removeMonitor(localClickMonitor) }
        globalClickMonitor = nil
        localClickMonitor = nil
    }
}

// MARK: - Status item drop zone

/// Opens the panel when a drag hovers the menu bar icon. It never accepts the drop
/// itself — the user picks a device in the panel — so it always reports "no operation".
private final class SpringLoadedDropView: NSView {

    var onDragEnter: (() -> Void)?
    var onClick: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        registerForDraggedTypes([.fileURL, .string, .URL])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        onDragEnter?()
        return []
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation { [] }

    // The overlay covers the button, so it owns the click too.
    override func mouseDown(with event: NSEvent) { onClick?() }
    override func rightMouseDown(with event: NSEvent) { onClick?() }
}

// MARK: - Panel

/// A borderless, non-activating panel.
///
/// A plain `NSPopover` is unreliable here: a drag from Finder never activates our app,
/// and a transient popover dismisses itself before the drop lands. A floating panel
/// stays put through the whole drag.
@MainActor
final class DropPanel: NSPanel {

    var onRequestClose: (() -> Void)?

    private static let contentWidth: CGFloat = 320

    init(model: AppModel) {
        super.init(contentRect: NSRect(x: 0, y: 0, width: DropPanel.contentWidth, height: 400),
                   styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
                   backing: .buffered,
                   defer: false)

        isFloatingPanel = true
        level = .popUpMenu          // above other windows, including during a drag
        hidesOnDeactivate = false
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isMovable = false
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]

        let root = PanelRootView(model: model)
            .frame(width: DropPanel.contentWidth)
        let hosting = NSHostingView(rootView: root)
        hosting.sizingOptions = [.preferredContentSize]
        contentView = hosting
    }

    override var canBecomeKey: Bool { true }

    /// Anchors under the menu bar icon, kept on screen.
    func present(below anchor: NSRect) {
        layoutIfNeeded()
        let fitting = contentView?.fittingSize.height ?? 0
        // Clamp: SwiftUI can report a collapsed height for scrollable content, and a
        // 20-point panel is worse than a slightly-too-tall one.
        let height = min(max(fitting, 180), 620)
        setContentSize(NSSize(width: DropPanel.contentWidth, height: height))

        let screen = NSScreen.screens.first { $0.frame.intersects(anchor) } ?? NSScreen.main
        let visible = screen?.visibleFrame ?? .zero

        var origin = NSPoint(x: anchor.midX - frame.width / 2,
                             y: anchor.minY - frame.height - 6)
        origin.x = min(max(origin.x, visible.minX + 8), visible.maxX - frame.width - 8)
        origin.y = max(origin.y, visible.minY + 8)

        setFrameOrigin(origin)
        orderFrontRegardless()
        makeKey()
    }
}
