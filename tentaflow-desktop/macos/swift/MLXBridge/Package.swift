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
        // CLI test runner — laduje model + transkrybuje WAV. Uzywany do
        // weryfikacji portu Whispera bez uruchamiania calego tentaflow stack'u.
        .executable(name: "WhisperTest", targets: ["WhisperTest"]),
    ],
    dependencies: [
        // Core MLX bindings (Array, GPU, etc.). Pin do <0.27 — wieksze wersje
        // wprowadzily zmiany w protokole `Quantizable` ktore wywalaja
        // `SwitchLayers.swift` w mlx-swift-examples 2.21.x. Range trzymamy
        // ciasno zeby SPM nie podbil sam.
        .package(url: "https://github.com/ml-explore/mlx-swift.git", "0.21.2" ..< "0.26.0"),
        // MLXLLM, MLXLMCommon, LLMModelFactory — high-level LLM runtime.
        // Ta sama wersja co iOS uzywa.
        .package(url: "https://github.com/ml-explore/mlx-swift-examples.git", from: "2.21.0"),
        // swift-transformers — tokenizer HF + AutoTokenizer.from(modelFolder:).
        // mlx-swift-examples sam wciagnie ta zaleznosc, dodajemy ja jawnie
        // zeby `import Tokenizers` w naszym kodzie nie zalezalo od kolejnosci.
        // Pin do dokladnej wersji 0.1.20 — nowsze swift-transformers (0.1.21+)
        // zmienily Config API i mlx-swift-examples 2.21.0 sie nie kompiluje
        // (ambiguous `Config.dictionary` w MLXLMCommon/Tokenizer.swift).
        .package(url: "https://github.com/huggingface/swift-transformers.git", exact: "0.1.20"),
    ],
    targets: [
        .target(
            name: "MLXBridge",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                // MLXNN, MLXFFT, MLXFast — uzywane przez wlasna implementacje
                // Whispera (encoder/decoder + log-mel spectrogram). MLXLLM
                // tego nie eksponuje, wiec port whisper.py z mlx-examples
                // leci na bazowych prymitywach mlx-swift.
                .product(name: "MLXNN", package: "mlx-swift"),
                .product(name: "MLXFFT", package: "mlx-swift"),
                .product(name: "MLXFast", package: "mlx-swift"),
                .product(name: "MLXRandom", package: "mlx-swift"),
                .product(name: "MLXLLM", package: "mlx-swift-examples"),
                .product(name: "MLXLMCommon", package: "mlx-swift-examples"),
                // swift-transformers wciagniete tranzytywnie przez mlx-swift-examples,
                // uzywamy go do tokenizera Whispera (parsowanie tokenizer.json HF).
                // Wersja 0.1.13 eksportuje tylko `Transformers` (umbrella),
                // ktory wciaga i `Tokenizers` i `Models`.
                .product(name: "Transformers", package: "swift-transformers"),
            ],
            path: "Sources/MLXBridge"
        ),
        .executableTarget(
            name: "WhisperTest",
            dependencies: ["MLXBridge"],
            path: "Sources/WhisperTest"
        ),
    ]
)
