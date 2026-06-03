// =============================================================================
// Plik: WhisperEngineBridge.swift
// Opis: Rejestracja natywnego MLX Whisper (WhisperEngine.swift) w Rust core
//       przez tentaflow_register_whisper — tak samo jak KokoroEngine /
//       AppleTTSEngine. Na iOS nie ma libMLXBridge.dylib do dlopen, wiec Swift
//       podaje wskazniki funkcji + context (MLXWhisperEngine.shared) przy
//       starcie aplikacji. STT na iOS idzie WYLACZNIE przez MLX (nie whisper.cpp).
// =============================================================================

import Foundation

enum WhisperEngineBridge {
    /// Rejestruje wskazniki w Rust core. Context = singleton MLXWhisperEngine.
    static func registerWithRust() {
        let ctx = Unmanaged.passUnretained(MLXWhisperEngine.shared).toOpaque()
        tentaflow_register_whisper(
            whisperLoadModel,
            whisperUnloadModel,
            whisperTranscribe,
            ctx
        )
        print("[MLXWhisper] Callbacks zarejestrowane w Rust")
    }
}

// =============================================================================
// Wskazniki funkcji przekazywane do Rust (top-level, bez przechwytywania ->
// Swift konwertuje na C function ptr). Wolaja publiczne API MLXWhisperEngine.
// =============================================================================

private func whisperLoadModel(
    _ path: UnsafePointer<CChar>?,
    _ context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let p = path.flatMap({ String(cString: $0) }) else { return -1 }
    return MLXWhisperEngine.shared.loadModel(path: p) ? 0 : -1
}

private func whisperUnloadModel(_ context: UnsafeMutableRawPointer?) {
    MLXWhisperEngine.shared.unloadModel()
}

private func whisperTranscribe(
    _ pcm: UnsafePointer<Float>?,
    _ nSamples: Int32,
    _ language: UnsafePointer<CChar>?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard let pcm, nSamples > 0 else { return nil }
    let lang = language.flatMap { String(cString: $0) } ?? "en"
    let buffer = UnsafeBufferPointer(start: pcm, count: Int(nSamples))
    let samples = Array(buffer)
    guard let text = MLXWhisperEngine.shared.transcribe(pcm: samples, language: lang) else {
        return nil
    }
    // strdup -> Rust zwalnia przez libc free (kontrakt TranscribeFn).
    return strdup(text)
}
