// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "mlx-server",
    platforms: [.macOS(.v15)],
    products: [.executable(name: "mlx-server", targets: ["mlx-server"])],
    dependencies: [
        .package(url: "https://github.com/ml-explore/mlx-swift-lm.git", exact: "3.31.4"),
        .package(url: "https://github.com/ml-explore/mlx-swift", exact: "0.31.6"),
        .package(url: "https://github.com/huggingface/swift-transformers.git", exact: "1.3.3"),
        .package(url: "https://github.com/huggingface/swift-huggingface.git", exact: "0.9.0")
    ],
    targets: [
        .executableTarget(
            name: "mlx-server",
            dependencies: [
                .product(name: "MLXLLM", package: "mlx-swift-lm"),
                .product(name: "MLXVLM", package: "mlx-swift-lm"),
                .product(name: "MLXLMCommon", package: "mlx-swift-lm"),
                .product(name: "MLXHuggingFace", package: "mlx-swift-lm"),
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "HuggingFace", package: "swift-huggingface"),
                .product(name: "Tokenizers", package: "swift-transformers")
            ]
        )
    ]
)
