// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "PairDropApp",
    platforms: [
        .macOS(.v14)
    ],
    dependencies: [
        .package(path: "../shared/PairDropKit")
    ],
    targets: [
        .executableTarget(
            name: "PairDropApp",
            dependencies: [
                .product(name: "PairDropKit", package: "PairDropKit")
            ]
        )
    ]
)
