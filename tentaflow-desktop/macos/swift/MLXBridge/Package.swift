// swift-tools-version:6.1
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
        // CLI diag — laduje LLM przez nasz MLXBridgeEngine i generuje, zeby
        // weryfikowac tokenizacje/output naszego mlx-swift bez calego stacku.
    ],
    dependencies: [
        // Core MLX bindings (Array, GPU, etc.). mlx-swift-lm 3.x wymaga 0.31.x
        // (QuantizedKVCache + kvBits/kvGroupSize/quantizedKVStart).
        .package(url: "https://github.com/ml-explore/mlx-swift.git", from: "0.31.3"),
        // MLXLLM, MLXLMCommon, MLXHuggingFace — high-level LLM runtime.
        // Repo przemianowane z mlx-swift-examples; 3.x ma kwantyzacje KV cache.
        .package(url: "https://github.com/ml-explore/mlx-swift-lm.git", from: "3.31.3"),
        // swift-transformers 1.x — tokenizer HF (`Tokenizers`) dla portu Whispera
        // i dla bridge tokenizera LLM przez makra MLXHuggingFace.
        .package(url: "https://github.com/huggingface/swift-transformers.git", from: "1.3.3"),
        // swift-huggingface — `HuggingFace` (HubApi). 3.x przenioslo Hub tu;
        // makra #hubDownloader()/#huggingFaceTokenizerLoader() tego wymagaja.
        .package(url: "https://github.com/huggingface/swift-huggingface.git", from: "0.9.0"),
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
                .product(name: "MLXLLM", package: "mlx-swift-lm"),
                .product(name: "MLXLMCommon", package: "mlx-swift-lm"),
                .product(name: "MLXHuggingFace", package: "mlx-swift-lm"),
                // MLXEmbedders — silnik embeddingow (BERT/Qwen3/Gemma3 ...).
                // Qwen3 jest w EmbedderTypeRegistry, wiec jina-embeddings-v5
                // (Qwen3-0.6B + 1_Pooling) laduje sie natywnie.
                .product(name: "MLXEmbedders", package: "mlx-swift-lm"),
                // Tokenizer HF dla portu Whispera (parsowanie tokenizer.json) oraz
                // HubApi dla makr MLXHuggingFace (loadContainer from/using).
                .product(name: "Tokenizers", package: "swift-transformers"),
                .product(name: "HuggingFace", package: "swift-huggingface"),
            ],
            path: "Sources/MLXBridge",
            // Tools 6.1 (wymagane przez mlx-swift-lm 3.x), ale kod jest pisany pod
            // semantyke Swift 5 (DispatchSemaphore, FFI). Pelna migracja na Swift 6
            // strict concurrency to osobny temat — zostajemy w trybie 5.
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "WhisperTest",
            dependencies: ["MLXBridge"],
            path: "Sources/WhisperTest",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
