import XCTest
@testable import PairDropKit

/// These lock down the parts of the wire format where a subtle difference from the
/// JavaScript client would silently break interop rather than fail loudly.
final class Cyrb53Tests: XCTestCase {

    /// Expected values produced by running the original `cyrb53` from
    /// public/scripts/util.js in Node on the same inputs.
    func testMatchesJavaScriptReference() {
        let vectors: [(String, UInt64)] = [
            ("", 3338908027751811),
            ("a", 7929297801672961),
            ("PairDrop", 3259817742790581),
            ("sha-256 AB:CD:EF:01:23:45:67:89sha-256 98:76:54:32:10:FE:DC:BA", 8763102360577714),
            ("d41d8cd98f00b204e9800998ecf8427e", 6911763364504760),
            (String(repeating: "0123456789", count: 10), 1336842503148492)
        ]

        for (input, expected) in vectors {
            XCTAssertEqual(Cyrb53.hash(input), expected, "cyrb53(\(input.prefix(20)))")
        }
    }

    func testConnectionHashIsPaddedToSixteenDigits() {
        XCTAssertEqual(Cyrb53.connectionHash("PairDrop").count, 16)
        // A value shorter than 16 digits must be left-padded, as the web client does.
        XCTAssertTrue(Cyrb53.connectionHash("").allSatisfy(\.isNumber))
    }
}

final class ServerEndpointTests: XCTestCase {

    func testBareHostDefaultsToHTTPS() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint(address: "drop.example.com"))
        XCTAssertTrue(endpoint.isSecure)
        XCTAssertEqual(endpoint.wsDomain, "drop.example.com/")
    }

    func testLanAddressDefaultsToHTTP() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint(address: "192.168.1.50:3000"))
        XCTAssertFalse(endpoint.isSecure)
        XCTAssertEqual(endpoint.wsDomain, "192.168.1.50:3000/")
    }

    /// The web client builds `protocol://host+pathname + "server"`, so a subpath
    /// deployment has to keep its prefix.
    func testWebSocketURLIncludesSubpath() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint(address: "https://example.com/pairdrop"))
        let url = try XCTUnwrap(endpoint.webSocketURL(signalingServer: nil, peerId: nil, peerIdHash: nil))
        XCTAssertEqual(url.scheme, "wss")
        XCTAssertEqual(url.path, "/pairdrop/server")
        XCTAssertEqual(url.query, "webrtc_supported=true")
    }

    func testWebSocketURLCarriesPeerIdentity() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint(address: "http://localhost:3000"))
        let url = try XCTUnwrap(endpoint.webSocketURL(signalingServer: nil,
                                                      peerId: "abc",
                                                      peerIdHash: "def"))
        let query = try XCTUnwrap(url.query)
        XCTAssertTrue(query.contains("peer_id=abc"))
        XCTAssertTrue(query.contains("peer_id_hash=def"))
    }

    func testSignalingServerOverrideWins() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint(address: "https://example.com"))
        let url = try XCTUnwrap(endpoint.webSocketURL(signalingServer: "signal.example.net/",
                                                      peerId: nil,
                                                      peerIdHash: nil))
        XCTAssertEqual(url.host, "signal.example.net")
        XCTAssertEqual(url.path, "/server")
    }

    func testRejectsGarbage() {
        XCTAssertNil(ServerEndpoint(address: ""))
        XCTAssertNil(ServerEndpoint(address: "ftp://example.com"))
    }

    /// The server sends `signalingServer: false` when unset, not a string.
    func testConfigDecodesFalseAsAbsent() throws {
        let data = Data(#"{"signalingServer":false,"buttons":{}}"#.utf8)
        let config = try JSONDecoder().decode(InstanceConfig.self, from: data)
        XCTAssertNil(config.signalingServer)
    }
}

final class TransferMessageTests: XCTestCase {

    func testTextIsBase64OfUTF8() throws {
        // btoa(unescape(encodeURIComponent(text))) in the web client.
        let message = TransferMessage.text("héllo 🌍")
        let json = message.json
        XCTAssertEqual(json["text"] as? String, "aMOpbGxvIPCfjI0=")

        let round = try XCTUnwrap(TransferMessage.parse(try XCTUnwrap(message.encoded())))
        guard case .text(let decoded) = round else { return XCTFail("wrong case") }
        XCTAssertEqual(decoded, "héllo 🌍")
    }

    func testRequestRoundTrip() throws {
        let request = TransferRequest(
            header: [FileHeader(name: "a.png", mime: "image/png", size: 12),
                     FileHeader(name: "b.png", mime: "image/png", size: 34)],
            totalSize: 46,
            imagesOnly: true,
            thumbnailDataUrl: nil
        )
        let data = try XCTUnwrap(TransferMessage.request(request).encoded())
        guard case .request(let parsed)? = TransferMessage.parse(data) else {
            return XCTFail("did not parse back as a request")
        }
        XCTAssertEqual(parsed.header, request.header)
        XCTAssertEqual(parsed.totalSize, 46)
        XCTAssertTrue(parsed.imagesOnly)
    }

    /// The web client always includes the key, using "" for no preview.
    func testRequestAlwaysCarriesThumbnailKey() {
        let json = TransferMessage.request(
            TransferRequest(header: [], totalSize: 0, imagesOnly: false, thumbnailDataUrl: nil)
        ).json
        XCTAssertEqual(json["thumbnailDataUrl"] as? String, "")
    }

    func testParsesLargeSizesAsInt64() throws {
        let data = Data(#"{"type":"header","name":"big.iso","mime":"","size":5368709120}"#.utf8)
        guard case .header(let header)? = TransferMessage.parse(data) else {
            return XCTFail("did not parse")
        }
        XCTAssertEqual(header.size, 5_368_709_120)
    }
}

final class ServerMessageTests: XCTestCase {

    func testParsesPeersFrame() throws {
        let json = """
        {"type":"peers","roomType":"ip","roomId":"127.0.0.1","peers":[
          {"id":"1111","rtcSupported":true,
           "name":{"model":null,"os":"Mac OS","browser":"Safari","type":null,
                   "deviceName":"Mac Safari","displayName":"Amethyst Orca"}}]}
        """
        guard case .peers(let peers, let room)? = ServerMessage.parse(Data(json.utf8)) else {
            return XCTFail("did not parse")
        }
        XCTAssertEqual(room.type, .ip)
        XCTAssertEqual(room.id, "127.0.0.1")
        XCTAssertEqual(peers.first?.name.displayName, "Amethyst Orca")
        XCTAssertEqual(peers.first?.rtcSupported, true)
    }

    /// `urls` is a bare string in the default rtc_config and an array in others.
    func testIceServerUrlsAcceptEitherShape() throws {
        let json = """
        {"type":"ws-config","wsConfig":{"wsFallback":true,"rtcConfig":{
          "sdpSemantics":"unified-plan",
          "iceServers":[{"urls":"stun:stun.l.google.com:19302"},
                        {"urls":["turns:a:5349","turn:a:3478"],"username":"u","credential":"c"}]}}}
        """
        guard case .wsConfig(let config)? = ServerMessage.parse(Data(json.utf8)) else {
            return XCTFail("did not parse")
        }
        XCTAssertEqual(config.wsFallback, true)
        XCTAssertEqual(config.rtcConfig?.iceServers.first?.urls, ["stun:stun.l.google.com:19302"])
        XCTAssertEqual(config.rtcConfig?.iceServers.last?.urls.count, 2)
        XCTAssertEqual(config.rtcConfig?.iceServers.last?.username, "u")
    }

    func testSignalCarriesSDPAndSender() throws {
        let json = """
        {"type":"signal","roomType":"ip","roomId":"127.0.0.1",
         "sender":{"id":"abc","rtcSupported":true},
         "sdp":{"type":"offer","sdp":"v=0\\r\\n"}}
        """
        guard case .signal(let sender, _, let sdp, let ice, _)? = ServerMessage.parse(Data(json.utf8)) else {
            return XCTFail("did not parse")
        }
        XCTAssertEqual(sender.id, "abc")
        XCTAssertEqual(sdp?.type, "offer")
        XCTAssertNil(ice)
    }

    func testUnknownTypeDoesNotCrash() {
        guard case .unknown(let type)? = ServerMessage.parse(Data(#"{"type":"future-thing"}"#.utf8)) else {
            return XCTFail("expected unknown")
        }
        XCTAssertEqual(type, "future-thing")
    }
}

final class FileReceiverTests: XCTestCase {

    /// The sender chooses the filename, so it must not be able to escape the
    /// download folder or hide the result.
    func testSanitizesHostileFilenames() {
        XCTAssertEqual(FileReceiver.sanitize("../../etc/passwd"), "passwd")
        XCTAssertEqual(FileReceiver.sanitize("/etc/passwd"), "passwd")
        XCTAssertEqual(FileReceiver.sanitize(".bashrc"), "bashrc")
        XCTAssertEqual(FileReceiver.sanitize("a/b.txt"), "b.txt")
        XCTAssertEqual(FileReceiver.sanitize("   "), "Received file")
        XCTAssertEqual(FileReceiver.sanitize("ok name.txt"), "ok name.txt")
    }

    func testAvoidsOverwritingExistingFiles() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let first = FileReceiver.uniqueURL(in: directory, for: "note.txt")
        XCTAssertEqual(first.lastPathComponent, "note.txt")
        try Data("x".utf8).write(to: first)

        let second = FileReceiver.uniqueURL(in: directory, for: "note.txt")
        XCTAssertEqual(second.lastPathComponent, "note (2).txt")
    }
}
