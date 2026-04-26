// swift-tools-version:6.0
// =============================================================================
// Plik: Package.swift (KokoroBridge)
// Opis: Niezalezna dylib `libKokoroBridge.dylib` wystawiajaca cdecl `Kokoro_*`
//       dla Rusta. Trzymana osobno od MLXBridge bo `KokoroSwift` (mlalma)
//       wymaga mlx-swift 0.30+ i Swift 5.9+, a MLXBridge LLM/Whisper jest
//       zapinowany na starszego mlx-swift (0.21..0.26) z powodu mlx-swift-examples
//       2.21.x. Dwie dyliby = niezalezne wersje mlx-swift, brak konfliktow.
// =============================================================================

import PackageDescription

let package = Package(
    name: "KokoroBridge",
    platforms: [
        // KokoroSwift wymaga macOS 15 / iOS 18 (nowy NaturalLanguage POS API).
        .macOS(.v15),
        .iOS(.v18),
    ],
    products: [
        .library(name: "KokoroBridge", type: .dynamic, targets: ["KokoroBridge"]),
    ],
    dependencies: [
        // Native MLX Kokoro vendored jako KokoroSwiftLocal (ta sama tresc co
        // mlalma/kokoro-ios 1.0.9, ale z poprawnym MLXFast dep — upstream
        // 1.0.9 importowal MLXFast bez deklarowania zaleznosci, co lamie
        // budowanie spod naszego packagea). MisakiSwift i MLXUtilsLibrary
        // zostaly jako zewnetrzne dep — sa OK.
        .package(url: "https://github.com/ml-explore/mlx-swift", from: "0.29.1"),
        .package(url: "https://github.com/mlalma/MisakiSwift.git", exact: "1.0.6"),
        .package(url: "https://github.com/mlalma/MLXUtilsLibrary.git", from: "0.0.6"),
    ],
    targets: [
        .target(
            name: "KokoroSwiftLocal",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXFast", package: "mlx-swift"),
                .product(name: "MLXNN", package: "mlx-swift"),
                .product(name: "MLXRandom", package: "mlx-swift"),
                .product(name: "MLXFFT", package: "mlx-swift"),
                .product(name: "MisakiSwift", package: "MisakiSwift"),
                .product(name: "MLXUtilsLibrary", package: "MLXUtilsLibrary"),
            ],
            path: "Sources/KokoroSwiftLocal",
            resources: [.copy("../../Resources/")]
        ),
        .target(
            name: "KokoroBridge",
            dependencies: ["KokoroSwiftLocal"],
            path: "Sources/KokoroBridge"
        ),
    ]
)
