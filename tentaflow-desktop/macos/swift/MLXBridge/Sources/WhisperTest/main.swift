// =============================================================================
// Plik: WhisperTest/main.swift
// Opis: CLI runner do testowania portu MLX Whisper. Uzycie:
//         swift run -c release WhisperTest <model_dir> <wav_file> [language]
//       np.
//         swift run -c release WhisperTest /tmp/whisper-test/model /tmp/whisper-test/long.wav en
//
//       Tym samym kodem co dla Rust FFI (engine.transcribe), zeby blad
//       w portu pokazal sie tu rownie szybko jak w prawdziwym pipeline.
// =============================================================================

import Foundation
import MLXBridge

@main
struct WhisperTest {
    static func main() async {
        let args = CommandLine.arguments
        guard args.count >= 3 else {
            print("Usage: WhisperTest <model_dir> <wav_file> [language]")
            print("       <wav_file> must be PCM 16-bit mono 16 kHz.")
            exit(2)
        }
        let modelDir = args[1]
        let wavPath = args[2]
        let language = args.count >= 4 ? args[3] : "en"

        // Load model
        let t0 = Date()
        guard MLXWhisperEngine.shared.loadModel(path: modelDir) else {
            print("ERROR: load failed")
            exit(1)
        }
        print("[test] model loaded in \(String(format: "%.2f", -t0.timeIntervalSinceNow))s")

        // Read WAV
        let url = URL(filePath: wavPath)
        let pcm: [Float]
        do {
            let arr = try WhisperAudio.loadPCM16(url: url)
            // MLXArray -> [Float] for the public API. asArray(Float.self) wymaga eval.
            pcm = arr.asArray(Float.self)
        } catch {
            print("ERROR: read WAV: \(error)")
            exit(1)
        }
        print("[test] loaded \(pcm.count) samples (\(String(format: "%.2f", Double(pcm.count) / 16000.0))s)")

        // Transcribe
        let t1 = Date()
        let text = MLXWhisperEngine.shared.transcribe(pcm: pcm, language: language)
        let elapsed = -t1.timeIntervalSinceNow
        print("===")
        print("LANG: \(language)")
        print("TIME: \(String(format: "%.2f", elapsed))s")
        if let text {
            print("TEXT: \(text)")
        } else {
            print("TEXT: <nil>")
            exit(1)
        }
    }
}
