// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "PairDropKit",
    platforms: [
        .macOS(.v14),
        .iOS(.v17)
    ],
    products: [
        .library(name: "PairDropKit", targets: ["PairDropKit"])
    ],
    dependencies: [
        .package(url: "https://github.com/stasel/WebRTC.git", from: "152.0.0")
    ],
    targets: [
        .target(
            name: "PairDropKit",
            dependencies: [.product(name: "WebRTC", package: "WebRTC")]
        ),
        // Headless peer for testing the protocol without a GUI.
        .executableTarget(
            name: "pairdrop-probe",
            dependencies: ["PairDropKit"]
        ),
        .testTarget(
            name: "PairDropKitTests",
            dependencies: ["PairDropKit"]
        ),
    ]
)
