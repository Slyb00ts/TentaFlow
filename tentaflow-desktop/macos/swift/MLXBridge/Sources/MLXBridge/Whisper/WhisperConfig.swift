// =============================================================================
// Plik: WhisperConfig.swift
// Opis: Wymiary modelu Whisper + loader `config.json` z repo HF
//       `mlx-community/whisper-large-v3-turbo-4bit` (i innych wariantow).
//       Wartosci 1:1 jak Pythonowe `ModelDimensions` z mlx-examples/whisper.
// =============================================================================

import Foundation

/// Konfiguracja kwantyzacji wczytana z `config.json["quantization"]`. Pole
/// opcjonalne — modele FP16 (np. `whisper-large-v3` bez sufiksu `-4bit`)
/// nie maja tego klucza. `groupSize`/`bits` musza dokladnie zgadzac sie
/// z parametrami uzytymi przy quantize Pythonowym, inaczej dequantize daje
/// smieci zamiast wag.
public struct WhisperQuantization: Codable {
    public let groupSize: Int
    public let bits: Int

    enum CodingKeys: String, CodingKey {
        case groupSize = "group_size"
        case bits
    }
}

/// Wymiary architektury Whispera. Klucze odpowiadaja `ModelDimensions` z
/// Python whisper.py. `n_audio_*` opisuja encoder, `n_text_*` decoder.
/// Turbo to wariant z plytkim decoderem (`n_text_layer = 4`) — encoder
/// ma standardowe 32 warstwy, dlatego enkodowanie jest tak samo szybkie
/// jak large-v3 ale dekodowanie 8x szybsze.
public struct WhisperConfig: Codable {
    public let nMels: Int
    public let nVocab: Int
    public let nAudioCtx: Int
    public let nAudioState: Int
    public let nAudioHead: Int
    public let nAudioLayer: Int
    public let nTextCtx: Int
    public let nTextState: Int
    public let nTextHead: Int
    public let nTextLayer: Int
    public let quantization: WhisperQuantization?

    enum CodingKeys: String, CodingKey {
        case nMels = "n_mels"
        case nVocab = "n_vocab"
        case nAudioCtx = "n_audio_ctx"
        case nAudioState = "n_audio_state"
        case nAudioHead = "n_audio_head"
        case nAudioLayer = "n_audio_layer"
        case nTextCtx = "n_text_ctx"
        case nTextState = "n_text_state"
        case nTextHead = "n_text_head"
        case nTextLayer = "n_text_layer"
        case quantization
    }

    /// Wczytuje `config.json` z katalogu modelu. Plik musi byc bezposrednio
    /// w `directory` — taki layout serwuje HF snapshot dla wszystkich repo
    /// `mlx-community/whisper-*`.
    public static func load(from directory: URL) throws -> WhisperConfig {
        let configURL = directory.appendingPathComponent("config.json")
        let data = try Data(contentsOf: configURL)
        let decoder = JSONDecoder()
        return try decoder.decode(WhisperConfig.self, from: data)
    }

    /// Wymiar pojedynczej glowy uwagi w encoderze. Zwykle 64 dla Whispera
    /// (1280 / 20 = 64 dla turbo).
    public var audioHeadDim: Int { nAudioState / nAudioHead }

    /// Wymiar glowy w decoderze.
    public var textHeadDim: Int { nTextState / nTextHead }
}
