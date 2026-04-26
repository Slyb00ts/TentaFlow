// =============================================================================
// Plik: AppleTtsCli.swift
// Opis: Maly CLI do testowania `AppleTTSEngine` bez calego stacku Rusta.
//       Uruchamiac jako:
//         swift run -c release WhisperTest --apple-tts "Tekst do wymowy" pl-PL out.wav
//       Detekcja po pierwszym argumencie `--apple-tts`.
// =============================================================================

import AVFoundation
import Foundation
import MLXBridge

enum AppleTtsCli {
    /// Zapisuje PCM Float32 jako WAV mono. Sample rate w naglowku.
    static func writeWav(samples: [Float], sampleRate: Int, to url: URL) throws {
        let bytesPerSample = 2  // konwersja na Int16 dla kompatybilnosci
        let dataSize = samples.count * bytesPerSample
        var data = Data()
        // RIFF header
        data.append(contentsOf: Array("RIFF".utf8))
        var chunkSize = UInt32(36 + dataSize).littleEndian
        data.append(Data(bytes: &chunkSize, count: 4))
        data.append(contentsOf: Array("WAVE".utf8))
        // fmt chunk
        data.append(contentsOf: Array("fmt ".utf8))
        var fmtSize = UInt32(16).littleEndian
        data.append(Data(bytes: &fmtSize, count: 4))
        var audioFormat = UInt16(1).littleEndian  // PCM
        data.append(Data(bytes: &audioFormat, count: 2))
        var channels = UInt16(1).littleEndian
        data.append(Data(bytes: &channels, count: 2))
        var sr = UInt32(sampleRate).littleEndian
        data.append(Data(bytes: &sr, count: 4))
        var byteRate = UInt32(sampleRate * bytesPerSample).littleEndian
        data.append(Data(bytes: &byteRate, count: 4))
        var blockAlign = UInt16(bytesPerSample).littleEndian
        data.append(Data(bytes: &blockAlign, count: 2))
        var bps = UInt16(16).littleEndian
        data.append(Data(bytes: &bps, count: 2))
        // data chunk
        data.append(contentsOf: Array("data".utf8))
        var dSize = UInt32(dataSize).littleEndian
        data.append(Data(bytes: &dSize, count: 4))
        var pcm16 = [Int16]()
        pcm16.reserveCapacity(samples.count)
        for s in samples {
            let clamped = max(-1.0, min(1.0, s))
            pcm16.append(Int16(clamped * 32767.0))
        }
        pcm16.withUnsafeBytes { data.append(contentsOf: $0) }
        try data.write(to: url)
    }

    static func main(_ args: [String]) async {
        guard args.count >= 4 else {
            print("Usage: WhisperTest --apple-tts <text> <language> <out.wav>")
            print("Example: WhisperTest --apple-tts 'Witaj swiecie' pl-PL /tmp/zosia.wav")
            exit(2)
        }
        let text = args[1]
        let language = args[2]
        let outPath = args[3]
        do {
            let result = try await AppleTTSEngine.synthesize(
                text: text, voiceId: nil, language: language
            )
            try writeWav(samples: result.pcm, sampleRate: result.sampleRate, to: URL(filePath: outPath))
            print("[apple-tts] \(result.pcm.count) samples @ \(result.sampleRate) Hz -> \(outPath)")
        } catch {
            print("[apple-tts] ERROR: \(error)")
            exit(1)
        }
    }
}
