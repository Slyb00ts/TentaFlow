// =============================================================================
// Plik: KokoroBridge.swift
// Opis: Singleton + cdecl FFI dla mlalma/KokoroSwift. Eksportuje:
//         Kokoro_loadModel(path)
//         Kokoro_unloadModel()
//         Kokoro_synthesize(text, voice, language, speed) -> Float* PCM
//         Kokoro_freeBuffer(ptr)
//         Kokoro_listVoices() -> JSON string
//
//       Format katalogu modelu (HF repo `mlx-community/Kokoro-82M-bf16`):
//         <root>/
//           kokoro-v1_0.safetensors    (wagi sieci)
//           config.json                (parametry; uzywany przez KokoroSwift)
//           voices/<name>.safetensors  (embedding stylu, np. af_heart, am_michael)
// =============================================================================

import Foundation
import KokoroSwiftLocal
import MLX

public final class KokoroBridgeEngine: @unchecked Sendable {
    public static let shared = KokoroBridgeEngine()

    private var tts: KokoroTTS?
    private var voices: [String: MLXArray] = [:]
    private var modelDir: URL?

    private init() {
        // Limit cache GPU jak w MLXBridge — 256 MB powinno zmiescic Kokoro
        // 82M (bf16 = ~165 MB) plus aktywacje.
        MLX.GPU.set(cacheLimit: 256 * 1024 * 1024)
    }

    /// Laduje model + voices z katalogu. Synchroniczne dla cdecl FFI.
    public func loadModel(path: String) -> Bool {
        let dir = URL(filePath: path)
        let fm = FileManager.default
        // Przejrzyj typowe nazwy plikow z wagami — repo MLX uzywa
        // `kokoro-v1_0.safetensors`, ale fallback dla starszych snapshotow.
        let weightCandidates = ["kokoro-v1_0.safetensors", "kokoro-v1.safetensors", "model.safetensors"]
        let weightsURL = weightCandidates
            .map { dir.appendingPathComponent($0) }
            .first(where: { fm.fileExists(atPath: $0.path) })
        guard let weightsURL else {
            print("[Kokoro] brak pliku wag w \(path)")
            return false
        }
        // Domyslnie misaki G2P (EN). eSpeakNG opcjonalnie w przyszlosci.
        let engine = KokoroTTS(modelPath: weightsURL, g2p: .misaki)
        self.tts = engine
        self.modelDir = dir

        // Laduj wszystkie voices z `voices/*.safetensors`. Kazdy plik to
        // jeden tensor stylu — laduje sie jako pojedynczy array.
        let voicesDir = dir.appendingPathComponent("voices")
        if let entries = try? fm.contentsOfDirectory(atPath: voicesDir.path) {
            for fname in entries where fname.hasSuffix(".safetensors") {
                let name = String(fname.dropLast(".safetensors".count))
                let url = voicesDir.appendingPathComponent(fname)
                guard let arrays = try? loadArrays(url: url) else { continue }
                if let firstValue = arrays.values.first {
                    voices[name] = firstValue
                }
            }
        }
        print("[Kokoro] zaladowano \(voices.count) glosow z \(voicesDir.path)")
        return !voices.isEmpty
    }

    public func unloadModel() {
        self.tts = nil
        self.voices.removeAll()
        self.modelDir = nil
        MLX.GPU.clearCache()
    }

    /// Synteza pojedyncza. Zwraca PCM Float32 @ 24 kHz mono. Jezeli `voiceName`
    /// nie istnieje w cache, uzywany pierwszy dostepny ("af_heart" zwykle).
    public func synthesize(
        text: String,
        voiceName: String,
        language: String,
        speed: Float
    ) -> [Float]? {
        guard let tts else {
            print("[Kokoro] brak zaladowanego modelu")
            return nil
        }
        let voice: MLXArray
        if let v = voices[voiceName] {
            voice = v
        } else if let v = voices.first?.value {
            print("[Kokoro] brak glosu '\(voiceName)' — uzywam pierwszy dostepny")
            voice = v
        } else {
            print("[Kokoro] brak voices w cache")
            return nil
        }
        let lang: Language = (language.lowercased() == "en-gb") ? .enGB : .enUS
        do {
            let (samples, _) = try tts.generateAudio(
                voice: voice, language: lang, text: text, speed: speed
            )
            return samples
        } catch {
            print("[Kokoro] generateAudio error: \(error)")
            return nil
        }
    }

    public func listVoices() -> [String] {
        return Array(voices.keys).sorted()
    }
}

// =============================================================================
// C-ABI exports
// =============================================================================

@_cdecl("Kokoro_getContext")
public func Kokoro_getContext() -> UnsafeMutableRawPointer {
    return Unmanaged.passUnretained(KokoroBridgeEngine.shared).toOpaque()
}

@_cdecl("Kokoro_loadModel")
public func Kokoro_loadModel(
    modelPath: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }
    let engine = Unmanaged<KokoroBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

@_cdecl("Kokoro_unloadModel")
public func Kokoro_unloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<KokoroBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

/// Zwraca PCM Float32 + sample_rate w `outSampleRate`. Caller zwalnia bufor
/// przez `Kokoro_freeBuffer`.
@_cdecl("Kokoro_synthesize")
public func Kokoro_synthesize(
    text: UnsafePointer<CChar>?,
    voice: UnsafePointer<CChar>?,
    language: UnsafePointer<CChar>?,
    speed: Float,
    outSampleRate: UnsafeMutablePointer<Int32>?,
    outNumSamples: UnsafeMutablePointer<Int32>?,
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<Float>? {
    guard let textStr = text.flatMap({ String(cString: $0) }),
          let ctx = context,
          let outSampleRate, let outNumSamples else { return nil }
    let voiceStr = voice.flatMap { String(cString: $0) } ?? "af_heart"
    let langStr = language.flatMap { String(cString: $0) } ?? "en-us"
    let engine = Unmanaged<KokoroBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    guard let samples = engine.synthesize(
        text: textStr, voiceName: voiceStr, language: langStr, speed: speed
    ), !samples.isEmpty else { return nil }

    outSampleRate.pointee = 24_000  // Kokoro fixed sample rate
    outNumSamples.pointee = Int32(samples.count)
    let buf = UnsafeMutablePointer<Float>.allocate(capacity: samples.count)
    buf.update(from: samples, count: samples.count)
    return buf
}

@_cdecl("Kokoro_freeBuffer")
public func Kokoro_freeBuffer(ptr: UnsafeMutablePointer<Float>?) {
    ptr?.deallocate()
}

/// JSON tablica nazw glosow zaladowanych. Caller zwalnia przez free().
@_cdecl("Kokoro_listVoices")
public func Kokoro_listVoices(
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard let ctx = context else { return nil }
    let engine = Unmanaged<KokoroBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    let voices = engine.listVoices()
    guard let data = try? JSONSerialization.data(withJSONObject: voices),
          let s = String(data: data, encoding: .utf8) else { return nil }
    return strdup(s)
}
