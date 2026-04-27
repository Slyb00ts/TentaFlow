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

    /// Synchroniczna wersja syntezy uzywana przez cdecl FFI. Nie spawnuje
    /// Taska — wewnetrzny `synth.write` callback jest wywolywany na biezacym
    /// thread (Rust spawn_blocking → tu). Wczesniej uzylismy Task + semaphore,
    /// ale to dawalo deadlock gdy caller blokowal main thread.
    public static func synthesizeSync(
        text: String,
        voiceId: String? = nil,
        language: String = "en-US",
        rate: Float = AVSpeechUtteranceDefaultSpeechRate
    ) -> AppleTTSResult? {
        // AVSpeechSynthesizer.write wywoluje callback przez CFRunLoop.main
        // (lub current). Gdy caller jest Rust thread bez RunLoop, callback
        // nigdy nie sie nie odpala — pusty bufor. Rozwiazanie: dispatch
        // synchronicznie na main queue przez `DispatchQueue.main.sync`,
        // ktore CFRunLoop main przeprocesuje. Caller (Rust spawn_blocking)
        // musi NIE byc na main queue — w naszym setupie spawn_blocking idzie
        // na thread pool, wiec OK.
        let utterance = AVSpeechUtterance(string: text)
        if let voiceId, let v = AVSpeechSynthesisVoice(identifier: voiceId) {
            utterance.voice = v
        } else if let v = AVSpeechSynthesisVoice(language: language) {
            utterance.voice = v
        }
        utterance.rate = max(AVSpeechUtteranceMinimumSpeechRate,
                             min(AVSpeechUtteranceMaximumSpeechRate, rate))

        // Wszystko ponizej musi sie dziac na MAIN thread bo AVSpeechSynthesizer
        // dispatches callbacks z main runloop. Sami sterujemy run-loop az do
        // pojawienia sie pustego bufora (sygnal konca).
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
            // Pompuj run loop az synth.write skonczy generacje. Limit 60 s.
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

    /// Async wersja syntezy — uzywana z poziomu Swift bez cdecl. Zachowana dla
    /// CLI testow i przyszlej integracji iOS gdzie continuation jest naturalne.
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

    // Uzywamy synchronicznego wariantu — Task + semaphore.wait dawal deadlock
    // gdy caller (Rust dlsym) wywolywal nas z main thread.
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

/// Zwalnia bufor zwrocony przez `MLXAppleTTS_synthesize`. Bezpieczny dla NULL.
@_cdecl("MLXAppleTTS_freeBuffer")
public func MLXAppleTTS_freeBuffer(ptr: UnsafeMutablePointer<Float>?) {
    ptr?.deallocate()
}
