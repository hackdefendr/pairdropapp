import Foundation
import UniformTypeIdentifiers

public enum TransferConstants {
    /// Matches FileChunker in public/scripts/network.js. Both values are part of the
    /// wire contract: the receiver acknowledges one partition at a time.
    public static let chunkSize = 64_000
    public static let maxPartitionSize = 1_000_000
}

// MARK: - Outgoing

/// Streams a file off disk in the chunk/partition rhythm PairDrop expects.
/// Not thread-safe; drive it from a single serial queue.
final class FileChunker: @unchecked Sendable {

    let url: URL
    let size: Int64

    private let handle: FileHandle
    private(set) var offset: Int64 = 0
    private var partitionBytes = 0

    init(url: URL) throws {
        self.url = url
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        self.size = (attributes[.size] as? NSNumber)?.int64Value ?? 0
        self.handle = try FileHandle(forReadingFrom: url)
    }

    deinit {
        try? handle.close()
    }

    var isFileEnd: Bool { offset >= size }

    var isPartitionEnd: Bool { partitionBytes >= TransferConstants.maxPartitionSize }

    func beginPartition() {
        partitionBytes = 0
    }

    /// Returns the next chunk, or nil at end of file.
    func nextChunk() throws -> Data? {
        guard !isFileEnd else { return nil }
        let data = try handle.read(upToCount: TransferConstants.chunkSize) ?? Data()
        guard !data.isEmpty else {
            // File shrank underneath us; treat as end of file.
            offset = size
            return nil
        }
        offset += Int64(data.count)
        partitionBytes += data.count
        return data
    }

    func close() {
        try? handle.close()
    }
}

// MARK: - Incoming

/// Reassembles an incoming file, streaming straight to disk so a multi-gigabyte
/// transfer never has to fit in memory.
final class FileReceiver: @unchecked Sendable {

    let header: FileHeader
    private(set) var bytesReceived: Int64 = 0

    private let temporaryURL: URL
    private let handle: FileHandle

    init(header: FileHeader) throws {
        self.header = header
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("PairDrop", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        temporaryURL = directory.appendingPathComponent(UUID().uuidString)
        FileManager.default.createFile(atPath: temporaryURL.path, contents: nil)
        handle = try FileHandle(forWritingTo: temporaryURL)
    }

    var isComplete: Bool { bytesReceived >= header.size }

    /// - Returns: true once the declared size has been reached.
    @discardableResult
    func append(_ data: Data) throws -> Bool {
        guard !data.isEmpty else { return isComplete }
        try handle.write(contentsOf: data)
        bytesReceived += Int64(data.count)
        return isComplete
    }

    /// Closes the file and moves it to `directory`, avoiding collisions the way the
    /// Finder does — `name (2).ext`.
    func finish(movingTo directory: URL) throws -> URL {
        try handle.close()

        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let destination = FileReceiver.uniqueURL(in: directory, for: header.name)
        try FileManager.default.moveItem(at: temporaryURL, to: destination)
        return destination
    }

    func discard() {
        try? handle.close()
        try? FileManager.default.removeItem(at: temporaryURL)
    }

    static func uniqueURL(in directory: URL, for filename: String) -> URL {
        let safeName = sanitize(filename)
        var candidate = directory.appendingPathComponent(safeName)
        guard FileManager.default.fileExists(atPath: candidate.path) else { return candidate }

        let base = (safeName as NSString).deletingPathExtension
        let ext = (safeName as NSString).pathExtension
        var counter = 2
        repeat {
            let name = ext.isEmpty ? "\(base) (\(counter))" : "\(base) (\(counter)).\(ext)"
            candidate = directory.appendingPathComponent(name)
            counter += 1
        } while FileManager.default.fileExists(atPath: candidate.path)
        return candidate
    }

    /// A peer controls the filename, so strip anything that could escape the target
    /// directory or hide the file.
    static func sanitize(_ filename: String) -> String {
        let name = (filename as NSString).lastPathComponent
        var cleaned = name.replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: ":", with: "_")
            .replacingOccurrences(of: "\0", with: "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        while cleaned.hasPrefix(".") { cleaned.removeFirst() }
        if cleaned.isEmpty || cleaned == ".." { cleaned = "Received file" }
        return String(cleaned.prefix(200))
    }
}

// MARK: - Helpers

public enum MimeType {
    /// Best-effort MIME for an outgoing file. PairDrop uses it to decide whether a
    /// transfer is images-only and to name the download correctly.
    public static func of(_ url: URL) -> String {
        if let type = UTType(filenameExtension: url.pathExtension),
           let mime = type.preferredMIMEType {
            return mime
        }
        return "application/octet-stream"
    }

    public static func isImage(_ mime: String) -> Bool {
        mime.hasPrefix("image/")
    }
}
