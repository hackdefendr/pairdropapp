import AppKit

// AppKit rather than a SwiftUI `App`: the menu bar item has to accept drags from other
// apps, which needs direct access to the NSStatusItem's button and a floating panel
// that survives a drag. SwiftUI still renders everything inside that panel.
@main
enum PairDropMain {

    /// `NSApplication.delegate` is weak, so the delegate is held here for the
    /// lifetime of the process.
    @MainActor static let delegate = AppDelegate()

    @MainActor static func main() {
        let application = NSApplication.shared
        application.delegate = delegate
        application.run()
    }
}
