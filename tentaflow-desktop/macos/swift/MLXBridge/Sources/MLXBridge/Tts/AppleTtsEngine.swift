// =============================================================================
// Plik: AppleTtsEngine.swift
// Opis: Wrapper na `AVSpeechSynthesizer` — natywny TTS Apple, dziala na
//       macOS/iOS bez zaleznosci. Glos `Zosia` (pl-PL) i wszystkie inne
//       glosy ktore sa zainstalowane w systemie.
//
//       Synteza odbywa sie offline przez `AVSpeechSynthesizer.write` ktore
//       wywoluje callback z `AVAudioBuffer`. Konkatenujemy wszystkie buffery
//       do PCM Float32 i zwracamy razem z sample rate (zwykle 22050 Hz).
//
//       Uruchamia sie tez na iOS 13+ (write(_:toBufferCallback:) dostepne).
//       Cdecl `MLXAppleTTS_*` jest niezalezny od MLXBridge LLM/Whisper.
// =============================================================================

import AVFoundation
import Foundation

/// Wynik syntezy: czysty PCM Float32 + sample rate.
public struct AppleTTSResult {
    public let pcm: [Float]
    public let sampleRate: Int
}

public enum AppleTTSEngine {
    /// Lista zainstalowanych glosow z metadata. Dla wyboru przez panel
    /// uzytkownik widzi `identifier` (uzywany w `synth`), `language`, `name`.
    public static func availableVoices() -> [[String: String]] {
        return AVSpeechSynthesisVoice.speechVoices().map { v in
            [
                "id": v.identifier,
                "name": v.name,
                "language": v.language,
                "quality": v.quality == .enhanced ? "enhanced" : "default",
            ]
        }
    }

    /// Synteza tekstu. `voiceId` to `AVSpeechSynthesisVoice.identifier` (np.
    /// "com.apple.voice.compact.pl-PL.Zosia"); `nil` wybiera domyslny dla `language`.
    /// `language` typu "pl-PL", "en-US". `rate` 0.0-1.0 (0.5 = AVSpeechUtteranceDefaultSpeechRate).
    public static func synthesize(
        text: String,
        voiceId: String? = nil,
        language: String = "en-US",
        rate: Float = AVSpeechUtteranceDefaultSpeechRate
    ) async throws -> AppleTTSResult {
        let utterance = AVSpeechUtterance(string: text)
        if let voiceId, let v = AVSpeechSynthesisVoice(identifier: voiceId) {
            utterance.voice = v
        } else if let v = AVSpeechSynthesisVoice(language: language) {
            utterance.voice = v
        }
        utterance.rate = max(AVSpeechUtteranceMinimumSpeechRate,
                             min(AVSpeechUtteranceMaximumSpeechRate, rate))

        // `write(_:toBufferCallback:)` wymaga zachowania `synth` przy zyciu
        // przez caly czas trwania callbackow. Trzymamy referencje w klasie
        // `Box` zeby ARC nie zwolnil obiektu zaraz po wyjsciu z funkcji.
        // Sygnal koncowy: callback z `frameLength == 0` (Apple docs).
        // Delegate methods (didFinish) NIE sa wolane przez `write(...)` —
        // dziala tylko przez `speak(...)`. Stad detekcja konca tylko przez
        // pusty bufor.
        let box = SynthBox()
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<AppleTTSResult, Error>) in
            box.synth.write(utterance) { buffer in
                if box.completed { return }
                guard let pcmBuffer = buffer as? AVAudioPCMBuffer else { return }
                let frameLen = Int(pcmBuffer.frameLength)
                box.detectedRate = Int(pcmBuffer.format.sampleRate)
                if frameLen == 0 {
                    // Empty buffer = koniec syntezy.
                    box.completed = true
                    cont.resume(returning: AppleTTSResult(
                        pcm: box.samples,
                        sampleRate: box.detectedRate
                    ))
                    return
                }
                if let f32 = pcmBuffer.floatChannelData {
                    let chan = f32[0]
                    box.samples.append(contentsOf: UnsafeBufferPointer(start: chan, count: frameLen))
                } else if let i16 = pcmBuffer.int16ChannelData {
                    let chan = i16[0]
                    let buf = UnsafeBufferPointer(start: chan, count: frameLen)
                    box.samples.reserveCapacity(box.samples.count + frameLen)
                    for s in buf { box.samples.append(Float(s) / 32768.0) }
                }
            }
        }
    }
}

/// Trzyma stan i synth w jednym miejscu zeby nie tracic referencji w trakcie
/// callbackow `write(_:toBufferCallback:)`.
private final class SynthBox {
    let synth = AVSpeechSynthesizer()
    var samples: [Float] = []
    var detectedRate: Int = 0
    var completed: Bool = false
}

// =============================================================================
// C-ABI exports — niezalezne od MLX bridge.
// =============================================================================

/// Listuje dostepne glosy systemowe jako JSON tablice.
/// Caller zwalnia wynik przez free().
@_cdecl("MLXAppleTTS_listVoices")
public func MLXAppleTTS_listVoices() -> UnsafeMutablePointer<CChar>? {
    let voices = AppleTTSEngine.availableVoices()
    guard let data = try? JSONSerialization.data(withJSONObject: voices),
          let s = String(data: data, encoding: .utf8) else { return nil }
    return strdup(s)
}

/// Wynik syntezy zwracany jako (sample_rate, num_samples, samples_ptr).
/// Caller zwalnia bufor `samples_ptr` przez `MLXAppleTTS_freeBuffer`.
@_cdecl("MLXAppleTTS_synthesize")
public func MLXAppleTTS_synthesize(
    text: UnsafePointer<CChar>?,
    voiceId: UnsafePointer<CChar>?,
    language: UnsafePointer<CChar>?,
    rate: Float,
    outSampleRate: UnsafeMutablePointer<Int32>?,
    outNumSamples: UnsafeMutablePointer<Int32>?
) -> UnsafeMutablePointer<Float>? {
    guard let text = text.flatMap({ String(cString: $0) }),
          let outSampleRate, let outNumSamples else { return nil }
    let voiceIdStr = voiceId.flatMap { String(cString: $0) }
    let languageStr = language.flatMap { String(cString: $0) } ?? "en-US"

    let semaphore = DispatchSemaphore(value: 0)
    var result: AppleTTSResult? = nil
    Task {
        do {
            result = try await AppleTTSEngine.synthesize(
                text: text,
                voiceId: voiceIdStr,
                language: languageStr,
                rate: rate
            )
        } catch {
            print("[AppleTTS] synthesize error: \(error)")
        }
        semaphore.signal()
    }
    semaphore.wait()
    guard let r = result, !r.pcm.isEmpty else { return nil }

    outSampleRate.pointee = Int32(r.sampleRate)
    outNumSamples.pointee = Int32(r.pcm.count)
    let buf = UnsafeMutablePointer<Float>.allocate(capacity: r.pcm.count)
    buf.update(from: r.pcm, count: r.pcm.count)
    return buf
}

/// Zwalnia bufor zwrocony przez `MLXAppleTTS_synthesize`. Bezpieczny dla NULL.
@_cdecl("MLXAppleTTS_freeBuffer")
public func MLXAppleTTS_freeBuffer(ptr: UnsafeMutablePointer<Float>?) {
    ptr?.deallocate()
}
