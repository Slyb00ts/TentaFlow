// =============================================================================
// Plik: WhisperLoader.swift
// Opis: Loader wag Whispera z safetensors. Etapy:
//         1. Wczytaj `config.json` -> `WhisperConfig`.
//         2. Zbuduj pusty `Whisper` model.
//         3. Jesli config.quantization != nil → `quantize(model:groupSize:bits:)`
//            zeby Linear/Embedding zmienily klase na QuantizedLinear/QuantizedEmbedding
//            i pasowaly do kluczy `*.scales`/`*.biases` w safetensors.
//         4. Zaladuj `weights.safetensors` (mozliwe ze rozbity na shardy)
//            i wlej do modelu przez `update(parameters:)`.
//
//       Step 1 ladowanie wag, ale BEZ forward inference — to bedzie test
//       w step 2 razem z log-mel preprocessingiem.
// =============================================================================

import Foundation
import MLX
import MLXNN

public enum WhisperLoaderError: Error, CustomStringConvertible {
    case missingConfig(URL)
    case noWeightsFiles(URL)
    case loadFailed(String)

    public var description: String {
        switch self {
        case .missingConfig(let url):
            return "Brak config.json w \(url.path)"
        case .noWeightsFiles(let url):
            return "Brak plikow weights.safetensors w \(url.path)"
        case .loadFailed(let msg):
            return "Blad ladowania wag: \(msg)"
        }
    }
}

public enum WhisperLoader {
    /// Laduje pelny model Whispera z katalogu HF snapshot. Katalog musi
    /// zawierac:
    ///   - `config.json`
    ///   - `weights.safetensors` LUB `weights.0.safetensors`+`weights.1.safetensors`+...
    ///   - `tokenizer.json` (uzywany dopiero przy dekodowaniu, tu nie wymagany)
    ///
    /// Zwraca model z zaladowanymi wagami, gotowy do `evaluate()` i forward.
    public static func load(directory: URL) throws -> Whisper {
        let config = try WhisperConfig.load(from: directory)
        let model = Whisper(config: config)

        // Quantization MUSI byc zaaplikowana PRZED `update(parameters:)`,
        // inaczej Linear/Embedding nie maja pol `.scales`/`.biases` i mlx-swift
        // odrzuci `update` z bledem niespojnych ksztaltow.
        if let q = config.quantization {
            quantize(model: model, groupSize: q.groupSize, bits: q.bits) { _, module in
                // Domyslna polityka mlx-examples: kwantyzujemy wszystkie
                // Linear i Embedding poza tymi ktore sa za male zeby siec
                // nie stracila precyzji na cienkich projekcjach. Whisper
                // turbo nie ma takich, wiec bez wyjatkow.
                return module is Linear || module is Embedding
            }
        }

        let weightFiles = try shardedWeightURLs(in: directory)
        var merged: [String: MLXArray] = [:]
        for url in weightFiles {
            do {
                let arrays = try loadArrays(url: url)
                for (k, v) in arrays {
                    // Pomin specjalne klucze ktore nie sa parametrami modelu:
                    //   - `alignment_heads` (uzywane tylko do word-timestamp DTW)
                    //   - `_*` (np. `_positional_embedding` — bufor encoderowych
                    //     sinusoid; my generujemy je w init).
                    if k == "alignment_heads" || k.hasPrefix("_") || k.contains("._") {
                        continue
                    }
                    merged[remapKey(k)] = v
                }
            } catch {
                throw WhisperLoaderError.loadFailed(
                    "loadArrays(\(url.lastPathComponent)): \(error)"
                )
            }
        }

        // mlx-swift `Module` chce drzewa zagniezdzonego (NestedDictionary), a
        // safetensors trzyma wagi po placskim kluczu z kropkami. Konwersja
        // przez `unflattened`.
        let nested = ModuleParameters.unflattened(
            merged.map { ($0.key, $0.value) }
        )
        // verify=.none: pozwala miec klucze ktorych model nie uzywa (np.
        // pominiete `_positional_embedding` encodera) i odwrotnie. Whisper
        // turbo ma jeden taki: `decoder.positional_embedding` JEST uzywany
        // jako parametr (laduje sie OK), ale bufor sinusoid encodera nie
        // jest w safetensors (i dobrze, bo go obliczamy w init).
        try model.update(parameters: nested, verify: .none)
        // `eval` odpala lazy compute na wszystkich parametrach, dzieki czemu
        // pierwszy forward nie placi kosztu kopiowania na GPU od zera.
        eval(model.parameters())
        return model
    }

    /// Remapuje klucze safetensors zgodnie z naszym layoutem modelu.
    /// `mlx-community/whisper-large-v3-turbo-4bit` zapisuje MLP jako dwa
    /// odrebne moduly `mlp1` / `mlp2` (zamiast Sequential), bo MLX-Python
    /// implementacja w mlx-examples uzywa wprost dwoch warstw. Mapujemy:
    ///   `*.mlp1.*` → `*.mlp.fc1.*`
    ///   `*.mlp2.*` → `*.mlp.fc2.*`
    private static func remapKey(_ k: String) -> String {
        if k.contains(".mlp1.") {
            return k.replacingOccurrences(of: ".mlp1.", with: ".mlp.fc1.")
        }
        if k.contains(".mlp2.") {
            return k.replacingOccurrences(of: ".mlp2.", with: ".mlp.fc2.")
        }
        return k
    }

    /// Lista plikow weights w kolejnosci ktora ma sens dla `loadArrays`.
    /// Nowsze konwertery `mlx-community/*` zapisuja `model.safetensors`,
    /// starsze `weights.safetensors` (lub shardy `weights.0.safetensors`,
    /// `weights.1.safetensors`...). Akceptujemy wszystkie warianty.
    private static func shardedWeightURLs(in directory: URL) throws -> [URL] {
        let fm = FileManager.default
        let entries = (try? fm.contentsOfDirectory(atPath: directory.path)) ?? []
        let weightNames = entries
            .filter { (name: String) -> Bool in
                guard name.hasSuffix(".safetensors") else { return false }
                return name.hasPrefix("weights") || name.hasPrefix("model")
            }
            .sorted()
        guard !weightNames.isEmpty else {
            throw WhisperLoaderError.noWeightsFiles(directory)
        }
        return weightNames.map { directory.appendingPathComponent($0) }
    }
}
