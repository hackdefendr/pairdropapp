import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

/// What the user dragged in: files, or a snippet of text.
enum DropPayload {
    case files([URL])
    case text(String)
}

enum DropReader {

    static let acceptedTypes: [UTType] = [.fileURL, .utf8PlainText, .plainText, .url]

    /// Resolves dragged item providers into something we can send.
    ///
    /// Files win over text: dragging from Finder offers both a file URL and the
    /// filename as a string, and the file is obviously what was meant.
    static func payload(from providers: [NSItemProvider]) async -> DropPayload? {
        var urls: [URL] = []
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            if let url = await loadFileURL(from: provider) {
                urls.append(url)
            }
        }
        if !urls.isEmpty { return .files(urls) }

        for provider in providers {
            if let text = await loadText(from: provider), !text.isEmpty {
                return .text(text)
            }
        }
        return nil
    }

    private static func loadFileURL(from provider: NSItemProvider) async -> URL? {
        await withCheckedContinuation { continuation in
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
                switch item {
                case let url as URL:
                    continuation.resume(returning: url)
                case let data as Data:
                    continuation.resume(returning: URL(dataRepresentation: data, relativeTo: nil))
                default:
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    private static func loadText(from provider: NSItemProvider) async -> String? {
        for type in [UTType.utf8PlainText, .plainText, .url] where provider.hasItemConformingToTypeIdentifier(type.identifier) {
            let value: String? = await withCheckedContinuation { continuation in
                provider.loadItem(forTypeIdentifier: type.identifier, options: nil) { item, _ in
                    switch item {
                    case let string as String:
                        continuation.resume(returning: string)
                    case let url as URL:
                        continuation.resume(returning: url.absoluteString)
                    case let data as Data:
                        continuation.resume(returning: String(data: data, encoding: .utf8))
                    default:
                        continuation.resume(returning: nil)
                    }
                }
            }
            if let value { return value }
        }
        return nil
    }
}

extension View {
    /// Marks this view as a PairDrop drop target.
    func pairDropTarget(isTargeted: Binding<Bool>, perform: @escaping (DropPayload) -> Void) -> some View {
        onDrop(of: DropReader.acceptedTypes, isTargeted: isTargeted) { providers in
            Task { @MainActor in
                guard let payload = await DropReader.payload(from: providers) else { return }
                perform(payload)
            }
            return true
        }
    }
}
