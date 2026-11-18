// swift-tools-version:6.0
// =============================================================================
// Plik: Package.swift (KokoroBridge)
// Opis: Standalone `libKokoroBridge.dylib` exposing cdecl `Kokoro_*` to Rust.
//       A separate dylib from MLXBridge, but built against the SAME
//       mlx-swift (PrismML fork, same revision): both dylibs load one
//       `mlx-swift_Cmlx.bundle` from beside the binary, so a second mlx-swift
//       would ship a metallib without the fork's kernels.
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
        // Same fork and revision as MLXBridge and the iOS app's root pin: one
        // mlx-swift per binary. A revision requirement from this root (and from
        // the local MisakiSwift) overrides MLXUtilsLibrary's version requirement
        // on ml-explore/mlx-swift, so that one resolves to the fork as well.
        .package(
            url: "https://github.com/PrismML-Eng/mlx-swift.git",
            revision: "9c3a80a4ce8be105540e378c2a703bfc9de57f25"
        ),
        // Vendored MisakiSwift (fork 1.0.6): upstream pins ml-explore/mlx-swift
        // exact 0.30.2; the local copy pins the same fork revision as above. See
        // vendor/MisakiSwift/Package.swift.
        .package(path: "../vendor/MisakiSwift"),
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
