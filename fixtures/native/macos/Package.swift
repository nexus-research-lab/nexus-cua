// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "NexusCUANativeFixture",
    platforms: [.macOS(.v14)],
    products: [
        .executable(
            name: "nexus-cua-native-fixture",
            targets: ["NexusCUANativeFixture"]
        )
    ],
    targets: [
        .executableTarget(
            name: "NexusCUANativeFixture",
            path: "Sources"
        )
    ],
    swiftLanguageModes: [.v5]
)
