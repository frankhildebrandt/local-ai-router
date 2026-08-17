// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "mlx-speech-server",
    platforms: [.macOS(.v15)],
    products: [.executable(name: "mlx-speech-server", targets: ["mlx-speech-server"])],
    dependencies: [
        // kokoro-swift pins Misaki as a local path package, which SwiftPM rejects for
        // revision-based git dependencies. The vendor checkout is prepared by
        // scripts/prepare-kokoro-vendor.sh at commit 20bf04c506e913ff129d7d2229398180ba24c690.
        .package(path: "../vendor/kokoro-swift"),
    ],
    targets: [
        .executableTarget(
            name: "mlx-speech-server",
            dependencies: [
                .product(name: "Kokoro", package: "kokoro-swift"),
            ]
        )
    ]
)
