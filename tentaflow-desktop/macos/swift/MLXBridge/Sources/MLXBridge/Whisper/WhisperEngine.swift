// =============================================================================
// Plik: WhisperEngine.swift
// Opis: Singleton silnik MLX Whisper + FFI exports `MLXWhisper_*` dla Rust.
//       API kontrakt analogiczny do `MLXBridge_*` (LLM):
//         MLXWhisper_getContext()
//         MLXWhisper_loadModel(modelDir)
//         MLXWhisper_unloadModel()
//         MLXWhisper_transcribe(pcmPtr, nSamples, language, callback)
//
//       PCM dostarczany przez Rust to f32 mono 16 kHz znormalizowany do
//       [-1, 1] (audio bridge w teams-bocie juz robi resampling). Caller
//       wysyla od razu pelne okno 30s lub krotsze — pad/trim robimy my.
// =============================================================================

import Foundation
import MLX

/// Pobiera mlx-community/whisper-* (config + safetensors) i openai/whisper-*
/// (tokenizer.json) z HF Hub bezposrednio przez URLSession do scalonego
/// katalogu cache. Brak zaleznosci od `Hub` modulu swift-transformers (ten
/// nie jest wystawiony jako library product w 0.1.20). macOS analog:
/// `tentaflow-core/src/stt/mlx_whisper.rs::prepare_model`.
public enum MLXWhisperPrepare {
    public static func tokenizerRepoId(for mlxRepoId: String) -> String {
        let lower = mlxRepoId.lowercased()
        if lower.contains("v3") || lower.contains("turbo") {
            return "openai/whisper-large-v3-turbo"
        }
        return "openai/whisper-large-v2"
    }

    /// Sciagnij `https://huggingface.co/<repo>/resolve/main/<file>` do `dst`.
    /// Idempotentne — jezeli plik juz istnieje, pomijamy. `optional=true`
    /// powoduje ze 404 nie jest bledem (niektore tokenizer files moga nie istniec).
    private static func downloadFile(
        repo: String, file: String, dst: URL, optional: Bool
    ) async throws {
        let fm = FileManager.default
        if fm.fileExists(atPath: dst.path) { return }
        guard let url = URL(string: "https://huggingface.co/\(repo)/resolve/main/\(file)") else {
            throw NSError(domain: "MLXWhisperPrepare", code: 1)
        }
        let (tmp, response) = try await URLSession.shared.download(from: url)
        if let http = response as? HTTPURLResponse {
            if http.statusCode == 404 && optional {
                try? fm.removeItem(at: tmp)
                return
            }
            if http.statusCode != 200 {
                try? fm.removeItem(at: tmp)
                throw NSError(
                    domain: "MLXWhisperPrepare", code: http.statusCode,
                    userInfo: [NSLocalizedDescriptionKey: "HTTP \(http.statusCode) \(url)"]
                )
            }
        }
        try fm.createDirectory(at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
        if fm.fileExists(atPath: dst.path) {
            try fm.removeItem(at: dst)
        }
        try fm.moveItem(at: tmp, to: dst)
    }

    /// Pobiera oba repo. Zwraca sciezke gotowa do `MLXWhisperEngine.shared.loadModel`.
    public static func prepare(repoId: String) async throws -> URL {
        let mlxFiles = ["config.json", "model.safetensors"]
        // openai whisper repo: lista plikow tokenizera. Niektore (added_tokens,
        // normalizer) sa opcjonalne, brak nie jest bledem.
        let oaiFilesRequired = ["tokenizer.json", "tokenizer_config.json"]
        let oaiFilesOptional = [
            "added_tokens.json",
            "special_tokens_map.json",
            "generation_config.json",
            "vocab.json",
            "merges.txt",
            "normalizer.json",
        ]
        let fm = FileManager.default
        let baseDir = try fm.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ).appendingPathComponent("tentaflow-mlx-whisper", isDirectory: true)
        let safeName = repoId.replacingOccurrences(of: "/", with: "_")
        let target = baseDir.appendingPathComponent(safeName, isDirectory: true)
        try fm.createDirectory(at: target, withIntermediateDirectories: true)

        // Sprawdzamy szybko: pelen zestaw plikow juz istnieje — short-circuit.
        let already = mlxFiles.allSatisfy { fm.fileExists(atPath: target.appendingPathComponent($0).path) }
            && oaiFilesRequired.allSatisfy { fm.fileExists(atPath: target.appendingPathComponent($0).path) }
        if already { return target }

        let oaiRepo = tokenizerRepoId(for: repoId)
        for f in mlxFiles {
            try await downloadFile(repo: repoId, file: f, dst: target.appendingPathComponent(f), optional: false)
        }
        for f in oaiFilesRequired {
            try await downloadFile(repo: oaiRepo, file: f, dst: target.appendingPathComponent(f), optional: false)
        }
        for f in oaiFilesOptional {
            try await downloadFile(repo: oaiRepo, file: f, dst: target.appendingPathComponent(f), optional: true)
        }
        return target
    }
}

public final class MLXWhisperEngine {
    public static let shared = MLXWhisperEngine()

    private var model: Whisper?
    private var tokenizer: WhisperTokenizer?
    private var modelPath: String?

    private init() {}

    /// Laduje model z katalogu HF snapshot. Synchroniczne (uzywamy
    /// DispatchSemaphore zeby Rust mogl czekac na koniec).
    public func loadModel(path: String) -> Bool {
        let url = URL(filePath: path)
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXWhisper] Sciezka nie istnieje: \(path)")
            return false
        }
        let semaphore = DispatchSemaphore(value: 0)
        var success = false
        Task {
            do {
                print("[MLXWhisper] Ladowanie modelu z \(path)")
                let m = try WhisperLoader.load(directory: url)
                let tk = try await WhisperTokenizer(folder: url)
                self.model = m
                self.tokenizer = tk
                self.modelPath = path
                success = true
                print("[MLXWhisper] Model zaladowany — \(m.config.nVocab) tokenow, \(m.config.nAudioLayer)/\(m.config.nTextLayer) warstw enc/dec")
            } catch {
                print("[MLXWhisper] Blad ladowania: \(error)")
            }
            semaphore.signal()
        }
        semaphore.wait()
        return success
    }

    public func unloadModel() {
        self.model = nil
        self.tokenizer = nil
        self.modelPath = nil
    }

    /// Transkrybuje audio dowolnej dlugosci. Dziala w trybie chunked: dzieli
    /// PCM na nienakladajace sie okna 30s (`nSamples = 480_000`), transkrybuje
    /// kazde osobno i sklada wyniki jednym pojedynczym odstepem.
    ///
    /// Ograniczenia MVP:
    ///   - Nie korzysta z tokenow timestampow do sklejania (czasem traci
    ///     ostatnie/pierwsze slowo na granicy 30s). Dla typowej rozmowy
    ///     bez przerw to nieuniknione bez timestamp-aware seek (TODO).
    ///   - `eval` na koniec kazdego okna — pamiec GPU jest zwalniana
    ///     przed nastepnym chunkiem zeby dlugie nagrania (>>30 min) sie
    ///     nie blokowaly na limicie cache.
    public func transcribe(pcm: [Float], language: String) -> String? {
        guard let model, let tokenizer else {
            print("[MLXWhisper] Brak zaladowanego modelu")
            return nil
        }
        let total = pcm.count
        if total == 0 { return "" }
        let windowSamples = WhisperAudio.nSamples
        let sampleRate = WhisperAudio.sampleRate
        var pieces: [String] = []
        var offset = 0
        var chunkIdx = 0
        // Bezpiecznik: minimum 1s przesuniecia po znalezieniu timestampa zeby
        // model wpadly w nieskonczona petle gdy halucynuje "<|0.00|>" jako
        // ostatni token (zdarza sie na pustych odcinkach).
        let minSeekSamples = sampleRate
        while offset < total {
            let end = min(offset + windowSamples, total)
            let slice = Array(pcm[offset ..< end])
            let samples = MLXArray(slice)
            let padded = WhisperAudio.padOrTrim(samples)
            let mel = WhisperAudio.logMelSpectrogram(samples: padded, nMels: model.config.nMels)
            eval(mel)
            let result = WhisperDecoder.transcribe(
                model: model,
                tokenizer: tokenizer,
                mel: mel,
                language: language
            )
            // Whisper standardowy filter ciszy: gdy `<|nospeech|>` dominuje
            // i avgLogprob jest niski, calkowicie ignorujemy okno (zamiast
            // emitowac halucynacje typu "Thank you for watching").
            //   - noSpeech AND niska pewnosc — to OpenAI default (wymaga obu)
            //   - compressionRatio > 2.4 — powtarzalny tekst (to klasyczna
            //     petla halucynacji, np. "tak tak tak tak..."); Whisper default
            //   - blacklista znanych outrow podcastowych (PL/EN) — Whisper byl
            //     trenowany na YT/podcastach, na ciszy odpala wyuczone outro
            //     z dobrymi statystykami (low noSpeech, ok logprob), wiec
            //     filtry probabilistyczne ich nie lapia
            let decodedText = tokenizer.decode(tokens: result.tokens)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let isSilenceProb = result.noSpeechProb > 0.6 && result.avgLogprob < -1.0
            let isRepeatLoop = result.compressionRatio > 2.4
            let isKnownHallucination = WhisperHallucinationFilter.isHallucination(decodedText)
            let text: String = (isSilenceProb || isRepeatLoop || isKnownHallucination)
                ? ""
                : decodedText
            if isKnownHallucination {
                print("[MLXWhisper] odrzucono halucynacje (known phrase): \(decodedText.prefix(80))")
            } else if isRepeatLoop {
                print("[MLXWhisper] odrzucono halucynacje (compression=\(String(format: "%.2f", result.compressionRatio))): \(decodedText.prefix(80))")
            }
            // Decyduj o seek na podstawie ostatniego znalezionego timestampa.
            // Gdy go brak (model nie zamkanl okna timestampem) idziemy pelne 30s.
            let advance: Int
            if let ts = result.lastTimestampSeconds, ts > 0 {
                let advanceSamples = Int(ts * Double(sampleRate))
                advance = max(advanceSamples, minSeekSamples)
            } else {
                advance = windowSamples
            }
            print("[MLXWhisper] chunk \(chunkIdx) offset=\(offset / sampleRate)s adv=\(advance / sampleRate)s: \(text.prefix(80))")
            if !text.isEmpty { pieces.append(text) }
            offset += min(advance, total - offset)
            chunkIdx += 1
            // Cache GPU jest istotny dla dlugich nagran (godzinnych) — bez
            // tego unified memory rosnie liniowo z liczba chunkow.
            MLX.GPU.clearCache()
            // Bezpiecznik na nieskonczona petle: max 200 chunkow (≈100 min audio).
            if chunkIdx > 200 { break }
        }
        return pieces.joined(separator: " ")
    }
}

// =============================================================================
// C-ABI exports — sygnatury MUSZA pasowac do
// tentaflow-core/src/inference/mlx_whisper_bridge.rs (utworzony w nastepnym kroku).
// =============================================================================

@_cdecl("MLXWhisper_getContext")
public func MLXWhisper_getContext() -> UnsafeMutableRawPointer {
    return Unmanaged.passUnretained(MLXWhisperEngine.shared).toOpaque()
}

@_cdecl("MLXWhisper_loadModel")
public func MLXWhisper_loadModel(
    modelPath: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }
    let engine = Unmanaged<MLXWhisperEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

/// Pobiera oba HF repo i scala je w jednym katalogu, podobnie jak Rust
/// `prepare_model`. Zwraca strdup'owany C-string z absolutna sciezka (caller
/// zwalnia przez free()), albo NULL w razie bledu. Synchroniczne — uruchamia
/// async Task w semafor.
@_cdecl("MLXWhisper_prepareModel")
public func MLXWhisper_prepareModel(
    repoId: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    guard let repoIdStr = repoId.flatMap({ String(cString: $0) }) else { return nil }
    let semaphore = DispatchSemaphore(value: 0)
    var resolvedPath: String? = nil
    Task {
        do {
            let url = try await MLXWhisperPrepare.prepare(repoId: repoIdStr)
            resolvedPath = url.path
        } catch {
            print("[MLXWhisper] prepareModel(\(repoIdStr)) failed: \(error)")
        }
        semaphore.signal()
    }
    semaphore.wait()
    guard let p = resolvedPath else { return nil }
    return strdup(p)
}

@_cdecl("MLXWhisper_unloadModel")
public func MLXWhisper_unloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<MLXWhisperEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

/// `pcmPtr`: Float32 array of mono 16kHz samples in [-1, 1].
/// `nSamples`: liczba probek (max 480_000 = 30s; wieksze sa przycinane).
/// `language`: dwuliterowy kod ISO ("en", "pl", ...). NULL → "en".
/// Zwraca strdup-owany C-string z transkrypcja (caller wola free()).
@_cdecl("MLXWhisper_transcribe")
public func MLXWhisper_transcribe(
    pcmPtr: UnsafePointer<Float>?,
    nSamples: Int32,
    language: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard let pcmPtr, nSamples > 0, let ctx = context else { return nil }
    let engine = Unmanaged<MLXWhisperEngine>.fromOpaque(ctx).takeUnretainedValue()
    let lang = language.flatMap { String(cString: $0) } ?? "en"
    let buffer = UnsafeBufferPointer(start: pcmPtr, count: Int(nSamples))
    let pcm = Array(buffer)
    guard let text = engine.transcribe(pcm: pcm, language: lang) else { return nil }
    return strdup(text)
}
