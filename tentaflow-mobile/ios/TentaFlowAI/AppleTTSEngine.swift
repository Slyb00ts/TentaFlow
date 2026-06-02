// =============================================================================
// Plik: AppleTTSEngine.swift
// Opis: Natywny TTS Apple (AVSpeechSynthesizer) na iOS. Renderuje PCM Float32
//       offline przez `write(_:toBufferCallback:)` i rejestruje wskazniki
//       funkcji w Rust core (tentaflow_register_apple_tts) — analogicznie do
//       MLXSwiftEngine. Na iOS nie ma libMLXBridge.dylib do dlopen, wiec Rust
//       dostaje wskazniki przez rejestracje przy starcie aplikacji.
// =============================================================================

import AVFoundation
import Foundation

/// Wynik syntezy: czysty PCM Float32 + sample rate.
struct AppleTTSResult {
    let pcm: [Float]
    let sampleRate: Int
}

enum AppleTTSEngine {
    /// Rejestruje wskazniki cdecl w Rust core. Wywolywane raz z AppDelegate
    /// przed `tentaflow_mobile_start()`.
    static func registerWithRust() {
        tentaflow_register_apple_tts(
            appleTtsListVoices,
            appleTtsSynthesize,
            appleTtsFreeBuffer
        )
        print("[AppleTTS] Callbacks zarejestrowane w Rust")
    }

    /// Lista zainstalowanych glosow systemowych z metadanymi.
    static func availableVoices() -> [[String: String]] {
        return AVSpeechSynthesisVoice.speechVoices().map { v in
            [
                "id": v.identifier,
                "name": v.name,
                "language": v.language,
                "quality": v.quality == .enhanced ? "enhanced" : "default",
            ]
        }
    }

    /// Synchroniczna synteza dla cdecl FFI. `write` dostarcza buffery przez
    /// main run-loop — pompujemy go recznie az do pustego bufora (sygnal konca).
    /// Caller (Rust spawn_blocking) NIE moze byc na main queue.
    static func synthesizeSync(
        text: String,
        voiceId: String? = nil,
        language: String = "en-US",
        rate: Float = AVSpeechUtteranceDefaultSpeechRate
    ) -> AppleTTSResult? {
        let utterance = AVSpeechUtterance(string: text)
        if let voiceId, let v = AVSpeechSynthesisVoice(identifier: voiceId) {
            utterance.voice = v
        } else if let v = AVSpeechSynthesisVoice(language: language) {
            utterance.voice = v
        }
        utterance.rate = max(AVSpeechUtteranceMinimumSpeechRate,
                             min(AVSpeechUtteranceMaximumSpeechRate, rate))

        let runOnMain: () -> AppleTTSResult? = {
            let box = SynthBox()
            var samples: [Float] = []
            var detectedRate: Int = 0
            var done = false
            box.synth.write(utterance) { buffer in
                if done { return }
                guard let pcmBuffer = buffer as? AVAudioPCMBuffer else { return }
                let frameLen = Int(pcmBuffer.frameLength)
                detectedRate = Int(pcmBuffer.format.sampleRate)
                if frameLen == 0 {
                    done = true
                    return
                }
                if let f32 = pcmBuffer.floatChannelData {
                    samples.append(contentsOf: UnsafeBufferPointer(start: f32[0], count: frameLen))
                } else if let i16 = pcmBuffer.int16ChannelData {
                    let buf = UnsafeBufferPointer(start: i16[0], count: frameLen)
                    samples.reserveCapacity(samples.count + frameLen)
                    for s in buf { samples.append(Float(s) / 32768.0) }
                }
            }
            let deadline = Date(timeIntervalSinceNow: 60)
            while !done && Date() < deadline {
                RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.1))
            }
            if !done { return nil }
            return AppleTTSResult(pcm: samples, sampleRate: detectedRate)
        }

        if Thread.isMainThread {
            return runOnMain()
        } else {
            var out: AppleTTSResult? = nil
            DispatchQueue.main.sync { out = runOnMain() }
            return out
        }
    }
}

/// Trzyma synth + stan zywe podczas callbackow `write(_:toBufferCallback:)`.
private final class SynthBox {
    let synth = AVSpeechSynthesizer()
    var samples: [Float] = []
    var detectedRate: Int = 0
    var completed: Bool = false
}

// =============================================================================
// Wskazniki funkcji przekazywane do Rust (tentaflow_register_apple_tts).
// Top-level funkcje bez przechwytywania -> Swift konwertuje na C function ptr.
// =============================================================================

/// Listuje glosy systemowe jako JSON. Caller (Rust) zwalnia przez free().
private func appleTtsListVoices() -> UnsafeMutablePointer<CChar>? {
    let voices = AppleTTSEngine.availableVoices()
    guard let data = try? JSONSerialization.data(withJSONObject: voices),
          let s = String(data: data, encoding: .utf8) else { return nil }
    return strdup(s)
}

/// Synteza -> malloc'd bufor Float32 + sample_rate + num_samples. Caller
/// zwalnia bufor przez appleTtsFreeBuffer.
private func appleTtsSynthesize(
    _ text: UnsafePointer<CChar>?,
    _ voiceId: UnsafePointer<CChar>?,
    _ language: UnsafePointer<CChar>?,
    _ rate: Float,
    _ outSampleRate: UnsafeMutablePointer<Int32>?,
    _ outNumSamples: UnsafeMutablePointer<Int32>?
) -> UnsafeMutablePointer<Float>? {
    guard let text = text.flatMap({ String(cString: $0) }),
          let outSampleRate, let outNumSamples else { return nil }
    let voiceIdStr = voiceId.flatMap { String(cString: $0) }
    let languageStr = language.flatMap { String(cString: $0) } ?? "en-US"

    let result = AppleTTSEngine.synthesizeSync(
        text: text,
        voiceId: voiceIdStr,
        language: languageStr,
        rate: rate
    )
    guard let r = result, !r.pcm.isEmpty else { return nil }

    outSampleRate.pointee = Int32(r.sampleRate)
    outNumSamples.pointee = Int32(r.pcm.count)
    let buf = UnsafeMutablePointer<Float>.allocate(capacity: r.pcm.count)
    buf.update(from: r.pcm, count: r.pcm.count)
    return buf
}

/// Zwalnia bufor zwrocony przez appleTtsSynthesize. Bezpieczny dla NULL.
private func appleTtsFreeBuffer(_ ptr: UnsafeMutablePointer<Float>?) {
    ptr?.deallocate()
}
