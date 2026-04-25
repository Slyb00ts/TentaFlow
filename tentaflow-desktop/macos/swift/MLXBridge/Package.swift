// swift-tools-version:5.9
// =============================================================================
// Plik: Package.swift
// Opis: Swift Package — biblioteka dynamiczna libMLXBridge.dylib mostkujaca
//       Rust desktop bin do mlx-swift / MLXLLM. Dokladnie taka sama
//       implementacja jak na iOS w tentaflow-mobile/ios/TentaFlowAI/MLXSwiftEngine.swift,
//       gdzie Bielik 4.5B 4-bit dziala bezbledne.
//
//       build.rs w tentaflow-desktop/macos/ odpala `swift build -c release`
//       i kopiuje libMLXBridge.dylib do target/release/. Rust desktop bin
//       linkuje sie przeciwko niej i woła MLXBridge_register() przy starcie.
// =============================================================================

import PackageDescription

let package = Package(
    name: "MLXBridge",
    platforms: [
        // mlx-swift-examples wymaga macOS 14+ (Sonoma) dla Metal Performance Shaders.
        .macOS(.v14),
    ],
    products: [
        // .dynamic = .dylib ktory Rust binary linkuje przy starcie.
        // Static byłoby pakietem .a, ale Rust musi dolinkować Foundation/AppKit
        // co jest jaśniejsze przez dynamic library + cargo:rustc-link-lib.
        .library(name: "MLXBridge", type: .dynamic, targets: ["MLXBridge"]),
    ],
    dependencies: [
        // Core MLX bindings (Array, GPU, etc.)
        .package(url: "https://github.com/ml-explore/mlx-swift.git", from: "0.21.0"),
        // MLXLLM, MLXLMCommon, LLMModelFactory — high-level LLM runtime.
        // Ta sama wersja co iOS uzywa.
        .package(url: "https://github.com/ml-explore/mlx-swift-examples.git", from: "2.21.0"),
    ],
    targets: [
        .target(
            name: "MLXBridge",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXLLM", package: "mlx-swift-examples"),
                .product(name: "MLXLMCommon", package: "mlx-swift-examples"),
            ],
            path: "Sources/MLXBridge"
        ),
    ]
)
