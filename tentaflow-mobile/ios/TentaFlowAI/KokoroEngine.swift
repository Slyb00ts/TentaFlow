// =============================================================================
// Plik: KokoroEngine.swift
// Opis: Native MLX Kokoro 82M na iOS przez pakiet KokoroBridge (KokoroSwiftLocal
//       + MisakiSwift). Rejestruje wskazniki funkcji w Rust core przez
//       tentaflow_register_kokoro — tak samo jak MLXSwiftEngine (LLM) i
//       AppleTTSEngine. Na iOS nie ma libKokoroBridge.dylib do dlopen, wiec
//       Swift podaje wskazniki + context (engine) przy starcie aplikacji.
// =============================================================================

import Foundation
import KokoroBridge

enum KokoroEngineBridge {
    /// Rejestruje wskazniki w Rust core. Context = singleton KokoroBridgeEngine.
    static func registerWithRust() {
        let ctx = Unmanaged.passUnretained(KokoroBridgeEngine.shared).toOpaque()
        tentaflow_register_kokoro(
            kokoroLoadModel,
            kokoroUnloadModel,
            kokoroSynthesize,
            kokoroFreeBuffer,
            ctx
        )
        print("[Kokoro] Callbacks zarejestrowane w Rust")
    }
}

// =============================================================================
// Wskazniki funkcji przekazywane do Rust (top-level, bez przechwytywania ->
// Swift konwertuje na C function ptr). Wolaja publiczne API KokoroBridgeEngine.
// =============================================================================

private func kokoroLoadModel(
    _ path: UnsafePointer<CChar>?,
    _ context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let p = path.flatMap({ String(cString: $0) }) else { return -1 }
    return KokoroBridgeEngine.shared.loadModel(path: p) ? 0 : -1
}

private func kokoroUnloadModel(_ context: UnsafeMutableRawPointer?) {
    KokoroBridgeEngine.shared.unloadModel()
}

private func kokoroSynthesize(
    _ text: UnsafePointer<CChar>?,
    _ voice: UnsafePointer<CChar>?,
    _ language: UnsafePointer<CChar>?,
    _ speed: Float,
    _ outSampleRate: UnsafeMutablePointer<Int32>?,
    _ outNumSamples: UnsafeMutablePointer<Int32>?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<Float>? {
    guard let t = text.flatMap({ String(cString: $0) }),
          let outSampleRate, let outNumSamples else { return nil }
    let v = voice.flatMap { String(cString: $0) } ?? "af_heart"
    let l = language.flatMap { String(cString: $0) } ?? "en-us"
    guard let samples = KokoroBridgeEngine.shared.synthesize(
        text: t, voiceName: v, language: l, speed: speed
    ), !samples.isEmpty else { return nil }

    outSampleRate.pointee = 24_000  // Kokoro staly sample rate
    outNumSamples.pointee = Int32(samples.count)
    let buf = UnsafeMutablePointer<Float>.allocate(capacity: samples.count)
    buf.update(from: samples, count: samples.count)
    return buf
}

private func kokoroFreeBuffer(_ ptr: UnsafeMutablePointer<Float>?) {
    ptr?.deallocate()
}
