import Foundation

// Control frames exchanged over the peer-to-peer data channel.
// Mirrors the `Peer` class in public/scripts/network.js.

public struct FileHeader: Codable, Hashable, Sendable {
    public let name: String
    public let mime: String
    public let size: Int64

    public init(name: String, mime: String, size: Int64) {
        self.name = name
        self.mime = mime
        self.size = size
    }
}

public struct TransferRequest: Sendable {
    public let header: [FileHeader]
    public let totalSize: Int64
    public let imagesOnly: Bool
    public let thumbnailDataUrl: String?

    public init(header: [FileHeader], totalSize: Int64, imagesOnly: Bool, thumbnailDataUrl: String?) {
        self.header = header
        self.totalSize = totalSize
        self.imagesOnly = imagesOnly
        self.thumbnailDataUrl = thumbnailDataUrl
    }
}

public enum TransferMessage: Sendable {
    case request(TransferRequest)
    case filesTransferResponse(accepted: Bool, reason: String?)
    case header(FileHeader)
    case partition(offset: Int64)
    case partitionReceived
    case progress(Double)
    case fileTransferComplete
    case messageTransferComplete
    case text(String)
    case displayNameChanged(String)

    // MARK: Encoding

    public var json: [String: Any] {
        switch self {
        case .request(let request):
            var payload: [String: Any] = [
                "type": "request",
                "header": request.header.map { ["name": $0.name, "mime": $0.mime, "size": $0.size] },
                "totalSize": request.totalSize,
                "imagesOnly": request.imagesOnly
            ]
            // The web client always sends the key, using "" when there is no preview.
            payload["thumbnailDataUrl"] = request.thumbnailDataUrl ?? ""
            return payload

        case .filesTransferResponse(let accepted, let reason):
            var payload: [String: Any] = ["type": "files-transfer-response", "accepted": accepted]
            if let reason { payload["reason"] = reason }
            return payload

        case .header(let header):
            return ["type": "header", "size": header.size, "name": header.name, "mime": header.mime]

        case .partition(let offset):
            return ["type": "partition", "offset": offset]

        case .partitionReceived:
            // The web client echoes the whole `partition` frame back as `offset`; the sender
            // ignores the value entirely, so a plain offset-less ack is equivalent.
            return ["type": "partition-received", "offset": 0]

        case .progress(let progress):
            return ["type": "progress", "progress": progress]

        case .fileTransferComplete:
            return ["type": "file-transfer-complete"]

        case .messageTransferComplete:
            return ["type": "message-transfer-complete"]

        case .text(let text):
            // btoa(unescape(encodeURIComponent(text))) === base64 of the UTF-8 bytes
            return ["type": "text", "text": Data(text.utf8).base64EncodedString()]

        case .displayNameChanged(let name):
            return ["type": "display-name-changed", "displayName": name]
        }
    }

    public func encoded() -> Data? {
        try? JSONSerialization.data(withJSONObject: json)
    }

    // MARK: Decoding

    public static func parse(_ data: Data) -> TransferMessage? {
        guard let object = try? JSONSerialization.jsonObject(with: data),
              let dict = object as? [String: Any] else { return nil }
        return parse(dict)
    }

    public static func parse(_ dict: [String: Any]) -> TransferMessage? {
        guard let type = dict["type"] as? String else { return nil }

        switch type {
        case "request":
            let headers = (dict["header"] as? [[String: Any]] ?? []).compactMap { entry -> FileHeader? in
                guard let name = entry["name"] as? String else { return nil }
                let size = (entry["size"] as? NSNumber)?.int64Value ?? 0
                return FileHeader(name: name, mime: entry["mime"] as? String ?? "", size: size)
            }
            let total = (dict["totalSize"] as? NSNumber)?.int64Value ?? headers.reduce(0) { $0 + $1.size }
            let thumbnail = dict["thumbnailDataUrl"] as? String
            return .request(TransferRequest(header: headers,
                                            totalSize: total,
                                            imagesOnly: dict["imagesOnly"] as? Bool ?? false,
                                            thumbnailDataUrl: (thumbnail?.isEmpty ?? true) ? nil : thumbnail))

        case "files-transfer-response":
            return .filesTransferResponse(accepted: dict["accepted"] as? Bool ?? false,
                                          reason: dict["reason"] as? String)

        case "header":
            guard let name = dict["name"] as? String else { return nil }
            let size = (dict["size"] as? NSNumber)?.int64Value ?? 0
            return .header(FileHeader(name: name, mime: dict["mime"] as? String ?? "", size: size))

        case "partition":
            return .partition(offset: (dict["offset"] as? NSNumber)?.int64Value ?? 0)

        case "partition-received":
            return .partitionReceived

        case "progress":
            return .progress((dict["progress"] as? NSNumber)?.doubleValue ?? 0)

        case "file-transfer-complete":
            return .fileTransferComplete

        case "message-transfer-complete":
            return .messageTransferComplete

        case "text":
            guard let encoded = dict["text"] as? String,
                  let data = Data(base64Encoded: encoded),
                  let text = String(data: data, encoding: .utf8) else { return nil }
            return .text(text)

        case "display-name-changed":
            guard let name = dict["displayName"] as? String else { return nil }
            return .displayNameChanged(name)

        default:
            return nil
        }
    }
}
