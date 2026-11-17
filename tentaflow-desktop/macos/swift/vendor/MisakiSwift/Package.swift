// swift-tools-version: 6.2
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
  name: "MisakiSwift",
  platforms: [
    .iOS(.v18), .macOS(.v15)
  ],
  products: [
    .library(
      name: "MisakiSwift",
      type: .dynamic,
      targets: ["MisakiSwift"]
    ),
  ],
  dependencies: [
    // Vendored fork: upstream pins ml-explore/mlx-swift exact 0.30.2. One binary
    // = one mlx-swift, so this points at the PrismML fork revision that MLXBridge,
    // KokoroBridge and the iOS app pin (0.31.6 + Prism quantization kernels).
    // MLXUtilsLibrary 0.0.6 asks for ml-explore from 0.29.1; the revision
    // requirement overrides it.
    .package(
      url: "https://github.com/PrismML-Eng/mlx-swift.git",
      revision: "9c3a80a4ce8be105540e378c2a703bfc9de57f25"
    ),
    .package(url: "https://github.com/mlalma/MLXUtilsLibrary.git", exact: "0.0.6")
  ],
  targets: [
    .target(
      name: "MisakiSwift",
      dependencies: [
        .product(name: "MLX", package: "mlx-swift"),
        .product(name: "MLXNN", package: "mlx-swift"),
        .product(name: "MLXUtilsLibrary", package: "MLXUtilsLibrary")
     ],
     resources: [
      .copy("../../Resources/")
     ]
    ),
    .testTarget(
      name: "MisakiSwiftTests",
      dependencies: ["MisakiSwift"]
    ),
  ]
)
