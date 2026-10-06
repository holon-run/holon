// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "HolonClientSDK",
    platforms: [.iOS(.v18), .macOS(.v13)],
    products: [
        .library(name: "HolonWire", targets: ["HolonWire"]),
        .library(name: "HolonClient", targets: ["HolonClient"]),
    ],
    targets: [
        // Generated transport models are isolated from the concurrency-safe domain.
        .target(name: "HolonWire", swiftSettings: [.swiftLanguageMode(.v5)]),
        .target(name: "HolonClient", dependencies: ["HolonWire"]),
        .testTarget(name: "HolonClientTests", dependencies: ["HolonClient", "HolonWire"]),
        .testTarget(name: "LiveDaemonProbeTests", dependencies: ["HolonWire"]),
    ]
)
