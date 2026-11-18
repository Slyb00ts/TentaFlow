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

## Limit pamieci aplikacji (entitlements)

Target `TentaFlowAI` podpisuje sie plikiem `TentaFlowAI/TentaFlowAI.entitlements`
(`CODE_SIGN_ENTITLEMENTS` w konfiguracjach Debug i Release) z dwoma uprawnieniami:

- `com.apple.developer.kernel.increased-memory-limit` — podnosi limit jetsam aplikacji,
- `com.apple.developer.kernel.extended-virtual-addressing` — wieksza przestrzen adresowa
  dla mapowanych wag modeli.

App ID `ai.tentaflow.mobile` musi miec wlaczone capabilities "Increased Memory Limit"
i "Extended Virtual Addressing". Przy automatycznym podpisywaniu Xcode rejestruje je
w App ID podczas buildu, jesli zespol (KZXD4ZWQ85) na to pozwala; w przeciwnym razie
wlacz je recznie w Certificates, Identifiers & Profiles i odswiez profil.

TentaFlow raportuje jako pamiec GPU iOS realny limit aplikacji
(`os_proc_available_memory()` + biezacy `phys_footprint`), a nie fizyczny RAM —
po zmianie uprawnien sprawdz wartosc w panelu wezla.
