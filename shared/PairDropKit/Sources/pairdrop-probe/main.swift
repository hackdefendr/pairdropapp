import Foundation
import PairDropKit

// A headless PairDrop peer, for exercising the protocol from a terminal.
//
//   pairdrop-probe <server> [--name <name>] [--send <file>…] [--to <peer>]
//                           [--out <dir>] [--quit-after <seconds>]
//
// With --send it waits for a matching peer, sends, and exits. Without it, it sits
// there accepting whatever arrives — useful as the other end of a transfer test.

struct Options {
    var server = ""
    var name = "Probe"
    var filesToSend: [URL] = []
    var target: String?
    var outputDirectory = FileManager.default.temporaryDirectory.appendingPathComponent("pairdrop-probe")
    var quitAfter: TimeInterval = 120
}

func parseArguments() -> Options {
    var options = Options()
    var arguments = Array(CommandLine.arguments.dropFirst())

    guard let server = arguments.first, !server.hasPrefix("--") else {
        FileHandle.standardError.write(Data("usage: pairdrop-probe <server> [--name N] [--send FILE…] [--to PEER] [--out DIR] [--quit-after S]\n".utf8))
        exit(2)
    }
    options.server = server
    arguments.removeFirst()

    var index = 0
    while index < arguments.count {
        let flag = arguments[index]
        index += 1
        func next() -> String? {
            guard index < arguments.count, !arguments[index].hasPrefix("--") else { return nil }
            defer { index += 1 }
            return arguments[index]
        }

        switch flag {
        case "--name":
            options.name = next() ?? options.name
        case "--to":
            options.target = next()
        case "--out":
            if let path = next() { options.outputDirectory = URL(fileURLWithPath: path) }
        case "--quit-after":
            options.quitAfter = next().flatMap(TimeInterval.init) ?? options.quitAfter
        case "--send":
            while let path = next() {
                options.filesToSend.append(URL(fileURLWithPath: path).standardizedFileURL)
            }
        default:
            FileHandle.standardError.write(Data("unknown flag \(flag)\n".utf8))
            exit(2)
        }
    }
    return options
}

func log(_ message: String) {
    let stamp = ISO8601DateFormatter().string(from: Date())
    print("[\(stamp)] \(message)")
    fflush(stdout)
}

@MainActor
final class Probe {

    private let options: Options
    private let client: PairDropClient
    private var hasSent = false
    private var seenEvents = 0

    init(options: Options) {
        self.options = options
        try? FileManager.default.createDirectory(at: options.outputDirectory, withIntermediateDirectories: true)
        client = PairDropClient(serverAddress: options.server,
                                downloadDirectory: options.outputDirectory,
                                displayName: options.name,
                                allowUntrustedTLS: true)
        client.autoAcceptEverything = true
    }

    func run() {
        log("connecting to \(options.server) as \"\(options.name)\"")
        client.start()
        poll()

        Timer.scheduledTimer(withTimeInterval: options.quitAfter, repeats: false) { _ in
            Task { @MainActor in
                log("time limit reached; exiting")
                self.client.stop()
                exit(self.hasSent || self.options.filesToSend.isEmpty ? 0 : 1)
            }
        }
        RunLoop.main.run()
    }

    /// Simple polling instead of observation: this is a diagnostic tool, and polling
    /// keeps the whole flow visible in one place.
    private func poll() {
        Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { _ in
            Task { @MainActor in self.tick() }
        }
    }

    private var lastSummary = ""

    private func tick() {
        drainEvents()

        let summary = "state=\(client.connectionState) peers=["
            + client.peers.map {
                "\($0.displayName)/\($0.connectionState)/\($0.activity)/hash=\($0.connectionHash ?? "-")"
            }.joined(separator: ", ")
            + "]"
        if summary != lastSummary {
            lastSummary = summary
            log(summary)
        }

        guard !hasSent, !options.filesToSend.isEmpty else { return }
        guard let peer = matchingPeer(), peer.connectionState == .connected else { return }

        hasSent = true
        log("sending \(options.filesToSend.count) file(s) to \(peer.displayName)")
        client.send(urls: options.filesToSend, to: peer)
    }

    private func matchingPeer() -> PairDropPeer? {
        guard let target = options.target else { return client.peers.first }
        return client.peers.first {
            $0.displayName.localizedCaseInsensitiveContains(target)
        }
    }

    private func drainEvents() {
        let events = client.events
        guard events.count > seenEvents else { return }
        for event in events[seenEvents...] {
            switch event.kind {
            case .incomingFiles(let files):
                for file in files {
                    log("RECEIVED FILE \(file.url.path) (\(file.size) bytes, \(file.mime))")
                }
            case .incomingText(let text):
                log("RECEIVED TEXT: \(text)")
            case .success:
                log("OK: \(event.message)")
                if hasSent {
                    // Give the peer a moment to settle, then exit cleanly.
                    Timer.scheduledTimer(withTimeInterval: 1.0, repeats: false) { _ in
                        Task { @MainActor in
                            self.client.stop()
                            exit(0)
                        }
                    }
                }
            case .failure:
                log("FAIL: \(event.message)")
            case .info:
                log("info: \(event.message)")
            }
        }
        seenEvents = events.count
    }
}

let options = parseArguments()
MainActor.assumeIsolated {
    Probe(options: options).run()
}
