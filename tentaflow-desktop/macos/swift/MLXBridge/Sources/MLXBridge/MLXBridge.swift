// =============================================================================
// Plik: MLXBridge.swift
// Opis: Mostek mlx-swift / MLXLLM dla TentaFlow desktop macOS. Adaptacja
//       tentaflow-mobile/ios/TentaFlowAI/MLXSwiftEngine.swift gdzie Bielik
//       4.5B 4-bit dziala bez zarzutu. Eksportuje cztery `@_cdecl` funkcje
//       ktorych Rust desktop bin uzywa do FFI registration:
//         MLXBridge_loadModel
//         MLXBridge_unloadModel
//         MLXBridge_generate
//         MLXBridge_modelInfo
//       Plus akcesor `MLXBridge_getContext` zwracajacy pointer na singleton
//       silnika — Rust przekazuje go jako `context` do callbackow.
// =============================================================================

import Foundation
import HuggingFace
import MLX
import MLXEmbedders
import MLXHuggingFace
import MLXLLM
import MLXLMCommon
import Tokenizers

/// Silnik MLX na macOS — singleton zarzadzajacy modelem MLX i FFI callbacks.
/// @unchecked Sendable: dostep z watkow FFI Rusta jest serializowany przez
/// DispatchSemaphore w kazdej metodzie (Swift 6 nie widzi tego sam).
public final class MLXBridgeEngine: @unchecked Sendable {
    /// Globalny singleton — Swift gwarantuje thread-safe lazy init.
    public static let shared = MLXBridgeEngine()

    private var modelContainer: ModelContainer?
    private var modelPath: String?

    /// Kontener modelu embeddingow (osobny od LLM — moga wspolistniec).
    /// Ladowany przez `loadModel` gdy katalog ma `1_Pooling/config.json`
    /// (sentence-transformers, np. jina-embeddings-v5 / Qwen3-Embedding).
    private var embedderContainer: EmbedderModelContainer?

    private init() {
        // Limit cache GPU — wystarczy duzo dla M-series, ale nie bezgranicznie.
        // 256 MB dziala dobrze dla modeli 4-7B 4-bit; wieksze modele beda
        // korzystac z system memory through unified memory architecture.
        MLX.GPU.set(cacheLimit: 256 * 1024 * 1024)
    }

    /// Laduje model z podanej sciezki. Synchroniczne (blokuje watek wolajacy).
    public func loadModel(path: String) -> Bool {
        print("[MLXBridge] Ladowanie modelu: \(path)")

        let url = URL(filePath: path)
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXBridge] Sciezka nie istnieje: \(path)")
            return false
        }

        // Sentence-transformers (embeddingi) maja `1_Pooling/config.json` —
        // wtedy ladujemy przez EmbedderModelFactory zamiast LLMModelFactory.
        let poolingConfig = url.appending(components: "1_Pooling", "config.json")
        if FileManager.default.fileExists(atPath: poolingConfig.path) {
            return loadEmbedder(url: url, path: path)
        }

        let semaphore = DispatchSemaphore(value: 0)
        var success = false

        Task {
            do {
                let config = ModelConfiguration(directory: url)
                // 3.x odpial tokenizer/downloader: makra MLXHuggingFace wstrzykuja
                // HubApi (HuggingFace) i loader tokenizera (Tokenizers).
                self.modelContainer = try await LLMModelFactory.shared.loadContainer(
                    from: #hubDownloader(),
                    using: #huggingFaceTokenizerLoader(),
                    configuration: config
                ) { progress in
                    let pct = Int(progress.fractionCompleted * 100)
                    if pct % 25 == 0 {
                        print("[MLXBridge] Ladowanie: \(pct)%")
                    }
                }
                self.modelPath = path
                success = true
                print("[MLXBridge] Model zaladowany pomyslnie")
            } catch {
                print("[MLXBridge] Blad ladowania: \(error)")
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Wyladowuje model z pamieci (LLM i embedder).
    public func unloadModel() {
        print("[MLXBridge] Wyladowywanie modelu")
        modelContainer = nil
        embedderContainer = nil
        modelPath = nil
    }

    /// Wymusza zaladowanie modelu embeddingow przez EmbedderModelFactory,
    /// niezaleznie od obecnosci `1_Pooling/config.json`. Repo `-mlx` Jina v5
    /// (qwen3 decoder-only) NIE niesie katalogu sentence-transformers, wiec
    /// heurystyka `loadModel` wziela by je za LLM. Deploy zna `category =
    /// embeddings`, wiec wola te sciezke wprost. Pooling fallbackuje na
    /// `Qwen3Model.poolingStrategy = .last` gdy brak 1_Pooling.
    public func loadEmbedderModel(path: String) -> Bool {
        let url = URL(filePath: path)
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXBridge] Sciezka embeddera nie istnieje: \(path)")
            return false
        }
        return loadEmbedder(url: url, path: path)
    }

    /// Laduje model embeddingow (sentence-transformers) przez EmbedderModelFactory.
    /// Synchroniczne (blokuje watek wolajacy), jak loadModel.
    private func loadEmbedder(url: URL, path: String) -> Bool {
        print("[MLXBridge] Ladowanie modelu embeddingow: \(path)")
        let semaphore = DispatchSemaphore(value: 0)
        var success = false
        Task {
            do {
                let config = ModelConfiguration(directory: url)
                self.embedderContainer = try await EmbedderModelFactory.shared.loadContainer(
                    from: #hubDownloader(),
                    using: #huggingFaceTokenizerLoader(),
                    configuration: config
                ) { progress in
                    let pct = Int(progress.fractionCompleted * 100)
                    if pct % 25 == 0 {
                        print("[MLXBridge] Ladowanie embeddera: \(pct)%")
                    }
                }
                self.modelPath = path
                success = true
                print("[MLXBridge] Model embeddingow zaladowany")
            } catch {
                print("[MLXBridge] Blad ladowania embeddera: \(error)")
                success = false
            }
            semaphore.signal()
        }
        semaphore.wait()
        return success
    }

    /// Liczy embedding dla jednego tekstu. Zwraca wektor floatow albo nil.
    /// Pooling (mean/last/cls) i L2-normalizacja sa wyznaczane z 1_Pooling
    /// config modelu — dla jina-embeddings-v5 to last-token + normalize.
    public func embed(text: String) -> [Float]? {
        guard let container = embedderContainer else {
            print("[MLXBridge] Brak zaladowanego modelu embeddingow")
            return nil
        }
        let semaphore = DispatchSemaphore(value: 0)
        var result: [Float]? = nil
        Task {
            do {
                let vec = try await container.perform { (ctx: EmbedderModelContext) -> [Float] in
                    let ids = ctx.tokenizer.encode(text: text, addSpecialTokens: true)
                    let input = MLXArray(ids).reshaped([1, ids.count])
                    let mask = MLXArray.ones([1, ids.count])
                    let output = ctx.model(
                        input, positionIds: nil, tokenTypeIds: nil, attentionMask: mask)
                    let pooled = ctx.pooling(output, mask: mask, normalize: true)
                    pooled.eval()
                    return pooled.asArray(Float.self)
                }
                result = vec
            } catch {
                print("[MLXBridge] Blad embed: \(error)")
                result = nil
            }
            semaphore.signal()
        }
        semaphore.wait()
        return result
    }

    /// Generuje tekst z callbackiem na każdy token. Synchroniczne.
    /// Zwraca kod: 0=OK, -1=blad generyczny, -10=brak pamieci/przekroczony
    /// kontekst (guard przerwal zanim doszlo do OOM). maxContextTokens=0 i
    /// memoryBudgetMB=0 wylaczaja odpowiednie limity (zachowanie jak wczesniej).
    public func generate(
        prompt: String,
        maxTokens: Int,
        temperature: Float,
        topP: Float,
        maxContextTokens: Int,
        memoryBudgetMB: Int,
        // (text, isFinal, promptTokens, completionTokens, prefillTps, decodeTps).
        // Liczniki i prędkości faz są niezerowe tylko na finalnym wywołaniu
        // (isFinal=true). prefillTps/decodeTps pochodzą z realnych pomiarów MLX
        // (GenerateCompletionInfo), nie z wall-clock TTFT.
        tokenCallback: @escaping (String, Bool, Int, Int, Double, Double) -> Void
    ) -> Int32 {
        guard let container = modelContainer else {
            print("[MLXBridge] Brak zaladowanego modelu")
            return -1
        }

        print("[MLXBridge] Generowanie: max_tokens=\(maxTokens), temp=\(temperature), topP=\(topP), maxCtx=\(maxContextTokens), budgetMB=\(memoryBudgetMB)")
        print("[MLXBridge] Prompt (\(prompt.count) znakow): \(prompt.prefix(200))")

        // Limit pamieci MLX z relaxed:true: pozwala MLX zwalniac cache/eviction
        // zamiast wywalac caly proces (natywny abort/trap) przy przekroczeniu
        // budzetu. Przy wspolrezydencji wielu modeli MLX (np. whisper STT + LLM)
        // suma wag + KV moze przekroczyc budzet — relaxed:false ubilby proces.
        // Budzet egzekwuje czysto miekki straznik snapshotu w callbacku tokenow
        // (zwraca .stop -> kod -10), wiec nie potrzebujemy twardego trapa.
        let budgetBytes = memoryBudgetMB > 0 ? memoryBudgetMB * 1024 * 1024 : 0
        if budgetBytes > 0 {
            MLX.GPU.set(memoryLimit: budgetBytes, relaxed: true)
        }

        let semaphore = DispatchSemaphore(value: 0)
        var resultCode: Int32 = -1

        // Bielik 4-bit (i ogolnie male instruct modele) bez `repetitionPenalty`
        // wpadaja w pętle po 200+ tokenach. Conservative default 1.1 z context
        // size 20 jest standardem dla mlx-swift LLMEval i nie psuje koherencji.
        let parameters = GenerateParameters(
            temperature: temperature,
            topP: topP,
            repetitionPenalty: 1.1,
            repetitionContextSize: 20
        )

        Task {
            do {
                let counts = try await container.perform { context in
                    // Prompt juz jest sformatowany przez Rust (ChatML lub Mistral).
                    let tokenIds = context.tokenizer.encode(text: prompt)
                    // Sam prompt przekracza budzet kontekstu -> odmow zadania
                    // czysto, zanim cokolwiek policzymy.
                    if maxContextTokens > 0 && tokenIds.count > maxContextTokens {
                        throw MLXContextBudgetExceeded()
                    }
                    let promptCount = tokenIds.count
                    let inputTokens = MLXArray(tokenIds)
                    let input = LMInput(tokens: inputTokens)

                    let stopStrings = ["<|im_end|>", "<|endoftext|>", "</s>"]

                    // Flaga OOM w scope `perform` (jak `lastOutput`) — unika
                    // mutacji zmiennej func-scope w @Sendable token-closure.
                    var lastOutput = ""
                    var memExceeded = false
                    // Licznik wygenerowanych tokenow (perform-scope, jak lastOutput)
                    // — mutowany w @Sendable token-closure, zwracany na koncu.
                    var genCount = 0
                    let info = try MLXLMCommon.generate(
                        input: input,
                        parameters: parameters,
                        context: context
                    ) { tokens in
                        // Guard pamieci: przerwij czysto zanim MLX zrobi OOM.
                        if budgetBytes > 0 {
                            let snap = MLX.GPU.snapshot()
                            if snap.activeMemory + snap.cacheMemory > budgetBytes {
                                memExceeded = true
                                return .stop
                            }
                        }
                        // Inkrementalny dekod — emituj tylko nowy fragment.
                        let fullText = context.tokenizer.decode(tokenIds: tokens)
                        if fullText.count > lastOutput.count {
                            let newPart = String(fullText.dropFirst(lastOutput.count))
                            tokenCallback(newPart, false, 0, 0, 0, 0)
                        }
                        lastOutput = fullText
                        genCount = tokens.count

                        for stop in stopStrings {
                            if fullText.contains(stop) {
                                return .stop
                            }
                        }
                        // Cap kontekstu wyliczony z budzetu pamieci (prompt+gen).
                        if maxContextTokens > 0 && promptCount + tokens.count >= maxContextTokens {
                            return .stop
                        }
                        return tokens.count >= maxTokens ? .stop : .more
                    }
                    if memExceeded { throw MLXContextBudgetExceeded() }
                    // Realne prędkości faz z MLX: promptTokensPerSecond = prefill,
                    // tokensPerSecond = decode. Dużo dokładniejsze niż wall-clock.
                    return (promptCount, genCount, info.promptTokensPerSecond, info.tokensPerSecond)
                }

                tokenCallback("", true, counts.0, counts.1, counts.2, counts.3)
                resultCode = 0
                print("[MLXBridge] Generowanie zakonczone (kod \(resultCode))")
            } catch {
                print("[MLXBridge] Blad generowania: \(error)")
                tokenCallback("", true, 0, 0, 0, 0)
                // Przekroczony kontekst albo blad alokacji pod twardym budzetem
                // -> raportuj jako brak pamieci; inne bledy jako generyczne.
                if error is MLXContextBudgetExceeded || budgetBytes > 0 {
                    resultCode = -10
                } else {
                    resultCode = -1
                }
            }
            semaphore.signal()
        }

        semaphore.wait()
        return resultCode
    }

    /// Zwraca JSON z info o zaladowanym modelu.
    public func modelInfo() -> String? {
        guard modelContainer != nil, let path = modelPath else { return nil }
        let name = URL(filePath: path).lastPathComponent
        let info: [String: Any] = [
            "name": name,
            "path": path,
            "backend": "mlx-swift",
            "loaded": true,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: info),
           let json = String(data: data, encoding: .utf8) {
            return json
        }
        return nil
    }
}

/// Sygnalizuje, ze sam prompt nie miesci sie w limicie kontekstu — mapowane na
/// kod -10 (brak pamieci) przez @_cdecl, zeby Rust dal czysty komunikat.
private struct MLXContextBudgetExceeded: Error {}

// =============================================================================
// C-ABI exports — Rust uzywa ich przez FFI w tentaflow_register_mlx_swift.
// Sygnatury MUSZA pasowac dokladnie do typow w
// tentaflow-core/src/inference/mlx_swift_bridge.rs.
// =============================================================================

/// Zwraca surowy pointer na singleton silnika. Rust przekazuje go jako
/// `context` do każdego z czterech callbacków poniżej.
@_cdecl("MLXBridge_getContext")
public func MLXBridge_getContext() -> UnsafeMutableRawPointer {
    return Unmanaged.passUnretained(MLXBridgeEngine.shared).toOpaque()
}

@_cdecl("MLXBridge_loadModel")
public func MLXBridge_loadModel(
    modelPath: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

@_cdecl("MLXBridge_loadEmbedder")
public func MLXBridge_loadEmbedder(
    modelPath: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadEmbedderModel(path: path) ? 0 : -1
}

@_cdecl("MLXBridge_unloadModel")
public func MLXBridge_unloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

@_cdecl("MLXBridge_generate")
public func MLXBridge_generate(
    prompt: UnsafePointer<CChar>?,
    maxTokens: Int32,
    temperature: Float,
    topP: Float,
    maxContextTokens: Int32,
    memoryBudgetMB: Int32,
    tokenCallback: (@convention(c) (UnsafePointer<CChar>?, Bool, UInt32, UInt32, Float, Float, UnsafeMutableRawPointer?) -> Void)?,
    callbackContext: UnsafeMutableRawPointer?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let promptStr = prompt.flatMap({ String(cString: $0) }),
          let ctx = context,
          let tokenCb = tokenCallback else { return -1 }

    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()

    return engine.generate(
        prompt: promptStr,
        maxTokens: Int(maxTokens),
        temperature: temperature,
        topP: topP,
        maxContextTokens: Int(maxContextTokens),
        memoryBudgetMB: Int(memoryBudgetMB)
    ) { text, isFinal, promptTokens, completionTokens, prefillTps, decodeTps in
        text.withCString { cstr in
            tokenCb(cstr, isFinal, UInt32(promptTokens), UInt32(completionTokens), Float(prefillTps), Float(decodeTps), callbackContext)
        }
    }
}

@_cdecl("MLXBridge_modelInfo")
public func MLXBridge_modelInfo(
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard let ctx = context else { return nil }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    guard let json = engine.modelInfo() else { return nil }
    // strdup() alokuje przez malloc — Rust zwolni przez libc free().
    return strdup(json)
}

/// Liczy embedding dla jednego tekstu. Zwraca bufor `Float` zaalokowany przez
/// malloc (Rust zwalnia go przez libc free()) i zapisuje dlugosc do `outLen`.
/// NULL = blad. Sygnatura MUSI pasowac do `EmbedFn` w mlx_swift_bridge.rs.
@_cdecl("MLXBridge_embed")
public func MLXBridge_embed(
    text: UnsafePointer<CChar>?,
    outLen: UnsafeMutablePointer<Int32>?,
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<Float>? {
    guard let textStr = text.flatMap({ String(cString: $0) }),
          let ctx = context else { return nil }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    guard let vec = engine.embed(text: textStr), !vec.isEmpty else { return nil }

    let bytes = vec.count * MemoryLayout<Float>.stride
    guard let raw = malloc(bytes) else { return nil }
    let fptr = raw.assumingMemoryBound(to: Float.self)
    vec.withUnsafeBufferPointer { src in
        fptr.update(from: src.baseAddress!, count: vec.count)
    }
    outLen?.pointee = Int32(vec.count)
    return fptr
}
