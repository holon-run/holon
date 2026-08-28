// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "HolonMenu",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(
            name: "HolonMenu",
            targets: ["HolonMenu"]
        ),
    ],
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.9.4"),
    ],
    targets: [
        .executableTarget(
            name: "HolonMenu",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle"),
            ],
            path: "Sources/HolonMenu",
            resources: [
                .process("Resources"),
            ]
        ),
        .testTarget(
            name: "HolonMenuTests",
            dependencies: ["HolonMenu"],
            path: "Tests/HolonMenuTests"
        ),
    ]
)
