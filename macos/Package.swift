// swift-tools-version: 6.0
// The mcpgw macOS app: a SwiftUI front end for the gateway. It bundles the
// `mcpgw` binary and drives the daemon through it and the admin API.
// `./build.sh` builds the Rust binary and this package into `mcpgw.app`.

import PackageDescription

let package = Package(
    name: "McpgwMac",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "McpgwApp", targets: ["McpgwApp"]),
    ],
    targets: [
        // Talking to the daemon (CLI and HTTP API); no UI, so it's testable.
        .target(name: "McpgwKit"),
        .executableTarget(name: "McpgwApp", dependencies: ["McpgwKit"]),
        .testTarget(name: "McpgwKitTests", dependencies: ["McpgwKit"]),
    ]
)
