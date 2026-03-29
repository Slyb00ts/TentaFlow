# Konfiguracja Swift Package Manager — MLX dependencies

## Wymagane pakiety

Dodaj w Xcode: File > Add Package Dependencies...

### 1. mlx-swift
- URL: `https://github.com/ml-explore/mlx-swift`
- Branch: `main`
- Target: `MLX`

### 2. mlx-swift-examples
- URL: `https://github.com/ml-explore/mlx-swift-examples`
- Branch: `main`
- Targets: `MLXLLM`, `MLXLMCommon`

## Kroki

1. Otworz `TentaFlowAI.xcodeproj` w Xcode
2. Kliknij na projekt w nawigatorze (lewa kolumna)
3. Wybierz target `TentaFlowAI`
4. Zakładka "Package Dependencies"
5. Kliknij "+" i dodaj oba pakiety powyzej
6. W "Frameworks, Libraries, and Embedded Content" upewnij sie ze `MLX`, `MLXLLM`, `MLXLMCommon` sa dodane

## Uwagi

- Wymaga Xcode 15.4+ i macOS 14.5+
- Kompilacja MLX wymaga Apple Silicon (arm64)
- Minimalny iOS deployment target: 16.0
