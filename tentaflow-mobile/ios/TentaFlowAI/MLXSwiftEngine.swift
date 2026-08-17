// =============================================================================
// Plik: MLXSwiftEngine.swift
// Opis: Natywny silnik inferencji MLX na iOS — wrapper na MLXLLM framework.
//       Rejestruje callbacks w Rust core przez FFI.
// =============================================================================

import Foundation
import HuggingFace
import MLX
import MLXHuggingFace
import MLXLLM
import MLXLMCommon
import Tokenizers

/// Silnik MLX na iOS — ladowanie modeli i generowanie tekstu
class MLXSwiftEngine: @unchecked Sendable {
    static let shared = MLXSwiftEngine()

    private var modelContainer: ModelContainer?
    private var modelPath: String?
    private let queue = DispatchQueue(label: "ai.tentaflow.mlx", qos: .userInitiated)

    private init() {
        // Ustaw limit cache GPU — maly limit na iOS zeby nie wyczerpac pamieci
        MLX.GPU.set(cacheLimit: 64 * 1024 * 1024)
    }

    /// Rejestruje callbacks w Rust core
    func registerWithRust() {
        // Przekaz function pointers do Rust
        let context = Unmanaged.passUnretained(self).toOpaque()

        tentaflow_register_mlx_swift(
            swiftLoadModel,
            swiftUnloadModel,
            swiftGenerate,
            swiftModelInfo,
            context
        )
        print("[MLXSwift] Callbacks zarejestrowane w Rust")
    }

    /// Laduje model z podanej sciezki
    func loadModel(path: String) -> Bool {
        print("[MLXSwift] Ladowanie modelu: \(path)")

        let url = URL(filePath: path)

        // Sprawdz czy katalog istnieje
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXSwift] Sciezka nie istnieje: \(path)")
            return false
        }

        // Zaladuj model synchronicznie (blokuje watek)
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
                    if Int(progress.fractionCompleted * 100) % 25 == 0 {
                        print("[MLXSwift] Ladowanie: \(Int(progress.fractionCompleted * 100))%")
                    }
                }
                self.modelPath = path
                success = true
                print("[MLXSwift] Model zaladowany pomyslnie")
            } catch {
                print("[MLXSwift] Blad ladowania: \(error)")
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Wyladowuje model z pamieci
    func unloadModel() {
        print("[MLXSwift] Wyladowywanie modelu")
        modelContainer = nil
        modelPath = nil
        // Zwolnij globalny MLX GPU cache (zwolnione bufory trzymane do reuse).
        // Bez tego wymiana modeli (residency eviction) zostawia bufory w cache —
        // przy wielokrotnych przeladowaniach pamiec rosnie az do jetsam.
        MLX.GPU.clearCache()
    }

    /// Generuje tekst z callbackiem na kazdy token. Zwraca kod: 0=OK,
    /// -1=blad generyczny, -10=brak pamieci/przekroczony kontekst (guard
    /// przerwal przed OOM). maxContextTokens=0 / memoryBudgetMB=0 wylaczaja limity.
    func generate(
        prompt: String,
        maxTokens: Int,
        temperature: Float,
        topP: Float,
        maxContextTokens: Int,
        memoryBudgetMB: Int,
        // (text, isFinal, promptTokens, completionTokens, prefillTps, decodeTps).
        // Liczniki i predkosci faz sa niezerowe tylko na finalnym wywolaniu
        // (isFinal=true) i pochodza z realnych pomiarow MLX
        // (GenerateCompletionInfo), nie z wall-clock TTFT. Kontrakt musi byc
        // identyczny z macOS MLXBridge — Rust ma jeden typ callbacku dla obu.
        tokenCallback: @escaping (String, Bool, Int, Int, Double, Double) -> Void
    ) -> Int32 {
        guard let container = modelContainer else {
            print("[MLXSwift] Brak zaladowanego modelu")
            return -1
        }

        print("[MLXSwift] Generowanie: max_tokens=\(maxTokens), temp=\(temperature), maxCtx=\(maxContextTokens), budgetMB=\(memoryBudgetMB)")
        print("[MLXSwift] Prompt (\(prompt.count) znakow): \(prompt.prefix(200))")

        // Twardy limit pamieci (relaxed:false -> blad zamiast OOM). Na iOS budzet
        // jest znacznie mniejszy niz na Macu.
        let budgetBytes = memoryBudgetMB > 0 ? memoryBudgetMB * 1024 * 1024 : 0
        if budgetBytes > 0 {
            MLX.GPU.set(memoryLimit: budgetBytes, relaxed: false)
        }

        let semaphore = DispatchSemaphore(value: 0)
        var resultCode: Int32 = -1

        let parameters = GenerateParameters(temperature: temperature, topP: topP)

        Task {
            do {
                let counts = try await container.perform { context in
                    // Prompt juz jest sformatowany przez Rust (ChatML).
                    let tokenIds = context.tokenizer.encode(text: prompt)
                    if maxContextTokens > 0 && tokenIds.count > maxContextTokens {
                        throw MLXContextBudgetExceeded()
                    }
                    let promptCount = tokenIds.count
                    let inputTokens = MLXArray(tokenIds)
                    let input = LMInput(tokens: inputTokens)

                    let stopStrings = ["<|im_end|>", "<|endoftext|>", "</s>"]

                    // Flaga OOM w scope `perform` (jak `lastOutput`).
                    var lastOutput = ""
                    var memExceeded = false
                    // Licznik wygenerowanych tokenow — mutowany w token-closure,
                    // zwracany na koncu (MLX nie oddaje go w GenerateCompletionInfo).
                    var genCount = 0
                    let info = try MLXLMCommon.generate(
                        input: input,
                        parameters: parameters,
                        context: context
                    ) { tokens in
                        if budgetBytes > 0 {
                            let snap = MLX.GPU.snapshot()
                            if snap.activeMemory + snap.cacheMemory > budgetBytes {
                                memExceeded = true
                                return .stop
                            }
                        }
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
                        if maxContextTokens > 0 && promptCount + tokens.count >= maxContextTokens {
                            return .stop
                        }
                        return tokens.count >= maxTokens ? .stop : .more
                    }
                    if memExceeded { throw MLXContextBudgetExceeded() }
                    // Realne predkosci faz z MLX: promptTokensPerSecond = prefill,
                    // tokensPerSecond = decode. Dokladniejsze niz wall-clock.
                    return (promptCount, genCount, info.promptTokensPerSecond, info.tokensPerSecond)
                }

                tokenCallback("", true, counts.0, counts.1, counts.2, counts.3)
                resultCode = 0
                print("[MLXSwift] Generowanie zakonczone (kod \(resultCode))")
            } catch {
                print("[MLXSwift] Blad generowania: \(error)")
                tokenCallback("", true, 0, 0, 0, 0)
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

    /// Zwraca JSON z info o modelu
    func modelInfo() -> String? {
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

// =============================================================================
// C callbacks — wywolywane z Rust przez FFI
// =============================================================================

/// C callback: zaladuj model
private func swiftLoadModel(modelPath: UnsafePointer<CChar>?, context: UnsafeMutableRawPointer?) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }

    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

/// C callback: wyladuj model
private func swiftUnloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

/// Sygnalizuje prompt przekraczajacy limit kontekstu -> kod -10.
private struct MLXContextBudgetExceeded: Error {}

/// C callback: generuj tekst
private func swiftGenerate(
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

    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()

    return engine.generate(
        prompt: promptStr,
        maxTokens: Int(maxTokens),
        temperature: temperature,
        topP: topP,
        maxContextTokens: Int(maxContextTokens),
        memoryBudgetMB: Int(memoryBudgetMB)
    ) { text, isFinal, promptTokens, completionTokens, prefillTps, decodeTps in
        // Wywolaj Rust token callback
        text.withCString { cstr in
            tokenCb(cstr, isFinal, UInt32(promptTokens), UInt32(completionTokens), Float(prefillTps), Float(decodeTps), callbackContext)
        }
    }
}

/// C callback: info o modelu
private func swiftModelInfo(context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>? {
    guard let ctx = context else { return nil }
    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()

    guard let json = engine.modelInfo() else { return nil }

    // Alokuj C string — Rust musi go zwolnic przez free()
    return strdup(json)
}
