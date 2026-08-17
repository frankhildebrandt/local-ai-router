// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "mlx-image-server",
    platforms: [.macOS(.v15)],
    products: [.executable(name: "mlx-image-server", targets: ["mlx-image-server"])],
    dependencies: [
        .package(path: "../vendor/flux-2-swift-mlx"),
        .package(url: "https://github.com/ml-explore/mlx-swift-examples.git", revision: "378f2449c257788c5067b9f8b086731d76b39b33"),
        .package(url: "https://github.com/ml-explore/mlx-swift.git", from: "0.30.2"),
        .package(url: "https://github.com/huggingface/swift-transformers.git", from: "1.3.0"),
    ],
    targets: [
        .executableTarget(
            name: "mlx-image-server",
            dependencies: [
                .product(name: "Flux2Core", package: "flux-2-swift-mlx"),
                .product(name: "FluxTextEncoders", package: "flux-2-swift-mlx"),
                .product(name: "StableDiffusion", package: "mlx-swift-examples"),
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "Hub", package: "swift-transformers"),
            ]
        )
    ]
)
